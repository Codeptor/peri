#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

/// Verification 2026-08-08 — Hyperliquid WS allMids subscription shape
///
/// Checked https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions
/// Result:
///   - Native: {"method":"subscribe","subscription":{"type":"allMids"}}
///   - Per-dex: {"method":"subscribe","subscription":{"type":"allMids","dex":"xyz"}}
///     The docs list `allMids: { type: "allMids", dex: "<dex>" }` with `dex` optional; if omitted, first perp dex.
///     Data format: AllMids { mids: Record<string,string> } delivered as
///     {"channel":"allMids","data":{"mids":{...}}}.
///
/// Finding: per-dex ws IS supported (dex field). Therefore primary path is ws for both dexes.
/// Fallback (spec-required): if per-dex ws were unsupported, xyz would be fetched via
/// 2s REST poll of `{"type":"allMids","dex":"xyz"}` behind the same `tx` interface so callers
/// are agnostic. That fallback code is retained (see `spawn_rest_poll_fallback`) and
/// gated by `PER_DEX_WS_SUPPORTED`.
///
const PER_DEX_WS_SUPPORTED: bool = true;

/// Verification 2026-08-22 — Hyperliquid's WebSocket "Timeouts and heartbeats" docs
/// (https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/timeouts-and-heartbeats):
/// clients initiate a heartbeat with exactly `{"method":"ping"}` and the server responds
/// with `{"channel":"pong"}`. These are application frames, not WebSocket control pings.
const PING_FRAME: &str = r#"{"method":"ping"}"#;
const PING_INTERVAL: Duration = Duration::from_secs(20);
const LIVENESS_DEADLINE: Duration = Duration::from_secs(30);
const DATA_IDLE_DEADLINE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
struct LivenessTiming {
    ping_interval: Duration,
    deadline: Duration,
    data_idle: Duration,
}

const LIVE_LIVENESS_TIMING: LivenessTiming = LivenessTiming {
    ping_interval: PING_INTERVAL,
    deadline: LIVENESS_DEADLINE,
    data_idle: DATA_IDLE_DEADLINE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LivenessRespawn {
    UnansweredPing,
    DataIdle,
}

/// Connection-local liveness state. A ping deadline starts with the first unanswered ping;
/// subsequent 20s pings do not extend it, so two unanswered pings force a reconnect at 30s.
struct ConnectionLiveness {
    last_data: tokio::time::Instant,
    awaiting_inbound_since: Option<tokio::time::Instant>,
    timing: LivenessTiming,
}

impl ConnectionLiveness {
    fn new(now: tokio::time::Instant) -> Self {
        Self::with_timing(now, LIVE_LIVENESS_TIMING)
    }

    fn with_timing(now: tokio::time::Instant, timing: LivenessTiming) -> Self {
        Self {
            last_data: now,
            awaiting_inbound_since: None,
            timing,
        }
    }

    fn on_ping_sent(&mut self, now: tokio::time::Instant) {
        self.awaiting_inbound_since.get_or_insert(now);
    }

    fn on_inbound(&mut self, now: tokio::time::Instant, is_data: bool) {
        self.awaiting_inbound_since = None;
        if is_data {
            self.last_data = now;
        }
    }

    fn respawn_reason(
        &self,
        now: tokio::time::Instant,
        require_data: bool,
    ) -> Option<LivenessRespawn> {
        if self
            .awaiting_inbound_since
            .is_some_and(|sent| now.saturating_duration_since(sent) >= self.timing.deadline)
        {
            return Some(LivenessRespawn::UnansweredPing);
        }
        if require_data && now.saturating_duration_since(self.last_data) >= self.timing.data_idle {
            return Some(LivenessRespawn::DataIdle);
        }
        None
    }

    fn next_deadline(&self, require_data: bool) -> tokio::time::Instant {
        let ping_deadline = self
            .awaiting_inbound_since
            .map(|sent| sent + self.timing.deadline)
            .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(24 * 60 * 60));
        if require_data {
            ping_deadline.min(self.last_data + self.timing.data_idle)
        } else {
            ping_deadline
        }
    }
}

fn ping_interval(timing: LivenessTiming) -> tokio::time::Interval {
    tokio::time::interval_at(
        tokio::time::Instant::now() + timing.ping_interval,
        timing.ping_interval,
    )
}

pub type MidsUpdate = HashMap<String, f64>;

/// A stream counts as live only if it produced a message inside this window.
pub const WS_FRESH_WINDOW_MS: i64 = 30_000;

/// Freshness clock for the market-data streams — the single source of truth behind
/// `/api/health.ws_connected`.
///
/// Post-mortem 2026-08-09: health used to read a set-once `WS_CONNECTED` bool that the
/// mids supervisor re-set to `true` on every reconnect attempt. It therefore reported
/// `true` through two full outages (10:42/10:45Z stream deaths) while no data flowed.
/// Age-based freshness cannot lie that way: a socket that died, is mid-backoff, or is
/// open but silent all read as disconnected within `WS_FRESH_WINDOW_MS`.
#[derive(Debug, Default)]
pub struct WsFreshness {
    mids_ms: AtomicI64,
    ctxs_ms: AtomicI64,
}

impl WsFreshness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_mids(&self, now_ms: i64) {
        self.mids_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn mark_ctxs(&self, now_ms: i64) {
        self.ctxs_ms.store(now_ms, Ordering::SeqCst);
    }

    pub fn mids_ms(&self) -> i64 {
        self.mids_ms.load(Ordering::SeqCst)
    }

    pub fn ctxs_ms(&self) -> i64 {
        self.ctxs_ms.load(Ordering::SeqCst)
    }

    /// Truthful `ws_connected`: either stream seen inside the freshness window.
    pub fn connected_at(&self, now_ms: i64) -> bool {
        ws_connected_at(now_ms, self.mids_ms(), self.ctxs_ms(), WS_FRESH_WINDOW_MS)
    }

    /// Age of the freshest inbound frame across both streams — the watchdog's data clock.
    /// Floored at `boot_ms` so a daemon that has never received a frame ages from its own
    /// start instead of from the epoch (no false 40-year-old feed alert one tick in).
    pub fn feed_age_ms(&self, now_ms: i64, boot_ms: i64) -> i64 {
        feed_age_ms(now_ms, self.mids_ms(), self.ctxs_ms(), boot_ms)
    }
}

/// Pure companion of `WsFreshness::feed_age_ms`. `0` stamps mean "never seen" and lose to
/// `boot_ms`; a future stamp (clock skew) clamps to age 0 rather than reading negative.
pub fn feed_age_ms(now_ms: i64, last_mids_ms: i64, last_ctxs_ms: i64, boot_ms: i64) -> i64 {
    let newest = last_mids_ms.max(last_ctxs_ms).max(boot_ms);
    now_ms.saturating_sub(newest).max(0)
}

/// Pure freshness predicate over raw timestamps (unit-tested).
/// `0` means "never seen" and is never fresh. A stamp in the future (clock skew)
/// counts as fresh rather than tripping a false outage.
pub fn ws_connected_at(now_ms: i64, last_mids_ms: i64, last_ctxs_ms: i64, window_ms: i64) -> bool {
    let fresh = |t: i64| t > 0 && now_ms.saturating_sub(t) < window_ms;
    fresh(last_mids_ms) || fresh(last_ctxs_ms)
}

/// Reconnect backoff floor / cap. Schedule is 1s, 2s, 4s … capped at 30s.
pub const BACKOFF_FLOOR_MS: u64 = 1_000;
pub const BACKOFF_CAP_MS: u64 = 30_000;

/// Pure: un-jittered backoff for a 0-based attempt number (1s doubling, 30s cap).
pub fn backoff_base_ms(attempt: u32) -> u64 {
    let doublings = attempt.min(16);
    BACKOFF_FLOOR_MS
        .saturating_mul(1u64 << doublings)
        .min(BACKOFF_CAP_MS)
}

/// Pure: equal jitter over `[base/2, base]`, clamped to `[1s, 30s]`, `jitter01 ∈ [0,1]`.
/// Jitter keeps the three streams (mids/ctxs/books) from resynchronising onto one retry
/// cadence after a venue-side drop that kills all of them at the same instant.
pub fn backoff_delay_ms(attempt: u32, jitter01: f64) -> u64 {
    let base = backoff_base_ms(attempt);
    let half = base / 2;
    let delay = half + (half as f64 * jitter01.clamp(0.0, 1.0)) as u64;
    delay.clamp(BACKOFF_FLOOR_MS, BACKOFF_CAP_MS)
}

