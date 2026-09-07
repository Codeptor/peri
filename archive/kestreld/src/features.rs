#![allow(dead_code)]

use std::collections::HashMap;

use crate::contracts::Features;
use crate::hl_rest::{Candle, CtxRow};

/// Ring buffer with fixed capacity, preallocated, zero alloc after init.
struct Ring<T: Clone> {
    buf: Vec<Option<T>>,
    head: usize,
    len: usize,
}

impl<T: Clone> Ring<T> {
    fn new(cap: usize) -> Self {
        Self {
            buf: vec![None; cap],
            head: 0,
            len: 0,
        }
    }

    fn cap(&self) -> usize {
        self.buf.len()
    }

    fn push(&mut self, val: T) {
        self.buf[self.head] = Some(val);
        self.head = (self.head + 1) % self.buf.len();
        if self.len < self.buf.len() {
            self.len += 1;
        }
    }

    /// Iterate in chronological order (oldest to newest)
    fn iter(&self) -> impl Iterator<Item = &T> {
        let cap = self.buf.len();
        let start = if self.len == cap {
            self.head
        } else {
            0
        };
        (0..self.len).filter_map(move |i| {
            let idx = (start + i) % cap;
            self.buf[idx].as_ref()
        })
    }

    fn last(&self) -> Option<&T> {
        if self.len == 0 {
            return None;
        }
        let idx = if self.head == 0 {
            self.buf.len() - 1
        } else {
            self.head - 1
        };
        self.buf[idx].as_ref()
    }

    /// Mutable handle to the newest slot — lets a same-bucket update overwrite in place
    /// instead of pushing (which would otherwise consume a ring slot every call).
    fn last_mut(&mut self) -> Option<&mut T> {
        if self.len == 0 {
            return None;
        }
        let idx = if self.head == 0 {
            self.buf.len() - 1
        } else {
            self.head - 1
        };
        self.buf[idx].as_mut()
    }
}

#[derive(Clone, Debug)]
struct Bar {
    ts: i64, // minute start ms
    close: f64,
    high: f64,
    low: f64,
}

struct MarketState {
    raw: Ring<(i64, f64)>, // 1s mids, 5400 slots = 90min
    bars: Ring<Bar>,       // 1m bars, 1440 slots = 24h
    cur_bar_ts: Option<i64>,
    cur_bar_high: f64,
    cur_bar_low: f64,
    cur_bar_close: f64,
    funding_hist: Ring<f64>, // trailing 7d funding (≈168 hourly samples, use 200 cap)
    funding_bucket: Option<i64>, // hour bucket (ts_ms / FUNDING_HOUR_MS) of funding_hist's newest slot
    funding_z: f64,
    latest_mid: Option<f64>,
    latest_ts: Option<i64>,
}

impl MarketState {
    fn new() -> Self {
        Self {
            raw: Ring::new(5400),
            bars: Ring::new(1440),
            cur_bar_ts: None,
            cur_bar_high: f64::NEG_INFINITY,
            cur_bar_low: f64::INFINITY,
            cur_bar_close: 0.0,
            funding_hist: Ring::new(200),
            funding_bucket: None,
            funding_z: 0.0,
            latest_mid: None,
            latest_ts: None,
        }
    }
}

/// Hyperliquid settles funding once per hour; this is the bucket width `funding_z` dedupes
/// samples against — see its doc comment for why the bucket (not the old value-only epsilon)
/// is what keeps the ring a series of distinct HOURLY samples.
const FUNDING_HOUR_MS: i64 = 3_600_000;

/// Minimum settled hourly samples a ring needs before `funding_z` reports anything but 0.0 —
/// one full day, so the mean spans a complete daily funding cycle rather than a handful of
/// consecutive hours from a single regime. The daemon's boot seed pulls 7 days (~168 samples)
/// before `funding_z` is ever called live, and every one of the 326 markets in
/// `backtest_cache.db` (2026-08-10) has at least 46 cached hourly rows, so this floor costs
/// nothing in production today — it exists purely to protect a market that has JUST listed,
/// whose ring is still warming up from zero.
const MIN_FUNDING_SAMPLES: usize = 24;

/// Below this ABSOLUTE stddev, a ring is pinned (values identical, or differing only by float
/// noise) and `funding_z` reports 0.0 rather than a z-score no dispersion can back. Many
/// markets sit at an exact funding-rate-cap value for hours at a time — measured against
/// `backtest_cache.db`, 74 of 326 cached markets have std under this floor, most of them
/// LITERALLY zero (funding pinned bit-for-bit) or ~1e-21 (float noise around a pinned cap).
const FUNDING_STD_ABS_FLOOR: f64 = 1e-9;

/// Below this ratio of stddev to |mean|, a ring isn't pinned but is varying only as noise
/// around a baseline far larger than that noise — the same "z-score of nothing" trap, one
/// step short of literally flat. Measured against `backtest_cache.db`: the worst
/// near-degenerate market this floor alone catches sits at ratio 0.036 (kFLOKI); the
/// dislocation this guard must NEVER suppress, xyz:TSLA, sits at ratio ~1.85 — std EXCEEDS
/// the mean, two orders of magnitude of headroom above this floor.
const FUNDING_STD_REL_FLOOR: f64 = 0.05;

/// Below this |mean|, the ratio above is undefined (or explodes) even for a market with real,
/// healthy dispersion oscillating around zero funding — that case is judged by
/// `FUNDING_STD_ABS_FLOOR` alone; the ratio floor is skipped rather than misfiring on it.
const FUNDING_MEAN_EPS: f64 = 1e-12;

/// Belt-and-braces ceiling on a trustworthy z, applied only once the guards above have already
/// accepted the ring's dispersion. Every one of the 326 cached markets that clears the guards
/// tops out at |z| = 4.48 (xyz:AMZN) today; a real dislocation the size of xyz:TSLA's
/// 2026-08-10 print reads ≈ -7.85. 20.0 sits comfortably above both.
///
/// It is, in fact, unreachable through `funding_z` as written today: the population z of a
/// self-inclusive sample (the current value is always one of the values its own mean/std are
/// computed over — Samuelson's inequality) is bounded by `sqrt(n - 1)`, and the ring caps `n`
/// at 200, so no call through the public API can ever raise a raw z above `sqrt(199) ≈ 14.11`
/// — comfortably under this ceiling regardless of how extreme a single print is. The clamp
/// stays anyway as defense-in-depth against a future change to the ring cap or to how the
/// population is built (e.g. scoring against a window that excludes the current sample, which
/// would void the Samuelson bound silently); `funding_z_ceiling_clamps_an_unreachable_raw_z`
/// exercises it directly against `funding_z_score`, since the ring path cannot reach it today.
const FUNDING_Z_CEILING: f64 = 20.0;

pub struct FeatureEngine {
    markets: HashMap<String, MarketState>,
}

impl FeatureEngine {
    pub fn new() -> Self {
        Self {
            markets: HashMap::new(),
        }
    }

    pub fn raw_len(&self, market: &str) -> Option<usize> {
        self.markets.get(market).map(|s| s.raw.len)
    }

    /// Latest mid known to the engine (from ws allMids via on_mid). Used to seed
    /// snapshot rows for open-position markets that are outside the vlm universe
    /// (position markets are always tracked).
    pub fn latest_mid(&self, market: &str) -> Option<f64> {
        self.markets.get(market).and_then(|s| s.latest_mid)
    }

    fn get_or_create(&mut self, market: &str) -> &mut MarketState {
        self.markets.entry(market.to_string()).or_insert_with(MarketState::new)
    }

