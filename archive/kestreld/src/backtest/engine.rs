//! The replay loop (spec Decision 6): one deterministic pass over 1m steps, driving the REAL
//! strategy modules — `screener::screen`, `sizing::size_position`, `risk::Risk`'s gate chain —
//! against recomputed features, into an in-memory ledger shaped like the live one.
//!
//! WHAT IS REAL AND WHAT IS MIRRORED
//!
//! Real (the daemon's own code, called here): the screener, sizing, every gate predicate
//! (`gate_data_age` / `gate_entry` / `gate_regime` / `gate_churn`), the fill rules
//! ([`super::fills`]), the feature formulas ([`super::features`]).
//!
//! Mirrored, because the live version is async and ledger-backed rather than pure:
//!
//!   * `main::EntryGate::check` — the ORDER the four gate segments run in, reproduced verbatim
//!     in [`Replay::gate`]. The predicates are the real ones; only the plumbing that fetches
//!     their arguments is local.
//!   * `main::EntryGate::excluded_markets` — the screener's pre-filter (open / per-market cap
//!     spent / inside a cooldown), reproduced in [`Replay::excluded`].
//!   * the 60s equity + kill task — equity is `bankroll − Σfees + Σrealized + unrealized`,
//!     exactly `ledger::Store::equity`, evaluated once per step (the live cadence is 60s, so
//!     this is the same clock); the kill switch latches for the rest of the UTC day and clears
//!     on the day roll, which also rebases day-open equity and the daily entry count.
//!
//! ABSENT BY DESIGN (spec Decision 2): no analyst, therefore no per-nominee conviction (a
//! fixed `--conviction` stands in), no stop/tp overrides, no review loop, no veto closes and no
//! time-stop-by-review. Brackets and horizon expiry are the only exits.
//!
//! DETERMINISM (spec Decision 6): no wall clock and no RNG anywhere below. Markets live in a
//! `BTreeMap` and are therefore always visited in name order, which is also how screener ties
//! break; positions are visited by id. Two runs over the same cache produce the same bytes.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use tracing::debug;

use crate::config::Config;
use crate::contracts::{EquityPoint, MarketRow, Nominee, Position, Side, Trade};
use crate::risk::{CloseCause, GateRefusal, GateState, KillState, Risk};
use crate::screener::screen;
use crate::sizing::size_position;

use super::cache::{Cache, CacheError};
use super::features::{MarketSeries, funding_z_at, funding_z_series};
use super::fills::{self, Bar, BracketOutcome};
use super::{DAY_MS, MINUTE_MS};

/// Nominees considered per screener tick. The live screener hands the top 2 to the analyst
/// (`nom.into_iter().take(2)`, bounded by the 2-permit analyst semaphore); with no analyst the
/// number is still the ceiling on entries a single tick can produce, so it is mirrored rather
/// than silently relaxed.
pub const MAX_ENTRIES_PER_TICK: usize = 2;

/// Tape loaded BEFORE the replay window so features are warm at the first traded minute: a
/// full day, which is the longest lookback any feature uses (r24h, range_pos, the volume
/// proxy). Without it the first day of a run would trade on the fallback returns a warming
/// engine reports.
pub const WARMUP_MS: i64 = DAY_MS;

/// Funding history pulled before the window, matching the daemon's boot seed
/// (`main::boot_seed_funding` seeds 7 days into the same ring).
pub const FUNDING_SEED_MS: i64 = 7 * DAY_MS;

/// Market whose 1h realized vol drives the regime gate — `main::REGIME_MARKET`.
pub const REGIME_MARKET: &str = "BTC";

/// One market's replay input: the dense 1m tape plus the funding series, pre-walked through
/// the live ring so `funding_z` at any minute is a lookup rather than a re-derivation.
#[derive(Debug, Clone)]
pub struct MarketData {
    pub series: MarketSeries,
    /// `(sample ts, rate)` — the raw rate carried on `MarketRow.funding`.
    pub funding: Vec<(i64, f64)>,
    /// `(sample ts, z)` — [`funding_z_series`] output, the live ring's own z.
    pub funding_z: Vec<(i64, f64)>,
}

impl MarketData {
    pub fn new(market: &str, candles: Vec<super::cache::CachedCandle>, funding: Vec<(i64, f64)>) -> Self {
        let funding_z = funding_z_series(market, &funding);
        Self { series: MarketSeries::new(market, candles), funding, funding_z }
    }

    /// The funding rate in force at `t` — the last sample at or before it, 0.0 before the
    /// first (the live ctx stream has nothing to report either).
    fn funding_at(&self, t: i64) -> f64 {
        match self.funding.binary_search_by_key(&t, |(ts, _)| *ts) {
            Ok(i) => self.funding[i].1,
            Err(0) => 0.0,
            Err(i) => self.funding[i - 1].1,
        }
    }
}

/// Rows the screener would see at minute `t`: one per market whose tape actually covers it
/// exactly. A free function — not a `Replay` method — so [`super::stats`] can build the same
/// rows the replay's screener sees without instantiating the whole mutable replay state; the
/// only consumer that needs a `Replay` around it is [`Replay::rows_at`], which just forwards
/// here.
pub fn rows_at(data: &BTreeMap<String, MarketData>, t: i64) -> Vec<MarketRow> {
    data.iter()
        .filter_map(|(market, d)| {
            let idx = d.series.index_at(t)?;
            if d.series.candle(idx)?.t != t {
                return None;
            }
            let z = funding_z_at(&d.funding_z, t);
            d.series.market_row_at(idx, d.funding_at(t), z)
        })
        .collect()
}

