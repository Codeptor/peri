#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]

use std::collections::HashMap;

use thiserror::Error;

use crate::config::RiskCfg;

/// GATE CHAIN PLACEMENT (read before adding a refusal)
///
/// The entry decision runs five segments, in this order:
///
/// ```text
///   1. Risk::gate_entries_enabled (pre-check)  -> EntriesHalted
///   2. Risk::gate_data_age   (pre-check,  R2)  -> StaleData
///   3. Risk::gate_entry      (FROZEN chain)    -> KillSwitch, MaxConcurrent, DailyCap,
///                                                 DupMarket, Cooldown, LowConviction, Veto
///   4. Risk::gate_regime     (post-check, T1)  -> Regime
///   5. Risk::gate_churn      (post-check, T1)  -> PerMarketCap, Cooldown (post-SL), Paced
///   6. Risk::gate_min_rr     (post-check, B)   -> MinRR
/// ```
///
/// `gate_entry`'s body and its `GateState` argument are frozen: every gate-order test builds
/// a `GateState` literal, so adding a field or an argument would rewrite those tests and the
/// order they pin. New checks therefore go OUTSIDE it — the pre-check style R2 introduced
/// (`gate_data_age`), or the post-check style T1 uses here.
///
/// Pre vs post: staleness is about whether the decision may be trusted AT ALL, so it outranks
/// even the kill switch. Regime and churn are policy on top of a decision that already passed
/// every hard rail, so they run last — a killed / capped / duplicate entry keeps reporting its
/// more fundamental reason. Within segment 4 the order is cap -> post-SL cooldown -> pacing,
/// cheapest-and-most-specific first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateRefusal {
    /// Manual new-entry halt. Position management and closes never pass through this gate.
    EntriesHalted,
    KillSwitch,
    MaxConcurrent,
    /// Arena-wide paper book capacity (separate from each model's max_concurrent).
    GlobalCap,
    /// Arena-wide daily entry capacity (separate from each model's daily cap).
    GlobalDailyCap,
    DailyCap,
    DupMarket,
    Cooldown,
    LowConviction,
    Veto,
    /// Appended (never inserted) — see `Risk::gate_data_age` for why it lives outside
    /// the frozen in-gate order.
    StaleData,
    /// Appended (never inserted) — `Risk::gate_churn`, per-market entries-per-UTC-day cap.
    PerMarketCap,
    /// Appended (never inserted) — `Risk::gate_churn`, morning entry budget spent.
    Paced,
    /// Appended (never inserted) — `Risk::gate_regime`, BTC vol1h above `regime_vol_max`.
    Regime,
    /// Appended (never inserted) — `Risk::gate_min_rr`, an `open` stating both brackets
    /// with `tp_pct < min_rr * stop_pct` (Batch B: minimum 2:1 reward:risk).
    MinRR,
}

impl GateRefusal {
    /// Stable log/ledger token. Kept in one place so `decisions.reason` suffixes and
    /// tracing fields can never drift apart.
    pub fn as_str(&self) -> &'static str {
        match self {
            GateRefusal::EntriesHalted => "EntriesHalted",
            GateRefusal::KillSwitch => "KillSwitch",
            GateRefusal::MaxConcurrent => "MaxConcurrent",
            GateRefusal::GlobalCap => "GlobalCap",
            GateRefusal::GlobalDailyCap => "GlobalDailyCap",
            GateRefusal::DailyCap => "DailyCap",
            GateRefusal::DupMarket => "DupMarket",
            GateRefusal::Cooldown => "Cooldown",
            GateRefusal::LowConviction => "LowConviction",
            GateRefusal::Veto => "Veto",
            GateRefusal::StaleData => "StaleData",
            GateRefusal::PerMarketCap => "PerMarketCap",
            GateRefusal::Paced => "Paced",
            GateRefusal::Regime => "Regime",
            GateRefusal::MinRR => "MinRR",
        }
    }
}

/// Why a market's most recent position closed — the asymmetric-cooldown discriminator.
/// Only a stop-out extends the cooldown; TP / veto / time-stop / manual closes keep the
/// base `cooldown_min` window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCause {
    Sl,
    Other,
}

impl CloseCause {
    /// Stable token for logs and `/api/gates` cooldown rows.
    pub fn as_str(&self) -> &'static str {
        match self {
            CloseCause::Sl => "sl",
            CloseCause::Other => "other",
        }
    }

    /// Ledger `trades.action` -> cause. The trigger loop writes `sl` / `tp` / `time_stop`,
    /// the review loop writes `veto_close`; anything else is `Other`.
    pub fn from_action(action: &str) -> Self {
        if action.eq_ignore_ascii_case("sl") {
            CloseCause::Sl
        } else {
            CloseCause::Other
        }
    }
}

