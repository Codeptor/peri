//! `kestreld backtest stats` — a read-only distribution report over the cache (batch B4).
//!
//! Unlike [`super::engine::run`]/[`super::sweep`] this is NOT a strategy replay: no gates, no
//! sizing decisions, no fills, no pnl. It walks the same 1m tape the replay engine does (same
//! [`engine::rows_at`], same feature recomputation, same funding ring) and reports what the
//! cached window's price/funding/signal DATA looks like — the numbers a knob's calibration is
//! actually argued from, rather than assumed. Two concrete uses this was built for:
//!
//!   * `regime_vol_max` is currently 1.5; the first sweeps (`docs/backtests/2026-08-09-first-
//!     sweeps`) measured BTC `vol1h` peaking at 0.079% over a 3-day window — nowhere near the
//!     threshold. `btc_vol1h` below is the percentile table that number came from, over
//!     whatever window is cached when this runs.
//!   * `screener.min_score` is currently 1.8; `score` below is the SAME cross-sectional scoring
//!     formula (`screener::screen`), run with an open floor and top_k so every eligible
//!     market-minute is pooled, then bucketed against the real threshold.
//!
//! Score and funding_z are computed with the REAL `screener::screen` / `features::funding_z_at`
//! — nothing here re-derives a formula the strategy modules already own.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::config::{Config, ScreenerCfg};
use crate::screener::screen;
use crate::sizing::size_position;

use super::engine::{self, MarketData};
use super::features::funding_z_at;
use super::fills;
use super::MINUTE_MS;

/// How many markets the per-market vol1h table keeps, ranked by the volume proxy — enough to
/// see the head of the universe without turning the report into a 300-row dump.
pub const TOP_MARKETS_BY_VOLUME: usize = 15;

#[derive(Debug, clap::Args)]
pub struct StatsArgs {
    /// First UTC day to analyze, `YYYY-MM-DD` (inclusive, from 00:00Z).
    #[arg(long)]
    pub from: String,
    /// Last UTC day to analyze, `YYYY-MM-DD` (inclusive, through 23:59Z).
    #[arg(long)]
    pub to: String,
    /// Markets to analyze, comma separated. Defaults to everything the cache holds.
    #[arg(long, value_delimiter = ',')]
    pub markets: Option<Vec<String>>,
    /// Directory the report is written under.
    #[arg(long, default_value = super::REPORT_DIR_DEFAULT)]
    pub out: String,
    /// Name of the report directory, after the date. Defaults to the window.
    #[arg(long)]
    pub label: Option<String>,
}

/// A distribution's tail — enough percentiles to argue about a threshold with. Linear
/// interpolation between ranks (numpy's default `'linear'` method), so a number quoted here
/// reproduces in any spreadsheet or notebook someone re-derives it in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub n: usize,
    pub p50: f64,
    pub p75: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

/// Percentiles of `vals` (sorted internally — callers never need to sort first). `None` for an
/// empty input: a market with zero eligible minutes has no distribution to report, not a
/// distribution of zero.
pub fn percentiles(mut vals: Vec<f64>) -> Option<Percentiles> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let at = |p: f64| -> f64 {
        if vals.len() == 1 {
            return vals[0];
        }
        let idx = p * (vals.len() - 1) as f64;
        let lo = idx.floor() as usize;
        let hi = idx.ceil() as usize;
        if lo == hi {
            vals[lo]
        } else {
            vals[lo] + (vals[hi] - vals[lo]) * (idx - lo as f64)
        }
    };
    Some(Percentiles {
        n: vals.len(),
        p50: at(0.50),
        p75: at(0.75),
        p90: at(0.90),
        p95: at(0.95),
        p99: at(0.99),
        max: *vals.last().expect("non-empty"),
    })
}