/// Load the replay window (plus warmup) for `markets` out of the cache.
pub async fn load(
    cache: &Cache,
    markets: &[String],
    from_ms: i64,
    to_ms: i64,
) -> Result<BTreeMap<String, MarketData>, CacheError> {
    let mut out = BTreeMap::new();
    for market in markets {
        let candles = cache.candles(market, from_ms - WARMUP_MS, to_ms).await?;
        if candles.is_empty() {
            continue;
        }
        let funding = cache.funding(market, from_ms - FUNDING_SEED_MS, to_ms).await?;
        out.insert(market.clone(), MarketData::new(market, candles, funding));
    }
    Ok(out)
}

/// What a run was asked to do. Carried into the report so a result can be reproduced.
#[derive(Debug, Clone, PartialEq)]
pub struct RunParams {
    pub from_ms: i64,
    pub to_ms: i64,
    pub conviction: f64,
    /// Markets actually replayed, sorted.
    pub markets: Vec<String>,
    /// `--set key=value` overrides applied to the config, sorted.
    pub overrides: Vec<String>,
}

/// One position as the in-memory ledger holds it: the live `Position` shape plus the status
/// columns `positions` carries in sqlite.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionRow {
    pub pos: Position,
    pub status: &'static str,
    pub closed_ts: Option<i64>,
    /// `sl` | `tp` | `time_stop` — the `trades.action` of the close, absent while open.
    pub close_action: Option<String>,
}

/// The raw ledger a run produced. Turned into a `RunReport` by [`super::report`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub params: RunParams,
    pub bankroll: f64,
    /// 1m steps actually walked.
    pub bars: i64,
    /// First and last minute of tape the replay saw (warmup included).
    pub tape: Option<(i64, i64)>,
    pub positions: Vec<PositionRow>,
    pub trades: Vec<Trade>,
    pub equity: Vec<EquityPoint>,
    /// `GateRefusal::as_str()` -> count.
    pub refusals: BTreeMap<String, i64>,
    /// UTC days on which the kill switch latched.
    pub kill_days: Vec<String>,
    /// Entries the gates allowed that could not be filled: the signal landed on the last
    /// minute of a market's tape, so there was no next open to fill at.
    pub unfilled: i64,
}

/// UTC day key (`%Y-%m-%d`) — `main::utc_day_key`, the same key the daily count and the kill
/// latch are keyed by.
fn utc_day_key(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(0).expect("epoch"))
        .format("%Y-%m-%d")
        .to_string()
}

/// The replay's whole mutable world.
struct Replay<'a> {
    cfg: &'a Config,
    risk: Risk,
    data: &'a BTreeMap<String, MarketData>,
    params: RunParams,

    positions: Vec<PositionRow>,
    trades: Vec<Trade>,
    equity: Vec<EquityPoint>,

    fees_total: f64,
    realized_total: f64,

    /// `GateState::last_close_ts` — the in-memory base-cooldown map `gate_entry` reads.
    last_close_ts: BTreeMap<String, i64>,
    /// The ledger's `last_close(market)` — `(ts, action)`, what `gate_churn` reads.
    last_close: BTreeMap<String, (i64, String)>,
    /// Entries taken per market since 00:00 UTC (`Store::market_entries_since`).
    market_entries_today: BTreeMap<String, usize>,

    day_key: String,
    day_open_equity: f64,
    daily_count: usize,
    kill_active: bool,
    kill_days: BTreeSet<String>,

    next_id: i64,
    refusals: BTreeMap<String, i64>,
    unfilled: i64,
}

impl<'a> Replay<'a> {
    fn new(cfg: &'a Config, data: &'a BTreeMap<String, MarketData>, params: RunParams) -> Self {
        Self {
            risk: Risk::new(cfg.risk.clone()),
            cfg,
            data,
            day_key: utc_day_key(params.from_ms),
            params,
            positions: Vec::new(),
            trades: Vec::new(),
            equity: Vec::new(),
            fees_total: 0.0,
            realized_total: 0.0,
            last_close_ts: BTreeMap::new(),
            last_close: BTreeMap::new(),
            market_entries_today: BTreeMap::new(),
            day_open_equity: cfg.sizing.bankroll,
            daily_count: 0,
            kill_active: false,
            kill_days: BTreeSet::new(),
            next_id: 1,
            refusals: BTreeMap::new(),
            unfilled: 0,
        }
    }

    /// Rows the screener sees this minute: one per market whose tape actually covers `t`.
    /// A market whose history starts later (or ended earlier) is simply absent, exactly as it
    /// would be absent from a live snapshot.
    fn rows_at(&self, t: i64) -> Vec<MarketRow> {
        rows_at(self.data, t)
    }

    fn open_indices(&self) -> Vec<usize> {
        (0..self.positions.len()).filter(|i| self.positions[*i].status == "open").collect()
    }

    fn open_markets(&self) -> Vec<String> {
        self.open_indices().into_iter().map(|i| self.positions[i].pos.market.clone()).collect()
    }

    /// Unrealized pnl of the open book at `marks` — `ledger::Store::equity`'s inner loop,
    /// including its money guard (a missing or non-positive mark contributes nothing rather
    /// than pricing the position at zero).
    fn unrealized(&self, marks: &BTreeMap<String, f64>) -> f64 {
        self.open_indices()
            .into_iter()
            .filter_map(|i| {
                let p = &self.positions[i].pos;
                let mark = *marks.get(&p.market)?;
                if mark <= 0.0 || !mark.is_finite() {
                    return None;
                }
                Some(fills::gross_pnl(p.side, p.size, p.entry_px, mark))
            })
            .sum()
    }

    fn equity_at(&self, marks: &BTreeMap<String, f64>) -> f64 {
        self.cfg.sizing.bankroll - self.fees_total + self.realized_total + self.unrealized(marks)
    }