/// Jitter source — sub-second clock noise, no `rand` dependency.
fn jitter01() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as f64 / 1_000_000_000.0)
        .unwrap_or(0.5)
}

/// Shared latest order books (market -> book), refreshed by the books stream.
pub type BookCache = std::sync::Arc<tokio::sync::RwLock<HashMap<String, crate::contracts::L2Book>>>;

/// Manage l2Book subscriptions: positions open/close drive Sub/Unsub per market.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookCmd {
    Sub(String),
    Unsub(String),
}

/// Batched ctx rows from allDexsAssetCtxs ws stream (positional arrays → names via name_maps).
#[derive(Debug, Clone, Default)]
pub struct CtxBatch {
    pub rows: Vec<crate::hl_rest::CtxRow>,
}

/// Verification 2026-08-08 — Hyperliquid WS allDexsAssetCtxs subscription shape
///
/// Checked https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions.md
/// Result:
///   - Subscription: {"method":"subscribe","subscription":{"type":"allDexsAssetCtxs"}}
///   - Data format: WsAllDexsAssetCtxs { ctxs: Array<[string /*dex*/, Array<PerpsAssetCtx>]> }
///     where PerpsAssetCtx = {dayNtlVlm, prevDayPx, markPx, midPx?, funding, openInterest, oraclePx} as NUMBERS
///     (verified against gitbook Data type definitions: PerpsAssetCtx fields are `number`, not string).
///     Field names match REST metaAndAssetCtxs ctx fields but types differ: REST uses strings, ws uses numbers.
///   - Positional: ctxs arrays are parallel to the universe ordering for each dex (same ordering
///     as HlRest::meta_and_ctxs universe[i] ↔ ctxs[i]); midPx is optional (None → fallback to markPx).
///   - Channel: "allDexsAssetCtxs" with data {"ctxs": [...] }.
///
/// Finding: subscription has NO dex param (covers all dexs). Data carries per-dex arrays in
/// numeric form. Implement positional mapping via name_maps seeded from REST universe
/// (HashMap<dex, Vec<market_name>>) — index→name, skip out-of-range or not-yet-seeded.
///
/// Verification 2026-08-21 — the subscriptions documentation lists the `l2Book` shape and
/// options, but documents no per-connection subscription limit or burst guidance. We therefore
/// shard and pace subscriptions defensively for the observed 60s disconnects. The same page
/// explicitly supports `{"method":"unsubscribe","subscription":...}` with the original
/// l2Book subscription object, so removed markets are unsubscribed in-place.
const BOOKS_PER_SHARD: usize = 50;
const BOOK_SUBSCRIBE_BATCH: usize = 10;
const BOOK_SUBSCRIBE_STAGGER: Duration = Duration::from_millis(150);

struct BookShard {
    markets: Vec<String>,
    tx: mpsc::Sender<BookCmd>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct BookMembershipChange {
    removed: Vec<(usize, String)>,
    added_to_existing: Vec<(usize, String)>,
    new_shards: Vec<Vec<String>>,
}

/// Handle for updating the desired l2Book market membership after a universe refresh.
#[derive(Clone)]
pub struct BooksSupervisor {
    markets_tx: watch::Sender<Vec<String>>,
}

impl BooksSupervisor {
    pub fn update_markets(&self, markets: &[String]) {
        let markets = unique_book_markets(markets);
        self.markets_tx.send_if_modified(|current| {
            if *current == markets {
                false
            } else {
                *current = markets;
                true
            }
        });
    }
}

fn shard_book_markets(markets: Vec<String>) -> Vec<Vec<String>> {
    markets
        .chunks(BOOKS_PER_SHARD)
        .map(<[String]>::to_vec)
        .collect()
}

fn unique_book_markets(markets: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    markets
        .iter()
        .filter(|market| seen.insert((*market).clone()))
        .cloned()
        .collect()
}

fn reconcile_book_membership(shards: &[Vec<String>], markets: &[String]) -> BookMembershipChange {
    let desired = unique_book_markets(markets);
    let desired_set: HashSet<&String> = desired.iter().collect();
    let current_set: HashSet<&String> = shards.iter().flatten().collect();
    let mut simulated = shards.to_vec();
    let mut change = BookMembershipChange::default();

    for (shard_index, shard) in simulated.iter_mut().enumerate() {
        let removed: Vec<String> = shard
            .iter()
            .filter(|market| !desired_set.contains(market))
            .cloned()
            .collect();
        for market in &removed {
            change.removed.push((shard_index, market.clone()));
        }
        shard.retain(|market| desired_set.contains(market));
    }

    for market in desired
        .into_iter()
        .filter(|market| !current_set.contains(market))
    {
        if let Some((shard_index, shard)) = simulated
            .iter_mut()
            .enumerate()
            .find(|(_, shard)| shard.len() < BOOKS_PER_SHARD)
        {
            shard.push(market.clone());
            change.added_to_existing.push((shard_index, market));
        } else if let Some(shard) = change
            .new_shards
            .last_mut()
            .filter(|shard| shard.len() < BOOKS_PER_SHARD)
        {
            shard.push(market);
        } else {
            change.new_shards.push(vec![market]);
        }
    }
    change
}

fn book_subscription_batches(markets: Vec<String>) -> Vec<Vec<String>> {
    markets
        .chunks(BOOK_SUBSCRIBE_BATCH)
        .map(<[String]>::to_vec)
        .collect()
}

fn l2book_frame(method: &str, coin: &str) -> String {
    serde_json::json!({"method":method,"subscription":{"type":"l2Book","coin":coin}}).to_string()
}

/// Spawn sharded l2Book streams. Each shard reconnects and re-subscribes independently, while
/// all book updates share `cache` and the existing bounded command channel remains unchanged.
pub fn spawn_books_stream(
    url: String,
    markets: Vec<String>,
    mut cmd_rx: mpsc::Receiver<BookCmd>,
    cache: BookCache,
) -> BooksSupervisor {
    let (markets_tx, mut markets_rx) = watch::channel(unique_book_markets(&markets));
    tokio::spawn(async move {
        let chunks = shard_book_markets(markets_rx.borrow().clone());
        let shard_count = chunks.len();
        let mut shards: Vec<BookShard> = chunks
            .into_iter()
            .enumerate()
            .map(|(index, shard_markets)| {
                let tx = spawn_book_shard(
                    url.clone(),
                    shard_markets.clone(),
                    cache.clone(),
                    index + 1,
                    shard_count,
                );
                BookShard {
                    markets: shard_markets,
                    tx,
                }
            })
            .collect();

        loop {
            tokio::select! {
                changed = markets_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let current: Vec<Vec<String>> = shards.iter().map(|shard| shard.markets.clone()).collect();
                    let change = reconcile_book_membership(&current, &markets_rx.borrow());
                    for (shard_index, market) in change.removed {
                        let shard = &mut shards[shard_index];
                        if shard.tx.send(BookCmd::Unsub(market.clone())).await.is_err() {
                            warn!(stream = "books", market = %market, "book shard command channel closed");
                        }
                        shard.markets.retain(|tracked| tracked != &market);
                    }
                    for (shard_index, market) in change.added_to_existing {
                        let shard = &mut shards[shard_index];
                        if shard.tx.send(BookCmd::Sub(market.clone())).await.is_err() {
                            warn!(stream = "books", market = %market, "book shard command channel closed");
                        }
                        shard.markets.push(market);
                    }
                    let shard_count = shards.len() + change.new_shards.len();
                    for markets in change.new_shards {
                        let shard_index = shards.len() + 1;
                        let tx = spawn_book_shard(url.clone(), markets.clone(), cache.clone(), shard_index, shard_count);
                        shards.push(BookShard { markets, tx });
                    }
                }
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return };
                    let coin = match &cmd {
                        BookCmd::Sub(coin) | BookCmd::Unsub(coin) => coin,
                    };
                    let Some(shard) = shards.iter_mut().find(|shard| shard.markets.contains(coin)) else {
                        warn!(stream = "books", market = %coin, "book command ignored for market outside current shard set");
                        continue;
                    };
                    if shard.tx.send(cmd.clone()).await.is_err() {
                        warn!(stream = "books", market = %coin, "book shard command channel closed");
                    }
                }
            }
        }
    });
    BooksSupervisor { markets_tx }
}

