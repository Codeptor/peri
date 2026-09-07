#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(unused_mut)]
mod alerts;
mod analyst;
mod analytics;
mod api;
mod backtest;
mod config;
mod contracts;
mod digest;
mod features;
mod hl_rest;
mod hl_ws;
mod ledger;
mod news;
mod notify;
mod risk;
mod screener;
mod sizing;
mod triggers;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::sync::{Mutex, RwLock, Semaphore, broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::alerts::{AlertKind, Alerter, AnalystFailureAlert, ConsecutiveFail, DayOnce, Episode};
use crate::analyst::{Analyst, AnalystInput};
use crate::config::Config;
use crate::contracts::{Nominee, Snapshot, WsMsg};
use crate::features::FeatureEngine;
use crate::hl_rest::HlRest;
use crate::ledger::Store;
use crate::news::News;
use crate::notify::{Notify, tpl};
use crate::risk::{CloseCause, GateRefusal, GateState, Risk};
use crate::sizing::size_position;

/// Watchdog cadence and thresholds (Phase R2).
///
/// The data clock is `hl_ws::WsFreshness` — the same per-stream last-frame stamps that
/// `/api/health.ws_connected` is derived from, so there is exactly one freshness mechanism
/// in the daemon. The self-probe is the wedge detector: during both 2026-08-09 wedges the
/// process stayed alive and the streams looked fine while `/api/snapshot` never returned,
/// so liveness has to be measured from the outside, over HTTP, on a bounded timeout.
const WATCHDOG_TICK: Duration = Duration::from_secs(60);
const FEED_WARN_AGE_MS: i64 = 120_000;
const FEED_ALERT_AGE_MS: i64 = 300_000;
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const PROBE_FAIL_THRESHOLD: u32 = 2;
const FORCE_NOMINEE_TTL_MS: i64 = 10 * 60 * 1000;
const FORCE_NOMINEE_CAP: usize = 4;
const SKIP_RECHECK_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

type SkipRecheckMap = HashMap<String, (i64, f64)>;

fn prune_skip_rechecks(skip_rechecks: &mut SkipRecheckMap, now_ms: i64) {
    skip_rechecks.retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= SKIP_RECHECK_RETENTION_MS);
}

fn filter_skip_rechecks(
    candidates: Vec<(Nominee, bool)>,
    skip_rechecks: &mut SkipRecheckMap,
    now_ms: i64,
    cfg: &crate::config::ScreenerCfg,
) -> Vec<(Nominee, bool)> {
    let recheck_ms = cfg.skip_recheck_min.saturating_mul(60_000);
    candidates.into_iter().filter(|(nominee, forced_by_trenchers_den)| {
        *forced_by_trenchers_den || skip_rechecks.get(&nominee.market).is_none_or(|(last_ts, last_score)| {
            now_ms.saturating_sub(*last_ts) > recheck_ms
                || nominee.score >= *last_score + cfg.skip_recheck_score_jump
        })
    }).collect()
}

fn record_skip_recheck(skip_rechecks: &mut SkipRecheckMap, market: &str, score: f64, now_ms: i64, is_skip_or_no_entry: bool) {
    if is_skip_or_no_entry {
        skip_rechecks.insert(market.to_string(), (now_ms, score));
    }
}

/// Expire old calls, then retain the four most-recent markets. This intentionally knows
/// nothing about positions or cooldowns: the existing entry gate remains authoritative.
fn take_force_nominees(force_nominees: &mut HashMap<String, i64>, now_ms: i64) -> Vec<String> {
    force_nominees.retain(|_, queued_ms| now_ms.saturating_sub(*queued_ms) <= FORCE_NOMINEE_TTL_MS);
    let mut queued: Vec<(String, i64)> = force_nominees
        .iter()
        .map(|(market, ts)| (market.clone(), *ts))
        .collect();
    queued.sort_by_key(|(_, ts)| *ts);
    if queued.len() > FORCE_NOMINEE_CAP {
        for (market, _) in &queued[..queued.len() - FORCE_NOMINEE_CAP] {
            force_nominees.remove(market);
        }
        queued.drain(..queued.len() - FORCE_NOMINEE_CAP);
    }
    queued.into_iter().map(|(market, _)| market).collect()
}

/// Forced calls bypass momentum score only. A live row and fresh feature calculation are
/// still required before the normal analyst, gate, sizing, and paper-fill path runs.
fn forced_candidates(
    rows: &[crate::contracts::MarketRow],
    markets: &[String],
    now_ms: i64,
) -> Vec<Nominee> {
    markets
        .iter()
        .filter_map(|market| rows.iter().find(|row| row.market == *market))
        .filter_map(|row| {
            row.features.as_ref().map(|features| Nominee {
                ts: now_ms,
                market: row.market.clone(),
                side_hint: if features.r1h >= 0.0 {
                    crate::contracts::Side::Long
                } else {
                    crate::contracts::Side::Short
                },
                score: 0.0,
                features: features.clone(),
            })
        })
        .collect()
}

/// Market whose 1h realized vol drives the regime gate (Phase T1). BTC is the only row in
/// the universe every other market correlates to, and it is always in the vlm-filtered set.
const REGIME_MARKET: &str = "BTC";

#[derive(Debug, Parser)]
#[command(
    name = "kestreld",
    version,
    about = "Kestrel — autonomous paper trader daemon (paper-only)"
)]
struct Cli {
    #[arg(long, default_value = "kestreld.toml")]
    config: String,
    /// With no subcommand, `kestreld` is the daemon — exactly as it has always been.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Backtest harness: replay cached history through the strategy (no daemon, no orders).
    Backtest {
        #[command(subcommand)]
        cmd: crate::backtest::BacktestCmd,
    },
}

/// The daemon's logging setup, extracted so the backtest subcommand can share it verbatim.
fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn is_stub() -> bool {
    std::env::var("ANALYST_STUB")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn funding_seed_enabled() -> bool {
    !is_stub()
}

use crate::contracts::Position;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReviewAction {
    Hold,
    VetoClose,
    MoveStop(f64),
}

/// Map an analyst review decision to a concrete position action (spec §5: hold / move stop / veto_close).
/// Review-loop MoveStop clamp stays [0.4, 4.0] — tighten-only semantics (profit-locking,
/// different direction from entry stops). Entry stop overrides (screener block) are clamped
/// [1.0, 4.0] per user-locked amendment 2026-08-09 so analyst may not undercut the new 1.0%
/// floor; tp overrides stay [0.4, 4.0] (tight tp is fine).
fn classify_review(dec: &Option<crate::contracts::Decision>) -> ReviewAction {
    let Some(d) = dec else {
        return ReviewAction::Hold;
    };
    if d.action == "veto_close" {
        return ReviewAction::VetoClose;
    }
    if let Some(sp) = d.stop_pct {
        // Tighten-only semantics: this clamp allows profit-locking tighter than 1.0% (e.g. 0.6% trail).
        // Distinct from entry clamp [1.0, 4.0] which must sit outside noise.
        return ReviewAction::MoveStop(sp.clamp(0.4, 4.0));
    }
    ReviewAction::Hold
}

/// Build a GateState from REAL state: live daily entry count (meta table), open markets,
/// kill latch, and cooldown map from shared gate state. Fixes audit: gates previously hardwired.
async fn build_gate_state(store: &Store, gate: &Arc<Mutex<GateState>>, analyst: &str) -> GateState {
    let day_start = crate::risk::utc_day_start_ms(chrono::Utc::now().timestamp_millis());
    let daily_count = store.analyst_daily_entries(analyst, day_start).await.unwrap_or(0) as usize;
    let open_markets = store
        .open_positions_for_analyst(analyst)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.market)
        .collect();
    let g = gate.lock().await;
    GateState {
        open_markets,
        daily_count,
        last_close_ts: g.last_close_ts.iter().filter_map(|(k, v)| {
            if analyst.is_empty() { Some((k.clone(), *v)) } else { k.strip_prefix(&format!("{analyst}:")).map(|m| (m.to_string(), *v)) }
        }).collect(),
        kill_active: g.kill_active,
        veto: false,
        day_key: g.day_key.clone(),
    }
}

/// Age of the market state an entry decision rests on: `now - snapshot.ts`.
///
/// `snapshot.ts` is stamped by the ctxs ws fan, the 30s ctx poll and the universe rebuilds,
/// i.e. by every writer of the state the screener reads — so it freezes exactly when the
/// data plane freezes (it did in both 2026-08-09 wedges, while the analyst kept deciding).
///
/// A stamp in the future (clock skew) clamps to age 0, matching `hl_ws::feed_age_ms`.
///
/// LOCK DISCIPLINE: one short read guard, a single `i64` cloned out, no `.await` inside the
/// guard and no engine lock anywhere near it.
async fn snapshot_age_ms(snapshot: &Arc<RwLock<Snapshot>>, now_ms: i64) -> i64 {
    let ts = { snapshot.read().await.ts };
    now_ms.saturating_sub(ts).max(0)
}

/// Age of the state an entry decision rests on — the MAX of two independent clocks:
///
///   * `now - snapshot.ts` (R2): stamped by every writer of the snapshot,
///   * `WsFreshness::feed_age_ms` (T1): last frame on either ws stream.
///
/// The snapshot stamp alone was maskable: the 30s REST ctx poll re-stamps `snapshot.ts` on
/// every successful poll, so both mids streams could be dead while the entry path still saw
/// "fresh" state and traded on prices that stopped moving. Taking the max means EITHER clock
/// freezing refuses the entry, and neither can vouch for the other.
///
/// `boot_ms` floors the ws clock so a daemon that has not yet received a frame ages from its
/// own start rather than from the epoch.
async fn entry_data_age_ms(
    snapshot: &Arc<RwLock<Snapshot>>,
    ws_fresh: &hl_ws::WsFreshness,
    now_ms: i64,
    boot_ms: i64,
) -> i64 {
    let snap_age = snapshot_age_ms(snapshot, now_ms).await;
    snap_age.max(ws_fresh.feed_age_ms(now_ms, boot_ms))
}

/// BTC 1h realized vol from the snapshot — the regime gate's input. `None` when the BTC row
/// is absent or its features have not been computed yet (gate stays inactive).
///
/// LOCK DISCIPLINE: one short read guard, a single `f64` copied out, no `.await` inside the
/// guard and no engine lock anywhere near it.
async fn regime_vol(snapshot: &Arc<RwLock<Snapshot>>) -> Option<f64> {
    let s = snapshot.read().await;
    s.markets
        .iter()
        .find(|m| m.market == REGIME_MARKET)
        .and_then(|m| m.features.as_ref())
        .map(|f| f.vol1h)
}

/// UTC day key (`%Y-%m-%d`) for a timestamp — keys the once-per-day operator latches.
fn utc_day_key(now_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y-%m-%d")
        .to_string()
}

/// The whole entry gate chain, in one place, so the screener task reads as
/// "decide -> gate -> size -> open" and every rail is testable without Telegram or an LLM.
///
/// Segment order (and why each check lives where it does) is documented on
/// `risk::GateRefusal`:
///   1. `gate_entries_enabled` pre-check — EntriesHalted, outranks everything,
///   2. `gate_data_age` pre-check — StaleData,
///   3. `gate_entry` — the FROZEN chain, untouched,
///   4. `gate_regime` — Regime,
///   5. `gate_churn` — PerMarketCap / Cooldown (post-SL) / Paced,
///   6. `gate_min_rr` — MinRR (called by the screener entry task after `check` returns Ok).
///
/// Cheap to clone (Arc handles + a `Store` clone that shares the pool), so each per-nominee
/// task gets its own copy.
#[derive(Clone)]
struct EntryGate {
    risk: Arc<Risk>,
    store: Store,
    gate: Arc<Mutex<GateState>>,
    snapshot: Arc<RwLock<Snapshot>>,
    ws_fresh: Arc<hl_ws::WsFreshness>,
    /// WARNs once per UTC day that the regime gate has no BTC row to read.
    regime_once: Arc<DayOnce>,
    boot_ms: i64,
}

impl EntryGate {
    async fn check(&self, analyst: &str, market: &str, conviction: f64, now_ms: i64) -> Result<(), GateRefusal> {
        // 1. manual halt — applies to every analyst, including force nominations and chat.
        self.risk.gate_entries_enabled()?;

        // 2. staleness — never open on state that stopped updating (either clock).
        let age_ms = entry_data_age_ms(&self.snapshot, &self.ws_fresh, now_ms, self.boot_ms).await;
        self.risk.gate_data_age(age_ms)?;

        // The per-model gate below intentionally sees only its own book in arena mode. This
        // separate query is the shared $1000-book capacity rail when configured.
        if self.risk.cfg().global_max_concurrent > 0
            && self.store.open_positions().await.unwrap_or_default().len() >= self.risk.cfg().global_max_concurrent {
            return Err(GateRefusal::GlobalCap);
        }

        if self.risk.cfg().daily_cap > 0 {
            let day = utc_day_key(now_ms);
            let global_daily_count = match self.store.daily_count(&day).await {
                Ok(count) => count.max(0) as usize,
                Err(error) => {
                    warn!(error = %error, "global daily entry count failed; later ledger gates will fail closed");
                    0
                }
            };
            if global_daily_count >= self.risk.cfg().daily_cap {
                return Err(GateRefusal::GlobalDailyCap);
            }
        }

        // 3. frozen chain against real state (live daily count, kill latch, cooldown map).
        let state = build_gate_state(&self.store, &self.gate, analyst).await;
        self.risk.gate_entry(market, conviction, &state, now_ms)?;

        // 4. regime — a missing BTC row leaves the gate inactive, but says so once a day.
        let btc_vol1h = regime_vol(&self.snapshot).await;
        if btc_vol1h.is_none() && self.regime_once.first_today(&utc_day_key(now_ms)) {
            warn!(
                market = REGIME_MARKET,
                "regime gate inactive: no BTC features in snapshot"
            );
        }
        self.risk.gate_regime(btc_vol1h)?;

        // 5. churn. Both inputs come from the ledger, so they survive restarts; a ledger we
        // cannot read is not permission to trade, so a query failure refuses the entry.
        let day_start = crate::risk::utc_day_start_ms(now_ms);
        let market_entries_today = match self.store.analyst_market_entries(analyst, market, day_start).await {
            Ok(n) => n.max(0) as usize,
            Err(e) => {
                warn!(market, error=%e, "per-market entry count failed — refusing entry (fail closed)");
                return Err(GateRefusal::PerMarketCap);
            }
        };
        let last_close = match self.store.analyst_last_close(analyst, market).await {
            Ok(row) => row.map(|(ts, action)| (ts, CloseCause::from_action(&action))),
            Err(e) => {
                warn!(market, error=%e, "last-close lookup failed — refusing entry (fail closed)");
                return Err(GateRefusal::Cooldown);
            }
        };
        self.risk
            .gate_churn(market_entries_today, state.daily_count, last_close, now_ms)
    }

    /// The non-per-market half of the live gate state, read through the SAME helpers and in the
    /// same order as `check` — `/api/gates` therefore reports the decision an entry would get
    /// right now, not a re-derivation of it. `/api/gates` keeps `cap: 0` to mean unlimited, so
    /// dashboards remain compatible while `count` continues to show actual executed entries.
    ///
    /// LOCK DISCIPLINE: each helper takes and releases its own short guard; nothing is held
    /// across the ledger read in `build_gate_state`, and the snapshot lock is never held
    /// together with the feature engine's.
    async fn status(&self, now_ms: i64) -> GateStatus {
        let data_age_ms =
            entry_data_age_ms(&self.snapshot, &self.ws_fresh, now_ms, self.boot_ms).await;
        let state = build_gate_state(&self.store, &self.gate, "").await;
        let open_positions = self.store.open_positions().await.unwrap_or_default();
        let mut per_analyst_open_counts = std::collections::BTreeMap::new();
        for position in &open_positions {
            *per_analyst_open_counts.entry(position.analyst.clone()).or_insert(0) += 1;
        }
        let global_daily_count = self
            .store
            .daily_count(&utc_day_key(now_ms))
            .await
            .unwrap_or(self.risk.cfg().daily_cap as i64)
            .max(0) as usize;
        let btc_vol1h = regime_vol(&self.snapshot).await;
        GateStatus {
            kill_active: state.kill_active,
            global_open_count: open_positions.len(),
            per_analyst_open_counts: per_analyst_open_counts.into_iter().collect(),
            daily_count: global_daily_count,
            data_age_ms,
            stale: self.risk.gate_data_age(data_age_ms).is_err(),
            btc_vol1h,
            regime_blocking: self.risk.gate_regime(btc_vol1h).is_err(),
            cfg: self.risk.cfg().clone(),
        }
    }

