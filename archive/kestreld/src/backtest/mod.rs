//! Backtest harness — replay historical market data through the real strategy machinery.
//!
//! Spec: `docs/superpowers/specs/2026-08-09-backtest-harness.md`.
//!
//!   * **B1, the data layer** — the cache ([`cache`]), the resumable backfill ([`fetch`]) and
//!     feature recomputation ([`features`]): `kestreld backtest fetch --from … --to …`.
//!   * **B2, the engine** — the shared fill rules ([`fills`]), the 1m replay loop ([`engine`])
//!     and the report ([`report`]): `kestreld backtest run --from … --to …`.
//!   * **B3, the grid** — the cartesian sweep runner and its ranked summary ([`sweep`]):
//!     `kestreld backtest sweep --from … --to … --sweep stop_floor=0.6,1.0,1.4`.
//!   * **B4, capacity/fee axes + distributions** — five more sweep axes (`daily_cap`,
//!     `max_concurrent`, `min_score`, `margin`, `tp_mult`, see [`sweep`]'s doc table) and a
//!     read-only distribution report over the cache ([`stats`]):
//!     `kestreld backtest stats --from … --to …`.
//!
//! PAPER-ONLY, and one step further: the harness has no order surface at all. It reads public
//! candle/funding endpoints, writes its own sqlite cache and simulates. It never opens
//! `kestreld.db` — the daemon owns that file under systemd and nothing here may disturb it.

pub mod cache;
pub mod engine;
pub mod features;
pub mod fetch;
pub mod fills;
pub mod report;
pub mod stats;
pub mod sweep;

use anyhow::{Context, bail};
use tracing::info;

use crate::config::Config;
use crate::hl_rest::HlRest;

use cache::Cache;

pub const MINUTE_MS: i64 = 60_000;
pub const DAY_MS: i64 = 24 * 60 * MINUTE_MS;

/// Where reports land, relative to the daemon's working directory — the same `../docs`
/// convention `digest::LEDGER_DIR_DEFAULT` uses, so both artifacts sit in the repo's docs tree.
pub const REPORT_DIR_DEFAULT: &str = "../docs/backtests";

/// Default `--conviction` (spec Decision 2): the analyst is replaced by one fixed number, and
/// 0.75 is the live `conviction_min` — a replay at the floor the daemon actually trades at.
pub const DEFAULT_CONVICTION: f64 = 0.75;

/// `kestreld backtest <cmd>`.
#[derive(Debug, clap::Subcommand)]
pub enum BacktestCmd {
    /// Backfill the candle/funding cache for a UTC date range (cache-first, resumable).
    Fetch(FetchArgs),
    /// Replay the cached window through the strategy and write a report.
    Run(RunArgs),
    /// Replay the cached window once per point of a parameter grid and rank the results.
    Sweep(sweep::SweepArgs),
    /// Read-only distribution report over the cache — no strategy replay (see [`stats`]).
    Stats(stats::StatsArgs),
}

#[derive(Debug, clap::Args)]
pub struct FetchArgs {
    /// First UTC day to cache, `YYYY-MM-DD` (inclusive, from 00:00Z).
    #[arg(long)]
    pub from: String,
    /// Last UTC day to cache, `YYYY-MM-DD` (inclusive, through 23:59Z).
    #[arg(long)]
    pub to: String,
    /// Markets to fetch, comma separated (e.g. `BTC,ETH,xyz:TSLA`). Defaults to the venue's
    /// current market list.
    #[arg(long, value_delimiter = ',')]
    pub markets: Option<Vec<String>>,
}

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// First UTC day to replay, `YYYY-MM-DD` (inclusive, from 00:00Z).
    #[arg(long)]
    pub from: String,
    /// Last UTC day to replay, `YYYY-MM-DD` (inclusive, through 23:59Z).
    #[arg(long)]
    pub to: String,
    /// Markets to replay, comma separated. Defaults to everything the cache holds.
    #[arg(long, value_delimiter = ',')]
    pub markets: Option<Vec<String>>,
    /// The fixed conviction every entry is taken at — the analyst's stand-in (spec Decision 2).
    #[arg(long, default_value_t = DEFAULT_CONVICTION)]
    pub conviction: f64,
    /// Override a config knob for this run, e.g. `--set risk.cooldown_after_sl_min=30`.
    /// Repeatable. Only knobs the replay reads may be set; anything else is an error.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub set: Vec<String>,
    /// Directory the report is written under.
    #[arg(long, default_value = REPORT_DIR_DEFAULT)]
    pub out: String,
    /// Name of the report directory, after the date. Defaults to the window and conviction.
    #[arg(long)]
    pub label: Option<String>,
}