fn spawn_book_shard(
    url: String,
    initial_markets: Vec<String>,
    cache: BookCache,
    shard_index: usize,
    shard_count: usize,
) -> mpsc::Sender<BookCmd> {
    let (tx, mut rx) = mpsc::channel(32);
    tokio::spawn(async move {
        use tokio_tungstenite::tungstenite::Message;
        let mut tracked = initial_markets;
        let mut attempt: u32 = 0;
        loop {
            let ws_stream = match tokio_tungstenite::connect_async(&url).await {
                Ok((stream, _)) => stream,
                Err(error) => {
                    attempt = attempt.saturating_add(1);
                    let delay = backoff_delay_ms(attempt, jitter01());
                    info!(stream = "books", shard = format_args!("{shard_index}/{shard_count}"), attempt, delay_ms = delay, error = %error, "ws connect failed, reconnecting");
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    continue;
                }
            };
            info!(
                stream = "books",
                shard = format_args!("{shard_index}/{shard_count}"),
                attempt,
                "ws connected"
            );
            let (mut write, mut read) = ws_stream.split();
            let mut subscribe_failed = false;
            for (batch_index, batch) in book_subscription_batches(tracked.clone())
                .into_iter()
                .enumerate()
            {
                if batch_index > 0 {
                    tokio::time::sleep(BOOK_SUBSCRIBE_STAGGER).await;
                }
                for coin in batch {
                    if write
                        .send(Message::Text(l2book_frame("subscribe", &coin).into()))
                        .await
                        .is_err()
                    {
                        subscribe_failed = true;
                        break;
                    }
                }
                if subscribe_failed {
                    break;
                }
            }
            if subscribe_failed {
                attempt = attempt.saturating_add(1);
                continue;
            }
            attempt = 0;
            let mut ping = ping_interval(LIVE_LIVENESS_TIMING);
            let mut liveness = ConnectionLiveness::new(tokio::time::Instant::now());
            loop {
                tokio::select! {
                    msg = read.next() => {
                        let Some(Ok(message)) = msg else { break };
                        liveness.on_inbound(tokio::time::Instant::now(), matches!(&message, Message::Text(_)));
                        match message {
                            Message::Text(text) => {
                                if let Some((coin, book)) = parse_l2book_text(&text) {
                                    cache.write().await.insert(coin, book);
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                    _ = ping.tick() => {
                        liveness.on_ping_sent(tokio::time::Instant::now());
                        if write.send(Message::Text(PING_FRAME.into())).await.is_err() { break; }
                    }
                    _ = tokio::time::sleep_until(liveness.next_deadline(false)) => {
                        if let Some(reason) = liveness.respawn_reason(tokio::time::Instant::now(), false) {
                            warn!(stream = "books", shard = format_args!("{shard_index}/{shard_count}"), ?reason, "liveness timeout, force-respawn");
                            break;
                        }
                    }
                    cmd = rx.recv() => match cmd {
                        Some(BookCmd::Sub(coin)) if !tracked.contains(&coin) => {
                            tracked.push(coin.clone());
                            if write.send(Message::Text(l2book_frame("subscribe", &coin).into())).await.is_err() { break; }
                        }
                        Some(BookCmd::Unsub(coin)) => {
                            if let Some(index) = tracked.iter().position(|market| market == &coin) {
                                tracked.remove(index);
                                if write.send(Message::Text(l2book_frame("unsubscribe", &coin).into())).await.is_err() { break; }
                                cache.write().await.remove(&coin);
                            }
                        }
                        Some(BookCmd::Sub(_)) => {}
                        None => return,
                    }
                }
            }
            attempt = attempt.saturating_add(1);
            let delay = backoff_delay_ms(attempt, jitter01());
            info!(
                stream = "books",
                shard = format_args!("{shard_index}/{shard_count}"),
                attempt,
                delay_ms = delay,
                "ws stream ended, reconnecting"
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    });
    tx
}

/// Parse one l2Book ws frame: {"channel":"l2Book","data":{"coin":"...","time":ms,"levels":[[bids],[asks]]}}
/// levels carry string numbers {"px","sz","n"}. Returns (coin, L2Book) with levels best-first:
/// bids sorted desc by px, asks asc (HL ships them ordered, we re-sort defensively).
fn parse_l2book_text(text: &str) -> Option<(String, crate::contracts::L2Book)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("channel").and_then(|c| c.as_str()) != Some("l2Book") {
        return None;
    }
    let data = v.get("data")?;
    let coin = data.get("coin").and_then(|c| c.as_str())?.to_string();
    let ts = data
        .get("time")
        .and_then(|t| t.as_i64())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let levels = data.get("levels").and_then(|l| l.as_array())?;
    let mut out = crate::contracts::L2Book {
        levels: [vec![], vec![]],
        ts,
    };
    for (i, side) in levels.iter().take(2).enumerate() {
        let arr = side.as_array()?;
        let mut parsed: Vec<crate::contracts::L2Level> = arr
            .iter()
            .filter_map(|l| {
                let px: f64 = l.get("px").and_then(|x| x.as_str())?.parse().ok()?;
                let sz: f64 = l.get("sz").and_then(|x| x.as_str())?.parse().ok()?;
                Some(crate::contracts::L2Level { px, sz })
            })
            .collect();
        if i == 0 {
            parsed.sort_by(|a, b| b.px.partial_cmp(&a.px).unwrap_or(std::cmp::Ordering::Equal)); // bids desc
        } else {
            parsed.sort_by(|a, b| a.px.partial_cmp(&b.px).unwrap_or(std::cmp::Ordering::Equal)); // asks asc
        }
        out.levels[i] = parsed;
    }
    Some((coin, out))
}

/// Parse allDexsAssetCtxs ws frame positional → CtxRows via name_maps.
/// Frame shape: {"channel":"allDexsAssetCtxs","data":{"ctxs":[["","[...PerpsAssetCtx...]"],["xyz",[...]],...]}}
/// Returns None on channel mismatch or missing data; skips dexes not yet in name_maps and indices out of range.
fn parse_ctxs_text(text: &str, name_maps: &HashMap<String, Vec<String>>) -> Option<CtxBatch> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    if v.get("channel").and_then(|c| c.as_str()) != Some("allDexsAssetCtxs") {
        return None;
    }
    let data = v.get("data")?;
    // data may be {"ctxs": [...]} or directly the ctxs array (defensive)
    let ctxs_val = data.get("ctxs").unwrap_or(data);
    let ctxs_arr = ctxs_val.as_array()?;
    let mut rows = Vec::new();
    for entry in ctxs_arr {
        let pair = entry.as_array()?;
        if pair.len() != 2 {
            continue;
        }
        let dex = pair[0].as_str().unwrap_or("").to_string();
        let perps = pair[1].as_array()?;
        let names = match name_maps.get(&dex) {
            Some(n) => n,
            None => continue, // names not yet seeded — skip this dex
        };
        for (idx, ctx) in perps.iter().enumerate() {
            let Some(name) = names.get(idx) else {
                continue; // out-of-range index → skip
            };
            // helper: number or string → f64 (ws is number, REST is string; be tolerant)
            let f = |field: &str| -> Option<f64> {
                let val = ctx.get(field)?;
                match val {
                    serde_json::Value::Number(n) => n.as_f64(),
                    serde_json::Value::String(s) => s.parse().ok(),
                    serde_json::Value::Null => Some(0.0),
                    _ => None,
                }
            };
            // optional midPx: null/0 → fallback to markPx handled below
            let mark = f("markPx")?;
            let oracle = f("oraclePx").unwrap_or(mark);
            let mid_raw = ctx.get("midPx").and_then(|v| match v {
                serde_json::Value::Number(n) => n.as_f64(),
                serde_json::Value::String(s) => s.parse().ok(),
                serde_json::Value::Null => None,
                _ => None,
            });
            let mid = mid_raw.unwrap_or(mark);
            let funding = f("funding").unwrap_or(0.0);
            let oi = f("openInterest").unwrap_or(0.0);
            let vlm = f("dayNtlVlm").unwrap_or(0.0);
            let prev = f("prevDayPx").unwrap_or(0.0);
            rows.push(crate::hl_rest::CtxRow {
                market: name.clone(),
                mark,
                oracle,
                mid,
                funding,
                open_interest: oi,
                day_ntl_vlm: vlm,
                prev_day_px: prev,
            });
        }
    }
    if rows.is_empty() {
        None
    } else {
        Some(CtxBatch { rows })
    }
}

/// Spawn the allDexsAssetCtxs stream. Reconnects forever with jittered 1s→30s backoff,
/// resubscribing on every reconnect; each cycle logs at INFO with its attempt count.
/// A session that delivered at least one frame resets the schedule to 1s — only a run of
/// dead sessions escalates toward the cap.
/// Bounded channel (64); on lag drop-oldest (log debug). name_maps supplies universe ordering for positional mapping.
pub fn spawn_ctxs_stream(
    url: String,
    name_maps: std::sync::Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    tx: mpsc::Sender<Vec<crate::hl_rest::CtxRow>>,
    fresh: std::sync::Arc<WsFreshness>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut attempt: u32 = 0;
        loop {
            let session_start = chrono::Utc::now().timestamp_millis();
            if let Err(e) = run_ctxs_once(&url, &name_maps, &tx, &fresh).await {
                warn!(stream = "ctxs", error=%e, "ctxs stream ended");
            }
            attempt = if fresh.ctxs_ms() >= session_start {
                0
            } else {
                attempt.saturating_add(1)
            };
            let delay = backoff_delay_ms(attempt, jitter01());
            info!(
                stream = "ctxs",
                attempt,
                delay_ms = delay,
                "ws reconnecting"
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    })
}

async fn run_ctxs_once(
    url: &str,
    name_maps: &std::sync::Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    tx: &mpsc::Sender<Vec<crate::hl_rest::CtxRow>>,
    fresh: &WsFreshness,
) -> Result<(), String> {
    run_ctxs_once_with_timing(url, name_maps, tx, fresh, LIVE_LIVENESS_TIMING).await
}

async fn run_ctxs_once_with_timing(
    url: &str,
    name_maps: &std::sync::Arc<tokio::sync::RwLock<HashMap<String, Vec<String>>>>,
    tx: &mpsc::Sender<Vec<crate::hl_rest::CtxRow>>,
    fresh: &WsFreshness,
    timing: LivenessTiming,
) -> Result<(), String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("ctxs connect failed: {e}"))?;
    info!(stream = "ctxs", "ws connected");
    let (mut write, mut read) = ws_stream.split();
    let sub = serde_json::json!({"method":"subscribe","subscription":{"type":"allDexsAssetCtxs"}})
        .to_string();
    write
        .send(tokio_tungstenite::tungstenite::Message::Text(sub.into()))
        .await
        .map_err(|e| format!("ctxs subscribe send failed: {e}"))?;
    let mut ping = ping_interval(timing);
    let mut liveness = ConnectionLiveness::with_timing(tokio::time::Instant::now(), timing);
    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else { return Err("ctxs stream ended".into()) };
                let msg = msg.map_err(|e| format!("ws read error: {e}"))?;
                // Any inbound frame, including the documented app-level pong, proves socket liveness.
                fresh.mark_ctxs(chrono::Utc::now().timestamp_millis());
                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                    tokio_tungstenite::tungstenite::Message::Close(_) => return Err("ws closed".into()),
                    _ => {
                        liveness.on_inbound(tokio::time::Instant::now(), false);
                        continue;
                    }
                };
                liveness.on_inbound(
                    tokio::time::Instant::now(),
                    is_stream_data(&text, "allDexsAssetCtxs"),
                );
                // need to read name_maps snapshot for this frame
                let maps = name_maps.read().await.clone();
                if let Some(batch) = parse_ctxs_text(&text, &maps) {
                    match tx.try_send(batch.rows) {
                        Ok(()) => {},
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            debug!("ctxs channel full (64), dropping batch (idempotent)");
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return Err("ctxs channel closed".into()),
                    }
                }
            }
            _ = ping.tick() => {
                liveness.on_ping_sent(tokio::time::Instant::now());
                write
                    .send(tokio_tungstenite::tungstenite::Message::Text(PING_FRAME.into()))
                    .await
                    .map_err(|e| format!("ctxs ping send failed: {e}"))?;
            }
            _ = tokio::time::sleep_until(liveness.next_deadline(true)) => {
                if let Some(reason) = liveness.respawn_reason(tokio::time::Instant::now(), true) {
                    warn!(stream = "ctxs", shard = "n/a", ?reason, "liveness timeout, force-respawn");
                    return Err("liveness timeout".into());
                }
            }
        }
    }
}

