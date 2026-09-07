//! The sweep grid runner (spec Decision 7): N replays of ONE cached tape, ranked.
//!
//! `--sweep key=v1,v2` is repeatable and expands to the cartesian product of its axes. Every
//! combo is a full [`super::engine::run`] with its own [`super::report::RunReport`]; the
//! ranked table on top is the artifact a decision is actually made from.
//!
//! THE TAPE IS LOADED ONCE. Reading a 340-market window out of sqlite and pre-walking its
//! funding rings costs more than a replay does, and a grid that reloaded per combo would
//! spend most of its wall time in the same query. Loading once is also what makes the ranking
//! honest: every row saw byte-identical input, so a difference between two rows is the knob
//! and nothing else.
//!
//! HOW EACH SWEEPABLE KEY REACHES THE ENGINE — every one of them is a `--set` override on the
//! config the run is built from ([`super::load_config`]), except conviction, which is a run
//! parameter because the analyst it replaces is not a config knob:
//!
//! | sweep key | override | what it moves |
//! |---|---|---|
//! | `stop_floor` | `sizing.stop_floor_pct` | the `stop_pct` clamp's lower bound in `sizing::size_position` — and therefore `tp_pct`, which stays `tp_mult`R |
//! | `conviction` | *(run parameter)* + `risk.conviction_min` | `size_position`'s leverage and margin, and the `LowConviction` gate — see [`CONVICTION_NOTE`] |
//! | `regime_vol_max` | `risk.regime_vol_max` | `risk::gate_regime`'s BTC-vol ceiling; the value `off` disables it |
//! | `cooldown_after_sl` | `risk.cooldown_after_sl_min` | `risk::gate_churn`'s post-stop window AND the screener pre-filter's exclusion clock |
//! | `daily_cap` | `risk.daily_cap` | `risk::gate_entry`'s daily entry counter — the capacity rail B3 found dominates every outcome (60 entries = `daily_cap`×days in every first-sweeps run) |
//! | `max_concurrent` | `risk.max_concurrent` | `risk::gate_entry`'s open-position ceiling — the other capacity rail |
//! | `min_score` | `screener.min_score` | `screener::screen`'s nomination threshold — signal selectivity |
//! | `margin` | `sizing.margin_min` AND `sizing.margin_max`, pinned together | `size_position`'s margin clamp — see [`MARGIN_NOTE`] |
//! | `tp_mult` | `sizing.tp_mult` | `size_position`'s `tp_pct = tp_mult * stop_pct` — the take-profit multiple of the stop distance (default 2.0, i.e. 2R) |
//!
//! Any dotted key `--set` accepts is also a valid sweep key, so the names above are aliases for
//! the common cases rather than the whole vocabulary.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::Config;

use super::engine::{self, MarketData};
use super::report::{self, RunReport};

/// Hard ceiling on grid size. A combo is a full replay of the window; a grid that expands
/// past this is a typo (`--sweep risk.daily_cap=1,2,3,...,20` twice) far more often than it is
/// an intention, and finding out after 40 minutes of replays is not a good way to learn.
pub const MAX_COMBOS: usize = 64;

/// The interpretation of a conviction sweep in mechanical mode, printed in every sweep summary.
///
/// With no analyst there is one conviction for the whole run, so `risk.conviction_min` is not
/// a threshold any more — it is a switch that either passes every entry or refuses every entry
/// (`GateRefusal::LowConviction`). Swept against a fixed 0.75 floor, `conviction=0.70` would
/// score zero trades and `0.75`/`0.80` would differ only in sizing: two thirds of a signal and
/// one third of an artifact of the comparison.
///
/// So a conviction axis moves the floor with it: each run is "the machine, run as if the
/// analyst always answered X", and what the sweep measures is what conviction mechanically
/// controls — leverage (`5 + 15·c·vol_ratio`) and margin (`bankroll·(0.01 + 0.04·c)`), i.e.
/// the notional at risk per trade. Naming `risk.conviction_min` yourself (as a `--set` or as
/// another axis) turns the alignment off and gets the gate-binding reading back.
pub const CONVICTION_NOTE: &str = "conviction axis: with no analyst every entry carries the same \
conviction, so risk.conviction_min moves with it (each run = 'the analyst always answered X'). \
What the axis measures is sizing — leverage 5+15·c·vol_ratio and margin bankroll·(0.01+0.04·c). \
Set risk.conviction_min explicitly to sweep the gate threshold instead.";

/// The config key a conviction axis aligns unless the caller names it themselves.
pub const CONVICTION_MIN_KEY: &str = "risk.conviction_min";

/// The mapping a `margin` sweep axis applies, printed in every sweep summary that uses it.
///
/// `sizing.margin_min` and `sizing.margin_max` are normally a CLAMP around the conviction-driven
/// raw margin (`bankroll·(0.01+0.04·conviction)`) — see `sizing::size_position`. In mechanical
/// replay mode every entry of a run already carries the same fixed conviction (spec Decision 2),
/// so that raw value is already constant; the clamp only decides whether it is used as-is,
/// floored or capped. Sweeping `margin_min`/`margin_max` independently would therefore mostly be
/// testing whether the clamp bites — not a position-size lever.
///
/// So a `margin` axis pins BOTH ends of the clamp to the SAME swept value
/// (`clamp(x, v, v) == v` for any `x`), which makes margin — and therefore notional
/// (`margin × leverage`) — exactly that many dollars on every entry, independent of conviction.
/// `margin=80` means "every position risks $80 of margin", full stop. This is what lets a
/// `margin` sweep be paired with a `daily_cap` or `max_concurrent` sweep (moved the opposite
/// direction) to compare FEWER, LARGER positions against MORE, SMALLER ones at a similar
/// aggregate notional — the capacity axes control how many positions fit in a day/at once, this
/// one controls how big each one is.
///
/// Escape hatch: exactly like `conviction` and `risk.conviction_min`, if the caller already
/// names `sizing.margin_min` or `sizing.margin_max` themselves (as `--set` or as their own axis),
/// that end of the clamp is left alone rather than pinned.
pub const MARGIN_NOTE: &str = "margin axis: pins sizing.margin_min AND sizing.margin_max to the \
swept value (clamp(x,v,v) = v), so every position's margin is exactly v dollars regardless of \
conviction — pair it with a daily_cap/max_concurrent sweep (moved the other way) to compare \
fewer-larger against more-smaller positions at a similar aggregate notional. Name \
sizing.margin_min/margin_max yourself to leave that end of the clamp alone.";

pub const MARGIN_MIN_KEY: &str = "sizing.margin_min";
pub const MARGIN_MAX_KEY: &str = "sizing.margin_max";

/// Notional traded across a run, derived from its fee bill.
///
/// Every fee the replay charges is exactly [`fills::FEE_RATE`] of that leg's notional
/// (`fills::fee`), so the bill divided by the rate IS the turnover that produced it. Deriving it
/// rather than accumulating a second counter is deliberate: a separate accumulator could drift
/// from the ledger the pnl is scored on, and turnover only matters here as the thing the fee
/// bill is proportional to.
pub fn turnover(fees: f64) -> f64 {
    fees / super::fills::FEE_RATE
}