/// One market's vol1h distribution, ranked into the report by `avg_day_ntl_vlm`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketVol {
    pub market: String,
    /// Mean of the volume-proxy (`features::MarketSeries::day_ntl_vlm_at`) across the window —
    /// what markets are RANKED by (`avg_day_ntl_vlm`), not a percentile in its own right.
    pub avg_day_ntl_vlm: f64,
    pub vol1h: Percentiles,
}

/// Funding_z distribution: what fraction of market-minutes sat past each side-flip-relevant
/// threshold. `screener::screen` flips a nominee's side when `|funding_z| > 2.5` opposes
/// momentum, so that third bucket is the one that number is grounded against.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FundingZBuckets {
    pub n: usize,
    pub share_abs_gt_1_pct: f64,
    pub share_abs_gt_2_pct: f64,
    pub share_abs_gt_2_5_pct: f64,
}

fn funding_z_buckets(vals: &[f64]) -> FundingZBuckets {
    let n = vals.len();
    let share = |thresh: f64| -> f64 {
        if n == 0 {
            return 0.0;
        }
        vals.iter().filter(|v| **v > thresh).count() as f64 / n as f64 * 100.0
    };
    FundingZBuckets {
        n,
        share_abs_gt_1_pct: share(1.0),
        share_abs_gt_2_pct: share(2.0),
        share_abs_gt_2_5_pct: share(2.5),
    }
}

/// Screener score distribution, pooled across every eligible market-minute (vlm-filtered,
/// features warmed, gates and cooldowns NOT applied — this measures signal density, not what
/// the strategy would actually take).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreReport {
    pub n: usize,
    pub min_score: f64,
    pub share_above_min_score_pct: f64,
    pub percentiles: Option<Percentiles>,
}

/// Turnover/fee arithmetic at one conviction — `sizing::size_position` run at the window's
/// measured median vol1h, so the reference is grounded in this window's own data rather than an
/// assumption. `round_trip_fee_bp` is independent of notional by construction (fee is linear in
/// notional), so it is the same number at every conviction — reported per-row anyway so the
/// identity is visible rather than asserted from outside.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizingRow {
    pub conviction: f64,
    pub vol1h_used_pct: f64,
    pub leverage: f64,
    pub margin: f64,
    pub notional: f64,
    pub round_trip_fee_usd: f64,
    pub round_trip_fee_bp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub from_ms: i64,
    pub to_ms: i64,
    /// Minutes the window spans by calendar (`to_ms - from_ms) / 60_000 + 1`.
    pub requested_minutes: i64,
    /// Minutes that actually had at least one market's candle — what HL's ~3.6-day retention
    /// left in the cache, not necessarily `requested_minutes`.
    pub bars: i64,
    pub markets: usize,
    pub caveats: Vec<String>,
    pub btc_vol1h: Option<Percentiles>,
    pub universe_vol1h: Option<Percentiles>,
    /// Top [`TOP_MARKETS_BY_VOLUME`] markets by the volume proxy, ranked descending.
    pub per_market_vol1h: Vec<MarketVol>,
    pub funding_z: FundingZBuckets,
    pub score: ScoreReport,
    pub sizing_reference: Vec<SizingRow>,
}

/// The standing disclaimers for a stats report — deliberately NOT [`super::report::caveats`]:
/// this is not a strategy replay, so gate/fill/exit deviations do not apply here.
pub fn caveats(cfg: &Config, requested_minutes: i64, bars: i64, markets: usize) -> Vec<String> {
    vec![
        "distribution report only — no gates, no sizing decisions, no fills, no pnl: this is \
         what the cached price/funding tape looks like, not a strategy replay"
            .to_string(),
        format!(
            "Hyperliquid retains only ~3.6 days of 1m candles — a window older than that is \
             unobtainable, ever. Requested {requested_minutes} minute(s) across {markets} \
             market(s); {bars} of them actually had cached tape (the rest predates the venue's \
             retention or the cache has not been backfilled that far back)."
        ),
        "day_ntl_vlm is a proxy — rolling 24h candle quote volume, not the venue's own \
         dayNtlVlm (spec Decision 4) — and it is what avg_day_ntl_vlm below ranks markets by"
            .to_string(),
        "features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints \
         cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)"
            .to_string(),
        "score is screener::screen run with an open floor and top_k so every vlm-eligible, \
         feature-warmed market-minute is pooled — no gate, cooldown or exclusion is applied, so \
         this is signal DENSITY, not what the strategy would actually enter"
            .to_string(),
        "min_score is read from the config file (--config, default kestreld.toml) — this \
         subcommand takes no --set overrides, so it always reports the config as-is"
            .to_string(),
    ]
}