/// Spawn the mids stream task. Reconnects forever with jittered 1s→30s backoff; each
/// cycle logs at INFO with its attempt count. A session that delivered at least one frame
/// resets the schedule to 1s — only a run of dead sessions escalates toward the cap.
/// Bounded channel (64); on lag drop-oldest (log debug).
pub fn spawn_mids_stream(
    url: String,
    dexs: Vec<String>,
    tx: mpsc::Sender<MidsUpdate>,
    fresh: std::sync::Arc<WsFreshness>,
) -> JoinHandle<()> {
    // If per-dex ws unsupported and xyz requested, spawn REST poll fallback for xyz
    // behind the same tx. Currently disabled because verification shows ws supports dex.
    if !PER_DEX_WS_SUPPORTED && dexs.iter().any(|d| d == "xyz") {
        let tx_clone = tx.clone();
        tokio::spawn(async move { rest_poll_fallback(tx_clone).await });
    }

    tokio::spawn(async move {
        let mut attempt: u32 = 0;
        loop {
            let session_start = chrono::Utc::now().timestamp_millis();
            if let Err(e) = run_once(&url, &dexs, &tx, &fresh).await {
                warn!(stream = "mids", error=%e, "hl_ws stream ended");
            }
            attempt = if fresh.mids_ms() >= session_start {
                0
            } else {
                attempt.saturating_add(1)
            };
            let delay = backoff_delay_ms(attempt, jitter01());
            info!(
                stream = "mids",
                attempt,
                delay_ms = delay,
                "ws reconnecting"
            );
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    })
}

async fn rest_poll_fallback(tx: mpsc::Sender<MidsUpdate>) {
    // 2s REST poll for xyz mids — same tx interface so callers never know.
    let rest = crate::hl_rest::HlRest::new("https://api.hyperliquid.xyz");
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        match rest.all_mids(Some("xyz")).await {
            Ok(map) => {
                // Already dex-prefixed? HL xyz mids are native names? Need to prefix xyz:
                // REST allMids for dex xyz returns keys without prefix? Spec says dex-prefixed
                // market names like "xyz:TSLA". Empirically xyz meta names include prefix,
                // but allMids may return bare names. Prefix if missing.
                let mut prefixed = HashMap::with_capacity(map.len());
                for (k, v) in map {
                    let key = if k.contains(':') {
                        k
                    } else {
                        format!("xyz:{k}")
                    };
                    prefixed.insert(key, v);
                }
                if let Err(e) = try_send(&tx, prefixed).await {
                    debug!(error=%e, "rest fallback send lag, dropped");
                }
            }
            Err(e) => {
                debug!(error=%e, "rest fallback poll failed");
            }
        }
    }
}

async fn try_send(tx: &mpsc::Sender<MidsUpdate>, update: MidsUpdate) -> Result<(), String> {
    match tx.try_send(update) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            debug!("mids channel full (64), dropping update (mids idempotent)");
            Err("channel full".into())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err("channel closed".into()),
    }
}

async fn run_once(
    url: &str,
    dexs: &[String],
    tx: &mpsc::Sender<MidsUpdate>,
    fresh: &WsFreshness,
) -> Result<(), String> {
    run_once_with_timing(url, dexs, tx, fresh, LIVE_LIVENESS_TIMING).await
}

