//! Feature recomputation from cached 1m candles + the funding ring (spec Decision 4).
//!
//! The live engine sees a 1/s mid tape; the harness sees 1m candles. The SAMPLING differs and
//! that is stated in every report — the ARITHMETIC does not: `r5m/r1h/r24h`, `vol1h` and
//! `range_pos` come from `crate::features::{pct_change, returns_stddev_pct, range_position}`,
//! the same functions `FeatureEngine::features` calls, and `funding_z` comes from
//! `FeatureEngine::funding_z` itself. Nothing in this file re-derives a live formula. That
//! includes `funding_z`'s degenerate-variance guard (a ring with too few samples, or too
//! little dispersion, reports 0.0 rather than an exploding z) — [`funding_z_series`] calls the
//! guarded method directly, so it applies here automatically, with no second copy to keep in
//! sync.
//!
//! Deviations from live, all forced by what the venue serves for history (spec Decision 4):
//!
//!   * **`day_ntl_vlm` is a proxy** — see [`MarketSeries::day_ntl_vlm_at`].
//!   * **`open_interest` is 0.0** — historical OI is not served at all. Nothing in the
//!     screener, sizing or the gates reads it; it exists on `MarketRow` for the API.
//!   * **mids cadence is 1m, not 1/5s** — `r5m` is measured between two minute closes rather
//!     than two 1-second mids, so it is the same formula on a coarser tape.

use crate::contracts::{Features, MarketRow};
use crate::features::{FeatureEngine, pct_change, range_position, returns_stddev_pct};

use super::cache::CachedCandle;

pub const MINUTE_MS: i64 = 60_000;
/// Minutes of history each lookback needs, in candles.
pub const R5M_MINUTES: usize = 5;
pub const R1H_MINUTES: usize = 60;
pub const DAY_MINUTES: usize = 1440;
/// Closes in a vol1h sample: 61 closes give the 60 1m returns the live engine uses.
pub const VOL_CLOSES: usize = R1H_MINUTES + 1;

/// One market's dense 1m tape over the replayed window.
///
/// DENSE IS LOAD-BEARING. Hyperliquid emits a candle only for a minute that traded, and the
/// live engine fills such gaps itself (`FeatureEngine::on_mid` pushes filler bars carrying the
/// previous close as open/high/low/close). [`MarketSeries::new`] does the same, so a quiet
/// minute contributes a zero return on both sides instead of a jump across the hole, and an
/// index is a minute: `idx = (t - first.t) / 60_000`.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketSeries {
    pub market: String,
    candles: Vec<CachedCandle>,
    closes: Vec<f64>,
}

impl MarketSeries {
    /// Sort, de-duplicate by minute and gap-fill `rows` into a dense tape.
    pub fn new(market: impl Into<String>, mut rows: Vec<CachedCandle>) -> Self {
        rows.sort_by_key(|c| c.t);
        rows.dedup_by_key(|c| c.t);
        let candles = densify(rows);
        let closes = candles.iter().map(|c| c.c).collect();
        Self { market: market.into(), candles, closes }
    }

    pub fn candles(&self) -> &[CachedCandle] {
        &self.candles
    }

    pub fn len(&self) -> usize {
        self.candles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candles.is_empty()
    }

    pub fn first_ts(&self) -> Option<i64> {
        self.candles.first().map(|c| c.t)
    }

    pub fn last_ts(&self) -> Option<i64> {
        self.candles.last().map(|c| c.t)
    }

    /// Index of the candle covering `t` — the minute `t` falls in, or the last minute before
    /// it when `t` is past the tape. `None` before the tape starts or when it is empty.
    pub fn index_at(&self, t: i64) -> Option<usize> {
        let first = self.first_ts()?;
        if t < first {
            return None;
        }
        let idx = ((t - first) / MINUTE_MS) as usize;
        Some(idx.min(self.candles.len() - 1))
    }

    pub fn candle(&self, idx: usize) -> Option<&CachedCandle> {
        self.candles.get(idx)
    }