/// Build the report by walking the SAME per-minute rows the replay engine scores
/// ([`engine::rows_at`]) — one pass over `[from_ms, to_ms]`, no strategy state.
pub fn build(cfg: &Config, data: &BTreeMap<String, MarketData>, from_ms: i64, to_ms: i64) -> Report {
    let mut pooled_vol1h: Vec<f64> = Vec::new();
    let mut btc_vol1h: Vec<f64> = Vec::new();
    let mut per_market_vol1h: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut vlm_sum: BTreeMap<String, f64> = BTreeMap::new();
    let mut vlm_n: BTreeMap<String, u64> = BTreeMap::new();
    let mut funding_abs: Vec<f64> = Vec::new();
    let mut score_pool: Vec<f64> = Vec::new();

    // Wide open: every vlm-eligible, feature-warmed market-minute becomes a scored nominee, so
    // the pool is the RAW score distribution rather than what top_k/min_score would have kept.
    let wide_screener = ScreenerCfg {
        interval_s: cfg.screener.interval_s,
        top_k: usize::MAX,
        min_score: f64::NEG_INFINITY,
        skip_recheck_min: cfg.screener.skip_recheck_min,
        skip_recheck_score_jump: cfg.screener.skip_recheck_score_jump,
    };
    let no_excluded: HashSet<String> = HashSet::new();

    let mut bars = 0i64;
    let mut t = from_ms;
    while t <= to_ms {
        let rows = engine::rows_at(data, t);
        if !rows.is_empty() {
            bars += 1;
            for row in &rows {
                *vlm_sum.entry(row.market.clone()).or_insert(0.0) += row.day_ntl_vlm;
                *vlm_n.entry(row.market.clone()).or_insert(0) += 1;
                if let Some(d) = data.get(&row.market) {
                    funding_abs.push(funding_z_at(&d.funding_z, t).abs());
                }
                if let Some(f) = &row.features {
                    pooled_vol1h.push(f.vol1h);
                    per_market_vol1h.entry(row.market.clone()).or_default().push(f.vol1h);
                    if row.market == engine::REGIME_MARKET {
                        btc_vol1h.push(f.vol1h);
                    }
                }
            }
            for n in screen(&rows, &wide_screener, &cfg.universe, &no_excluded) {
                score_pool.push(n.score);
            }
        }
        t += MINUTE_MS;
    }

    let mut ranked: Vec<(String, f64)> = vlm_sum
        .iter()
        .map(|(m, sum)| (m.clone(), sum / vlm_n.get(m).copied().unwrap_or(1).max(1) as f64))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal).then_with(|| a.0.cmp(&b.0)));
    let per_market_vol1h_report: Vec<MarketVol> = ranked
        .into_iter()
        .take(TOP_MARKETS_BY_VOLUME)
        .filter_map(|(market, avg_vlm)| {
            let vols = per_market_vol1h.get(&market)?.clone();
            let p = percentiles(vols)?;
            Some(MarketVol { market, avg_day_ntl_vlm: avg_vlm, vol1h: p })
        })
        .collect();

    let n_score = score_pool.len();
    let share_above = if n_score == 0 {
        0.0
    } else {
        score_pool.iter().filter(|s| **s >= cfg.screener.min_score).count() as f64 / n_score as f64 * 100.0
    };
    let score_percentiles = percentiles(score_pool);
    let score = ScoreReport {
        n: n_score,
        min_score: cfg.screener.min_score,
        share_above_min_score_pct: share_above,
        percentiles: score_percentiles,
    };

    let universe_vol1h = percentiles(pooled_vol1h);
    let vol1h_used = universe_vol1h.as_ref().map(|p| p.p50).unwrap_or(0.0);
    let sizing_reference: Vec<SizingRow> = [super::DEFAULT_CONVICTION, 1.0f64]
        .into_iter()
        .map(|conviction| {
            let sized = size_position(&cfg.sizing, vol1h_used, conviction);
            let round_trip = 2.0 * fills::fee(sized.notional);
            SizingRow {
                conviction,
                vol1h_used_pct: vol1h_used,
                leverage: sized.leverage,
                margin: sized.margin,
                notional: sized.notional,
                round_trip_fee_usd: round_trip,
                round_trip_fee_bp: if sized.notional > 0.0 { round_trip / sized.notional * 10_000.0 } else { 0.0 },
            }
        })
        .collect();

    let requested_minutes = (to_ms - from_ms) / MINUTE_MS + 1;
    Report {
        from: super::report::stamp(from_ms),
        to: super::report::stamp(to_ms),
        from_ms,
        to_ms,
        requested_minutes,
        bars,
        markets: data.len(),
        caveats: caveats(cfg, requested_minutes, bars, data.len()),
        btc_vol1h: percentiles(btc_vol1h),
        universe_vol1h,
        per_market_vol1h: per_market_vol1h_report,
        funding_z: funding_z_buckets(&funding_abs),
        score,
        sizing_reference,
    }
}

