//! Chunked, resumable backfill of 1m candles + hourly funding into `backtest_cache.db`
//! (spec Decision 3).
//!
//! Three rules the whole module exists to keep:
//!
//!   * **≤5000 candles per request** — Hyperliquid's `candleSnapshot` cap. Windows are cut
//!     to exactly that many minutes ([`chunk_windows`]), so a range longer than ~3.5 days
//!     can never be silently truncated by the venue.
//!   * **Cache-first** — only the sub-ranges the cache does not already cover are fetched
//!     ([`subtract_ranges`]). A rerun over an already-backfilled window issues zero requests.
//!   * **Throttled** — the same pacing as the daemon's fundingHistory boot seed
//!     (`main::boot_seed_funding`): 100 ms between calls, 1 s every 20.

use std::time::Duration;

use tracing::{info, warn};

use crate::hl_rest::HlRest;

use super::cache::{Cache, Series};

pub const MINUTE_MS: i64 = 60_000;
pub const HOUR_MS: i64 = 3_600_000;

/// Hyperliquid returns at most this many candles per `candleSnapshot` request.
pub const MAX_CANDLES_PER_REQUEST: i64 = 5000;

/// The only interval the harness caches: 1m is the finest HL serves and the cadence the
/// replay loop steps at (spec Decision 6).
pub const INTERVAL: &str = "1m";

/// Funding history window per request. Hourly samples, so 10 days is 240 rows — far under
/// any server-side row cap, and a failed chunk costs one cheap retry.
pub const FUNDING_CHUNK_MS: i64 = 10 * 24 * HOUR_MS;

/// Throttle, copied from `main::boot_seed_funding` so the harness is exactly as polite to
/// the venue as the daemon's boot seed.
pub const THROTTLE_MS: u64 = 100;
pub const THROTTLE_PAUSE_EVERY: usize = 20;
pub const THROTTLE_PAUSE_MS: u64 = 1000;