/// What share of a LOSING run's loss is the fee bill — `fees / |net|`, the statistic B3's "fees
/// are 83% of losses" names (`docs/backtests/2026-08-09-first-sweeps`: gross −13.11, fees −62.67,
/// net −75.78 → 82.7%).
///
/// `None` for a run that made money: there is no loss to apportion, and `fees/|net|` on a
/// positive net would read as a share of a profit, which is a different (and misleading) number.
pub fn fee_share_of_loss(net_pnl: f64, fees: f64) -> Option<f64> {
    (net_pnl < 0.0).then(|| fees / -net_pnl * 100.0)
}

/// Token that disables a ceiling-shaped knob. Only `risk.regime_vol_max` takes it — an
/// infinite ceiling is exactly "the gate never refuses", with no second code path.
pub const OFF_TOKEN: &str = "off";

#[derive(Debug, clap::Args)]
pub struct SweepArgs {
    /// First UTC day to replay, `YYYY-MM-DD` (inclusive, from 00:00Z).
    #[arg(long)]
    pub from: String,
    /// Last UTC day to replay, `YYYY-MM-DD` (inclusive, through 23:59Z).
    #[arg(long)]
    pub to: String,
    /// Markets to replay, comma separated. Defaults to everything the cache holds.
    #[arg(long, value_delimiter = ',')]
    pub markets: Option<Vec<String>>,
    /// One axis of the grid: `key=v1,v2,...`. Repeatable — the runs are the cartesian product.
    #[arg(long = "sweep", value_name = "KEY=V1,V2", required = true)]
    pub sweep: Vec<String>,
    /// Conviction for axes that do not sweep it (spec Decision 2's analyst stand-in).
    #[arg(long, default_value_t = super::DEFAULT_CONVICTION)]
    pub conviction: f64,
    /// Config override applied to EVERY run in the grid, e.g. `--set risk.daily_cap=10`.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// Directory the sweep is written under.
    #[arg(long, default_value = super::REPORT_DIR_DEFAULT)]
    pub out: String,
    /// Name of the sweep directory, after the date. Defaults to the axis names.
    #[arg(long)]
    pub label: Option<String>,
}

/// What one axis varies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SweepKey {
    /// The run's fixed conviction (and, unless overridden, `risk.conviction_min` with it).
    Conviction,
    /// The position-size lever: `sizing.margin_min` AND `sizing.margin_max`, pinned together
    /// (unless the caller names either themselves) — see [`MARGIN_NOTE`].
    Margin,
    /// A dotted config knob, applied exactly as `--set` applies it.
    Config(String),
}

impl SweepKey {
    /// Resolve a user-facing axis name. The spec target questions and the capacity/fee knobs
    /// get short names; every dotted key `--set` accepts passes through unchanged.
    pub fn parse(name: &str) -> Self {
        match name {
            "conviction" => SweepKey::Conviction,
            "margin" => SweepKey::Margin,
            "stop_floor" | "stop_floor_pct" => SweepKey::Config("sizing.stop_floor_pct".to_string()),
            "regime" | "regime_vol_max" => SweepKey::Config("risk.regime_vol_max".to_string()),
            "cooldown_after_sl" | "cooldown_after_sl_min" => {
                SweepKey::Config("risk.cooldown_after_sl_min".to_string())
            }
            "daily_cap" => SweepKey::Config("risk.daily_cap".to_string()),
            "max_concurrent" => SweepKey::Config("risk.max_concurrent".to_string()),
            "min_score" => SweepKey::Config("screener.min_score".to_string()),
            "tp_mult" => SweepKey::Config("sizing.tp_mult".to_string()),
            other => SweepKey::Config(other.to_string()),
        }
    }

    /// The short name used in labels, slugs and the summary table.
    pub fn display(&self) -> &str {
        match self {
            SweepKey::Conviction => "conviction",
            SweepKey::Margin => "margin",
            SweepKey::Config(key) => key,
        }
    }
}

/// One axis: a key and the values it takes, in the order given.
#[derive(Debug, Clone, PartialEq)]
pub struct Axis {
    pub key: SweepKey,
    pub values: Vec<String>,
}

/// One point of the grid — one value per axis, in axis order.
#[derive(Debug, Clone, PartialEq)]
pub struct Combo {
    pub assignments: Vec<(SweepKey, String)>,
}

impl Combo {
    /// `stop_floor=0.6 conviction=0.75` — the human label, and the summary table's first column.
    pub fn label(&self) -> String {
        self.assignments
            .iter()
            .map(|(k, v)| format!("{}={}", k.display(), v))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Directory name for this combo's report. Path-safe and stable across runs.
    pub fn slug(&self) -> String {
        let raw = self
            .assignments
            .iter()
            .map(|(k, v)| format!("{}-{}", k.display(), v))
            .collect::<Vec<_>>()
            .join("__");
        raw.chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect()
    }
}

/// Parse `--sweep key=v1,v2` specs into axes.
///
/// A key repeated across several `--sweep` flags EXTENDS its axis rather than replacing it, so
/// `--sweep conviction=0.70 --sweep conviction=0.80` is the same grid as `conviction=0.70,0.80`.
/// Exact duplicate values collapse — a duplicate is two identical replays, never a tie-break.
pub fn parse_axes(specs: &[String]) -> anyhow::Result<Vec<Axis>> {
    let mut axes: Vec<Axis> = Vec::new();
    for spec in specs {
        let (name, raw) = spec
            .split_once('=')
            .with_context(|| format!("--sweep expects key=v1,v2, got {spec:?}"))?;
        let name = name.trim();
        if name.is_empty() {
            bail!("--sweep expects key=v1,v2, got {spec:?}");
        }
        let values: Vec<String> =
            raw.split(',').map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).collect();
        if values.is_empty() {
            bail!("--sweep {name}: no values in {spec:?}");
        }
        let key = SweepKey::parse(name);
        match axes.iter_mut().find(|a| a.key == key) {
            Some(axis) => {
                for v in values {
                    if !axis.values.contains(&v) {
                        axis.values.push(v);
                    }
                }
            }
            None => {
                let mut deduped: Vec<String> = Vec::with_capacity(values.len());
                for v in values {
                    if !deduped.contains(&v) {
                        deduped.push(v);
                    }
                }
                axes.push(Axis { key, values: deduped });
            }
        }
    }
    Ok(axes)
}

/// The cartesian product of `axes`, with the LAST axis varying fastest — odometer order, so a
/// grid reads down the page the way it was written.
pub fn expand(axes: &[Axis]) -> Vec<Combo> {
    let mut combos = vec![Combo { assignments: Vec::new() }];
    for axis in axes {
        let mut next = Vec::with_capacity(combos.len() * axis.values.len());
        for combo in &combos {
            for value in &axis.values {
                let mut c = combo.clone();
                c.assignments.push((axis.key.clone(), value.clone()));
                next.push(c);
            }
        }
        combos = next;
    }
    combos
}

/// What one combo actually runs as: a conviction and the `--set` list for [`super::load_config`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRun {
    pub conviction: f64,
    pub sets: Vec<String>,
}