    /// Features as of the close of candle `idx`, mirroring `FeatureEngine::features`.
    ///
    /// `None` while the tape is too short to price the 5m lookback — the live engine returns
    /// `None` for exactly the same reason (its `find_mid(now - 5m)` finds nothing), and a
    /// `MarketRow` with no features is invisible to the screener.
    ///
    /// The 1h and 24h lookbacks fall back the same way live does: 1h to the 5m price, 24h to
    /// the 1h price, so a warming tape reports a shorter-horizon return rather than nothing.
    pub fn features_at(&self, idx: usize, funding_z: f64) -> Option<Features> {
        if idx >= self.candles.len() || idx < R5M_MINUTES {
            return None;
        }
        let mid_now = self.closes[idx];
        let mid_5m = self.closes[idx - R5M_MINUTES];
        let mid_1h = if idx >= R1H_MINUTES { self.closes[idx - R1H_MINUTES] } else { mid_5m };
        let mid_24h = if idx >= DAY_MINUTES { self.closes[idx - DAY_MINUTES] } else { mid_1h };

        let vol_from = idx + 1 - VOL_CLOSES.min(idx + 1);
        let vol1h = returns_stddev_pct(&self.closes[vol_from..=idx]);

        let win_from = idx.saturating_sub(DAY_MINUTES);
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        for c in &self.candles[win_from..=idx] {
            if c.h > high {
                high = c.h;
            }
            if c.l < low {
                low = c.l;
            }
        }
        if mid_now > high {
            high = mid_now;
        }
        if mid_now < low {
            low = mid_now;
        }

        Some(Features {
            r5m: pct_change(mid_now, mid_5m),
            r1h: pct_change(mid_now, mid_1h),
            r24h: pct_change(mid_now, mid_24h),
            vol1h,
            funding_z,
            range_pos: range_position(mid_now, high, low),
        })
    }

    /// DEVIATION FROM LIVE (spec Decision 4): live reads `dayNtlVlm` off `metaAndAssetCtxs`,
    /// the venue's own rolling 24h notional. That series is not served historically, so the
    /// harness proxies it with the trailing 24h of candle QUOTE volume — `Σ v_i · c_i` over
    /// the last 1440 minutes (base volume × close, i.e. notional traded per minute), fewer
    /// while the tape is warming.
    ///
    /// It is the universe filter's input (`min_vlm_native` / `min_vlm_dex`), so a systematic
    /// bias here changes WHICH markets are eligible, not just a score — every report says so.
    /// Gap-filled minutes carry `v = 0` and contribute nothing, which is the truth: no trades.
    pub fn day_ntl_vlm_at(&self, idx: usize) -> f64 {
        if idx >= self.candles.len() {
            return 0.0;
        }
        let from = idx + 1 - DAY_MINUTES.min(idx + 1);
        self.candles[from..=idx].iter().map(|c| c.v * c.c).sum()
    }

    /// The `MarketRow` the screener consumes, as of candle `idx`.
    ///
    /// `mid`/`mark`/`oracle` all collapse to the candle close: the harness has one price per
    /// minute and pretending to a mark-vs-oracle spread it never observed would be invention.
    /// `prev_day_px` is the close 24h back (the oldest close available while warming).
    pub fn market_row_at(&self, idx: usize, funding: f64, funding_z: f64) -> Option<MarketRow> {
        let close = self.candles.get(idx)?.c;
        let prev_day_px = self.closes[idx.saturating_sub(DAY_MINUTES)];
        Some(MarketRow {
            market: self.market.clone(),
            mid: close,
            mark: close,
            oracle: close,
            funding,
            open_interest: 0.0,
            day_ntl_vlm: self.day_ntl_vlm_at(idx),
            prev_day_px,
            features: self.features_at(idx, funding_z),
        })
    }
}

/// Insert a filler candle for every minute with no trades, carrying the previous close as
/// open/high/low/close and zero volume — byte-for-byte the live engine's gap bar
/// (`Bar { close, high: close, low: close }` in `FeatureEngine::on_mid`).
pub fn densify(rows: Vec<CachedCandle>) -> Vec<CachedCandle> {
    if rows.len() < 2 {
        return rows;
    }
    let span = ((rows[rows.len() - 1].t - rows[0].t) / MINUTE_MS + 1) as usize;
    let mut out: Vec<CachedCandle> = Vec::with_capacity(span);
    for row in rows {
        if let Some(prev) = out.last().copied() {
            let mut t = prev.t + MINUTE_MS;
            while t < row.t {
                out.push(CachedCandle { t, o: prev.c, h: prev.c, l: prev.c, c: prev.c, v: 0.0 });
                t += MINUTE_MS;
            }
        }
        out.push(row);
    }
    out
}