/// Default label: the window alone (no conviction/axes to name, unlike run/sweep).
pub fn default_label(from: &str, to: &str) -> String {
    format!("stats-{from}_{to}")
}

fn fmt_pct(p: &Option<Percentiles>) -> String {
    match p {
        None => "no data".to_string(),
        Some(p) => format!(
            "n={} p50={:.4} p75={:.4} p90={:.4} p95={:.4} p99={:.4} max={:.4}",
            p.n, p.p50, p.p75, p.p90, p.p95, p.p99, p.max
        ),
    }
}

/// The one-screen summary the CLI prints when a stats run finishes.
pub fn human_summary(rep: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "window   {} .. {}  ({} bars of {} requested minutes, {} markets)",
        rep.from, rep.to, rep.bars, rep.requested_minutes, rep.markets
    );
    let _ = writeln!(out, "\nBTC vol1h%      {}", fmt_pct(&rep.btc_vol1h));
    let _ = writeln!(out, "universe vol1h% {}", fmt_pct(&rep.universe_vol1h));
    let _ = writeln!(
        out,
        "\nfunding_z  n={}  |z|>1 {:.2}%  |z|>2 {:.2}%  |z|>2.5 {:.2}%",
        rep.funding_z.n, rep.funding_z.share_abs_gt_1_pct, rep.funding_z.share_abs_gt_2_pct, rep.funding_z.share_abs_gt_2_5_pct
    );
    let _ = writeln!(
        out,
        "score      n={}  min_score={}  share above {:.2}%  {}",
        rep.score.n,
        rep.score.min_score,
        rep.score.share_above_min_score_pct,
        fmt_pct(&rep.score.percentiles)
    );
    let _ = writeln!(out, "\nsizing reference (round-trip fee is independent of notional by construction):");
    for r in &rep.sizing_reference {
        let _ = writeln!(
            out,
            "  conviction {:<5} vol1h_used {:.4}%  leverage {:>4.1}  margin {:>7.2}  notional {:>8.2}  round-trip {:.2} ({:.2}bp)",
            r.conviction, r.vol1h_used_pct, r.leverage, r.margin, r.notional, r.round_trip_fee_usd, r.round_trip_fee_bp
        );
    }
    let _ = writeln!(out, "\ntop {} markets by volume proxy:", TOP_MARKETS_BY_VOLUME);
    let _ = writeln!(out, "{:<12} {:>16} vol1h%", "market", "avg_day_ntl_vlm");
    for m in &rep.per_market_vol1h {
        let _ = writeln!(out, "{:<12} {:>16.0} {}", m.market, m.avg_day_ntl_vlm, fmt_pct(&Some(m.vol1h)));
    }
    out
}