/// Turn a combo into a runnable pair, on top of the sweep-wide `--conviction` and `--set`.
///
/// The conviction axis also pins `risk.conviction_min` (see [`CONVICTION_NOTE`]) and the margin
/// axis pins BOTH `sizing.margin_min` and `sizing.margin_max` (see [`MARGIN_NOTE`]) — in both
/// cases with the same escape hatch: if the caller already names the pinned key themselves (in
/// `--set` or as its own axis), that key is left alone rather than overwritten.
pub fn resolve(base_conviction: f64, base_sets: &[String], combo: &Combo) -> anyhow::Result<ResolvedRun> {
    let names_key = |sets: &[String], key: &str| {
        sets.iter().any(|s| s.split('=').next().map(str::trim) == Some(key))
    };
    let axis_pins = |key: &str| {
        combo.assignments.iter().any(|(k, _)| matches!(k, SweepKey::Config(kk) if kk == key))
    };
    let should_pin = |key: &str| !axis_pins(key) && !names_key(base_sets, key);

    let mut sets = base_sets.to_vec();
    let mut conviction = base_conviction;
    for (key, value) in &combo.assignments {
        match key {
            SweepKey::Conviction => {
                conviction = value
                    .parse::<f64>()
                    .with_context(|| format!("--sweep conviction: {value:?} is not a number"))?;
                if should_pin(CONVICTION_MIN_KEY) {
                    sets.push(format!("{CONVICTION_MIN_KEY}={conviction}"));
                }
            }
            SweepKey::Margin => {
                let margin: f64 = value
                    .parse()
                    .with_context(|| format!("--sweep margin: {value:?} is not a number"))?;
                if should_pin(MARGIN_MIN_KEY) {
                    sets.push(format!("{MARGIN_MIN_KEY}={margin}"));
                }
                if should_pin(MARGIN_MAX_KEY) {
                    sets.push(format!("{MARGIN_MAX_KEY}={margin}"));
                }
            }
            SweepKey::Config(k) => sets.push(format!("{k}={}", normalize_value(k, value))),
        }
    }
    Ok(ResolvedRun { conviction, sets })
}

/// `off` on a ceiling knob means "no ceiling", which as a number is infinity — the gate's own
/// `v > max` comparison then answers false for every vol, with no second branch to keep true.
/// TOML spells it `inf`. Every other key takes its value verbatim.
pub fn normalize_value(key: &str, value: &str) -> String {
    if key == "risk.regime_vol_max" && value.eq_ignore_ascii_case(OFF_TOKEN) {
        return "inf".to_string();
    }
    value.to_string()
}

/// One row of the ranked summary: the combo and the numbers a decision is made on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SweepRow {
    pub label: String,
    pub slug: String,
    /// Axis -> value, for machine consumers that would rather not parse `label`.
    pub combo: BTreeMap<String, String>,
    pub conviction: f64,
    pub overrides: Vec<String>,
    pub net_pnl: f64,
    /// Before fees — `net_pnl + fees`. Carried so a row shows whether a losing run lost on the
    /// trades or on the turnover that took them.
    pub gross_pnl: f64,
    pub unrealized_pnl: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub entries: i64,
    pub closes: i64,
    pub win_rate: f64,
    pub expectancy: f64,
    pub payoff: f64,
    pub fees: f64,
    pub tp: i64,
    pub sl: i64,
    pub time_stop: i64,
    pub open_at_end: i64,
    pub end_equity: f64,
    pub return_pct: f64,
    pub kill_days: Vec<String>,
}

impl SweepRow {
    pub fn from_report(combo: &Combo, rep: &RunReport) -> Self {
        let s = &rep.summary;
        Self {
            label: combo.label(),
            slug: combo.slug(),
            combo: combo
                .assignments
                .iter()
                .map(|(k, v)| (k.display().to_string(), v.clone()))
                .collect(),
            conviction: rep.params.conviction,
            overrides: rep.params.overrides.clone(),
            net_pnl: s.net_pnl,
            gross_pnl: s.gross_pnl,
            unrealized_pnl: s.unrealized_pnl,
            max_drawdown: s.max_drawdown,
            max_drawdown_pct: s.max_drawdown_pct,
            entries: s.entries,
            closes: s.closes,
            win_rate: s.win_rate,
            expectancy: s.expectancy,
            payoff: s.payoff,
            fees: s.fees,
            tp: rep.exit_mix.tp,
            sl: rep.exit_mix.sl,
            time_stop: rep.exit_mix.time_stop,
            open_at_end: rep.exit_mix.open_at_end,
            end_equity: s.end_equity,
            return_pct: s.return_pct,
            kill_days: s.kill_days.clone(),
        }
    }
}

/// Rank by net descending, ties broken by the SHALLOWER drawdown, then by label (spec
/// Decision 7). The label tie-break is what makes the ranking total: two combos with identical
/// numbers — which a degenerate axis produces routinely — must not swap places between runs.
///
/// A NaN net cannot occur (every arm of the report guards its divisions) but sorting must not
/// depend on that: an unorderable pair keeps its input order.
pub fn rank(rows: &mut [SweepRow]) {
    rows.sort_by(|a, b| {
        b.net_pnl
            .partial_cmp(&a.net_pnl)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.max_drawdown.partial_cmp(&b.max_drawdown).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.label.cmp(&b.label))
    });
}

/// The whole sweep, as JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SweepReport {
    pub label: String,
    pub from: String,
    pub to: String,
    pub from_ms: i64,
    pub to_ms: i64,
    pub markets: usize,
    pub bars: i64,
    /// The axes, in the order they were given.
    pub axes: BTreeMap<String, Vec<String>>,
    /// Sweep-wide `--set` overrides.
    pub base_overrides: Vec<String>,
    pub caveats: Vec<String>,
    /// Ranked: best net first.
    pub rows: Vec<SweepRow>,
}

/// Header caveats for a sweep: [`report::caveats`], with the conviction clause rewritten when
/// conviction is itself an axis (no single number is true of the grid).
pub fn sweep_caveats(cfg: &Config, axes: &[Axis], conviction: f64) -> Vec<String> {
    let mut out = match axes.iter().find(|a| a.key == SweepKey::Conviction) {
        Some(axis) => report::caveats_with(
            cfg,
            &format!("fixed per run — this grid sweeps it over {}", axis.values.join(", ")),
        ),
        None => report::caveats(cfg, conviction),
    };
    if axes.iter().any(|a| a.key == SweepKey::Conviction) {
        out.push(CONVICTION_NOTE.to_string());
    }
    if axes.iter().any(|a| a.key == SweepKey::Margin) {
        out.push(MARGIN_NOTE.to_string());
    }
    out
}