    /// Entries taken per market since 00:00 UTC — the `PerMarketCap` counter for every market
    /// that traded today, in one query (`Store::market_entries_by_market`, same predicate as the
    /// per-market read `check` uses). A ledger failure reports an empty day rather than
    /// inventing counts: this feeds views and the screener's pre-filter, never a trade decision,
    /// and the authoritative fail-closed check still runs in `check`.
    async fn per_market_entries(&self, now_ms: i64) -> Vec<(String, i64)> {
        let day_start = crate::risk::utc_day_start_ms(now_ms);
        match self.store.market_entries_by_market(day_start).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error=%e, "per-market entry counts unavailable");
                Vec::new()
            }
        }
    }

    /// Cooldown windows still open at `now_ms`, with the clock that owns each one.
    ///
    /// Both clocks the gate actually compares against are reported, and the LONGER one wins the
    /// `until_ts` and the cause:
    ///   * base window — `GateState::last_close_ts` (in memory) + `cooldown_min`, exactly what
    ///     `gate_entry` reads. It is empty after a restart, and this view says so honestly
    ///     rather than inventing a window the gate would not enforce.
    ///   * post-SL window — the ledger's last close + `cooldown_after_sl_min`, what `gate_churn`
    ///     reads. Ledger-backed, so it survives restarts.
    ///
    /// Candidates are only markets that closed inside the longest window (plus whatever the
    /// in-memory map still holds), so the per-market `last_close` calls stay a handful.
    async fn cooldowns(&self, now_ms: i64) -> Vec<CooldownView> {
        let cfg = self.risk.cfg();
        let base_ms = (cfg.cooldown_min as i64).saturating_mul(60_000);
        let sl_ms = (cfg.cooldown_after_sl_min as i64).saturating_mul(60_000);
        let mem: HashMap<String, i64> = { self.gate.lock().await.last_close_ts.clone() };
        let mut candidates: std::collections::BTreeSet<String> = self
            .store
            .markets_closed_since(now_ms - base_ms.max(sl_ms))
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        candidates.extend(mem.keys().cloned());

        let mut out = Vec::new();
        for market in candidates {
            let mut until = mem.get(&market).map(|ts| ts + base_ms);
            let mut cause = CloseCause::Other;
            if let Ok(Some((ts, action))) = self.store.last_close(&market).await
                && CloseCause::from_action(&action) == CloseCause::Sl
            {
                let sl_until = ts + sl_ms;
                if until.is_none_or(|u| sl_until >= u) {
                    until = Some(sl_until);
                    cause = CloseCause::Sl;
                }
            }
            if let Some(until_ts) = until
                && until_ts > now_ms
            {
                out.push(CooldownView {
                    market,
                    until_ts,
                    cause,
                });
            }
        }
        out
    }

    /// Markets a per-market rail would refuse right now: already open (`DupMarket`), per-market
    /// daily cap spent (`PerMarketCap`), or inside a cooldown window (`Cooldown`, base or
    /// post-SL). The screener excludes these before scoring, so nominations stop burning analyst
    /// calls on markets the gate is certain to reject.
    ///
    /// Deliberately only the MARKET-SCOPED rails: kill switch, daily cap, pacing, regime and
    /// staleness are global, and benching the whole universe on them would hide the ranking the
    /// dashboard shows. Computed once per screener tick from three ledger reads.
    async fn excluded_markets(&self, now_ms: i64) -> HashSet<String> {
        let cap = self.risk.cfg().per_market_daily_cap as i64;
        let mut out: HashSet<String> = self
            .store
            .open_positions()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.market)
            .collect();
        out.extend(
            self.per_market_entries(now_ms)
                .await
                .into_iter()
                .filter(|(_, n)| *n >= cap)
                .map(|(m, _)| m),
        );
        out.extend(self.cooldowns(now_ms).await.into_iter().map(|c| c.market));
        out
    }
}

/// One market's open cooldown window: when it lifts, and which clock set it.
#[derive(Debug, Clone, PartialEq)]
struct CooldownView {
    market: String,
    until_ts: i64,
    cause: CloseCause,
}

/// Live readings of the global (non-per-market) rails, plus the knobs they compare against.
/// `stale` and `regime_blocking` are the gates' own verdicts (`gate_data_age` / `gate_regime`),
/// not a second opinion computed from the raw numbers.
#[derive(Debug, Clone)]
struct GateStatus {
    kill_active: bool,
    global_open_count: usize,
    per_analyst_open_counts: Vec<(String, usize)>,
    daily_count: usize,
    data_age_ms: i64,
    stale: bool,
    btc_vol1h: Option<f64>,
    regime_blocking: bool,
    cfg: crate::config::RiskCfg,
}

/// Land a risk-gate refusal where an operator can see it: structured log + appended to the
/// decision row's `reason` (so `/api/decisions` explains why a decided `open` never opened),
/// plus a once-per-UTC-day TG alert when the daily cap is what stopped it.
///
/// Best-effort by construction: a failed ledger write or a failed Telegram push only costs
/// visibility, never the refusal itself (which already happened at the call site).
/// Individual refusals never reach Telegram — there are dozens a day and they are normal
/// operation. The ledger keeps every one (`gate_refused:<kind>` on the decision row) and the
/// daily digest counts them by kind. The single exception is the day's entry budget running
/// out, which changes what the daemon will do for the rest of the day, and fires once.
async fn record_gate_refusal(
    store: &Store,
    alerter: &Arc<Alerter>,
    daily_cap_once: &Arc<DayOnce>,
    decision_id: i64,
    market: &str,
    refusal: &GateRefusal,
    daily_cap: usize,
) {
    info!(
        market,
        refusal = refusal.as_str(),
        "risk gate refused entry"
    );
    if decision_id != 0 {
        let suffix = format!(" gate_refused:{}", refusal.as_str());
        if let Err(e) = store.append_decision_reason(decision_id, &suffix).await {
            warn!(error=%e, "append gate refusal to decision reason failed");
        }
    }
    if matches!(refusal, GateRefusal::DailyCap | GateRefusal::GlobalDailyCap) {
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if daily_cap_once.first_today(&day) {
            let count = store.daily_count(&day).await.unwrap_or(daily_cap as i64) as usize;
            alerter
                .send(
                    AlertKind::DailyCap,
                    &tpl::daily_cap(count, daily_cap, "00:00 UTC"),
                )
                .await;
        }
    }
}

/// Pure: the account block the entry prompt reads.
///
/// `kill_budget_used_pct` is the share of the kill-switch drawdown budget already spent — 0
/// while the book is flat or up on the day, 100 exactly at the floor where entries stop. It
/// exists so conviction is not priced the same at -8% on the day as at breakeven.
fn account_state(equity: f64, day_open: f64, kill_switch_pct: f64) -> crate::analyst::AccountState {
    let day_pnl_pct = if day_open.abs() > 1e-9 {
        (equity - day_open) / day_open * 100.0
    } else {
        0.0
    };
    let kill_budget_used_pct = if kill_switch_pct > 0.0 {
        (-day_pnl_pct / kill_switch_pct * 100.0).max(0.0)
    } else {
        0.0
    };
    crate::analyst::AccountState {
        equity,
        day_pnl_pct,
        kill_budget_used_pct,
    }
}

/// Record cooldown for a closed market (spec §5: per-market 30min cooldown after close).
pub(crate) async fn record_close_cooldown(gate: &Arc<Mutex<GateState>>, market: &str, now_ms: i64) {
    let mut g = gate.lock().await;
    g.last_close_ts.insert(market.to_string(), now_ms);
}

async fn record_analyst_calls(
    store: &Store,
    market: &str,
    trigger: &str,
    analyst: &str,
    calls: &[crate::analyst::AnalystCall],
) {
    let ts = chrono::Utc::now().timestamp_millis();
    for call in calls {
        if let Err(error) = sqlx::query("INSERT INTO analyst_calls (ts,market,trigger,prompt,response_raw,outcome_kind,parsed_json,latency_ms,analyst) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)")
            .bind(ts).bind(market).bind(trigger).bind(&call.prompt).bind(&call.response_raw).bind(&call.outcome_kind).bind(&call.parsed_json).bind(call.latency_ms as i64).bind(analyst).execute(store.pool()).await {
            warn!(%error, "analyst call audit write failed");
        }
    }
}

/// Apply a review action to an open position. Returns true if a close/stop was applied.
///
/// MONEY GUARD: `mark <= 0.0` (or non-finite / missing propagated as `0.0`) is
/// treated as "unknown" — veto_close must WARN and NOT close when its mark is
/// invalid. Close on next review instead. Prevents pricing with `0.0`
/// fallback-artifacts that would fill at `0` or equity-spike / false kill-trip.
async fn apply_review_action(
    store: &Store,
    gate: &Arc<Mutex<GateState>>,
    pos: &Position,
    mark: f64,
    action: ReviewAction,
    book: Option<&crate::contracts::L2Book>,
) -> bool {
    match action {
        ReviewAction::Hold => false,
        ReviewAction::VetoClose => {
            if mark <= 0.0 || !mark.is_finite() {
                warn!(market=%pos.market, mark, "veto_close skipped: invalid mark (unknown) — will retry next review");
                return false;
            }
            match store.close_position(pos.id, mark, "veto_close", book).await {
                Ok(_) => {
                    record_close_cooldown(gate, &pos.market, chrono::Utc::now().timestamp_millis())
                        .await;
                    true
                }
                Err(e) => {
                    warn!(error=%e, "veto_close failed");
                    false
                }
            }
        }
        ReviewAction::MoveStop(stop_pct) => {
            // Tighten-only rule: longs may only raise sl, shorts only lower it.
            let new_sl = match pos.side {
                crate::contracts::Side::Long => pos.entry_px * (1.0 - stop_pct / 100.0),
                crate::contracts::Side::Short => pos.entry_px * (1.0 + stop_pct / 100.0),
            };
            let tightens = match pos.side {
                crate::contracts::Side::Long => new_sl > pos.sl_px,
                crate::contracts::Side::Short => new_sl < pos.sl_px,
            };
            if !tightens {
                return false;
            }
            match store.update_stop(pos.id, new_sl).await {
                Ok(()) => true,
                Err(e) => {
                    warn!(error=%e, "move stop failed");
                    false
                }
            }
        }
    }
}

/// HIP-3 perps are 24/7 — this seam is kept for a future session-aware variant
/// but currently returns `"open"` unconditionally for all markets (native and `xyz:`).
///
/// Ground truth 2026-08-09 that overturned the original US-RTH assumption:
/// (a) `allMids` ws streams `xyz:` mids continuously through weekends on the live daemon;
/// (b) user-side proof: caller trading `xyz:SPCX` on a Saturday ("spcx flying – on a weekend").
/// RTH 09:30-16:00 ET Mon-Fri described the *underlying equity venue* only, not the perp
/// (oracle-driven perpetual swaps trade continuously). Analyst prompt slot `market_hours={}`
/// already exists and will now see `open` at all times — no prompt shape change needed.
///
/// Kept as a function so a future session-aware variant can be grafted without touching call sites.
fn is_xyz_open(_market: &str, _now_ms: i64) -> &'static str {
    "open"
}

