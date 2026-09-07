//! `RunReport` — what a replay produced, as JSON and as markdown (spec Decision 7).
//!
//! Everything here is a pure function of a [`RunOutcome`] and the config that produced it: no
//! clock, no environment, no ordering that depends on a hash. Two identical runs therefore
//! serialize to identical bytes, which is what makes a sweep's ranking trustworthy and a
//! committed report diffable.
//!
//! Every report carries its own caveats ([`caveats`]) — spec Decision 2 requires the "mechanical
//! replay" disclaimer in the header, and the feature deviations of Decision 4 belong next to the
//! numbers they shaped, not in a doc someone has to remember to read.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::contracts::{EquityPoint, Side};

use super::engine::{RunOutcome, net_by_position};

/// Equity points a report carries. A 7-day run is 10 080 minutes; 500 points is a legible
/// chart and a small file. The last point is always kept, so a curve may hold `MAX + 1`.
pub const EQUITY_CURVE_MAX_POINTS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub params: ReportParams,
    /// What this number is NOT — printed in every header (spec Decision 2 / 4).
    pub caveats: Vec<String>,
    pub summary: Summary,
    pub exit_mix: ExitMix,
    /// `GateRefusal::as_str()` -> count. Only the rails a nominee can actually reach appear:
    /// the market-scoped ones (dup / per-market cap / cooldown) are applied by the screener's
    /// pre-filter, exactly as in the daemon, so they refuse silently by never nominating.
    pub refusals: BTreeMap<String, i64>,
    pub per_market: Vec<MarketReport>,
    pub equity_curve: Vec<EquityPoint>,
    pub open_at_end: Vec<OpenPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportParams {
    pub from: String,
    pub to: String,
    pub from_ms: i64,
    pub to_ms: i64,
    pub conviction: f64,
    pub markets: Vec<String>,
    /// `--set key=value` arguments, as given.
    pub overrides: Vec<String>,
    /// Every knob the replay actually read, resolved after the overrides — a report is
    /// reproducible from its own params.
    pub knobs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// 1m steps that had at least one market with tape.
    pub bars: i64,
    pub entries: i64,
    pub closes: i64,
    /// Gated-through entries that had no next candle to fill at (end of a market's tape).
    pub unfilled: i64,
    pub gross_pnl: f64,
    pub fees: f64,
    /// REALIZED only: gross of every close, minus every fee paid — including the entry fee of
    /// positions still open. It is not the equity delta; see [`Summary::unrealized_pnl`].
    pub net_pnl: f64,
    /// Open positions marked to the last close they saw. Reported so the identity
    /// `end_equity = start_equity + net_pnl + unrealized_pnl` is visible rather than inferred —
    /// a run that ends holding three positions has an equity delta its `net` cannot explain.
    pub unrealized_pnl: f64,
    pub wins: i64,
    pub losses: i64,
    pub win_rate: f64,
    /// Net pnl per close.
    pub expectancy: f64,
    /// `avg_win / |avg_loss|`; 0.0 when there is no loss to divide by.
    pub payoff: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub start_equity: f64,
    pub end_equity: f64,
    pub return_pct: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    /// UTC days the kill switch latched on.
    pub kill_days: Vec<String>,
}