/// The committed artifact: the ranked table, its caveats, and how to reproduce any row.
pub fn summary_markdown(sweep: &SweepReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# backtest sweep — {}\n", sweep.label);
    let _ = writeln!(
        out,
        "**{} .. {} UTC** · {} bars · {} markets · {} run{}\n",
        sweep.from,
        sweep.to,
        sweep.bars,
        sweep.markets,
        sweep.rows.len(),
        if sweep.rows.len() == 1 { "" } else { "s" }
    );
    let _ = writeln!(out, "Axes:\n");
    for (key, values) in &sweep.axes {
        let _ = writeln!(out, "- `{key}` = {}", values.join(", "));
    }
    if !sweep.base_overrides.is_empty() {
        let _ = writeln!(out, "\nApplied to every run: `{}`", sweep.base_overrides.join("` `"));
    }

    let _ = writeln!(out, "\n## What this is not\n");
    for c in &sweep.caveats {
        let _ = writeln!(out, "- {c}");
    }

    let _ = writeln!(out, "\n## Ranked — by net, ties to the shallower drawdown\n");
    let _ = writeln!(
        out,
        "| # | combo | net | maxDD | maxDD% | entries | closes | win% | expectancy | payoff | tp | sl | time_stop | open | equity end | return% |"
    );
    let _ = writeln!(out, "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for (i, r) in sweep.rows.iter().enumerate() {
        let _ = writeln!(
            out,
            "| {} | `{}` | {:+.2} | {:.2} | {:.2}% | {} | {} | {:.1}% | {:+.2} | {:.2} | {} | {} | {} | {} | {:.2} | {:+.2}% |",
            i + 1,
            r.label,
            r.net_pnl,
            r.max_drawdown,
            r.max_drawdown_pct,
            r.entries,
            r.closes,
            r.win_rate,
            r.expectancy,
            r.payoff,
            r.tp,
            r.sl,
            r.time_stop,
            r.open_at_end,
            r.end_equity,
            r.return_pct
        );
    }

    let _ = writeln!(
        out,
        "\n`net` is REALIZED only; a run holding positions at the end carries the rest in \
         `unrealized`, and `equity end` is the sum of both.\n"
    );

    let _ = writeln!(out, "\n## Fee anatomy — what the turnover cost\n");
    let _ = writeln!(
        out,
        "`gross` is before fees (`net + fees`); `turnover` is the notional those fees were \
         charged on (fee is a flat {:.2}bp per side, so turnover = fees ÷ {}); `fees ÷ |net|` is \
         what share of a LOSING run's loss the fee bill is, and is blank for a run that made \
         money.\n",
        super::fills::FEE_RATE * 10_000.0,
        super::fills::FEE_RATE
    );
    let _ = writeln!(
        out,
        "| combo | gross | fees | turnover | fees ÷ \\|net\\| | unrealized | kill-switch days | full report |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---|---|");
    for r in &sweep.rows {
        let share = match fee_share_of_loss(r.net_pnl, r.fees) {
            Some(pct) => format!("{pct:.1}%"),
            None => String::new(),
        };
        let _ = writeln!(
            out,
            "| `{}` | {:+.2} | {:.2} | {:.0} | {} | {:+.2} | {} | [`{}/report.md`]({}/report.md) |",
            r.label,
            r.gross_pnl,
            r.fees,
            turnover(r.fees),
            share,
            r.unrealized_pnl,
            if r.kill_days.is_empty() { "none".to_string() } else { r.kill_days.join(", ") },
            r.slug,
            r.slug
        );
    }

    let _ = writeln!(out, "\n## Reproduce\n");
    let _ = writeln!(out, "```bash");
    for r in &sweep.rows {
        let sets: String = r.overrides.iter().map(|s| format!(" --set {s}")).collect();
        let _ = writeln!(
            out,
            "kestreld backtest run --from {} --to {} --conviction {}{}   # {}",
            sweep.from.split(' ').next().unwrap_or(&sweep.from),
            sweep.to.split(' ').next().unwrap_or(&sweep.to),
            r.conviction,
            sets,
            r.label
        );
    }
    let _ = writeln!(out, "```");
    out
}

/// The one-screen table the CLI prints when a sweep finishes.
pub fn human_table(sweep: &SweepReport) -> String {
    let mut out = String::new();
    let width = sweep.rows.iter().map(|r| r.label.len()).max().unwrap_or(5).max(5);
    let _ = writeln!(
        out,
        "{:<3} {:<width$} {:>10} {:>10} {:>9} {:>9} {:>8} {:>8} {:>7} {:>7}",
        "#", "combo", "net", "gross", "fees", "maxDD", "entries", "closes", "win%", "payoff"
    );
    for (i, r) in sweep.rows.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:<3} {:<width$} {:>+10.2} {:>+10.2} {:>9.2} {:>9.2} {:>8} {:>8} {:>6.1}% {:>7.2}",
            i + 1,
            r.label,
            r.net_pnl,
            r.gross_pnl,
            r.fees,
            r.max_drawdown,
            r.entries,
            r.closes,
            r.win_rate,
            r.payoff
        );
    }
    out
}

/// Default label: the axes that were swept.
pub fn default_label(axes: &[Axis]) -> String {
    let names: Vec<String> = axes
        .iter()
        .map(|a| a.key.display().rsplit('.').next().unwrap_or(a.key.display()).to_string())
        .collect();
    format!("sweep-{}", names.join("-"))
}