    pub fn on_mid(&mut self, market: &str, ts_ms: i64, mid: f64) {
        let st = self.get_or_create(market);
        // Zero alloc after preallocation: only ring buffer writes
        st.raw.push((ts_ms, mid));
        st.latest_mid = Some(mid);
        st.latest_ts = Some(ts_ms);

        // 1m bar aggregation
        let bar_ts = ts_ms - (ts_ms % 60_000);
        match st.cur_bar_ts {
            None => {
                st.cur_bar_ts = Some(bar_ts);
                st.cur_bar_high = mid;
                st.cur_bar_low = mid;
                st.cur_bar_close = mid;
            }
            Some(cur) if cur == bar_ts => {
                if mid > st.cur_bar_high {
                    st.cur_bar_high = mid;
                }
                if mid < st.cur_bar_low {
                    st.cur_bar_low = mid;
                }
                st.cur_bar_close = mid;
            }
            Some(cur) => {
                // close previous bar(s) — handle gaps
                let prev = Bar {
                    ts: cur,
                    close: st.cur_bar_close,
                    high: st.cur_bar_high,
                    low: st.cur_bar_low,
                };
                st.bars.push(prev);
                // If gap >1m, fill with last close? For simplicity fill gap with same close
                let mut fill_ts = cur + 60_000;
                while fill_ts < bar_ts {
                    let filler = Bar {
                        ts: fill_ts,
                        close: st.cur_bar_close,
                        high: st.cur_bar_close,
                        low: st.cur_bar_close,
                    };
                    st.bars.push(filler);
                    fill_ts += 60_000;
                }
                st.cur_bar_ts = Some(bar_ts);
                st.cur_bar_high = mid;
                st.cur_bar_low = mid;
                st.cur_bar_close = mid;
            }
        }
    }

    pub fn on_ctx(&mut self, row: &CtxRow, funding_hist_z: f64) {
        let st = self.get_or_create(&row.market);
        st.funding_z = funding_hist_z;
    }

    /// Ring hygiene for the ctx stream: Hyperliquid *settles* funding once per HOUR, but the
    /// ctx feed reports the current hour's ACCRUING rate, and that number drifts continuously
    /// — measured live moving ~1.6e-8 every ~9s, four orders of magnitude above the exact-value
    /// epsilon this guard used to rely on alone. An epsilon-only guard therefore treats every
    /// drift tick as a new sample and refills the 200-slot ring with ~30 minutes of intra-hour
    /// noise, evicting the 7-day seed.
    ///
    /// The fix is bucketing by settlement hour (`ts_ms / FUNDING_HOUR_MS`): a push landing in
    /// the SAME hour as the ring's newest slot UPDATES that slot in place — the drift is still
    /// one sample until the hour actually settles. A push in a NEW hour APPENDS, even when its
    /// rate happens to equal the previous hour's: a repeated rate is still a genuinely new
    /// settled sample and must be counted, or a market sitting at a flat funding rate would
    /// silently lose ring coverage. The value-epsilon check survives only as a cheap
    /// short-circuit within a bucket: an unchanged value skips the mean/std recompute.
    ///
    /// DEGENERATE-VARIANCE GUARD: fixing the ring above surfaced a second defect — a raw
    /// `(funding - mean) / std` has no floor on `std`, and HL funding is frequently near-flat
    /// (many markets sit pinned at their funding-rate cap for hours). Below `MIN_FUNDING_SAMPLES`
    /// this returns 0.0 outright (not enough history to trust ANY z); at or above it,
    /// `funding_z_score` decides whether the ring's dispersion can support a z-score at all —
    /// see its doc comment and the constants above for the calibration against
    /// `backtest_cache.db`. `backtest::features::funding_z_series` calls this method directly
    /// (not a copy of its arithmetic), so the guard applies identically to live and replay.
    pub fn funding_z(&mut self, market: &str, funding: f64, ts_ms: i64) -> f64 {
        let st = self.get_or_create(market);
        let bucket = ts_ms.div_euclid(FUNDING_HOUR_MS);

        if st.funding_bucket == Some(bucket) {
            // Still the same settlement hour as the newest ring slot: not a new sample.
            if let Some(&last) = st.funding_hist.last()
                && (funding - last).abs() < 1e-12
            {
                return st.funding_z; // cheap short-circuit: value hasn't moved at all
            }
            if let Some(slot) = st.funding_hist.last_mut() {
                *slot = funding; // intra-hour drift updates in place, no append
            }
        } else {
            // A new hour (or the very first sample ever): always append, regardless of value.
            st.funding_hist.push(funding);
            st.funding_bucket = Some(bucket);
        }

        let n = st.funding_hist.len;
        let z = if n < MIN_FUNDING_SAMPLES {
            0.0
        } else {
            let vals: Vec<f64> = st.funding_hist.iter().copied().collect();
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64;
            funding_z_score(funding, mean, var.sqrt())
        };
        st.funding_z = z;
        z
    }

    pub fn funding_hist_len(&self, market: &str) -> Option<usize> {
        self.markets.get(market).map(|s| s.funding_hist.len)
    }

    /// Seed the trailing funding ring from historical fundingHistory rows.
    /// `samples` must be oldest→newest (we iterate in given order) and each row's OWN
    /// timestamp is what makes this a real hourly seed: fundingHistory rows are one per
    /// settled hour, so pushing them through `funding_z` with their real `ts` — rather than
    /// discarding it — is what turns a 7-day seed into ~168 hourly ring samples instead of 168
    /// arbitrary ones. Respects the same hour-bucket + epsilon hygiene as every other caller
    /// (see `funding_z`), so seeding rows that are not perfectly hour-spaced, or re-seeding,
    /// stays safe.
    pub fn seed_funding_history(&mut self, market: &str, samples: &[(i64, f64)]) {
        for &(ts, funding) in samples {
            self.funding_z(market, funding, ts);
        }
    }

    pub fn funding_hist_vals(&self, market: &str) -> Option<Vec<f64>> {
        self.markets
            .get(market)
            .map(|s| s.funding_hist.iter().copied().collect())
    }

    pub fn features(&self, market: &str) -> Option<Features> {
        let st = self.markets.get(market)?;
        // Need at least 5m wall time or 300 ticks (to handle sparse real feed vs test)
        if st.raw.len < 2 {
            return None;
        }
        let first_ts = st.raw.iter().next().map(|(ts, _)| *ts).unwrap_or(0);
        let last_ts = st.raw.last().map(|(ts, _)| *ts).unwrap_or(0);
        let time_ok = last_ts - first_ts >= 5 * 60 * 1000;
        let count_ok = st.raw.len >= 300;
        if !time_ok && !count_ok {
            return None;
        }
        let (ts_now, mid_now) = st.raw.last().copied()?;
        // Helpers to find mid at target time
        let find_mid = |target: i64| -> Option<f64> {
            // Search raw first (90min window), then bars
            // For raw: find closest entry with ts <= target? Use nearest.
            let mut best: Option<(i64, f64)> = None;
            for (ts, mid) in st.raw.iter() {
                if *ts <= target {
                    best = Some((*ts, *mid));
                } else {
                    // Since iter is chronological, first > target, break
                    // But best is the latest <= target
                    break;
                }
            }
            if let Some((_, m)) = best {
                return Some(m);
            }
            // Fallback to bars (older than 90min)
            // bars ts is minute start, close price
            let mut best_bar: Option<&Bar> = None;
            for bar in st.bars.iter() {
                if bar.ts <= target {
                    best_bar = Some(bar);
                } else {
                    break;
                }
            }
            best_bar.map(|b| b.close)
        };

        let mid_5m = find_mid(ts_now - 5 * 60 * 1000)?;
        let mid_1h = find_mid(ts_now - 60 * 60 * 1000).unwrap_or(mid_5m);
        // For 24h, if not enough history, use earliest available
        let mid_24h = find_mid(ts_now - 24 * 60 * 60 * 1000).unwrap_or(mid_1h);

        let r5m = pct_change(mid_now, mid_5m);
        let r1h = pct_change(mid_now, mid_1h);
        let r24h = pct_change(mid_now, mid_24h);

        // vol1h: stddev of 1m simple returns over 60m, pct
        // Collect last 61 bar closes including current cur_bar_close if needed
        // We have bars + cur bar
        let mut closes: Vec<f64> = Vec::with_capacity(61);
        // gather bars in order
        for bar in st.bars.iter() {
            closes.push(bar.close);
        }
        // add current bar if it has data and not yet pushed
        if let Some(cur_ts) = st.cur_bar_ts {
            // avoid duplicate if last bar ts == cur_ts
            let last_bar_ts = st.bars.last().map(|b| b.ts);
            if Some(cur_ts) != last_bar_ts {
                closes.push(st.cur_bar_close);
            }
        }
        // Need at least 61 closes for 60 returns; with fewer, compute over what there is.
        let vol1h = if closes.len() >= 61 {
            returns_stddev_pct(&closes[closes.len() - 61..])
        } else {
            returns_stddev_pct(&closes)
        };

        // range_pos: (mid - 24h low)/(24h high - low) in [0,1]
        // Compute high/low over 24h window using bars + raw
        let window_start = ts_now - 24 * 60 * 60 * 1000;
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        for bar in st.bars.iter() {
            if bar.ts >= window_start {
                if bar.high > high {
                    high = bar.high;
                }
                if bar.low < low {
                    low = bar.low;
                }
            }
        }
        // include raw mids in window and cur bar
        for (ts, mid) in st.raw.iter() {
            if *ts >= window_start {
                if *mid > high {
                    high = *mid;
                }
                if *mid < low {
                    low = *mid;
                }
            }
        }
        if let Some(cur_ts) = st.cur_bar_ts
            && cur_ts >= window_start
        {
            if st.cur_bar_high > high {
                high = st.cur_bar_high;
            }
            if st.cur_bar_low < low {
                low = st.cur_bar_low;
            }
        }
        // Also consider current mid
        if mid_now > high {
            high = mid_now;
        }
        if mid_now < low {
            low = mid_now;
        }
        let range_pos = range_position(mid_now, high, low);

        Some(Features {
            r5m,
            r1h,
            r24h,
            vol1h,
            funding_z: st.funding_z,
            range_pos,
        })
    }
}