pub async fn run(cmd: BacktestCmd, config_path: &str) -> anyhow::Result<()> {
    match cmd {
        BacktestCmd::Fetch(args) => fetch_cmd(&args, config_path).await,
        BacktestCmd::Run(args) => run_cmd(&args, config_path).await,
        BacktestCmd::Sweep(args) => sweep_cmd(&args, config_path).await,
        BacktestCmd::Stats(args) => stats_cmd(&args, config_path).await,
    }
}

/// Midnight UTC of a `YYYY-MM-DD` day, in ms.
pub fn day_start_ms(day: &str) -> anyhow::Result<i64> {
    let d = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .with_context(|| format!("bad date {day:?}, expected YYYY-MM-DD"))?;
    Ok(d.and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp_millis())
}

/// The inclusive minute window `--from`/`--to` name: 00:00Z of `from` through 23:59Z of `to`.
///
/// The end is clamped to the last CLOSED minute at `now`. The minute in progress has a
/// half-formed candle, and caching one would be worse than not caching it: coverage is the
/// cached span, so a partial candle would mark that minute done and never be refetched.
pub fn resolve_window(from: &str, to: &str, now_ms: i64) -> anyhow::Result<(i64, i64)> {
    let from_ms = day_start_ms(from)?;
    let day_end = day_start_ms(to)? + DAY_MS - MINUTE_MS;
    let last_closed_minute = now_ms - now_ms.rem_euclid(MINUTE_MS) - MINUTE_MS;
    let to_ms = day_end.min(last_closed_minute);
    if to_ms < from_ms {
        bail!("empty window: {from}..{to} contains no completed minute (from must not be in the future)");
    }
    Ok((from_ms, to_ms))
}

/// Normalise a `--markets` list: trim, drop blanks, sort, dedup. Sorting makes a run
/// deterministic (spec Decision 6: ties broken by market name).
pub fn normalize_markets(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = raw.iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// The venue's current market list, per dex, via the same `/meta` path that seeds the ws
/// name maps (`HlRest::universe_names`, what `main::seed_name_maps` calls).
///
/// Deliberately NOT vlm-filtered: the daemon's filter reads a live `dayNtlVlm` that history
/// does not serve, so the harness caches everything the venue lists and applies the volume
/// filter at replay time from the proxy (`features::MarketSeries::day_ntl_vlm_at`).
async fn universe_markets(hl: &HlRest, config_path: &str) -> anyhow::Result<Vec<String>> {
    let cfg = crate::config::Config::load(config_path).context("load config")?;
    let mut all = Vec::new();
    for dex in &cfg.universe.dexs {
        let opt = if dex.is_empty() { None } else { Some(dex.as_str()) };
        let names = hl
            .universe_names(opt)
            .await
            .map_err(|e| anyhow::anyhow!("universe names for dex {dex:?}: {e}"))?;
        info!(dex = %dex, markets = names.len(), "backtest universe from /meta");
        all.extend(names);
    }
    Ok(normalize_markets(&all))
}

async fn fetch_cmd(args: &FetchArgs, config_path: &str) -> anyhow::Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (from_ms, to_ms) = resolve_window(&args.from, &args.to, now_ms)?;
    let minutes = (to_ms - from_ms) / MINUTE_MS + 1;

    let hl = HlRest::new(HlRest::MAINNET);
    let markets = match &args.markets {
        Some(list) if !list.is_empty() => normalize_markets(list),
        _ => universe_markets(&hl, config_path).await?,
    };
    if markets.is_empty() {
        bail!("no markets to fetch");
    }

    let url = Cache::default_url();
    let cache = Cache::open(&url).await.with_context(|| format!("open cache {url}"))?;

    println!(
        "backtest fetch  {}..{}  ({} minutes)  {} market{}",
        args.from,
        args.to,
        minutes,
        markets.len(),
        if markets.len() == 1 { "" } else { "s" }
    );
    println!("cache  {url}");

    let summary = fetch::backfill(&hl, &cache, &markets, from_ms, to_ms).await?;
    cache.close().await;

    println!(
        "done   {} requests · {} candles fetched · {} funding rows · {} already cached",
        summary.requests,
        summary.candles_fetched(),
        summary.funding_fetched(),
        summary.candles_cached()
    );
    for m in summary.markets.iter().filter(|m| m.error.is_some()) {
        println!("  FAILED {} — {}", m.market, m.error.as_deref().unwrap_or(""));
    }
    if summary.failed > 0 {
        println!("{} market(s) stopped early; rerun to resume from the cached span", summary.failed);
    }
    Ok(())
}