/// Run the grid. `data` is the shared tape; every combo replays it under its own config.
pub fn run_grid(
    config_path: &str,
    args: &SweepArgs,
    axes: &[Axis],
    combos: &[Combo],
    data: &BTreeMap<String, MarketData>,
    from_ms: i64,
    to_ms: i64,
) -> anyhow::Result<(SweepReport, Vec<(Combo, RunReport)>)> {
    let markets: Vec<String> = data.keys().cloned().collect();
    let mut results: Vec<(Combo, RunReport)> = Vec::with_capacity(combos.len());

    for (i, combo) in combos.iter().enumerate() {
        let resolved = resolve(args.conviction, &args.set, combo)?;
        let cfg = super::load_config(config_path, &resolved.sets)
            .with_context(|| format!("config for combo {}", combo.label()))?;
        let params = engine::RunParams {
            from_ms,
            to_ms,
            conviction: resolved.conviction,
            markets: markets.clone(),
            overrides: resolved.sets.clone(),
        };
        info!(combo = %combo.label(), run = i + 1, of = combos.len(), "sweep replay");
        println!("[{}/{}] {}", i + 1, combos.len(), combo.label());
        let outcome = engine::run(&cfg, data, params);
        let rep = report::build(&cfg, &outcome);
        println!(
            "        net {:+.2}   maxDD {:.2}   entries {}   closes {}   win {:.1}%",
            rep.summary.net_pnl,
            rep.summary.max_drawdown,
            rep.summary.entries,
            rep.summary.closes,
            rep.summary.win_rate
        );
        results.push((combo.clone(), rep));
    }

    // The header describes the grid, so it is built from the base config — the one every run
    // starts from before its own axis values are applied.
    let base_cfg = super::load_config(config_path, &args.set)?;
    let mut rows: Vec<SweepRow> =
        results.iter().map(|(combo, rep)| SweepRow::from_report(combo, rep)).collect();
    rank(&mut rows);

    let sweep = SweepReport {
        label: args.label.clone().unwrap_or_else(|| default_label(axes)),
        from: report::stamp(from_ms),
        to: report::stamp(to_ms),
        from_ms,
        to_ms,
        markets: markets.len(),
        bars: results.first().map(|(_, r)| r.summary.bars).unwrap_or(0),
        axes: axes.iter().map(|a| (a.key.display().to_string(), a.values.clone())).collect(),
        base_overrides: args.set.clone(),
        caveats: sweep_caveats(&base_cfg, axes, args.conviction),
        rows,
    };
    Ok((sweep, results))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::engine::tests::{fixture_cfg, golden_fixture, golden_params};
    use crate::backtest::engine::{PositionRow, run};

    fn axes(specs: &[&str]) -> Vec<Axis> {
        parse_axes(&specs.iter().map(|s| s.to_string()).collect::<Vec<_>>()).expect("axes")
    }

    #[test]
    fn axes_parse_short_names_dotted_keys_and_repeats() {
        let a = axes(&["stop_floor=0.6,1.0,1.4"]);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].key, SweepKey::Config("sizing.stop_floor_pct".into()));
        assert_eq!(a[0].values, vec!["0.6", "1.0", "1.4"]);

        // every spec alias lands on the knob it names
        assert_eq!(axes(&["conviction=0.7"])[0].key, SweepKey::Conviction);
        assert_eq!(
            axes(&["regime=1.5"])[0].key,
            SweepKey::Config("risk.regime_vol_max".into()),
        );
        assert_eq!(
            axes(&["cooldown_after_sl=30,120"])[0].key,
            SweepKey::Config("risk.cooldown_after_sl_min".into()),
        );
        // a dotted key needs no alias
        assert_eq!(axes(&["risk.daily_cap=3"])[0].key, SweepKey::Config("risk.daily_cap".into()));
        // and an alias and its dotted form are ONE axis, not two
        let merged = axes(&["cooldown_after_sl=30", "risk.cooldown_after_sl_min=120"]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        assert_eq!(merged[0].values, vec!["30", "120"], "a repeated key extends its axis");

        // batch B4: the capacity/fee axes, same alias pattern
        assert_eq!(axes(&["daily_cap=5,10"])[0].key, SweepKey::Config("risk.daily_cap".into()));
        assert_eq!(
            axes(&["max_concurrent=1,3"])[0].key,
            SweepKey::Config("risk.max_concurrent".into())
        );
        assert_eq!(
            axes(&["min_score=1.8,3.5"])[0].key,
            SweepKey::Config("screener.min_score".into())
        );
        assert_eq!(axes(&["tp_mult=1.5,2.0"])[0].key, SweepKey::Config("sizing.tp_mult".into()));
        assert_eq!(axes(&["margin=20,80"])[0].key, SweepKey::Margin);

        // whitespace, and exact duplicates collapsing
        let w = axes(&[" conviction = 0.70 , 0.75 , 0.70 "]);
        assert_eq!(w[0].values, vec!["0.70", "0.75"]);

        for bad in ["stop_floor", "=1,2", "stop_floor=", "stop_floor=,"] {
            assert!(parse_axes(&[bad.to_string()]).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn expansion_is_the_cartesian_product_with_the_last_axis_fastest() {
        let grid = expand(&axes(&["stop_floor=0.6,1.0", "conviction=0.70,0.75,0.80"]));
        assert_eq!(grid.len(), 6, "2 × 3");
        let labels: Vec<String> = grid.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            vec![
                "sizing.stop_floor_pct=0.6 conviction=0.70",
                "sizing.stop_floor_pct=0.6 conviction=0.75",
                "sizing.stop_floor_pct=0.6 conviction=0.80",
                "sizing.stop_floor_pct=1.0 conviction=0.70",
                "sizing.stop_floor_pct=1.0 conviction=0.75",
                "sizing.stop_floor_pct=1.0 conviction=0.80",
            ]
        );
        // three axes multiply, and every combo names every axis exactly once
        let three = expand(&axes(&["a.b=1,2", "c.d=3,4", "e.f=5,6,7"]));
        assert_eq!(three.len(), 8 + 4, "2 × 2 × 3");
        assert!(three.iter().all(|c| c.assignments.len() == 3));
        // slugs are path-safe and unique
        let slugs: Vec<String> = grid.iter().map(|c| c.slug()).collect();
        assert_eq!(slugs[0], "sizing.stop_floor_pct-0.6__conviction-0.70");
        let mut sorted = slugs.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), slugs.len(), "slugs collide: {slugs:?}");
        assert!(slugs.iter().all(|s| !s.contains('/') && !s.contains(' ')));
        // an empty grid is one empty combo, not zero runs
        assert_eq!(expand(&[]), vec![Combo { assignments: vec![] }]);
    }

    #[test]
    fn a_config_axis_resolves_to_a_set_override_on_top_of_the_base() {
        let combo = &expand(&axes(&["cooldown_after_sl=30"]))[0];
        let base = vec!["risk.daily_cap=4".to_string()];
        let r = resolve(0.75, &base, combo).expect("resolve");
        assert_eq!(r.conviction, 0.75, "a config axis never moves conviction");
        assert_eq!(
            r.sets,
            vec!["risk.daily_cap=4".to_string(), "risk.cooldown_after_sl_min=30".to_string()],
            "the axis is appended AFTER the sweep-wide sets, so an axis wins a collision"
        );
    }

    /// The documented interpretation ([`CONVICTION_NOTE`]): a conviction axis moves the gate
    /// floor with it, unless the caller pins the floor themselves.
    #[test]
    fn a_conviction_axis_moves_the_gate_floor_unless_it_is_pinned() {
        let grid = expand(&axes(&["conviction=0.70,0.80"]));
        let low = resolve(0.75, &[], &grid[0]).expect("resolve");
        assert_eq!(low.conviction, 0.70);
        assert_eq!(low.sets, vec!["risk.conviction_min=0.7".to_string()]);
        let high = resolve(0.75, &[], &grid[1]).expect("resolve");
        assert_eq!(high.sets, vec!["risk.conviction_min=0.8".to_string()]);

        // pinned via --set: the axis leaves it alone
        let pinned = resolve(0.75, &["risk.conviction_min=0.75".to_string()], &grid[0]).expect("resolve");
        assert_eq!(pinned.conviction, 0.70);
        assert_eq!(pinned.sets, vec!["risk.conviction_min=0.75".to_string()], "no alignment added");

        // pinned as its own axis: same
        let both = expand(&axes(&["conviction=0.70", "risk.conviction_min=0.75"]));
        let r = resolve(0.75, &[], &both[0]).expect("resolve");
        assert_eq!(r.conviction, 0.70);
        assert_eq!(r.sets, vec!["risk.conviction_min=0.75".to_string()]);

        assert!(resolve(0.75, &[], &expand(&axes(&["conviction=high"]))[0]).is_err());
    }

    /// The documented interpretation ([`MARGIN_NOTE`]): a margin axis pins BOTH ends of the
    /// clamp to the same value, unless the caller pins one end themselves.
    #[test]
    fn a_margin_axis_pins_both_clamp_ends_unless_they_are_pinned() {
        let grid = expand(&axes(&["margin=20,80"]));
        let low = resolve(0.75, &[], &grid[0]).expect("resolve");
        assert_eq!(low.conviction, 0.75, "a margin axis never moves conviction");
        assert_eq!(
            low.sets,
            vec!["sizing.margin_min=20".to_string(), "sizing.margin_max=20".to_string()],
            "both ends pin to the SAME value"
        );
        let high = resolve(0.75, &[], &grid[1]).expect("resolve");
        assert_eq!(
            high.sets,
            vec!["sizing.margin_min=80".to_string(), "sizing.margin_max=80".to_string()]
        );

        // pinned via --set: the axis leaves that end alone, but still pins the other
        let min_pinned =
            resolve(0.75, &["sizing.margin_min=5".to_string()], &grid[0]).expect("resolve");
        assert_eq!(
            min_pinned.sets,
            vec!["sizing.margin_min=5".to_string(), "sizing.margin_max=20".to_string()],
            "margin_min left alone, margin_max still pinned"
        );

        // pinned as its own axis: same — margin_min still pins (from the Margin branch),
        // margin_max does not (the later Config assignment claims it), in assignment order
        let both = expand(&axes(&["margin=20", "sizing.margin_max=999"]));
        let r = resolve(0.75, &[], &both[0]).expect("resolve");
        assert_eq!(r.sets, vec!["sizing.margin_min=20".to_string(), "sizing.margin_max=999".to_string()]);

        assert!(resolve(0.75, &[], &expand(&axes(&["margin=cheap"]))[0]).is_err());
    }

    #[test]
    fn regime_off_is_an_infinite_ceiling_that_the_config_round_trips() {
        assert_eq!(normalize_value("risk.regime_vol_max", "off"), "inf");
        assert_eq!(normalize_value("risk.regime_vol_max", "OFF"), "inf");
        assert_eq!(normalize_value("risk.regime_vol_max", "1.5"), "1.5");
        // `off` is only a ceiling word — anywhere else it stays a (failing) literal
        assert_eq!(normalize_value("risk.daily_cap", "off"), "off");

        let combo = &expand(&axes(&["regime=off"]))[0];
        let sets = resolve(0.75, &[], combo).expect("resolve").sets;
        assert_eq!(sets, vec!["risk.regime_vol_max=inf".to_string()]);
        // and it survives parse -> re-serialize -> parse in load_config
        let cfg = crate::backtest::load_config("kestreld.toml", &sets).expect("inf config");
        assert!(cfg.risk.regime_vol_max.is_infinite(), "{}", cfg.risk.regime_vol_max);
        assert!(crate::risk::Risk::new(cfg.risk).gate_regime(Some(99.0)).is_ok(), "gate is off");
    }

    /// Every sweepable key must demonstrably change what the ENGINE does, not just what the
    /// config says — the golden fixture is scripted tightly enough that each one has a visible
    /// consequence.
    #[test]
    fn each_swept_key_changes_engine_behaviour() {
        let run_with = |sets: Vec<String>, conviction: f64| {
            let cfg = {
                // The fixture config, not kestreld.toml: the golden run's arithmetic is pinned.
                let mut c = fixture_cfg();
                for s in &sets {
                    let (k, v) = crate::backtest::parse_override(s).expect("override");
                    match (k.as_str(), v) {
                        ("sizing.stop_floor_pct", toml::Value::Float(f)) => c.sizing.stop_floor_pct = f,
                        ("risk.regime_vol_max", toml::Value::Float(f)) => c.risk.regime_vol_max = f,
                        ("risk.cooldown_after_sl_min", toml::Value::Integer(i)) => {
                            c.risk.cooldown_after_sl_min = i as u64
                        }
                        ("risk.conviction_min", toml::Value::Float(f)) => c.risk.conviction_min = f,
                        ("risk.daily_cap", toml::Value::Integer(i)) => c.risk.daily_cap = i as usize,
                        ("risk.max_concurrent", toml::Value::Integer(i)) => {
                            c.risk.max_concurrent = i as usize
                        }
                        ("screener.min_score", toml::Value::Float(f)) => c.screener.min_score = f,
                        ("screener.min_score", toml::Value::Integer(i)) => c.screener.min_score = i as f64,
                        ("sizing.margin_min", toml::Value::Float(f)) => c.sizing.margin_min = f,
                        ("sizing.margin_min", toml::Value::Integer(i)) => c.sizing.margin_min = i as f64,
                        ("sizing.margin_max", toml::Value::Float(f)) => c.sizing.margin_max = f,
                        ("sizing.margin_max", toml::Value::Integer(i)) => c.sizing.margin_max = i as f64,
                        ("sizing.tp_mult", toml::Value::Float(f)) => c.sizing.tp_mult = f,
                        ("sizing.tp_mult", toml::Value::Integer(i)) => c.sizing.tp_mult = i as f64,
                        other => panic!("unhandled override {other:?}"),
                    }
                }
                c
            };
            run(&cfg, &golden_fixture(), engine::RunParams { conviction, ..golden_params() })
        };
        let resolved = |spec: &str| {
            let combo = expand(&axes(&[spec]))[0].clone();
            resolve(0.75, &[], &combo).expect("resolve")
        };

        // stop_floor -> the bracket distance. The fixture's vol is ~0.064%, so 1.5·vol is under
        // every floor swept and the floor IS the stop; tp stays 2R off it.
        for (spec, stop) in [("stop_floor=0.6", 0.6f64), ("stop_floor=1.0", 1.0), ("stop_floor=1.4", 1.4)] {
            let r = resolved(spec);
            let out = run_with(r.sets, r.conviction);
            let p = &out.positions[0].pos;
            assert!(
                (p.sl_px - p.entry_px * (1.0 - stop / 100.0)).abs() < 1e-9,
                "{spec}: sl {} off entry {}",
                p.sl_px,
                p.entry_px
            );
            assert!((p.tp_px - p.entry_px * (1.0 + 2.0 * stop / 100.0)).abs() < 1e-9, "{spec}: tp is 2R");
        }
        // The floor moves the TARGET too (2R), so it decides whether an exit happens at all:
        // AAA's minute-65 spike tops at 103.0, which clears a 0.6% floor's 1.2% target
        // (101.73) and a 1.0%'s 2.0% (102.53) but not a 1.4%'s 2.8% (103.33).
        let outcome_of = |spec: &str| {
            let r = resolved(spec);
            let out = run_with(r.sets, r.conviction);
            let aaa: &PositionRow =
                out.positions.iter().find(|p| p.pos.market == "AAA").expect("AAA traded");
            aaa.close_action.clone()
        };
        assert_eq!(outcome_of("stop_floor=0.6").as_deref(), Some("tp"));
        assert_eq!(outcome_of("stop_floor=1.0").as_deref(), Some("tp"));
        assert_eq!(outcome_of("stop_floor=1.4"), None, "a 2.8% target is outside the spike");

        // conviction -> sizing. margin = 1000·(0.01+0.04c); leverage = round(5+15·c·1.6),
        // which is 21.8 and 24.2 raw — both clamped to the 20 ceiling, so only margin (and
        // therefore notional) actually moves here.
        for (spec, margin, leverage) in [("conviction=0.70", 38.0f64, 20.0f64), ("conviction=0.80", 42.0, 20.0)]
        {
            let r = resolved(spec);
            let out = run_with(r.sets, r.conviction);
            let p = &out.positions[0].pos;
            assert!((p.margin - margin).abs() < 1e-9, "{spec}: margin {}", p.margin);
            assert!((p.leverage - leverage).abs() < 1e-9, "{spec}: leverage {}", p.leverage);
            assert!(!out.positions.is_empty(), "{spec}: the aligned floor keeps the gate open");
        }
        // and WITHOUT the alignment the low run is refused outright — the degeneracy the
        // alignment exists to avoid.
        let unaligned = run_with(vec!["risk.conviction_min=0.75".to_string()], 0.70);
        assert!(unaligned.positions.is_empty(), "0.70 under a 0.75 floor takes no trade at all");
        assert!(unaligned.refusals.get("LowConviction").copied().unwrap_or(0) > 0);

        // regime -> the BTC-vol ceiling. The golden fixture has no BTC row, so the gate is
        // inactive there; a hot-BTC fixture is what shows the knob biting.
        let mut hot = golden_fixture();
        let btc: Vec<crate::backtest::cache::CachedCandle> = (0..400)
            .map(|i| {
                let t = crate::backtest::engine::tests::T0 + i * 60_000;
                let px = if i % 2 == 0 { 100.0 } else { 101.0 };
                crate::backtest::cache::CachedCandle { t, o: px, h: px, l: px, c: px, v: 100.0 }
            })
            .collect();
        hot.insert("BTC".to_string(), MarketData::new("BTC", btc, vec![]));
        let regime_run = |spec: &str| {
            let r = resolved(spec);
            let mut c = fixture_cfg();
            for s in &r.sets {
                let (k, v) = crate::backtest::parse_override(s).expect("override");
                if k == "risk.regime_vol_max" {
                    c.risk.regime_vol_max = match v {
                        toml::Value::Float(f) => f,
                        toml::Value::Integer(i) => i as f64,
                        other => panic!("regime value {other:?}"),
                    };
                }
            }
            run(&c, &hot, golden_params())
        };
        let on = regime_run("regime=0.5");
        assert!(on.refusals.get("Regime").copied().unwrap_or(0) > 0, "refusals {:?}", on.refusals);
        let off = regime_run("regime=off");
        assert_eq!(off.refusals.get("Regime"), None, "`off` must never refuse: {:?}", off.refusals);
        assert!(
            off.positions.len() > on.positions.iter().filter(|p| p.pos.market != "BTC").count(),
            "turning the regime gate off has to let trades through"
        );

        // cooldown_after_sl -> BBB's re-entry 45 minutes after its stop-out.
        let long_cd = run_with(resolved("cooldown_after_sl=120").sets, 0.75);
        let short_cd = run_with(resolved("cooldown_after_sl=30").sets, 0.75);
        assert_eq!(long_cd.positions.len(), 2, "120m swallows the re-run");
        assert_eq!(short_cd.positions.len(), 3, "30m lets it through");

        // daily_cap -> the capacity rail B3 found dominates every outcome (docs/backtests
        // 2026-08-09-first-sweeps: 60 entries = daily_cap × days in EVERY run). A cap of 1
        // trades only the day's first signal; BBB's later one is refused outright.
        let capped = run_with(resolved("daily_cap=1").sets, 0.75);
        assert_eq!(capped.positions.len(), 1, "only the day's first signal trades");
        assert_eq!(capped.positions[0].pos.market, "AAA");
        assert!(
            capped.refusals.get("DailyCap").copied().unwrap_or(0) > 0,
            "refusals {:?}",
            capped.refusals
        );
        let uncapped = run_with(resolved("daily_cap=20").sets, 0.75);
        assert_eq!(uncapped.positions.len(), 2, "the fixture's own cap trades both signals");

        // max_concurrent -> the other capacity rail. AAA is flattened after its fill (never
        // touches its bracket again) so it stays open and overlaps BBB's minute-201 signal.
        //
        // AAA being open also EXCLUDES it from the screener's own candidate pool
        // (`Replay::excluded`) — not just from re-nomination, from the cross-sectional z-score
        // baseline too, which would otherwise quietly drop BBB's score below the fixture's
        // min_score (3 markets score BBB at ~4.44; with AAA excluded, 2 markets score it at
        // ~3.2 < 3.5 — the exact trap `a_take_profit_close_only_costs_the_base_cooldown`'s
        // comment already warns about for a shrunk universe). Four extra flat laggards keep the
        // pool large enough that losing one market to `excluded` cannot swing BBB below
        // threshold either way.
        let mut overlap = golden_fixture();
        {
            let aaa = overlap.get_mut("AAA").expect("AAA");
            let mut candles = aaa.series.candles().to_vec();
            for c in candles.iter_mut().skip(61) {
                *c = crate::backtest::cache::CachedCandle {
                    t: c.t,
                    o: 100.5,
                    h: 100.5,
                    l: 100.5,
                    c: 100.5,
                    v: 100.0,
                };
            }
            *aaa = MarketData::new("AAA", candles, vec![]);
        }
        for filler in ["DDD", "EEE", "FFF", "GGG"] {
            let candles: Vec<crate::backtest::cache::CachedCandle> = (0..400i64)
                .map(|i| crate::backtest::cache::CachedCandle {
                    t: crate::backtest::engine::tests::T0 + i * 60_000,
                    o: 100.0,
                    h: 100.0,
                    l: 100.0,
                    c: 100.0,
                    v: 100.0,
                })
                .collect();
            overlap.insert(filler.to_string(), MarketData::new(filler, candles, vec![]));
        }
        let mut tight_cfg = fixture_cfg();
        tight_cfg.risk.max_concurrent = 1;
        let tight = run(&tight_cfg, &overlap, golden_params());
        assert_eq!(tight.positions.len(), 1, "AAA holds the only concurrency slot");
        assert!(
            tight.refusals.get("MaxConcurrent").copied().unwrap_or(0) > 0,
            "refusals {:?}",
            tight.refusals
        );
        let loose = run(&fixture_cfg(), &overlap, golden_params());
        assert_eq!(
            loose.positions.iter().filter(|p| p.pos.market == "BBB").count(),
            1,
            "default max_concurrent=5 lets BBB in alongside AAA: {:?}",
            loose.positions.iter().map(|p| p.pos.market.clone()).collect::<Vec<_>>()
        );
        // and the sweep plumbing resolves the same knob `run_with` was just given by hand
        assert_eq!(resolved("max_concurrent=1").sets, vec!["risk.max_concurrent=1".to_string()]);

        // min_score -> signal selectivity. The fixture's fresh moves score 4.443 (see
        // fixture_cfg's doc comment); a floor above that nominates nothing.
        let selective = run_with(resolved("min_score=5.0").sets, 0.75);
        assert!(selective.positions.is_empty(), "4.443 < 5.0 never nominates");
        assert!(selective.refusals.is_empty(), "a market that never nominates never reaches a gate");
        let permissive = run_with(resolved("min_score=3.5").sets, 0.75);
        assert_eq!(permissive.positions.len(), 2, "the fixture's own floor trades both signals");

        // margin -> the position-size lever (MARGIN_NOTE): pins margin_min AND margin_max to
        // the SAME dollar value, overriding the conviction-driven raw margin ($40 at 0.75)
        // entirely. Leverage is untouched (it reads vol/conviction, not margin), so notional
        // scales exactly with the pinned margin.
        for (spec, margin) in [("margin=15.0", 15.0f64), ("margin=80.0", 80.0)] {
            let r = resolved(spec);
            assert_eq!(r.conviction, 0.75, "a margin axis never moves conviction");
            let out = run_with(r.sets, r.conviction);
            let p = &out.positions[0].pos;
            assert!((p.margin - margin).abs() < 1e-9, "{spec}: margin {}", p.margin);
            assert!((p.leverage - 20.0).abs() < 1e-9, "{spec}: leverage untouched by margin");
            assert!(
                (p.size * p.entry_px - margin * 20.0).abs() < 1e-6,
                "{spec}: notional tracks the pinned margin"
            );
        }

        // tp_mult -> the take-profit multiple of the stop distance. The stop stays the 1.0%
        // floor (fixture vol ~0.064%); only the target moves, which decides whether AAA's
        // minute-65 spike (high 103.0) reaches it at all.
        for (spec, mult) in [("tp_mult=1.0", 1.0f64), ("tp_mult=3.0", 3.0)] {
            let r = resolved(spec);
            let out = run_with(r.sets, r.conviction);
            let p = &out.positions[0].pos;
            assert!((p.sl_px - p.entry_px * 0.99).abs() < 1e-9, "{spec}: stop untouched by tp_mult");
            assert!(
                (p.tp_px - p.entry_px * (1.0 + mult / 100.0)).abs() < 1e-9,
                "{spec}: tp {} vs entry {}",
                p.tp_px,
                p.entry_px
            );
        }
        let closes_at = |spec: &str| {
            let r = resolved(spec);
            let out = run_with(r.sets, r.conviction);
            let aaa: &PositionRow =
                out.positions.iter().find(|p| p.pos.market == "AAA").expect("AAA traded");
            aaa.close_action.clone()
        };
        assert_eq!(closes_at("tp_mult=1.0").as_deref(), Some("tp"), "a 1% target clears the spike");
        assert_eq!(closes_at("tp_mult=3.0"), None, "a ~3.09% target is outside the spike (high 103.0)");
    }

    fn row(label: &str, net: f64, dd: f64) -> SweepRow {
        SweepRow {
            label: label.to_string(),
            slug: label.to_string(),
            combo: BTreeMap::new(),
            conviction: 0.75,
            overrides: vec![],
            net_pnl: net,
            gross_pnl: net + 1.0,
            unrealized_pnl: 0.0,
            max_drawdown: dd,
            max_drawdown_pct: dd / 10.0,
            entries: 1,
            closes: 1,
            win_rate: 100.0,
            expectancy: net,
            payoff: 0.0,
            fees: 1.0,
            tp: 1,
            sl: 0,
            time_stop: 0,
            open_at_end: 0,
            end_equity: 1000.0 + net,
            return_pct: net / 10.0,
            kill_days: vec![],
        }
    }

    /// The fee-anatomy derivations. B3's headline ("fees are 83% of losses") is the worked
    /// example, so it is the one pinned here.
    #[test]
    fn turnover_and_fee_share_are_derived_from_the_fee_bill() {
        // 2026-08-09-first-sweeps, best run: gross -13.11, fees 62.67, net -75.78.
        let (net, fees) = (-75.78, 62.67);
        let share = fee_share_of_loss(net, fees).expect("a losing run has a loss to apportion");
        assert!((share - 82.70).abs() < 0.01, "share {share}");

        // 62.67 of fee at 7.5bp a side is $83_560 of notional — and the round trip identity
        // holds: 15bp of the turnover a 60-entry run round-tripped.
        let t = turnover(fees);
        assert!((t - 62.67 / 0.00075).abs() < 1e-6, "turnover {t}");
        assert!((t - 83_560.0).abs() < 1.0, "turnover {t}");
        assert!((turnover(fees) * super::super::fills::FEE_RATE - fees).abs() < 1e-9);

        // A run that made money has no loss to apportion.
        assert_eq!(fee_share_of_loss(5.0, 1.0), None);
        assert_eq!(fee_share_of_loss(0.0, 1.0), None, "flat is not a loss either");
        assert_eq!(turnover(0.0), 0.0, "no fee, no turnover");
    }

    #[test]
    fn ranking_is_net_then_shallower_drawdown_then_label() {
        let mut rows = vec![
            row("c", 5.0, 3.0),
            row("a", 12.0, 40.0),
            row("b", 5.0, 1.0),
            row("d", -2.0, 0.0),
            row("aa", 5.0, 1.0),
        ];
        rank(&mut rows);
        let order: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
        assert_eq!(
            order,
            vec!["a", "aa", "b", "c", "d"],
            "net first; the 5.0 trio orders by drawdown (1.0, 1.0, 3.0) then by label"
        );
        // total and stable: ranking an already-ranked list is a no-op
        let once = rows.clone();
        rank(&mut rows);
        assert_eq!(rows, once);
        // and it does not depend on input order
        let mut shuffled = vec![row("d", -2.0, 0.0), row("b", 5.0, 1.0), row("a", 12.0, 40.0), row("aa", 5.0, 1.0), row("c", 5.0, 3.0)];
        rank(&mut shuffled);
        assert_eq!(shuffled.iter().map(|r| r.label.clone()).collect::<Vec<_>>(), order);
    }

    #[test]
    fn the_summary_carries_the_mechanical_replay_caveat_and_every_row() {
        let cfg = fixture_cfg();
        let ax = axes(&["conviction=0.70,0.80"]);
        let sweep = SweepReport {
            label: "t".into(),
            from: "2026-08-07 00:00".into(),
            to: "2026-08-08 23:59".into(),
            from_ms: 0,
            to_ms: 0,
            markets: 3,
            bars: 400,
            axes: ax.iter().map(|a| (a.key.display().to_string(), a.values.clone())).collect(),
            base_overrides: vec![],
            caveats: sweep_caveats(&cfg, &ax, 0.75),
            rows: vec![row("conviction=0.80", 5.0, 1.0), row("conviction=0.70", -1.0, 2.0)],
        };
        let md = summary_markdown(&sweep);
        assert!(md.contains("mechanical replay"), "spec Decision 2 header missing:\n{md}");
        assert!(md.contains("sweeps it over 0.70, 0.80"), "{md}");
        assert!(md.contains("conviction axis:"), "the interpretation note is in the header:\n{md}");
        assert!(md.contains("| 1 | `conviction=0.80` | +5.00 |"), "{md}");
        assert!(md.contains("| 2 | `conviction=0.70` | -1.00 |"), "the loser ranks second:\n{md}");
        assert!(md.contains("kestreld backtest run --from 2026-08-07"), "reproduce block:\n{md}");
        // the fee anatomy: gross, the turnover the fee bill implies, and the loss share
        assert!(md.contains("## Fee anatomy"), "{md}");
        assert!(
            md.contains("| `conviction=0.70` | +0.00 | 1.00 | 1333 | 100.0% |"),
            "the losing row apportions its loss to fees:\n{md}"
        );
        assert!(
            md.contains("| `conviction=0.80` | +6.00 | 1.00 | 1333 |  |"),
            "the winning row leaves the loss share blank:\n{md}"
        );
        assert!(summary_markdown(&sweep) == md, "markdown is a pure function of the report");

        // a sweep that does not touch conviction states the one number it used instead
        let ax2 = axes(&["stop_floor=0.6,1.0"]);
        let plain = sweep_caveats(&cfg, &ax2, 0.75);
        assert!(plain[0].contains("fixed at 0.75"), "{:?}", plain[0]);
        assert!(!plain.iter().any(|c| c.contains("conviction axis:")));

        let text = human_table(&sweep);
        assert!(text.contains("conviction=0.80"), "{text}");
        assert!(text.contains("+5.00"), "{text}");
    }

    #[test]
    fn default_labels_name_the_axes() {
        assert_eq!(default_label(&axes(&["stop_floor=0.6,1.0"])), "sweep-stop_floor_pct");
        assert_eq!(default_label(&axes(&["conviction=0.7"])), "sweep-conviction");
        assert_eq!(
            default_label(&axes(&["regime=off", "cooldown_after_sl=30"])),
            "sweep-regime_vol_max-cooldown_after_sl_min"
        );
    }
}
