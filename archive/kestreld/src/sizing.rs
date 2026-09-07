#![allow(dead_code)]

use crate::config::SizingCfg;

/// Upper bound of the `stop_pct` clamp (user-locked amendment 2026-08-09: 2.5 -> 3.0).
pub const STOP_CEILING_PCT: f64 = 3.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Sized {
    pub leverage: f64,
    pub margin: f64,
    pub notional: f64,
    pub stop_pct: f64,
    pub tp_pct: f64,
}

/// Pure sizing per spec §6 (amended).
/// ```
/// vol1h_pct   = stddev(1m returns over 60m) * 100
/// stop_pct    = clamp(1.5 * vol1h_pct, 1.0, 3.0)
/// tp_pct      = tp_mult * stop_pct
/// vol_ref     = 0.4
/// lev_raw     = 5.0 + 15.0 * conviction * clamp(vol_ref / vol1h_pct, 0.4, 1.6)
/// leverage    = clamp(round(lev_raw), 5, 20)
/// margin_usd  = clamp(bankroll * (0.01 + 0.04*conviction), 10, 50)
/// ```
/// User-locked amendment 2026-08-09 (loss-reduction directive; spec §6 superseded):
/// day-1+2 tape shows 8 SL hits (-$110, avg -$13.8) mostly at stop distances 1.2-1.9%
/// in 3-5% noise assets — stops inside the noise band. Widened floor 0.6%→1.0% and
/// ceiling 2.5%→3.0% so stops sit outside noise. tp remains 2R (`tp_mult` defaults to 2.0,
/// unchanged) — see `[sizing] tp_mult` below.
///
/// The floor is `[sizing] stop_floor_pct` (default 1.0 — the amendment's value), so the backtest
/// harness can sweep it without forking this formula. The ceiling stays the constant
/// [`STOP_CEILING_PCT`]: nothing has asked to move it, and a sweep of a knob nobody tunes is
/// noise. A floor swept ABOVE the ceiling raises the ceiling with it rather than panicking in
/// `f64::clamp`.
///
/// `tp_mult` is `[sizing] tp_mult` (default 2.0 — the same 2R the formula always used), the same
/// treatment: a literal until the backtest harness needed to sweep it (sweep key `tp_mult`), now
/// a knob whose default reproduces the old behaviour exactly.
pub fn size_position(cfg: &SizingCfg, vol1h_pct: f64, conviction: f64) -> Sized {
    // Guard against zero/negative vol which would cause div-by-zero; clamp behaviour
    // would push vol_ref/vol -> huge -> clamped to 1.6. Use a tiny epsilon.
    let vol = if vol1h_pct <= 1e-9 { 1e-9 } else { vol1h_pct };

    let floor = cfg.stop_floor_pct;
    let stop_pct = (1.5 * vol).clamp(floor, STOP_CEILING_PCT.max(floor));
    let tp_pct = cfg.tp_mult * stop_pct;

    let vol_ratio = (cfg.vol_ref / vol).clamp(0.4, 1.6);
    let lev_raw = 5.0 + 15.0 * conviction * vol_ratio;
    let leverage = (lev_raw.round() as i64).clamp(5, 20) as f64;

    let margin_raw = cfg.bankroll * (0.01 + 0.04 * conviction);
    let margin = margin_raw.clamp(cfg.margin_min, cfg.margin_max);

    let notional = margin * leverage;

    Sized {
        leverage,
        margin,
        notional,
        stop_pct,
        tp_pct,
    }
}

/// Clamp a requested leverage to 1.0–10.0, then per-market cap: `xyz:` max 5x, otherwise 10x.
/// Pure, never panics, handles non-finite as 1.0.
pub fn clamp_leverage(market: &str, leverage: f64) -> f64 {
    let mut lev = if leverage.is_finite() { leverage } else { 1.0 };
    lev = lev.clamp(1.0, 10.0);
    let cap = if market.starts_with("xyz:") { 5.0 } else { 10.0 };
    lev.min(cap)
}

/// Resolve effective leverage from an optional per-trade request.
/// `None` → default 3.0 (backwards compat, clamped per-market but 3.0 is under both caps).
/// `Some(v)` → `clamp_leverage(market, v)`.
pub fn resolve_leverage(market: &str, requested: Option<f64>) -> f64 {
    match requested {
        Some(v) => clamp_leverage(market, v),
        None => 3.0,
    }
}