/// Parse one `key=value` override into a dotted path and a TOML scalar.
///
/// The value is parsed AS TOML (`3`, `1.5`, `true`, `"text"`), so a knob keeps its type and a
/// typo like `daily_cap=five` fails here rather than silently deserializing into something.
pub fn parse_override(spec: &str) -> anyhow::Result<(String, toml::Value)> {
    let (key, raw) = spec
        .split_once('=')
        .with_context(|| format!("--set expects key=value, got {spec:?}"))?;
    let key = key.trim();
    let raw = raw.trim();
    if key.is_empty() || raw.is_empty() {
        bail!("--set expects key=value, got {spec:?}");
    }
    let wrapped: toml::Table = toml::from_str(&format!("v = {raw}"))
        .with_context(|| format!("--set {key}: {raw:?} is not a TOML value (try 30, 1.5, true)"))?;
    let value = wrapped.get("v").cloned().expect("the wrapper table has exactly one key");
    Ok((key.to_string(), value))
}

/// Apply `--set` overrides to a parsed config document.
///
/// `allowed` is the closed set of knobs the replay actually reads
/// ([`report::config_knobs`]). Overriding anything else would be a lie — the run would report
/// a knob it never consulted — so an unknown key is an error that lists the valid ones.
pub fn apply_overrides(doc: &mut toml::Table, sets: &[String], allowed: &[String]) -> anyhow::Result<()> {
    for spec in sets {
        let (key, value) = parse_override(spec)?;
        if !allowed.contains(&key) {
            bail!("--set {key}: not a knob the replay reads.\nvalid keys:\n  {}", allowed.join("\n  "));
        }
        let (section, field) = key.split_once('.').expect("allowed keys are all dotted");
        let table = doc
            .get_mut(section)
            .and_then(|v| v.as_table_mut())
            .with_context(|| format!("--set {key}: config has no [{section}] section"))?;
        table.insert(field.to_string(), value);
    }
    Ok(())
}

/// Load the config, then apply the run's overrides on top of the parsed document.
pub fn load_config(config_path: &str, sets: &[String]) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("read config {config_path}"))?;
    let mut doc: toml::Table =
        toml::from_str(&text).with_context(|| format!("parse config {config_path}"))?;
    if !sets.is_empty() {
        let base: Config = toml::from_str(&text).with_context(|| format!("parse config {config_path}"))?;
        let allowed: Vec<String> = report::config_knobs(&base).into_keys().collect();
        apply_overrides(&mut doc, sets, &allowed)?;
    }
    let merged = toml::to_string(&doc).context("re-serialize config with overrides")?;
    toml::from_str(&merged).context("config with overrides is not a valid config")
}

/// `docs/backtests/<today>-<label>` — the date is the day the run was MADE (the report's own
/// window lives inside it), matching the spec's `YYYY-MM-DD-<label>` layout.
pub fn report_dir(out: &str, label: &str, today: &str) -> std::path::PathBuf {
    std::path::Path::new(out).join(format!("{today}-{label}"))
}