impl Default for FeatureEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Feature formulas, shared with the backtest harness ────────────────────────────────
//
// `FeatureEngine::features` computes over its own rings and the harness computes over cached
// 1m candles, but the ARITHMETIC must be one implementation or the two silently drift and
// every sweep result becomes unfalsifiable. These three are that implementation; the harness
// (`backtest::features`) calls them and nothing else re-derives them.

/// Percent change from `then` to `now`, the r5m/r1h/r24h expression.
///
/// Deliberately unguarded: a zero `then` yields inf/NaN rather than a made-up 0.0, exactly as
/// the live engine has always behaved. Callers get `then` from a real observed price.
pub fn pct_change(now: f64, then: f64) -> f64 {
    (now - then) / then * 100.0
}

/// Population stddev, in percent, of the consecutive simple returns of `closes` — the vol1h
/// formula. Fewer than two closes is 0.0 (nothing to measure); a zero previous close
/// contributes a 0.0 return rather than an infinity.
pub fn returns_stddev_pct(closes: &[f64]) -> f64 {
    if closes.len() < 2 {
        return 0.0;
    }
    let rets: Vec<f64> = closes
        .windows(2)
        .map(|w| {
            let (prev, cur) = (w[0], w[1]);
            if prev.abs() < 1e-12 {
                0.0
            } else {
                (cur - prev) / prev * 100.0
            }
        })
        .collect();
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rets.len() as f64;
    var.sqrt()
}

/// Where `mid` sits inside `[low, high]`, clamped to `[0, 1]`. A degenerate or non-finite
/// range carries no information and reads as the midpoint, 0.5.
pub fn range_position(mid: f64, high: f64, low: f64) -> f64 {
    if high.is_finite() && low.is_finite() && (high - low).abs() > 1e-12 {
        ((mid - low) / (high - low)).clamp(0.0, 1.0)
    } else {
        0.5
    }
}

/// The z-score `funding_z` reports for `current` against a ring whose population mean/std are
/// `mean`/`std`, or 0.0 when that ring's own distribution can't support a trustworthy z.
/// Unlike `pct_change`/`returns_stddev_pct`/`range_position` above, this isn't imported by the
/// backtest harness directly — `backtest::features::funding_z_series` calls
/// `FeatureEngine::funding_z` itself (see that file's module doc), which calls this, so live
/// and replay share the guard by construction rather than by a second copy of it.
///
/// Three independent conditions each report "no signal" rather than a number a trivial move
/// can blow up — see the constants above `FeatureEngine` for their calibration against
/// `backtest_cache.db`:
///
///  1. `std` under `FUNDING_STD_ABS_FLOOR` — the ring is pinned (often bit-for-bit; sometimes
///     float noise around a value repeatedly hitting the venue's funding-rate cap).
///  2. `std / |mean|` under `FUNDING_STD_REL_FLOOR` — not pinned, but varying only as noise
///     around a baseline far larger than that noise: one step short of literally flat.
///  3. Neither fires — the ring is judged trustworthy, and the z is clamped to
///     ±`FUNDING_Z_CEILING` as belt-and-braces against a still-pathological print.
///
/// Condition 2 is skipped when `|mean|` is itself under `FUNDING_MEAN_EPS`: the ratio would
/// divide by ~0 even for a market with real, healthy dispersion oscillating around zero
/// funding, and `FUNDING_STD_ABS_FLOOR` alone already judges that case correctly.
///
/// (The caller is also responsible for the sample-count floor, `MIN_FUNDING_SAMPLES` — that
/// check short-circuits before `mean`/`std` are even computed, so it isn't repeated here.)
fn funding_z_score(current: f64, mean: f64, std: f64) -> f64 {
    if std < FUNDING_STD_ABS_FLOOR {
        return 0.0;
    }
    if mean.abs() > FUNDING_MEAN_EPS && std / mean.abs() < FUNDING_STD_REL_FLOOR {
        return 0.0;
    }
    ((current - mean) / std).clamp(-FUNDING_Z_CEILING, FUNDING_Z_CEILING)
}

/// Classic Wilder ATR over high/low/close, returned as % of last close.
/// `period` is the Wilder period (e.g. 14). Returns None when fewer than
/// period+1 candles are supplied (need period TR values) or last close ~0.
pub fn atr_pct(candles: &[Candle], period: usize) -> Option<f64> {
    if period == 0 || candles.len() < period + 1 {
        return None;
    }
    // True ranges for candles[1..]
    let mut trs: Vec<f64> = Vec::with_capacity(candles.len() - 1);
    for i in 1..candles.len() {
        let prev_close = candles[i - 1].c;
        let high = candles[i].h;
        let low = candles[i].l;
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        trs.push(tr);
    }
    if trs.len() < period {
        return None;
    }
    // Wilder smoothing: first ATR = mean of first `period` TRs
    let mut atr = trs[..period].iter().sum::<f64>() / period as f64;
    for &tr in &trs[period..] {
        atr = (atr * (period as f64 - 1.0) + tr) / period as f64;
    }
    let last_close = candles.last().map(|c| c.c).unwrap_or(0.0);
    if last_close.abs() < 1e-12 {
        return None;
    }
    Some(atr / last_close * 100.0)
}