/// SHARED-STATE LOCK INVARIANT (load-bearing — do not relax)
///
/// Two locks guard the hot path: `snapshot: Arc<RwLock<Snapshot>>` and
/// `engine: Arc<Mutex<FeatureEngine>>`. The rule for every task that touches them:
///
///   1. NEVER hold both at the same time.
///   2. NEVER hold either across an `.await` that does I/O (store, HTTP, ws).
///
/// Shape that satisfies it: short `snapshot.read()` → clone out → engine work under the
/// engine guard alone (pure, no awaits) → assignment-only `snapshot.write()`.
///
/// The read→compute→write split costs atomicity, so the one path whose semantics are
/// "preserve the snapshot's mids" (the 30s ctx poll) re-applies live mids inside its
/// write guard via `hl_rest::overlay_live_mids`. The universe rebuilds (bootstrap,
/// hourly) intentionally do NOT: replacing mids from REST is their hourly heal for any
/// market whose ws mid stopped ticking.
///
/// Why (post-mortem 2026-08-09, two production wedges): the mids fan took
/// snapshot.write → engine.lock while the ctxs fan and 30s ctx poll took
/// engine.lock → snapshot.write. That ABBA inversion parked both tasks permanently, and
/// because `tokio::sync::RwLock` is task-fair, a queued writer starves every later
/// reader — so `/api/snapshot` and `/api/positions` hung forever (curl never returned)
/// while health/trades/equity/decisions/news, which never touch the snapshot, kept
/// serving. Equity rows stopped dead at 12:30:00Z (run 1) and 13:24:33Z (run 2); the ws
/// "stream ended" and REST `Shape` errors in the log were coincident, not causal.
///
/// `api::STATE_TIMEOUT` is the backstop: if this invariant is ever broken again, readers
/// return 503 in 3s instead of hanging.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Subcommands are tools, not the daemon: they run and exit before any daemon setup —
    // no ledger, no ws, no API, no .env (they touch public endpoints only). Parsing first
    // costs the daemon path nothing: a successful parse prints nothing.
    let mut cli = Cli::parse();
    if let Some(Command::Backtest { cmd }) = cli.command.take() {
        init_tracing();
        return backtest::run(cmd, &cli.config).await;
    }

    println!(
        "kestreld v{} — Kestrel paper-only daemon",
        env!("CARGO_PKG_VERSION")
    );

    // load .env from worktree root and kestreld dir
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path("../.env");
    let _ = dotenvy::from_path(".env");

    init_tracing();

    info!(config = %cli.config, "starting");

    let cfg = Config::load(&cli.config).context("load config")?;
    info!(port = cfg.server.port, top_k = cfg.screener.top_k, models = cfg.analyst.enabled_models().len(), "config loaded");

    // Store open - persistent file
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let store_path = format!("sqlite:{}/kestreld.db?mode=rwc", cwd.display());
    let store = Store::open(&store_path).await.context("open store")?;
    info!("store opened");

    // ensure bankroll in meta
    if store.get_meta("bankroll").await?.is_none() {
        store
            .set_meta("bankroll", &cfg.sizing.bankroll.to_string())
            .await?;
    }
    // day open equity handling - init if missing
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    if store.day_open_equity(&today).await?.is_none() {
        store
            .set_day_open_equity(&today, cfg.sizing.bankroll)
            .await?;
    }

    let snapshot = Arc::new(RwLock::new(Snapshot {
        ts: chrono::Utc::now().timestamp_millis(),
        markets: vec![],
    }));
    let nominees = Arc::new(RwLock::new(Vec::<Nominee>::new()));
    // Candle cache feeding the analyst TECHNICALS block: boot-seeded per tracked market,
    // refreshed by two tasks (3min nominees' 15m / 15min full universe), staleness-gated
    // at prompt build (>30min old ⇒ treated as missing, decide proceeds without a series).
    let candle_cache: CandleCache = Arc::new(RwLock::new(HashMap::new()));
    let (bcast_tx, _rx) = broadcast::channel::<WsMsg>(256);
    let ws_fresh = Arc::new(hl_ws::WsFreshness::new());
    let markets_tracked = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    // One boot stamp for every age clock (watchdog feed age + entry staleness), so a daemon
    // that has never seen a ws frame ages from its own start instead of from the epoch.
    let boot_ms = chrono::Utc::now().timestamp_millis();

    // Risk gate shared state (declared early: fan task + screener + equity all consume it)
    let gate_state = Arc::new(Mutex::new(GateState::new()));

    // News
    let news = Arc::new(News::new(store.clone(), cfg.news.clone()));
    let (force_nominee_tx, mut force_nominee_rx) = mpsc::channel::<Vec<String>>(64);
    let analyst_failure_streak = Arc::new(AtomicU64::new(0));
    let mut analysts: Vec<Arc<Analyst>> = if is_stub() || !cfg.analyst.enabled { Vec::new() } else {
        {
            let retrieval = Arc::new(crate::analyst::WebRetriever::new());
            cfg.analyst.enabled_models().into_iter().map(|model| Arc::new(Analyst::for_model_with_retriever(cfg.analyst.clone(), model, retrieval.clone()))).collect()
        }
    };
    // The interactive endpoint deliberately has one selected seat; the screener below fans out
    // to every seat. Failure counters are owned by each Analyst instance.
    let analyst = analysts.iter().find(|a| a.model_id() == cfg.analyst.chat_model_id()).cloned()
        .or_else(|| analysts.first().cloned());
    // Spawn RSS poller
    let _rss_handle = news.clone().spawn_rss();

    // Feature engine
    let engine = Arc::new(Mutex::new(FeatureEngine::new()));
    // debug task
    let engine_dbg = engine.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            let eng = engine_dbg.lock().await;
            let btc_len = eng.raw_len("BTC").unwrap_or(0);
            let sol_len = eng.raw_len("SOL").unwrap_or(0);
            let tsla_len = eng.raw_len("xyz:TSLA").unwrap_or(0);
            debug!(btc_len, sol_len, tsla_len, "engine raw len");
            if let Some(f) = eng.features("BTC") {
                debug!(?f, "BTC features");
            }
            if let Some(f) = eng.features("xyz:TSLA") {
                debug!(?f, "TSLA features");
            }
        }
    });

    // HL REST client. Built before the API because `/api/analytics` replays veto
    // counterfactuals through its candle endpoint.
    let hl_rest = HlRest::new(HlRest::MAINNET);

    // Risk rails + the entry gate chain: assembled ONCE here and shared by the screener (which
    // trades through it) and `/api/gates` (which reports it), so the endpoint can never describe
    // a gate the trader is not actually running.
    let risk = Arc::new(Risk::new(cfg.risk.clone()));
    // "regime gate has no BTC row" WARN latch — log-only (not an alert kind).
    let regime_once = Arc::new(DayOnce::new());
    let entry_gate = EntryGate {
        risk: risk.clone(),
        store: store.clone(),
        gate: gate_state.clone(),
        snapshot: snapshot.clone(),
        ws_fresh: ws_fresh.clone(),
        regime_once: regime_once.clone(),
        boot_ms,
    };

    // AppState for API
    let app_state = api::AppState {
        store: store.clone(),
        snapshot: snapshot.clone(),
        nominees: nominees.clone(),
        tx: bcast_tx.clone(),
        start,
        ws_fresh: ws_fresh.clone(),
        markets_tracked: markets_tracked.clone(),
        news: news.clone(),
        entry_gate: entry_gate.clone(),
        hl: hl_rest.clone(),
        force_nominees: force_nominee_tx,
        analyst_failure_streak: analyst_failure_streak.clone(),
        analysts_enabled: cfg.analyst.enabled,
        analyst: analyst.clone(),
        analyst_model: cfg.analyst.chat_model_id(),
        analyst_roster: cfg
            .analyst
            .enabled_models()
            .into_iter()
            .map(|model| model.id)
            .collect(),
        sizing: cfg.sizing.clone(),
        chat_gate: gate_state.clone(),
    };
    let router = api::build_router(app_state);
    let addr = format!("127.0.0.1:{}", cfg.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.context("bind")?;
    info!(addr=%addr, "api listening");
    let api_handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            error!(error=%e, "api server error");
        }
    });

    // Notify
    let notifier = Notify::new(cfg.notify.bot_token_env.clone(), cfg.notify.chat_id.clone());
    // Operator alerts ride the same notifier, rate-limited 1/min per kind and episode-deduped.
    let alerter = Arc::new(Alerter::new(notifier.clone()));
    let analyst_alert = Arc::new(Mutex::new(AnalystFailureAlert::default()));
    // "first daily-cap refusal of the day" latch — shared by the per-nominee entry tasks.
    let daily_cap_once = Arc::new(DayOnce::new());

    // HL clients (the REST client itself is built above, next to the API)
    let hl_rest_clone = hl_rest.clone();
    // Positional name maps for allDexsAssetCtxs ws stream: dex string "" (native) -> ordered market names.
    // Tolerates empty until seeded (ws decoder skips dexes not yet present).
    let name_maps: std::sync::Arc<RwLock<HashMap<String, Vec<String>>>> =
        std::sync::Arc::new(RwLock::new(HashMap::new()));

    // Universe bootstrap
    let snapshot_clone = snapshot.clone();
    let engine_clone = engine.clone();
    let markets_tracked_clone = markets_tracked.clone();
    // initial fetch — self-heals without relying on hydration order: bootstrap seeds
    // the vlm-filtered universe, then merges open-position markets from store (union).
    let init_fetch = {
        let hl = hl_rest.clone();
        let snap = snapshot.clone();
        let eng = engine.clone();
        let mt = markets_tracked.clone();
        let cfg_univ = cfg.universe.clone();
        let store_boot = store.clone();
        async move {
            match fetch_universe(&hl, &cfg_univ).await {
                Ok(rows) => {
                    // LOCK ORDER INVARIANT (see module note above main): read snapshot →
                    // release → engine work → release → assignment-only snapshot write.
                    let ts = chrono::Utc::now().timestamp_millis();
                    let snapshot_mids: HashMap<String, f64> = {
                        let snap_r = snap.read().await;
                        snap_r
                            .markets
                            .iter()
                            .map(|m| (m.market.clone(), m.mid))
                            .collect()
                    };
                    let ctx_mids: HashMap<String, f64> =
                        rows.iter().map(|r| (r.market.clone(), r.mid)).collect();
                    // UNION source: store I/O with NO lock held.
                    let open_markets: Vec<String> = store_boot
                        .open_positions()
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.market)
                        .collect();
                    let (markets, filtered_len) = {
                        let mut eng = eng.lock().await;
                        for r in &rows {
                            let z = eng.funding_z(&r.market, r.funding, ts);
                            eng.on_ctx(r, z);
                        }
                        let mut markets: Vec<crate::contracts::MarketRow> = rows
                            .into_iter()
                            .map(|r| {
                                let feats = eng.features(&r.market);
                                crate::contracts::MarketRow {
                                    market: r.market,
                                    mid: r.mid,
                                    mark: r.mark,
                                    oracle: r.oracle,
                                    funding: r.funding,
                                    open_interest: r.open_interest,
                                    day_ntl_vlm: r.day_ntl_vlm,
                                    prev_day_px: r.prev_day_px,
                                    features: feats,
                                }
                            })
                            .collect();
                        // markets_tracked counts only filtered_len (documented writer).
                        let filtered_len = markets.len();
                        let engine_mids: HashMap<String, f64> = open_markets
                            .iter()
                            .filter_map(|om| eng.latest_mid(om).map(|mid| (om.clone(), mid)))
                            .collect();
                        let feats_fn = |mk: &str| eng.features(mk);
                        crate::hl_rest::ensure_position_markets(
                            &mut markets,
                            &open_markets,
                            &snapshot_mids,
                            &engine_mids,
                            &ctx_mids,
                            feats_fn,
                        );
                        (markets, filtered_len)
                    };
                    mt.store(filtered_len, Ordering::SeqCst);
                    // Full replace with REST mids — unchanged semantics for the universe
                    // rebuilds (this is the once-an-hour heal for any market whose ws mid
                    // went silent). Only the 30s poll, which preserves snapshot mids by
                    // design, needs `overlay_live_mids` to stay exact across the split.
                    let total = {
                        let mut snap_w = snap.write().await;
                        *snap_w = Snapshot { ts, markets };
                        snap_w.markets.len()
                    };
                    info!(markets=%total, filtered=%filtered_len, "universe bootstrap done (union with positions)");
                }
                Err(e) => warn!(error=%e, "universe bootstrap failed"),
            }
        }
    };
    init_fetch.await;
    // Seed name_maps from REST universe ordering (bootstrap) — reused for ws positional mapping.
    {
        let seeded = seed_name_maps(&hl_rest, &cfg.universe, &name_maps).await;
        if seeded == 0 {
            warn!(
                "name_maps bootstrap seeded 0 dexes — ctxs stream will degrade until hourly refresh"
            );
        } else {
            info!(dexes = seeded, "name_maps seeded");
        }
    }

    // Batch 3: fundingHistory boot seed — last-7d per tracked market into funding ring.
    // Must run after universe bootstrap + BEFORE the screener's first tick. Throttled:
    // 100ms between calls, extra 1s pause every 20, log every 25. On failure warn+continue.
    // Skipped entirely when ANALYST_STUB=1 (fast smoke boots).
    boot_seed_funding(&hl_rest, &engine, &snapshot).await;

    // Batch 3b: candle boot seed — 15m x64 + 4h x60 per tracked market for the analyst
    // TECHNICALS block. Same throttle/log pattern and stub-skip as the fundingHistory seed.
    boot_seed_candles(&hl_rest, &snapshot, &candle_cache).await;

    // Boot alert — after the seed so the equity number comes from a warmed daemon.
    // Spawned, never awaited: startup must not stall behind a slow Telegram API.
    // LOCK DISCIPLINE: snapshot read guard is dropped before the store I/O.
    {
        let alerter_boot = alerter.clone();
        let store_boot_alert = store.clone();
        let snapshot_boot = snapshot.clone();
        let notifier_boot = notifier.clone();
        let markets_boot = markets_tracked.clone();
        let bankroll = cfg.sizing.bankroll;
        tokio::spawn(async move {
            let marks: HashMap<String, f64> = {
                let s = snapshot_boot.read().await;
                s.markets
                    .iter()
                    .map(|m| (m.market.clone(), m.mid))
                    .collect()
            };
            let equity = store_boot_alert.equity(&marks).await.unwrap_or(bankroll);
            alerter_boot
                .send(
                    AlertKind::Boot,
                    &tpl::boot(
                        equity,
                        markets_boot.load(Ordering::SeqCst),
                        start.elapsed().as_secs(),
                    ),
                )
                .await;
            // Back-fill a missed day roll: if the daemon was down at midnight, yesterday never
            // got its digest. `deliver` is idempotent against the markdown ledger, so this is a
            // no-op on every boot that follows a roll the daemon was alive for.
            let yesterday =
                crate::risk::utc_day_start_ms(chrono::Utc::now().timestamp_millis()) - 86_400_000;
            crate::digest::deliver(
                &store_boot_alert,
                &notifier_boot,
                std::path::Path::new(crate::digest::LEDGER_DIR_DEFAULT),
                yesterday,
            )
            .await;
        });
    }

    // Watchdog (Phase R2): data-feed age + external self-probe. It is the only task that
    // can tell the operator the daemon has gone quiet or wedged, so it owns no shared state
    // beyond the freshness clock — its episode latches are plain task-local structs.
    {
        let ws_fresh_wd = ws_fresh.clone();
        let alerter_wd = alerter.clone();
        let probe_url = format!("http://127.0.0.1:{}/api/snapshot", cfg.server.port);
        tokio::spawn(async move {
            let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
                Ok(c) => c,
                Err(e) => {
                    error!(error=%e, "watchdog probe client build failed — watchdog disabled");
                    return;
                }
            };
            let mut feed_episode = Episode::new();
            let mut probe = ConsecutiveFail::new(PROBE_FAIL_THRESHOLD);
            let mut ticker = tokio::time::interval(WATCHDOG_TICK);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let now_ms = chrono::Utc::now().timestamp_millis();

                // (a) data freshness — WARN at 120s, TG alert once per episode at 300s.
                let age_ms = ws_fresh_wd.feed_age_ms(now_ms, boot_ms);
                if age_ms >= FEED_WARN_AGE_MS {
                    warn!(
                        age_s = age_ms / 1000,
                        "ws data feed stale (no frames on either stream)"
                    );
                }
                if feed_episode.observe(age_ms >= FEED_ALERT_AGE_MS) {
                    alerter_wd
                        .send(
                            AlertKind::StreamDeath,
                            &tpl::stream_death(age_ms / 1000, "allMids+allDexsAssetCtxs"),
                        )
                        .await;
                }

                // (b) self-probe — the wedge detector. 503 counts as a failure: that is the
                // R1 state-unavailable path, which means the shared state is unreachable.
                let ok = match client.get(&probe_url).send().await {
                    Ok(resp) => resp.status().is_success(),
                    Err(e) => {
                        debug!(error=%e, "watchdog probe failed");
                        false
                    }
                };
                let wedged = probe.observe(ok);
                debug!(
                    ok,
                    consecutive_fails = probe.count(),
                    age_s = age_ms / 1000,
                    "watchdog tick"
                );
                if wedged {
                    alerter_wd
                        .send(
                            AlertKind::Wedge,
                            &tpl::wedge("/api/snapshot", PROBE_FAIL_THRESHOLD),
                        )
                        .await;
                }
            }
        });
    }

    // l2Book depth feed: subscribed to the bootstrapped universe. The supervisor receives each
    // hourly snapshot membership update so fresh listings get depth before their first fill.
    let book_cache: hl_ws::BookCache = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let (book_cmd_tx, book_cmd_rx) = mpsc::channel::<hl_ws::BookCmd>(32);
    let book_markets = snapshot
        .read()
        .await
        .markets
        .iter()
        .map(|row| row.market.clone())
        .collect();
    let books = hl_ws::spawn_books_stream(
        "wss://api.hyperliquid.xyz/ws".to_string(),
        book_markets,
        book_cmd_rx,
        book_cache.clone(),
    );
    // Hydrate subscriptions for positions reopened after daemon restart.
    if let Ok(positions) = store.open_positions().await {
        for p in positions {
            let _ = book_cmd_tx.try_send(hl_ws::BookCmd::Sub(p.market));
        }
    }
    // Adaptive tool-use wiring: give free-tier chat models the three read-only tools.
    // Orderbook/funding/candles all via existing caches (no new fetchers), max 1 tool round.
    // Responses-api models (muse-spark) gracefully fallback to one-shot (TODO in analyst.rs).
    {
        let tool_executor = crate::analyst::ToolExecutor::new(book_cache.clone(), engine.clone(), candle_cache.clone());
        analysts = analysts.into_iter().map(|a| Arc::new((*a).clone().with_tool_executor(tool_executor.clone()))).collect();
        // AppState's analyst (for the chat endpoint) keeps its original one-shot path — the screener's
        // arena seats (analysts) are the ones that gain tool-use.
        let _ = &analyst;
    }

    // Hourly universe refresh (also refreshes name_maps for positional mapping)
    let hl_for_univ = hl_rest.clone();
    let cfg_univ2 = cfg.universe.clone();
    let snap_for_univ = snapshot.clone();
    let eng_for_univ = engine.clone();
    let mt_for_univ = markets_tracked.clone();
    let name_maps_hourly = name_maps.clone();
    let store_hourly = store.clone();
    let books_hourly = books.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(3600));
        loop {
            interval.tick().await;
            match fetch_universe(&hl_for_univ, &cfg_univ2).await {
                Ok(rows) => {
                    // LOCK ORDER INVARIANT: short snapshot read → store/engine work with no
                    // snapshot guard → assignment-only snapshot write. Nothing awaits I/O
                    // (store, HTTP) while either lock is held.
                    let ts = chrono::Utc::now().timestamp_millis();
                    let snapshot_mids: HashMap<String, f64> = {
                        let snap_r = snap_for_univ.read().await;
                        snap_r
                            .markets
                            .iter()
                            .map(|m| (m.market.clone(), m.mid))
                            .collect()
                    };
                    let ctx_mids: HashMap<String, f64> =
                        rows.iter().map(|r| (r.market.clone(), r.mid)).collect();
                    // UNION source: add open-position markets missing from filtered universe.
                    let open_markets: Vec<String> = store_hourly
                        .open_positions()
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.market)
                        .collect();
                    let (markets, filtered_len) = {
                        let mut eng = eng_for_univ.lock().await;
                        for r in &rows {
                            let z = eng.funding_z(&r.market, r.funding, ts);
                            eng.on_ctx(r, z);
                        }
                        let mut markets: Vec<crate::contracts::MarketRow> = rows
                            .into_iter()
                            .map(|r| {
                                let feats = eng.features(&r.market);
                                crate::contracts::MarketRow {
                                    market: r.market,
                                    mid: r.mid,
                                    mark: r.mark,
                                    oracle: r.oracle,
                                    funding: r.funding,
                                    open_interest: r.open_interest,
                                    day_ntl_vlm: r.day_ntl_vlm,
                                    prev_day_px: r.prev_day_px,
                                    features: feats,
                                }
                            })
                            .collect();
                        let filtered_len = markets.len();
                        let engine_mids: HashMap<String, f64> = open_markets
                            .iter()
                            .filter_map(|om| eng.latest_mid(om).map(|mid| (om.clone(), mid)))
                            .collect();
                        let feats_fn = |mk: &str| eng.features(mk);
                        crate::hl_rest::ensure_position_markets(
                            &mut markets,
                            &open_markets,
                            &snapshot_mids,
                            &engine_mids,
                            &ctx_mids,
                            feats_fn,
                        );
                        (markets, filtered_len)
                    };
                    mt_for_univ.store(filtered_len, Ordering::SeqCst);
                    // Full replace with REST mids — unchanged semantics: this is the
                    // hourly heal for any market whose ws mid stopped ticking. (The 30s
                    // poll preserves snapshot mids instead, hence its overlay.)
                    let (total, book_markets) = {
                        let mut snap_w = snap_for_univ.write().await;
                        *snap_w = Snapshot { ts, markets };
                        let book_markets: Vec<String> =
                            snap_w.markets.iter().map(|row| row.market.clone()).collect();
                        (snap_w.markets.len(), book_markets)
                    };
                    books_hourly.update_markets(&book_markets);
                    // refresh name_maps after snapshot so ws decoder stays aligned with REST
                    // ordering — two HTTP round-trips, deliberately outside every guard.
                    let n = seed_name_maps(&hl_for_univ, &cfg_univ2, &name_maps_hourly).await;
                    info!(markets=%total, filtered=%filtered_len, dexes = n, "universe hourly refresh done (name_maps refreshed, union with positions)");
                }
                Err(e) => warn!(error=%e, "universe refresh failed"),
            }
        }
    });

    // Mids WS
    let (mids_tx, mut mids_rx) = mpsc::channel::<HashMap<String, f64>>(64);
    let ws_url = "wss://api.hyperliquid.xyz/ws".to_string();
    let dexs = cfg.universe.dexs.clone();
    let _ws_handle = hl_ws::spawn_mids_stream(ws_url.clone(), dexs, mids_tx, ws_fresh.clone());

    // allDexsAssetCtxs ctxs WS — real-time funding/OI/vlm stream (bounded 64, jittered 1s→30s reconnect, resubscribe)
    let (ctxs_tx, mut ctxs_rx) = mpsc::channel::<Vec<crate::hl_rest::CtxRow>>(64);
    let _ctxs_handle =
        hl_ws::spawn_ctxs_stream(ws_url.clone(), name_maps.clone(), ctxs_tx, ws_fresh.clone());
    // Fan ctxs batches into engine.on_ctx + snapshot ctx fields (funding/OI/vlm/mark/oracle; mids stay from mids feed)
    // Correct: ws fan is update-only — it may UPDATE ctx fields of markets already in the
    // filtered snapshot, but must NEVER insert a market that isn't already there.
    // Snapshot membership is written only by universe bootstrap / hourly refresh / 30s poll
    // (the vlm-filtered universe). This keeps markets_tracked == filtered count.
    {
        let engine_ctx = engine.clone();
        let snapshot_ctx = snapshot.clone();
        tokio::spawn(async move {
            while let Some(batch) = ctxs_rx.recv().await {
                // Single wall-clock read reused below for both the funding hour-bucket and the
                // snapshot timestamp — one read per batch, no clock access inside features.rs.
                let now_ms = chrono::Utc::now().timestamp_millis();
                // LOCK ORDER INVARIANT: the engine guard is fully released before the
                // snapshot guard is taken. Holding both (engine→snapshot here, while the
                // mids fan held snapshot→engine) is the ABBA inversion that wedged the API.
                let feats: HashMap<String, Option<crate::contracts::Features>> = {
                    let mut eng = engine_ctx.lock().await;
                    for row in &batch {
                        let z = eng.funding_z(&row.market, row.funding, now_ms);
                        eng.on_ctx(row, z);
                    }
                    batch
                        .iter()
                        .map(|r| (r.market.clone(), eng.features(&r.market)))
                        .collect()
                };
                // Refresh snapshot ctx fields without touching mids (authoritative from mids feed)
                // — update-only via pure helper (no inserts for unknown markets). No awaits inside.
                let mut snap = snapshot_ctx.write().await;
                crate::hl_rest::merge_ctx_rows(&mut snap.markets, &batch);
                // Update features only for markets already in snapshot (update-only)
                for entry in snap.markets.iter_mut() {
                    if let Some(f) = feats.get(&entry.market) {
                        entry.features = f.clone();
                    }
                }
                snap.ts = now_ms;
            }
        });
    }

    // Books drive depth-aware impact fills in the ledger; flat slippage stays as fallback when
    // they are stale. The supervisor above owns the live subscription membership.
    // health.ws_connected needs no mirror task: it is derived on demand from stream
    // last-message age (hl_ws::WsFreshness), so it can never report a socket that died.

    // Fan task: mids -> features/triggers/throttled broadcast
    let engine_fan = engine.clone();
    let store_fan = store.clone();
    let bcast_fan = bcast_tx.clone();
    let snapshot_fan = snapshot.clone();
    let notifier_fan = notifier.clone();
    let gate_fan = gate_state.clone();
    let book_cache_fan = book_cache.clone();
    let book_cmd_fan = book_cmd_tx.clone();
    tokio::spawn(async move {
        // throttling for mids broadcast: send at most 1/s
        let mut pending_mids: Option<HashMap<String, f64>> = None;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Some(mids) = mids_rx.recv() => {
                    let now = chrono::Utc::now().timestamp_millis();
                    // LOCK ORDER INVARIANT: every engine touch for this batch happens here,
                    // under the engine guard alone, and the resulting features are carried
                    // out as plain data. The snapshot guard below therefore never nests an
                    // engine acquisition (the ABBA half that wedged the API on 2026-08-09).
                    let feats: HashMap<String, Option<crate::contracts::Features>> = {
                        let mut eng = engine_fan.lock().await;
                        for (market, mid) in &mids {
                            eng.on_mid(market, now, *mid);
                        }
                        mids.keys().map(|m| (m.clone(), eng.features(m))).collect()
                    };
                    // trigger scan
                    let open_positions = store_fan.open_positions().await.unwrap_or_default();
                    for pos in open_positions {
                        if let Some(mid) = mids.get(&pos.market).copied()
                            && let Some(action) = triggers::check_triggers(&pos, mid, now) {
                                let reason = match action {
                                    triggers::TriggerAction::Sl => "sl",
                                    triggers::TriggerAction::Tp => "tp",
                                    triggers::TriggerAction::TimeStop => "time_stop",
                                };
                                let book = book_cache_fan.read().await.get(&pos.market).cloned();
                                match store_fan.close_position(pos.id, mid, reason, book.as_ref()).await {
                                    Ok(trade) => {
                                        let _ = bcast_fan.send(WsMsg::Position(pos.clone()));
                                        notifier_fan
                                            .send(&tpl::triggered(reason, &pos.market, trade.px, trade.realized_pnl - trade.fee))
                                            .await;
                                        // per-market 30min cooldown starts at close (spec §5)
                                        record_close_cooldown(&gate_fan, &pos.market, chrono::Utc::now().timestamp_millis()).await;
                                        // stop tracking this market's book
                                        let _ = book_cmd_fan.try_send(crate::hl_ws::BookCmd::Unsub(pos.market.clone()));
                                    }
                                    Err(e) => warn!(error=%e, "trigger close failed"),
                                }
                        }
                    }
                    // update snapshot mids — assignment only, zero awaits inside the guard
                    {
                        let mut snap = snapshot_fan.write().await;
                        for m in snap.markets.iter_mut() {
                            if let Some(mid) = mids.get(&m.market) {
                                m.mid = *mid;
                                m.mark = *mid;
                                if let Some(Some(f)) = feats.get(&m.market) {
                                    m.features = Some(f.clone());
                                }
                            }
                        }
                    }
                    // queue mids for throttled broadcast
                    pending_mids = Some(mids);
                }
                _ = interval.tick() => {
                    if let Some(mids) = pending_mids.take() {
                        let _ = bcast_fan.send(WsMsg::Mids(mids));
                    }
                }
            }
        }
    });

    // 30s ctx poll — map-merge that preserves position rows (union) and updates ctx fields.
    let hl_for_ctx = hl_rest_clone;
    let snap_for_ctx = snapshot.clone();
    let eng_for_ctx = engine.clone();
    let mt_for_ctx = markets_tracked.clone();
    let cfg_univ3 = cfg.universe.clone();
    let store_ctx = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            match fetch_universe(&hl_for_ctx, &cfg_univ3).await {
                Ok(rows) => {
                    // LOCK ORDER INVARIANT: short snapshot read (clone out) → store I/O and
                    // engine work with no snapshot guard → assignment-only snapshot write.
                    let ts = chrono::Utc::now().timestamp_millis();
                    let ctx_mids: HashMap<String, f64> =
                        rows.iter().map(|r| (r.market.clone(), r.mid)).collect();
                    let existing: Vec<crate::contracts::MarketRow> = {
                        let snap_r = snap_for_ctx.read().await;
                        snap_r.markets.clone()
                    };
                    // snapshot mids before merge for seeding priority
                    let snapshot_mids: HashMap<String, f64> =
                        existing.iter().map(|m| (m.market.clone(), m.mid)).collect();
                    // UNION source: inject any open-position markets not in filtered set.
                    let open_markets: Vec<String> = store_ctx
                        .open_positions()
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|p| p.market)
                        .collect();
                    let (mut markets, filtered_len) = {
                        let mut eng = eng_for_ctx.lock().await;
                        for r in &rows {
                            let z = eng.funding_z(&r.market, r.funding, ts);
                            eng.on_ctx(r, z);
                        }
                        // merge: keep existing markets but update ctx fields, preserve mids
                        let mut map: HashMap<String, crate::contracts::MarketRow> = existing
                            .into_iter()
                            .map(|m| (m.market.clone(), m))
                            .collect();
                        let filtered_len = rows.len();
                        for r in rows {
                            let feats = eng.features(&r.market);
                            let entry = map.entry(r.market.clone()).or_insert_with(|| {
                                crate::contracts::MarketRow {
                                    market: r.market.clone(),
                                    mid: r.mid,
                                    mark: r.mark,
                                    oracle: r.oracle,
                                    funding: r.funding,
                                    open_interest: r.open_interest,
                                    day_ntl_vlm: r.day_ntl_vlm,
                                    prev_day_px: r.prev_day_px,
                                    features: None,
                                }
                            });
                            entry.mark = r.mark;
                            entry.oracle = r.oracle;
                            entry.funding = r.funding;
                            entry.open_interest = r.open_interest;
                            entry.day_ntl_vlm = r.day_ntl_vlm;
                            entry.prev_day_px = r.prev_day_px;
                            entry.features = feats;
                            if entry.mid == 0.0 {
                                entry.mid = r.mid;
                            }
                        }
                        let engine_mids: HashMap<String, f64> = open_markets
                            .iter()
                            .filter_map(|om| eng.latest_mid(om).map(|mid| (om.clone(), mid)))
                            .collect();
                        let feats_fn = |mk: &str| eng.features(mk);
                        crate::hl_rest::ensure_position_markets_map(
                            &mut map,
                            &open_markets,
                            &snapshot_mids,
                            &engine_mids,
                            &ctx_mids,
                            feats_fn,
                        );
                        (map.into_values().collect::<Vec<_>>(), filtered_len)
                    };
                    // markets_tracked counts only filtered universe (writer documented in hl_rest::ensure_position_markets).
                    mt_for_ctx.store(filtered_len, Ordering::SeqCst);
                    let mut snap = snap_for_ctx.write().await;
                    crate::hl_rest::overlay_live_mids(&mut markets, &snap.markets);
                    snap.ts = ts;
                    snap.markets = markets;
                }
                Err(e) => warn!(error=%e, "ctx poll failed"),
            }
        }
    });

    // Candle refresh tasks — keep the analyst TECHNICALS block fed.
    // 3min: re-fetch only the CURRENT nominees' 15m bars (cheap; intraday must stay fresh).
    {
        let hl_c = hl_rest.clone();
        let cache_c = candle_cache.clone();
        let nom_c = nominees.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(180));
            loop {
                interval.tick().await;
                let markets: Vec<String> = {
                    let n = nom_c.read().await;
                    n.iter().map(|nom| nom.market.clone()).collect()
                };
                for market in markets {
                    refresh_market_candles(&hl_c, &cache_c, &market, true).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        });
    }
    // 15min: full universe, both intervals — also heals markets the boot seed missed.
    {
        let hl_c = hl_rest.clone();
        let cache_c = candle_cache.clone();
        let snap_c = snapshot.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(900));
            loop {
                interval.tick().await;
                let markets: Vec<String> = {
                    let s = snap_c.read().await;
                    s.markets.iter().map(|m| m.market.clone()).collect()
                };
                let total = markets.len();
                let mut refreshed = 0usize;
                let mut failed = 0usize;
                for (idx, market) in markets.iter().enumerate() {
                    if refresh_market_candles(&hl_c, &cache_c, market, false).await {
                        refreshed += 1;
                    } else {
                        failed += 1;
                    }
                    let n = idx + 1;
                    if n % 25 == 0 {
                        info!(
                            progress = format!("{n}/{total}"),
                            refreshed, failed, "candle refresh progress"
                        );
                    }
                    if n < total {
                        if n % 20 == 0 {
                            tokio::time::sleep(Duration::from_millis(1000)).await;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
                debug!(total, refreshed, failed, "candle refresh done");
            }
        });
    }

    // analyst or stub
    let analysts_rev = analysts.clone();
    let sizing_cfg = cfg.sizing.clone();
    let semaphore = Arc::new(Semaphore::new(2));

    // 45s screener
    let store_scr = store.clone();
    let snap_scr = snapshot.clone();
    let nom_scr = nominees.clone();
    let bcast_scr = bcast_tx.clone();
    let notifier_scr = notifier.clone();
    let news_scr = news.clone();
    let sem_scr = semaphore.clone();
    let screener_cfg = cfg.screener.clone();
    let universe_cfg = cfg.universe.clone();
    let screen_interval_s = cfg.screener.interval_s;
    let book_cache_scr = book_cache.clone();
    let book_cmd_scr = book_cmd_tx.clone();
    let hl_scr = hl_rest.clone();
    let candle_cache_scr = candle_cache.clone();
    let alerter_scr = alerter.clone();
    let daily_cap_once_scr = daily_cap_once.clone();
    let analyst_alert_scr = analyst_alert.clone();
    let analysts_scr = analysts.clone();
    let analysts_enabled_scr = cfg.analyst.enabled;
    let skip_rechecks_scr = Arc::new(Mutex::new(SkipRecheckMap::new()));
    let kill_pct_scr = cfg.risk.kill_switch_pct;
    let daily_cap_scr = cfg.risk.daily_cap;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(screen_interval_s));
        let mut force_nominees = HashMap::new();
        loop {
            interval.tick().await;
            // snapshot read
            let rows = {
                let s = snap_scr.read().await;
                s.markets.clone()
            };
            // Gate-doomed markets never reach the analyst: already open, per-market daily cap
            // spent, or inside a cooldown window (base or post-SL). Same ledger helpers the
            // entry gate refuses with, computed once per tick — see EntryGate::excluded_markets.
            let now = chrono::Utc::now().timestamp_millis();
            while let Ok(markets) = force_nominee_rx.try_recv() {
                for market in markets {
                    force_nominees.insert(market, now);
                }
            }
            let forced_markets = take_force_nominees(&mut force_nominees, now);
            // Arena gates are model-scoped, so a pre-filter built from the combined book would
            // incorrectly stop model B from evaluating a market model A already owns.
            let excluded = HashSet::new();
            // audit fix: use actual config, not shadowed literals (toml is the source of truth)
            let nom = screener::screen(&rows, &screener_cfg, &universe_cfg, &excluded);
            {
                let mut w = nom_scr.write().await;
                *w = nom.clone();
            }
            let forced = forced_candidates(&rows, &forced_markets, now);
            {
                let mut rechecks = skip_rechecks_scr.lock().await;
                prune_skip_rechecks(&mut rechecks, now);
            }
            if nom.is_empty() && forced.is_empty() {
                continue;
            }
            if !analysts_enabled_scr {
                continue;
            }
            let mut candidates: Vec<(Nominee, bool)> = nom
                .into_iter()
                .take(2)
                .map(|nominee| (nominee, false))
                .collect();
            for nominee in forced {
                if let Some((_, forced_by_trenchers_den)) = candidates.iter_mut().find(|(existing, _)| existing.market == nominee.market) {
                    *forced_by_trenchers_den = true;
                } else {
                    candidates.push((nominee, true));
                }
            }
            // Recheck suppression is deliberately applied only after a model has decided;
            // applying the old market-only map here would let one model's skip suppress every
            // other arena seat. Each seat remains independently eligible this tick.
            // serialized analyst max 2 concurrent
            for (nominee, forced_by_trenchers_den) in candidates {
                // Every enabled model receives the same already-assembled candidate/context;
                // its decision, position and later review are tagged with that model id.
                let arena: Vec<Arc<Analyst>> = if is_stub() { Vec::new() } else { analysts_scr.clone() };
                for analyst_c in arena {
                let Ok(permit) = sem_scr.clone().acquire_owned().await else { break; };
                let nominee = nominee.clone();
                let store_c = store_scr.clone();
                let bcast_c = bcast_scr.clone();
                let notifier_c = notifier_scr.clone();
                let alerter_c = alerter_scr.clone();
                let daily_cap_once_c = daily_cap_once_scr.clone();
                let news_c = news_scr.clone();
                let snap_c = snap_scr.clone();
                let entry_gate_c = entry_gate.clone();
                let sizing_c = sizing_cfg.clone();
                let book_cache_c = book_cache_scr.clone();
                let book_cmd_c = book_cmd_scr.clone();
                let hl_atr_c = hl_scr.clone();
                let candle_cache_c = candle_cache_scr.clone();
                let analyst_alert_c = analyst_alert_scr.clone();
                let skip_rechecks_c = skip_rechecks_scr.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    // subscribe this nominee's book early so the open fill can use depth
                    let _ = book_cmd_c.try_send(crate::hl_ws::BookCmd::Sub(nominee.market.clone()));
                    // gather news: tavily (also ingests into store), then matched last-6h from store (spec §5, <=10)
                    let tavily_items = news_c
                        .tavily_recent(&nominee.market)
                        .await
                        .unwrap_or_default();
                    let six_h_ago = chrono::Utc::now().timestamp_millis() - 6 * 60 * 60 * 1000;
                    let mut matched_news: Vec<crate::contracts::NewsItem> = news_c
                        .matched_recent(&nominee.market, six_h_ago, 10)
                        .await
                        .unwrap_or_default();
                    if matched_news.is_empty() {
                        matched_news = tavily_items;
                    }
                    // market hours flag
                    let mh = is_xyz_open(&nominee.market, chrono::Utc::now().timestamp_millis());
                    // 15m ATR(14) over 6h window (24 candles) — failure => None, never blocks decide
                    let atr_pct = {
                        let end_ms = chrono::Utc::now().timestamp_millis();
                        let start_ms = end_ms - 6 * 60 * 60 * 1000;
                        match hl_atr_c
                            .candle_snapshot(&nominee.market, "15m", start_ms, end_ms)
                            .await
                        {
                            Ok(candles) => crate::features::atr_pct(&candles, 14),
                            Err(e) => {
                                warn!(market=%nominee.market, error=?e, "candle_snapshot failed, atr_pct=None");
                                None
                            }
                        }
                    };
                    // analyst decide — log with executed=0 initially; mark 1 only after open succeeds.
                    // Executed wiring uses Store::log_decision/mark_decision_executed via last_insert_rowid
                    // (connection-local, not a racing SELECT; documented per spec — least-racy at max-2-concurrency).
                    let (decision_opt, decision_id, decision_analyst) = if is_stub() {
                        (
                            Some(crate::contracts::Decision {
                                action: "open".into(),
                                side: Some(crate::contracts::Side::Long),
                                conviction: 0.9,
                                thesis: "stub momentum".into(),
                                horizon_hours: Some(24.0),
                                stop_pct: Some(1.2),
                                tp_pct: Some(2.4),
                                invalidation_condition: None,
                                risk_usd: None,
                                leverage: None,
                            }),
                            0, "stub".to_string(),
                        )
                    } else {
                        let an = analyst_c;
                        // Entry context (Wave 2). LOCK DISCIPLINE: every ledger read happens with
                        // NO guard held — the one snapshot read below copies its map out and drops
                        // the guard before the next await (R1 invariant).
                        let open_now = store_c.open_positions().await.unwrap_or_default();
                        let (marks, nominee_oi, nominee_funding): (
                            HashMap<String, f64>,
                            Option<f64>,
                            Option<f64>,
                        ) = {
                            let s = snap_c.read().await;
                            let row = s.markets.iter().find(|m| m.market == nominee.market);
                            (
                                s.markets
                                    .iter()
                                    .map(|m| (m.market.clone(), m.mid))
                                    .collect(),
                                row.map(|r| r.open_interest),
                                row.map(|r| r.funding),
                            )
                        };
                        // TECHNICALS input: cached nominee candles, staleness-gated (>30min
                        // old or absent => None => prompt prints `series: unavailable` and
                        // the decide proceeds — the same never-block rule as atr_pct above).
                        let candles = {
                            let now = chrono::Utc::now().timestamp_millis();
                            candle_cache_c
                                .read()
                                .await
                                .get(&nominee.market)
                                .cloned()
                                .filter(|mc| now - mc.fetched_ms <= CANDLE_STALE_MS)
                        };
                        let equity = store_c.equity(&marks).await.unwrap_or(sizing_c.bankroll);
                        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
                        let day_open = store_c
                            .day_open_equity(&today)
                            .await
                            .unwrap_or(None)
                            .unwrap_or(equity);
                        let ctx_now = chrono::Utc::now().timestamp_millis();
                        let recent = store_c
                            .recent_closes(&nominee.market, 3)
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|c| crate::analyst::RecentOutcome {
                                action: c.action,
                                net_pnl: c.net_pnl,
                                hours_ago: (ctx_now - c.ts).max(0) as f64 / 3_600_000.0,
                            })
                            .collect();
                        // Per-model adaptive feedback: last 10 closes for THIS analyst + open book filtered later.
                        let analyst_recent = store_c
                            .analyst_recent_closes(an.model_id(), 10)
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|c| crate::analyst::RecentOutcome {
                                action: c.action,
                                net_pnl: c.net_pnl,
                                hours_ago: (ctx_now - c.ts).max(0) as f64 / 3_600_000.0,
                            })
                            .collect();
                        let input = AnalystInput {
                            nominee: nominee.clone(),
                            news: matched_news,
                            open_positions: open_now,
                            market_hours: mh,
                            atr_pct,
                            recent,
                            account: account_state(equity, day_open, kill_pct_scr),
                            open_marks: marks,
                            candles,
                            open_interest: nominee_oi,
                            funding: nominee_funding,
                            analyst_id: an.model_id().to_string(),
                            analyst_recent,
                        };
                        let outcome = if an.is_chat() && an.has_tools() {
                            an.decide_with_tools(input).await
                        } else {
                            an.decide(input).await
                        };
                        record_analyst_calls(&store_c, &nominee.market, "decide", &outcome.model_used, &outcome.calls)
                            .await;
                        let alert = analyst_alert_c
                            .lock()
                            .await
                            .observe(an.failure_streak(), chrono::Utc::now().timestamp_millis());
                        if alert.down {
                            alerter_c.send(AlertKind::AnalystDown, &format!("🔴 <b>analyst down</b>\nstreak <code>{}</code> failed calls\nentries are blind — check ANALYST_API_KEY/endpoint", an.failure_streak())).await;
                        }
                        if alert.up {
                            alerter_c
                                .send(AlertKind::AnalystUp, "🟢 <b>analyst recovered</b>")
                                .await;
                        }
                        // log decision with executed=false; capture last_insert_rowid for later mark
                        let vetoed = outcome
                            .decision
                            .as_ref()
                            .map(|d| d.action == "veto_close")
                            .unwrap_or(false);
                        // reason format extension: `... refundable:<kind>:<excerpt>` only on refusal paths (never on success)
                        // kind ∈ {empty_output, no_json, incomplete_max_tokens, http_err, timeout}; excerpt is first 120 chars sanitized single-line for empty-ish kinds, error class only for http_err/timeout
                        // plus optional ` searched:"q1","q2"` (max 3 sanitized queries) for web retrieval forensic view
                        let mut reason = format!(
                            "model {} refused:{} latency:{}",
                            outcome.model_used, outcome.refused, outcome.latency_ms
                        );
                        if forced_by_trenchers_den {
                            reason.push_str(" forced:trenchers_den");
                        }
                        if let Some(ref kind) = outcome.refusal_kind {
                            let excerpt = outcome.refusal_excerpt.as_deref().unwrap_or("");
                            reason.push_str(&format!(" refundable:{kind}:{excerpt}"));
                        }
                        if !outcome.searched_queries.is_empty() {
                            let qs: Vec<String> = outcome
                                .searched_queries
                                .iter()
                                .take(3)
                                .map(|q| format!("\"{q}\""))
                                .collect();
                            reason.push_str(&format!(" searched:{}", qs.join(",")));
                        }
                        let ts = chrono::Utc::now().timestamp_millis();
                        let action = outcome
                            .decision
                            .as_ref()
                            .map(|d| d.action.as_str())
                            .unwrap_or("skip");
                        let side = outcome
                            .decision
                            .as_ref()
                            .and_then(|d| d.side.as_ref())
                            .map(|s| match s {
                                crate::contracts::Side::Long => "long",
                                crate::contracts::Side::Short => "short",
                            })
                            .unwrap_or("skip");
                        let conviction = outcome
                            .decision
                            .as_ref()
                            .map(|d| d.conviction)
                            .unwrap_or(0.0);
                        let thesis = outcome
                            .decision
                            .as_ref()
                            .map(|d| d.thesis.clone())
                            .unwrap_or_default();
                        let horizon = outcome
                            .decision
                            .as_ref()
                            .and_then(|d| d.horizon_hours)
                            .unwrap_or(24.0);
                        let id = store_c
                            .log_decision_for_analyst(
                                ts,
                                &nominee.market,
                                action,
                                side,
                                conviction,
                                &thesis,
                                horizon,
                                vetoed,
                                false,
                                &reason,
                                &outcome.model_used,
                                outcome
                                    .decision
                                    .as_ref()
                                    .and_then(|d| d.invalidation_condition.as_deref()),
                            )
                            .await
                            .unwrap_or(0);
                        (outcome.decision, id, outcome.model_used)
                    };
                    if decision_opt.is_none() {
                        record_skip_recheck(&mut *skip_rechecks_c.lock().await, &nominee.market, nominee.score, chrono::Utc::now().timestamp_millis(), true);
                        let _ = book_cmd_c
                            .try_send(crate::hl_ws::BookCmd::Unsub(nominee.market.clone()));
                    }
                    if let Some(dec) = decision_opt {
                        if dec.action != "open" {
                            record_skip_recheck(&mut *skip_rechecks_c.lock().await, &nominee.market, nominee.score, chrono::Utc::now().timestamp_millis(), dec.action != "veto_close");
                            let _ = book_cmd_c
                                .try_send(crate::hl_ws::BookCmd::Unsub(nominee.market.clone()));
                            return;
                        }
                        let side = match dec.side {
                            Some(crate::contracts::Side::Long) => crate::contracts::Side::Long,
                            Some(crate::contracts::Side::Short) => crate::contracts::Side::Short,
                            None => nominee.side_hint,
                        };
                        let conviction = dec.conviction;
                        // risk gate — the full chain (staleness pre-check -> frozen gate_entry ->
                        // regime -> churn) lives in EntryGate::check; see risk::GateRefusal for the
                        // placement rationale. Entry path only: the review loop is never gated, so a
                        // position can always be closed.
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if let Err(refusal) = entry_gate_c
                            .check(&decision_analyst, &nominee.market, conviction, now_ms)
                            .await
                        {
                            record_skip_recheck(&mut *skip_rechecks_c.lock().await, &nominee.market, nominee.score, chrono::Utc::now().timestamp_millis(), true);
                            record_gate_refusal(
                                &store_c,
                                &alerter_c,
                                &daily_cap_once_c,
                                decision_id,
                                &nominee.market,
                                &refusal,
                                daily_cap_scr,
                            )
                            .await;
                            let _ = book_cmd_c
                                .try_send(crate::hl_ws::BookCmd::Unsub(nominee.market.clone()));
                            return;
                        }
                        // 6. RR floor (Batch B post-check): an open that states BOTH brackets
                        // must promise at least min_rr reward:risk. Stop-less decisions keep
                        // the sizer's default bracket and pass — see Risk::gate_min_rr.
                        if let Err(refusal) =
                            entry_gate_c.risk.gate_min_rr(&dec.action, dec.stop_pct, dec.tp_pct)
                        {
                            record_skip_recheck(&mut *skip_rechecks_c.lock().await, &nominee.market, nominee.score, chrono::Utc::now().timestamp_millis(), true);
                            record_gate_refusal(
                                &store_c,
                                &alerter_c,
                                &daily_cap_once_c,
                                decision_id,
                                &nominee.market,
                                &refusal,
                                daily_cap_scr,
                            )
                            .await;
                            let _ = book_cmd_c
                                .try_send(crate::hl_ws::BookCmd::Unsub(nominee.market.clone()));
                            return;
                        }
                        // sizing
                        let vol = nominee.features.vol1h;
                        let mut sized = size_position(&sizing_c, vol, conviction);
                        // User-locked amendment 2026-08-09: entry stop floor 1.0% (noise bleed) — analyst may not undercut;
                        // tp stays [0.4, 4.0] (tight tp is fine). Review MoveStop clamp stays [0.4, 4.0] (profit-locking).
                        if let Some(sp) = dec.stop_pct {
                            sized.stop_pct = sp.clamp(1.0, 4.0);
                        }
                        if let Some(tp) = dec.tp_pct {
                            sized.tp_pct = tp.clamp(0.4, 4.0);
                        }
                        // Per-trade leverage 1-10x (majors 10x, xyz 5x, otherwise 10x). None → keep sizing's vol/conviction leverage (capped per-market for safety); Some → clamped per-market. Spec's 3.0 default is available via `resolve_leverage` for backtests/tests, but live path preserves sizing's leverage when the model omits it.
                        if let Some(lev) = dec.leverage {
                            sized.leverage = crate::sizing::clamp_leverage(&nominee.market, lev);
                        } else {
                            sized.leverage = crate::sizing::clamp_leverage(&nominee.market, sized.leverage);
                        }
                        sized.notional = sized.margin * sized.leverage;
                        let is_xyz = nominee.market.starts_with("xyz:");
                        // get mark from snapshot
                        let mark = {
                            let s = snap_c.read().await;
                            s.markets
                                .iter()
                                .find(|m| m.market == nominee.market)
                                .map(|m| m.mid)
                                .unwrap_or(100.0)
                        };
                        let book = book_cache_c.read().await.get(&nominee.market).cloned();
                        match store_c
                            .open_position_for_analyst(
                                &nominee.market,
                                side,
                                &sized,
                                mark,
                                is_xyz,
                                dec.horizon_hours.unwrap_or(24.0),
                                book.as_ref(),
                                &decision_analyst,
                            )
                            .await
                        {
                            Ok(pos) => {
                                if decision_id != 0 {
                                    let _ = store_c.mark_decision_executed(decision_id).await;
                                }
                                let _ = bcast_c.send(WsMsg::Position(pos.clone()));
                                let side_s = match pos.side {
                                    crate::contracts::Side::Long => "long",
                                    crate::contracts::Side::Short => "short",
                                };
                                notifier_c
                                    .send(&tpl::opened(
                                        &pos.market,
                                        side_s,
                                        pos.entry_px,
                                        pos.leverage,
                                        &dec.thesis,
                                    ))
                                    .await;
                                // book sub stays: position is open, closes will Unsub
                            }
                            Err(e) => {
                                record_skip_recheck(&mut *skip_rechecks_c.lock().await, &nominee.market, nominee.score, chrono::Utc::now().timestamp_millis(), true);
                                warn!(error=%e, "open position failed");
                                let _ = book_cmd_c
                                    .try_send(crate::hl_ws::BookCmd::Unsub(nominee.market.clone()));
                            }
                        }
                    }
                });
                }
            }
        }
    });

    // Position review loop (spec §5): every review_interval_min, analyst re-reviews each open
    // position with fresh features + matched last-6h news; may hold / tighten stop / veto_close.
    // NOTE: must clone analyst before the screener task moves it.
    if cfg.analyst.enabled {
    let store_rev = store.clone();
    let snap_rev = snapshot.clone();
    let eng_rev = engine.clone();
    let news_rev = news.clone();
    let bcast_rev = bcast_tx.clone();
    let notifier_rev = notifier.clone();
    let gate_rev = gate_state.clone();
    let book_cache_rev = book_cache.clone();
    let book_cmd_rev = book_cmd_tx.clone();
    let alerter_rev = alerter.clone();
    let analyst_alert_rev = analyst_alert.clone();
    let review_interval_s = cfg.risk.review_interval_min * 60;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(review_interval_s));
        loop {
            interval.tick().await;
            let positions = store_rev.open_positions().await.unwrap_or_default();
            for pos in positions {
                let Some(an) = analysts_rev.iter().find(|an| an.model_id() == pos.analyst) else { continue };
                let feats = {
                    let eng = eng_rev.lock().await;
                    eng.features(&pos.market)
                };
                let Some(feats) = feats else { continue };
                // MONEY GUARD: missing or invalid mid (<=0) is "unknown" — propagate 0.0 so
                // apply_review_action's veto_close guard can WARN+skip instead of filling at entry_px.
                // LOCK DISCIPLINE: one short read guard, a single `f64` copied out, no `.await` inside.
                // The same mark feeds the review prompt (mark / uPnL / R multiple) and the close that
                // may follow, so the model and the fill can never disagree about the price.
                let mark = {
                    let s = snap_rev.read().await;
                    s.markets
                        .iter()
                        .find(|m| m.market == pos.market)
                        .map(|m| m.mid)
                        .unwrap_or(0.0)
                };
                let six_h_ago = chrono::Utc::now().timestamp_millis() - 6 * 60 * 60 * 1000;
                let matched = news_rev
                    .matched_recent(&pos.market, six_h_ago, 10)
                    .await
                    .unwrap_or_default();
                let outcome = an.review(&pos, &feats, &matched, mark).await;
                record_analyst_calls(&store_rev, &pos.market, "review", &outcome.model_used, &outcome.calls).await;
                let alert = analyst_alert_rev
                    .lock()
                    .await
                    .observe(an.failure_streak(), chrono::Utc::now().timestamp_millis());
                if alert.down {
                    alerter_rev.send(AlertKind::AnalystDown, &format!("🔴 <b>analyst down</b>\nstreak <code>{}</code> failed calls\nentries are blind — check ANALYST_API_KEY/endpoint", an.failure_streak())).await;
                }
                if alert.up {
                    alerter_rev
                        .send(AlertKind::AnalystUp, "🟢 <b>analyst recovered</b>")
                        .await;
                }
                let action = classify_review(&outcome.decision);
                let book = book_cache_rev.read().await.get(&pos.market).cloned();
                let applied =
                    apply_review_action(&store_rev, &gate_rev, &pos, mark, action, book.as_ref())
                        .await;
                if action == ReviewAction::VetoClose && applied {
                    let _ = book_cmd_rev.try_send(crate::hl_ws::BookCmd::Unsub(pos.market.clone()));
                }
                let action_str = match action {
                    ReviewAction::Hold => outcome
                        .decision
                        .as_ref()
                        .map(|d| d.action.clone())
                        .unwrap_or_else(|| "hold".into()),
                    ReviewAction::VetoClose => "veto_close".to_string(),
                    ReviewAction::MoveStop(sp) => format!("move_stop:{sp:.2}"),
                };
                // reason format extension: `... refundable:<kind>:<excerpt>` only on refusal paths
                // plus optional ` searched:"q1","q2"` for web retrieval forensic
                let mut reason = format!(
                    "review; model {} refused:{} latency:{}",
                    outcome.model_used, outcome.refused, outcome.latency_ms
                );
                if let Some(ref kind) = outcome.refusal_kind {
                    let excerpt = outcome.refusal_excerpt.as_deref().unwrap_or("");
                    reason.push_str(&format!(" refundable:{kind}:{excerpt}"));
                }
                if !outcome.searched_queries.is_empty() {
                    let qs: Vec<String> = outcome
                        .searched_queries
                        .iter()
                        .take(3)
                        .map(|q| format!("\"{q}\""))
                        .collect();
                    reason.push_str(&format!(" searched:{}", qs.join(",")));
                }
                // Review decisions: executed = applied (veto_close closes or move-stop tightens). Hold never executes.
                let _ = store_rev
                    .log_decision(
                        chrono::Utc::now().timestamp_millis(),
                        &pos.market,
                        &action_str,
                        match pos.side {
                            crate::contracts::Side::Long => "long",
                            crate::contracts::Side::Short => "short",
                        },
                        outcome
                            .decision
                            .as_ref()
                            .map(|d| d.conviction)
                            .unwrap_or(0.0),
                        &outcome
                            .decision
                            .as_ref()
                            .map(|d| d.thesis.clone())
                            .unwrap_or_default(),
                        pos.horizon_hours.unwrap_or(24.0),
                        action == ReviewAction::VetoClose,
                        applied,
                        &reason,
                    )
                    .await;
                if action == ReviewAction::VetoClose && applied {
                    notifier_rev.send(&tpl::veto_close(&pos.market, mark)).await;
                    let _ = bcast_rev.send(WsMsg::Position(pos.clone()));
                }
            }
        }
    });
    }

    // 60s equity snapshot + kill eval
    let store_eq = store.clone();
    let snap_eq = snapshot.clone();
    let bcast_eq = bcast_tx.clone();
    let risk_eq = risk.clone();
    let gate_eq = gate_state.clone();
    let alerter_eq = alerter.clone();
    let notifier_eq = notifier.clone();
    let kill_pct_eq = cfg.risk.kill_switch_pct;
    let kill_enabled_eq = cfg.risk.kill_enabled;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            // compute marks map
            let marks: HashMap<String, f64> = {
                let s = snap_eq.read().await;
                s.markets
                    .iter()
                    .map(|m| (m.market.clone(), m.mid))
                    .collect()
            };
            let equity = store_eq.equity(&marks).await.unwrap_or(1000.0);
            let ts = chrono::Utc::now().timestamp_millis();
            let _ = store_eq.snapshot_equity(ts, equity).await;
            let _ = bcast_eq.send(WsMsg::Equity(crate::contracts::EquityPoint { ts, equity }));
            // kill eval against day-open equity
            let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
            if kill_enabled_eq && let Some(open) = store_eq.day_open_equity(&today).await.unwrap_or(None) {
                match risk_eq.on_equity(equity, open) {
                    crate::risk::KillState::Killed => {
                        // Latch under the guard, alert after it is released: a Telegram push is
                        // network I/O and must never be awaited while the gate mutex is held
                        // (the screener's build_gate_state waits on that same mutex).
                        let newly_killed = {
                            let mut gate = gate_eq.lock().await;
                            let first = !gate.kill_active;
                            gate.kill_active = true;
                            first
                        };
                        if newly_killed {
                            let floor = open * (1.0 - kill_pct_eq / 100.0);
                            alerter_eq
                                .send(
                                    AlertKind::KillSwitch,
                                    &tpl::kill_switch(equity, floor, open),
                                )
                                .await;
                        }
                    }
                    crate::risk::KillState::Normal => {
                        // latch stays until day roll below (documented: kill is a per-day latch)
                    }
                }
            }
            // day roll at 00:00 UTC: fresh day-open = current equity; kill latch resets with new day.
            // ONE clock read for both the day key and the digest's window — two reads could
            // straddle midnight and digest the wrong day.
            let roll_now = chrono::Utc::now();
            let now_ms = roll_now.timestamp_millis();
            let new_day = roll_now.format("%Y-%m-%d").to_string();
            {
                let mut gate = gate_eq.lock().await;
                if gate.day_key != new_day {
                    gate.day_key = new_day.clone();
                    gate.kill_active = false; // new day -> fresh day-open baseline, latch clears
                    drop(gate);
                    let _ = store_eq.set_day_open_equity(&new_day, equity).await; // INSERT OR IGNORE-ish
                    info!(day=%new_day, "day rolled: day-open equity set, kill latch reset");
                    // The day that just ENDED gets its digest — Telegram + docs/ledger. Spawned
                    // so a slow Telegram push can never stall the equity/kill loop, and
                    // idempotent against the ledger file so the boot back-fill cannot double it.
                    let store_d = store_eq.clone();
                    let notifier_d = notifier_eq.clone();
                    let ended = crate::risk::utc_day_start_ms(now_ms) - 86_400_000;
                    tokio::spawn(async move {
                        crate::digest::deliver(
                            &store_d,
                            &notifier_d,
                            std::path::Path::new(crate::digest::LEDGER_DIR_DEFAULT),
                            ended,
                        )
                        .await;
                    });
                }
            }
        }
    });

    // SIGINT
    tokio::signal::ctrl_c().await?;
    info!("SIGINT received, shutting down");
    api_handle.abort();
    Ok(())
}