    /// When this market's cooldown lifts, under whichever of the two clocks is longer — the
    /// same pair `main::EntryGate::cooldowns` reports and the gates enforce (base
    /// `cooldown_min` from any close, extended `cooldown_after_sl_min` after a stop-out).
    fn cooldown_until(&self, market: &str) -> Option<i64> {
        let base_ms = (self.cfg.risk.cooldown_min as i64).saturating_mul(60_000);
        let sl_ms = (self.cfg.risk.cooldown_after_sl_min as i64).saturating_mul(60_000);
        let mut until = self.last_close_ts.get(market).map(|ts| ts + base_ms);
        if let Some((ts, action)) = self.last_close.get(market)
            && CloseCause::from_action(action) == CloseCause::Sl
        {
            let sl_until = ts + sl_ms;
            if until.is_none_or(|u| sl_until >= u) {
                until = Some(sl_until);
            }
        }
        until
    }

    /// `main::EntryGate::excluded_markets`: the market-scoped rails, applied before scoring so
    /// the screener does not spend its top-k on markets the gate is certain to refuse.
    fn excluded(&self, now_ms: i64) -> HashSet<String> {
        let cap = self.cfg.risk.per_market_daily_cap;
        let mut out: HashSet<String> = self.open_markets().into_iter().collect();
        out.extend(self.market_entries_today.iter().filter(|(_, n)| **n >= cap).map(|(m, _)| m.clone()));
        out.extend(
            self.data
                .keys()
                .filter(|m| self.cooldown_until(m).is_some_and(|until| until > now_ms))
                .cloned(),
        );
        out
    }

    /// `main::EntryGate::check`, segment for segment:
    ///   1. staleness pre-check — trivially fresh here (spec Decision 5): the replay's state IS
    ///      the minute it is standing on, so the age is 0. The call stays so the chain is the
    ///      whole chain and a future non-zero age has one place to enter.
    ///   2. the frozen `gate_entry` chain,
    ///   3. the regime post-check,
    ///   4. the churn post-checks.
    fn gate(&self, market: &str, rows: &[MarketRow], now_ms: i64) -> Result<(), GateRefusal> {
        self.risk.gate_data_age(0)?;

        let state = GateState {
            open_markets: self.open_markets(),
            daily_count: self.daily_count,
            last_close_ts: self.last_close_ts.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            kill_active: self.kill_active,
            // The veto flag is set by nothing in the daemon's entry path (it exists for a
            // manual halt); with no analyst there is nothing to set it here either.
            veto: false,
            day_key: self.day_key.clone(),
        };
        self.risk.gate_entry(market, self.params.conviction, &state, now_ms)?;

        let btc_vol1h = rows
            .iter()
            .find(|r| r.market == REGIME_MARKET)
            .and_then(|r| r.features.as_ref())
            .map(|f| f.vol1h);
        self.risk.gate_regime(btc_vol1h)?;

        let entries_today = self.market_entries_today.get(market).copied().unwrap_or(0);
        let last_close =
            self.last_close.get(market).map(|(ts, action)| (*ts, CloseCause::from_action(action)));
        self.risk.gate_churn(entries_today, self.daily_count, last_close, now_ms)
    }

    fn refuse(&mut self, refusal: &GateRefusal) {
        *self.refusals.entry(refusal.as_str().to_string()).or_insert(0) += 1;
    }

    /// Close position `idx` at `px` and record the trade — `ledger::Store::close_position`:
    /// `realized_pnl` is GROSS, the fee is 7.5bp of the exit notional in its own column, and
    /// the close feeds both cooldown clocks.
    fn close(&mut self, idx: usize, px: f64, action: &str, ts: i64) {
        let p = self.positions[idx].pos.clone();
        let gross = fills::gross_pnl(p.side, p.size, p.entry_px, px);
        let fee = fills::fee(p.size * px);
        self.fees_total += fee;
        self.realized_total += gross;
        let id = self.next_trade_id();
        self.trades.push(Trade {
            id,
            position_id: p.id,
            market: p.market.clone(),
            action: action.to_string(),
            px,
            size: p.size,
            fee,
            realized_pnl: gross,
            ts,
            // Historical order books are not served, so every backtest fill is the flat-slip
            // model — the same token the live ledger writes when it has no usable book.
            fill_mode: "flat".to_string(),
        });
        self.positions[idx].status = "closed";
        self.positions[idx].closed_ts = Some(ts);
        self.positions[idx].close_action = Some(action.to_string());
        self.last_close_ts.insert(p.market.clone(), ts);
        self.last_close.insert(p.market, (ts, action.to_string()));
    }

    fn next_trade_id(&self) -> i64 {
        self.trades.len() as i64 + 1
    }

    /// Exits, resolved against candle `t` — brackets first (stop before target inside the
    /// candle, [`fills::touch`]), then the horizon. Same precedence as `triggers::check_triggers`.
    fn exits(&mut self, t: i64) {
        let horizon_ms = (self.cfg.risk.time_stop_hours * 3_600_000.0) as i64;
        for idx in self.open_indices() {
            let (market, side, sl_px, tp_px, opened_ts) = {
                let p = &self.positions[idx].pos;
                (p.market.clone(), p.side, p.sl_px, p.tp_px, p.opened_ts)
            };
            let Some(d) = self.data.get(&market) else { continue };
            let Some(bar_idx) = d.series.index_at(t) else { continue };
            let Some(candle) = d.series.candle(bar_idx) else { continue };
            if candle.t != t {
                continue;
            }
            let bar = Bar::new(candle.h, candle.l, candle.c);
            match fills::touch(bar, side, sl_px, tp_px) {
                Some(BracketOutcome::Sl) => self.close(idx, sl_px, "sl", t),
                Some(BracketOutcome::Tp) => self.close(idx, tp_px, "tp", t),
                _ if t >= opened_ts.saturating_add(horizon_ms) => {
                    self.close(idx, candle.c, "time_stop", t)
                }
                _ => {}
            }
        }
    }