/// Exponential moving average over `closes` (oldest→newest), standard `k = 2/(period+1)`
/// smoothing seeded with the first close, aligned 1:1 with the input. Empty input or
/// `period == 0` yields an empty series; period 1 is the identity (k = 1).
pub fn ema(closes: &[f64], period: usize) -> Vec<f64> {
    if period == 0 || closes.is_empty() {
        return Vec::new();
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut out = Vec::with_capacity(closes.len());
    let mut prev = closes[0];
    out.push(prev);
    for &c in &closes[1..] {
        prev = c * k + prev * (1.0 - k);
        out.push(prev);
    }
    out
}

/// MACD histogram (12/26/9): fast EMA minus slow EMA, minus the 9-EMA signal of that line.
/// Same seeding convention as `ema`, so the series is aligned 1:1 with `closes` from the first
/// bar — early values are warm-up, which is why prompt rendering reads the tail. Empty input
/// yields an empty series.
pub fn macd_hist(closes: &[f64]) -> Vec<f64> {
    if closes.is_empty() {
        return Vec::new();
    }
    let fast = ema(closes, 12);
    let slow = ema(closes, 26);
    let line: Vec<f64> = fast.iter().zip(slow.iter()).map(|(f, s)| f - s).collect();
    let signal = ema(&line, 9);
    line.iter()
        .zip(signal.iter())
        .map(|(m, s)| m - s)
        .collect()
}

/// Wilder RSI over `closes` (oldest→newest), aligned 1:1 with the input; the first `period`
/// slots are `None` (warm-up — `period` deltas are needed before the first average). Fewer
/// than `period + 1` closes or `period == 0` yields an empty series. A window with zero
/// losses (strictly rising or flat) reads 100.0, the textbook RSI limit as avg_loss → 0.
pub fn rsi(closes: &[f64], period: usize) -> Vec<Option<f64>> {
    if period == 0 || closes.len() < period + 1 {
        return Vec::new();
    }
    let mut out: Vec<Option<f64>> = vec![None; closes.len()];
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    for i in 1..=period {
        let d = closes[i] - closes[i - 1];
        if d > 0.0 {
            avg_gain += d;
        } else {
            avg_loss -= d;
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    out[period] = Some(rsi_from_avgs(avg_gain, avg_loss));
    for i in (period + 1)..closes.len() {
        let d = closes[i] - closes[i - 1];
        let (gain, loss) = if d > 0.0 { (d, 0.0) } else { (0.0, -d) };
        let n = period as f64;
        avg_gain = (avg_gain * (n - 1.0) + gain) / n;
        avg_loss = (avg_loss * (n - 1.0) + loss) / n;
        out[i] = Some(rsi_from_avgs(avg_gain, avg_loss));
    }
    out
}

fn rsi_from_avgs(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss.abs() < 1e-12 {
        return 100.0;
    }
    let rs = avg_gain / avg_loss;
    100.0 - 100.0 / (1.0 + rs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hl_rest::CtxRow;

    #[test]
    fn none_before_5m() {
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        for i in 0..100 {
            eng.on_mid("BTC", start + i * 1000, 100.0);
        }
        assert!(eng.features("BTC").is_none(), "should be None before 5m");
        for i in 100..300 {
            eng.on_mid("BTC", start + i * 1000, 100.0);
        }
        assert!(eng.features("BTC").is_some(), "should have features after 5m");
    }

    #[test]
    fn linear_ramp_r1h_and_r5m() {
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        // 100 ->101 over 3600s, 1 per 3600 increment = 0.0002777 per sec
        for i in 0..=3600 {
            let mid = 100.0 + (i as f64) * (1.0 / 3600.0);
            eng.on_mid("BTC", start + i * 1000, mid);
        }
        let f = eng.features("BTC").expect("features");
        assert!((f.r1h - 1.0).abs() < 1e-9, "r1h {} expected 1.0", f.r1h);
        // r5m over 300s = 300/3600 =0.08333... pct at ~100.8 mid => approx 0.0833
        // Use tolerance 1e-3
        let expected_r5m = {
            let mid_now = 101.0;
            let mid_5m = 101.0 - 300.0 / 3600.0;
            (mid_now - mid_5m) / mid_5m * 100.0
        };
        assert!(
            (f.r5m - expected_r5m).abs() < 1e-4,
            "r5m {} expected {}",
            f.r5m,
            expected_r5m
        );
        // also check approximate 0.0834 as spec notes
        assert!((f.r5m - 0.0834).abs() < 0.001, "r5m approx 0.0834 got {}", f.r5m);
    }

    #[test]
    fn vol_constant_zero() {
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        for i in 0..=3600 {
            eng.on_mid("BTC", start + i * 1000, 100.0);
        }
        let f = eng.features("BTC").expect("features");
        assert!(f.vol1h.abs() < 1e-12, "vol {} expected 0", f.vol1h);
    }

    #[test]
    fn range_pos_at_high() {
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        // Create 24h of data where low 90 mid 100 high 110 at end
        // Simpler: feed linear ramp 90->110 over 24h then mid at high
        for i in 0..=86400 {
            let mid = 90.0 + (i as f64) * (20.0 / 86400.0);
            eng.on_mid("BTC", start + i * 1000, mid);
        }
        let f = eng.features("BTC").expect("features");
        assert!((f.range_pos - 1.0).abs() < 1e-9, "range_pos {} expected 1.0", f.range_pos);
    }

    #[test]
    fn funding_z_computation() {
        // CHANGED: the old version jittered each value by 1e-9 to dodge the value-epsilon
        // guard and simulate "hourly distinct samples" by hand. Hour-bucketing makes that
        // unnecessary — 10 pushes a real hour apart are 10 distinct samples regardless of
        // value, and this now also exercises "identical rate, different hour still appends"
        // (see funding_z's doc comment), which the jittered version never could.
        //
        // CHANGED AGAIN (degenerate-variance guard): bumped from 10 pre-spike samples to 24 —
        // below `MIN_FUNDING_SAMPLES` the spike would now read 0.0 regardless of its size, so
        // the ring needs to actually clear that floor for this test to mean anything. The
        // min-sample floor itself is `funding_z_below_minimum_samples_is_zero_despite_real_dispersion`.
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        for i in 0..24 {
            eng.funding_z("BTC", 0.01, start + i * FUNDING_HOUR_MS);
        }
        let z = eng.funding_z("BTC", 0.05, start + 24 * FUNDING_HOUR_MS);
        assert!(z > 2.0, "z should be large for spike, got {z}");
    }

    #[test]
    fn funding_z_same_hour_drift_updates_in_place_not_append() {
        // RENAMED + CHANGED from `funding_z_ring_hygiene_500_identical_then_step`: that test
        // asserted a value "step" (same wall-clock instant) grew the ring to 2 samples — true
        // under the old value-epsilon guard, but exactly the bug this fix removes (a same-hour
        // value change is drift, not a new sample). The 500-identical-pushes hygiene intent is
        // preserved; the step now asserts the CORRECT in-place update, and a genuine hour
        // rollover is added to show that path still appends (task's hour-rollover case).
        let mut eng = FeatureEngine::new();
        let hour0 = 1_700_002_800_000i64; // arbitrary; only its hour bucket matters
        for i in 0..500 {
            eng.funding_z("BTC", 0.01, hour0 + i * 1000);
        }
        assert_eq!(eng.funding_hist_len("BTC"), Some(1), "500 same-hour pushes collapse to 1 sample");

        // Drift within the SAME hour to 0.05 — updates the slot in place, ring stays length 1.
        let z = eng.funding_z("BTC", 0.05, hour0 + 500_000);
        assert_eq!(eng.funding_hist_len("BTC"), Some(1), "same-hour drift updates in place, does not append");
        assert!(z.abs() < 1e-12, "one ring sample has no variance -> z is 0.0, got {z}");

        // Repeat the same drifted value, still same hour -> still 1 sample, z unchanged.
        let z2 = eng.funding_z("BTC", 0.05, hour0 + 600_000);
        assert_eq!(eng.funding_hist_len("BTC"), Some(1));
        assert!((z2 - z).abs() < 1e-12, "duplicate value within the hour should not move z");

        // Roll into the NEXT hour with a distinct rate -> a real new sample, ring grows to 2.
        let z3 = eng.funding_z("BTC", 0.03, hour0 + FUNDING_HOUR_MS);
        assert_eq!(eng.funding_hist_len("BTC"), Some(2), "hour rollover appends a new sample");
        // CHANGED (degenerate-variance guard): the ring's raw arithmetic still gives vals
        // [0.05, 0.03] -> mean 0.04, std 0.01, z(0.03) = (0.03-0.04)/0.01 = -1.0 exactly — but
        // n=2 is far below MIN_FUNDING_SAMPLES (24), so the guard now reports 0.0 regardless of
        // how clean that raw arithmetic is. This is the correct new behavior, not a regression
        // of the hour-bucketing fix above (which is what the ring-length asserts still prove).
        assert!(z3.abs() < 1e-12, "n=2 is below the min-sample floor, so z is suppressed to 0.0, got {z3}");
    }

    #[test]
    fn on_ctx_stores_funding_z() {
        let mut eng = FeatureEngine::new();
        let start = 1_700_000_000_000i64;
        for i in 0..=400 {
            eng.on_mid("BTC", start + i * 1000, 100.0 + i as f64 * 0.01);
        }
        let row = CtxRow {
            market: "BTC".to_string(),
            mark: 104.0,
            oracle: 104.0,
            mid: 104.0,
            funding: 0.01,
            open_interest: 1000.0,
            day_ntl_vlm: 1_000_000.0,
            prev_day_px: 100.0,
        };
        eng.on_ctx(&row, 1.23);
        let f = eng.features("BTC").expect("features");
        assert!((f.funding_z - 1.23).abs() < 1e-9);
    }

    #[test]
    fn seed_funding_history_basic_and_len() {
        // CHANGED: the original fixture spaced its 3 samples 6 minutes apart. That was
        // irrelevant under the old (value-only) guard but would now collapse all 3 into the
        // SAME hour bucket — updated to real hourly spacing so the fixture still exercises "3
        // distinct fundingHistory rows seed 3 distinct ring samples".
        let mut eng = FeatureEngine::new();
        // Unseeded ring is empty, z is 0
        assert_eq!(eng.funding_hist_len("BTC"), None);
        let samples = vec![
            (1_700_000_000_000, 0.01),
            (1_700_000_000_000 + FUNDING_HOUR_MS, 0.02),
            (1_700_000_000_000 + 2 * FUNDING_HOUR_MS, 0.03),
        ];
        eng.seed_funding_history("BTC", &samples);
        assert_eq!(eng.funding_hist_len("BTC"), Some(3));
        let vals = eng.funding_hist_vals("BTC").unwrap();
        assert!((vals[0] - 0.01).abs() < 1e-12);
        assert!((vals[1] - 0.02).abs() < 1e-12);
        assert!((vals[2] - 0.03).abs() < 1e-12);
    }

    #[test]
    fn seed_funding_history_seeded_z_vs_unseeded() {
        // CHANGED: same re-spacing as `seed_funding_history_basic_and_len` above, for the same
        // reason. The hand-computed z is unaffected — it only depends on the 3 values.
        //
        // CHANGED AGAIN (degenerate-variance guard): a 3-sample seed used to be enough to prove
        // "seeding produces a real z, unlike one raw sample" — it no longer is, because 3 is
        // itself below MIN_FUNDING_SAMPLES (24). Seeded and unseeded now agree at 0.0, for two
        // DIFFERENT reasons (n=1 vs n=3, both under the floor) — the raw statistic a 3-sample
        // ring would produce is kept below as a comment so the "not pinned, just too short"
        // distinction stays visible. The "seeding with enough history produces a real z" case
        // this test used to cover now lives in `funding_z_tsla_shaped_dislocation_survives_the_guard`
        // and `funding_z_near_pinned_boundary_std_over_mean_ratio` (n=118 / n=30).
        let mut unseeded = FeatureEngine::new();
        let z_unseeded = unseeded.funding_z("BTC", 0.03, 1_700_000_000_000);
        // With only one sample, z is 0 (below the min-sample floor).
        assert!((z_unseeded).abs() < 1e-12);

        let mut seeded = FeatureEngine::new();
        let samples = vec![
            (1_700_000_000_000, 0.01),
            (1_700_000_000_000 + FUNDING_HOUR_MS, 0.02),
            (1_700_000_000_000 + 2 * FUNDING_HOUR_MS, 0.03),
        ];
        seeded.seed_funding_history("BTC", &samples);
        // Raw ring arithmetic over [0.01,0.02,0.03]: mean 0.02, std ~0.0081649658,
        // z(0.03) ~1.2247449 — real dispersion, NOT a pinned ring. But n=3 < 24, so the guard
        // suppresses it to 0.0 regardless; this is a min-sample suppression, not a
        // std/rel-floor one.
        let st = seeded.markets.get("BTC").unwrap();
        let z = st.funding_z;
        assert!(z.abs() < 1e-12, "n=3 is below the min-sample floor, so seeded z is 0.0 too, got {z}");
    }

    #[test]
    fn seed_funding_history_hand_computed_z_small_series() {
        // CHANGED: re-spaced to real hours, same reason as the two tests above.
        let mut eng = FeatureEngine::new();
        let samples = vec![
            (1_700_000_000_000, 0.01),
            (1_700_000_000_000 + FUNDING_HOUR_MS, 0.05),
            (1_700_000_000_000 + 2 * FUNDING_HOUR_MS, 0.03),
        ];
        eng.seed_funding_history("BTC", &samples);
        let st = eng.markets.get("BTC").unwrap();
        let z = st.funding_z;
        // mean 0.03 std sqrt((( -0.02)^2+(0.02)^2+0)/3)=0.0163299316 z for 0.03 ->0
        assert!(z.abs() < 1e-9, "z for mean value should be 0 got {z}");
        // CHANGED (degenerate-variance guard): after seeding [0.01,0.05], the raw ring
        // arithmetic is mean 0.03 std 0.02, z(0.05) = 1.0 — but n=2 is below
        // MIN_FUNDING_SAMPLES (24), so the guard now suppresses this to 0.0 too.
        let mut eng2 = FeatureEngine::new();
        let samples2 = vec![(1_700_000_000_000, 0.01), (1_700_000_000_000 + FUNDING_HOUR_MS, 0.05)];
        eng2.seed_funding_history("BTC", &samples2);
        let z2 = eng2.markets.get("BTC").unwrap().funding_z;
        assert!(z2.abs() < 1e-12, "n=2 is below the min-sample floor, so z for [0.01,0.05] is 0.0, got {z2}");
    }

    #[test]
    fn seed_funding_history_respects_hour_bucketing() {
        // RENAMED + CHANGED from `seed_funding_history_respects_change_guard`: that test's
        // "consecutive dupes collapse" fixture put 5 samples within ~24 minutes of each other,
        // all landing in the SAME hour bucket now — which would flatten to 1 ring sample, not
        // the 2 the old value-only guard produced, since the guard that matters is the hour now,
        // not the value. Rebuilt across two real hours so it exercises exactly what the fix
        // changed: same-hour churn (identical AND drifting values) collapses to the hour's last
        // value, while a new hour always appends — even repeating a rate a previous hour used.
        let mut eng = FeatureEngine::new();
        let h0 = 1_700_002_800_000i64; // arbitrary; only its hour bucket matters
        let samples = vec![
            (h0, 0.01),                                // hour N: first sample
            (h0 + 6 * 60_000, 0.01),                   // hour N: identical value, no-op
            (h0 + 12 * 60_000, 0.015),                 // hour N: drift, updates in place
            (h0 + FUNDING_HOUR_MS, 0.02),               // hour N+1: new sample, appended
            (h0 + FUNDING_HOUR_MS + 6 * 60_000, 0.02), // hour N+1: identical value, no-op
        ];
        eng.seed_funding_history("BTC", &samples);
        // Only 2 distinct HOURS were sampled, so the ring holds exactly 2 settled values — the
        // last value observed within each hour (0.015 for hour N, 0.02 for hour N+1).
        assert_eq!(eng.funding_hist_len("BTC"), Some(2));
        let vals = eng.funding_hist_vals("BTC").unwrap();
        assert!((vals[0] - 0.015).abs() < 1e-12, "hour N settles on the last value seen in it");
        assert!((vals[1] - 0.02).abs() < 1e-12);

        // A further identical-value push still inside hour N+1 must not grow the ring.
        let before = eng.funding_hist_len("BTC");
        eng.funding_z("BTC", 0.02, h0 + FUNDING_HOUR_MS + 600_000);
        assert_eq!(eng.funding_hist_len("BTC"), before);

        // A THIRD hour repeating hour N+1's rate must still append: an identical value across
        // an hour boundary is a genuinely new settled sample, not a duplicate.
        eng.funding_z("BTC", 0.02, h0 + 2 * FUNDING_HOUR_MS);
        assert_eq!(eng.funding_hist_len("BTC"), Some(3), "same rate in a new hour still appends");
    }

    #[test]
    fn drift_within_the_seeded_hour_does_not_evict_the_7day_seed() {
        // The exact scenario from the live bug report: a freshly booted daemon seeds 7 days of
        // settled hourly funding (168 samples), then the ctx stream keeps polling every ~9s
        // WITHIN the hour the seed's newest sample belongs to. Before the fix, every one of
        // those drift ticks was a "new" sample to the value-epsilon guard, and ~200 of them
        // (~30 min at ~9s) would evict the entire 7-day seed. After the fix, none of them is
        // even a new RING sample — they update the seed's last slot in place.
        let mut eng = FeatureEngine::new();
        let base = 1_700_002_800_000i64; // exactly hour-bucket-aligned, so the offsets below are exact
        // Alternating +/-1e-3 around 0.01: mean is exactly 0.01, std exactly 1e-3, and the
        // 168th (last, odd-indexed) sample sits at exactly z = -1.0 — hand-verifiable. (Bumped
        // from the original +/-1e-4 for the degenerate-variance guard: at 1e-4 the ratio
        // std/|mean| is 0.01, itself under FUNDING_STD_REL_FLOOR (0.05), which would suppress
        // this fixture's z to 0.0 and defeat the point of the test. 1e-3 gives ratio 0.1 — real
        // dispersion, comfortably clear of the floor — while an equally-weighted two-point
        // population keeps the mean/std/z exactly hand-verifiable regardless of amplitude.)
        let seed: Vec<(i64, f64)> = (0..168i64)
            .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { 0.011 } else { 0.009 }))
            .collect();
        eng.seed_funding_history("BTC", &seed);
        assert_eq!(eng.funding_hist_len("BTC"), Some(168), "7 days of hourly rows seed 168 samples");
        let settled_z = eng.markets.get("BTC").unwrap().funding_z;
        assert!((settled_z - (-1.0)).abs() < 1e-9, "settled z {settled_z} expected exactly -1.0");

        // 300 ticks ~9s apart (the measured live cadence) — 2,691s, comfortably inside the
        // 3,600s hour the ring's 168th (newest) sample belongs to.
        let (last_hour_start, last_rate) = *seed.last().unwrap();
        let mut z = settled_z;
        let mut last_drift = last_rate;
        for k in 0..300i64 {
            last_drift = last_rate + 1.6e-8 * k as f64; // ~measured live drift per tick
            z = eng.funding_z("BTC", last_drift, last_hour_start + k * 9_000);
        }

        assert_eq!(
            eng.funding_hist_len("BTC"),
            Some(168),
            "300 same-hour drift ticks must not append — the ring still holds exactly the 168 seeded samples"
        );
        // Hand-computed expected z: the same 168-value ring, but the 168th value is the FINAL
        // drift tick's value instead of the clean seed's — exactly what an in-place update
        // produces, and independent proof that comes out at the same number the engine reports.
        let mut vals: Vec<f64> = seed.iter().map(|&(_, r)| r).collect();
        *vals.last_mut().unwrap() = last_drift;
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let var = vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64;
        let expected = (last_drift - mean) / var.sqrt();
        assert!((z - expected).abs() < 1e-9, "z {z} expected {expected}");
        assert!(
            (z - settled_z).abs() < 0.1,
            "z should stay close to the settled 7-day value despite 300 same-hour drift ticks: settled {settled_z} drifted {z}"
        );
    }

    #[test]
    fn seeded_vs_settled_matches_the_broken_live_scenario_now_fixed() {
        // Same setup as `drift_within_the_seeded_hour_does_not_evict_the_7day_seed`, but
        // contrasted directly against a literal replica of the OLD (pre-fix) ring — a
        // value-epsilon-only guard with no hour bucketing — to prove they now diverge exactly
        // where the live bug report said they did: 200 ticks (~30 min at ~9s) fully evict the
        // old ring's 168-sample seed, while the fixed engine still reads the settled z.
        //
        // Same +/-1e-3 amplitude as that test, for the same degenerate-variance-guard reason
        // (see its comment): +/-1e-4 would have a std/|mean| ratio under FUNDING_STD_REL_FLOOR
        // and get suppressed to 0.0 by the NEW engine below, defeating the comparison.
        let base = 1_700_002_800_000i64;
        let seed: Vec<(i64, f64)> = (0..168i64)
            .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { 0.011 } else { 0.009 }))
            .collect();
        let (last_hour_start, last_rate) = *seed.last().unwrap();
        const TICKS: i64 = 200; // ~30 minutes at ~9s — the live bug's own eviction window

        // NEW (fixed) engine: seed, then 30 minutes of same-hour drift.
        let mut eng = FeatureEngine::new();
        eng.seed_funding_history("BTC", &seed);
        let settled_z = eng.markets.get("BTC").unwrap().funding_z;
        assert!((settled_z - (-1.0)).abs() < 1e-9);
        let mut new_z = settled_z;
        for k in 0..TICKS {
            let drift = last_rate + 1.6e-8 * k as f64;
            new_z = eng.funding_z("BTC", drift, last_hour_start + k * 9_000);
        }
        assert!(
            (new_z - settled_z).abs() < 0.1,
            "fixed engine: 30 min of same-hour drift should still read close to the settled z, got {new_z} vs settled {settled_z}"
        );

        // OLD (pre-fix) ring: value-epsilon-only guard, no hour bucketing — a literal replica
        // of the removed `FeatureEngine::funding_z` body, kept ONLY so this test can show what
        // it used to do to the exact same input.
        struct OldRing {
            vals: Vec<f64>,
        }
        impl OldRing {
            fn push(&mut self, funding: f64) -> f64 {
                if let Some(&last) = self.vals.last()
                    && (funding - last).abs() < 1e-12
                {
                    return self.z();
                }
                self.vals.push(funding);
                if self.vals.len() > 200 {
                    self.vals.remove(0); // 200-slot ring, oldest evicted first
                }
                self.z()
            }
            fn z(&self) -> f64 {
                if self.vals.len() < 2 {
                    return 0.0;
                }
                let mean = self.vals.iter().sum::<f64>() / self.vals.len() as f64;
                let var = self.vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / self.vals.len() as f64;
                let std = var.sqrt();
                if std < 1e-12 { 0.0 } else { (self.vals.last().unwrap() - mean) / std }
            }
        }
        let mut old = OldRing { vals: seed.iter().map(|&(_, r)| r).collect() };
        let mut old_z = old.z();
        for k in 0..TICKS {
            let drift = last_rate + 1.6e-8 * k as f64;
            old_z = old.push(drift);
        }
        assert_eq!(old.vals.len(), 200, "the pre-fix ring fills to its 200-slot cap on drift alone");
        assert!(
            (old_z - settled_z).abs() > 1.5,
            "the pre-fix ring should have drifted well away from the settled z by now: old {old_z} settled {settled_z}"
        );
    }

    // ── Degenerate-variance guard ───────────────────────────────────────────────────────────
    //
    // Calibration evidence (backtest_cache.db, 2026-08-10, 326 cached markets): before this
    // guard, only 4 markets read |z|>2.5 and the ring's raw std<1e-12 check suppressed 74
    // markets outright (mostly exact-zero std). After: still exactly 4 markets keep |z|>2.5
    // (xyz:AMZN -4.48, VIRTUAL -3.76, XMR +3.33, APT -2.94 — none of them newly caught by this
    // guard), and suppression widens to 79/326 (24%) — the same 74 std-near-zero markets plus 5
    // more (GRAM, MET, MOODENG, SYRUP, kFLOKI) whose ring isn't literally pinned but varies only
    // as noise around a baseline 20-70x larger (ratios 0.015-0.036, all under
    // FUNDING_STD_REL_FLOOR). None of the 5 newly-suppressed markets was among the 4 that ever
    // read |z|>2.5 — nothing that looked like a real signal under the old guard is touched by
    // the new one.

    #[test]
    fn funding_z_pinned_ring_yields_no_signal() {
        // A market sitting at an exact funding-rate-cap value for 30 distinct settled hours —
        // the literal shape backtest_cache.db shows for most of the 74 std-near-zero markets
        // this guard suppresses (e.g. AZTEC, BANANA, BIGTIME: every cached row is bit-for-bit
        // 1.25e-05).
        let mut eng = FeatureEngine::new();
        let base = 1_700_002_800_000i64;
        let seed: Vec<(i64, f64)> = (0..30i64).map(|i| (base + i * FUNDING_HOUR_MS, 1.25e-5)).collect();
        eng.seed_funding_history("BTC", &seed);
        assert_eq!(
            eng.funding_hist_len("BTC"),
            Some(30),
            "30 distinct settlement hours, even at one identical rate, still ring 30 samples"
        );
        let z = eng.markets.get("BTC").unwrap().funding_z;
        assert!(z.abs() < 1e-12, "a pinned ring (std = 0) must read 0.0, not blow up on the next identical print, got {z}");
        // Pushing yet another identical hour keeps it at 0.0 — nothing to divide by, ever.
        let z2 = eng.funding_z("BTC", 1.25e-5, base + 30 * FUNDING_HOUR_MS);
        assert!(z2.abs() < 1e-12, "still pinned after another hour, got {z2}");
    }

    #[test]
    fn funding_z_near_pinned_boundary_std_over_mean_ratio() {
        // Two rings straddling FUNDING_STD_REL_FLOOR (0.05) by construction: an
        // equally-weighted two-point population (mean +/- std, balanced count) has EXACT
        // population mean and std, so the ratio std/|mean| is exact too — no hand-waving needed
        // to land on either side of the boundary. Both rings clear MIN_FUNDING_SAMPLES (n=30)
        // and FUNDING_STD_ABS_FLOOR (std ~5e-7, far above 1e-9), isolating the
        // relative-dispersion condition specifically.
        let base = 1_700_002_800_000i64;
        let mean = 1e-5_f64;
        let build = |ratio: f64| -> f64 {
            let std = mean * ratio;
            let mut eng = FeatureEngine::new();
            let seed: Vec<(i64, f64)> = (0..30i64)
                .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { mean + std } else { mean - std }))
                .collect();
            eng.seed_funding_history("BTC", &seed);
            eng.markets.get("BTC").unwrap().funding_z
        };

        let z_under = build(0.0499); // ratio 4.99% — just under the 5% floor
        assert!(z_under.abs() < 1e-12, "ratio 0.0499 < FUNDING_STD_REL_FLOOR must suppress to 0.0, got {z_under}");

        let z_over = build(0.0501); // ratio 5.01% — just over the 5% floor
        // Last (odd-indexed) sample is mean - std -> z = -1.0 exactly, the same two-point
        // construction used elsewhere in this file.
        assert!(
            (z_over - (-1.0)).abs() < 1e-9,
            "ratio 0.0501 > FUNDING_STD_REL_FLOOR must NOT suppress, expected z=-1.0, got {z_over}"
        );
    }

    #[test]
    fn funding_z_tsla_shaped_dislocation_survives_the_guard() {
        // Real xyz:TSLA distribution off backtest_cache.db (2026-08-10, 118 cached hourly
        // rows): mean +5.189736e-06, std 9.580771e-06 — std EXCEEDS the mean (ratio ~1.85),
        // nowhere near either suppression floor. Reproduced here as an alternating two-point
        // series (mean +/- std, balanced count) rather than pasting the 118 raw rows: an
        // equally-weighted two-point population has EXACT mean/std equal to the center/
        // amplitude, so this hits the real market's summary statistics exactly and
        // hand-verifiably.
        let mut eng = FeatureEngine::new();
        let base = 1_700_002_800_000i64;
        let mean = 5.189736e-6_f64;
        let std = 9.580771e-6_f64;
        let seed: Vec<(i64, f64)> = (0..118i64)
            .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { mean + std } else { mean - std }))
            .collect();
        eng.seed_funding_history("BTC", &seed);

        // The live daemon's ctx stream reported exactly this on 2026-08-10 — a genuine funding
        // dislocation, not noise — reading z = -7.86 live. Pushed here as the 119th (new-hour)
        // sample against the reconstructed history above.
        let z = eng.funding_z("BTC", -1.040e-4, base + 118 * FUNDING_HOUR_MS);
        assert!(z.abs() > 5.0, "a genuine dislocation must keep a large |z|, got {z}");
        // Independently hand/script-computed from this exact construction: -7.847332044498539.
        // Close to, but not bit-identical to, the live daemon's own -7.86 — this reconstruction
        // is an idealized two-point stand-in for TSLA's real 118-row history, not a byte-for-
        // byte replay of it.
        assert!((z - (-7.847332044498539)).abs() < 1e-9, "expected the reconstructed TSLA z, got {z}");
    }

    #[test]
    fn funding_z_below_minimum_samples_is_zero_despite_real_dispersion() {
        // Textbook non-degenerate dispersion (mean 0, std 1 exactly — as clean a signal as a
        // ring can produce) but only 10 settled hours: MIN_FUNDING_SAMPLES (24) is what
        // suppresses this, not the std/rel floors, which would happily pass n=10's numbers if
        // sample count didn't gate first.
        let mut eng = FeatureEngine::new();
        let base = 1_700_002_800_000i64;
        let seed: Vec<(i64, f64)> = (0..10i64)
            .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { 1.0 } else { -1.0 }))
            .collect();
        eng.seed_funding_history("BTC", &seed);
        assert_eq!(eng.funding_hist_len("BTC"), Some(10));
        let z = eng.markets.get("BTC").unwrap().funding_z;
        assert!(z.abs() < 1e-12, "n=10 < MIN_FUNDING_SAMPLES must suppress even textbook dispersion, got {z}");
    }

    #[test]
    fn funding_z_zero_mean_dispersion_is_not_suppressed_by_relative_floor() {
        // A market whose funding genuinely oscillates around ~zero makes std/|mean| explode or
        // divide by zero — FUNDING_MEAN_EPS exists exactly so this case is judged by the
        // absolute floor alone, not incorrectly flagged as "noise around a huge baseline".
        let mut eng = FeatureEngine::new();
        let base = 1_700_002_800_000i64;
        let seed: Vec<(i64, f64)> = (0..30i64)
            .map(|i| (base + i * FUNDING_HOUR_MS, if i % 2 == 0 { 2e-6 } else { -2e-6 }))
            .collect();
        eng.seed_funding_history("BTC", &seed);
        let z = eng.markets.get("BTC").unwrap().funding_z;
        // mean is exactly 0.0, std is exactly 2e-6 (far above FUNDING_STD_ABS_FLOOR) — real
        // dispersion, must NOT be suppressed. Last (odd-indexed) sample is -2e-6 -> z = -1.0.
        assert!((z - (-1.0)).abs() < 1e-9, "zero-mean dispersion must not be suppressed, expected z=-1.0, got {z}");
    }

    #[test]
    fn funding_z_ceiling_clamps_an_unreachable_raw_z() {
        // FUNDING_Z_CEILING's own doc comment proves the ring path can never actually reach it
        // (Samuelson's inequality bounds a self-inclusive population z at sqrt(n-1), and n caps
        // at 200 -> sqrt(199) ~= 14.11, always under the 20.0 ceiling). So the only way to
        // exercise the clamp branch at all is to call the pure scoring function directly with a
        // (current, mean, std) triple no real ring could ever produce.
        assert!((funding_z_score(1_000.0, 0.0, 1.0) - FUNDING_Z_CEILING).abs() < 1e-12);
        assert!((funding_z_score(-1_000.0, 0.0, 1.0) - (-FUNDING_Z_CEILING)).abs() < 1e-12);
        // A value just inside the ceiling passes through unclamped.
        let just_under = funding_z_score(19.0, 0.0, 1.0);
        assert!((just_under - 19.0).abs() < 1e-12, "a raw z under the ceiling must pass through unchanged, got {just_under}");
    }

    #[test]
    fn atr_pct_none_when_insufficient_candles() {
        // Need period+1 candles; period 14 needs 15
        let candles: Vec<crate::hl_rest::Candle> = (0..10)
            .map(|i| crate::hl_rest::Candle {
                t: i * 900_000,
                T: i * 900_000 + 900_000,
                s: "BTC".to_string(),
                i: "15m".to_string(),
                o: 100.0,
                c: 100.0,
                h: 101.0,
                l: 99.0,
                v: 1000.0,
                n: 10,
            })
            .collect();
        assert!(atr_pct(&candles, 14).is_none());
        // Edge: exactly period candles (14) -> still None (needs 15)
        let mut fourteen = candles.clone();
        fourteen.extend((10..14).map(|i| crate::hl_rest::Candle {
            t: i * 900_000,
            T: i * 900_000 + 900_000,
            s: "BTC".to_string(),
            i: "15m".to_string(),
            o: 100.0,
            c: 100.0,
            h: 101.0,
            l: 99.0,
            v: 1000.0,
            n: 10,
        }));
        assert_eq!(fourteen.len(), 14);
        assert!(atr_pct(&fourteen, 14).is_none());
        // period 0 -> None
        assert!(atr_pct(&candles, 0).is_none());
    }

    #[test]
    fn atr_pct_hand_computed_wilder_synthetic() {
        // Synthetic 5 candles, period 2, hand-computed Wilder ATR
        // C0: h10 l10 c10  (seed, no TR)
        // C1: h12 l8 c11 => TR1 = max(4, |12-10|=2, |8-10|=2)=4
        // C2: h13 l11 c12 => TR2 = max(2, |13-11|=2, |11-11|=0)=2
        // C3: h14 l10 c13 => TR3 = max(4, |14-12|=2, |10-12|=2)=4
        // C4: h15 l13 c14 => TR4 = max(2, |15-13|=2, |13-13|=0)=2
        // TRs = [4,2,4,2]; period2: ATR1=(4+2)/2=3.0; ATR2=(3*1+4)/2=3.5; ATR3=(3.5*1+2)/2=2.75
        // atr_pct = 2.75/14*100 = 19.642857142857146
        let candles = vec![
            crate::hl_rest::Candle { t: 0, T: 1, s: "BTC".into(), i: "15m".into(), o: 10.0, c: 10.0, h: 10.0, l: 10.0, v: 100.0, n: 1 },
            crate::hl_rest::Candle { t: 1, T: 2, s: "BTC".into(), i: "15m".into(), o: 11.0, c: 11.0, h: 12.0, l: 8.0, v: 100.0, n: 1 },
            crate::hl_rest::Candle { t: 2, T: 3, s: "BTC".into(), i: "15m".into(), o: 12.0, c: 12.0, h: 13.0, l: 11.0, v: 100.0, n: 1 },
            crate::hl_rest::Candle { t: 3, T: 4, s: "BTC".into(), i: "15m".into(), o: 13.0, c: 13.0, h: 14.0, l: 10.0, v: 100.0, n: 1 },
            crate::hl_rest::Candle { t: 4, T: 5, s: "BTC".into(), i: "15m".into(), o: 14.0, c: 14.0, h: 15.0, l: 13.0, v: 100.0, n: 1 },
        ];
        let pct = atr_pct(&candles, 2).expect("atr");
        let expected = 2.75 / 14.0 * 100.0;
        assert!((pct - expected).abs() < 1e-9, "atr_pct {pct} expected {expected}");
        // Constant TR case: all TR=2 -> ATR=2, last close 14? Actually use uniform
        let uniform: Vec<crate::hl_rest::Candle> = (0..5)
            .map(|i| crate::hl_rest::Candle {
                t: i,
                T: i + 1,
                s: "BTC".into(),
                i: "15m".into(),
                o: 10.0 + i as f64,
                c: 10.0 + i as f64,
                h: 11.0 + i as f64,
                l: 9.0 + i as f64,
                v: 100.0,
                n: 1,
            })
            .collect();
        // TR for uniform shift 1 per candle: high-low=2, but high-prev close: (11+i)-(10+i-1)=2, low-prev close: (9+i)-(10+i-1)= -2 abs2 => TR=2 always
        let pct2 = atr_pct(&uniform, 2).expect("uniform atr");
        // With 5 candles (4 TRs), period2: ATR stable 2, last close 14 => 14.285714...
        assert!((pct2 - (2.0 / 14.0 * 100.0)).abs() < 1e-9);
    }

    #[test]
    fn ema_period_one_is_identity_and_empty_inputs_are_empty() {
        let closes = [3.0, 1.5, 4.25, -2.0];
        assert_eq!(ema(&closes, 1), closes.to_vec(), "k=1 tracks the series exactly");
        assert!(ema(&closes, 0).is_empty());
        assert!(ema(&[], 20).is_empty());
    }

    #[test]
    fn ema_hand_computed_tiny_series() {
        // period 3 -> k = 2/(3+1) = 0.5, seeded with the first close:
        // [10, 12, 14] -> [10, 0.5*12+0.5*10=11, 0.5*14+0.5*11=12.5]
        let out = ema(&[10.0, 12.0, 14.0], 3);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 10.0).abs() < 1e-12);
        assert!((out[1] - 11.0).abs() < 1e-12);
        assert!((out[2] - 12.5).abs() < 1e-12);
    }

    #[test]
    fn rsi_strictly_rising_series_is_100() {
        let closes: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        let out = rsi(&closes, 7);
        assert_eq!(out.len(), closes.len());
        assert!(
            out[..7].iter().all(|v| v.is_none()),
            "the first `period` slots are warm-up"
        );
        for v in &out[7..] {
            assert!(
                (v.unwrap() - 100.0).abs() < 1e-12,
                "zero losses -> RSI 100, got {v:?}"
            );
        }
    }

    #[test]
    fn rsi_hand_computed_tiny_series_and_insufficient_history() {
        // period 2 over [10, 11, 10, 12]: deltas +1, -1, +2.
        // seed window: avg_gain = 1/2, avg_loss = 1/2 -> RS 1 -> RSI 50 at idx 2.
        // idx 3 (Wilder): avg_gain = (0.5*1 + 2)/2 = 1.25, avg_loss = (0.5*1 + 0)/2 = 0.25
        // -> RS 5 -> 100 - 100/6 = 83.333...
        let out = rsi(&[10.0, 11.0, 10.0, 12.0], 2);
        assert_eq!(out.len(), 4);
        assert!(out[0].is_none() && out[1].is_none());
        assert!((out[2].unwrap() - 50.0).abs() < 1e-12);
        assert!((out[3].unwrap() - (100.0 - 100.0 / 6.0)).abs() < 1e-12);
        // graceful None/empty on insufficient history
        assert!(rsi(&[1.0, 2.0], 14).is_empty(), "period+1 closes minimum");
        assert!(rsi(&[1.0, 2.0], 0).is_empty());
        assert!(rsi(&[], 7).is_empty());
    }

    #[test]
    fn macd_hist_constant_series_is_zero_and_empty_is_empty() {
        // Constant series: every EMA is the constant, macd line 0, signal 0 -> hist 0.
        let flat = vec![50.0; 40];
        let hist = macd_hist(&flat);
        assert_eq!(hist.len(), 40);
        assert!(
            hist.iter().all(|h| h.abs() < 1e-12),
            "constant series -> zero histogram"
        );
        assert!(macd_hist(&[]).is_empty());
    }

    #[test]
    fn macd_hist_ramp_matches_component_emas() {
        // Rising ramp: the fast EMA sits above the slow one, so after warm-up the histogram
        // is positive; the last value is hand-checkable straight from the ema() definition.
        let ramp: Vec<f64> = (0..40).map(|i| 100.0 + i as f64).collect();
        let hist = macd_hist(&ramp);
        assert_eq!(hist.len(), 40);
        let fast = ema(&ramp, 12);
        let slow = ema(&ramp, 26);
        let line: Vec<f64> = fast.iter().zip(slow.iter()).map(|(f, s)| f - s).collect();
        let signal = ema(&line, 9);
        let expected_last = line[39] - signal[39];
        assert!((hist[39] - expected_last).abs() < 1e-12);
        assert!(hist[39] > 0.0, "steady ramp -> positive macd histogram");
    }
}