async fn seed_name_maps(
    hl: &HlRest,
    cfg: &crate::config::UniverseCfg,
    maps: &std::sync::Arc<RwLock<HashMap<String, Vec<String>>>>,
) -> usize {
    let mut seeded = 0usize;
    for dex in &cfg.dexs {
        let opt = if dex.is_empty() {
            None
        } else {
            Some(dex.as_str())
        };
        match hl.universe_names(opt).await {
            Ok(names) => {
                maps.write().await.insert(dex.clone(), names);
                seeded += 1;
            }
            Err(e) => warn!(dex=%dex, error=?e, "seed_name_maps failed for dex"),
        }
    }
    seeded
}

/// Candle cache shared by the boot seed, the two refresh tasks, and the screener's prompt
/// build. Keyed by market (dex-prefixed, e.g. `xyz:TSLA`); values oldest→newest per interval.
type CandleCache = Arc<RwLock<HashMap<String, crate::hl_rest::MarketCandles>>>;

const CANDLE_M15_BARS: i64 = 64;
const CANDLE_H4_BARS: i64 = 60;
const CANDLE_M15_MS: i64 = 15 * 60 * 1000;
const CANDLE_H4_MS: i64 = 4 * 60 * 60 * 1000;
/// Older than this, cached candles are treated as missing: the decide proceeds without a
/// series rather than prompt the model off stale bars.
const CANDLE_STALE_MS: i64 = 30 * 60 * 1000;