    /// Equity mark + kill evaluation + the 00:00 UTC day roll, in the order the live 60s task
    /// runs them. The roll rebases day-open equity, clears the kill latch and resets both the
    /// daily entry count and the per-market counts (all of which are UTC-day keyed live).
    fn mark(&mut self, t: i64, marks: &BTreeMap<String, f64>) {
        let equity = self.equity_at(marks);
        let day = utc_day_key(t);
        if day != self.day_key {
            self.day_key = day;
            self.day_open_equity = equity;
            self.kill_active = false;
            self.daily_count = 0;
            self.market_entries_today.clear();
        }
        self.equity.push(EquityPoint { ts: t, equity });
        if self.risk.on_equity(equity, self.day_open_equity) == KillState::Killed && !self.kill_active {
            self.kill_active = true;
            self.kill_days.insert(self.day_key.clone());
            debug!(day = %self.day_key, equity, day_open = self.day_open_equity, "backtest kill switch latched");
        }
    }

    /// Screener tick + entry pass. Nominees are scored on this minute's close and fill at the
    /// NEXT minute's open (spec Decision 5) — a signal can never be traded at the price that
    /// produced it.
    fn entries(&mut self, t: i64, rows: &[MarketRow]) {
        let excluded = self.excluded(t);
        let mut nominees: Vec<Nominee> =
            screen(rows, &self.cfg.screener, &self.cfg.universe, &excluded);
        // `screener::screen` stamps `ts` from the wall clock — the one impurity in an otherwise
        // pure module. Restamping with the replay's own minute keeps the run deterministic
        // without forking the screener.
        for n in &mut nominees {
            n.ts = t;
        }

        for nominee in nominees.into_iter().take(MAX_ENTRIES_PER_TICK) {
            if let Err(refusal) = self.gate(&nominee.market, rows, t) {
                self.refuse(&refusal);
                continue;
            }
            let Some(d) = self.data.get(&nominee.market) else { continue };
            let Some(idx) = d.series.index_at(t) else { continue };
            let Some(next) = d.series.candle(idx + 1).filter(|c| c.t == t + MINUTE_MS) else {
                // The signal landed on the last minute of this market's tape: there is no open
                // to fill at, and inventing one would be inventing a trade.
                self.unfilled += 1;
                continue;
            };
            self.open(&nominee, next.o, t + MINUTE_MS);
        }
    }

    /// `ledger::Store::open_position`: fill, size from the notional, brackets off the fill, a
    /// 7.5bp open fee and the daily counters the churn gates read.
    fn open(&mut self, nominee: &Nominee, open_px: f64, ts: i64) {
        let side = nominee.side_hint;
        let market = nominee.market.clone();
        let sized = size_position(&self.cfg.sizing, nominee.features.vol1h, self.params.conviction);
        let fill_px = fills::entry_fill_px(open_px, side, &market);
        let size = sized.notional / fill_px;
        let (sl_px, tp_px) = fills::bracket_pxs(fill_px, side, sized.stop_pct, sized.tp_pct);
        let fee = fills::fee(sized.notional);
        self.fees_total += fee;

        let id = self.next_id;
        self.next_id += 1;
        let pos = Position {
            id,
            market: market.clone(),
            side,
            entry_px: fill_px,
            size,
            leverage: sized.leverage,
            margin: sized.margin,
            sl_px,
            tp_px,
            opened_ts: ts,
            analyst: String::new(),
            horizon_hours: Some(self.cfg.risk.time_stop_hours),
        };
        let trade_id = self.next_trade_id();
        self.trades.push(Trade {
            id: trade_id,
            position_id: id,
            market: market.clone(),
            action: "open".to_string(),
            px: fill_px,
            size,
            fee,
            realized_pnl: 0.0,
            ts,
            fill_mode: "flat".to_string(),
        });
        self.positions.push(PositionRow { pos, status: "open", closed_ts: None, close_action: None });
        self.daily_count += 1;
        *self.market_entries_today.entry(market).or_insert(0) += 1;
    }
}

/// Replay `data` over `params`' window.
///
/// One step per minute, and within a step: exits resolve on this candle, the book is marked and
/// the kill switch evaluated, then the screener nominates and any entry fills at the NEXT
/// candle's open. That order is what makes the loop causal — nothing entered this minute can
/// exit on the candle that produced its signal.
pub fn run(cfg: &Config, data: &BTreeMap<String, MarketData>, params: RunParams) -> RunOutcome {
    let mut r = Replay::new(cfg, data, params);
    let tape = data.values().filter_map(|d| Some((d.series.first_ts()?, d.series.last_ts()?))).fold(
        None,
        |acc: Option<(i64, i64)>, (a, b)| match acc {
            Some((lo, hi)) => Some((lo.min(a), hi.max(b))),
            None => Some((a, b)),
        },
    );

    let mut bars = 0i64;
    let mut t = r.params.from_ms;
    while t <= r.params.to_ms {
        let rows = r.rows_at(t);
        if !rows.is_empty() {
            bars += 1;
            let marks: BTreeMap<String, f64> = rows.iter().map(|m| (m.market.clone(), m.mid)).collect();
            r.exits(t);
            r.mark(t, &marks);
            r.entries(t, &rows);
        }
        t += MINUTE_MS;
    }

    RunOutcome {
        params: r.params,
        bankroll: cfg.sizing.bankroll,
        bars,
        tape,
        positions: r.positions,
        trades: r.trades,
        equity: r.equity,
        refusals: r.refusals,
        kill_days: r.kill_days.into_iter().collect(),
        unfilled: r.unfilled,
    }
}