async fn run_once_with_timing(
    url: &str,
    dexs: &[String],
    tx: &mpsc::Sender<MidsUpdate>,
    fresh: &WsFreshness,
    timing: LivenessTiming,
) -> Result<(), String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    info!(stream = "mids", "ws connected");
    let (mut write, mut read) = ws_stream.split();

    // Subscribe per dex
    for dex in dexs {
        let sub = if dex.is_empty() {
            serde_json::json!({"method":"subscribe","subscription":{"type":"allMids"}})
        } else {
            serde_json::json!({"method":"subscribe","subscription":{"type":"allMids","dex":dex}})
        };
        let txt = sub.to_string();
        write
            .send(tokio_tungstenite::tungstenite::Message::Text(txt.into()))
            .await
            .map_err(|e| format!("subscribe send failed: {e}"))?;
    }

    let mut ping = ping_interval(timing);
    let mut liveness = ConnectionLiveness::with_timing(tokio::time::Instant::now(), timing);
    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else { return Err("ws stream ended".into()) };
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => return Err(format!("ws read error: {e}")),
                };
                // Any inbound frame, including the documented app-level pong, proves socket liveness.
                fresh.mark_mids(chrono::Utc::now().timestamp_millis());
                let text = match msg {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t,
                    tokio_tungstenite::tungstenite::Message::Close(_) => return Err("ws closed".into()),
                    _ => {
                        liveness.on_inbound(tokio::time::Instant::now(), false);
                        continue;
                    }
                };
                liveness.on_inbound(tokio::time::Instant::now(), is_stream_data(&text, "allMids"));
                if let Some(update) = parse_mids_text(&text) {
                    // Bounded channel 64, drop-oldest on lag
                    let _ = try_send(tx, update).await;
                }
            }
            _ = ping.tick() => {
                liveness.on_ping_sent(tokio::time::Instant::now());
                write
                    .send(tokio_tungstenite::tungstenite::Message::Text(PING_FRAME.into()))
                    .await
                    .map_err(|e| format!("ping send failed: {e}"))?;
            }
            _ = tokio::time::sleep_until(liveness.next_deadline(true)) => {
                if let Some(reason) = liveness.respawn_reason(tokio::time::Instant::now(), true) {
                    warn!(stream = "mids", shard = "n/a", ?reason, "liveness timeout, force-respawn");
                    return Err("liveness timeout".into());
                }
            }
        }
    }
}