/// Fetch and store one market's candles. `m15_only` (the 3min nominee refresh) merges into
/// the existing entry and keeps its 4h bars; a full refresh re-fetches both intervals, and a
/// failed 4h leg still keeps the fresh 15m (partial beats nothing at prompt time). Returns
/// false only when the 15m fetch itself failed — the entry is then left untouched.
async fn refresh_market_candles(hl: &HlRest, cache: &CandleCache, market: &str, m15_only: bool) -> bool {
    let end_ms = chrono::Utc::now().timestamp_millis();
    let m15_start = end_ms - CANDLE_M15_BARS * CANDLE_M15_MS;
    let m15 = match hl.candle_snapshot(market, "15m", m15_start, end_ms).await {
        Ok(c) => c,
        Err(e) => {
            warn!(market=%market, error=?e, "candle_snapshot 15m failed");
            return false;
        }
    };
    let h4 = if m15_only {
        None
    } else {
        let h4_start = end_ms - CANDLE_H4_BARS * CANDLE_H4_MS;
        match hl.candle_snapshot(market, "4h", h4_start, end_ms).await {
            Ok(c) => Some(c),
            Err(e) => {
                warn!(market=%market, error=?e, "candle_snapshot 4h failed; keeping previous 4h bars");
                None
            }
        }
    };
    let mut w = cache.write().await;
    let entry = w
        .entry(market.to_string())
        .or_insert_with(|| crate::hl_rest::MarketCandles {
            m15: Vec::new(),
            h4: Vec::new(),
            fetched_ms: 0,
        });
    entry.m15 = m15;
    if let Some(h4) = h4 {
        entry.h4 = h4;
    }
    entry.fetched_ms = end_ms;
    true
}