/// Coalesce `ranges` into sorted, disjoint, inclusive ranges. Two ranges closer than one
/// `step` are contiguous (a day ending 23:59 and the next starting 00:00 are one range), so
/// day-by-day backfills accumulate into a single island rather than a row per run.
pub fn merge_ranges(mut ranges: Vec<(i64, i64)>, step: i64) -> Vec<(i64, i64)> {
    ranges.retain(|(a, b)| a <= b);
    ranges.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(ranges.len());
    for (a, b) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1.saturating_add(step) => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The parts of the inclusive window `[from_ms, to_ms]` that `covered` does not already
/// contain. `covered` must be sorted and disjoint — [`merge_ranges`] output, which is what
/// the cache stores.
///
/// COVERAGE IS WHAT WAS ASKED FOR, NOT WHAT CAME BACK. Two venue behaviours make anything
/// inferred from the rows themselves wrong:
///
///   * a minute with no trades has no candle, so per-minute presence would mark real data as
///     a gap and refetch it forever;
///   * Hyperliquid keeps only ~3.6 days of 1m history (measured 2026-08-09: a 2000-candle
///     window six days back returns zero rows), so an older range answers *empty forever* —
///     a fact worth remembering exactly once.
///
/// A range is recorded only after the venue answers for it, so a failed chunk leaves its
/// range uncovered and the next run resumes there.
pub fn subtract_ranges(from_ms: i64, to_ms: i64, covered: &[(i64, i64)], step: i64) -> Vec<(i64, i64)> {
    if to_ms < from_ms {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = from_ms;
    for &(a, b) in covered {
        if b < cur {
            continue;
        }
        if a > to_ms {
            break;
        }
        if a > cur {
            let end = (a - step).min(to_ms);
            if cur <= end {
                out.push((cur, end));
            }
        }
        cur = b.saturating_add(step);
        if cur > to_ms {
            return out;
        }
    }
    if cur <= to_ms {
        out.push((cur, to_ms));
    }
    out
}

/// Cut `[from_ms, to_ms]` (inclusive, minute-aligned) into request windows of at most
/// `span_ms` each. Windows are inclusive on both ends and tile the range with exactly one
/// minute between them — the same granularity coverage is merged at, so a partially fetched
/// range records an exact, minute-aligned prefix.
pub fn windows(from_ms: i64, to_ms: i64, span_ms: i64) -> Vec<(i64, i64)> {
    if to_ms < from_ms {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = from_ms;
    while start <= to_ms {
        let end = (start + span_ms - MINUTE_MS).min(to_ms);
        out.push((start, end));
        start = end + MINUTE_MS;
    }
    out
}

/// Candle request windows: at most [`MAX_CANDLES_PER_REQUEST`] 1m candles each, so the
/// venue's row cap is never the thing that decides where the data stops.
pub fn chunk_windows(from_ms: i64, to_ms: i64) -> Vec<(i64, i64)> {
    windows(from_ms, to_ms, MAX_CANDLES_PER_REQUEST * MINUTE_MS)
}

/// Funding request windows — hourly samples, so these are far coarser.
pub fn funding_windows(from_ms: i64, to_ms: i64) -> Vec<(i64, i64)> {
    windows(from_ms, to_ms, FUNDING_CHUNK_MS)
}

/// What one market's backfill did. `error` is set when the market stopped early — the rest
/// of the run continues, and the next run resumes from the contiguous span already cached.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketStat {
    pub market: String,
    pub candles_cached: i64,
    pub candles_fetched: u64,
    pub funding_cached: i64,
    pub funding_fetched: u64,
    pub requests: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FetchSummary {
    pub markets: Vec<MarketStat>,
    pub requests: usize,
    pub failed: usize,
}

impl FetchSummary {
    pub fn candles_fetched(&self) -> u64 {
        self.markets.iter().map(|m| m.candles_fetched).sum()
    }

    pub fn candles_cached(&self) -> i64 {
        self.markets.iter().map(|m| m.candles_cached).sum()
    }

    pub fn funding_fetched(&self) -> u64 {
        self.markets.iter().map(|m| m.funding_fetched).sum()
    }
}

/// Request pacing shared by every call the backfill makes.
struct Throttle {
    calls: usize,
}

impl Throttle {
    fn new() -> Self {
        Self { calls: 0 }
    }

    async fn pace(&mut self) {
        if self.calls > 0 {
            if self.calls.is_multiple_of(THROTTLE_PAUSE_EVERY) {
                tokio::time::sleep(Duration::from_millis(THROTTLE_PAUSE_MS)).await;
            }
            tokio::time::sleep(Duration::from_millis(THROTTLE_MS)).await;
        }
        self.calls += 1;
    }
}

/// Backfill `markets` over the inclusive window `[from_ms, to_ms]`, cache-first.
///
/// Markets are walked in the order given (the CLI sorts them, so a run is deterministic) and
/// each market's chunks chronologically. A failed chunk ends that market for this run — the
/// cached span therefore stays contiguous, which is what makes [`missing_ranges`] a correct
/// resume point.
pub async fn backfill(
    hl: &HlRest,
    cache: &Cache,
    markets: &[String],
    from_ms: i64,
    to_ms: i64,
) -> anyhow::Result<FetchSummary> {
    let mut throttle = Throttle::new();
    let mut summary = FetchSummary::default();
    let total = markets.len();

    for (idx, market) in markets.iter().enumerate() {
        let mut stat = MarketStat {
            market: market.clone(),
            candles_cached: cache.count_candles(market, from_ms, to_ms).await?,
            candles_fetched: 0,
            funding_cached: cache.count_funding(market, from_ms, to_ms).await?,
            funding_fetched: 0,
            requests: 0,
            error: None,
        };

        let covered = cache.coverage(market, Series::Candles).await?;
        'candles: for (range_from, range_to) in subtract_ranges(from_ms, to_ms, &covered, MINUTE_MS) {
            let mut done_through = None;
            for (start, end) in chunk_windows(range_from, range_to) {
                throttle.pace().await;
                stat.requests += 1;
                match hl.candle_snapshot(market, INTERVAL, start, end).await {
                    Ok(rows) => {
                        stat.candles_fetched += cache.put_candles(market, &rows).await?;
                        done_through = Some(end);
                    }
                    Err(e) => {
                        warn!(market = %market, start, end, error = ?e, "candle chunk failed — stopping this market");
                        stat.error = Some(format!("candles {start}..{end}: {e}"));
                        break;
                    }
                }
            }
            // Only the prefix the venue actually answered for is covered; the rest is the
            // next run's work.
            if let Some(end) = done_through {
                cache.add_coverage(market, Series::Candles, range_from, end, MINUTE_MS).await?;
            }
            if stat.error.is_some() {
                break 'candles;
            }
        }

        if stat.error.is_none() {
            let fcovered = cache.coverage(market, Series::Funding).await?;
            'funding: for (range_from, range_to) in subtract_ranges(from_ms, to_ms, &fcovered, MINUTE_MS) {
                let mut done_through = None;
                for (start, end) in funding_windows(range_from, range_to) {
                    throttle.pace().await;
                    stat.requests += 1;
                    match hl.funding_history(market, start, end).await {
                        Ok(rows) => {
                            stat.funding_fetched += cache.put_funding(market, &rows).await?;
                            done_through = Some(end);
                        }
                        Err(e) => {
                            warn!(market = %market, start, end, error = ?e, "funding chunk failed — stopping this market");
                            stat.error = Some(format!("funding {start}..{end}: {e}"));
                            break;
                        }
                    }
                }
                if let Some(end) = done_through {
                    cache.add_coverage(market, Series::Funding, range_from, end, MINUTE_MS).await?;
                }
                if stat.error.is_some() {
                    break 'funding;
                }
            }
        }

        if stat.error.is_some() {
            summary.failed += 1;
        }
        let span = cache.candle_span(market).await?;
        info!(
            progress = format!("{}/{}", idx + 1, total),
            market = %market,
            cached = stat.candles_cached,
            fetched = stat.candles_fetched,
            funding = stat.funding_fetched,
            requests = stat.requests,
            span = ?span,
            "backtest backfill market done"
        );
        summary.requests += stat.requests;
        summary.markets.push(stat);
    }

    info!(
        markets = total,
        requests = summary.requests,
        candles_fetched = summary.candles_fetched(),
        funding_fetched = summary.funding_fetched(),
        failed = summary.failed,
        "backtest backfill done"
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = MINUTE_MS;

    #[test]
    fn chunk_windows_caps_at_5000_candles_and_tiles_without_overlap() {
        // Exactly 5000 minutes -> one request at the cap.
        let w = chunk_windows(0, 4999 * M);
        assert_eq!(w.len(), 1, "5000 candles is one request");
        assert_eq!(w[0], (0, 4999 * M));
        assert_eq!((w[0].1 - w[0].0) / M + 1, MAX_CANDLES_PER_REQUEST);

        // One minute past the cap splits, and the split is contiguous (no minute lost, none
        // requested twice).
        let w = chunk_windows(0, 5000 * M);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], (0, 4999 * M));
        assert_eq!(w[1], (5000 * M, 5000 * M));
        assert_eq!(w[1].0 - w[0].1, M, "windows tile, exactly one minute apart");

        // A week of 1m candles: ceil(10080/5000) = 3 requests, all within the cap, covering
        // the whole range end to end.
        let week = chunk_windows(0, (7 * 1440 - 1) * M);
        assert_eq!(week.len(), 3);
        assert!(week.iter().all(|(a, b)| (b - a) / M < MAX_CANDLES_PER_REQUEST), "each window holds ≤5000 candles");
        assert_eq!(week[0].0, 0);
        assert_eq!(week.last().unwrap().1, (7 * 1440 - 1) * M);
        let covered: i64 = week.iter().map(|(a, b)| (b - a) / M + 1).sum();
        assert_eq!(covered, 7 * 1440);

        // Degenerate windows.
        assert_eq!(chunk_windows(5 * M, 5 * M), vec![(5 * M, 5 * M)], "single minute");
        assert!(chunk_windows(10 * M, 9 * M).is_empty(), "inverted range fetches nothing");
    }

    #[test]
    fn subtract_ranges_is_the_whole_window_when_nothing_is_covered() {
        assert_eq!(subtract_ranges(0, 100 * M, &[], M), vec![(0, 100 * M)]);
    }

    #[test]
    fn subtract_ranges_is_empty_when_the_window_is_already_covered() {
        // THE RERUN CASE: a second `backtest fetch` over the same days issues zero requests.
        assert!(subtract_ranges(10 * M, 20 * M, &[(10 * M, 20 * M)], M).is_empty());
        assert!(subtract_ranges(10 * M, 20 * M, &[(0, 100 * M)], M).is_empty());
        // Including when the covered range answered EMPTY (older than the venue's 1m
        // retention): the emptiness is remembered, not re-probed.
        assert!(subtract_ranges(0, 5000 * M, &[(0, 5000 * M)], M).is_empty());
    }

    #[test]
    fn subtract_ranges_extends_head_tail_and_both() {
        assert_eq!(subtract_ranges(0, 20 * M, &[(0, 10 * M)], M), vec![(11 * M, 20 * M)], "tail");
        assert_eq!(subtract_ranges(0, 20 * M, &[(10 * M, 20 * M)], M), vec![(0, 9 * M)], "head");
        assert_eq!(
            subtract_ranges(0, 20 * M, &[(5 * M, 15 * M)], M),
            vec![(0, 4 * M), (16 * M, 20 * M)],
            "both ends of a covered island"
        );
    }

    #[test]
    fn subtract_ranges_handles_multiple_islands_and_disjoint_requests() {
        // Two covered islands leave exactly the hole between them.
        assert_eq!(
            subtract_ranges(0, 30 * M, &[(0, 10 * M), (20 * M, 30 * M)], M),
            vec![(11 * M, 19 * M)]
        );
        // A request entirely below/above the covered island fetches exactly itself.
        assert_eq!(subtract_ranges(0, 10 * M, &[(50 * M, 60 * M)], M), vec![(0, 10 * M)]);
        assert_eq!(subtract_ranges(100 * M, 110 * M, &[(50 * M, 60 * M)], M), vec![(100 * M, 110 * M)]);
        assert!(subtract_ranges(10 * M, 9 * M, &[], M).is_empty(), "inverted window");
    }

    #[test]
    fn merge_ranges_coalesces_adjacent_days_into_one_island() {
        let day = 1440 * M;
        // Two consecutive days backfilled on two separate runs are one range, not two.
        let merged = merge_ranges(vec![(0, day - M), (day, 2 * day - M)], M);
        assert_eq!(merged, vec![(0, 2 * day - M)]);
        // Overlapping and out-of-order input.
        assert_eq!(merge_ranges(vec![(10 * M, 20 * M), (0, 15 * M)], M), vec![(0, 20 * M)]);
        // A genuine gap stays two islands.
        assert_eq!(
            merge_ranges(vec![(0, 10 * M), (20 * M, 30 * M)], M),
            vec![(0, 10 * M), (20 * M, 30 * M)]
        );
        // Nested and degenerate input.
        assert_eq!(merge_ranges(vec![(0, 30 * M), (10 * M, 20 * M)], M), vec![(0, 30 * M)]);
        assert!(merge_ranges(vec![(10 * M, 9 * M)], M).is_empty(), "inverted ranges are dropped");
        assert!(merge_ranges(vec![], M).is_empty());
    }

    #[test]
    fn coverage_round_trip_leaves_nothing_to_fetch() {
        // The property the whole module rests on: subtract, fetch, record, subtract again.
        let day = 1440 * M;
        let (from, to) = (0, day - M);
        let first = subtract_ranges(from, to, &[], M);
        assert_eq!(first, vec![(from, to)]);
        let covered = merge_ranges(first, M);
        assert!(subtract_ranges(from, to, &covered, M).is_empty(), "rerun fetches nothing");
        // Extending forward by a day asks only for the new day.
        assert_eq!(subtract_ranges(from, 2 * day - M, &covered, M), vec![(day, 2 * day - M)]);
    }

    #[test]
    fn funding_windows_chunk_by_ten_days_on_the_same_minute_grid() {
        let day = 24 * HOUR_MS;
        let w = funding_windows(0, 30 * day);
        assert_eq!(w.len(), 4, "30 days -> 3 full chunks + remainder");
        assert!(w.iter().all(|(a, b)| b - a < FUNDING_CHUNK_MS));
        assert_eq!(w[0].0, 0);
        assert_eq!(w.last().unwrap().1, 30 * day);
        for pair in w.windows(2) {
            // One MINUTE apart, not one millisecond: coverage merges on the minute grid, so a
            // partially fetched funding range must record a minute-aligned prefix.
            assert_eq!(pair[1].0 - pair[0].1, M, "windows tile with no overlap");
        }
        assert!(funding_windows(10 * M, 9 * M).is_empty());
        // Consecutive windows are contiguous under the same merge rule the cache uses.
        assert_eq!(merge_ranges(w, M).len(), 1, "the chunks of one range merge back into it");
    }
}