fn is_stream_data(text: &str, channel: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|frame| {
            frame
                .get("channel")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some(channel)
}

fn parse_mids_text(text: &str) -> Option<MidsUpdate> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    // HL sends {"channel":"allMids","data":{"mids":{...}}}  or {"channel":"allMids","data":{"mids":{...},"dex":"xyz"?}}
    // Also handle direct {"mids":{...}} for tests
    let mids_val = if let Some(data) = v.get("data") {
        // common shape
        if let Some(m) = data.get("mids") {
            m
        } else {
            // fallback: data itself is mids map?
            data
        }
    } else if let Some(m) = v.get("mids") {
        m
    } else {
        return None;
    };
    let obj = mids_val.as_object()?;
    let mut out = HashMap::with_capacity(obj.len());
    for (k, val) in obj {
        let s = val.as_str()?;
        let f: f64 = s.parse().ok()?;
        out.insert(k.clone(), f);
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    async fn start_mock_server() -> (
        String,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("ws://{addr}");
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = counter.clone();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let ws = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("accept");
                let (mut write, mut read) = ws.split();
                // consume subscribe messages for a bit (non-blocking)
                let _ = tokio::time::timeout(Duration::from_millis(500), read.next()).await;
                // Send one allMids frame
                let frame = serde_json::json!({
                    "channel": "allMids",
                    "data": {"mids": {"BTC":"65000.5","xyz:TSLA":"320.1"}}
                });
                let _ = write.send(Message::Text(frame.to_string().into())).await;
                // close after short delay
                tokio::time::sleep(Duration::from_millis(200)).await;
                let _ = write.send(Message::Close(None)).await;
                // loop to accept next connection (reconnect test)
            }
        });
        (url, handle, counter)
    }

    #[tokio::test]
    async fn ws_receives_update_and_reconnects() {
        let (url, _server_handle, counter) = start_mock_server().await;
        let (tx, mut rx) = mpsc::channel::<MidsUpdate>(64);
        let fresh = std::sync::Arc::new(WsFreshness::new());
        let handle = spawn_mids_stream(url.clone(), vec!["".to_string()], tx, fresh.clone());

        // Wait for first update
        let update = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timeout first update")
            .expect("channel closed");
        assert!(update.contains_key("BTC"), "BTC missing {update:?}");
        assert!((update["BTC"] - 65000.5).abs() < 1e-9);
        assert!(update.contains_key("xyz:TSLA"));

        // Verify reconnect: server should see 2nd connection within 5s
        let start = tokio::time::Instant::now();
        loop {
            if counter.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                panic!(
                    "did not reconnect within 5s, count {}",
                    counter.load(std::sync::atomic::Ordering::SeqCst)
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Should get second update after reconnect
        let update2 = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timeout second update")
            .expect("channel closed");
        assert!(update2.contains_key("BTC"));

        // Freshness is fed by the live stream, so health reads "connected" right now and
        // "disconnected" once the window has elapsed with no further frames.
        let seen = fresh.mids_ms();
        assert!(seen > 0, "mids freshness must be stamped by a live stream");
        assert!(fresh.connected_at(seen));
        assert!(!fresh.connected_at(seen + WS_FRESH_WINDOW_MS));

        handle.abort();
    }

    #[test]
    fn parse_l2book_frame_and_sorts() {
        let frame = r#"{"channel":"l2Book","data":{"coin":"xyz:TSLA","time":1760000000000,"levels":[[{"px":"320.10","sz":"1.5","n":3},{"px":"320.00","sz":"2.0","n":1}],[{"px":"320.30","sz":"4.0","n":2},{"px":"320.25","sz":"0.5","n":1}]]}}"#;
        let (coin, book) = parse_l2book_text(frame).expect("parse l2book");
        assert_eq!(coin, "xyz:TSLA");
        assert_eq!(book.ts, 1760000000000i64);
        // bids desc by default order retained
        assert!((book.levels[0][0].px - 320.10).abs() < 1e-9);
        assert!((book.levels[0][1].px - 320.00).abs() < 1e-9);
        // asks got re-sorted asc defensively
        assert!((book.levels[1][0].px - 320.25).abs() < 1e-9);
        assert!((book.levels[1][1].px - 320.30).abs() < 1e-9);
        // sz parsed from string
        assert!((book.levels[1][0].sz - 0.5).abs() < 1e-9);
        // non-l2book channels ignored
        assert!(parse_l2book_text(r#"{"channel":"allMids","data":{"mids":{}}}"#).is_none());
    }

    #[test]
    fn book_shards_cap_each_connection_at_fifty_markets() {
        for (count, expected) in [
            (0, vec![]),
            (1, vec![1]),
            (49, vec![49]),
            (50, vec![50]),
            (51, vec![50, 1]),
            (140, vec![50, 50, 40]),
        ] {
            let markets = (0..count).map(|i| format!("M{i}")).collect();
            let shards = shard_book_markets(markets);
            assert_eq!(shards.iter().map(Vec::len).collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn book_membership_diff_handles_add_remove_mixed_and_noop() {
        let shards = vec![vec!["SOL".into(), "ETH".into()], vec!["BTC".into()]];

        let noop = reconcile_book_membership(&shards, &["SOL".into(), "ETH".into(), "BTC".into()]);
        assert!(noop.removed.is_empty());
        assert!(noop.added_to_existing.is_empty());
        assert!(noop.new_shards.is_empty());

        let add = reconcile_book_membership(
            &shards,
            &["SOL".into(), "ETH".into(), "BTC".into(), "HYPE".into()],
        );
        assert_eq!(add.added_to_existing, vec![(0, "HYPE".into())]);

        let remove = reconcile_book_membership(&shards, &["SOL".into(), "BTC".into()]);
        assert_eq!(remove.removed, vec![(0, "ETH".into())]);

        let mixed = reconcile_book_membership(&shards, &["SOL".into(), "HYPE".into()]);
        assert_eq!(mixed.removed, vec![(0, "ETH".into()), (1, "BTC".into())]);
        assert_eq!(mixed.added_to_existing, vec![(0, "HYPE".into())]);
    }

    #[test]
    fn book_membership_spills_additions_into_a_new_shard() {
        let full = (0..BOOKS_PER_SHARD).map(|i| format!("M{i}")).collect();
        let desired: Vec<String> = (0..=BOOKS_PER_SHARD).map(|i| format!("M{i}")).collect();
        let change = reconcile_book_membership(&[full], &desired);
        assert!(change.added_to_existing.is_empty());
        assert_eq!(change.new_shards, vec![vec![format!("M{BOOKS_PER_SHARD}")]]);
    }

    #[test]
    fn book_membership_addition_uses_shard_with_capacity() {
        let shards = vec![
            (0..BOOKS_PER_SHARD).map(|i| format!("A{i}")).collect(),
            vec!["SOL".into()],
        ];
        let mut desired: Vec<String> = shards.iter().flatten().cloned().collect();
        desired.push("HYPE".into());
        let change = reconcile_book_membership(&shards, &desired);
        assert_eq!(change.added_to_existing, vec![(1, "HYPE".into())]);
        assert!(change.new_shards.is_empty());
    }

    #[test]
    fn book_subscription_batches_preserve_shard_order_and_frame_shape() {
        let shard = (0..21).map(|i| format!("M{i}")).collect();
        let batches = book_subscription_batches(shard);
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![10, 10, 1]
        );
        assert_eq!(batches[0][0], "M0");
        assert_eq!(batches[2][0], "M20");
        let frame: serde_json::Value =
            serde_json::from_str(&l2book_frame("subscribe", "M0")).expect("frame json");
        assert_eq!(frame["method"], "subscribe");
        assert_eq!(frame["subscription"]["type"], "l2Book");
        assert_eq!(frame["subscription"]["coin"], "M0");
    }

    #[tokio::test]
    async fn books_stream_subscribes_and_fills_cache() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        use tokio_tungstenite::tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("ws://{addr}");
        let subs = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
        let conns = Arc::new(AtomicUsize::new(0));
        let (s2, c2) = (subs.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                c2.fetch_add(1, AOrdering::SeqCst);
                let w2 = s2.clone();
                tokio::spawn(async move {
                    let ws = tokio_tungstenite::accept_async(stream)
                        .await
                        .expect("accept");
                    let (mut write, mut read) = ws.split();
                    while let Some(Ok(m)) = read.next().await {
                        if let Message::Text(t) = m {
                            let v: serde_json::Value =
                                serde_json::from_str(&t).unwrap_or(serde_json::json!({}));
                            if v.get("subscription")
                                .and_then(|s| s.get("type"))
                                .and_then(|s| s.as_str())
                                == Some("l2Book")
                            {
                                let coin =
                                    v["subscription"]["coin"].as_str().unwrap_or("").to_string();
                                if v.get("method").and_then(|m| m.as_str()) == Some("subscribe") {
                                    w2.lock().unwrap().insert(coin.clone());
                                    // send one book frame
                                    let frame = serde_json::json!({"channel":"l2Book","data":{"coin":coin,"time":1760000000000i64,"levels":[[{"px":"99.9","sz":"2.0","n":1}],[{"px":"100.1","sz":"3.0","n":1}]]}});
                                    let _ =
                                        write.send(Message::Text(frame.to_string().into())).await;
                                } else {
                                    w2.lock().unwrap().remove(&coin);
                                }
                            }
                        }
                    }
                });
            }
        });

        let cache: BookCache =
            std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
        let (cmd_tx, cmd_rx) = mpsc::channel::<BookCmd>(32);
        let books = spawn_books_stream(url, vec!["SOL".into()], cmd_rx, cache.clone());
        cmd_tx
            .send(BookCmd::Sub("SOL".into()))
            .await
            .expect("sub send");
        // wait for book in cache
        let start = tokio::time::Instant::now();
        loop {
            if let Some(b) = cache.read().await.get("SOL") {
                assert!((b.levels[1][0].px - 100.1).abs() < 1e-9);
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "book never arrived"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(subs.lock().unwrap().contains("SOL"));
        // An hourly universe refresh adds a market that did not exist in the initial shard set.
        books.update_markets(&["SOL".into(), "HYPE".into()]);
        let start = tokio::time::Instant::now();
        loop {
            if cache.read().await.contains_key("HYPE") {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "new-market book never arrived"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(subs.lock().unwrap().contains("HYPE"));
        // unsubscribe removes from server-side set and cache
        cmd_tx
            .send(BookCmd::Unsub("SOL".into()))
            .await
            .expect("unsub send");
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!subs.lock().unwrap().contains("SOL"));
        assert!(cache.read().await.get("SOL").is_none());
    }

    #[tokio::test]
    async fn book_shard_reconnects_with_only_its_own_subscriptions() {
        use std::sync::Arc;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("ws://{}", listener.local_addr().expect("addr"));
        let received = Arc::new(tokio::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let received_server = received.clone();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept");
                let ws = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("ws accept");
                let (mut write, mut read) = ws.split();
                let mut coins = Vec::new();
                while coins.len() < 2 {
                    let Some(Ok(Message::Text(text))) = read.next().await else {
                        break;
                    };
                    let frame: serde_json::Value =
                        serde_json::from_str(&text).expect("subscribe json");
                    coins.push(
                        frame["subscription"]["coin"]
                            .as_str()
                            .expect("coin")
                            .to_string(),
                    );
                }
                received_server.lock().await.push(coins);
                let _ = write.send(Message::Close(None)).await;
            }
        });

        let cache: BookCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let shard_tx = spawn_book_shard(url, vec!["SOL".into(), "ETH".into()], cache, 1, 2);
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if received.lock().await.len() == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("shard did not reconnect");
        assert_eq!(
            *received.lock().await,
            vec![vec!["SOL", "ETH"], vec!["SOL", "ETH"]]
        );
        drop(shard_tx);
    }

    #[test]
    fn parse_direct_mids() {
        let txt = r#"{"channel":"allMids","data":{"mids":{"SOL":"123.45","ETH":"3000.0"}}}"#;
        let m = parse_mids_text(txt).expect("parse");
        assert!((m["SOL"] - 123.45).abs() < 1e-9);
    }

    #[test]
    fn bounded_channel_drop_oldest() {
        // Verify that try_send drops when full (idempotent mids)
        let (tx, _rx) = mpsc::channel::<MidsUpdate>(1);
        // Fill channel
        let mut map = HashMap::new();
        map.insert("BTC".to_string(), 1.0);
        // Use try_send directly
        tx.try_send(map.clone()).expect("first send");
        // Second send should be Full
        match tx.try_send(map) {
            Err(mpsc::error::TrySendError::Full(_)) => {}
            _ => panic!("expected Full"),
        }
    }

    #[test]
    fn parse_ctxs_verified_shape_numeric_per_dex() {
        // Verified shape 2026-08-08: {"method":"subscribe","subscription":{"type":"allDexsAssetCtxs"}}
        // data {"ctxs":[["",[PerpsAssetCtx]],["xyz",[PerpsAssetCtx]]]} with NUMERIC fields.
        let mut maps = HashMap::new();
        maps.insert("".to_string(), vec!["BTC".to_string(), "ETH".to_string()]);
        maps.insert("xyz".to_string(), vec!["xyz:TSLA".to_string()]);
        let frame = serde_json::json!({
            "channel": "allDexsAssetCtxs",
            "data": {
                "ctxs": [
                    ["", [
                        {"dayNtlVlm": 1234567.0, "prevDayPx": 64000.0, "markPx": 65000.5, "midPx": 65000.0, "funding": 0.0001, "openInterest": 1000.0, "oraclePx": 64999.0},
                        {"dayNtlVlm": 2345678.0, "prevDayPx": 3000.0, "markPx": 3100.0, "funding": 0.0002, "openInterest": 2000.0, "oraclePx": 3099.0}
                    ]],
                    ["xyz", [
                        {"dayNtlVlm": 999999.0, "prevDayPx": 320.0, "markPx": 325.0, "midPx": 324.5, "funding": 0.0003, "openInterest": 500.0, "oraclePx": 324.0}
                    ]]
                ]
            }
        })
        .to_string();
        let batch = parse_ctxs_text(&frame, &maps).expect("parse ctxs");
        assert_eq!(batch.rows.len(), 3);
        let btc = batch.rows.iter().find(|r| r.market == "BTC").expect("BTC");
        assert!((btc.mark - 65000.5).abs() < 1e-9);
        assert!((btc.mid - 65000.0).abs() < 1e-9);
        assert!((btc.funding - 0.0001).abs() < 1e-12);
        assert!((btc.open_interest - 1000.0).abs() < 1e-9);
        assert!((btc.day_ntl_vlm - 1234567.0).abs() < 1e-9);
        let eth = batch.rows.iter().find(|r| r.market == "ETH").expect("ETH");
        // midPx missing -> fallback to markPx (3100.0)
        assert!((eth.mid - 3100.0).abs() < 1e-9, "mid fallback to markPx");
        let tsla = batch
            .rows
            .iter()
            .find(|r| r.market == "xyz:TSLA")
            .expect("TSLA");
        assert!((tsla.mark - 325.0).abs() < 1e-9);
        assert!((tsla.funding - 0.0003).abs() < 1e-12);
    }

    #[test]
    fn parse_ctxs_positional_skip_out_of_range_and_empty_maps() {
        // Out-of-range: names has 1 entry but ctx array has 2 -> second skipped
        let mut maps = HashMap::new();
        maps.insert("".to_string(), vec!["BTC".to_string()]);
        let frame = serde_json::json!({
            "channel": "allDexsAssetCtxs",
            "data": {"ctxs": [["", [
                {"dayNtlVlm": 1.0, "prevDayPx": 1.0, "markPx": 100.0, "funding": 0.01, "openInterest": 1.0, "oraclePx": 100.0},
                {"dayNtlVlm": 1.0, "prevDayPx": 1.0, "markPx": 101.0, "funding": 0.02, "openInterest": 1.0, "oraclePx": 101.0}
            ]]]}
        })
        .to_string();
        let batch = parse_ctxs_text(&frame, &maps).expect("parse");
        assert_eq!(
            batch.rows.len(),
            1,
            "second index out-of-range should be skipped"
        );
        assert_eq!(batch.rows[0].market, "BTC");
        assert!((batch.rows[0].funding - 0.01).abs() < 1e-12);

        // Empty name_maps (not yet seeded) -> returns None (degrade to no-op)
        let empty: HashMap<String, Vec<String>> = HashMap::new();
        let none = parse_ctxs_text(&frame, &empty);
        assert!(none.is_none(), "empty name_maps should degrade to None");

        // Unknown dex not in maps -> skip that dex, but still parse known dex
        let mut maps2 = HashMap::new();
        maps2.insert("xyz".to_string(), vec!["xyz:TSLA".to_string()]);
        let frame2 = serde_json::json!({
            "channel": "allDexsAssetCtxs",
            "data": {"ctxs": [
                ["", [{"dayNtlVlm": 1.0, "prevDayPx": 1.0, "markPx": 100.0, "funding": 0.01, "openInterest": 1.0, "oraclePx": 100.0}]],
                ["xyz", [{"dayNtlVlm": 2.0, "prevDayPx": 2.0, "markPx": 200.0, "funding": 0.02, "openInterest": 2.0, "oraclePx": 200.0}]]
            ]}
        })
        .to_string();
        let batch2 = parse_ctxs_text(&frame2, &maps2).expect("parse2");
        assert_eq!(batch2.rows.len(), 1);
        assert_eq!(batch2.rows[0].market, "xyz:TSLA");
    }

    #[tokio::test]
    async fn ctxs_stream_subscribe_frame_and_batch_emitted() {
        // Mock server asserts subscribe frame shape, sends verified numeric PerpsAssetCtx frame
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("ws://{addr}");
        let observed_sub = std::sync::Arc::new(tokio::sync::Mutex::new(String::new()));
        let obs2 = observed_sub.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("accept");
            let (mut write, mut read) = ws.split();
            // capture subscribe frame
            if let Some(Ok(Message::Text(t))) = read.next().await {
                *obs2.lock().await = t.to_string();
            }
            // send one allDexsAssetCtxs frame with numeric fields
            let frame = serde_json::json!({
                "channel": "allDexsAssetCtxs",
                "data": {
                    "ctxs": [
                        ["", [
                            {"dayNtlVlm": 1000000.0, "prevDayPx": 100.0, "markPx": 101.0, "midPx": 100.5, "funding": 0.0001, "openInterest": 500.0, "oraclePx": 100.9}
                        ]],
                        ["xyz", [
                            {"dayNtlVlm": 2000000.0, "prevDayPx": 300.0, "markPx": 310.0, "midPx": 309.0, "funding": 0.0002, "openInterest": 600.0, "oraclePx": 309.5}
                        ]]
                    ]
                }
            });
            let _ = write.send(Message::Text(frame.to_string().into())).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = write.send(Message::Close(None)).await;
        });

        let name_maps = std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::from([
            ("".to_string(), vec!["BTC".to_string()]),
            ("xyz".to_string(), vec!["xyz:TSLA".to_string()]),
        ])));
        let (tx, mut rx) = mpsc::channel::<Vec<crate::hl_rest::CtxRow>>(64);
        let fresh = std::sync::Arc::new(WsFreshness::new());
        let handle = spawn_ctxs_stream(url, name_maps, tx, fresh.clone());
        let batch = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("timeout ctxs batch")
            .expect("channel closed");
        assert_eq!(batch.len(), 2);
        assert!(
            batch
                .iter()
                .any(|r| r.market == "BTC" && (r.funding - 0.0001).abs() < 1e-12)
        );
        assert!(
            batch
                .iter()
                .any(|r| r.market == "xyz:TSLA" && (r.funding - 0.0002).abs() < 1e-12)
        );
        // Assert subscribe frame was correctly formed (verified shape)
        let sub = observed_sub.lock().await.clone();
        let v: serde_json::Value = serde_json::from_str(&sub).expect("subscribe json");
        assert_eq!(v["method"], "subscribe");
        assert_eq!(v["subscription"]["type"], "allDexsAssetCtxs");
        // ctxs freshness stamped independently of mids
        assert!(fresh.ctxs_ms() > 0, "ctxs freshness must be stamped");
        assert_eq!(
            fresh.mids_ms(),
            0,
            "ctxs stream must not stamp the mids clock"
        );
        handle.abort();
    }

    // ── Reconnect backoff scheduling (pure) ──

    #[test]
    fn backoff_base_doubles_from_1s_and_caps_at_30s() {
        assert_eq!(backoff_base_ms(0), 1_000);
        assert_eq!(backoff_base_ms(1), 2_000);
        assert_eq!(backoff_base_ms(2), 4_000);
        assert_eq!(backoff_base_ms(3), 8_000);
        assert_eq!(backoff_base_ms(4), 16_000);
        // 32s would exceed the cap
        assert_eq!(backoff_base_ms(5), BACKOFF_CAP_MS);
        assert_eq!(backoff_base_ms(9), BACKOFF_CAP_MS);
        // never overflows, however long the outage runs
        assert_eq!(backoff_base_ms(u32::MAX), BACKOFF_CAP_MS);
    }

    #[test]
    fn backoff_delay_is_jittered_within_floor_and_cap() {
        // attempt 0: base 1s, equal jitter would land in [0.5s, 1s] but the 1s floor holds
        assert_eq!(backoff_delay_ms(0, 0.0), BACKOFF_FLOOR_MS);
        assert_eq!(backoff_delay_ms(0, 1.0), BACKOFF_FLOOR_MS);
        // attempt 2: base 4s -> [2s, 4s]
        assert_eq!(backoff_delay_ms(2, 0.0), 2_000);
        assert_eq!(backoff_delay_ms(2, 0.5), 3_000);
        assert_eq!(backoff_delay_ms(2, 1.0), 4_000);
        // at the cap the jitter still spreads retries over [15s, 30s] instead of stacking on 30s
        assert_eq!(backoff_delay_ms(7, 0.0), 15_000);
        assert_eq!(backoff_delay_ms(7, 1.0), BACKOFF_CAP_MS);
        // out-of-range / NaN jitter is clamped, never panics or escapes the window
        assert_eq!(backoff_delay_ms(2, -5.0), 2_000);
        assert_eq!(backoff_delay_ms(2, 9.0), 4_000);
        assert_eq!(backoff_delay_ms(2, f64::NAN), 2_000);
        // whole schedule stays inside [1s, 30s] for every attempt/jitter combination
        for attempt in 0..40u32 {
            for j in [0.0, 0.13, 0.5, 0.87, 1.0] {
                let d = backoff_delay_ms(attempt, j);
                assert!(
                    (BACKOFF_FLOOR_MS..=BACKOFF_CAP_MS).contains(&d),
                    "attempt {attempt} jitter {j} -> {d}ms outside [1s,30s]"
                );
            }
        }
    }

    #[test]
    fn runtime_jitter_source_is_in_unit_range() {
        for _ in 0..64 {
            let j = jitter01();
            assert!((0.0..1.0).contains(&j), "jitter {j} out of range");
        }
    }

    // ── Truthful ws_connected: freshness transitions (pure over timestamps) ──

    #[test]
    fn ws_connected_tracks_last_message_age_not_a_connect_flag() {
        let now = 1_800_000_000_000i64;
        let w = WS_FRESH_WINDOW_MS;
        // never seen -> disconnected (this is boot, and it is also what a set-once flag got wrong)
        assert!(!ws_connected_at(now, 0, 0, w));
        // one stream fresh is enough
        assert!(ws_connected_at(now, now - 1_000, 0, w));
        assert!(ws_connected_at(now, 0, now - 1_000, w));
        // exactly at the window edge is stale; just inside is fresh
        assert!(!ws_connected_at(now, now - w, 0, w));
        assert!(ws_connected_at(now, now - (w - 1), 0, w));
        // both streams stale -> disconnected, even though both sockets once connected
        assert!(!ws_connected_at(now, now - 60_000, now - 45_000, w));
        // ctxs alive while mids is dead still reads connected
        assert!(ws_connected_at(now, now - 120_000, now - 5_000, w));
        // clock skew (stamp in the future) must not read as a false outage
        assert!(ws_connected_at(now, now + 5_000, 0, w));
    }

    #[test]
    fn ws_freshness_transitions_true_then_false_as_time_advances() {
        let fresh = WsFreshness::new();
        let t0 = 1_800_000_000_000i64;
        assert!(!fresh.connected_at(t0), "no message yet -> disconnected");
        fresh.mark_mids(t0);
        assert!(fresh.connected_at(t0));
        assert!(fresh.connected_at(t0 + WS_FRESH_WINDOW_MS - 1));
        // stream dies: no further marks, so the flag flips on its own
        assert!(!fresh.connected_at(t0 + WS_FRESH_WINDOW_MS));
        assert!(!fresh.connected_at(t0 + 10 * WS_FRESH_WINDOW_MS));
        // the other stream reviving is enough to read connected again
        fresh.mark_ctxs(t0 + 10 * WS_FRESH_WINDOW_MS);
        assert!(fresh.connected_at(t0 + 10 * WS_FRESH_WINDOW_MS));
        // reconnect re-stamps mids and clears the outage
        fresh.mark_mids(t0 + 11 * WS_FRESH_WINDOW_MS);
        assert!(fresh.connected_at(t0 + 11 * WS_FRESH_WINDOW_MS));
    }

    #[test]
    fn feed_age_uses_freshest_stream_and_boot_floor() {
        let boot = 1_800_000_000_000i64;
        let now = boot + 600_000; // 10m after boot
        // nothing ever arrived -> age from boot, not from the epoch
        assert_eq!(feed_age_ms(now, 0, 0, boot), 600_000);
        // freshest of the two streams wins
        assert_eq!(feed_age_ms(now, now - 400_000, now - 30_000, boot), 30_000);
        assert_eq!(feed_age_ms(now, now - 30_000, now - 400_000, boot), 30_000);
        // a stamp older than boot (restart with stale memory) cannot beat the boot floor
        assert_eq!(feed_age_ms(now, boot - 100_000, 0, boot), 600_000);
        // clock skew clamps at 0 instead of going negative
        assert_eq!(feed_age_ms(now, now + 5_000, 0, boot), 0);
    }

    #[test]
    fn ws_freshness_feed_age_tracks_marks() {
        let fresh = WsFreshness::new();
        let boot = 1_800_000_000_000i64;
        assert_eq!(
            fresh.feed_age_ms(boot + 120_000, boot),
            120_000,
            "silent since boot"
        );
        fresh.mark_ctxs(boot + 60_000);
        assert_eq!(
            fresh.feed_age_ms(boot + 120_000, boot),
            60_000,
            "ctxs frame resets the clock"
        );
        fresh.mark_mids(boot + 119_000);
        assert_eq!(
            fresh.feed_age_ms(boot + 120_000, boot),
            1_000,
            "mids frame is freshest"
        );
    }

    #[test]
    fn liveness_requires_two_unanswered_pings_before_respawn() {
        let start = tokio::time::Instant::now();
        let mut liveness = ConnectionLiveness::new(start);
        liveness.on_ping_sent(start + Duration::from_secs(20));
        liveness.on_ping_sent(start + Duration::from_secs(40));
        assert_eq!(
            liveness.respawn_reason(start + Duration::from_secs(49), false),
            None
        );
        assert_eq!(
            liveness.respawn_reason(start + Duration::from_secs(50), false),
            Some(LivenessRespawn::UnansweredPing)
        );
    }

    #[test]
    fn liveness_inbound_frame_clears_the_ping_deadline() {
        let start = tokio::time::Instant::now();
        let mut liveness = ConnectionLiveness::new(start);
        liveness.on_ping_sent(start + Duration::from_secs(20));
        liveness.on_inbound(start + Duration::from_secs(25), true);
        assert_eq!(
            liveness.respawn_reason(start + Duration::from_secs(55), true),
            None
        );
    }

    #[test]
    fn liveness_respawns_data_stream_when_pongs_arrive_but_data_is_idle() {
        let start = tokio::time::Instant::now();
        let mut liveness = ConnectionLiveness::new(start);
        for second in [20, 40, 60] {
            let at = start + Duration::from_secs(second);
            liveness.on_ping_sent(at);
            liveness.on_inbound(at, false);
        }
        assert_eq!(
            liveness.respawn_reason(start + Duration::from_secs(60), true),
            Some(LivenessRespawn::DataIdle)
        );
    }

    fn test_liveness_timing() -> LivenessTiming {
        LivenessTiming {
            ping_interval: Duration::from_millis(10),
            deadline: Duration::from_millis(15),
            data_idle: Duration::from_millis(35),
        }
    }

    #[tokio::test]
    async fn mids_silence_after_subscribe_forces_a_respawn() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("ws://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("ws accept");
            let (_write, mut read) = ws.split();
            while read.next().await.is_some() {}
        });
        let (tx, _rx) = mpsc::channel(1);
        let fresh = WsFreshness::new();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            run_once_with_timing(&url, &[String::new()], &tx, &fresh, test_liveness_timing()),
        )
        .await
        .expect("liveness timeout must end the session");
        assert_eq!(result, Err("liveness timeout".into()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(PING_FRAME).expect("ping json"),
            serde_json::json!({"method":"ping"})
        );
    }

    #[tokio::test]
    async fn mids_data_flow_keeps_the_connection_alive() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("ws://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("ws accept");
            let (mut write, mut read) = ws.split();
            let _ = read
                .next()
                .await
                .expect("subscribe")
                .expect("subscribe frame");
            let frame =
                serde_json::json!({"channel":"allMids","data":{"mids":{"BTC":"1"}}}).to_string();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(2)) => {
                        if write.send(Message::Text(frame.clone().into())).await.is_err() { return; }
                    }
                    message = read.next() => if message.is_none() { return; },
                }
            }
        });
        let (tx, _rx) = mpsc::channel(64);
        let fresh = WsFreshness::new();
        let result = tokio::time::timeout(
            Duration::from_millis(80),
            run_once_with_timing(&url, &[String::new()], &tx, &fresh, test_liveness_timing()),
        )
        .await;
        assert!(result.is_err(), "healthy data flow must not force-respawn");
    }

    #[tokio::test]
    async fn ctxs_pongs_without_data_force_a_data_idle_respawn() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let url = format!("ws://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("ws accept");
            let (mut write, mut read) = ws.split();
            while let Some(Ok(Message::Text(text))) = read.next().await {
                if text == PING_FRAME {
                    write
                        .send(Message::Text(r#"{"channel":"pong"}"#.into()))
                        .await
                        .expect("send pong");
                }
            }
        });
        let name_maps = std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let (tx, _rx) = mpsc::channel(1);
        let fresh = WsFreshness::new();
        let result = tokio::time::timeout(
            Duration::from_millis(250),
            run_ctxs_once_with_timing(&url, &name_maps, &tx, &fresh, test_liveness_timing()),
        )
        .await
        .expect("data-idle timeout must end the session");
        assert_eq!(result, Err("liveness timeout".into()));
    }

    #[test]
    fn books_use_the_same_unanswered_ping_deadline() {
        let start = tokio::time::Instant::now();
        let mut liveness = ConnectionLiveness::new(start);
        liveness.on_ping_sent(start + PING_INTERVAL);
        liveness.on_ping_sent(start + PING_INTERVAL * 2);
        assert_eq!(
            liveness.respawn_reason(start + PING_INTERVAL + LIVENESS_DEADLINE, false),
            Some(LivenessRespawn::UnansweredPing)
        );
    }
}