/// Boot seed for the candle cache: 15m x64 bars + 4h x60 bars per tracked market, following
/// the fundingHistory seed's pattern — throttled (100ms between markets, extra 1s every 20),
/// progress log every 25, warn+continue on failure, skipped entirely under ANALYST_STUB=1.
async fn boot_seed_candles(
    hl: &HlRest,
    snapshot: &std::sync::Arc<RwLock<crate::contracts::Snapshot>>,
    cache: &CandleCache,
) {
    if is_stub() {
        info!("ANALYST_STUB=1: skipping candle boot seed");
        return;
    }
    let markets: Vec<String> = {
        let snap = snapshot.read().await;
        snap.markets.iter().map(|m| m.market.clone()).collect()
    };
    if markets.is_empty() {
        warn!("candle boot seed: no tracked markets (universe bootstrap may have failed)");
        return;
    }
    let total = markets.len();
    info!(total, "candle boot seed starting (15m x64 + 4h x60, throttled)");
    let mut seeded = 0usize;
    let mut failed = 0usize;
    for (idx, market) in markets.iter().enumerate() {
        if refresh_market_candles(hl, cache, market, false).await {
            seeded += 1;
        } else {
            failed += 1;
        }
        let n = idx + 1;
        if n % 25 == 0 {
            info!(
                progress = format!("{n}/{total}"),
                seeded, failed, "candle seed progress"
            );
        }
        if n < total {
            if n % 20 == 0 {
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    info!(
        total,
        seeded,
        failed,
        "candle boot seed done (partial seed is ok — refresh tasks keep filling)"
    );
}

async fn boot_seed_funding(
    hl: &HlRest,
    engine: &std::sync::Arc<tokio::sync::Mutex<FeatureEngine>>,
    snapshot: &std::sync::Arc<RwLock<crate::contracts::Snapshot>>,
) {
    if !funding_seed_enabled() {
        info!("ANALYST_STUB=1: skipping fundingHistory boot seed");
        return;
    }
    let markets: Vec<String> = {
        let snap = snapshot.read().await;
        snap.markets.iter().map(|m| m.market.clone()).collect()
    };
    if markets.is_empty() {
        warn!("fundingHistory boot seed: no tracked markets (universe bootstrap may have failed)");
        return;
    }
    let end_ms = chrono::Utc::now().timestamp_millis();
    let start_ms = end_ms - 7 * 24 * 60 * 60 * 1000;
    let total = markets.len();
    info!(
        total,
        "fundingHistory boot seed starting (7d window, throttled)"
    );
    let mut seeded = 0usize;
    let mut failed = 0usize;
    for (idx, market) in markets.iter().enumerate() {
        match hl.funding_history(market, start_ms, end_ms).await {
            Ok(samples) => {
                if !samples.is_empty() {
                    let mut eng = engine.lock().await;
                    eng.seed_funding_history(market, &samples);
                    seeded += 1;
                } else {
                    debug!(market=%market, "fundingHistory empty (no samples)");
                }
            }
            Err(e) => {
                warn!(market=%market, error=?e, "fundingHistory fetch failed");
                failed += 1;
            }
        }
        let n = idx + 1;
        if n % 25 == 0 {
            info!(
                progress = format!("{n}/{total}"),
                seeded, failed, "fundingHistory seed progress"
            );
        }
        if n < total {
            if n % 20 == 0 {
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    info!(
        total,
        seeded,
        failed,
        "fundingHistory boot seed done (partial seed is ok — stream+poll still feed ring)"
    );
}

async fn fetch_universe(
    hl: &HlRest,
    cfg: &crate::config::UniverseCfg,
) -> anyhow::Result<Vec<crate::hl_rest::CtxRow>> {
    let mut all = Vec::new();
    for dex in &cfg.dexs {
        let opt = if dex.is_empty() {
            None
        } else {
            Some(dex.as_str())
        };
        let mut rows = hl
            .meta_and_ctxs(opt)
            .await
            .map_err(|e| anyhow::anyhow!("hl meta {e:?}"))?;
        // prefix xyz?
        if let Some(d) = opt {
            for r in &mut rows {
                if !r.market.contains(':') {
                    r.market = format!("{d}:{}", r.market);
                }
            }
        }
        // vlm filter
        for r in rows {
            let min = if r.market.starts_with("xyz:") {
                cfg.min_vlm_dex
            } else {
                cfg.min_vlm_native
            };
            if r.day_ntl_vlm >= min {
                all.push(r);
            }
        }
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Decision, Side};
    use crate::sizing::Sized;
    use chrono::TimeZone;

    async fn seeded_store() -> Store {
        Store::open("sqlite::memory:").await.expect("store")
    }

    fn test_sized() -> Sized {
        Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        }
    }

    fn test_screener_cfg() -> crate::config::ScreenerCfg {
        crate::config::ScreenerCfg { interval_s: 45, top_k: 6, min_score: 1.8, skip_recheck_min: 15, skip_recheck_score_jump: 0.5 }
    }

    fn test_nominee(market: &str, score: f64) -> Nominee {
        Nominee {
            ts: 0, market: market.to_string(), side_hint: Side::Long, score,
            features: crate::contracts::Features { r5m: 0.0, r1h: 0.0, r24h: 0.0, vol1h: 0.0, funding_z: 0.0, range_pos: 0.5 },
        }
    }

    fn test_risk_cfg() -> crate::config::RiskCfg {
        crate::config::RiskCfg {
            max_concurrent: 5,
            global_max_concurrent: 12,
            daily_cap: 20,
            cooldown_min: 30,
            kill_switch_pct: 12.0,
            kill_enabled: false,
            entries_enabled: true,
            conviction_min: 0.50,
            review_interval_min: 15,
            time_stop_hours: 24.0,
            max_feature_age_s: 120,
            per_market_daily_cap: 3,
            cooldown_after_sl_min: 120,
            morning_entry_budget: 12,
            regime_vol_max: 1.5,
            min_rr: 2.0,
        }
    }

    #[test]
    fn force_nominees_drain_expire_and_cap_oldest() {
        let now = 1_800_000_000_000;
        let mut forced = HashMap::from([
            ("BTC".to_string(), now - FORCE_NOMINEE_TTL_MS - 1),
            ("SOL".to_string(), now - 4),
            ("ETH".to_string(), now - 3),
            ("HYPE".to_string(), now - 2),
            ("PURR".to_string(), now - 1),
            ("X".to_string(), now),
        ]);
        let forced = take_force_nominees(&mut forced, now);
        assert_eq!(forced, vec!["ETH", "HYPE", "PURR", "X"]);
        assert!(!forced.contains(&"BTC".to_string()));
        assert!(!forced.contains(&"SOL".to_string()));
    }

    #[test]
    fn forced_candidate_needs_features_but_not_momentum_score() {
        let rows = vec![crate::contracts::MarketRow {
            market: "SOL".into(),
            mid: 100.0,
            mark: 100.0,
            oracle: 100.0,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: 100.0,
            features: Some(crate::contracts::Features {
                r5m: 0.0,
                r1h: -0.1,
                r24h: 0.0,
                vol1h: 0.1,
                funding_z: 0.0,
                range_pos: 0.5,
            }),
        }];
        let candidates = forced_candidates(&rows, &["SOL".into(), "MISSING".into()], 42);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].market, "SOL");
        assert_eq!(candidates[0].score, 0.0);
        assert_eq!(candidates[0].side_hint, Side::Short);
    }

    #[tokio::test]
    async fn manual_halt_refuses_screener_and_force_nominated_entries_first() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let halted = crate::config::RiskCfg { entries_enabled: false, ..test_risk_cfg() };
        let entry_gate = gate_at(
            &store,
            &gate,
            snapshot_at(now - 121_000, vec![market_row("BTC", Some(feats(2.0)))]),
            halted,
            now,
        );

        assert_eq!(
            entry_gate.check("alpha", "SOL", 0.9, now).await,
            Err(GateRefusal::EntriesHalted),
            "ordinary screener candidate must stop before stale/regime rails"
        );
        let forced = forced_candidates(
            &[market_row("SOL", Some(feats(0.4)))],
            &["SOL".to_string()],
            now,
        );
        assert_eq!(forced.len(), 1, "Trencher's Den call must nominate SOL");
        assert_eq!(
            entry_gate.check("bravo", &forced[0].market, 0.9, now).await,
            Err(GateRefusal::EntriesHalted),
            "force-nominated Trencher's Den candidate must use the same first gate"
        );
    }

    #[test]
    fn skip_recheck_filter_drops_recent_skips() {
        let now = 1_800_000_000_000;
        let mut skips = HashMap::from([("SOL".to_string(), (now - 60_000, 2.0))]);
        let candidates = filter_skip_rechecks(vec![(test_nominee("SOL", 2.4), false)], &mut skips, now, &test_screener_cfg());
        assert!(candidates.is_empty());
    }

    #[test]
    fn skip_recheck_filter_allows_material_score_jump() {
        let now = 1_800_000_000_000;
        let mut skips = HashMap::from([("SOL".to_string(), (now - 60_000, 2.0))]);
        let candidates = filter_skip_rechecks(vec![(test_nominee("SOL", 2.5), false)], &mut skips, now, &test_screener_cfg());
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn skip_recheck_filter_allows_forced_nomination() {
        let now = 1_800_000_000_000;
        let mut skips = HashMap::from([("SOL".to_string(), (now - 60_000, 2.0))]);
        let candidates = filter_skip_rechecks(vec![(test_nominee("SOL", 0.0), true)], &mut skips, now, &test_screener_cfg());
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn executed_open_does_not_record_skip_recheck() {
        let mut skips = HashMap::new();
        record_skip_recheck(&mut skips, "SOL", 2.0, 42, false);
        assert!(skips.is_empty());
    }

    #[test]
    fn skip_recheck_prunes_entries_older_than_a_day() {
        let now = 1_800_000_000_000;
        let mut skips = HashMap::from([("OLD".to_string(), (now - SKIP_RECHECK_RETENTION_MS - 1, 2.0)), ("SOL".to_string(), (now - SKIP_RECHECK_RETENTION_MS, 2.0))]);
        prune_skip_rechecks(&mut skips, now);
        assert_eq!(skips.len(), 1);
        assert!(skips.contains_key("SOL"));
    }

    fn risk_with_stale_window(max_feature_age_s: u64) -> Risk {
        Risk::new(crate::config::RiskCfg {
            max_feature_age_s,
            ..test_risk_cfg()
        })
    }

    /// Entry path: fresh snapshot passes, stale snapshot refuses, exact boundary passes.
    /// Exercised through the real read helper so the short-guard read is covered too.
    #[tokio::test]
    async fn entry_staleness_gate_fresh_stale_and_exact_boundary() {
        let risk = risk_with_stale_window(120);
        let now = 1_800_000_000_000i64;

        let fresh = Arc::new(RwLock::new(Snapshot {
            ts: now - 5_000,
            markets: vec![],
        }));
        assert_eq!(snapshot_age_ms(&fresh, now).await, 5_000);
        assert!(
            risk.gate_data_age(snapshot_age_ms(&fresh, now).await)
                .is_ok(),
            "5s-old state trades"
        );

        let exact = Arc::new(RwLock::new(Snapshot {
            ts: now - 120_000,
            markets: vec![],
        }));
        assert_eq!(snapshot_age_ms(&exact, now).await, 120_000);
        assert!(
            risk.gate_data_age(snapshot_age_ms(&exact, now).await)
                .is_ok(),
            "exactly 120s still trades"
        );

        let stale = Arc::new(RwLock::new(Snapshot {
            ts: now - 120_001,
            markets: vec![],
        }));
        assert_eq!(
            risk.gate_data_age(snapshot_age_ms(&stale, now).await),
            Err(GateRefusal::StaleData),
            "1ms past the window refuses"
        );

        // wedge shape: snapshot frozen 30 minutes ago (wedge #1 had the analyst deciding
        // on features stuck since 11:29Z) must refuse, whatever the other gates say.
        let wedged = Arc::new(RwLock::new(Snapshot {
            ts: now - 30 * 60 * 1000,
            markets: vec![],
        }));
        assert_eq!(
            risk.gate_data_age(snapshot_age_ms(&wedged, now).await),
            Err(GateRefusal::StaleData)
        );
    }

    #[tokio::test]
    async fn snapshot_age_is_zero_for_future_stamp() {
        let now = 1_800_000_000_000i64;
        let skewed = Arc::new(RwLock::new(Snapshot {
            ts: now + 10_000,
            markets: vec![],
        }));
        assert_eq!(
            snapshot_age_ms(&skewed, now).await,
            0,
            "clock skew must not fabricate staleness"
        );
    }

    /// Refusals must land in `decisions.reason` — that row is the only record that an
    /// analyst `open` was decided and then blocked.
    #[tokio::test]
    async fn gate_refusal_lands_on_the_decision_row() {
        let store = seeded_store().await;
        let alerter = Arc::new(crate::alerts::Alerter::new(Notify::new(
            "UNSET_TOKEN_ENV_XYZ".into(),
            String::new(),
        )));
        let once = Arc::new(crate::alerts::DayOnce::new());
        let id = store
            .log_decision(
                1,
                "SOL",
                "open",
                "long",
                0.9,
                "thesis",
                24.0,
                false,
                false,
                "model m refused:false latency:10",
            )
            .await
            .expect("log decision");

        record_gate_refusal(
            &store,
            &alerter,
            &once,
            id,
            "SOL",
            &GateRefusal::StaleData,
            8,
        )
        .await;

        let reason: String = sqlx::query_scalar("SELECT reason FROM decisions WHERE id=?1")
            .bind(id)
            .fetch_one(store.pool())
            .await
            .expect("reason");
        assert!(
            reason.ends_with(" gate_refused:StaleData"),
            "refusal appended, got {reason}"
        );
        assert!(
            reason.starts_with("model m refused:false"),
            "original reason preserved, got {reason}"
        );
        // must not read as an analyst refusal to the dashboard parser
        assert!(
            !reason.contains("refused:true"),
            "gate refusal must not masquerade as an analyst refusal"
        );

        // daily-cap refusal on a stub decision (id 0) must not touch the ledger or panic
        record_gate_refusal(&store, &alerter, &once, 0, "ETH", &GateRefusal::DailyCap, 8).await;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM decisions")
            .fetch_one(store.pool())
            .await
            .expect("count");
        assert_eq!(
            count, 1,
            "refusal recording never inserts rows, only annotates"
        );
    }

    #[tokio::test]
    async fn gate_state_reflects_kill_latch_and_real_daily_count() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        // simulate: kill active + two real opens today
        gate.lock().await.kill_active = true;
        for m in ["SOL", "ETH"] {
            store
                .open_position(m, Side::Long, &test_sized(), 100.0, false, 24.0, None)
                .await
                .expect("open");
        }
        let state = build_gate_state(&store, &gate, "").await;
        assert!(state.kill_active, "kill latch must reach gate");
        assert_eq!(
            state.daily_count, 2,
            "daily_count must count today's real entries"
        );
        assert!(state.open_markets.contains(&"SOL".to_string()));
        assert!(state.open_markets.contains(&"ETH".to_string()));
    }

    #[tokio::test]
    async fn cooldown_written_on_close_then_blocks_within_30m() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let pos = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        let now = chrono::Utc::now().timestamp_millis();
        store
            .close_position(pos.id, 101.0, "sl", None)
            .await
            .expect("close");
        record_close_cooldown(&gate, &pos.market, now).await;
        let state = build_gate_state(&store, &gate, "").await;
        let risk = Risk::new(test_risk_cfg());
        // within cooldown window -> refused
        let r = risk.gate_entry("SOL", 0.9, &state, now + 10 * 60 * 1000);
        assert!(
            matches!(r, Err(crate::risk::GateRefusal::Cooldown)),
            "cooldown must block re-entry, got {r:?}"
        );
        // after cooldown window -> passes cooldown gate
        let r2 = risk.gate_entry("SOL", 0.9, &state, now + 31 * 60 * 1000);
        assert!(r2.is_ok(), "after 30min cooldown entry allowed, got {r2:?}");
    }

    #[tokio::test]
    async fn daily_cap_of_20_blocks_21st_entry_via_real_count() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        for i in 0..20 {
            let m = format!("M{i}");
            store
                .open_position(&m, Side::Long, &test_sized(), 100.0, false, 24.0, None)
                .await
                .expect("open");
            // close immediately so max-concurrent(5) doesn't block first
            let p = store.open_positions().await.unwrap();
            store
                .close_position(p[0].id, 100.0, "close", None)
                .await
                .unwrap();
        }
        let state = build_gate_state(&store, &gate, "").await;
        assert_eq!(state.daily_count, 20);
        let risk = Risk::new(test_risk_cfg());
        let r = risk.gate_entry("NEW", 0.9, &state, chrono::Utc::now().timestamp_millis());
        assert!(
            matches!(r, Err(crate::risk::GateRefusal::DailyCap)),
            "daily cap 20 must block 21st, got {r:?}"
        );
    }

    #[tokio::test]
    async fn daily_cap_zero_allows_entries_past_the_former_boundary() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let mut cfg = test_risk_cfg();
        cfg.daily_cap = 0;
        cfg.global_max_concurrent = 30;
        cfg.morning_entry_budget = 100;
        let now = chrono::Utc::now().timestamp_millis();

        for i in 0..21 {
            let position = store
                .open_position_for_analyst(
                    &format!("M{i}"),
                    Side::Long,
                    &test_sized(),
                    100.0,
                    false,
                    24.0,
                    None,
                    "alpha",
                )
                .await
                .expect("open");
            store.close_position(position.id, 100.0, "tp", None).await.expect("close");
        }

        let entry_gate = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            cfg,
            now,
        );
        assert!(
            entry_gate.check("alpha", "HYPE", 0.9, now).await.is_ok(),
            "zero daily cap must leave both global and per-analyst daily gates unlimited"
        );
    }

    #[tokio::test]
    async fn global_max_concurrent_zero_allows_entries_past_the_former_boundary() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let mut cfg = test_risk_cfg();
        cfg.max_concurrent = 0;
        cfg.global_max_concurrent = 0;
        cfg.daily_cap = 0;
        cfg.morning_entry_budget = 100;
        let now = chrono::Utc::now().timestamp_millis();

        for i in 0..13 {
            store.open_position_for_analyst(&format!("M{i}"), Side::Long, &test_sized(), 100.0, false, 24.0, None, "alpha").await.expect("open");
        }

        let entry_gate = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            cfg,
            now,
        );
        assert!(
            entry_gate.check("alpha", "HYPE", 0.9, now).await.is_ok(),
            "zero position caps must permit the 14th open position"
        );
    }

    #[tokio::test]
    async fn global_daily_cap_blocks_an_analyst_with_room_in_its_own_budget() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let mut cfg = test_risk_cfg();
        cfg.daily_cap = 2;
        cfg.global_max_concurrent = 10;
        cfg.morning_entry_budget = 100;
        let now = chrono::Utc::now().timestamp_millis();

        for (market, analyst) in [("SOL", "alpha"), ("ETH", "bravo")] {
            let position = store
                .open_position_for_analyst(
                    market,
                    Side::Long,
                    &test_sized(),
                    100.0,
                    false,
                    24.0,
                    None,
                    analyst,
                )
                .await
                .expect("open");
            store
                .close_position(position.id, 100.0, "tp", None)
                .await
                .expect("close");
        }

        let entry_gate = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            cfg,
            now,
        );
        assert_eq!(
            entry_gate.check("charlie", "HYPE", 0.9, now).await,
            Err(GateRefusal::GlobalDailyCap),
            "charlie has no own entries, but the shared daily budget is spent"
        );
    }

    #[test]
    fn classify_review_maps_actions() {
        assert_eq!(classify_review(&None), ReviewAction::Hold);
        let hold = Some(Decision {
            action: "hold".into(),
            side: None,
            conviction: 0.5,
            thesis: "".into(),
            horizon_hours: None,
            stop_pct: None,
            tp_pct: None,
            invalidation_condition: None,
            risk_usd: None,
            leverage: None,
        });
        assert_eq!(classify_review(&hold), ReviewAction::Hold);
        let veto = Some(Decision {
            action: "veto_close".into(),
            side: None,
            conviction: 0.9,
            thesis: "".into(),
            horizon_hours: None,
            stop_pct: None,
            tp_pct: None,
            invalidation_condition: None,
            risk_usd: None,
            leverage: None,
        });
        assert_eq!(classify_review(&veto), ReviewAction::VetoClose);
        let mv = Some(Decision {
            action: "hold".into(),
            side: None,
            conviction: 0.5,
            thesis: "".into(),
            horizon_hours: None,
            stop_pct: Some(0.5),
            tp_pct: None,
            invalidation_condition: None,
            risk_usd: None,
            leverage: None,
        });
        assert_eq!(classify_review(&mv), ReviewAction::MoveStop(0.5));
        let mv_clamped = Some(Decision {
            action: "hold".into(),
            side: None,
            conviction: 0.5,
            thesis: "".into(),
            horizon_hours: None,
            stop_pct: Some(9.0),
            tp_pct: None,
            invalidation_condition: None,
            risk_usd: None,
            leverage: None,
        });
        assert_eq!(classify_review(&mv_clamped), ReviewAction::MoveStop(4.0));
    }

    #[tokio::test]
    async fn veto_close_closes_position_and_starts_cooldown() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let pos = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        let applied =
            apply_review_action(&store, &gate, &pos, 100.5, ReviewAction::VetoClose, None).await;
        assert!(applied, "veto_close must apply");
        assert!(
            store.open_positions().await.unwrap().is_empty(),
            "position closed"
        );
        assert!(
            gate.lock().await.last_close_ts.contains_key("SOL"),
            "cooldown recorded"
        );
    }

    #[tokio::test]
    async fn manual_halt_leaves_triggers_and_reviews_able_to_close_positions() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let pos = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open position predates halt");
        let halted = Risk::new(crate::config::RiskCfg { entries_enabled: false, ..test_risk_cfg() });

        assert_eq!(
            crate::triggers::check_triggers(&pos, pos.sl_px, pos.opened_ts + 1),
            Some(crate::triggers::TriggerAction::Sl),
            "the halt does not suppress an existing position's SL trigger"
        );
        assert_eq!(
            crate::triggers::check_triggers(&pos, pos.tp_px, pos.opened_ts + 1),
            Some(crate::triggers::TriggerAction::Tp),
            "the halt does not suppress an existing position's TP trigger"
        );
        assert_eq!(
            crate::triggers::check_triggers(&pos, pos.entry_px, pos.opened_ts + 24 * 3_600_000),
            Some(crate::triggers::TriggerAction::TimeStop),
            "the halt does not suppress an existing position's time stop"
        );
        assert_eq!(halted.gate_entries_enabled(), Err(GateRefusal::EntriesHalted));
        assert!(
            apply_review_action(&store, &gate, &pos, 100.5, ReviewAction::VetoClose, None).await,
            "the halt does not suppress an existing position's review close"
        );
        assert!(store.open_positions().await.expect("positions").is_empty());
    }

    #[tokio::test]
    async fn move_stop_only_tightens() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        // long entry 100.02 (2bp slip), stop 1% -> sl ~99.02. Wider stop (2%) must NOT apply.
        let pos = store
            .open_position(
                "SOL",
                Side::Long,
                &Sized {
                    leverage: 10.0,
                    margin: 20.0,
                    notional: 200.0,
                    stop_pct: 1.0,
                    tp_pct: 2.0,
                },
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        let widened = apply_review_action(
            &store,
            &gate,
            &pos,
            100.0,
            ReviewAction::MoveStop(2.0),
            None,
        )
        .await;
        assert!(!widened, "loosening stop must be rejected");
        let p = store.open_positions().await.unwrap();
        assert!((p[0].sl_px - pos.sl_px).abs() < 1e-9, "sl unchanged");
        let tightened = apply_review_action(
            &store,
            &gate,
            &pos,
            100.0,
            ReviewAction::MoveStop(0.6),
            None,
        )
        .await;
        assert!(tightened, "tighter stop applies");
        let p2 = store.open_positions().await.unwrap();
        let expect = pos.entry_px * (1.0 - 0.6 / 100.0);
        assert!(
            (p2[0].sl_px - expect).abs() < 1e-9,
            "sl moved to {expect}, got {}",
            p2[0].sl_px
        );
    }

    #[test]
    fn account_state_prices_the_kill_budget() {
        // flat on the day: nothing of the budget spent
        let flat = account_state(1000.0, 1000.0, 12.0);
        assert_eq!(
            (flat.equity, flat.day_pnl_pct, flat.kill_budget_used_pct),
            (1000.0, 0.0, 0.0)
        );
        // up on the day is still zero, never negative
        let up = account_state(1050.0, 1000.0, 12.0);
        assert!((up.day_pnl_pct - 5.0).abs() < 1e-9);
        assert_eq!(
            up.kill_budget_used_pct, 0.0,
            "being up does not bank kill budget"
        );
        // half the 12% budget spent
        let down = account_state(940.0, 1000.0, 12.0);
        assert!((down.day_pnl_pct - -6.0).abs() < 1e-9);
        assert!((down.kill_budget_used_pct - 50.0).abs() < 1e-9);
        // exactly at the floor is 100% — the point where entries stop
        let at_floor = account_state(880.0, 1000.0, 12.0);
        assert!((at_floor.kill_budget_used_pct - 100.0).abs() < 1e-9);
        // degenerate inputs cannot produce NaN in the prompt
        assert_eq!(account_state(1000.0, 0.0, 12.0).day_pnl_pct, 0.0);
        assert_eq!(account_state(900.0, 1000.0, 0.0).kill_budget_used_pct, 0.0);
    }

    #[tokio::test]
    async fn day_roll_resets_kill_latch() {
        let gate = Arc::new(Mutex::new(GateState::new()));
        {
            let mut g = gate.lock().await;
            g.kill_active = true;
            g.day_key = "1970-01-01".into(); // stale day forces roll on next tick logic
        }
        // replicate the equity task's roll block
        let new_day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut gate_g = gate.lock().await;
        if gate_g.day_key != new_day {
            gate_g.day_key = new_day.clone();
            gate_g.kill_active = false;
        }
        assert!(!gate_g.kill_active, "kill latch clears at day roll");
        assert_eq!(gate_g.day_key, new_day);
    }

    #[test]
    fn funding_seed_skipped_when_analyst_stub() {
        // Preserve prior value to avoid cross-test pollution (single-threaded test runner helps,
        // but we restore explicitly).
        let prev = std::env::var("ANALYST_STUB").ok();
        unsafe { std::env::set_var("ANALYST_STUB", "1") };
        assert!(
            !funding_seed_enabled(),
            "seed must be disabled when ANALYST_STUB=1"
        );
        match prev {
            Some(v) => unsafe { std::env::set_var("ANALYST_STUB", v) },
            None => unsafe { std::env::remove_var("ANALYST_STUB") },
        }
        assert!(
            funding_seed_enabled(),
            "seed must be enabled when ANALYST_STUB !=1"
        );
    }

    // ── Money guard: veto_close must WARN+skip on invalid mark (0.0 / missing) ──

    #[tokio::test]
    async fn veto_close_on_invalid_mark_does_not_close_position() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let pos = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        // Invalid mark 0.0 — must WARN and NOT close; position stays open, no cooldown.
        let applied =
            apply_review_action(&store, &gate, &pos, 0.0, ReviewAction::VetoClose, None).await;
        assert!(
            !applied,
            "veto_close with mark 0.0 must be skipped (money guard)"
        );
        let opens = store.open_positions().await.unwrap();
        assert_eq!(
            opens.len(),
            1,
            "position must stay open after invalid veto_close"
        );
        assert!(
            !gate.lock().await.last_close_ts.contains_key("SOL"),
            "cooldown must NOT be recorded on skipped close"
        );
        // Negative and NaN also invalid
        let applied_neg =
            apply_review_action(&store, &gate, &pos, -1.0, ReviewAction::VetoClose, None).await;
        assert!(!applied_neg, "negative mark must be skipped");
        let applied_nan =
            apply_review_action(&store, &gate, &pos, f64::NAN, ReviewAction::VetoClose, None).await;
        assert!(!applied_nan, "NaN mark must be skipped");
        assert_eq!(store.open_positions().await.unwrap().len(), 1);
        // Valid mark does close
        let applied_valid =
            apply_review_action(&store, &gate, &pos, 100.5, ReviewAction::VetoClose, None).await;
        assert!(applied_valid, "valid mark must close");
        assert!(store.open_positions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn veto_close_missing_mark_via_zero_sentinel_is_skipped() {
        // Review loop now propagates 0.0 sentinel for missing snapshot row (was entry_px before).
        // apply_review_action must treat 0.0 as unknown and skip.
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let pos = store
            .open_position(
                "MISSING",
                Side::Short,
                &test_sized(),
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        // Simulate mark resolution where snapshot lacks the market → 0.0 sentinel
        let missing_mark: f64 = 0.0;
        let applied = apply_review_action(
            &store,
            &gate,
            &pos,
            missing_mark,
            ReviewAction::VetoClose,
            None,
        )
        .await;
        assert!(!applied, "missing market (0 sentinel) must NOT veto_close");
        assert_eq!(store.open_positions().await.unwrap().len(), 1);
    }

    // HIP-3 perps are 24/7 — regression for is_xyz_open constant "open" behavior.
    // Ground truth 2026-08-09: (a) allMids ws streams xyz mids continuously through weekends
    // on our live daemon, (b) user-side proof: caller trading xyz:SPCX on a Saturday.
    #[test]
    fn xyz_is_open_24_7_weekend_and_off_hours() {
        // Saturday 2026-08-08 15:00 UTC (11:00 ET) — old RTH code returned "closed"
        let sat = chrono::Utc
            .with_ymd_and_hms(2026, 8, 8, 15, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            is_xyz_open("xyz:TSLA", sat),
            "open",
            "xyz:TSLA must be open on Saturday (HIP-3 24/7)"
        );
        assert_eq!(
            is_xyz_open("xyz:SPCX", sat),
            "open",
            "xyz:SPCX must be open on Saturday (caller evidence)"
        );

        // Sunday early ET: 2026-08-09 10:00 UTC = 06:00 ET Sunday — old code "closed"
        let sun_early = chrono::Utc
            .with_ymd_and_hms(2026, 8, 9, 10, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            is_xyz_open("xyz:SPCX", sun_early),
            "open",
            "xyz:SPCX must be open Sunday early ET (24/7)"
        );

        // Monday pre-RTH: 2026-08-10 10:00 UTC = 06:00 ET Monday — old code "closed"
        let mon_pre = chrono::Utc
            .with_ymd_and_hms(2026, 8, 10, 10, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            is_xyz_open("xyz:TSLA", mon_pre),
            "open",
            "xyz:TSLA must be open Monday pre-RTH (24/7)"
        );

        // Monday RTH: 2026-08-10 15:00 UTC = 11:00 ET Monday — both old and new "open"
        let mon_rth = chrono::Utc
            .with_ymd_and_hms(2026, 8, 10, 15, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(is_xyz_open("xyz:TSLA", mon_rth), "open");
    }

    // ── T1: hardened staleness, regime gate, churn control (full EntryGate chain) ──────

    fn feats(vol1h: f64) -> crate::contracts::Features {
        crate::contracts::Features {
            r5m: 0.0,
            r1h: 0.0,
            r24h: 0.0,
            vol1h,
            funding_z: 0.0,
            range_pos: 0.5,
        }
    }

    fn market_row(
        market: &str,
        features: Option<crate::contracts::Features>,
    ) -> crate::contracts::MarketRow {
        crate::contracts::MarketRow {
            market: market.to_string(),
            mid: 100.0,
            mark: 100.0,
            oracle: 100.0,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: 100.0,
            features,
        }
    }

    fn snapshot_at(ts: i64, markets: Vec<crate::contracts::MarketRow>) -> Arc<RwLock<Snapshot>> {
        Arc::new(RwLock::new(Snapshot { ts, markets }))
    }

    /// A gate whose data clocks are both fresh at `now_ms` — so tests exercise the rail they
    /// are about, not the staleness pre-check.
    fn gate_at(
        store: &Store,
        gate: &Arc<Mutex<GateState>>,
        snapshot: Arc<RwLock<Snapshot>>,
        cfg: crate::config::RiskCfg,
        now_ms: i64,
    ) -> EntryGate {
        let ws_fresh = Arc::new(hl_ws::WsFreshness::new());
        ws_fresh.mark_mids(now_ms);
        ws_fresh.mark_ctxs(now_ms);
        EntryGate {
            risk: Arc::new(Risk::new(cfg)),
            store: store.clone(),
            gate: gate.clone(),
            snapshot,
            ws_fresh,
            regime_once: Arc::new(DayOnce::new()),
            // an hour-old daemon: past the boot grace, so the ws clock is the real clock
            boot_ms: now_ms - 3_600_000,
        }
    }

    /// R2's flagged gap: the 30s REST ctx poll re-stamps `snapshot.ts` on every success, so a
    /// fresh snapshot said nothing about whether prices were still moving.
    #[tokio::test]
    async fn entry_age_is_the_max_of_snapshot_and_ws_clocks() {
        let now = 1_800_000_000_000i64;
        let boot = now - 3_600_000;
        let ws = hl_ws::WsFreshness::new();

        // fresh REST stamp, mids stream dead for 10 minutes -> the ws clock wins
        let snap = snapshot_at(now, vec![]);
        ws.mark_mids(now - 600_000);
        ws.mark_ctxs(now - 600_000);
        assert_eq!(
            entry_data_age_ms(&snap, &ws, now, boot).await,
            600_000,
            "dead feed must not be masked by a fresh REST stamp"
        );

        // live streams, frozen snapshot -> the snapshot clock wins
        let stale_snap = snapshot_at(now - 300_000, vec![]);
        ws.mark_mids(now);
        assert_eq!(
            entry_data_age_ms(&stale_snap, &ws, now, boot).await,
            300_000
        );

        // both fresh -> fresh
        assert_eq!(entry_data_age_ms(&snap, &ws, now, boot).await, 0);

        // never a frame yet -> ages from boot, not from the epoch
        let virgin = hl_ws::WsFreshness::new();
        assert_eq!(
            entry_data_age_ms(&snap, &virgin, now, boot).await,
            3_600_000
        );
        // boot grace: for the first max_feature_age_s of a fresh process the floor keeps the
        // gate open (a daemon 30s old cannot have a 10-minute-old feed), then it bites.
        let just_booted = now - 30_000;
        assert_eq!(
            entry_data_age_ms(&snap, &virgin, now, just_booted).await,
            30_000
        );
    }

    #[tokio::test]
    async fn hardened_staleness_refuses_entry_on_fresh_rest_but_dead_ws() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        // snapshot stamped RIGHT NOW by the REST poll, BTC calm — only the feed is dead.
        let snapshot = snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]);
        let mut eg = gate_at(&store, &gate, snapshot, test_risk_cfg(), now);

        // both clocks fresh -> entry allowed
        assert!(
            eg.check("", "SOL", 0.9, now).await.is_ok(),
            "fresh state trades"
        );

        // mids + ctxs silent for 121s while the REST stamp stays current -> refused
        let dead = Arc::new(hl_ws::WsFreshness::new());
        dead.mark_mids(now - 121_000);
        dead.mark_ctxs(now - 121_000);
        eg.ws_fresh = dead;
        assert_eq!(
            eg.check("", "SOL", 0.9, now).await,
            Err(GateRefusal::StaleData),
            "a dead mids stream must refuse the entry even with a fresh snapshot.ts"
        );
    }

    #[tokio::test]
    async fn regime_gate_blocks_new_entries_on_hot_btc_and_is_inactive_without_it() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let cfg = test_risk_cfg(); // regime_vol_max = 1.5

        // hot BTC -> Regime
        let hot = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(1.51)))]),
            cfg.clone(),
            now,
        );
        assert_eq!(hot.check("", "SOL", 0.9, now).await, Err(GateRefusal::Regime));

        // exactly at the max -> trades (strict `>`)
        let edge = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(1.5)))]),
            cfg.clone(),
            now,
        );
        assert!(
            edge.check("", "SOL", 0.9, now).await.is_ok(),
            "exactly regime_vol_max trades"
        );

        // calm -> trades
        let calm = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            cfg.clone(),
            now,
        );
        assert!(calm.check("", "SOL", 0.9, now).await.is_ok());

        // BTC row present but features not computed yet -> gate inactive
        let no_feats = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", None)]),
            cfg.clone(),
            now,
        );
        assert!(
            no_feats.check("", "SOL", 0.9, now).await.is_ok(),
            "missing features must not halt trading"
        );

        // BTC row absent entirely -> gate inactive (WARN once per day, log-only)
        let no_btc = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("SOL", Some(feats(9.0)))]),
            cfg,
            now,
        );
        assert!(
            no_btc.check("", "SOL", 0.9, now).await.is_ok(),
            "absent BTC row must not halt trading"
        );
        // the latch is one-shot per UTC day
        assert!(
            !no_btc.regime_once.first_today(&utc_day_key(now)),
            "WARN latch already spent today"
        );
    }

    #[tokio::test]
    async fn per_market_cap_allows_three_entries_a_day_then_refuses() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let eg = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            test_risk_cfg(),
            now,
        );

        for n in 1..=3 {
            // the Nth entry decision is gated BEFORE the position exists
            assert!(
                eg.check("", "SOL", 0.9, now).await.is_ok(),
                "entry {n} of 3 must pass"
            );
            let p = store
                .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
                .await
                .expect("open");
            // close on TP so neither the dup-market rail nor the post-SL cooldown interferes
            store
                .close_position(p.id, 101.0, "tp", None)
                .await
                .expect("close");
        }
        assert_eq!(
            eg.check("", "SOL", 0.9, now).await,
            Err(GateRefusal::PerMarketCap),
            "4th entry on SOL refused"
        );
        // other markets are untouched by SOL's budget
        assert!(
            eg.check("", "ETH", 0.9, now).await.is_ok(),
            "the cap is per market"
        );
        // and the count is per UTC day: tomorrow's clock sees none of today's entries
        let tomorrow = now + 86_400_000;
        let eg_tomorrow = gate_at(
            &store,
            &gate,
            snapshot_at(tomorrow, vec![market_row("BTC", Some(feats(0.4)))]),
            test_risk_cfg(),
            tomorrow,
        );
        assert!(
            eg_tomorrow.check("", "SOL", 0.9, tomorrow).await.is_ok(),
            "day roll refills the per-market budget"
        );
    }

    #[tokio::test]
    async fn stop_out_holds_a_market_for_120m_while_tp_only_holds_30m() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let snap = |ts: i64| snapshot_at(ts, vec![market_row("BTC", Some(feats(0.4)))]);

        // SOL stopped out just now (ledger records the cause as trades.action='sl')
        let p = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        store
            .close_position(p.id, 99.0, "sl", None)
            .await
            .expect("close");

        // past the base 30m window the frozen gate is happy, but the SL window still holds
        let t31 = now + 31 * 60 * 1000;
        let eg31 = gate_at(&store, &gate, snap(t31), test_risk_cfg(), t31);
        assert_eq!(
            eg31.check("", "SOL", 0.9, t31).await,
            Err(GateRefusal::Cooldown),
            "31m after a stop-out is still cooling"
        );
        let t119 = now + 119 * 60 * 1000;
        let eg119 = gate_at(&store, &gate, snap(t119), test_risk_cfg(), t119);
        assert_eq!(
            eg119.check("", "SOL", 0.9, t119).await,
            Err(GateRefusal::Cooldown)
        );
        // 121m later the market is tradeable again
        let t121 = now + 121 * 60 * 1000;
        let eg121 = gate_at(&store, &gate, snap(t121), test_risk_cfg(), t121);
        assert!(
            eg121.check("", "SOL", 0.9, t121).await.is_ok(),
            "121m after a stop-out trades again"
        );

        // ETH took profit instead: only the base 30m window applies, so 31m later it trades
        let e = store
            .open_position("ETH", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        store
            .close_position(e.id, 101.0, "tp", None)
            .await
            .expect("close");
        assert!(
            eg31.check("", "ETH", 0.9, t31).await.is_ok(),
            "a TP close is not extended past cooldown_min"
        );
        // a veto/time-stop close is treated the same as a TP (only stop-outs are penalised)
        let v = store
            .open_position("HYPE", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        store
            .close_position(v.id, 100.5, "veto_close", None)
            .await
            .expect("close");
        assert!(
            eg31.check("", "HYPE", 0.9, t31).await.is_ok(),
            "veto_close is not a stop-out"
        );
    }

    #[tokio::test]
    async fn morning_budget_paces_entries_until_1200_utc() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let real_now = chrono::Utc::now().timestamp_millis();
        // 12 entries taken today, each closed on TP so only the pacing rail can speak.
        for i in 0..12 {
            let m = format!("M{i}");
            let p = store
                .open_position(&m, Side::Long, &test_sized(), 100.0, false, 24.0, None)
                .await
                .expect("open");
            store
                .close_position(p.id, 101.0, "tp", None)
                .await
                .expect("close");
        }
        assert_eq!(build_gate_state(&store, &gate, "").await.daily_count, 12);

        let day_start = crate::risk::utc_day_start_ms(real_now);
        let snap = |ts: i64| snapshot_at(ts, vec![market_row("BTC", Some(feats(0.4)))]);

        // 11:59:59.999 UTC — the morning budget is spent
        let morning = day_start + 11 * 3_600_000 + 59 * 60_000 + 59_999;
        let eg_morning = gate_at(&store, &gate, snap(morning), test_risk_cfg(), morning);
        assert_eq!(
            eg_morning.check("", "NEW", 0.9, morning).await,
            Err(GateRefusal::Paced),
            "13th entry before noon is paced"
        );

        // 12:00:00.000 UTC sharp — released (the daily cap of 20 still applies via gate_entry)
        let noon = day_start + 12 * 3_600_000;
        let eg_noon = gate_at(&store, &gate, snap(noon), test_risk_cfg(), noon);
        assert!(
            eg_noon.check("", "NEW", 0.9, noon).await.is_ok(),
            "12:00 UTC releases the budget"
        );

        // one entry short of the budget still passes in the morning
        let smaller = crate::config::RiskCfg {
            morning_entry_budget: 13,
            ..test_risk_cfg()
        };
        let eg_under = gate_at(&store, &gate, snap(morning), smaller, morning);
        assert!(
            eg_under.check("", "NEW", 0.9, morning).await.is_ok(),
            "12 of a 13 budget still trades"
        );
    }

    #[tokio::test]
    async fn churn_rails_fail_closed_when_the_ledger_cannot_be_read() {
        // A ledger we cannot read is not permission to trade: the churn inputs are the only
        // record of how much this market has already been traded today.
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let eg = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![market_row("BTC", Some(feats(0.4)))]),
            test_risk_cfg(),
            now,
        );
        assert!(
            eg.check("", "SOL", 0.9, now).await.is_ok(),
            "healthy ledger trades"
        );
        store.pool().close().await;
        assert_eq!(
            eg.check("", "SOL", 0.9, now).await,
            Err(GateRefusal::PerMarketCap),
            "unreadable ledger must refuse"
        );
    }

    // ── T2: screener exclusion feed ───────────────────────────────────────────────────

    fn liquid_row(market: &str) -> crate::contracts::MarketRow {
        crate::contracts::MarketRow {
            day_ntl_vlm: 5_000_000.0,
            ..market_row(market, Some(feats(0.4)))
        }
    }

    /// `min_score: 0.0` — this is a test about the exclusion set, not about ranking, so every
    /// vlm-passing row nominates unless something benched it.
    fn screen_all(rows: &[crate::contracts::MarketRow], excluded: &HashSet<String>) -> Vec<String> {
        let scr = crate::config::ScreenerCfg {
            interval_s: 45,
            top_k: 20,
            min_score: 0.0,
            skip_recheck_min: 15,
            skip_recheck_score_jump: 0.5,
        };
        let uni = crate::config::UniverseCfg {
            dexs: vec![String::new(), "xyz".into()],
            min_vlm_native: 500_000.0,
            min_vlm_dex: 200_000.0,
            allowlist: vec![],
        };
        screener::screen(rows, &scr, &uni, excluded)
            .into_iter()
            .map(|n| n.market)
            .collect()
    }

    /// A market that spent its per-market daily cap is refused by `gate_churn` no matter how
    /// well it screens — nominating it again only burns an analyst call. The feed reads the same
    /// ledger counter the gate does.
    #[tokio::test]
    async fn capped_market_is_not_nominated() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let eg = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![]),
            test_risk_cfg(),
            now,
        );
        let rows = vec![liquid_row("SOL"), liquid_row("ETH")];

        // nothing traded yet: both markets are candidates
        let open_field = screen_all(&rows, &eg.excluded_markets(now).await);
        assert!(open_field.contains(&"SOL".to_string()) && open_field.contains(&"ETH".to_string()));

        // SOL takes its three entries for the day, each closed on TP (so only the cap can speak)
        for _ in 0..3 {
            let p = store
                .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
                .await
                .expect("open");
            store
                .close_position(p.id, 101.0, "tp", None)
                .await
                .expect("close");
        }
        // the gate itself refuses a 4th SOL entry ...
        assert_eq!(
            eg.check("", "SOL", 0.9, now).await,
            Err(GateRefusal::PerMarketCap)
        );
        // ... and the screener never even nominates it
        let excluded = eg.excluded_markets(now).await;
        assert!(
            excluded.contains("SOL"),
            "capped market must be excluded, got {excluded:?}"
        );
        let nominated = screen_all(&rows, &excluded);
        assert!(
            !nominated.contains(&"SOL".to_string()),
            "capped SOL nominated anyway: {nominated:?}"
        );
        assert!(
            nominated.contains(&"ETH".to_string()),
            "an untraded market must still nominate"
        );
    }

    /// The other two market-scoped rails: an open position (DupMarket) and a stop-out's 120m
    /// window. A market that closed on TP with the base window lapsed is NOT excluded.
    #[tokio::test]
    async fn open_and_stopped_out_markets_are_excluded_but_lapsed_ones_are_not() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let eg = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![]),
            test_risk_cfg(),
            now,
        );

        // OPEN: a live position benches its market
        store
            .open_position("ETH", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        // STOPPED OUT: ledger-backed 120m window, no in-memory stamp needed (survives restarts)
        let btc = store
            .open_position("BTC", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        store
            .close_position(btc.id, 99.0, "sl", None)
            .await
            .expect("sl");
        // TP'd with no in-memory stamp: the base window is the memory map's, and it is empty
        let sol = store
            .open_position("SOL", Side::Long, &test_sized(), 100.0, false, 24.0, None)
            .await
            .expect("open");
        store
            .close_position(sol.id, 101.0, "tp", None)
            .await
            .expect("tp");

        let excluded = eg.excluded_markets(now).await;
        assert!(
            excluded.contains("ETH"),
            "open position must bench its market"
        );
        assert!(
            excluded.contains("BTC"),
            "post-SL cooldown must bench its market"
        );
        assert!(
            !excluded.contains("SOL"),
            "a lapsed base window must not bench forever: {excluded:?}"
        );

        let rows = vec![liquid_row("SOL"), liquid_row("ETH"), liquid_row("BTC")];
        let nominated = screen_all(&rows, &excluded);
        assert_eq!(
            nominated,
            vec!["SOL".to_string()],
            "only the free market nominates: {nominated:?}"
        );

        // 121 minutes later the stop-out window has lapsed and BTC is a candidate again
        let later = now + 121 * 60_000;
        assert!(
            !eg.excluded_markets(later).await.contains("BTC"),
            "cooldown must expire"
        );
    }

    /// The aggregate the feed reads and the per-market counter the gate reads must never
    /// disagree — they are the same predicate written twice.
    #[tokio::test]
    async fn exclusion_feed_counts_agree_with_the_gate_counter() {
        let store = seeded_store().await;
        let gate = Arc::new(Mutex::new(GateState::new()));
        let now = chrono::Utc::now().timestamp_millis();
        let eg = gate_at(
            &store,
            &gate,
            snapshot_at(now, vec![]),
            test_risk_cfg(),
            now,
        );
        let day_start = crate::risk::utc_day_start_ms(now);
        for (m, n) in [("SOL", 2), ("ETH", 1)] {
            for _ in 0..n {
                let p = store
                    .open_position(m, Side::Long, &test_sized(), 100.0, false, 24.0, None)
                    .await
                    .expect("open");
                store
                    .close_position(p.id, 101.0, "tp", None)
                    .await
                    .expect("close");
            }
        }
        let agg = eg.per_market_entries(now).await;
        assert_eq!(
            agg,
            vec![("ETH".to_string(), 1), ("SOL".to_string(), 2)],
            "aggregate, market-ordered"
        );
        for (market, count) in agg {
            let single = store
                .market_entries_since(&market, day_start)
                .await
                .expect("per-market read");
            assert_eq!(
                single, count,
                "aggregate and per-market counter disagree for {market}"
            );
        }
    }

    #[test]
    fn utc_day_key_matches_the_alert_latch_format() {
        // 2026-08-09T14:00:00Z
        assert_eq!(utc_day_key(1_786_284_000_000), "2026-08-09");
        // last ms of that UTC day
        assert_eq!(utc_day_key(1_786_319_999_999), "2026-08-09");
        assert_eq!(utc_day_key(1_786_320_000_000), "2026-08-10");
    }

    #[test]
    fn is_xyz_open_native_and_xyz_always_open() {
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 8, 8, 15, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(is_xyz_open("SOL", ts), "open", "native SOL always open");
        assert_eq!(is_xyz_open("BTC", ts), "open");
        // xyz prefix must also be open even when native is open — 24/7 for all markets
        assert_eq!(is_xyz_open("xyz:GOLD", ts), "open");
        // non-xyz prefix with colon? stays open (only xyz: was ever gated, but now all open)
        assert_eq!(is_xyz_open("BTC", 0), "open");
    }
}