/// Resolve leverage when a fallback (e.g. sizing engine's `Sized.leverage`) should be kept if
/// no per-trade request is present, but still capped per-market. Used by the live entry path
/// when it wants to preserve the vol/conviction sizing as the default instead of the 3.0
/// constant (both paths clamp to the same per-market caps).
pub fn effective_leverage(market: &str, requested: Option<f64>, fallback: f64) -> f64 {
    match requested {
        Some(v) => clamp_leverage(market, v),
        None => clamp_leverage(market, fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SizingCfg;

    fn cfg() -> SizingCfg {
        SizingCfg {
            bankroll: 1000.0,
            vol_ref: 0.4,
            margin_min: 10.0,
            margin_max: 50.0,
            stop_floor_pct: 1.0,
            tp_mult: 2.0,
        }
    }

    #[test]
    fn pinned_case_1_vol04_conv1() {
        let s = size_position(&cfg(), 0.4, 1.0);
        assert!((s.leverage - 20.0).abs() < 1e-9, "lev {}", s.leverage);
        assert!((s.margin - 50.0).abs() < 1e-9, "margin {}", s.margin);
        assert!((s.stop_pct - 1.0).abs() < 1e-9, "stop {}", s.stop_pct);
        assert!((s.tp_pct - 2.0).abs() < 1e-9, "tp {}", s.tp_pct);
        assert!((s.notional - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn pinned_case_2_vol08_conv05() {
        let s = size_position(&cfg(), 0.8, 0.5);
        // lev_raw = 5 + 15*0.5*0.5 = 8.75 -> 9
        assert!((s.leverage - 9.0).abs() < 1e-9, "lev {}", s.leverage);
        assert!((s.margin - 30.0).abs() < 1e-9, "margin {}", s.margin);
        assert!((s.stop_pct - 1.2).abs() < 1e-9);
        assert!((s.tp_pct - 2.4).abs() < 1e-9);
        assert!((s.notional - 270.0).abs() < 1e-9);
    }

    #[test]
    fn pinned_case_3_vol01_conv02() {
        let s = size_position(&cfg(), 0.1, 0.2);
        // clamp(4.0)=1.6 -> lev_raw 9.8 -> 10
        assert!((s.leverage - 10.0).abs() < 1e-9, "lev {}", s.leverage);
        assert!((s.stop_pct - 1.0).abs() < 1e-9, "stop clamped {}", s.stop_pct);
        assert!((s.tp_pct - 2.0).abs() < 1e-9);
        // margin = 1000*(0.01+0.008)=18
        assert!((s.margin - 18.0).abs() < 1e-9, "margin {}", s.margin);
    }

    #[test]
    fn margin_clamps() {
        let c = cfg();
        let s_low = size_position(&c, 0.4, 0.0);
        assert!((s_low.margin - 10.0).abs() < 1e-9);
        let s_high = size_position(&c, 0.4, 1.0);
        assert!((s_high.margin - 50.0).abs() < 1e-9);
    }

    #[test]
    fn leverage_clamps() {
        let c = cfg();
        // very high conviction with low vol ratio 0.4 -> lev_raw 5+15*1*0.4=11 -> not extreme
        // very low vol 0.01 -> ratio 1.6 -> lev 5+15*1*1.6=29 -> clamp 20
        let s = size_position(&c, 0.01, 1.0);
        assert_eq!(s.leverage, 20.0);
        let s2 = size_position(&c, 10.0, 0.0);
        assert_eq!(s2.leverage, 5.0);
    }

    /// The floor is a knob now (backtest sweep key `stop_floor`), and it is the ONLY thing
    /// that moves: tp stays 2R off whatever stop the clamp produced, and leverage/margin —
    /// which read vol and conviction, not the stop — are untouched.
    #[test]
    fn stop_floor_is_configurable_and_defaults_to_the_locked_10() {
        let mut c = cfg();
        // vol 0.1 -> 1.5*0.1 = 0.15, under every floor swept, so the floor IS the stop.
        for (floor, expect) in [(0.6, 0.6), (1.0, 1.0), (1.4, 1.4)] {
            c.stop_floor_pct = floor;
            let s = size_position(&c, 0.1, 0.75);
            assert!((s.stop_pct - expect).abs() < 1e-9, "floor {floor} -> stop {}", s.stop_pct);
            assert!((s.tp_pct - 2.0 * expect).abs() < 1e-9, "tp stays 2R");
            assert!((s.margin - 40.0).abs() < 1e-9, "the floor never moves margin");
            assert!((s.leverage - 20.0).abs() < 1e-9, "nor leverage");
        }
        // A stop the vol earns outright ignores the floor entirely.
        c.stop_floor_pct = 0.6;
        assert!((size_position(&c, 1.2, 0.75).stop_pct - 1.8).abs() < 1e-9, "1.5*1.2 clears 0.6");
        // A floor above the ceiling raises the ceiling with it — clamp(min > max) would panic.
        c.stop_floor_pct = 4.0;
        let wide = size_position(&c, 5.0, 0.75);
        assert!((wide.stop_pct - 4.0).abs() < 1e-9, "floor wins over the 3.0 ceiling, no panic");

        // and the file's own value is the amendment's 1.0
        let live = crate::config::Config::load("kestreld.toml").expect("kestreld.toml");
        assert!(
            (live.sizing.stop_floor_pct - 1.0).abs() < 1e-9,
            "kestreld.toml must size at the locked 1.0% floor, got {}",
            live.sizing.stop_floor_pct
        );
    }

    /// `tp_mult` is a knob now (backtest sweep key `tp_mult`), and it moves ONLY `tp_pct`:
    /// `stop_pct`, leverage and margin all read vol/conviction, not the multiple.
    #[test]
    fn tp_mult_is_configurable_and_defaults_to_2r() {
        let mut c = cfg();
        // stop_pct = clamp(1.5*0.1, 1.0, 3.0) = 1.0, held fixed across every tp_mult below.
        for (mult, expect_tp) in [(1.0, 1.0), (2.0, 2.0), (3.0, 3.0), (1.5, 1.5)] {
            c.tp_mult = mult;
            let s = size_position(&c, 0.1, 0.75);
            assert!((s.stop_pct - 1.0).abs() < 1e-9, "mult {mult}: stop {}", s.stop_pct);
            assert!((s.tp_pct - expect_tp).abs() < 1e-9, "mult {mult}: tp {}", s.tp_pct);
            assert!((s.margin - 40.0).abs() < 1e-9, "tp_mult never moves margin");
            assert!((s.leverage - 20.0).abs() < 1e-12, "nor leverage");
        }
        // and the file's own value is the historical 2R default
        let live = crate::config::Config::load("kestreld.toml").expect("kestreld.toml");
        assert!(
            (live.sizing.tp_mult - 2.0).abs() < 1e-9,
            "kestreld.toml must size at the default 2R multiple, got {}",
            live.sizing.tp_mult
        );
        // omitting the key entirely still defaults to 2.0 (serde default, same treatment as
        // stop_floor_pct)
        let parsed: SizingCfg = toml::from_str(
            "bankroll = 1000.0\nvol_ref = 0.4\nmargin_min = 10.0\nmargin_max = 50.0\n",
        )
        .expect("sizing without tp_mult parses");
        assert!((parsed.tp_mult - 2.0).abs() < 1e-9, "missing tp_mult defaults to 2.0");
    }

    #[test]
    fn ceiling_clamp_vol25() {
        // vol1h=2.5 => 1.5*2.5=3.75 clamped to new ceiling 3.0 (was 2.5 under old clamp)
        // Verifies user-locked amendment 2026-08-09: ceiling raised 2.5% -> 3.0%.
        let c = cfg();
        let s = size_position(&c, 2.5, 0.8);
        assert!((s.stop_pct - 3.0).abs() < 1e-9, "stop {}", s.stop_pct);
        assert!((s.tp_pct - 6.0).abs() < 1e-9, "tp {}", s.tp_pct);
    }

    #[test]
    fn leverage_clamp_majors_max10_xyz_max5() {
        // native majors max 10x, xyz max 5x, otherwise 10x. 1.0 floor.
        assert!((clamp_leverage("BTC", 12.0) - 10.0).abs() < 1e-9, "BTC 12 -> 10");
        assert!((clamp_leverage("BTC", 15.0) - 10.0).abs() < 1e-9);
        assert!((clamp_leverage("ETH", 10.5) - 10.0).abs() < 1e-9);
        assert!((clamp_leverage("xyz:TSLA", 7.5) - 5.0).abs() < 1e-9, "xyz 7.5 -> 5");
        assert!((clamp_leverage("xyz:SPX", 12.0) - 5.0).abs() < 1e-9);
        assert!((clamp_leverage("SOL", 0.5) - 1.0).abs() < 1e-9, "floor 1.0");
        assert!((clamp_leverage("BTC", f64::NAN) - 1.0).abs() < 1e-9);
        assert!((clamp_leverage("BTC", 7.5) - 7.5).abs() < 1e-9, "within cap untouched");
        assert!((clamp_leverage("xyz:TSLA", 5.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn leverage_resolve_none_defaults_to_3() {
        assert!((resolve_leverage("BTC", None) - 3.0).abs() < 1e-9);
        assert!((resolve_leverage("xyz:TSLA", None) - 3.0).abs() < 1e-9);
        assert!((resolve_leverage("BTC", Some(7.5)) - 7.5).abs() < 1e-9);
        assert!((resolve_leverage("xyz:TSLA", Some(7.5)) - 5.0).abs() < 1e-9, "xyz capped");
        assert!((resolve_leverage("BTC", Some(15.0)) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn leverage_effective_preserves_fallback_capped() {
        // effective_leverage keeps sizing's fallback when None, but still caps per-market
        assert!((effective_leverage("BTC", None, 20.0) - 10.0).abs() < 1e-9, "BTC 20 capped to 10");
        assert!((effective_leverage("xyz:TSLA", None, 20.0) - 5.0).abs() < 1e-9);
        assert!((effective_leverage("BTC", Some(7.5), 20.0) - 7.5).abs() < 1e-9);
        assert!((effective_leverage("xyz:TSLA", Some(7.5), 20.0) - 5.0).abs() < 1e-9);
    }
}