/// Default label: the window and the conviction it was replayed at.
pub fn default_label(args: &RunArgs) -> String {
    format!("run-{}_{}-c{}", args.from, args.to, args.conviction)
}

/// Open the cache, resolve the market list and load the replay window's tape.
///
/// ONE loader for `run` and `sweep`: a grid replays the very same tape N times, and any
/// difference between how a single run and a grid row read the cache would be a difference the
/// ranking could not see.
async fn load_tape(
    markets_arg: &Option<Vec<String>>,
    from_ms: i64,
    to_ms: i64,
) -> anyhow::Result<std::collections::BTreeMap<String, engine::MarketData>> {
    let url = Cache::default_url();
    let cache = Cache::open(&url).await.with_context(|| format!("open cache {url}"))?;
    let markets = match markets_arg {
        Some(list) if !list.is_empty() => normalize_markets(list),
        _ => cache.markets().await.context("read cached markets")?,
    };
    if markets.is_empty() {
        cache.close().await;
        bail!("cache {url} holds no candles — run `kestreld backtest fetch --from … --to …` first");
    }
    println!("cache  {url}  ({} market{})", markets.len(), if markets.len() == 1 { "" } else { "s" });
    let data = engine::load(&cache, &markets, from_ms, to_ms).await.context("load cached tape");
    cache.close().await;
    let data = data?;
    if data.is_empty() {
        bail!(
            "no cached candles in {}..{} for any of {} market(s) — the venue serves only ~3.6 days \
             of 1m history, so an older window can never be backfilled",
            stamp_day(from_ms),
            stamp_day(to_ms),
            markets.len()
        );
    }
    Ok(data)
}

/// `YYYY-MM-DD` of an instant, for error messages that quote a window back.
fn stamp_day(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| ms.to_string())
}

/// `docs/backtests/<today>-<label>`, created.
fn make_report_dir(out: &str, label: &str, now_ms: i64) -> anyhow::Result<std::path::PathBuf> {
    let today = chrono::DateTime::from_timestamp_millis(now_ms)
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y-%m-%d")
        .to_string();
    let dir = report_dir(out, label, &today);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Write one run's `report.json` + `report.md` into `dir`.
fn write_report(dir: &std::path::Path, rep: &report::RunReport, label: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let json = serde_json::to_string_pretty(rep).context("serialize report")?;
    std::fs::write(dir.join("report.json"), &json)?;
    std::fs::write(dir.join("report.md"), report::markdown(rep, label))?;
    Ok(())
}

async fn run_cmd(args: &RunArgs, config_path: &str) -> anyhow::Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (from_ms, to_ms) = resolve_window(&args.from, &args.to, now_ms)?;
    let cfg = load_config(config_path, &args.set)?;

    println!("backtest run  {} .. {}", args.from, args.to);
    let data = load_tape(&args.markets, from_ms, to_ms).await?;

    let params = engine::RunParams {
        from_ms,
        to_ms,
        conviction: args.conviction,
        markets: data.keys().cloned().collect(),
        overrides: args.set.clone(),
    };
    info!(
        markets = data.len(),
        minutes = (to_ms - from_ms) / MINUTE_MS + 1,
        conviction = args.conviction,
        "backtest replay starting"
    );
    let outcome = engine::run(&cfg, &data, params);
    let rep = report::build(&cfg, &outcome);

    println!("\n{}", report::human_summary(&rep));

    let label = args.label.clone().unwrap_or_else(|| default_label(args));
    let dir = make_report_dir(&args.out, &label, now_ms)?;
    write_report(&dir, &rep, &label)?;
    println!("report  {}/report.{{json,md}}", dir.display());
    Ok(())
}

