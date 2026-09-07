#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerCfg,
    pub universe: UniverseCfg,
    pub screener: ScreenerCfg,
    pub sizing: SizingCfg,
    pub risk: RiskCfg,
    pub analyst: AnalystCfg,
    pub news: NewsCfg,
    pub notify: NotifyCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerCfg {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UniverseCfg {
    pub dexs: Vec<String>,
    pub min_vlm_native: f64,
    pub min_vlm_dex: f64,
    /// Native-market allowlist (exact symbol match, e.g. "BTC"). Empty = no allowlist
    /// (pre-2026-08-24 behavior). `xyz:` markets are NOT subject to it — the equities dex
    /// passes through on its volume floor alone (Batch B: majors-only pilot).
    #[serde(default)]
    pub allowlist: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScreenerCfg {
    pub interval_s: u64,
    pub top_k: usize,
    pub min_score: f64,
    #[serde(default = "default_skip_recheck_min")]
    pub skip_recheck_min: i64,
    #[serde(default = "default_skip_recheck_score_jump")]
    pub skip_recheck_score_jump: f64,
}

fn default_skip_recheck_min() -> i64 {
    15
}

fn default_skip_recheck_score_jump() -> f64 {
    0.5
}

fn default_stop_floor_pct() -> f64 {
    1.0
}

fn default_tp_mult() -> f64 {
    2.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct SizingCfg {
    pub bankroll: f64,
    pub vol_ref: f64,
    pub margin_min: f64,
    pub margin_max: f64,
    /// Lower bound of the `stop_pct` clamp in `sizing::size_position` — the narrowest stop the
    /// sizer may hand out, whatever the volatility says.
    ///
    /// It was a literal until the backtest harness needed to sweep it (spec Decision 1's first
    /// target question: 0.6 / 1.0 / 1.4). The default IS the user-locked amendment of
    /// 2026-08-09 (0.6 -> 1.0, stops inside the noise band), so a config that omits the key —
    /// `kestreld.toml` does — sizes exactly as it did before the knob existed.
    #[serde(default = "default_stop_floor_pct")]
    pub stop_floor_pct: f64,
    /// The take-profit multiple of the stop distance in `sizing::size_position`
    /// (`tp_pct = tp_mult * stop_pct`) — a fixed 2R (`2.0`) until the backtest harness needed to
    /// sweep it (wave-3 batch B4, sweep key `tp_mult`). The default IS that literal, so a config
    /// that omits the key — `kestreld.toml` does — sizes exactly as it did before the knob
    /// existed. Same treatment as `stop_floor_pct` above.
    #[serde(default = "default_tp_mult")]
    pub tp_mult: f64,
}

fn default_max_feature_age_s() -> u64 {
    120
}

fn default_per_market_daily_cap() -> usize {
    3
}

fn default_cooldown_after_sl_min() -> u64 {
    120
}

fn default_morning_entry_budget() -> usize {
    12
}

fn default_regime_vol_max() -> f64 {
    1.5
}

fn default_min_rr() -> f64 {
    2.0
}

fn default_kill_enabled() -> bool {
    false
}

fn default_entries_enabled() -> bool {
    true
}

fn default_global_max_concurrent() -> usize { 12 }

fn default_daily_cap() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskCfg {
    /// Per-analyst open-position cap. `0` disables the gate.
    pub max_concurrent: usize,
    /// Shared open-position cap across every analyst. `0` disables the gate.
    #[serde(default = "default_global_max_concurrent")]
    pub global_max_concurrent: usize,
    /// Global and per-analyst daily entry cap. `0` disables both daily-cap gates.
    #[serde(default = "default_daily_cap")]
    pub daily_cap: usize,
    pub cooldown_min: u64,
    pub kill_switch_pct: f64,
    /// Paper collection runs uninterrupted unless the daily drawdown latch is explicitly
    /// re-enabled. The threshold remains configured for that opt-in path.
    #[serde(default = "default_kill_enabled")]
    pub kill_enabled: bool,
    /// Manual entry halt. Existing positions continue through triggers, reviews, and closes.
    #[serde(default = "default_entries_enabled")]
    pub entries_enabled: bool,
    pub conviction_min: f64,
    pub review_interval_min: u64,
    pub time_stop_hours: f64,
    /// Entry staleness gate: an entry decision computed on snapshot state older than this
    /// is refused (`GateRefusal::StaleData`). Reviews/exits are deliberately NOT gated —
    /// closing on stale data beats being stuck in a position with no price feed.
    #[serde(default = "default_max_feature_age_s")]
    pub max_feature_age_s: u64,
    /// Churn control (T1): entries per market per UTC day (`GateRefusal::PerMarketCap`).
    #[serde(default = "default_per_market_daily_cap")]
    pub per_market_daily_cap: usize,
    /// Churn control (T1): cooldown after a market's last close was a STOP LOSS. Every other
    /// close cause (tp / veto / time-stop) keeps the base `cooldown_min` window — re-entering
    /// a market that just stopped us out is the churn pattern that bled the edge.
    #[serde(default = "default_cooldown_after_sl_min")]
    pub cooldown_after_sl_min: u64,
    /// Churn control (T1): max entries taken before 12:00 UTC (`GateRefusal::Paced`). Stops
    /// the daily cap being exhausted in the first two hours of the session.
    #[serde(default = "default_morning_entry_budget")]
    pub morning_entry_budget: usize,
    /// Regime gate (T1): while BTC `features.vol1h` exceeds this, NEW entries are refused
    /// (`GateRefusal::Regime`). Reviews/exits are unaffected.
    #[serde(default = "default_regime_vol_max")]
    pub regime_vol_max: f64,
    /// Minimum reward:risk for an `open` decision that states BOTH brackets —
    /// `tp_pct >= min_rr * stop_pct` (`GateRefusal::MinRR`, Batch B 2026-08-24, Alpha Arena
    /// lesson: minimum 2:1). Decisions omitting either bracket keep the sizer's default
    /// bracket and are NOT refused by this gate.
    #[serde(default = "default_min_rr")]
    pub min_rr: f64,
}

fn default_web_retrieval() -> bool {
    true
}

fn default_analysts_enabled() -> bool {
    true
}

fn default_tavily_key_env() -> String {
    "TAVILY_API_KEY".into()
}

fn default_exa_key_env() -> String {
    "EXA_API_KEY".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnalystCfg {
    /// Master switch: disables all analyst upstream activity while trigger-based position
    /// management continues.
    #[serde(default = "default_analysts_enabled")]
    pub enabled: bool,
    pub base_url: String,
    /// Deprecated single-model configuration. It is retained only so old test/config fixtures
    /// synthesize a one-entry roster.
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub models: Vec<AnalystModelCfg>,
    #[serde(default)]
    pub chat_model: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub api_key_env: String,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default = "default_web_retrieval")]
    pub web_retrieval: bool,
    #[serde(default = "default_tavily_key_env")]
    pub tavily_key_env: String,
    #[serde(default = "default_exa_key_env")]
    pub exa_key_env: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnalystModelCfg {
    pub id: String,
    #[serde(default = "default_api_style")]
    pub api_style: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Per-model sampling temperature override. None -> provider default (no key sent).
    /// Guidance: DeepSeek 1.0 for data-analysis variety, Mimo structure-first ~0.7,
    /// Nemotron balanced ~0.8, fallback ~0.7. Clamped 0.0-2.0 when sent.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Per-model output token budget override. None -> global AnalystCfg.max_completion_tokens
    /// (which itself defaults to server default when None). hy3/mimo bumped to 800 to
    /// avoid no_json truncation; nemotron-lightning/laguna capped 400 to avoid 60s timeouts.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

fn default_api_style() -> String { "responses".into() }
fn default_enabled() -> bool { true }

/// Map model-id patterns to free-tier sensible temperature defaults when the TOML omits
/// `temperature`. Callers should prefer `AnalystModelCfg::effective_temperature`.
pub fn default_temperature_for_model(id: &str) -> Option<f64> {
    let lower = id.to_ascii_lowercase();
    if lower.contains("deepseek") {
        Some(1.0)
    } else if lower.contains("mimo") {
        Some(0.7)
    } else if lower.contains("nemotron") {
        Some(0.8)
    } else if lower.contains("laguna") || lower.contains("hy3") || lower.contains("big-pickle") || lower.contains("x-preview") {
        Some(0.7)
    } else {
        None
    }
}

impl AnalystModelCfg {
    pub fn effective_temperature(&self) -> Option<f64> {
        self.temperature.or_else(|| default_temperature_for_model(&self.id))
    }
    pub fn effective_max_tokens(&self, global: Option<u32>) -> Option<u32> {
        self.max_tokens.or(global).or_else(|| {
            let lower = self.id.to_ascii_lowercase();
            if lower.contains("hy3") || lower.contains("mimo") {
                Some(800)
            } else if lower.contains("lightning") || lower.contains("laguna") {
                Some(400)
            } else {
                None
            }
        })
    }
}

impl AnalystCfg {
    pub fn roster(&self) -> Vec<AnalystModelCfg> {
        if self.models.is_empty() && !self.model.is_empty() {
            vec![AnalystModelCfg { id: self.model.clone(), api_style: "responses".into(), enabled: true, temperature: None, max_tokens: None }]
        } else { self.models.clone() }
    }
    pub fn enabled_models(&self) -> Vec<AnalystModelCfg> {
        self.roster().into_iter().filter(|m| m.enabled).collect()
    }
    pub fn chat_model_id(&self) -> String {
        self.chat_model.clone().or_else(|| self.enabled_models().into_iter().find(|m| m.api_style == "chat").map(|m| m.id)).unwrap_or_else(|| self.model.clone())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewsCfg {
    pub rss: Vec<String>,
    #[serde(default)]
    pub tavily_key_env: String,
    #[serde(default)]
    pub extra_keywords: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotifyCfg {
    pub bot_token_env: String,
    #[serde(default)]
    pub chat_id: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("io error reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("toml parse error in {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let p = path.as_ref();
        let s = std::fs::read_to_string(p).map_err(|e| ConfigError::Io {
            path: p.display().to_string(),
            source: e,
        })?;
        let cfg: Self = toml::from_str(&s).map_err(|e| ConfigError::Parse {
            path: p.display().to_string(),
            source: e,
        })?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_kestreld_toml_and_checks_key_fields() {
        let cfg = Config::load("kestreld.toml").expect("load kestreld.toml");
        assert_eq!(cfg.server.port, 7411, "port");
        assert_eq!(cfg.screener.top_k, 15, "top_k");
        assert!(
            (cfg.risk.kill_switch_pct - 12.0).abs() < f64::EPSILON,
            "kill_switch_pct expected 12.0 got {}",
            cfg.risk.kill_switch_pct
        );
        assert!(!cfg.risk.kill_enabled, "paper collection kill switch is opt-in");
        // These operational toggles are intentionally pinned to the current runtime TOML state.
        assert!(cfg.risk.entries_enabled, "new entries must be enabled when trading is resumed");
        assert!(cfg.analyst.enabled, "analysts must be enabled when trading is resumed");
        assert_eq!(cfg.analyst.enabled_models().len(), 9, "free analyst roster");
        assert_eq!(cfg.analyst.chat_model_id(), "x-preview-f-free");
        assert!(cfg.analyst.web_retrieval, "web_retrieval default true");
        assert_eq!(cfg.analyst.tavily_key_env, "TAVILY_API_KEY");
        assert_eq!(cfg.analyst.exa_key_env, "EXA_API_KEY");
        assert!(
            cfg.analyst.max_completion_tokens.is_none(),
            "kestreld.toml must omit max_completion_tokens (user-locked server default; 60s timeout bounds wall time), got {:?}",
            cfg.analyst.max_completion_tokens
        );

        // sanity check remaining defaults exist
        assert_eq!(cfg.universe.dexs, vec!["", "xyz"]);
        assert!((cfg.universe.min_vlm_native - 2000000.0).abs() < f64::EPSILON);
        assert!((cfg.universe.min_vlm_dex - 500000.0).abs() < f64::EPSILON);
        assert_eq!(
            cfg.universe.allowlist,
            vec!["BTC", "ETH", "SOL", "HYPE", "XRP", "DOGE", "BNB"],
            "majors-only pilot (Batch B 2026-08-24)"
        );
        assert_eq!(cfg.screener.interval_s, 45);
        assert!((cfg.screener.min_score - 1.8).abs() < f64::EPSILON);
        assert_eq!(cfg.screener.skip_recheck_min, 15);
        assert!((cfg.screener.skip_recheck_score_jump - 0.5).abs() < f64::EPSILON);
        assert!((cfg.sizing.bankroll - 1000.0).abs() < f64::EPSILON);
        assert!((cfg.sizing.vol_ref - 0.4).abs() < f64::EPSILON);
        assert_eq!(cfg.risk.max_feature_age_s, 120, "staleness gate window");

        // T1 churn/regime knobs (user-locked amendment #9, 2026-08-09)
        assert_eq!(cfg.risk.per_market_daily_cap, 3, "per-market daily entry cap");
        assert_eq!(cfg.risk.cooldown_after_sl_min, 120, "post-SL cooldown minutes");
        assert_eq!(cfg.risk.morning_entry_budget, 12, "entries allowed before 12:00 UTC");
        assert!(
            (cfg.risk.regime_vol_max - 1.5).abs() < f64::EPSILON,
            "regime_vol_max expected 1.5 got {}",
            cfg.risk.regime_vol_max
        );
        // user directive 2026-08-22: the configured zero disables the daily cap.
        assert_eq!(cfg.risk.daily_cap, 0, "daily_cap zero means unlimited entries");
        assert!((cfg.risk.conviction_min - 0.75).abs() < f64::EPSILON, "data-backed profitable band floor (Batch B)");
        assert_eq!(cfg.risk.max_concurrent, 0, "zero means unlimited per-analyst positions");
        assert_eq!(cfg.risk.global_max_concurrent, 0, "zero means unlimited global positions");
        assert_eq!(cfg.risk.cooldown_min, 30, "base cooldown user-locked at 30m");
        assert!((cfg.risk.min_rr - 2.0).abs() < f64::EPSILON, "RR floor (Batch B 2:1)");
    }

    #[test]
    fn t1_churn_and_regime_knobs_default_when_missing() {
        // The daemon must boot on a config written before T1 landed: every new knob carries
        // its user-locked value as a serde default.
        let cfg: Config = toml::from_str(MINIMAL_TOML).expect("parse without T1 knobs");
        assert!(cfg.risk.entries_enabled, "entries stay enabled for pre-halt configs");
        assert!(cfg.analyst.enabled, "analysts stay enabled for pre-switch configs");
        assert_eq!(cfg.screener.skip_recheck_min, 15);
        assert!((cfg.screener.skip_recheck_score_jump - 0.5).abs() < f64::EPSILON);
        assert_eq!(cfg.risk.per_market_daily_cap, 3);
        assert_eq!(cfg.risk.cooldown_after_sl_min, 120);
        assert_eq!(cfg.risk.morning_entry_budget, 12);
        assert!((cfg.risk.regime_vol_max - 1.5).abs() < f64::EPSILON);
        assert!(!cfg.risk.kill_enabled, "kill switch defaults off when omitted");
        assert!((cfg.risk.min_rr - 2.0).abs() < f64::EPSILON, "min_rr defaults to 2.0 when omitted");
        assert!(cfg.universe.allowlist.is_empty(), "allowlist defaults off when omitted");

        let legacy = MINIMAL_TOML.replace("            daily_cap = 20\n", "");
        let legacy_cfg: Config = toml::from_str(&legacy).expect("parse config before daily_cap existed");
        assert_eq!(legacy_cfg.risk.daily_cap, 20, "omitted daily_cap keeps the legacy default");

        let overridden = MINIMAL_TOML.replace(
            "time_stop_hours = 24.0",
            "time_stop_hours = 24.0\n            per_market_daily_cap = 1\n            cooldown_after_sl_min = 240\n            morning_entry_budget = 4\n            regime_vol_max = 0.9",
        ).replace(
            "min_score = 1.8",
            "min_score = 1.8\n            skip_recheck_min = 3\n            skip_recheck_score_jump = 0.8",
        );
        let cfg2: Config = toml::from_str(&overridden).expect("parse with explicit T1 knobs");
        assert_eq!(cfg2.risk.per_market_daily_cap, 1, "explicit value wins");
        assert_eq!(cfg2.risk.cooldown_after_sl_min, 240);
        assert_eq!(cfg2.risk.morning_entry_budget, 4);
        assert!((cfg2.risk.regime_vol_max - 0.9).abs() < f64::EPSILON);
        assert_eq!(cfg2.screener.skip_recheck_min, 3, "explicit value wins");
        assert!((cfg2.screener.skip_recheck_score_jump - 0.8).abs() < f64::EPSILON);
        let enabled = MINIMAL_TOML.replace("time_stop_hours = 24.0", "time_stop_hours = 24.0\n            kill_enabled = true");
        let cfg3: Config = toml::from_str(&enabled).expect("parse explicit kill switch");
        assert!(cfg3.risk.kill_enabled, "explicit true re-enables kill switch");
    }

    /// A config with every REQUIRED key and no optional ones — the defaults fixture.
    const MINIMAL_TOML: &str = r#"
            [server]
            port = 7411
            [universe]
            dexs = ["", "xyz"]
            min_vlm_native = 2000000.0
            min_vlm_dex = 500000.0
            [screener]
            interval_s = 45
            top_k = 6
            min_score = 1.8
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
            conviction_min = 0.50
            review_interval_min = 15
            time_stop_hours = 24.0
            [analyst]
            base_url = "https://api.meta.ai/v1"
            model = "muse-spark-1.2-contributor"
            api_key_env = "ANALYST_API_KEY"
            [news]
            rss = []
            [notify]
            bot_token_env = "TG_BOT_TOKEN"
        "#;

    #[test]
    fn max_feature_age_defaults_120_when_missing() {
        let toml = r#"
            [server]
            port = 7411
            [universe]
            dexs = ["", "xyz"]
            min_vlm_native = 2000000.0
            min_vlm_dex = 500000.0
            [screener]
            interval_s = 45
            top_k = 6
            min_score = 1.8
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
            conviction_min = 0.50
            review_interval_min = 15
            time_stop_hours = 24.0
            [analyst]
            base_url = "https://api.meta.ai/v1"
            model = "muse-spark-1.2-contributor"
            api_key_env = "ANALYST_API_KEY"
            [news]
            rss = []
            [notify]
            bot_token_env = "TG_BOT_TOKEN"
        "#;
        let cfg: Config = toml::from_str(toml).expect("parse missing max_feature_age_s");
        assert_eq!(cfg.risk.max_feature_age_s, 120, "serde default 120 when missing");

        let with_override = toml.replace("time_stop_hours = 24.0", "time_stop_hours = 24.0\n            max_feature_age_s = 45");
        let cfg2: Config = toml::from_str(&with_override).expect("parse explicit max_feature_age_s");
        assert_eq!(cfg2.risk.max_feature_age_s, 45, "explicit value wins");
    }

    #[test]
    fn analyst_web_retrieval_defaults_when_missing() {
        let toml = r#"
            [server]
            port = 7411
            [universe]
            dexs = ["", "xyz"]
            min_vlm_native = 2000000.0
            min_vlm_dex = 500000.0
            [screener]
            interval_s = 45
            top_k = 6
            min_score = 1.8
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
            conviction_min = 0.50
            review_interval_min = 15
            time_stop_hours = 24.0
            [analyst]
            base_url = "https://api.meta.ai/v1"
            model = "muse-spark-1.2-contributor"
            api_key_env = "ANALYST_API_KEY"
            [news]
            rss = []
            [notify]
            bot_token_env = "TG_BOT_TOKEN"
        "#;
        let cfg: Config = toml::from_str(toml).expect("parse missing web retrieval fields");
        assert!(cfg.analyst.web_retrieval, "serde default true when missing");
        assert_eq!(cfg.analyst.tavily_key_env, "TAVILY_API_KEY");
        assert_eq!(cfg.analyst.exa_key_env, "EXA_API_KEY");
        assert!(
            cfg.analyst.max_completion_tokens.is_none(),
            "max_completion_tokens default None when missing (user-locked server default)"
        );
    }

    #[test]
    fn roster_defaults_and_legacy_model_fallback() {
        let cfg: Config = toml::from_str(MINIMAL_TOML).expect("legacy config parses");
        let roster = cfg.analyst.enabled_models();
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].id, "muse-spark-1.2-contributor");
        assert_eq!(roster[0].api_style, "responses");

        let modern = MINIMAL_TOML.replace(
            "model = \"muse-spark-1.2-contributor\"",
            "models = [{ id = \"a\", api_style = \"chat\" }, { id = \"b\", enabled = false }]\n            chat_model = \"a\"",
        );
        let cfg: Config = toml::from_str(&modern).expect("roster config parses");
        assert_eq!(cfg.analyst.enabled_models().len(), 1);
        assert_eq!(cfg.analyst.chat_model_id(), "a");
    }

    #[test]
    fn analyst_max_completion_tokens_some_when_present() {
        let toml = r#"
            [server]
            port = 7411
            [universe]
            dexs = ["", "xyz"]
            min_vlm_native = 2000000.0
            min_vlm_dex = 500000.0
            [screener]
            interval_s = 45
            top_k = 6
            min_score = 1.8
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
            conviction_min = 0.50
            review_interval_min = 15
            time_stop_hours = 24.0
            [analyst]
            base_url = "https://api.meta.ai/v1"
            model = "muse-spark-1.2-contributor"
            api_key_env = "ANALYST_API_KEY"
            max_completion_tokens = 16000
            [news]
            rss = []
            [notify]
            bot_token_env = "TG_BOT_TOKEN"
        "#;
        let cfg: Config = toml::from_str(toml).expect("parse with max_completion_tokens");
        assert_eq!(cfg.analyst.max_completion_tokens, Some(16000));
    }
}