/// The committed artifact: the same numbers, as markdown.
pub fn markdown(rep: &Report, label: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# backtest stats — {label}\n");
    let _ = writeln!(
        out,
        "**{} .. {} UTC** · {} bars of {} requested minutes · {} markets\n",
        rep.from, rep.to, rep.bars, rep.requested_minutes, rep.markets
    );

    let _ = writeln!(out, "## What this is not\n");
    for c in &rep.caveats {
        let _ = writeln!(out, "- {c}");
    }

    let pct_row = |name: &str, p: &Option<Percentiles>| -> String {
        match p {
            None => format!("| {name} | — | — | — | — | — | — | — |"),
            Some(p) => format!(
                "| {name} | {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |",
                p.n, p.p50, p.p75, p.p90, p.p95, p.p99, p.max
            ),
        }
    };

    let _ = writeln!(out, "\n## vol1h% percentiles\n");
    let _ = writeln!(out, "| series | n | p50 | p75 | p90 | p95 | p99 | max |");
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|");
    let _ = writeln!(out, "{}", pct_row("BTC (regime gate input)", &rep.btc_vol1h));
    let _ = writeln!(out, "{}", pct_row("whole universe, pooled", &rep.universe_vol1h));

    if !rep.per_market_vol1h.is_empty() {
        let _ = writeln!(out, "\n## Per-market vol1h%, top {} by volume proxy\n", TOP_MARKETS_BY_VOLUME);
        let _ = writeln!(out, "| market | avg day_ntl_vlm | n | p50 | p75 | p90 | p95 | p99 | max |");
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|");
        for m in &rep.per_market_vol1h {
            let p = &m.vol1h;
            let _ = writeln!(
                out,
                "| {} | {:.0} | {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |",
                m.market, m.avg_day_ntl_vlm, p.n, p.p50, p.p75, p.p90, p.p95, p.p99, p.max
            );
        }
    }

    let _ = writeln!(out, "\n## funding_z distribution\n");
    let _ = writeln!(out, "market-minutes n = {}\n", rep.funding_z.n);
    let _ = writeln!(out, "| \\|z\\| threshold | share of market-minutes |");
    let _ = writeln!(out, "|---|---:|");
    let _ = writeln!(out, "| > 1.0 | {:.2}% |", rep.funding_z.share_abs_gt_1_pct);
    let _ = writeln!(out, "| > 2.0 | {:.2}% |", rep.funding_z.share_abs_gt_2_pct);
    let _ = writeln!(
        out,
        "| > 2.5 (screener's side-flip threshold) | {:.2}% |",
        rep.funding_z.share_abs_gt_2_5_pct
    );

    let _ = writeln!(out, "\n## screener score distribution\n");
    let _ = writeln!(out, "pooled market-minutes n = {}, current `min_score` = {}\n", rep.score.n, rep.score.min_score);
    let _ = writeln!(out, "share of market-minutes scoring >= min_score: **{:.2}%**\n", rep.score.share_above_min_score_pct);
    let _ = writeln!(out, "| n | p50 | p75 | p90 | p95 | p99 | max |");
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|");
    match &rep.score.percentiles {
        None => {
            let _ = writeln!(out, "| — | — | — | — | — | — | — |");
        }
        Some(p) => {
            let _ = writeln!(
                out,
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} |",
                p.n, p.p50, p.p75, p.p90, p.p95, p.p99, p.max
            );
        }
    }

    let _ = writeln!(out, "\n## turnover / fee reference\n");
    let _ = writeln!(
        out,
        "`sizing::size_position` at this window's measured median vol1h ({:.4}%):\n",
        rep.sizing_reference.first().map(|r| r.vol1h_used_pct).unwrap_or(0.0)
    );
    let _ = writeln!(out, "| conviction | leverage | margin | notional | round-trip fee $ | round-trip fee bp |");
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|");
    for r in &rep.sizing_reference {
        let _ = writeln!(
            out,
            "| {} | {:.1} | {:.2} | {:.2} | {:.2} | {:.2} |",
            r.conviction, r.leverage, r.margin, r.notional, r.round_trip_fee_usd, r.round_trip_fee_bp
        );
    }
    let _ = writeln!(out, "\nround-trip fee in bp is independent of notional (fee is linear in it), so it is the same at every conviction — confirms it here rather than asserting it from outside.\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::engine::tests::{fixture_cfg, golden_fixture};

    #[test]
    fn percentiles_are_numpy_style_linear_interpolation() {
        let p = percentiles(vec![1.0, 2.0, 3.0, 4.0, 5.0]).expect("non-empty");
        assert_eq!(p.n, 5);
        assert!((p.p50 - 3.0).abs() < 1e-12, "p50 {}", p.p50);
        assert!((p.p90 - 4.6).abs() < 1e-12, "p90 {}", p.p90); // idx=3.6 -> 4 + 0.6*(5-4)
        assert!((p.max - 5.0).abs() < 1e-12);
        // unsorted input is sorted internally
        let p2 = percentiles(vec![5.0, 1.0, 3.0, 2.0, 4.0]).expect("non-empty");
        assert_eq!(p, p2);
        // a single value has every percentile equal to it
        let one = percentiles(vec![7.0]).expect("single");
        assert_eq!((one.p50, one.p90, one.max), (7.0, 7.0, 7.0));
        assert!(percentiles(vec![]).is_none(), "empty has no distribution");
    }

    #[test]
    fn funding_z_buckets_count_shares_of_absolute_values() {
        let vals = vec![0.5, 1.5, 2.2, 2.6, 3.0];
        let b = funding_z_buckets(&vals);
        assert_eq!(b.n, 5);
        assert!((b.share_abs_gt_1_pct - 80.0).abs() < 1e-9, "4/5 > 1.0"); // 1.5,2.2,2.6,3.0
        assert!((b.share_abs_gt_2_pct - 60.0).abs() < 1e-9, "3/5 > 2.0"); // 2.2,2.6,3.0
        assert!((b.share_abs_gt_2_5_pct - 40.0).abs() < 1e-9, "2/5 > 2.5"); // 2.6,3.0
        assert_eq!(funding_z_buckets(&[]).n, 0);
        assert_eq!(funding_z_buckets(&[]).share_abs_gt_1_pct, 0.0, "empty is 0%, not NaN");
    }

    /// The golden fixture (spec Decision 8): three flat-then-scripted markets, no BTC, no
    /// funding history (`vec![]` for every market — see `engine::tests::golden_fixture`), so
    /// `funding_z` is 0.0 at every minute and every distribution is hand-verifiable.
    #[test]
    fn build_reports_sane_distributions_on_the_golden_fixture() {
        let cfg = fixture_cfg();
        let data = golden_fixture();
        let from_ms = crate::backtest::engine::tests::T0;
        let to_ms = from_ms + 399 * MINUTE_MS;
        let rep = build(&cfg, &data, from_ms, to_ms);

        assert_eq!(rep.markets, 3, "AAA, BBB, CCC");
        assert_eq!(rep.bars, 400, "dense tape covers every replayed minute");
        assert_eq!(rep.requested_minutes, 400);
        assert!(rep.btc_vol1h.is_none(), "the golden fixture has no BTC row");
        assert!(rep.universe_vol1h.is_some(), "AAA/BBB/CCC all warm past the 5m lookback");

        // features price from idx 5 (inclusive) through 399 -> 395 minutes per market.
        assert_eq!(rep.per_market_vol1h.len(), 3);
        for m in &rep.per_market_vol1h {
            assert_eq!(m.vol1h.n, 395, "{}: n {}", m.market, m.vol1h.n);
        }

        // Every market has funding = vec![] -> funding_z_at is 0.0 at every minute, for every
        // one of the 3 markets across all 400 bars.
        assert_eq!(rep.funding_z.n, 1200, "3 markets x 400 bars");
        assert_eq!(rep.funding_z.share_abs_gt_1_pct, 0.0);
        assert_eq!(rep.funding_z.share_abs_gt_2_pct, 0.0);
        assert_eq!(rep.funding_z.share_abs_gt_2_5_pct, 0.0);

        // Score pool: same 395-minute warmup per market, all three always clear the fixture's
        // vlm floor (v=100, c~100+, day_ntl_vlm clears 1000.0 within the first traded minute).
        assert_eq!(rep.score.n, 3 * 395);
        assert_eq!(rep.score.min_score, cfg.screener.min_score);
        assert!(rep.score.percentiles.is_some());
        // share_above_min_score is a real fraction of a real pool, not a placeholder
        assert!((0.0..=100.0).contains(&rep.score.share_above_min_score_pct));

        // Sizing reference: two rows (default conviction + 1.0), fee bp pinned exactly by
        // construction regardless of notional (FEE_RATE=0.00075 both sides).
        assert_eq!(rep.sizing_reference.len(), 2);
        assert!((rep.sizing_reference[0].conviction - super::super::DEFAULT_CONVICTION).abs() < 1e-12);
        assert!((rep.sizing_reference[1].conviction - 1.0).abs() < 1e-12);
        for r in &rep.sizing_reference {
            assert!((r.round_trip_fee_bp - 15.0).abs() < 1e-9, "round trip is always 15bp: {}", r.round_trip_fee_bp);
            assert!((r.notional - r.margin * r.leverage).abs() < 1e-6, "notional = margin x leverage");
            assert!((r.round_trip_fee_usd - r.notional * 0.0015).abs() < 1e-9);
        }

        // Deterministic: two builds over the same inputs are byte-identical (spec Decision 6's
        // determinism requirement, at the level this report is consumed).
        let rep2 = build(&cfg, &data, from_ms, to_ms);
        assert_eq!(rep, rep2);
        let json = serde_json::to_string_pretty(&rep).expect("serialize");
        let back: Report = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, rep);
    }

    #[test]
    fn markdown_and_human_summary_render_every_section() {
        let cfg = fixture_cfg();
        let data = golden_fixture();
        let from_ms = crate::backtest::engine::tests::T0;
        let to_ms = from_ms + 399 * MINUTE_MS;
        let rep = build(&cfg, &data, from_ms, to_ms);

        let md = markdown(&rep, "golden");
        assert!(md.contains("# backtest stats — golden"));
        assert!(md.contains("distribution report only"), "caveat header present:\n{md}");
        assert!(md.contains("vol1h% percentiles"));
        assert!(md.contains("Per-market vol1h%"));
        assert!(md.contains("funding_z distribution"));
        assert!(md.contains("screener score distribution"));
        assert!(md.contains("turnover / fee reference"));
        assert!(md.contains("15.00"), "the pinned 15bp round trip shows up:\n{md}");
        assert_eq!(markdown(&rep, "golden"), md, "markdown is a pure function of the report");

        let text = human_summary(&rep);
        assert!(text.contains("BTC vol1h%      no data"), "{text}");
        assert!(text.contains("funding_z"));
        assert!(text.contains("sizing reference"));
    }

    #[test]
    fn default_label_names_the_window() {
        assert_eq!(default_label("2026-08-06", "2026-08-09"), "stats-2026-08-06_2026-08-09");
    }
}