/// `kestreld backtest sweep` — expand the grid, replay the ONE loaded tape per combo, write a
/// report per combo plus the ranked `summary.{md,json}` on top (spec Decision 7).
async fn sweep_cmd(args: &sweep::SweepArgs, config_path: &str) -> anyhow::Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (from_ms, to_ms) = resolve_window(&args.from, &args.to, now_ms)?;

    let axes = sweep::parse_axes(&args.sweep)?;
    let combos = sweep::expand(&axes);
    if combos.len() > sweep::MAX_COMBOS {
        bail!(
            "{} combos is past the {} the grid runner will take — each one is a full replay of \
             the window; narrow the axes",
            combos.len(),
            sweep::MAX_COMBOS
        );
    }
    // Fail on a bad axis value BEFORE loading a tape: an unknown knob or an unparseable
    // conviction is a typo, and finding it after a 30-second load helps nobody.
    for combo in &combos {
        let resolved = sweep::resolve(args.conviction, &args.set, combo)?;
        load_config(config_path, &resolved.sets)
            .with_context(|| format!("combo {}", combo.label()))?;
    }

    println!("backtest sweep  {} .. {}  ({} runs)", args.from, args.to, combos.len());
    for axis in &axes {
        println!("  {} = {}", axis.key.display(), axis.values.join(", "));
    }
    let data = load_tape(&args.markets, from_ms, to_ms).await?;

    let (report, per_combo) =
        sweep::run_grid(config_path, args, &axes, &combos, &data, from_ms, to_ms)?;

    println!("\n{}", sweep::human_table(&report));

    let dir = make_report_dir(&args.out, &report.label, now_ms)?;
    for (combo, rep) in &per_combo {
        write_report(&dir.join(combo.slug()), rep, &combo.label())?;
    }
    std::fs::write(dir.join("summary.md"), sweep::summary_markdown(&report))?;
    std::fs::write(
        dir.join("summary.json"),
        serde_json::to_string_pretty(&report).context("serialize sweep")?,
    )?;
    println!("sweep   {}/summary.{{json,md}}  (+ {} run reports)", dir.display(), per_combo.len());
    Ok(())
}