/// `(sample time, funding_z)` for every funding sample, computed by pushing them through the
/// LIVE ring (`FeatureEngine::funding_z`) in order, each with its own timestamp.
///
/// The ring is imported, not re-implemented: its 200-sample cap, its hour-bucket dedup (a
/// repeated rate in the SAME settlement hour updates in place rather than resampling; a new
/// hour always appends) and its population-stddev z are whatever the daemon does today,
/// including any future change to them. `samples` here is the venue's fundingHistory table —
/// already one row per settled hour — so every push lands in its own bucket and this reduces
/// to "one ring sample per input row", exactly the NEW-path behavior the live fix now matches.
pub fn funding_z_series(market: &str, samples: &[(i64, f64)]) -> Vec<(i64, f64)> {
    let mut eng = FeatureEngine::new();
    samples.iter().map(|&(t, rate)| (t, eng.funding_z(market, rate, t))).collect()
}

/// The z in force at `t`: the last sample at or before it. Before the first sample the ring
/// is empty and the live engine reports 0.0, so this does too.
pub fn funding_z_at(series: &[(i64, f64)], t: i64) -> f64 {
    match series.binary_search_by_key(&t, |(ts, _)| *ts) {
        Ok(i) => series[i].1,
        Err(0) => 0.0,
        Err(i) => series[i - 1].1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = MINUTE_MS;

    fn candle(t: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> CachedCandle {
        CachedCandle { t, o, h, l, c, v }
    }

    /// Minute-aligned base so index arithmetic is exact.
    fn base() -> i64 {
        let t = 1_700_000_000_000i64;
        t - t.rem_euclid(M)
    }

    /// Closes chosen so every return is exactly ±10%: mean 0, variance 100, vol1h = 10.
    fn alternating_series() -> MarketSeries {
        let b = base();
        let closes = [100.0, 110.0, 99.0, 108.9, 98.01, 107.811, 97.0299];
        let rows = closes
            .iter()
            .enumerate()
            .map(|(i, &c)| candle(b + i as i64 * M, c, c, c, c, 10.0))
            .collect();
        MarketSeries::new("HAND", rows)
    }

    #[test]
    fn hand_built_candles_give_exact_returns_vol_and_range_pos() {
        let s = alternating_series();
        let f = s.features_at(6, 0.0).expect("features at the last candle");

        // r5m: 97.0299 vs the close five minutes back (110.0) -> -11.791% exactly.
        assert!((f.r5m - (-11.791)).abs() < 1e-9, "r5m {}", f.r5m);
        // Under an hour of tape, r1h falls back to the 5m price and r24h to the 1h price —
        // the live engine's `unwrap_or` chain, so all three agree.
        assert!((f.r1h - f.r5m).abs() < 1e-12, "r1h falls back to the 5m price");
        assert!((f.r24h - f.r1h).abs() < 1e-12, "r24h falls back to the 1h price");
        // vol1h: returns are +10, -10, +10, -10, +10, -10 -> mean 0, population var 100.
        assert!((f.vol1h - 10.0).abs() < 1e-9, "vol1h {}", f.vol1h);
        // range_pos: the last close IS the 24h low -> 0.0.
        assert!(f.range_pos.abs() < 1e-12, "range_pos {}", f.range_pos);
        // funding_z is passed through untouched (it belongs to the ring, not the candles).
        assert!((s.features_at(6, -1.75).unwrap().funding_z - (-1.75)).abs() < 1e-12);

        // Volume proxy: Σ v·c with v = 10 on every candle.
        let expected_vlm: f64 = 10.0 * (100.0 + 110.0 + 99.0 + 108.9 + 98.01 + 107.811 + 97.0299);
        assert!((s.day_ntl_vlm_at(6) - expected_vlm).abs() < 1e-9);

        // Warmup: the 5m lookback is not priceable before six candles exist.
        assert!(s.features_at(4, 0.0).is_none(), "no features before 5m of tape");
        assert!(s.features_at(5, 0.0).is_some(), "exactly 5m of tape prices r5m");
        assert!(s.features_at(7, 0.0).is_none(), "index past the tape");
    }

    #[test]
    fn range_pos_uses_candle_highs_and_lows_not_just_closes() {
        let b = base();
        let mut rows: Vec<CachedCandle> = (0..6).map(|i| candle(b + i * M, 100.0, 100.0, 100.0, 100.0, 1.0)).collect();
        // A wick to 110 three minutes ago sets the 24h high; the last close sits at 105.
        rows[3] = candle(b + 3 * M, 100.0, 110.0, 100.0, 100.0, 1.0);
        rows.push(candle(b + 6 * M, 100.0, 105.0, 100.0, 105.0, 1.0));
        let s = MarketSeries::new("WICK", rows);
        let f = s.features_at(6, 0.0).expect("features");
        assert!((f.range_pos - 0.5).abs() < 1e-12, "(105-100)/(110-100) = 0.5, got {}", f.range_pos);
    }

    #[test]
    fn gaps_are_filled_the_way_the_live_engine_fills_them() {
        let b = base();
        // Minutes 1..3 never traded.
        let rows = vec![
            candle(b, 100.0, 101.0, 99.0, 100.5, 5.0),
            candle(b + 4 * M, 102.0, 103.0, 101.0, 102.5, 7.0),
        ];
        let s = MarketSeries::new("GAP", rows);
        assert_eq!(s.len(), 5, "dense tape spans every minute between the two real candles");
        for i in 1..4 {
            let c = s.candle(i).expect("filler");
            assert_eq!(c.t, b + i as i64 * M);
            // Live's filler is Bar { close: prev_close, high: prev_close, low: prev_close }.
            assert!((c.c - 100.5).abs() < 1e-12);
            assert!((c.h - 100.5).abs() < 1e-12);
            assert!((c.l - 100.5).abs() < 1e-12);
            assert!(c.v.abs() < 1e-12, "a minute with no trades adds no volume");
        }
        assert_eq!(s.index_at(b + 2 * M), Some(2), "index is a minute offset");
        assert_eq!(s.index_at(b - M), None, "before the tape");
        assert_eq!(s.index_at(b + 99 * M), Some(4), "past the tape clamps to the last candle");
        // Out-of-order and duplicated rows collapse to the same dense tape.
        let shuffled = MarketSeries::new(
            "GAP",
            vec![
                candle(b + 4 * M, 102.0, 103.0, 101.0, 102.5, 7.0),
                candle(b, 100.0, 101.0, 99.0, 100.5, 5.0),
                candle(b + 4 * M, 102.0, 103.0, 101.0, 102.5, 7.0),
            ],
        );
        assert_eq!(shuffled, s);
    }

    /// The parity pin: one tape, two engines, identical `Features`.
    ///
    /// The live engine is fed three mids a minute (low, high, close) so its 1m bars carry the
    /// same open/high/low/close as the candles handed to the harness. Any divergence in the
    /// r's, vol1h or range_pos is then a real formula fork, not a sampling artifact.
    #[test]
    fn recomputed_features_match_the_live_engine_on_the_same_tape() {
        let b = base();
        let mut eng = FeatureEngine::new();
        let mut rows = Vec::new();
        let mut px = 100.0f64;
        for i in 0..200i64 {
            px += (i as f64 * 7.3).sin() * 0.5;
            let (low, high, close) = (px - 0.3, px + 0.4, px);
            let bar = b + i * M;
            eng.on_mid("BTC", bar, low);
            eng.on_mid("BTC", bar + 30_000, high);
            eng.on_mid("BTC", bar + 59_000, close);
            rows.push(candle(bar, low, high, low, close, 1.0));
        }
        let live = eng.features("BTC").expect("live features");
        let replay = MarketSeries::new("BTC", rows).features_at(199, 0.0).expect("replay features");

        assert!((replay.r5m - live.r5m).abs() < 1e-12, "r5m {} vs {}", replay.r5m, live.r5m);
        assert!((replay.r1h - live.r1h).abs() < 1e-12, "r1h {} vs {}", replay.r1h, live.r1h);
        assert!((replay.r24h - live.r24h).abs() < 1e-12, "r24h {} vs {}", replay.r24h, live.r24h);
        assert!((replay.vol1h - live.vol1h).abs() < 1e-12, "vol1h {} vs {}", replay.vol1h, live.vol1h);
        assert!(
            (replay.range_pos - live.range_pos).abs() < 1e-12,
            "range_pos {} vs {}",
            replay.range_pos,
            live.range_pos
        );
        assert!(live.vol1h > 0.0, "the fixture must actually move, or the pin proves nothing");
    }

    #[test]
    fn funding_z_series_is_the_live_ring_including_its_duplicate_guard() {
        // CHANGED: the original fixture spaced its "duplicate" sample a full hour after the
        // first (`b + 60 * M`). Under the OLD value-only guard that was irrelevant and the
        // repeat was skipped; under the fix a full hour later is a genuinely NEW settlement and
        // MUST append even at the same rate (see funding_z's doc comment) — so the duplicate
        // is moved to 1ms after `b`, same hour, same rate, and the two genuinely-distinct
        // samples shift from the 2h/3h marks to 1h/2h. Every hand-computed z below is
        // unchanged; only the timestamps used to reach them moved.
        let b = base();
        let samples = [(b, 0.01), (b + 1, 0.01), (b + 60 * M, 0.05), (b + 120 * M, 0.03)];
        let series = funding_z_series("BTC", &samples);

        // Same samples through the live ring, sample by sample.
        let mut eng = FeatureEngine::new();
        let expected: Vec<f64> = samples.iter().map(|&(t, r)| eng.funding_z("BTC", r, t)).collect();
        assert_eq!(series.iter().map(|(_, z)| *z).collect::<Vec<_>>(), expected);
        // ...and the same ring state, so a later push would agree too.
        assert_eq!(eng.funding_hist_len("BTC"), Some(3), "the same-hour duplicate never entered the ring");

        // Hand-computed ring arithmetic: one sample -> 0; same-hour duplicate -> unchanged;
        // [0.01,0.05] -> z(0.05) = 1.0 raw; [0.01,0.05,0.03] -> 0.03 is the mean -> 0.
        //
        // CHANGED (degenerate-variance guard): series[2]'s raw z(0.05)=1.0 is now suppressed to
        // 0.0 by MIN_FUNDING_SAMPLES (24) — n=2 here, same as every other small-n fixture in
        // this crate. The equality check above (`series` vs `expected`) already proves this
        // test's actual point — funding_z_series is bit-identical to pushing the live ring
        // directly, guard included — regardless of what the guard does to any individual value.
        assert!(series[0].1.abs() < 1e-12);
        assert!(series[1].1.abs() < 1e-12, "a repeated same-hour funding rate must not move z");
        assert!(series[2].1.abs() < 1e-12, "n=2 is below the min-sample floor, so the raw z(0.05)=1.0 is suppressed to 0.0");
        assert!(series[3].1.abs() < 1e-12);

        // The boot-seed path lands on the same z for the same samples.
        let mut seeded = FeatureEngine::new();
        seeded.seed_funding_history("BTC", &samples);
        assert_eq!(seeded.funding_hist_vals("BTC"), eng.funding_hist_vals("BTC"));

        // z_at holds the last sample's value until the next one prints — still true with the
        // guard active, just holding 0.0 (series[2]'s now-suppressed value) instead of 1.0.
        assert!(funding_z_at(&series, b - 1).abs() < 1e-12, "empty ring before the first sample");
        assert!(funding_z_at(&series, b + 90 * M).abs() < 1e-12, "z holds between samples");
        assert!(funding_z_at(&series, b + 60 * M).abs() < 1e-12, "exact hit");
        assert!(funding_z_at(&series, b + 10_000 * M).abs() < 1e-12, "last sample stays in force");
        assert!(funding_z_at(&[], b).abs() < 1e-12, "no funding history at all is z = 0");
    }

    /// Parity requirement (spec Decision 4 / wave-3 funding_z fix): `funding_z_series` builds
    /// its z series purely from the venue's settled hourly funding table — the SAME table the
    /// live daemon boot-seeds from and the SAME rate the ctx stream eventually reports each
    /// hour. Prove the live engine lands on the IDENTICAL z at every hour even when it also
    /// sees a batch of noisy intra-hour ctx polls the backtest never does: hour-bucketing means
    /// same-hour drift never changes which ring slot a settlement occupies, so once the live
    /// engine also observes the true settled rate (which it does — ctx converges to the
    /// settled value as the hour ends), its z for that hour is bit-identical to the backtest's,
    /// independent of how much drift noise came before it.
    #[test]
    fn live_engine_matches_the_backtest_hourly_series_despite_interleaved_drift() {
        let b = 1_700_002_800_000i64; // hour-bucket-aligned, so the intra-hour offsets below never spill over
        // Amplitude bumped from the original 0.0002 to 0.001 for the degenerate-variance guard:
        // at 0.0002 this sinusoid's std/|mean| ratio sits under FUNDING_STD_REL_FLOOR (0.05), so
        // every sample in the series would now be suppressed to 0.0 and the "must actually
        // vary" sanity check below would fail — not because parity broke, but because the
        // fixture stopped producing a signal at all. The parity assertions themselves don't
        // depend on the amplitude: both sides push the identical settled-rate sequence through
        // the identical guarded `funding_z`, so whatever it does, it does identically to both.
        let hourly: Vec<(i64, f64)> =
            (0..72i64).map(|i| (b + i * 60 * M, 0.01 + 0.001 * ((i as f64) * 0.9).sin())).collect();

        let backtest_series = funding_z_series("BTC", &hourly);

        // Live engine: for every hour, 4 noisy intra-hour polls, THEN the true settled rate as
        // the final push for that hour (mirroring the ctx stream converging on settlement) —
        // all strictly inside the hour (max offset 50min), so none of it crosses a bucket.
        let mut live = FeatureEngine::new();
        let mut live_series: Vec<(i64, f64)> = Vec::with_capacity(hourly.len());
        for &(t, rate) in &hourly {
            for tick in 1..5i64 {
                let noisy = rate + 1e-7 * tick as f64;
                live.funding_z("BTC", noisy, t + tick * 9_000);
            }
            let z = live.funding_z("BTC", rate, t + 50 * 60_000);
            live_series.push((t, z));
        }

        assert_eq!(live_series.len(), backtest_series.len());
        for (i, (&(bt_t, bt_z), &(live_t, live_z))) in backtest_series.iter().zip(live_series.iter()).enumerate() {
            assert_eq!(bt_t, live_t, "sample {i} timestamps diverged");
            assert!(
                (bt_z - live_z).abs() < 1e-9,
                "sample {i} (t={bt_t}): backtest z {bt_z} vs live-with-drift z {live_z}"
            );
        }
        assert!(backtest_series.iter().any(|&(_, z)| z.abs() > 1e-6), "the fixture must actually vary, or parity proves nothing");
    }

    /// Companion to the parity test above, but for the SUPPRESSED branch of the
    /// degenerate-variance guard: `funding_z_series` inherits the guard by calling
    /// `FeatureEngine::funding_z` directly (no second copy of the arithmetic — see this file's
    /// module doc), so live and replay agreeing when the guard reports a real z doesn't yet
    /// prove they agree when it reports NO signal. Same interleaved-intra-hour-drift shape as
    /// the test above, so this also confirms the noise never survives past its own hour to
    /// nudge a genuinely pinned ring out of "pinned".
    #[test]
    fn funding_z_series_parity_holds_when_the_guard_suppresses_both_sides() {
        let b = 1_700_002_800_000i64;
        // A market pinned at an identical funding-rate-cap value for 40 distinct settled hours
        // — std is exactly 0.0, so every sample is suppressed by FUNDING_STD_ABS_FLOOR on both
        // paths once MIN_FUNDING_SAMPLES (24) clears.
        let hourly: Vec<(i64, f64)> = (0..40i64).map(|i| (b + i * 60 * M, 1.25e-5)).collect();

        let backtest_series = funding_z_series("BTC", &hourly);

        let mut live = FeatureEngine::new();
        let mut live_series: Vec<(i64, f64)> = Vec::with_capacity(hourly.len());
        for &(t, rate) in &hourly {
            for tick in 1..5i64 {
                live.funding_z("BTC", rate + 1e-7 * tick as f64, t + tick * 9_000);
            }
            let z = live.funding_z("BTC", rate, t + 50 * 60_000);
            live_series.push((t, z));
        }

        assert_eq!(backtest_series, live_series, "the guard must suppress identically on both paths, sample for sample, even with intra-hour drift");
        assert!(
            backtest_series.iter().all(|&(_, z)| z.abs() < 1e-12),
            "a pinned market must read 0.0 at every sample once warmed up, guard active on both paths"
        );
    }

    #[test]
    fn market_row_carries_the_volume_proxy_and_no_open_interest() {
        let s = alternating_series();
        let row = s.market_row_at(6, 0.0001, 1.5).expect("row");
        assert_eq!(row.market, "HAND");
        assert!((row.mid - 97.0299).abs() < 1e-12);
        assert!((row.mark - row.mid).abs() < 1e-12, "one price per minute: mark == mid");
        assert!((row.oracle - row.mid).abs() < 1e-12);
        assert!((row.funding - 0.0001).abs() < 1e-12);
        assert!(row.open_interest.abs() < 1e-12, "historical OI is not served — never invented");
        assert!((row.day_ntl_vlm - s.day_ntl_vlm_at(6)).abs() < 1e-12);
        assert!((row.prev_day_px - 100.0).abs() < 1e-12, "warming tape: oldest close stands in");
        assert!((row.features.expect("features").funding_z - 1.5).abs() < 1e-12);
        // Before the 5m warmup the row still exists but carries no features, so the screener
        // skips it exactly as it skips a warming live market.
        assert!(s.market_row_at(2, 0.0, 0.0).expect("row").features.is_none());
    }
}