/// Sum of `side`-agnostic net pnl per CLOSED position: gross of the close minus both fees.
/// Mirrors the ledger's documented net-per-trade (`realized_pnl - (open_fee + close_fee)`).
pub fn net_by_position(trades: &[Trade]) -> BTreeMap<i64, f64> {
    let mut out: BTreeMap<i64, f64> = BTreeMap::new();
    for t in trades {
        *out.entry(t.position_id).or_insert(0.0) += t.realized_pnl - t.fee;
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::backtest::cache::CachedCandle;

    const M: i64 = MINUTE_MS;

    /// 2026-08-01 00:00:00 UTC — a day start, so the UTC-day helpers (daily cap, morning
    /// budget, kill latch) all key off minute 0 of the fixture.
    pub(crate) const T0: i64 = 1_785_542_400_000;

    /// The fixture config. Deliberately NOT `kestreld.toml`: a golden run has to stay pinned
    /// when the live knobs are tuned.
    ///
    /// `min_score = 3.5` is the one value chosen for the fixture rather than copied from
    /// production, and it is what makes the run scriptable. Cross-sectional z is
    /// scale-invariant, so in a universe where exactly one market has moved that market always
    /// scores |z| = √2 on each moved feature and the laggards −1/√2:
    ///
    /// ```text
    ///   fresh move (r1h AND r5m isolated):  2·√2 + √2 + 0.2 = 4.443   -> nominated
    ///   stale move (r1h only, r5m decayed): 2·√2       + 0.2 = 3.028   -> not nominated
    ///   a laggard:                          2/√2 + 1/√2       = 2.121  -> not nominated
    /// ```
    ///
    /// 3.5 sits between the first two, so each price move nominates for exactly the five
    /// minutes its r5m window remembers it, and every entry in the fixture is a scripted event
    /// rather than a standing invitation.
    pub(crate) fn fixture_cfg() -> Config {
        toml::from_str(
            r#"
            [server]
            port = 7411
            [universe]
            dexs = ["", "xyz"]
            min_vlm_native = 1000.0
            min_vlm_dex = 500.0
            [screener]
            interval_s = 45
            top_k = 6
            min_score = 3.5
            [sizing]
            bankroll = 1000.0
            vol_ref = 0.4
            margin_min = 10.0
            margin_max = 50.0
            [risk]
            max_concurrent = 5
            daily_cap = 20
            cooldown_min = 30
            kill_switch_pct = 12.0
            conviction_min = 0.75
            review_interval_min = 15
            time_stop_hours = 24.0
            [analyst]
            base_url = "https://example.invalid/v1"
            model = "fixture"
            api_key_env = "ANALYST_API_KEY"
            [news]
            rss = []
            [notify]
            bot_token_env = "TG_BOT_TOKEN"
        "#,
        )
        .expect("fixture config parses")
    }

    fn flat(t: i64, px: f64) -> CachedCandle {
        CachedCandle { t, o: px, h: px, l: px, c: px, v: 100.0 }
    }

    /// A tape of `minutes` flat candles at `px`, starting at [`T0`].
    fn flat_tape(px: f64, minutes: i64) -> Vec<CachedCandle> {
        (0..minutes).map(|i| flat(T0 + i * M, px)).collect()
    }

    /// THE GOLDEN FIXTURE (spec Decision 8).
    ///
    /// Three markets, 400 minutes, every price move chosen so the outcome is arithmetic rather
    /// than simulation:
    ///
    ///   * every market sits flat at 100 for the first hour, so `vol1h` at the first signal is
    ///     ~0.064% — under 0.25%, which pins BOTH sizing clamps: `vol_ref/vol` clamps to 1.6 so
    ///     leverage is `round(5 + 15·0.75·1.6) = 23 -> 20`, and `1.5·vol` clamps to the 1.0%
    ///     stop floor, so `stop_pct = 1.0` and `tp_pct = 2.0` exactly.
    ///   * margin is `clamp(1000·(0.01+0.04·0.75), 10, 50) = 40`, so notional is 800 — and a
    ///     bracket taken at its trigger pays exactly `notional × pct` whatever the fill was.
    ///
    ///   AAA: +0.5% at minute 60 -> nominated long, fills at minute 61's open, TP at minute 65.
    ///   BBB: -0.5% at minute 200 -> nominated short, fills at 201, SL at 205. Its re-run at
    ///        minute 250 is inside the 120m post-SL cooldown and must be refused.
    ///   CCC: never moves -> never scores, never trades.
    pub(crate) fn golden_fixture() -> BTreeMap<String, MarketData> {
        let mut aaa = flat_tape(100.0, 400);
        // minute 60: the 0.5% move that scores. Minutes 61..64 hold it (a long fills at 61's
        // open = 100.5 and must not be stopped before its target).
        for c in aaa.iter_mut().skip(60) {
            *c = flat(c.t, 100.5);
        }
        // minute 65: a spike through the 2% target (fill 100.52010 -> tp 102.5305...).
        aaa[65] = CachedCandle { t: T0 + 65 * M, o: 100.5, h: 103.0, l: 100.5, c: 102.6, v: 100.0 };
        for c in aaa.iter_mut().skip(66) {
            *c = flat(c.t, 102.6);
        }

        let mut bbb = flat_tape(100.0, 400);
        // minute 200: -0.5% -> a short nomination; fills at 201's open = 99.5.
        for c in bbb.iter_mut().skip(200) {
            *c = flat(c.t, 99.5);
        }
        // minute 205: a spike through the 1% stop (fill 99.48010 -> sl 100.47490).
        bbb[205] = CachedCandle { t: T0 + 205 * M, o: 99.5, h: 101.0, l: 99.5, c: 100.0, v: 100.0 };
        for c in bbb.iter_mut().skip(206) {
            *c = flat(c.t, 100.0);
        }
        // minute 250: the same shaped signal again, 45 minutes after the stop-out — past the
        // 30m base cooldown, well inside the 120m post-SL window.
        for c in bbb.iter_mut().skip(250) {
            *c = flat(c.t, 99.5);
        }

        let ccc = flat_tape(100.0, 400);

        BTreeMap::from([
            ("AAA".to_string(), MarketData::new("AAA", aaa, vec![])),
            ("BBB".to_string(), MarketData::new("BBB", bbb, vec![])),
            ("CCC".to_string(), MarketData::new("CCC", ccc, vec![])),
        ])
    }

    pub(crate) fn golden_params() -> RunParams {
        RunParams {
            from_ms: T0,
            to_ms: T0 + 399 * M,
            conviction: 0.75,
            markets: vec!["AAA".into(), "BBB".into(), "CCC".into()],
            overrides: vec![],
        }
    }

    pub(crate) fn golden_run() -> RunOutcome {
        run(&fixture_cfg(), &golden_fixture(), golden_params())
    }

    /// Hand-computed, to the cent:
    ///
    /// ```text
    /// AAA long   fill 100.5 × 1.0002 = 100.52010   notional 800  size 7.9585...
    ///            tp = fill × 1.02                  gross +16.000  fees 0.600 + 0.612
    ///            net +14.788
    /// BBB short  fill  99.5 × 0.9998 =  99.48010   notional 800
    ///            sl = fill × 1.01                  gross  -8.000  fees 0.600 + 0.606
    ///            net  -9.206
    /// equity     1000 + 14.788 - 9.206 = 1005.582
    /// ```
    #[test]
    fn golden_run_pins_entries_exits_fees_and_final_equity() {
        let out = golden_run();

        assert_eq!(out.positions.len(), 2, "AAA and BBB trade once each; CCC never scores");
        assert_eq!(out.unfilled, 0);

        let aaa = &out.positions[0];
        assert_eq!(aaa.pos.market, "AAA");
        assert_eq!(aaa.pos.side, Side::Long);
        assert_eq!(aaa.pos.opened_ts, T0 + 61 * M, "fills at the NEXT candle's open");
        assert!((aaa.pos.entry_px - 100.5 * 1.0002).abs() < 1e-9, "entry {}", aaa.pos.entry_px);
        assert!((aaa.pos.leverage - 20.0).abs() < 1e-12, "leverage clamps to 20");
        assert!((aaa.pos.margin - 40.0).abs() < 1e-12, "margin = 1000·(0.01+0.04·0.75)");
        assert!((aaa.pos.size * aaa.pos.entry_px - 800.0).abs() < 1e-9, "notional = margin × leverage");
        assert!((aaa.pos.sl_px - aaa.pos.entry_px * 0.99).abs() < 1e-9, "1.0% stop floor");
        assert!((aaa.pos.tp_px - aaa.pos.entry_px * 1.02).abs() < 1e-9, "tp is 2R");
        assert_eq!(aaa.status, "closed");
        assert_eq!(aaa.close_action.as_deref(), Some("tp"));
        assert_eq!(aaa.closed_ts, Some(T0 + 65 * M));

        let bbb = &out.positions[1];
        assert_eq!(bbb.pos.market, "BBB");
        assert_eq!(bbb.pos.side, Side::Short);
        assert_eq!(bbb.pos.opened_ts, T0 + 201 * M);
        assert!((bbb.pos.entry_px - 99.5 * 0.9998).abs() < 1e-9);
        assert_eq!(bbb.close_action.as_deref(), Some("sl"));
        assert_eq!(bbb.closed_ts, Some(T0 + 205 * M));

        // Trades: open, tp, open, sl — in that order, with the ledger's own columns.
        let actions: Vec<&str> = out.trades.iter().map(|t| t.action.as_str()).collect();
        assert_eq!(actions, vec!["open", "tp", "open", "sl"]);
        assert!((out.trades[0].fee - 0.600).abs() < 1e-9, "open fee is 7.5bp of 800");
        assert!((out.trades[1].realized_pnl - 16.0).abs() < 1e-9, "tp pays 2% of notional");
        assert!((out.trades[1].fee - 0.612).abs() < 1e-9);
        assert!((out.trades[2].fee - 0.600).abs() < 1e-9);
        assert!((out.trades[3].realized_pnl - -8.0).abs() < 1e-9, "sl pays -1% of notional");
        assert!((out.trades[3].fee - 0.606).abs() < 1e-9, "a short's stop sits 1% ABOVE the fill");
        assert!(out.trades.iter().all(|t| t.fill_mode == "flat"), "no historical books exist");

        let fees: f64 = out.trades.iter().map(|t| t.fee).sum();
        let gross: f64 = out.trades.iter().map(|t| t.realized_pnl).sum();
        assert!((fees - 2.418).abs() < 1e-9, "fees {fees}");
        assert!((gross - 8.0).abs() < 1e-9, "gross {gross}");

        let net = net_by_position(&out.trades);
        assert!((net[&1] - 14.788).abs() < 1e-9, "AAA net {}", net[&1]);
        assert!((net[&2] - -9.206).abs() < 1e-9, "BBB net {}", net[&2]);

        let end = out.equity.last().expect("equity curve").equity;
        assert!((end - 1005.582).abs() < 1e-9, "final equity {end}");
        assert_eq!(out.equity.len(), 400, "one equity point per replayed minute");
        assert!((out.equity[0].equity - 1000.0).abs() < 1e-12, "starts at the bankroll");
        assert!(out.kill_days.is_empty(), "a +0.56% day never trips a -12% kill switch");
    }

    /// The post-SL cooldown is a property of the LOOP, not just of `gate_churn`: BBB's second
    /// signal is 45 minutes after its stop-out — past the 30m base window, inside the 120m
    /// post-SL one — and the replay must skip it, then take it once the window is shorter.
    ///
    /// The refusal COUNTERS stay empty here, and that is the live behaviour being mirrored:
    /// market-scoped rails are applied by the screener's pre-filter
    /// (`main::EntryGate::excluded_markets`), so a cooled-down market never reaches the gate to
    /// be refused by it. The evidence is the trade that does not happen.
    #[test]
    fn post_sl_cooldown_is_honored_across_the_loop() {
        let out = golden_run();
        assert_eq!(out.positions.len(), 2, "the re-run at minute 250 never opens");
        assert!(out.positions.iter().all(|p| p.pos.opened_ts < T0 + 250 * M));

        // Same tape, a 30-minute post-SL window: 45 minutes is now clear and BBB re-enters on
        // the first minute of the second signal.
        let mut cfg = fixture_cfg();
        cfg.risk.cooldown_after_sl_min = 30;
        let out2 = run(&cfg, &golden_fixture(), golden_params());
        assert_eq!(out2.positions.len(), 3, "a shorter post-SL window lets the re-run through");
        assert_eq!(out2.positions[2].pos.market, "BBB");
        assert_eq!(out2.positions[2].pos.opened_ts, T0 + 251 * M);
        assert_eq!(out2.positions[2].status, "open", "it is still open when the tape ends");
    }

    /// The base cooldown alone (no stop-out involved) also has to hold across the loop: AAA
    /// took profit at minute 65, so a fresh signal at minute 80 is inside the 30m window and
    /// must be skipped — and taken when the window is 10m.
    #[test]
    fn a_take_profit_close_only_costs_the_base_cooldown() {
        let mut fixture = golden_fixture();
        // AAA moves again at minute 80 — 15 minutes after its TP.
        {
            let aaa = fixture.get_mut("AAA").expect("AAA");
            let mut candles = aaa.series.candles().to_vec();
            for c in candles.iter_mut().skip(80) {
                *c = flat(c.t, 103.1);
            }
            *aaa = MarketData::new("AAA", candles, vec![]);
        }
        let out = run(&fixture_cfg(), &fixture, golden_params());
        assert_eq!(
            out.positions.iter().filter(|p| p.pos.market == "AAA").count(),
            1,
            "a re-entry 15m after a TP is inside the base cooldown"
        );

        let mut cfg = fixture_cfg();
        cfg.risk.cooldown_min = 10;
        let out2 = run(&cfg, &fixture, golden_params());
        let aaa: Vec<&PositionRow> = out2.positions.iter().filter(|p| p.pos.market == "AAA").collect();
        assert_eq!(aaa.len(), 2, "a 10m window is spent by minute 80");
        assert_eq!(aaa[1].pos.opened_ts, T0 + 81 * M, "the first fresh minute after the window");
        // That second AAA position never closes (nothing touches its bracket), so from minute
        // 81 the filtered universe is two markets and the |z| arithmetic the fixture's
        // min_score is tuned for no longer holds — nothing after that minute is asserted here.
    }

    #[test]
    fn daily_cap_stops_entries_for_the_rest_of_the_day() {
        let mut cfg = fixture_cfg();
        cfg.risk.daily_cap = 1;
        let out = run(&cfg, &golden_fixture(), golden_params());
        assert_eq!(out.positions.len(), 1, "only the first signal of the day trades");
        assert_eq!(out.positions[0].pos.market, "AAA");
        assert!(
            out.refusals.get("DailyCap").copied().unwrap_or(0) > 0,
            "BBB's signal must land as a DailyCap refusal, refusals {:?}",
            out.refusals
        );
        // The cap is a COUNTER, not a latch: the same run with cap 2 takes both.
        cfg.risk.daily_cap = 2;
        assert_eq!(run(&cfg, &golden_fixture(), golden_params()).positions.len(), 2);
    }

    #[test]
    fn max_concurrent_and_per_market_cap_are_replayed() {
        // One position at a time: BBB's signal arrives long after AAA closed, so it still
        // trades — but a per-market cap of 0 refuses everything.
        let mut cfg = fixture_cfg();
        cfg.risk.per_market_daily_cap = 0;
        let out = run(&cfg, &golden_fixture(), golden_params());
        assert!(out.positions.is_empty(), "a zero per-market cap refuses every entry");
        assert!(out.refusals.get("PerMarketCap").copied().unwrap_or(0) > 0);

        // The morning budget is the other counter that binds inside a UTC morning.
        let mut cfg2 = fixture_cfg();
        cfg2.risk.morning_entry_budget = 1;
        let out2 = run(&cfg2, &golden_fixture(), golden_params());
        assert_eq!(out2.positions.len(), 1, "the fixture runs entirely before 12:00 UTC");
        assert!(out2.refusals.get("Paced").copied().unwrap_or(0) > 0);
    }

    /// The golden fixture plus a BTC tape that alternates ±1% every minute — `vol1h ≈ 1.0`,
    /// which the regime gate reads as a hot market.
    fn fixture_with_hot_btc() -> BTreeMap<String, MarketData> {
        let mut fixture = golden_fixture();
        let btc: Vec<CachedCandle> = (0..400)
            .map(|i| flat(T0 + i * M, if i % 2 == 0 { 100.0 } else { 101.0 }))
            .collect();
        fixture.insert("BTC".to_string(), MarketData::new("BTC", btc, vec![]));
        fixture
    }

    /// The regime gate reads BTC's vol from the same rows the screener scores, so a hot BTC
    /// halts entries in every other market too.
    #[test]
    fn regime_gate_halts_entries_when_btc_is_hot() {
        let fixture = fixture_with_hot_btc();
        let mut cfg = fixture_cfg();
        cfg.risk.regime_vol_max = 0.5;
        let out = run(&cfg, &fixture, golden_params());
        assert!(out.refusals.get("Regime").copied().unwrap_or(0) > 0, "refusals {:?}", out.refusals);
        assert!(
            out.positions.iter().all(|p| p.pos.market == "BTC"),
            "no non-BTC market may enter while the regime gate is refusing"
        );

        // The same tape with the production ceiling trades normally.
        let out2 = run(&fixture_cfg(), &fixture, golden_params());
        assert_eq!(out2.refusals.get("Regime").copied().unwrap_or(0), 0);
    }

    /// The chain runs in `main::EntryGate::check`'s order, so when several post-entry rails
    /// would fire the earlier segment speaks: regime (3) outranks churn post-checks (4).
    #[test]
    fn the_gate_chain_reports_the_earliest_segment_that_refuses() {
        let fixture = fixture_with_hot_btc();

        // Segment 3 vs 4: regime is hot and the per-market cap is spent -> Regime.
        let mut cfg = fixture_cfg();
        cfg.risk.regime_vol_max = 0.5;
        cfg.risk.per_market_daily_cap = 0;
        let out = run(&cfg, &fixture, golden_params());
        assert!(out.refusals.get("Regime").copied().unwrap_or(0) > 0, "refusals {:?}", out.refusals);
        assert_eq!(out.refusals.get("PerMarketCap"), None, "regime speaks before churn");
    }

    /// The kill switch is a per-day latch driven by the replay's own equity curve.
    ///
    /// The fixture ends the day UP, so the trip is engineered with a 0.05% budget: the moment
    /// AAA's position is open, equity is 1000 − 0.60 of fee − 0.16 of slip = 999.24, under the
    /// 999.50 floor. The latch then holds for the rest of the UTC day, so BBB's signal 140
    /// minutes later is refused — while AAA's own exit still runs (exits are never gated).
    #[test]
    fn kill_switch_latches_for_the_day_and_blocks_entries() {
        let mut cfg = fixture_cfg();
        cfg.risk.kill_switch_pct = 0.05;
        cfg.risk.kill_enabled = true;
        let out = run(&cfg, &golden_fixture(), golden_params());
        assert_eq!(out.kill_days, vec!["2026-08-01".to_string()], "kill days {:?}", out.kill_days);
        assert_eq!(out.positions.len(), 1, "nothing opens after the latch");
        assert_eq!(out.positions[0].close_action.as_deref(), Some("tp"), "exits are never gated");
        assert!(
            out.refusals.get("KillSwitch").copied().unwrap_or(0) > 0,
            "BBB's nomination must land as a KillSwitch refusal, refusals {:?}",
            out.refusals
        );
    }

    /// Entries fill at the next candle's open, so a signal on the last minute of a market's
    /// tape has nothing to fill against and is counted rather than invented.
    #[test]
    fn a_signal_on_the_last_minute_cannot_fill() {
        // The tape stops on the signal minute itself — what `load` produces when `--to` lands
        // there, since it never reads a candle past the window.
        let mut fixture = golden_fixture();
        for (market, d) in fixture.iter_mut() {
            let candles: Vec<CachedCandle> =
                d.series.candles().iter().copied().take_while(|c| c.t <= T0 + 60 * M).collect();
            *d = MarketData::new(market, candles, vec![]);
        }
        let out = run(&fixture_cfg(), &fixture, RunParams { to_ms: T0 + 60 * M, ..golden_params() });
        assert!(out.positions.is_empty());
        assert_eq!(out.unfilled, 1, "AAA's minute-60 signal had no minute-61 open to fill at");
    }

    /// Horizon expiry: with a 30-minute time stop, AAA's long is closed at the candle close
    /// 30 minutes after its fill instead of running to its target.
    #[test]
    fn horizon_expiry_closes_at_the_candle_close() {
        let mut fixture = golden_fixture();
        // Flatten AAA after its entry so neither bracket is ever touched.
        {
            let aaa = fixture.get_mut("AAA").expect("AAA");
            let mut candles = aaa.series.candles().to_vec();
            for c in candles.iter_mut().skip(61) {
                *c = flat(c.t, 100.5);
            }
            *aaa = MarketData::new("AAA", candles, vec![]);
        }
        let mut cfg = fixture_cfg();
        cfg.risk.time_stop_hours = 0.5;
        let out = run(&cfg, &fixture, golden_params());
        let aaa = out.positions.iter().find(|p| p.pos.market == "AAA").expect("AAA traded");
        assert_eq!(aaa.close_action.as_deref(), Some("time_stop"));
        assert_eq!(aaa.closed_ts, Some(T0 + 91 * M), "opened at 61, 30 minutes later is 91");
        let exit = out.trades.iter().find(|t| t.action == "time_stop").expect("time_stop trade");
        assert!((exit.px - 100.5).abs() < 1e-12, "expiry marks at the candle close");
    }

    /// A stop inside the ENTRY candle is honored — the position is live from its fill, and the
    /// candle it filled on is the first one the walker sees.
    #[test]
    fn the_entry_candle_can_stop_a_position_out() {
        let mut fixture = golden_fixture();
        {
            let aaa = fixture.get_mut("AAA").expect("AAA");
            let mut candles = aaa.series.candles().to_vec();
            // Minute 61 opens at 100.5 and collapses through the 1% stop within the minute.
            candles[61] = CachedCandle { t: T0 + 61 * M, o: 100.5, h: 100.5, l: 98.0, c: 99.0, v: 100.0 };
            for c in candles.iter_mut().skip(62) {
                *c = flat(c.t, 99.0);
            }
            *aaa = MarketData::new("AAA", candles, vec![]);
        }
        let out = run(&fixture_cfg(), &fixture, golden_params());
        let aaa = out.positions.iter().find(|p| p.pos.market == "AAA").expect("AAA traded");
        assert_eq!(aaa.close_action.as_deref(), Some("sl"));
        assert_eq!(aaa.closed_ts, Some(T0 + 61 * M), "stopped on its own entry candle");
    }

    #[test]
    fn markets_below_the_volume_floor_never_trade() {
        let mut cfg = fixture_cfg();
        cfg.universe.min_vlm_native = f64::MAX;
        let out = run(&cfg, &golden_fixture(), golden_params());
        assert!(out.positions.is_empty(), "the universe filter runs before scoring");
        assert!(out.refusals.is_empty(), "a market that never nominates never reaches a gate");
    }

    /// Two runs over the same inputs must be the same run — no clock, no RNG, no map ordering.
    #[test]
    fn two_runs_are_identical() {
        let a = golden_run();
        let b = golden_run();
        assert_eq!(a, b);
    }
}