/// `kestreld backtest stats` — a read-only distribution report, no strategy replay (batch B4,
/// [`stats`]). Shares [`load_tape`] with `run`/`sweep` so the tape it walks is built the exact
/// same way; reads the config as-is (no `--set` — a distribution report has nothing to sweep).
async fn stats_cmd(args: &stats::StatsArgs, config_path: &str) -> anyhow::Result<()> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let (from_ms, to_ms) = resolve_window(&args.from, &args.to, now_ms)?;
    let cfg = crate::config::Config::load(config_path).context("load config")?;

    println!("backtest stats  {} .. {}", args.from, args.to);
    let data = load_tape(&args.markets, from_ms, to_ms).await?;

    let rep = stats::build(&cfg, &data, from_ms, to_ms);
    println!("\n{}", stats::human_summary(&rep));

    let label = args.label.clone().unwrap_or_else(|| stats::default_label(&args.from, &args.to));
    let dir = make_report_dir(&args.out, &label, now_ms)?;
    std::fs::write(dir.join("stats.json"), serde_json::to_string_pretty(&rep).context("serialize stats")?)?;
    std::fs::write(dir.join("stats.md"), stats::markdown(&rep, &label))?;
    println!("stats   {}/stats.{{json,md}}", dir.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: i64 = MINUTE_MS;

    #[test]
    fn resolve_window_is_an_inclusive_day_range() {
        // 2026-08-08 00:00Z .. 2026-08-08 23:59Z, clamped by a `now` well past it.
        let now = day_start_ms("2026-08-10").unwrap();
        let (from, to) = resolve_window("2026-08-08", "2026-08-08", now).unwrap();
        assert_eq!(from, day_start_ms("2026-08-08").unwrap());
        assert_eq!(to, from + DAY_MS - M, "the last minute of the day is included");
        assert_eq!((to - from) / M + 1, 1440, "one full day of 1m candles");

        let (from, to) = resolve_window("2026-08-02", "2026-08-08", now).unwrap();
        assert_eq!((to - from) / M + 1, 7 * 1440, "seven inclusive days");
    }

    #[test]
    fn resolve_window_never_requests_the_minute_in_progress() {
        // 12:34:56Z today — the 12:34 candle is still forming and must not be cached.
        let now = day_start_ms("2026-08-09").unwrap() + 12 * 60 * M + 34 * M + 56_000;
        let (_, to) = resolve_window("2026-08-09", "2026-08-09", now).unwrap();
        assert_eq!(to, day_start_ms("2026-08-09").unwrap() + 12 * 60 * M + 33 * M, "last CLOSED minute");
    }

    #[test]
    fn resolve_window_rejects_bad_and_empty_ranges() {
        let now = day_start_ms("2026-08-09").unwrap();
        assert!(resolve_window("08-09-2026", "2026-08-09", now).is_err(), "wrong date format");
        assert!(resolve_window("2026-08-09", "not-a-date", now).is_err());
        // A window entirely in the future has no completed minute.
        assert!(resolve_window("2026-08-20", "2026-08-21", now).is_err());
        // to < from.
        assert!(resolve_window("2026-08-09", "2026-08-08", now).is_err());
    }

    #[test]
    fn normalize_markets_trims_sorts_and_dedups() {
        let raw = vec![" BTC ".to_string(), "xyz:TSLA".to_string(), "BTC".to_string(), "".to_string()];
        assert_eq!(normalize_markets(&raw), vec!["BTC".to_string(), "xyz:TSLA".to_string()]);
    }

    #[test]
    fn overrides_parse_as_typed_toml_values() {
        assert_eq!(parse_override("risk.daily_cap=3").unwrap().1, toml::Value::Integer(3));
        assert_eq!(parse_override("risk.regime_vol_max=1.5").unwrap().1, toml::Value::Float(1.5));
        assert_eq!(parse_override(" risk.daily_cap = 3 ").unwrap().0, "risk.daily_cap", "whitespace trimmed");
        // A value that is not TOML is a typo, not a string.
        assert!(parse_override("risk.daily_cap=five").is_err());
        assert!(parse_override("risk.daily_cap").is_err(), "missing =");
        assert!(parse_override("=3").is_err());
        assert!(parse_override("risk.daily_cap=").is_err());
    }

    /// `--set` edits the real `kestreld.toml`'s knobs and nothing else — including knobs the
    /// file omits and takes from a serde default.
    #[test]
    fn set_overrides_apply_to_the_live_config_and_reject_unknown_keys() {
        let base = Config::load("kestreld.toml").expect("kestreld.toml");
        let cfg = load_config(
            "kestreld.toml",
            &["risk.cooldown_after_sl_min=30".to_string(), "screener.min_score=2.5".to_string()],
        )
        .expect("apply overrides");
        assert_eq!(cfg.risk.cooldown_after_sl_min, 30);
        assert!((cfg.screener.min_score - 2.5).abs() < f64::EPSILON);
        // everything else is untouched
        assert_eq!(cfg.risk.daily_cap, base.risk.daily_cap);
        assert!((cfg.risk.conviction_min - base.risk.conviction_min).abs() < f64::EPSILON);
        assert!((cfg.sizing.bankroll - base.sizing.bankroll).abs() < f64::EPSILON);
        // no overrides at all is the file verbatim
        let plain = load_config("kestreld.toml", &[]).expect("no overrides");
        assert_eq!(report::config_knobs(&plain), report::config_knobs(&base));

        // A knob the replay never reads, and a typo, both refuse loudly.
        let err = load_config("kestreld.toml", &["analyst.model=\"x\"".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("not a knob the replay reads"), "{err}");
        let err = load_config("kestreld.toml", &["risk.daily_capp=3".to_string()]).unwrap_err();
        assert!(format!("{err}").contains("valid keys"), "{err}");
    }

    #[test]
    fn report_dirs_are_dated_and_labelled() {
        let args = RunArgs {
            from: "2026-08-02".into(),
            to: "2026-08-09".into(),
            markets: None,
            conviction: 0.75,
            set: vec![],
            out: REPORT_DIR_DEFAULT.into(),
            label: None,
        };
        assert_eq!(default_label(&args), "run-2026-08-02_2026-08-09-c0.75");
        assert_eq!(
            report_dir(&args.out, &default_label(&args), "2026-08-10"),
            std::path::Path::new("../docs/backtests/2026-08-10-run-2026-08-02_2026-08-09-c0.75")
        );
        assert_eq!(
            report_dir("out", "stop-floor-sweep", "2026-08-10"),
            std::path::Path::new("out/2026-08-10-stop-floor-sweep")
        );
    }

    /// Spec Decision 8 — feature-parity smoke against the RUNNING daemon.
    ///
    /// Ignored by default: it needs (a) kestreld serving on the port in `kestreld.toml` and
    /// (b) a cache backfilled through the CURRENT minute
    /// (`kestreld backtest fetch --from <today> --to <today>`, run immediately before). It
    /// reads only localhost and the local cache — no venue calls — and writes nothing.
    ///
    /// WHAT IT CAN AND CANNOT PROVE. The formula is pinned exactly, offline, by
    /// `features::tests::recomputed_features_match_the_live_engine_on_the_same_tape`. What is
    /// left here is everything the two systems genuinely do not share, and each one is either
    /// gated or reported rather than asserted through:
    ///
    ///   * **Lookbacks longer than the daemon's uptime.** A daemon up 30 minutes has no 1h or
    ///     24h price: `find_mid` falls back, so live `r1h`/`r24h` silently become `r5m` and
    ///     `range_pos` is measured over minutes instead of a day. Measured 2026-08-09 at
    ///     uptime 0.56 h: live r5m = r1h = r24h = -0.0107, replay r24h = +0.2338 — the replay
    ///     is the correct number. Each metric is therefore compared only once the daemon has
    ///     been up longer than the window it needs.
    ///   * **Mid tape vs trade tape.** Live builds bars from `allMids`; the harness gets
    ///     Hyperliquid's traded 1m candles. Trade prints cross the spread, so the trade tape
    ///     is the noisier of the two and `vol1h` runs higher (0.0061 vs 0.0049 in the same
    ///     measurement). Reported, not asserted — quantifying that gap is this smoke's job.
    ///   * **`funding_z` — FIXED, evidence retained.** Until this fix, the live ring was fed
    ///     the ctx stream's `funding`, which drifts intra-hour (observed changing every ~9 s),
    ///     and the duplicate guard only collapsed exact repeats — so the 200-slot ring refilled
    ///     with intra-hour samples within ~30 minutes of boot and the 7-day fundingHistory seed
    ///     was fully evicted. Live z measured drift against the last half hour (-0.059) while
    ///     the replay measured the settled hourly rate against 7 days (-1.10). `FeatureEngine`
    ///     now buckets ring samples by settlement HOUR (`ts_ms / FUNDING_HOUR_MS`): a same-hour
    ///     push updates the newest slot in place instead of appending, so intra-hour drift can
    ///     no longer evict the seed. The RING LOGIC is pinned by
    ///     `features::tests::funding_z_series_is_the_live_ring_including_its_duplicate_guard`
    ///     and the hour-bucket fix itself by
    ///     `features::tests::drift_within_the_seeded_hour_does_not_evict_the_7day_seed`,
    ///     `features::tests::seeded_vs_settled_matches_the_broken_live_scenario_now_fixed` and
    ///     `funding_z_series_is_the_live_ring_including_its_duplicate_guard`'s counterpart here,
    ///     `features::tests::live_engine_matches_the_backtest_hourly_series_despite_interleaved_drift`.
    ///     This smoke still only PRINTS funding_z rather than asserting it (see below) — it
    ///     needs the REDEPLOYED daemon to prove the live process itself picked up the fix, not
    ///     just that the source does the right thing offline.
    ///
    /// DEVIATION from Decision 8's "r's exact": live measures between two 1-second mids, the
    /// replay between two minute closes. Equality is not a property they can have; the
    /// tolerance below is that sampling difference.
    ///
    /// Run with: `cargo test -- --ignored --nocapture live_parity`
    #[tokio::test]
    #[ignore = "needs the running daemon on localhost plus a cache backfilled to this minute"]
    async fn live_parity_smoke_against_local_snapshot() {
        const BASE: &str = "http://127.0.0.1:7411";
        const R_TOL_ABS: f64 = 0.05; // percentage points
        const R_TOL_REL: f64 = 0.10;
        /// Slack on top of a window before the live engine is trusted to have filled it.
        const WARMUP_MARGIN_S: u64 = 300;

        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("http client");
        let health: crate::contracts::Health = http
            .get(format!("{BASE}/api/health"))
            .send()
            .await
            .expect("GET /api/health — is kestreld running?")
            .json()
            .await
            .expect("health json");
        let snap: crate::contracts::Snapshot = http
            .get(format!("{BASE}/api/snapshot"))
            .send()
            .await
            .expect("GET /api/snapshot")
            .json()
            .await
            .expect("snapshot json");
        let up = health.uptime_s;
        println!("daemon uptime {:.2} h — comparing windows it has actually filled", up as f64 / 3600.0);

        let cache = Cache::open(&Cache::default_url()).await.expect("open backtest cache");
        let close = |a: f64, b: f64, abs: f64, rel: f64| (a - b).abs() <= abs.max(b.abs() * rel);

        let mut compared = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for row in &snap.markets {
            let Some(live) = row.features.as_ref() else { continue };
            let rows = cache
                .candles(&row.market, snap.ts - DAY_MS - 60 * MINUTE_MS, snap.ts)
                .await
                .expect("read cached candles");
            if rows.len() < 61 {
                continue;
            }
            let series = features::MarketSeries::new(&row.market, rows);
            // Only compare when the tape actually reaches the snapshot's minute.
            if series.last_ts().is_none_or(|t| snap.ts - t > 2 * MINUTE_MS) {
                continue;
            }
            let idx = series.index_at(snap.ts).expect("index at snapshot ts");
            let funding = cache
                .funding(&row.market, snap.ts - 7 * DAY_MS, snap.ts)
                .await
                .expect("read cached funding");
            let z = features::funding_z_at(&features::funding_z_series(&row.market, &funding), snap.ts);
            let Some(replay) = series.features_at(idx, z) else { continue };

            compared += 1;
            println!(
                "{:<10} r5m {:>8.4}/{:<8.4} r1h {:>8.4}/{:<8.4} r24h {:>8.4}/{:<8.4} \
                 vol1h {:>7.4}/{:<7.4} range_pos {:>5.3}/{:<5.3} funding_z {:>6.3}/{:<6.3}  (replay/live)",
                row.market,
                replay.r5m,
                live.r5m,
                replay.r1h,
                live.r1h,
                replay.r24h,
                live.r24h,
                replay.vol1h,
                live.vol1h,
                replay.range_pos,
                live.range_pos,
                replay.funding_z,
                live.funding_z
            );

            let mut bad = Vec::new();
            // (metric, replay, live, seconds of history the live engine needs first)
            let gated = [
                ("r5m", replay.r5m, live.r5m, 5 * 60),
                ("r1h", replay.r1h, live.r1h, 60 * 60),
                ("r24h", replay.r24h, live.r24h, 24 * 60 * 60),
            ];
            for (name, r, l, needs_s) in gated {
                if up < needs_s + WARMUP_MARGIN_S {
                    continue;
                }
                if !close(r, l, R_TOL_ABS, R_TOL_REL) {
                    bad.push(format!("{name} replay {r:.4} vs live {l:.4}"));
                }
            }
            if up >= 24 * 60 * 60 + WARMUP_MARGIN_S && (replay.range_pos - live.range_pos).abs() > 0.10 {
                bad.push(format!("range_pos replay {:.3} vs live {:.3}", replay.range_pos, live.range_pos));
            }
            if !bad.is_empty() {
                failures.push(format!("{}: {}", row.market, bad.join(", ")));
            }
        }

        assert!(
            compared > 0,
            "no market had both live features and a cache reaching the snapshot minute — \
             run `kestreld backtest fetch --from <today> --to <today>` immediately before this test"
        );
        assert!(
            up >= 5 * 60 + WARMUP_MARGIN_S,
            "daemon has been up {up}s — nothing is comparable yet, not even r5m"
        );
        assert!(failures.is_empty(), "{compared} markets compared, parity failures:\n{}", failures.join("\n"));
        println!("{compared} market(s) compared, no parity failures on windows the daemon has filled");
    }
}