/// How positions ended. `veto_close` is absent by construction (spec Decision 2: no analyst,
/// so no review loop), which is itself a stated caveat rather than a zero row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitMix {
    pub tp: i64,
    pub sl: i64,
    pub time_stop: i64,
    /// Still open when the window ended — marked to the last close in `end_equity`.
    pub open_at_end: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketReport {
    pub market: String,
    pub entries: i64,
    pub closes: i64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub net_pnl: f64,
    pub wins: i64,
    pub tp: i64,
    pub sl: i64,
    pub time_stop: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenPosition {
    pub market: String,
    pub side: Side,
    pub entry_px: f64,
    pub size: f64,
    pub opened_ts: i64,
}

/// `YYYY-MM-DD HH:MM` UTC.
pub fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// Peak-to-trough drawdown of an equity curve: `(absolute, percent of the peak)`.
///
/// The peak is a RUNNING maximum, so the pair reported is the worst trough measured from the
/// highest equity that preceded it — not from the start, and not from the final peak. Both are
/// 0.0 for a curve that only ever rises.
pub fn max_drawdown(curve: &[EquityPoint]) -> (f64, f64) {
    let mut peak = f64::NEG_INFINITY;
    let (mut dd, mut dd_pct) = (0.0f64, 0.0f64);
    for p in curve {
        if p.equity > peak {
            peak = p.equity;
        }
        let drop = peak - p.equity;
        if drop > dd {
            dd = drop;
        }
        if peak > 0.0 {
            let pct = drop / peak * 100.0;
            if pct > dd_pct {
                dd_pct = pct;
            }
        }
    }
    (dd, dd_pct)
}

/// Keep at most `max` evenly spaced points, always including the first and the last.
pub fn downsample(points: &[EquityPoint], max: usize) -> Vec<EquityPoint> {
    if max == 0 || points.len() <= max {
        return points.to_vec();
    }
    let stride = points.len().div_ceil(max);
    let mut out: Vec<EquityPoint> = points.iter().step_by(stride).cloned().collect();
    if let Some(last) = points.last()
        && out.last().is_none_or(|p| p.ts != last.ts)
    {
        out.push(last.clone());
    }
    out
}

/// Every knob the replay reads, resolved. Sweeps diff two of these to label a run.
pub fn config_knobs(cfg: &Config) -> BTreeMap<String, String> {
    let r = &cfg.risk;
    let s = &cfg.sizing;
    BTreeMap::from([
        ("screener.min_score".to_string(), r_fmt(cfg.screener.min_score)),
        ("screener.top_k".to_string(), cfg.screener.top_k.to_string()),
        ("universe.min_vlm_native".to_string(), r_fmt(cfg.universe.min_vlm_native)),
        ("universe.min_vlm_dex".to_string(), r_fmt(cfg.universe.min_vlm_dex)),
        ("sizing.bankroll".to_string(), r_fmt(s.bankroll)),
        ("sizing.vol_ref".to_string(), r_fmt(s.vol_ref)),
        ("sizing.margin_min".to_string(), r_fmt(s.margin_min)),
        ("sizing.margin_max".to_string(), r_fmt(s.margin_max)),
        ("sizing.stop_floor_pct".to_string(), r_fmt(s.stop_floor_pct)),
        ("sizing.tp_mult".to_string(), r_fmt(s.tp_mult)),
        ("risk.max_concurrent".to_string(), r.max_concurrent.to_string()),
        ("risk.daily_cap".to_string(), r.daily_cap.to_string()),
        ("risk.cooldown_min".to_string(), r.cooldown_min.to_string()),
        ("risk.cooldown_after_sl_min".to_string(), r.cooldown_after_sl_min.to_string()),
        ("risk.per_market_daily_cap".to_string(), r.per_market_daily_cap.to_string()),
        ("risk.morning_entry_budget".to_string(), r.morning_entry_budget.to_string()),
        ("risk.regime_vol_max".to_string(), r_fmt(r.regime_vol_max)),
        ("risk.conviction_min".to_string(), r_fmt(r.conviction_min)),
        ("risk.kill_switch_pct".to_string(), r_fmt(r.kill_switch_pct)),
        ("risk.time_stop_hours".to_string(), r_fmt(r.time_stop_hours)),
    ])
}

/// Shortest round-tripping form of a float knob, so `1.5` records as `1.5` and not `1.500000`.
fn r_fmt(v: f64) -> String {
    format!("{v}")
}

/// The standing disclaimers, in the order they matter.
pub fn caveats(cfg: &Config, conviction: f64) -> Vec<String> {
    caveats_with(cfg, &format!("fixed at {conviction} for every entry"))
}

/// [`caveats`], with the conviction clause spelled by the caller — a sweep fixes a DIFFERENT
/// conviction in each of its runs, so its summary header cannot name one number.
pub fn caveats_with(cfg: &Config, conviction_clause: &str) -> Vec<String> {
    vec![
        format!(
            "mechanical replay — no analyst judgment, no news: conviction is {conviction_clause} \
             and the screener's side_hint is taken as the side (spec Decision 2)"
        ),
        "no review loop: no veto closes, no stop moves, no analyst time-stop — brackets and the \
         horizon are the only exits (spec Decision 5)"
            .to_string(),
        format!(
            "exits: SL is checked before TP inside a candle (a candle touching both is scored as \
             the stop) and fills AT the trigger; the {}h horizon fills at the candle close \
             (spec Decision 5)",
            cfg.risk.time_stop_hours
        ),
        "entries fill at the next 1m open plus flat slip (2bp native / 5bp dex); 7.5bp taker fee \
         per side (spec Decision 5)"
            .to_string(),
        "day_ntl_vlm is proxied by rolling 24h candle quote volume — the venue does not serve \
         historical dayNtlVlm, and this proxy decides WHICH markets clear the universe filter \
         (spec Decision 4)"
            .to_string(),
        "features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints \
         cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)"
            .to_string(),
        "open_interest is 0.0 throughout — historical OI is not served (nothing in the screener, \
         sizing or the gates reads it)"
            .to_string(),
        "the screener ticks every 1m (live: 45s) — the closest candle-aligned cadence \
         (spec Decision 6)"
            .to_string(),
    ]
}

/// Turn a run's ledger into its report.
pub fn build(cfg: &Config, out: &RunOutcome) -> RunReport {
    let net_by_pos = net_by_position(&out.trades);
    let closed: Vec<&super::engine::PositionRow> =
        out.positions.iter().filter(|p| p.status == "closed").collect();

    let mut nets: Vec<f64> = Vec::with_capacity(closed.len());
    let mut exit_mix = ExitMix::default();
    let mut per_market: BTreeMap<String, MarketReport> = BTreeMap::new();

    for p in &out.positions {
        let m = per_market.entry(p.pos.market.clone()).or_insert_with(|| MarketReport {
            market: p.pos.market.clone(),
            entries: 0,
            closes: 0,
            gross_pnl: 0.0,
            fees: 0.0,
            net_pnl: 0.0,
            wins: 0,
            tp: 0,
            sl: 0,
            time_stop: 0,
        });
        m.entries += 1;
        if p.status != "closed" {
            exit_mix.open_at_end += 1;
        }
    }
    for t in &out.trades {
        if let Some(m) = per_market.get_mut(&t.market) {
            m.gross_pnl += t.realized_pnl;
            m.fees += t.fee;
            m.net_pnl += t.realized_pnl - t.fee;
        }
    }
    for p in &closed {
        let net = net_by_pos.get(&p.pos.id).copied().unwrap_or(0.0);
        nets.push(net);
        let m = per_market.get_mut(&p.pos.market).expect("market seeded above");
        m.closes += 1;
        if net > 0.0 {
            m.wins += 1;
        }
        match p.close_action.as_deref() {
            Some("tp") => {
                exit_mix.tp += 1;
                m.tp += 1;
            }
            Some("sl") => {
                exit_mix.sl += 1;
                m.sl += 1;
            }
            Some("time_stop") => {
                exit_mix.time_stop += 1;
                m.time_stop += 1;
            }
            _ => {}
        }
    }

    let gross_pnl: f64 = out.trades.iter().map(|t| t.realized_pnl).sum();
    let fees: f64 = out.trades.iter().map(|t| t.fee).sum();
    let net_pnl = gross_pnl - fees;
    let wins = nets.iter().filter(|n| **n > 0.0).count() as i64;
    let losses = nets.len() as i64 - wins;
    let win_sum: f64 = nets.iter().filter(|n| **n > 0.0).sum();
    let loss_sum: f64 = nets.iter().filter(|n| **n <= 0.0).sum();
    let avg_win = if wins > 0 { win_sum / wins as f64 } else { 0.0 };
    let avg_loss = if losses > 0 { loss_sum / losses as f64 } else { 0.0 };
    let closes = nets.len() as i64;

    let start_equity = out.bankroll;
    let end_equity = out.equity.last().map(|p| p.equity).unwrap_or(start_equity);
    let (max_dd, max_dd_pct) = max_drawdown(&out.equity);

    RunReport {
        params: ReportParams {
            from: stamp(out.params.from_ms),
            to: stamp(out.params.to_ms),
            from_ms: out.params.from_ms,
            to_ms: out.params.to_ms,
            conviction: out.params.conviction,
            markets: out.params.markets.clone(),
            overrides: out.params.overrides.clone(),
            knobs: config_knobs(cfg),
        },
        caveats: caveats(cfg, out.params.conviction),
        summary: Summary {
            bars: out.bars,
            entries: out.positions.len() as i64,
            closes,
            unfilled: out.unfilled,
            gross_pnl,
            fees,
            net_pnl,
            // By subtraction rather than re-derivation, so the identity holds to the last bit.
            unrealized_pnl: end_equity - start_equity - net_pnl,
            wins,
            losses,
            win_rate: if closes > 0 { wins as f64 / closes as f64 * 100.0 } else { 0.0 },
            expectancy: if closes > 0 { nets.iter().sum::<f64>() / closes as f64 } else { 0.0 },
            payoff: if avg_loss < 0.0 { avg_win / avg_loss.abs() } else { 0.0 },
            avg_win,
            avg_loss,
            start_equity,
            end_equity,
            return_pct: if start_equity > 0.0 {
                (end_equity - start_equity) / start_equity * 100.0
            } else {
                0.0
            },
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            kill_days: out.kill_days.clone(),
        },
        exit_mix,
        refusals: out.refusals.clone(),
        per_market: per_market.into_values().filter(|m| m.entries > 0).collect(),
        equity_curve: downsample(&out.equity, EQUITY_CURVE_MAX_POINTS),
        open_at_end: out
            .positions
            .iter()
            .filter(|p| p.status != "closed")
            .map(|p| OpenPosition {
                market: p.pos.market.clone(),
                side: p.pos.side,
                entry_px: p.pos.entry_px,
                size: p.pos.size,
                opened_ts: p.pos.opened_ts,
            })
            .collect(),
    }
}

/// The one-screen summary the CLI prints when a run finishes.
pub fn human_summary(rep: &RunReport) -> String {
    let s = &rep.summary;
    let mut out = String::new();
    let _ = writeln!(out, "window   {} .. {}  ({} bars)", rep.params.from, rep.params.to, s.bars);
    let _ = writeln!(
        out,
        "universe {} market{}   conviction {}",
        rep.params.markets.len(),
        if rep.params.markets.len() == 1 { "" } else { "s" },
        rep.params.conviction
    );
    if !rep.params.overrides.is_empty() {
        let _ = writeln!(out, "overrides {}", rep.params.overrides.join(" "));
    }
    let _ = writeln!(
        out,
        "\nnet {:+.2} realized   gross {:+.2}   fees {:.2}   unrealized {:+.2}   equity {:.2} -> {:.2} ({:+.2}%)",
        s.net_pnl, s.gross_pnl, s.fees, s.unrealized_pnl, s.start_equity, s.end_equity, s.return_pct
    );
    let _ = writeln!(
        out,
        "entries {}   closes {}   win rate {:.1}%   expectancy {:+.2}/close   payoff {:.2}",
        s.entries, s.closes, s.win_rate, s.expectancy, s.payoff
    );
    let _ = writeln!(
        out,
        "maxDD {:.2} ({:.2}%)   exits tp {} · sl {} · time_stop {} · open {}",
        s.max_drawdown,
        s.max_drawdown_pct,
        rep.exit_mix.tp,
        rep.exit_mix.sl,
        rep.exit_mix.time_stop,
        rep.exit_mix.open_at_end
    );
    if !s.kill_days.is_empty() {
        let _ = writeln!(out, "kill days {}", s.kill_days.join(", "));
    }
    if !rep.refusals.is_empty() {
        let refusals: Vec<String> =
            rep.refusals.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(out, "refusals  {}", refusals.join(" · "));
    }
    if !rep.per_market.is_empty() {
        let _ = writeln!(out, "\n{:<12} {:>7} {:>7} {:>10} {:>8}", "market", "entries", "closes", "net", "fees");
        let mut rows: Vec<&MarketReport> = rep.per_market.iter().collect();
        rows.sort_by(|a, b| b.net_pnl.partial_cmp(&a.net_pnl).unwrap_or(std::cmp::Ordering::Equal));
        for m in rows {
            let _ = writeln!(
                out,
                "{:<12} {:>7} {:>7} {:>+10.2} {:>8.2}",
                m.market, m.entries, m.closes, m.net_pnl, m.fees
            );
        }
    }
    out
}

/// The committed artifact: the same numbers, as markdown.
pub fn markdown(rep: &RunReport, label: &str) -> String {
    let s = &rep.summary;
    let mut out = String::new();
    let _ = writeln!(out, "# backtest — {label}\n");
    let _ = writeln!(
        out,
        "**{} .. {} UTC** · {} bars · {} market{} · conviction **{}**\n",
        rep.params.from,
        rep.params.to,
        s.bars,
        rep.params.markets.len(),
        if rep.params.markets.len() == 1 { "" } else { "s" },
        rep.params.conviction
    );
    if !rep.params.overrides.is_empty() {
        let _ = writeln!(out, "Overrides: `{}`\n", rep.params.overrides.join("` `"));
    }

    let _ = writeln!(out, "## What this is not\n");
    for c in &rep.caveats {
        let _ = writeln!(out, "- {c}");
    }

    let _ = writeln!(out, "\n## Result\n");
    let _ = writeln!(out, "| metric | value |");
    let _ = writeln!(out, "|---|---:|");
    let _ = writeln!(out, "| net (realized) | {:+.2} |", s.net_pnl);
    let _ = writeln!(out, "| gross | {:+.2} |", s.gross_pnl);
    let _ = writeln!(out, "| fees | {:.2} |", s.fees);
    if s.unrealized_pnl.abs() > 1e-9 {
        let _ = writeln!(
            out,
            "| unrealized (still open) | {:+.2} |",
            s.unrealized_pnl
        );
    }
    let _ = writeln!(out, "| equity | {:.2} -> {:.2} ({:+.2}%) |", s.start_equity, s.end_equity, s.return_pct);
    let _ = writeln!(out, "| entries | {} |", s.entries);
    let _ = writeln!(out, "| closes | {} |", s.closes);
    let _ = writeln!(out, "| win rate | {:.1}% ({}W / {}L) |", s.win_rate, s.wins, s.losses);
    let _ = writeln!(out, "| expectancy | {:+.2} / close |", s.expectancy);
    let _ = writeln!(out, "| payoff | {:.2} (avg win {:+.2} / avg loss {:+.2}) |", s.payoff, s.avg_win, s.avg_loss);
    let _ = writeln!(out, "| max drawdown | {:.2} ({:.2}%) |", s.max_drawdown, s.max_drawdown_pct);
    let _ = writeln!(
        out,
        "| kill-switch days | {} |",
        if s.kill_days.is_empty() { "none".to_string() } else { s.kill_days.join(", ") }
    );
    if s.unfilled > 0 {
        let _ = writeln!(out, "| unfilled signals | {} (no next candle in the window) |", s.unfilled);
    }

    let _ = writeln!(out, "\n## Exit mix\n");
    let _ = writeln!(out, "| tp | sl | time_stop | open at end |");
    let _ = writeln!(out, "|---:|---:|---:|---:|");
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} |",
        rep.exit_mix.tp, rep.exit_mix.sl, rep.exit_mix.time_stop, rep.exit_mix.open_at_end
    );

    let _ = writeln!(out, "\n## Gate refusals\n");
    if rep.refusals.is_empty() {
        let _ = writeln!(
            out,
            "None reached a gate. Market-scoped rails (dup / per-market cap / cooldown) are applied \
             by the screener's pre-filter, so they refuse by never nominating."
        );
    } else {
        let _ = writeln!(out, "| refusal | count |");
        let _ = writeln!(out, "|---|---:|");
        for (k, v) in &rep.refusals {
            let _ = writeln!(out, "| {k} | {v} |");
        }
    }

    if !rep.per_market.is_empty() {
        let _ = writeln!(out, "\n## Per market\n");
        let _ = writeln!(out, "| market | entries | closes | net | gross | fees | wins | tp | sl | time_stop |");
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        let mut rows: Vec<&MarketReport> = rep.per_market.iter().collect();
        rows.sort_by(|a, b| {
            b.net_pnl.partial_cmp(&a.net_pnl).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.market.cmp(&b.market))
        });
        for m in rows {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {:+.2} | {:+.2} | {:.2} | {} | {} | {} | {} |",
                m.market, m.entries, m.closes, m.net_pnl, m.gross_pnl, m.fees, m.wins, m.tp, m.sl, m.time_stop
            );
        }
    }

    if !rep.open_at_end.is_empty() {
        let _ = writeln!(out, "\n## Open when the window ended\n");
        let _ = writeln!(out, "| market | side | entry | size | opened |");
        let _ = writeln!(out, "|---|---|---:|---:|---|");
        for p in &rep.open_at_end {
            let side = match p.side {
                Side::Long => "long",
                Side::Short => "short",
            };
            let _ = writeln!(
                out,
                "| {} | {} | {:.6} | {:.6} | {} |",
                p.market,
                side,
                p.entry_px,
                p.size,
                stamp(p.opened_ts)
            );
        }
    }

    let _ = writeln!(out, "\n## Knobs in force\n");
    let _ = writeln!(out, "| knob | value |");
    let _ = writeln!(out, "|---|---:|");
    for (k, v) in &rep.params.knobs {
        let _ = writeln!(out, "| {k} | {v} |");
    }

    let _ = writeln!(
        out,
        "\n## Equity curve\n\n{} points, downsampled from {} minutes.\n",
        rep.equity_curve.len(),
        s.bars
    );
    let _ = writeln!(out, "| ts | equity |");
    let _ = writeln!(out, "|---|---:|");
    for p in &rep.equity_curve {
        let _ = writeln!(out, "| {} | {:.2} |", stamp(p.ts), p.equity);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::engine::tests::{fixture_cfg, golden_run};

    fn curve(vals: &[f64]) -> Vec<EquityPoint> {
        vals.iter().enumerate().map(|(i, v)| EquityPoint { ts: i as i64 * 60_000, equity: *v }).collect()
    }

    #[test]
    fn max_drawdown_is_peak_to_trough_not_start_to_end() {
        // up to 1200, down to 900 (-300, -25%), back up to 1100: the run ENDS up on the day but
        // the drawdown is measured from the peak that preceded the trough.
        let (dd, pct) = max_drawdown(&curve(&[1000.0, 1200.0, 900.0, 1100.0]));
        assert!((dd - 300.0).abs() < 1e-9, "dd {dd}");
        assert!((pct - 25.0).abs() < 1e-9, "pct {pct}");
        // a monotone rise has no drawdown at all
        assert_eq!(max_drawdown(&curve(&[100.0, 200.0, 300.0])), (0.0, 0.0));
        // the deepest trough wins, even when a later peak is higher
        let (dd2, pct2) = max_drawdown(&curve(&[1000.0, 500.0, 2000.0, 1600.0]));
        assert!((dd2 - 500.0).abs() < 1e-9, "the 1000->500 leg is the biggest absolute drop");
        assert!((pct2 - 50.0).abs() < 1e-9, "and the deepest percentage one");
        // an empty curve is not a drawdown
        assert_eq!(max_drawdown(&[]), (0.0, 0.0));
    }

    #[test]
    fn downsampling_keeps_the_ends_and_bounds_the_size() {
        let full = curve(&(0..10_000).map(|i| 1000.0 + i as f64).collect::<Vec<_>>());
        let small = downsample(&full, EQUITY_CURVE_MAX_POINTS);
        assert!(small.len() <= EQUITY_CURVE_MAX_POINTS + 1, "len {}", small.len());
        assert_eq!(small.first(), full.first(), "the start is always kept");
        assert_eq!(small.last(), full.last(), "and so is the end");
        // strictly increasing timestamps, no duplicates
        assert!(small.windows(2).all(|w| w[0].ts < w[1].ts));
        // a curve under the cap is untouched
        let short = curve(&[1.0, 2.0, 3.0]);
        assert_eq!(downsample(&short, 500), short);
        assert_eq!(downsample(&short, 0), short, "a zero cap disables downsampling");
    }

    #[test]
    fn the_golden_run_reports_its_hand_computed_numbers() {
        let cfg = fixture_cfg();
        let rep = build(&cfg, &golden_run());
        let s = &rep.summary;

        assert_eq!((s.entries, s.closes), (2, 2));
        assert!((s.gross_pnl - 8.0).abs() < 1e-9);
        assert!((s.fees - 2.418).abs() < 1e-9);
        assert!((s.net_pnl - 5.582).abs() < 1e-9, "net {}", s.net_pnl);
        assert!(s.unrealized_pnl.abs() < 1e-9, "the golden run ends flat");
        assert!((s.end_equity - 1005.582).abs() < 1e-9);
        assert!((s.return_pct - 0.5582).abs() < 1e-9);
        assert_eq!((s.wins, s.losses), (1, 1));
        assert!((s.win_rate - 50.0).abs() < 1e-9);
        assert!((s.expectancy - 2.791).abs() < 1e-9, "5.582 net over two closes");
        assert!((s.avg_win - 14.788).abs() < 1e-9);
        assert!((s.avg_loss - -9.206).abs() < 1e-9);
        assert!((s.payoff - 14.788 / 9.206).abs() < 1e-9, "payoff {}", s.payoff);
        assert!(s.kill_days.is_empty());

        // The stop-out is the only drawdown: equity peaked at 1014.788 after AAA's target and
        // bottomed at 1005.582 after BBB's stop — 9.206, or 0.907% of the peak.
        assert!((s.max_drawdown - 9.206).abs() < 1e-3, "maxDD {}", s.max_drawdown);
        assert!((s.max_drawdown_pct - 9.206 / 1014.788 * 100.0).abs() < 1e-3);

        assert_eq!(rep.exit_mix, ExitMix { tp: 1, sl: 1, time_stop: 0, open_at_end: 0 });
        assert!(rep.open_at_end.is_empty());
        assert!(rep.refusals.is_empty(), "cooled-down markets never reach a gate");

        // Per-market: CCC never traded, so it is not a row.
        assert_eq!(rep.per_market.len(), 2);
        let aaa = rep.per_market.iter().find(|m| m.market == "AAA").expect("AAA row");
        assert_eq!((aaa.entries, aaa.closes, aaa.wins, aaa.tp, aaa.sl), (1, 1, 1, 1, 0));
        assert!((aaa.net_pnl - 14.788).abs() < 1e-9);
        assert!((aaa.fees - 1.212).abs() < 1e-9);
        let bbb = rep.per_market.iter().find(|m| m.market == "BBB").expect("BBB row");
        assert_eq!((bbb.wins, bbb.tp, bbb.sl), (0, 0, 1));
        assert!((bbb.net_pnl - -9.206).abs() < 1e-9);
        // per-market nets add up to the run's net
        let sum: f64 = rep.per_market.iter().map(|m| m.net_pnl).sum();
        assert!((sum - s.net_pnl).abs() < 1e-9);
    }

    /// Spec Decision 6's determinism requirement, at the level it is actually consumed: the
    /// REPORT bytes.
    ///
    /// The struct equality after a round-trip is not incidental — it is why `serde_json` is
    /// built with `float_roundtrip`. Without that feature the parser is allowed to land one ULP
    /// off (`14.787999999999993` came back as `...991`), so a committed report could not be read
    /// back into the numbers that produced it, and a sweep re-ranked from its own JSON could
    /// disagree with the run.
    #[test]
    fn a_report_round_trips_and_two_runs_serialize_identically() {
        let cfg = fixture_cfg();
        let a = serde_json::to_string_pretty(&build(&cfg, &golden_run())).expect("serialize");
        let b = serde_json::to_string_pretty(&build(&cfg, &golden_run())).expect("serialize");
        assert_eq!(a, b, "two runs must produce byte-identical reports");
        let back: RunReport = serde_json::from_str(&a).expect("deserialize");
        assert_eq!(back, build(&cfg, &golden_run()));
        assert_eq!(serde_json::to_string_pretty(&back).expect("re-serialize"), a);

        // markdown is a pure function of the report, so it is stable too
        let md_a = markdown(&back, "golden");
        assert_eq!(md_a, markdown(&build(&cfg, &golden_run()), "golden"));
        assert!(md_a.contains("mechanical replay"), "the disclaimer is in every header");
        assert!(md_a.contains("| net (realized) | +5.58 |"), "{md_a}");
        assert!(md_a.contains("risk.cooldown_after_sl_min"), "knobs are recorded");
    }

    #[test]
    fn the_human_summary_prints_the_headline_numbers() {
        let rep = build(&fixture_cfg(), &golden_run());
        let text = human_summary(&rep);
        assert!(text.contains("net +5.58"), "{text}");
        assert!(text.contains("win rate 50.0%"), "{text}");
        assert!(text.contains("tp 1 · sl 1"), "{text}");
        assert!(text.contains("AAA"), "per-market table present:\n{text}");
    }

    /// A run that ends holding a position: `net` stays realized-only and the equity delta is
    /// explained by `unrealized_pnl`, exactly.
    #[test]
    fn an_open_position_at_the_end_is_reported_as_unrealized() {
        use crate::backtest::engine::{run, tests::golden_fixture, tests::golden_params};
        let mut cfg = fixture_cfg();
        cfg.risk.cooldown_after_sl_min = 30; // lets BBB re-enter and stay open to the end
        let rep = build(&cfg, &run(&cfg, &golden_fixture(), golden_params()));
        let s = &rep.summary;
        assert_eq!(rep.exit_mix.open_at_end, 1);
        assert_eq!(rep.open_at_end.len(), 1);
        assert!(s.unrealized_pnl.abs() > 0.0, "an open position must carry a mark");
        assert!(
            (s.end_equity - (s.start_equity + s.net_pnl + s.unrealized_pnl)).abs() < 1e-9,
            "equity identity: {} vs {} + {} + {}",
            s.end_equity,
            s.start_equity,
            s.net_pnl,
            s.unrealized_pnl
        );
        // the still-open entry is counted as an entry, never as a close
        assert_eq!((s.entries, s.closes), (3, 2));
    }

    #[test]
    fn an_empty_run_reports_zeroes_rather_than_nan() {
        let cfg = fixture_cfg();
        let mut out = golden_run();
        out.positions.clear();
        out.trades.clear();
        let rep = build(&cfg, &out);
        let s = &rep.summary;
        assert_eq!((s.entries, s.closes, s.wins, s.losses), (0, 0, 0, 0));
        for v in [s.win_rate, s.expectancy, s.payoff, s.avg_win, s.avg_loss, s.net_pnl] {
            assert!(v.is_finite() && v == 0.0, "no division by zero anywhere: {v}");
        }
        assert!(rep.per_market.is_empty());
        // and it still serializes
        serde_json::to_string(&rep).expect("serialize an empty run");
    }
}