/// Pure staleness predicate. Data exactly `max_age_s` old is still FRESH (strict `>`),
/// matching the cooldown boundary convention (`exactly 30m` passes). A negative age
/// (snapshot stamped in the future — clock skew) is fresh, never a false refusal.
pub fn is_stale(age_ms: i64, max_age_s: u64) -> bool {
    age_ms > (max_age_s as i64).saturating_mul(1000)
}

/// Milliseconds in a UTC day / half-day. The unix epoch starts at 00:00:00 UTC, so
/// `rem_euclid` over these constants is an exact UTC clock with no calendar library and no
/// leap-second surprise (unix time has no leap seconds).
const DAY_MS: i64 = 86_400_000;
const HALF_DAY_MS: i64 = 43_200_000;

/// Pure: 00:00:00 UTC of the day containing `now_ms`. Keys the per-market entry cap.
/// `rem_euclid` keeps pre-1970 timestamps sane (test fixtures use small values).
pub fn utc_day_start_ms(now_ms: i64) -> i64 {
    now_ms - now_ms.rem_euclid(DAY_MS)
}

/// Pure: is `now_ms` before 12:00:00 UTC? The morning budget applies while this is true,
/// so 11:59:59.999 is still morning and 12:00:00.000 sharp is not.
pub fn is_before_utc_noon(now_ms: i64) -> bool {
    now_ms.rem_euclid(DAY_MS) < HALF_DAY_MS
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillState {
    Normal,
    Killed,
}

pub struct GateState {
    pub open_markets: Vec<String>,
    pub daily_count: usize,
    pub last_close_ts: HashMap<String, i64>,
    pub kill_active: bool,
    pub veto: bool,
    /// Current rolling UTC day key — drives 00:00 UTC day roll in main.rs.
    pub day_key: String,
}

impl GateState {
    pub fn new() -> Self {
        Self {
            open_markets: vec![],
            daily_count: 0,
            last_close_ts: HashMap::new(),
            kill_active: false,
            veto: false,
            day_key: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        }
    }
}

pub struct Risk {
    cfg: RiskCfg,
}

impl Risk {
    pub fn new(cfg: RiskCfg) -> Self {
        Self { cfg }
    }

    /// The knobs this gate enforces. Read-only, so `/api/gates` and the screener's exclusion
    /// feed report the SAME caps and windows the entry path applies — a config value can never
    /// be duplicated into a view and drift.
    pub fn cfg(&self) -> &RiskCfg {
        &self.cfg
    }

    /// Manual pre-check for NEW entries only. This intentionally outranks every other rail.
    pub fn gate_entries_enabled(&self) -> Result<(), GateRefusal> {
        if self.cfg.entries_enabled {
            Ok(())
        } else {
            Err(GateRefusal::EntriesHalted)
        }
    }

    /// Entry staleness pre-check (Phase R2). Refuses an entry whose decision rests on
    /// snapshot state older than `[risk] max_feature_age_s`.
    ///
    /// DELIBERATELY OUTSIDE `gate_entry`: the in-gate refusal order is frozen and pinned by
    /// tests that build `GateState` literals — threading a data age through either the
    /// struct or the signature would edit every one of those tests. Callers run this first,
    /// so `StaleData` outranks every in-gate refusal (including the kill switch) without
    /// touching the frozen order. Entry path only — reviews/exits are never gated.
    pub fn gate_data_age(&self, age_ms: i64) -> Result<(), GateRefusal> {
        if is_stale(age_ms, self.cfg.max_feature_age_s) {
            return Err(GateRefusal::StaleData);
        }
        Ok(())
    }

    /// Gate entry in frozen order: kill-switch -> max-concurrent -> daily-cap -> dup-market -> cooldown -> conviction -> veto
    pub fn gate_entry(&self, market: &str, conviction: f64, state: &GateState, now_ms: i64) -> Result<(), GateRefusal> {
        if self.cfg.kill_enabled && state.kill_active {
            return Err(GateRefusal::KillSwitch);
        }
        if self.cfg.max_concurrent > 0 && state.open_markets.len() >= self.cfg.max_concurrent {
            return Err(GateRefusal::MaxConcurrent);
        }
        if self.cfg.daily_cap > 0 && state.daily_count >= self.cfg.daily_cap {
            return Err(GateRefusal::DailyCap);
        }
        if state.open_markets.contains(&market.to_string()) {
            return Err(GateRefusal::DupMarket);
        }
        if let Some(last) = state.last_close_ts.get(market) {
            let cooldown_ms = self.cfg.cooldown_min as i64 * 60 * 1000;
            if now_ms - last < cooldown_ms {
                return Err(GateRefusal::Cooldown);
            }
        }
        if conviction < self.cfg.conviction_min {
            return Err(GateRefusal::LowConviction);
        }
        if state.veto {
            return Err(GateRefusal::Veto);
        }
        Ok(())
    }

    /// Regime post-check (Phase T1). BTC 1h realized vol is the market-wide risk dial: above
    /// `regime_vol_max` the whole book is noise and NEW entries are refused. Reviews and exits
    /// never run through this — being unable to open is fine, being unable to close is not.
    ///
    /// `None` (BTC row absent from the snapshot, or its features not yet computed) leaves the
    /// gate INACTIVE rather than blocking the daemon on missing data; the caller WARNs once per
    /// UTC day so a permanently missing BTC row is visible. Exactly `regime_vol_max` passes
    /// (strict `>`), same boundary convention as staleness and cooldown. A NaN vol also passes:
    /// every comparison with NaN is false, and a broken feature must not silently halt trading.
    pub fn gate_regime(&self, btc_vol1h: Option<f64>) -> Result<(), GateRefusal> {
        match btc_vol1h {
            Some(v) if v > self.cfg.regime_vol_max => Err(GateRefusal::Regime),
            _ => Ok(()),
        }
    }

    /// Churn post-checks (Phase T1), in order: per-market daily cap -> post-SL cooldown ->
    /// morning pacing. Runs AFTER `gate_entry` (see the chain note on `GateRefusal`).
    ///
    /// * `market_entries_today` — positions opened for THIS market since 00:00 UTC (entries
    ///   taken, not positions still open, so a churn loop cannot reset it by closing).
    /// * `entries_today` — entries across all markets today (`GateState::daily_count`); before
    ///   noon that IS the morning count, which is why the budget needs no separate counter.
    /// * `last_close` — `(ts_ms, cause)` of this market's most recent close, from the ledger.
    ///
    /// The post-SL window reuses `GateRefusal::Cooldown`: to an operator it is the same rule
    /// with a longer clock, and the dashboard's refusal buckets stay stable. Boundaries are
    /// `>=` for the caps and `<` for the cooldown, matching `gate_entry` exactly (a third
    /// entry passes when the cap is 3; a close exactly `cooldown_after_sl_min` ago passes).
    pub fn gate_churn(
        &self,
        market_entries_today: usize,
        entries_today: usize,
        last_close: Option<(i64, CloseCause)>,
        now_ms: i64,
    ) -> Result<(), GateRefusal> {
        if market_entries_today >= self.cfg.per_market_daily_cap {
            return Err(GateRefusal::PerMarketCap);
        }
        if let Some((ts, CloseCause::Sl)) = last_close {
            let window_ms = (self.cfg.cooldown_after_sl_min as i64).saturating_mul(60_000);
            if now_ms - ts < window_ms {
                return Err(GateRefusal::Cooldown);
            }
        }
        if is_before_utc_noon(now_ms) && entries_today >= self.cfg.morning_entry_budget {
            return Err(GateRefusal::Paced);
        }
        Ok(())
    }

    /// RR floor post-check (Batch B, 2026-08-24 — Alpha Arena lesson: minimum 2:1
    /// reward:risk). Applies ONLY to `open` decisions that state BOTH brackets:
    /// `tp_pct >= min_rr * stop_pct`, boundary inclusive (exactly 2x passes).
    ///
    /// A decision omitting either bracket is NOT refused — it keeps the sizer's default
    /// bracket (`sizing::size_position`, `tp_mult` default 2.0), which already satisfies
    /// the floor. Refusing it would just rename the default bracket as a refusal.
    pub fn gate_min_rr(&self, action: &str, stop_pct: Option<f64>, tp_pct: Option<f64>) -> Result<(), GateRefusal> {
        if action != "open" {
            return Ok(());
        }
        if let (Some(stop), Some(tp)) = (stop_pct, tp_pct)
            && tp < self.cfg.min_rr * stop
        {
            return Err(GateRefusal::MinRR);
        }
        Ok(())
    }

    pub fn on_equity(&self, equity: f64, day_open: f64) -> KillState {
        if !self.cfg.kill_enabled {
            return KillState::Normal;
        }
        if day_open <= 0.0 {
            return KillState::Normal;
        }
        let threshold = day_open * (1.0 - self.cfg.kill_switch_pct / 100.0);
        if equity <= threshold {
            KillState::Killed
        } else {
            KillState::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskCfg;
    use std::collections::HashMap;

    fn cfg() -> RiskCfg {
        RiskCfg {
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

    /// Fixed UTC instants on 2026-08-09 (the day the churn knobs were locked), so pacing
    /// never fires by accident in tests that are about something else.
    const DAY_START_MS: i64 = 1_786_233_600_000; // 00:00:00.000 UTC
    const MORNING_LAST_MS: i64 = 1_786_276_799_999; // 11:59:59.999 UTC — still morning
    const NOON_MS: i64 = 1_786_276_800_000; // 12:00:00.000 UTC — budget released
    const AFTERNOON_MS: i64 = 1_786_284_000_000; // 14:00:00.000 UTC


    fn state_with(open: usize, daily: usize, dup: bool, cooldown: bool, kill: bool, veto: bool) -> (GateState, i64) {
        let now = 1_700_000_000_000i64;
        let mut s = GateState {
            open_markets: (0..open).map(|i| format!("M{i}")).collect(),
            daily_count: daily,
            last_close_ts: HashMap::new(),
            kill_active: kill,
            veto,
            day_key: String::new(),
        };
        if dup {
            s.open_markets.push("TEST".to_string());
        }
        if cooldown {
            s.last_close_ts.insert("TEST".to_string(), now - 10 * 60 * 1000); // 10m ago, still in 30m cooldown
        }
        (s, now)
    }

    #[test]
    fn kill_switch_blocks() {
        let r = Risk::new(RiskCfg { kill_enabled: true, ..cfg() });
        let (s, now) = state_with(0, 0, false, false, true, false);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::KillSwitch));
        let (s2, now) = state_with(0, 0, false, false, false, false);
        assert!(r.gate_entry("TEST", 0.9, &s2, now).is_ok());
    }

    #[test]
    fn manual_halt_refuses_before_every_other_entry_rail() {
        let r = Risk::new(RiskCfg { entries_enabled: false, kill_enabled: true, ..cfg() });
        let (state, now) = state_with(5, 20, true, true, true, true);
        assert_eq!(r.gate_entries_enabled(), Err(GateRefusal::EntriesHalted));
        assert_eq!(r.gate_entry("TEST", 0.0, &state, now), Err(GateRefusal::KillSwitch));
    }

    #[test]
    fn disabled_kill_switch_never_latches_or_blocks() {
        let r = Risk::new(cfg());
        let (s, now) = state_with(0, 0, false, false, true, false);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Ok(()));
        assert_eq!(r.on_equity(800.0, 1_000.0), KillState::Normal);
    }

    #[test]
    fn max_concurrent_boundary() {
        let r = Risk::new(cfg());
        let (s, now) = state_with(5, 0, false, false, false, false);
        assert_eq!(r.gate_entry("NEW", 0.9, &s, now), Err(GateRefusal::MaxConcurrent));
        let (s2, now) = state_with(4, 0, false, false, false, false);
        assert!(r.gate_entry("NEW", 0.9, &s2, now).is_ok());
    }

    #[test]
    fn zero_max_concurrent_allows_entries_past_the_former_boundary_but_keeps_other_rails() {
        let r = Risk::new(RiskCfg { max_concurrent: 0, ..cfg() });
        let (open, now) = state_with(1_000, 0, false, false, false, false);
        assert!(r.gate_entry("NEW", 0.9, &open, now).is_ok());

        let (duplicate, now) = state_with(1_000, 0, true, false, false, false);
        assert_eq!(r.gate_entry("TEST", 0.9, &duplicate, now), Err(GateRefusal::DupMarket));

        let (cooling_down, now) = state_with(1_000, 0, false, true, false, false);
        assert_eq!(r.gate_entry("TEST", 0.9, &cooling_down, now), Err(GateRefusal::Cooldown));
    }

    #[test]
    fn daily_cap_boundary() {
        let r = Risk::new(cfg());
        let (s, now) = state_with(0, 20, false, false, false, false);
        assert_eq!(r.gate_entry("NEW", 0.9, &s, now), Err(GateRefusal::DailyCap));
        let (s2, now) = state_with(0, 19, false, false, false, false);
        assert!(r.gate_entry("NEW", 0.9, &s2, now).is_ok());
    }

    #[test]
    fn zero_daily_cap_disables_the_per_analyst_gate() {
        let r = Risk::new(RiskCfg { daily_cap: 0, ..cfg() });
        let (s, now) = state_with(0, 1_000, false, false, false, false);
        assert!(r.gate_entry("NEW", 0.9, &s, now).is_ok());
    }

    #[test]
    fn dup_market() {
        let r = Risk::new(cfg());
        let (s, now) = state_with(1, 0, true, false, false, false);
        // open_markets contains TEST, dup check should fail
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::DupMarket));
        assert!(r.gate_entry("OTHER", 0.9, &s, now).is_ok());
    }

    #[test]
    fn cooldown_boundary() {
        let r = Risk::new(cfg());
        let now = 1_700_000_000_000i64;
        let mut s = GateState {
            open_markets: vec![],
            daily_count: 0,
            last_close_ts: HashMap::new(),
            kill_active: false,
            veto: false,
            day_key: String::new(),
        };
        // exactly 30m ago => not in cooldown (need < cooldown)
        s.last_close_ts.insert("TEST".to_string(), now - 30 * 60 * 1000);
        assert!(r.gate_entry("TEST", 0.9, &s, now).is_ok(), "exactly 30m should pass");
        // 29m59s ago => still cooldown
        s.last_close_ts.insert("TEST".to_string(), now - 29 * 60 * 1000);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::Cooldown));
        // 10m ago
        s.last_close_ts.insert("TEST".to_string(), now - 10 * 60 * 1000);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::Cooldown));
        // no entry => no cooldown
        let mut s2 = GateState::new();
        assert!(r.gate_entry("TEST", 0.9, &s2, now).is_ok());
    }

    #[test]
    fn conviction_boundary() {
        let r = Risk::new(cfg());
        let (s, now) = state_with(0, 0, false, false, false, false);
        assert_eq!(r.gate_entry("TEST", 0.49, &s, now), Err(GateRefusal::LowConviction));
        assert!(r.gate_entry("TEST", 0.50, &s, now).is_ok());
        assert!(r.gate_entry("TEST", 0.51, &s, now).is_ok());
    }

    #[test]
    fn veto_blocks() {
        let r = Risk::new(cfg());
        let (mut s, now) = state_with(0, 0, false, false, false, true);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::Veto));
        s.veto = false;
        assert!(r.gate_entry("TEST", 0.9, &s, now).is_ok());
    }

    #[test]
    fn kill_equity_exactly_12() {
        let r = Risk::new(RiskCfg { kill_enabled: true, ..cfg() });
        let day_open = 1000.0;
        // exactly -12% => 880.0 should halt
        assert_eq!(r.on_equity(880.0, day_open), KillState::Killed);
        // -11.9% => 881.0 should not
        assert_eq!(r.on_equity(881.0, day_open), KillState::Normal);
        assert_eq!(r.on_equity(879.0, day_open), KillState::Killed);
        assert_eq!(r.on_equity(900.0, day_open), KillState::Normal);
    }

    #[test]
    fn staleness_boundary_fresh_exact_stale() {
        let r = Risk::new(cfg()); // max_feature_age_s = 120
        // fresh
        assert!(r.gate_data_age(0).is_ok(), "age 0 is fresh");
        assert!(r.gate_data_age(119_999).is_ok(), "1ms under the window is fresh");
        // exact boundary: 120s old is still fresh (strict >, same convention as cooldown)
        assert!(r.gate_data_age(120_000).is_ok(), "exactly max_feature_age_s must pass");
        // stale
        assert_eq!(r.gate_data_age(120_001), Err(GateRefusal::StaleData), "1ms over the window refuses");
        assert_eq!(r.gate_data_age(600_000), Err(GateRefusal::StaleData), "10m-old state refuses");
        // clock skew: snapshot stamped in the future must not refuse
        assert!(r.gate_data_age(-5_000).is_ok(), "negative age (future stamp) is fresh");
    }

    #[test]
    fn is_stale_pure_predicate() {
        assert!(!is_stale(0, 120));
        assert!(!is_stale(120_000, 120));
        assert!(is_stale(120_001, 120));
        // a zero window makes every non-zero age stale, but not age 0
        assert!(!is_stale(0, 0));
        assert!(is_stale(1, 0));
    }

    #[test]
    fn staleness_is_independent_of_the_frozen_in_gate_order() {
        // StaleData is a pre-check: it must NOT appear from gate_entry, whose order stays
        // kill -> max-concurrent -> daily-cap -> dup -> cooldown -> conviction -> veto.
        let r = Risk::new(RiskCfg { kill_enabled: true, ..cfg() });
        let (s, now) = state_with(0, 0, false, false, true, false);
        assert_eq!(r.gate_entry("TEST", 0.9, &s, now), Err(GateRefusal::KillSwitch), "gate_entry never yields StaleData");
        // and the pre-check outranks the kill switch when callers run it first
        assert_eq!(r.gate_data_age(999_999), Err(GateRefusal::StaleData));
    }

    #[test]
    fn refusal_tokens_are_stable() {
        assert_eq!(GateRefusal::EntriesHalted.as_str(), "EntriesHalted");
        assert_eq!(GateRefusal::KillSwitch.as_str(), "KillSwitch");
        assert_eq!(GateRefusal::DailyCap.as_str(), "DailyCap");
        assert_eq!(GateRefusal::GlobalDailyCap.as_str(), "GlobalDailyCap");
        assert_eq!(GateRefusal::StaleData.as_str(), "StaleData");
        assert_eq!(GateRefusal::PerMarketCap.as_str(), "PerMarketCap");
        assert_eq!(GateRefusal::Paced.as_str(), "Paced");
        assert_eq!(GateRefusal::Regime.as_str(), "Regime");
        assert_eq!(GateRefusal::MinRR.as_str(), "MinRR");
        // ledger suffix must never collide with the dash's `refused:true` refusal parser
        for r in [GateRefusal::EntriesHalted, GateRefusal::StaleData, GateRefusal::PerMarketCap, GateRefusal::Paced, GateRefusal::Regime, GateRefusal::MinRR] {
            let suffix = format!(" gate_refused:{}", r.as_str());
            assert!(!suffix.contains("refused:true"), "must not read as an analyst refusal: {suffix}");
        }
    }

    // ── T1 churn control ──────────────────────────────────────────────────────────────

    #[test]
    fn per_market_cap_allows_two_and_three_blocks_the_fourth() {
        let r = Risk::new(cfg()); // per_market_daily_cap = 3
        // 2 entries taken today on this market -> the 3rd is allowed
        assert!(r.gate_churn(2, 0, None, AFTERNOON_MS).is_ok(), "3rd entry of the day passes");
        // 3 taken -> the 4th is refused
        assert_eq!(
            r.gate_churn(3, 0, None, AFTERNOON_MS),
            Err(GateRefusal::PerMarketCap),
            "4th entry on the same market refused"
        );
        // and it stays refused past the cap
        assert_eq!(r.gate_churn(9, 0, None, AFTERNOON_MS), Err(GateRefusal::PerMarketCap));
        // a fresh market is unaffected
        assert!(r.gate_churn(0, 0, None, AFTERNOON_MS).is_ok(), "first entry of the day passes");
    }

    #[test]
    fn cooldown_split_sl_waits_120m_other_causes_dont() {
        let r = Risk::new(cfg()); // cooldown_after_sl_min = 120
        let now = AFTERNOON_MS;
        let m = |mins: i64| now - mins * 60_000;

        // SL close: blocked right through the 30m base window and up to 120m
        assert_eq!(r.gate_churn(0, 0, Some((m(1), CloseCause::Sl)), now), Err(GateRefusal::Cooldown));
        assert_eq!(r.gate_churn(0, 0, Some((m(31), CloseCause::Sl)), now), Err(GateRefusal::Cooldown), "past base cooldown, still inside the SL window");
        assert_eq!(r.gate_churn(0, 0, Some((m(119), CloseCause::Sl)), now), Err(GateRefusal::Cooldown));
        // exactly 120m passes (strict `<`, same convention as gate_entry's cooldown)
        assert!(r.gate_churn(0, 0, Some((m(120), CloseCause::Sl)), now).is_ok(), "exactly 120m after an SL passes");
        assert!(r.gate_churn(0, 0, Some((m(121), CloseCause::Sl)), now).is_ok());

        // TP / veto / time-stop keep the base 30m window, which `gate_entry` owns — the
        // churn post-check must not add anything on top of them.
        let other = CloseCause::Other;
        assert!(r.gate_churn(0, 0, Some((m(1), other)), now).is_ok(), "a non-SL close is not extended at all");
        assert!(r.gate_churn(0, 0, Some((m(31), other)), now).is_ok());
        // never closed -> nothing to wait for
        assert!(r.gate_churn(0, 0, None, now).is_ok());
    }

    #[test]
    fn close_cause_classifies_ledger_actions() {
        assert_eq!(CloseCause::from_action("sl"), CloseCause::Sl);
        assert_eq!(CloseCause::from_action("SL"), CloseCause::Sl);
        assert_eq!(CloseCause::from_action("tp"), CloseCause::Other);
        assert_eq!(CloseCause::from_action("veto_close"), CloseCause::Other);
        assert_eq!(CloseCause::from_action("time_stop"), CloseCause::Other);
        assert_eq!(CloseCause::from_action("close"), CloseCause::Other);
        // tokens the gates endpoint reports
        assert_eq!(CloseCause::Sl.as_str(), "sl");
        assert_eq!(CloseCause::Other.as_str(), "other");
    }

    #[test]
    fn morning_budget_binds_before_noon_and_releases_at_1200_utc() {
        let r = Risk::new(cfg()); // morning_entry_budget = 12
        // 11 entries in -> the 12th is allowed
        assert!(r.gate_churn(0, 11, None, MORNING_LAST_MS).is_ok(), "12th morning entry passes");
        // 12 in -> paced out for the rest of the morning
        assert_eq!(r.gate_churn(0, 12, None, MORNING_LAST_MS), Err(GateRefusal::Paced), "11:59:59.999 is still morning");
        assert_eq!(r.gate_churn(0, 12, None, DAY_START_MS), Err(GateRefusal::Paced), "00:00 UTC is morning");
        // 12:00:00.000 sharp releases the budget (the daily cap still applies via gate_entry)
        assert!(r.gate_churn(0, 12, None, NOON_MS).is_ok(), "12:00 UTC releases the morning budget");
        assert!(r.gate_churn(0, 19, None, AFTERNOON_MS).is_ok(), "afternoon is only bounded by daily_cap");
    }

    #[test]
    fn churn_order_cap_then_cooldown_then_pacing() {
        let r = Risk::new(cfg());
        let stopped_out = Some((MORNING_LAST_MS - 60_000, CloseCause::Sl));
        // all three would fire -> the most specific (per-market cap) wins
        assert_eq!(r.gate_churn(3, 12, stopped_out, MORNING_LAST_MS), Err(GateRefusal::PerMarketCap));
        // cap clear -> the SL cooldown speaks before pacing
        assert_eq!(r.gate_churn(0, 12, stopped_out, MORNING_LAST_MS), Err(GateRefusal::Cooldown));
        // cap + cooldown clear -> pacing
        assert_eq!(r.gate_churn(0, 12, None, MORNING_LAST_MS), Err(GateRefusal::Paced));
        // nothing set -> pass
        assert!(r.gate_churn(0, 0, None, MORNING_LAST_MS).is_ok());
    }

    #[test]
    fn utc_clock_helpers_are_exact() {
        assert_eq!(utc_day_start_ms(DAY_START_MS), DAY_START_MS, "midnight is its own day start");
        assert_eq!(utc_day_start_ms(AFTERNOON_MS), DAY_START_MS);
        assert_eq!(utc_day_start_ms(DAY_START_MS + 86_399_999), DAY_START_MS, "last ms of the day");
        assert_eq!(utc_day_start_ms(DAY_START_MS + 86_400_000), DAY_START_MS + 86_400_000, "next day rolls");
        assert!(is_before_utc_noon(DAY_START_MS));
        assert!(is_before_utc_noon(MORNING_LAST_MS));
        assert!(!is_before_utc_noon(NOON_MS));
        assert!(!is_before_utc_noon(AFTERNOON_MS));
        // sanity against chrono, so a rem_euclid mistake cannot hide
        use chrono::{TimeZone, Timelike};
        for ms in [DAY_START_MS, MORNING_LAST_MS, NOON_MS, AFTERNOON_MS] {
            let dt = chrono::Utc.timestamp_millis_opt(ms).unwrap();
            assert_eq!(is_before_utc_noon(ms), dt.hour() < 12, "noon check disagrees with chrono at {dt}");
            assert_eq!(
                utc_day_start_ms(ms),
                chrono::Utc.timestamp_millis_opt(ms).unwrap().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis(),
                "day start disagrees with chrono at {dt}"
            );
        }
    }

    // ── T1 regime gate ────────────────────────────────────────────────────────────────

    #[test]
    fn regime_gate_on_off_and_absent_btc() {
        let r = Risk::new(cfg()); // regime_vol_max = 1.5
        // calm: entries allowed
        assert!(r.gate_regime(Some(0.4)).is_ok(), "normal vol trades");
        // boundary: exactly the max still trades (strict `>`)
        assert!(r.gate_regime(Some(1.5)).is_ok(), "exactly regime_vol_max must pass");
        // hot: refused
        assert_eq!(r.gate_regime(Some(1.5001)), Err(GateRefusal::Regime), "a hair over the max refuses");
        assert_eq!(r.gate_regime(Some(4.0)), Err(GateRefusal::Regime));
        // absent BTC row / features -> gate inactive (caller WARNs once per day)
        assert!(r.gate_regime(None).is_ok(), "missing BTC must not halt trading");
        // a broken feature must not silently halt trading either
        assert!(r.gate_regime(Some(f64::NAN)).is_ok(), "NaN vol leaves the gate inactive");
    }

    #[test]
    fn t1_checks_are_independent_of_the_frozen_in_gate_order() {
        // Same contract as the R2 staleness pre-check: the frozen chain must never emit a
        // T1 refusal, and the T1 checks must never emit a frozen one.
        let r = Risk::new(RiskCfg { kill_enabled: true, ..cfg() });
        let (s, now) = state_with(0, 0, false, false, true, false);
        let frozen = r.gate_entry("TEST", 0.9, &s, now);
        assert_eq!(frozen, Err(GateRefusal::KillSwitch), "gate_entry still refuses kill first");
        for refusal in [GateRefusal::PerMarketCap, GateRefusal::Paced, GateRefusal::Regime, GateRefusal::StaleData, GateRefusal::MinRR] {
            assert_ne!(frozen, Err(refusal.clone()), "gate_entry never yields {}", refusal.as_str());
        }
        // and the post-checks only ever speak their own language
        assert_eq!(r.gate_regime(Some(9.0)), Err(GateRefusal::Regime));
        assert_eq!(r.gate_churn(5, 99, None, MORNING_LAST_MS), Err(GateRefusal::PerMarketCap));
    }

    // ── Batch B RR floor ────────────────────────────────────────────────────────────

    #[test]
    fn min_rr_refuses_sub_floor_and_passes_at_boundary() {
        let r = Risk::new(cfg()); // min_rr = 2.0
        // tp below 2x stop -> refused
        assert_eq!(
            r.gate_min_rr("open", Some(1.5), Some(2.9)),
            Err(GateRefusal::MinRR),
            "tp 2.9 on a 1.5 stop is under 2:1"
        );
        assert_eq!(
            r.gate_min_rr("open", Some(1.0), Some(1.0)),
            Err(GateRefusal::MinRR),
            "1:1 is refused"
        );
        // exactly 2x passes (>= boundary, same convention as the other gates)
        assert!(r.gate_min_rr("open", Some(1.5), Some(3.0)).is_ok(), "exactly 2:1 passes");
        assert!(r.gate_min_rr("open", Some(1.0), Some(3.5)).is_ok(), "better than 2:1 passes");
    }

    #[test]
    fn min_rr_ignores_incomplete_and_non_open_decisions() {
        let r = Risk::new(cfg());
        // missing either bracket -> default-bracket behavior, not this gate's business
        assert!(r.gate_min_rr("open", None, None).is_ok(), "no brackets passes");
        assert!(r.gate_min_rr("open", Some(4.0), None).is_ok(), "stop only passes");
        assert!(r.gate_min_rr("open", None, Some(0.4)).is_ok(), "tp only passes");
        // non-open actions never hit the gate even with a bad ratio
        assert!(r.gate_min_rr("skip", Some(1.0), Some(0.1)).is_ok());
        assert!(r.gate_min_rr("veto_close", Some(1.0), Some(0.1)).is_ok());
    }

    #[test]
    fn min_rr_uses_configured_floor() {
        let r = Risk::new(RiskCfg { min_rr: 3.0, ..cfg() });
        assert_eq!(r.gate_min_rr("open", Some(1.0), Some(2.9)), Err(GateRefusal::MinRR));
        assert!(r.gate_min_rr("open", Some(1.0), Some(3.0)).is_ok(), "exactly the configured floor passes");
    }

    #[test]
    fn gate_order_kill_first() {
        let r = Risk::new(RiskCfg { kill_enabled: true, ..cfg() });
        // create state where multiple gates would fail, ensure kill takes precedence
        let now = 1_700_000_000_000i64;
        let mut s = GateState {
            open_markets: vec!["M0".into(), "M1".into(), "M2".into(), "M3".into(), "M4".into()],
            daily_count: 20,
            last_close_ts: HashMap::new(),
            kill_active: true,
            veto: true,
            day_key: String::new(),
        };
        s.last_close_ts.insert("TEST".into(), now - 1000);
        // should return KillSwitch not MaxConcurrent
        assert_eq!(r.gate_entry("TEST", 0.1, &s, now), Err(GateRefusal::KillSwitch));
    }
}
