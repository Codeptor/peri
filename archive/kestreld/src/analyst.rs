#![allow(dead_code)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_return)]
#![allow(unused_imports)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, warn};

use crate::config::{AnalystCfg, AnalystModelCfg};
use crate::contracts::{Decision, Features, NewsItem, Nominee, Position, Side};

#[derive(Debug, Clone)]
pub struct AnalystInput {
    pub nominee: Nominee,
    pub news: Vec<NewsItem>,
    pub open_positions: Vec<Position>,
    pub market_hours: &'static str,
    pub atr_pct: Option<f64>,
    /// Last closes for the NOMINEE's market, newest first (`Store::recent_closes`, cap 3).
    /// Empty renders `none` — the model must be able to tell "never traded" from "no data".
    pub recent: Vec<RecentOutcome>,
    /// Where the book stands right now: equity, the day's move, and how much of the kill
    /// budget it has already spent.
    pub account: AccountState,
    /// Live mid per open-position market, for the open-book uPnL lines. A market missing
    /// from the snapshot renders `n/a` rather than a fabricated number (same money guard as
    /// `Store::equity`).
    pub open_marks: std::collections::HashMap<String, f64>,
    /// Nominee's cached candles (15m intraday + 4h context), freshness-gated by the caller:
    /// absent from the cache or fetched >30min ago ⇒ None, and the prompt prints
    /// `series: unavailable` rather than blocking the decide (same rule as `atr_pct`).
    pub candles: Option<crate::hl_rest::MarketCandles>,
    /// Snapshot open interest for the nominee market; None when it has no snapshot row.
    pub open_interest: Option<f64>,
    /// Snapshot funding rate for the nominee market; None when it has no snapshot row.
    pub funding: Option<f64>,
    /// Which model is deciding — used for the per-model performance feedback line.
    #[allow(dead_code)]
    pub analyst_id: String,
    /// Last 10 closed trades for THIS analyst, newest first (ledger query). Drives the
    /// `your stats (last 10 closed): win 3/10 avg -0.42 total -4.2 | open n=2 uPnL -1.2` line.
    pub analyst_recent: Vec<RecentOutcome>,
}

/// One closed trade on the nominee's market, as the entry prompt reads it back.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentOutcome {
    /// Close cause: `tp` / `sl` / `time_stop` / `veto_close`.
    pub action: String,
    /// Realized minus that exit's fee.
    pub net_pnl: f64,
    pub hours_ago: f64,
}

/// Account state at decision time. Computed by the caller (the daemon owns the config); the
/// analyst only renders it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AccountState {
    pub equity: f64,
    /// Equity move since the day-open stamp, in percent.
    pub day_pnl_pct: f64,
    /// Share of the kill-switch drawdown budget already spent, 0-100+. `0` while the book is
    /// flat or up on the day.
    pub kill_budget_used_pct: f64,
}

#[derive(Debug, Clone)]
pub struct AnalystOutcome {
    pub decision: Option<Decision>,
    pub model_used: String,
    pub refused: bool,
    pub latency_ms: u64,
    /// Forensic: refusal classification for decisions log (~45% of first attempts).
    /// None on success path (never emitted on success). Values: empty_output, no_json,
    /// incomplete_max_tokens, http_err, timeout
    pub refusal_kind: Option<String>,
    /// Bounded excerpt: first 120 chars of output_text single-line sanitized for
    /// empty-ish kinds; for http_err/timeout the error class only. Never raw secrets.
    pub refusal_excerpt: Option<String>,
    /// Client-side retrieval queries issued for prompt context. Collected for forensics
    /// `reason` as `searched:"q1","q2"`; never used for decision logic.
    pub searched_queries: Vec<String>,
    pub calls: Vec<AnalystCall>,
}

#[derive(Debug, Clone)]
pub struct AnalystCall {
    pub prompt: String,
    pub response_raw: String,
    pub outcome_kind: String,
    pub parsed_json: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Error)]
pub enum AnalystError {
    #[error("http error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone)]
struct Refusal {
    kind: String,
    excerpt: String,
    raw: String,
}

struct PrimarySuccess {
    decision: Decision,
    raw: String,
}

/// Sanitize excerpt: single line, no newlines, first 120 chars, trimmed.
/// Never log raw secrets or long payloads.
fn sanitize_excerpt(s: &str) -> String {
    let single = s.replace(['\r', '\n'], " ");
    // collapse? keep single spaces, just first 120 chars
    let excerpt: String = single.chars().take(120).collect();
    excerpt.trim().to_string()
}

const WEB_RETRIEVAL_TTL_MS: i64 = 15 * 60 * 1000;
const WEB_RETRIEVAL_TIMEOUT: Duration = Duration::from_secs(8);
const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
const EXA_SEARCH_URL: &str = "https://api.exa.ai/search";
static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct RetrievedContext {
    block: String,
    queries: Vec<String>,
}

#[derive(Debug, Clone)]
struct WebResult {
    title: String,
    url: String,
    date: Option<String>,
    snippet: String,
}

pub(crate) struct WebRetriever {
    cache: tokio::sync::Mutex<HashMap<String, (i64, Option<String>)>>,
    tavily_url: String,
    exa_url: String,
}

impl WebRetriever {
    pub(crate) fn new() -> Self {
        Self::with_urls(TAVILY_SEARCH_URL.into(), EXA_SEARCH_URL.into())
    }

    fn with_urls(tavily_url: String, exa_url: String) -> Self {
        Self {
            cache: tokio::sync::Mutex::new(HashMap::new()),
            tavily_url,
            exa_url,
        }
    }

    async fn retrieve(
        &self,
        client: &reqwest::Client,
        cfg: &AnalystCfg,
        market: &str,
    ) -> Option<RetrievedContext> {
        let coin = market.split(':').next_back().unwrap_or(market);
        let query = format!("{coin} crypto market news");
        let now = chrono::Utc::now().timestamp_millis();
        {
            let cache = self.cache.lock().await;
            if let Some((cached_at, cached)) = cache.get(market)
                && now - *cached_at < WEB_RETRIEVAL_TTL_MS
            {
                return cached.clone().map(|block| RetrievedContext {
                    block,
                    queries: vec![query],
                });
            }
        }

        let tavily_key = (!cfg.tavily_key_env.is_empty())
            .then(|| std::env::var(&cfg.tavily_key_env).ok())
            .flatten();
        let exa_key = (!cfg.exa_key_env.is_empty())
            .then(|| std::env::var(&cfg.exa_key_env).ok())
            .flatten();
        let (tavily, exa) = tokio::join!(
            fetch_tavily(client, &self.tavily_url, &query, tavily_key),
            fetch_exa(client, &self.exa_url, &query, exa_key),
        );
        let mut results = Vec::new();
        match tavily {
            Ok(items) => results.extend(items),
            Err(error) => {
                warn!(provider = "tavily", error = %error, "web retrieval failed; omitting provider context")
            }
        }
        match exa {
            Ok(items) => results.extend(items),
            Err(error) => {
                warn!(provider = "exa", error = %error, "web retrieval failed; omitting provider context")
            }
        }
        let block = format_retrieved_context(results);
        self.cache
            .lock()
            .await
            .insert(market.to_string(), (now, block.clone()));
        block.map(|block| RetrievedContext {
            block,
            queries: vec![query],
        })
    }
}

async fn fetch_tavily(
    client: &reqwest::Client,
    url: &str,
    query: &str,
    key: Option<String>,
) -> Result<Vec<WebResult>, String> {
    let key = key.ok_or_else(|| "configured API key env is not set".to_string())?;
    let request = client
        .post(url)
        .header("Authorization", format!("Bearer {key}"))
        .json(&serde_json::json!({"query": query, "topic": "news", "days": 1, "max_results": 3}));
    let response = tokio::time::timeout(WEB_RETRIEVAL_TIMEOUT, request.send())
        .await
        .map_err(|_| "request timed out".to_string())?
        .map_err(|_| "request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "invalid response JSON".to_string())?;
    Ok(value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(WebResult {
                title: item.get("title")?.as_str()?.to_string(),
                url: item.get("url")?.as_str()?.to_string(),
                date: item
                    .get("published_date")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                snippet: item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect())
}

async fn fetch_exa(
    client: &reqwest::Client,
    url: &str,
    query: &str,
    key: Option<String>,
) -> Result<Vec<WebResult>, String> {
    let key = key.ok_or_else(|| "configured API key env is not set".to_string())?;
    let request = client
        .post(url)
        .header("x-api-key", key)
        .json(&serde_json::json!({"query": query, "numResults": 3, "contents": {"text": true}}));
    let response = tokio::time::timeout(WEB_RETRIEVAL_TIMEOUT, request.send())
        .await
        .map_err(|_| "request timed out".to_string())?
        .map_err(|_| "request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "invalid response JSON".to_string())?;
    Ok(value["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let snippet = item
                .get("highlights")
                .and_then(|v| v.as_array())
                .map(|highlights| {
                    highlights
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .filter(|snippet| !snippet.is_empty())
                .or_else(|| {
                    item.get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_default();
            Some(WebResult {
                title: item.get("title")?.as_str()?.to_string(),
                url: item.get("url")?.as_str()?.to_string(),
                date: item
                    .get("publishedDate")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                snippet,
            })
        })
        .collect())
}

fn format_retrieved_context(results: Vec<WebResult>) -> Option<String> {
    let mut seen_urls = HashSet::new();
    let mut block = "RETRIEVED WEB CONTEXT (unverified, may be stale):\n".to_string();
    let mut count = 0;
    for result in results {
        if count == 6 || !seen_urls.insert(result.url.clone()) {
            continue;
        }
        let title = sanitize_excerpt(&result.title);
        let snippet: String = result
            .snippet
            .replace(['\r', '\n'], " ")
            .chars()
            .take(200)
            .collect();
        let date = result
            .date
            .as_deref()
            .map(|d| format!(" ({})", sanitize_excerpt(d)))
            .unwrap_or_default();
        let line = format!("- {title}{date} — {} {}\n", snippet.trim(), result.url);
        if block.len() + line.len() > 1800 {
            break;
        }
        block.push_str(&line);
        count += 1;
    }
    (count > 0).then_some(block)
}

const SYSTEM_PREAMBLE: &str = "You are the decision module of an automated PAPER trading simulator; no real funds exist; output is consumed by a simulation engine; this is quantitative research, not financial advice to a human. Decide top-down: (1) 4h trend (EMA20 vs EMA50, MACD, RSI14), (2) does 15m agree (close vs EMA20, RSI, MACD), (3) invalidation level where thesis is proven wrong, (4) risk/reward >=2:1 and leverage choice, (5) verdict — open only if all agree, else skip (default to skip on conflict). In thesis, state both bull case and bear case / why this could fail in one sentence. Respond with ONLY compact json matching the Decision schema (json). Schema: {\"action\":\"open\"|\"skip\", \"side\":\"long\"|\"short\"|null, \"conviction\":0.0-1.0, \"thesis\":string, \"horizon_hours\":number|null, \"stop_pct\":number|null, \"tp_pct\":number|null, \"invalidation_condition\":string|null, \"risk_usd\":number|null, \"leverage\":number|null} Rules: opens must include stop_pct and tp_pct with tp_pct >= 2x stop_pct (minimum 2:1 reward:risk); include a concrete invalidation_condition (e.g. 'if 4h RSI14 breaks below 40'); leverage: 1.0-10.0 (1x-10x), majors up to 10x, xyz up to 5x, choose based on conviction/volatility. Example open: {\"action\":\"open\",\"side\":\"long\",\"conviction\":0.8,\"thesis\":\"momentum + funding but could fail if 4h breaks down / bear case invalid\",\"horizon_hours\":24,\"stop_pct\":1.2,\"tp_pct\":2.4,\"invalidation_condition\":\"if 4h RSI14 breaks below 40\",\"risk_usd\":12.0,\"leverage\":5.0} Example skip: {\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"low edge\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null,\"invalidation_condition\":null,\"risk_usd\":null,\"leverage\":null} No prose, no disclaimer, only json. Your reply MUST contain at least one complete JSON object {...} matching the schema. Output the JSON object FIRST. Begin your reply with \"{\". If you must add commentary, place it AFTER the JSON. You may call get_orderbook_imbalance / get_funding_context / get_candle_context to gather more evidence before deciding (max 1 tool round).";

/// Paper fills charge 7.5bp per side (`ledger.rs`), so a round trip costs ≈15bp of notional.
/// Day-1 evidence: fees ran at 2x the gross edge because entries were taken for moves that
/// could never clear the spread. One line, identical in both prompts, so the entry model and
/// the review model price the same hurdle.
const FEE_HURDLE_LINE: &str = "FEE HURDLE: a round trip costs ~15bp of notional (7.5bp in + 7.5bp out); only open when the expected move clears at least 3x that cost (~0.45% of notional).";

/// Per-side taker fee used by the paper ledger — mirrored here only to ESTIMATE fees already
/// paid on an open position for the review prompt. `ledger.rs` remains the authority for what
/// is actually charged.
const FEE_RATE: f64 = 0.00075;

// ── Adaptive tool-use (read-only, max 1 tool round = 2 LLM calls) ───────────────────────
//
// Three tools, all backed by existing in-process caches — no new network fetchers:
//
// * `get_orderbook_imbalance` — bid/ask vol, spread_bps, imbalance from `book_cache` l2Book
// * `get_funding_context` — funding, funding_z, 24h mean/std, last 8 samples from FeatureEngine ring
// * `get_candle_context` — closes + ema20/macd_hist/rsi7/rsi14 on cached candles (reuses features.rs)
//
// Chat-completions path only: OpenAI `tools: [{type:"function",...}]` + `tool_choice:"auto"`.
// For `responses` api_style (muse-spark) we skip tools gracefully (one-shot) — leave TODO.
// Streaming: tool-use path uses non-streaming `chat/completions` to capture tool_calls cleanly;
// the existing streaming path is kept for the non-tool fallback.
const TOOL_CANDLE_STALE_MS: i64 = 30 * 60 * 1000;
const TOOL_PER_CALL_TIMEOUT: Duration = Duration::from_secs(2);

pub type BookCache = std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::contracts::L2Book>>>;
pub type CandleCache = std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::hl_rest::MarketCandles>>>;

/// Pure tool executor: holds Arc clones of read-only caches, never blocks on network >2s,
/// returns short JSON strings. Failures → "unavailable" and never fail the decide.
#[derive(Clone)]
pub struct ToolExecutor {
    pub book_cache: BookCache,
    pub engine: std::sync::Arc<tokio::sync::Mutex<crate::features::FeatureEngine>>,
    pub candle_cache: CandleCache,
}

impl ToolExecutor {
    pub fn new(book_cache: BookCache, engine: std::sync::Arc<tokio::sync::Mutex<crate::features::FeatureEngine>>, candle_cache: CandleCache) -> Self {
        Self { book_cache, engine, candle_cache }
    }

    /// `get_orderbook_imbalance { market, depth? }`
    pub async fn get_orderbook_imbalance(&self, market: &str, depth: Option<u8>) -> String {
        let d = depth.unwrap_or(10) as usize;
        let d = d.clamp(1, 20);
        let cache = self.book_cache.read().await;
        let Some(book) = cache.get(market) else {
            return r#"{"unavailable":"no book for market"}"#.to_string();
        };
        if book.levels[0].is_empty() || book.levels[1].is_empty() {
            return r#"{"unavailable":"empty book"}"#.to_string();
        }
        let bids = &book.levels[0];
        let asks = &book.levels[1];
        let take = |levels: &[crate::contracts::L2Level]| -> (f64, f64) {
            let n = levels.len().min(d);
            let vol: f64 = levels[..n].iter().map(|l| l.sz).sum();
            let best_px = levels.first().map(|l| l.px).unwrap_or(0.0);
            (vol, best_px)
        };
        let (bid_vol, bid_px) = take(bids);
        let (ask_vol, ask_px) = take(asks);
        if bid_px <= 0.0 || ask_px <= 0.0 || bid_vol + ask_vol <= 1e-12 {
            return r#"{"unavailable":"invalid book levels"}"#.to_string();
        }
        let mid = (bid_px + ask_px) * 0.5;
        let spread_bps = if mid > 0.0 { (ask_px - bid_px) / mid * 10000.0 } else { 0.0 };
        let imbalance = (bid_vol - ask_vol) / (bid_vol + ask_vol);
        let imbalance = imbalance.clamp(-1.0, 1.0);
        serde_json::json!({
            "market": market,
            "depth": d,
            "bid_vol": bid_vol,
            "ask_vol": ask_vol,
            "bid_px": bid_px,
            "ask_px": ask_px,
            "spread_bps": spread_bps,
            "imbalance": imbalance
        }).to_string()
    }

    /// `get_funding_context { market }`
    pub async fn get_funding_context(&self, market: &str) -> String {
        let eng = self.engine.lock().await;
        let vals_opt = eng.funding_hist_vals(market);
        let Some(vals) = vals_opt else {
            return r#"{"unavailable":"no funding history for market"}"#.to_string();
        };
        if vals.is_empty() {
            return r#"{"unavailable":"empty funding history"}"#.to_string();
        }
        let current = *vals.last().unwrap_or(&0.0);
        // funding_z via Features if available else 0
        let funding_z = eng.features(market).map(|f| f.funding_z).unwrap_or(0.0);
        // 24h mean/std over last 24 samples (hourly funding → 24 = 1 day)
        let n24 = vals.len().min(24);
        let slice24 = &vals[vals.len() - n24..];
        let mean24 = slice24.iter().sum::<f64>() / n24 as f64;
        let var24 = slice24.iter().map(|v| (v - mean24).powi(2)).sum::<f64>() / n24 as f64;
        let std24 = var24.sqrt();
        let last8: Vec<f64> = vals.iter().rev().take(8).rev().copied().collect();
        serde_json::json!({
            "market": market,
            "current_funding": current,
            "funding_z": funding_z,
            "mean_24h": mean24,
            "std_24h": std24,
            "last_8": last8
        }).to_string()
    }

    /// `get_candle_context { market, interval, limit? }`
    pub async fn get_candle_context(&self, market: &str, interval: &str, limit: Option<u8>) -> String {
        let lim = limit.unwrap_or(10) as usize;
        let lim = lim.clamp(1, 20);
        if interval != "15m" && interval != "4h" {
            return r#"{"unavailable":"interval must be 15m or 4h"}"#.to_string();
        }
        let cache = self.candle_cache.read().await;
        let Some(mc) = cache.get(market) else {
            return r#"{"unavailable":"no candles for market"}"#.to_string();
        };
        if chrono::Utc::now().timestamp_millis() - mc.fetched_ms > TOOL_CANDLE_STALE_MS {
            return r#"{"unavailable":"candle cache stale >30m"}"#.to_string();
        }
        let candles: &[crate::hl_rest::Candle] = if interval == "15m" { &mc.m15 } else { &mc.h4 };
        if candles.is_empty() {
            return r#"{"unavailable":"empty candle slice"}"#.to_string();
        }
        let n = candles.len().min(lim);
        let slice = &candles[candles.len() - n..];
        let closes: Vec<f64> = slice.iter().map(|c| c.c).collect();
        // reuse features.rs fns on this slice (not the full history — short slice is what the tool asked for)
        let ema20 = crate::features::ema(&closes, 20);
        let macd_hist = crate::features::macd_hist(&closes);
        let rsi7 = crate::features::rsi(&closes, 7);
        let rsi14 = crate::features::rsi(&closes, 14);
        let fmt_rsi = |v: &[Option<f64>]| -> Vec<serde_json::Value> {
            v.iter().map(|o| match o { Some(x) => serde_json::json!(*x), None => serde_json::Value::Null }).collect()
        };
        serde_json::json!({
            "market": market,
            "interval": interval,
            "limit": lim,
            "closes": closes,
            "ema20": ema20,
            "macd_hist": macd_hist,
            "rsi7": fmt_rsi(&rsi7),
            "rsi14": fmt_rsi(&rsi14)
        }).to_string()
    }

    /// Dispatch by tool name, with per-tool 2s timeout and "unavailable" fallback.
    pub async fn dispatch(&self, name: &str, args: &serde_json::Value) -> String {
        let fut = async {
            match name {
                "get_orderbook_imbalance" => {
                    let market = args.get("market").and_then(|v| v.as_str()).unwrap_or("");
                    if market.is_empty() { return r#"{"unavailable":"missing market"}"#.to_string(); }
                    let depth = args.get("depth").and_then(|v| v.as_u64()).map(|v| v as u8);
                    self.get_orderbook_imbalance(market, depth).await
                },
                "get_funding_context" => {
                    let market = args.get("market").and_then(|v| v.as_str()).unwrap_or("");
                    if market.is_empty() { return r#"{"unavailable":"missing market"}"#.to_string(); }
                    self.get_funding_context(market).await
                },
                "get_candle_context" => {
                    let market = args.get("market").and_then(|v| v.as_str()).unwrap_or("");
                    if market.is_empty() { return r#"{"unavailable":"missing market"}"#.to_string(); }
                    let interval = args.get("interval").and_then(|v| v.as_str()).unwrap_or("15m");
                    let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as u8);
                    self.get_candle_context(market, interval, limit).await
                },
                _ => r#"{"unavailable":"unknown tool"}"#.to_string(),
            }
        };
        match tokio::time::timeout(TOOL_PER_CALL_TIMEOUT, fut).await {
            Ok(s) => s,
            Err(_) => r#"{"unavailable":"tool timeout >2s"}"#.to_string(),
        }
    }
}

pub fn tool_definitions() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_orderbook_imbalance",
                "description": "Get orderbook imbalance for a market from the live l2Book snapshot. Returns bid_vol/ask_vol, spread_bps, imbalance [-1..1]. Use book_cache or l2Book snapshot; fallback 'unavailable'.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "market": {"type": "string", "description": "Market symbol e.g. BTC or xyz:TSLA"},
                        "depth": {"type": "integer", "description": "Depth levels to consider (default 10)", "minimum": 1, "maximum": 20}
                    },
                    "required": ["market"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_funding_context",
                "description": "Get funding context for a market from the FeatureEngine ring. Returns current funding, funding_z, 24h mean/std, last 8 funding samples.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "market": {"type": "string", "description": "Market symbol"}
                    },
                    "required": ["market"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_candle_context",
                "description": "Get candle context for a market from cached candles. Returns closes + ema20/macd_hist/rsi7/rsi14 for the slice. Reuses features.rs fns on cached candles; if cache miss/stale >30m returns unavailable.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "market": {"type": "string", "description": "Market symbol"},
                        "interval": {"type": "string", "enum": ["15m","4h"], "description": "Candle interval"},
                        "limit": {"type": "integer", "description": "Number of candles to return (default 10)", "minimum": 1, "maximum": 20}
                    },
                    "required": ["market","interval"]
                }
            }
        })
    ]
}

#[derive(Debug, Clone, Deserialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: ToolFunction,
}
#[derive(Debug, Clone, Deserialize)]
struct ToolFunction {
    name: String,
    arguments: String,
}

/// Position accounting shown to the reviewer (Phase T1). All values are gross of the exit fee,
/// which does not exist yet.
///
/// * `unrealized_pnl` — mark-to-market on the open size, same sign convention as
///   `ledger::Store::close_position`'s `realized_pnl` (price movement only).
/// * `initial_risk` — `|entry - sl| * size`, the dollars the stop was set to lose. `r_multiple`
///   is uPnL divided by it, so `-1R` is "the stop is being hit".
/// * `fees_paid` — entry fee only (`notional * 7.5bp`); an exit will cost roughly the same again.
///
/// `mark <= 0` or non-finite is UNKNOWN (the review loop propagates `0.0` for a market missing
/// from the snapshot — the same money guard `apply_review_action` uses), and every
/// mark-dependent field degrades to `n/a` instead of printing a fabricated number.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PositionContext {
    mark: Option<f64>,
    unrealized_pnl: Option<f64>,
    r_multiple: Option<f64>,
    held_h: f64,
    fees_paid: f64,
}

fn position_context(position: &Position, mark: f64, now_ms: i64) -> PositionContext {
    let valid_mark = (mark.is_finite() && mark > 0.0).then_some(mark);
    let unrealized_pnl = valid_mark.map(|m| match position.side {
        Side::Long => (m - position.entry_px) * position.size,
        Side::Short => (position.entry_px - m) * position.size,
    });
    // size at entry == current size: the paper trader has no partial-close path in the loops.
    let initial_risk = (position.entry_px - position.sl_px).abs() * position.size;
    let r_multiple = match (unrealized_pnl, initial_risk > 0.0) {
        (Some(pnl), true) => Some(pnl / initial_risk),
        _ => None,
    };
    PositionContext {
        mark: valid_mark,
        unrealized_pnl,
        r_multiple,
        held_h: (now_ms - position.opened_ts).max(0) as f64 / 3_600_000.0,
        fees_paid: position.entry_px * position.size * FEE_RATE,
    }
}

/// At most this many open positions get their own line before the summary collapses the tail.
/// The whole entry-context block is capped at ~12 lines so the news and the schema keep the
/// model's attention.
const OPEN_BOOK_MAX_LINES: usize = 6;

/// "3h ago" / "40m ago" — minutes under the hour, whole hours above it. Negative (a clock
/// step) reads as `0m ago` rather than a future trade.
fn fmt_ago(hours_ago: f64) -> String {
    let h = if hours_ago.is_finite() {
        hours_ago.max(0.0)
    } else {
        0.0
    };
    if h < 1.0 {
        format!("{:.0}m ago", h * 60.0)
    } else {
        format!("{h:.0}h ago")
    }
}

/// `recent SOL: sl -9.10 (3h ago), tp +14.20 (9h ago)` — the market's own recent record.
/// Newest first, `none` when it has never closed here.
fn recent_line(market: &str, recent: &[RecentOutcome]) -> String {
    if recent.is_empty() {
        return format!("recent {market}: none\n");
    }
    let parts: Vec<String> = recent
        .iter()
        .take(3)
        .map(|r| format!("{} {:+.2} ({})", r.action, r.net_pnl, fmt_ago(r.hours_ago)))
        .collect();
    format!("recent {market}: {}\n", parts.join(", "))
}

/// Per-model adaptive line: `your stats (last 10 closed): win 3/10 avg -0.42 total -4.20 | open n=2 uPnL -1.20`
/// Compact one-liner so laguna sees its own -16.53 and can adapt; still cheap for the context budget.
fn analyst_stats_line(input: &AnalystInput) -> String {
    let n = input.analyst_recent.len();
    let wins = input.analyst_recent.iter().filter(|r| r.net_pnl > 0.0).count();
    let total: f64 = input.analyst_recent.iter().map(|r| r.net_pnl).sum();
    let avg = if n > 0 { total / n as f64 } else { 0.0 };
    // Filter open positions belonging to this model.
    let own: Vec<&Position> = if input.analyst_id.is_empty() {
        Vec::new()
    } else {
        input.open_positions.iter().filter(|p| p.analyst == input.analyst_id).collect()
    };
    let open_n = own.len();
    let upnl: f64 = own
        .iter()
        .filter_map(|p| {
            let mark = input.open_marks.get(&p.market).copied().filter(|m| m.is_finite() && *m > 0.0)?;
            Some(match p.side {
                Side::Long => (mark - p.entry_px) * p.size,
                Side::Short => (p.entry_px - mark) * p.size,
            })
        })
        .sum();
    // Normalize -0.0 noise.
    let avg = if avg == 0.0 { 0.0 } else { avg };
    let total = if total == 0.0 { 0.0 } else { total };
    let upnl = if upnl == 0.0 { 0.0 } else { upnl };
    format!(
        "your stats (last 10 closed): win {wins}/{n} avg {avg:.2} total {total:.2} | open n={open_n} uPnL {upnl:.2}\n"
    )
}

/// TECHNICALS block for the entry prompt: 15m intraday series (last 10 points,
/// oldest→newest), a 4h context line, and OI/funding riding along from the snapshot row.
/// `input.candles` is already staleness-gated by the caller — None (or empty 15m bars)
/// prints `series: unavailable` and the decide proceeds anyway, the same never-block rule
/// as the `atr_pct` fetch in main.rs. Every 4h field degrades to `na` on its own when the
/// slower series is too short, so a partial cache never blanks the whole block.
fn technicals_block(input: &AnalystInput) -> String {
    let unavailable = || "TECHNICALS: series: unavailable\n".to_string();
    let Some(mc) = &input.candles else {
        return unavailable();
    };
    let m15: Vec<f64> = mc.m15.iter().map(|c| c.c).collect();
    if m15.is_empty() {
        return unavailable();
    }
    let series = |vals: &[f64], dp: usize| -> String {
        if vals.is_empty() {
            return "[na]".to_string();
        }
        let start = vals.len().saturating_sub(10);
        let parts: Vec<String> = vals[start..].iter().map(|v| format!("{v:.dp$}")).collect();
        format!("[{}]", parts.join(","))
    };
    let rsi_series = |vals: &[Option<f64>]| -> String {
        if vals.is_empty() {
            return "[na]".to_string();
        }
        let start = vals.len().saturating_sub(10);
        let parts: Vec<String> = vals[start..]
            .iter()
            .map(|v| {
                v.map(|x| format!("{x:.1}"))
                    .unwrap_or_else(|| "na".to_string())
            })
            .collect();
        format!("[{}]", parts.join(","))
    };
    let mut s = String::from("TECHNICALS 15m (oldest→newest):\n");
    s.push_str(&format!("close={}\n", series(&m15, 2)));
    s.push_str(&format!(
        "ema20={} macd_hist={} rsi7={} rsi14={}\n",
        series(&crate::features::ema(&m15, 20), 2),
        series(&crate::features::macd_hist(&m15), 4),
        rsi_series(&crate::features::rsi(&m15, 7)),
        rsi_series(&crate::features::rsi(&m15, 14)),
    ));
    // 4h context: trend (ema20 vs ema50), momentum (macd hist, rsi14), vol regime
    // (atr3 vs atr14), participation (latest volume vs its 10-bar average).
    let h4c: Vec<f64> = mc.h4.iter().map(|c| c.c).collect();
    let f = |v: Option<f64>, dp: usize| {
        v.map(|x| format!("{x:.dp$}"))
            .unwrap_or_else(|| "na".to_string())
    };
    let atr_s = |v: Option<f64>| {
        v.map(|x| format!("{x:.3}%"))
            .unwrap_or_else(|| "na".to_string())
    };
    let vol_now = mc.h4.last().map(|c| c.v);
    let vol_avg = {
        let start = mc.h4.len().saturating_sub(10);
        let n = mc.h4.len() - start;
        (n > 0).then(|| mc.h4[start..].iter().map(|c| c.v).sum::<f64>() / n as f64)
    };
    s.push_str(&format!(
        "4h: ema20={} vs ema50={} macd_hist={} rsi14={} atr3={} vs atr14={} vol={} vs avg={}\n",
        f(crate::features::ema(&h4c, 20).last().copied(), 2),
        f(crate::features::ema(&h4c, 50).last().copied(), 2),
        f(crate::features::macd_hist(&h4c).last().copied(), 4),
        f(crate::features::rsi(&h4c, 14).last().copied().flatten(), 1),
        atr_s(crate::features::atr_pct(&mc.h4, 3)),
        atr_s(crate::features::atr_pct(&mc.h4, 14)),
        f(vol_now, 0),
        f(vol_avg, 0),
    ));
    s.push_str(&format!(
        "oi={} funding={}\n",
        f(input.open_interest, 0),
        input
            .funding
            .map(|v| format!("{v:.6}"))
            .unwrap_or_else(|| "na".to_string()),
    ));
    s
}

fn build_prompt(input: &AnalystInput) -> String {
    let nominee = &input.nominee;
    let mut s = String::new();
    s.push_str(SYSTEM_PREAMBLE);
    s.push_str("\n\nNOMINEE:\n");
    let atr_token = match input.atr_pct {
        Some(v) => format!(" atr15m={:.3}%", v),
        None => String::new(),
    };
    s.push_str(&format!(
        "market={} side_hint={:?} score={:.3} features={{r5m:{:.3},r1h:{:.3},r24h:{:.3},vol1h:{:.3},funding_z:{:.3},range_pos:{:.3}}}{} market_hours={}\n",
        nominee.market, nominee.side_hint, nominee.score, nominee.features.r5m, nominee.features.r1h, nominee.features.r24h, nominee.features.vol1h, nominee.features.funding_z, nominee.features.range_pos, atr_token, input.market_hours
    ));
    // Same-market memory: the screener re-nominates a market it just stopped us out of, and
    // without this the model re-argued the identical thesis with no idea it had already lost
    // on it an hour ago.
    s.push_str(&recent_line(&nominee.market, &input.recent));
    s.push_str(&analyst_stats_line(input));
    // Account state: conviction should not be priced the same at -8% on the day as at flat.
    s.push_str(&format!(
        "account: equity=${:.2} day_pnl={:+.2}% kill_budget_used={:.0}%\n",
        input.account.equity, input.account.day_pnl_pct, input.account.kill_budget_used_pct
    ));
    // Open book: count + total entry notional, then one line per position with live uPnL.
    // fold from +0.0, not `sum()`: the f64 `Sum` impl seeds with -0.0, so an empty book would
    // print `notional=$-0.00`.
    let total_notional: f64 = input
        .open_positions
        .iter()
        .fold(0.0, |acc, p| acc + p.entry_px * p.size);
    s.push_str(&format!(
        "\nPORTFOLIO open_positions: n={} notional=${:.2}\n",
        input.open_positions.len(),
        total_notional
    ));
    if input.open_positions.is_empty() {
        s.push_str("none\n");
    } else {
        for p in input.open_positions.iter().take(OPEN_BOOK_MAX_LINES) {
            let upnl = input
                .open_marks
                .get(&p.market)
                .copied()
                .filter(|m| m.is_finite() && *m > 0.0)
                .map(|m| match p.side {
                    Side::Long => (m - p.entry_px) * p.size,
                    Side::Short => (p.entry_px - m) * p.size,
                });
            let upnl_s = match upnl {
                Some(v) => format!("{v:+.2}"),
                None => "n/a".to_string(),
            };
            s.push_str(&format!("{} {:?} uPnL={}\n", p.market, p.side, upnl_s));
        }
        if input.open_positions.len() > OPEN_BOOK_MAX_LINES {
            s.push_str(&format!(
                "(+{} more)\n",
                input.open_positions.len() - OPEN_BOOK_MAX_LINES
            ));
        }
    }
    s.push_str(&technicals_block(input));
    s.push_str("\nNEWS last 6h (up to 10):\n");
    if input.news.is_empty() {
        s.push_str("none\n");
    } else {
        for n in input.news.iter().take(10) {
            // char-safe truncation: byte-slicing panics on multi-byte UTF-8 (live-found panic)
            let body_snip: String = n.body.chars().take(200).collect();
            s.push_str(&format!("- {}: {} | {}\n", n.source, n.title, body_snip));
        }
    }
    s.push_str(FEE_HURDLE_LINE);
    s.push('\n');
    s.push_str("\nDecide: output ONLY JSON.\n");
    s
}

fn build_review_prompt(
    position: &Position,
    features: &Features,
    news: &[NewsItem],
    mark: f64,
    now_ms: i64,
) -> String {
    let ctx = position_context(position, mark, now_ms);
    let fmt = |v: Option<f64>, dp: usize| match v {
        Some(x) => format!("{x:.dp$}"),
        None => "n/a".to_string(),
    };
    let mut s = String::new();
    s.push_str(SYSTEM_PREAMBLE);
    s.push_str("\n\nPOSITION REVIEW:\n");
    s.push_str(&format!("market={} side={:?} entry={:.2} sl={:.2} tp={:.2} opened_ts={} features={{r5m:{:.3},r1h:{:.3},vol1h:{:.3},funding_z:{:.3},range_pos:{:.3}}}\n",
        position.market, position.side, position.entry_px, position.sl_px, position.tp_px, position.opened_ts, features.r5m, features.r1h, features.vol1h, features.funding_z, features.range_pos));
    // Live position accounting — without it the reviewer was judging an entry thesis with no
    // idea whether the trade was up, down, or already through most of its stop distance.
    s.push_str(&format!(
        "state: mark={} unrealized_pnl={} r_multiple={} held_h={:.2} fees_paid={:.4} (estimate: entry fee only, an exit costs ~the same again)\n",
        fmt(ctx.mark, 2),
        fmt(ctx.unrealized_pnl, 4),
        fmt(ctx.r_multiple, 2),
        ctx.held_h,
        ctx.fees_paid,
    ));
    s.push_str(FEE_HURDLE_LINE);
    s.push('\n');
    s.push_str("\nNEWS matched:\n");
    if news.is_empty() {
        s.push_str("none\n");
    } else {
        for n in news.iter().take(10) {
            s.push_str(&format!("- {}: {}\n", n.source, n.title));
        }
    }
    s.push_str("\nDecide hold/move-stop/veto_close: output ONLY JSON with action open/skip/veto_close etc. For hold use action skip, for veto_close use action veto_close.\n");
    s
}

/// Extract first balanced {...} from text.
fn extract_json(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if start.is_none() {
            if b == b'{' {
                start = Some(i);
                depth = 1;
                in_string = false;
                escape = false;
            }
            continue;
        }
        // inside json extraction
        if escape {
            escape = false;
            continue;
        }
        if b == b'\\' && in_string {
            escape = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                let s = start.unwrap();
                return Some(text[s..=i].to_string());
            }
        }
    }
    None
}

#[derive(Clone)]
pub struct Analyst {
    cfg: AnalystCfg,
    client: reqwest::Client,
    retriever: Arc<WebRetriever>,
    consecutive_total_failures: Arc<AtomicU64>,
    model: AnalystModelCfg,
    tool_executor: Option<ToolExecutor>,
}

impl Analyst {
    pub fn new(cfg: AnalystCfg) -> Self {
        // 60s/call timeout bounds wall time (user-locked); no client-side token cap — server default.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest build");
        Self::new_with_failure_counter(cfg, Arc::new(AtomicU64::new(0)))
    }

    pub fn new_with_failure_counter(
        cfg: AnalystCfg,
        consecutive_total_failures: Arc<AtomicU64>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest build");
        let model = cfg.roster().into_iter().next().unwrap_or(AnalystModelCfg { id: "".into(), api_style: "responses".into(), enabled: true, temperature: None, max_tokens: None });
        Self {
            cfg,
            client,
            retriever: Arc::new(WebRetriever::new()),
            consecutive_total_failures,
            model,
            tool_executor: None,
        }
    }

    pub fn failure_streak(&self) -> u64 {
        self.consecutive_total_failures.load(Ordering::SeqCst)
    }

    /// One independent arena seat. Failure state deliberately belongs to this client/model.
    pub fn for_model(cfg: AnalystCfg, model: AnalystModelCfg) -> Self {
        Self::for_model_with_retriever(cfg, model, Arc::new(WebRetriever::new()))
    }

    pub(crate) fn for_model_with_retriever(cfg: AnalystCfg, model: AnalystModelCfg, retriever: Arc<WebRetriever>) -> Self {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("reqwest build");
        Self { cfg, client, retriever, consecutive_total_failures: Arc::new(AtomicU64::new(0)), model, tool_executor: None }
    }

    pub fn model_id(&self) -> &str { &self.model.id }
    pub fn is_chat(&self) -> bool { self.model.api_style == "chat" }
    pub fn has_tools(&self) -> bool { self.tool_executor.is_some() }

    pub fn with_tool_executor(mut self, exec: ToolExecutor) -> Self {
        self.tool_executor = Some(exec);
        self
    }
    pub fn set_tool_executor(&mut self, exec: ToolExecutor) {
        self.tool_executor = Some(exec);
    }

    fn disabled_outcome(&self) -> AnalystOutcome {
        AnalystOutcome {
            decision: None,
            model_used: self.model.id.clone(),
            refused: false,
            latency_ms: 0,
            refusal_kind: None,
            refusal_excerpt: None,
            searched_queries: vec![],
            calls: vec![],
        }
    }

    /// Zen rejects valid keys without these identity headers.  Verified upstream 2026-08-22:
    /// base https://opencode.ai/zen/v1, Bearer auth plus UA/client/project and fresh msg_/ses_
    /// identifiers are required; otherwise Zen returns 403.
    fn upstream_post(&self, url: &str, key: &str) -> reqwest::RequestBuilder {
        let n = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut request = self.client.post(url).header("Authorization", format!("Bearer {key}"));
        for (name, value) in &self.cfg.headers { request = request.header(name, value); }
        request
            .header("x-opencode-request", format!("msg_{n:x}"))
            .header("x-opencode-session", format!("ses_{:x}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()))
    }

    /// For tests: custom timeout
    pub fn new_with_timeout(cfg: AnalystCfg, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest build");
        let model = cfg.roster().into_iter().next().unwrap_or(AnalystModelCfg { id: "".into(), api_style: "responses".into(), enabled: true, temperature: None, max_tokens: None });
        Self {
            cfg,
            client,
            retriever: Arc::new(WebRetriever::new()),
            consecutive_total_failures: Arc::new(AtomicU64::new(0)),
            model,
            tool_executor: None,
        }
    }

    #[cfg(test)]
    fn new_with_retrieval_urls(cfg: AnalystCfg, tavily_url: String, exa_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest build");
        let model = cfg.roster().into_iter().next().unwrap_or(AnalystModelCfg { id: "".into(), api_style: "responses".into(), enabled: true, temperature: None, max_tokens: None });
        Self {
            cfg,
            client,
            retriever: Arc::new(WebRetriever::with_urls(tavily_url, exa_url)),
            consecutive_total_failures: Arc::new(AtomicU64::new(0)),
            model,
            tool_executor: None,
        }
    }

    pub async fn decide(&self, input: AnalystInput) -> AnalystOutcome {
        if !self.cfg.enabled { return self.disabled_outcome(); }
        let mut prompt = build_prompt(&input);
        let queries = self
            .append_retrieved_context(&mut prompt, &input.nominee.market)
            .await;
        self.call_with_retry(&prompt, queries).await
    }

    /// `mark` is the live mid for the position's market; `0.0` (or any non-finite/negative
    /// value) means UNKNOWN and degrades the mark-dependent context fields to `n/a` — the same
    /// sentinel the review loop already passes to `apply_review_action`'s money guard.
    pub async fn review(
        &self,
        position: &Position,
        features: &Features,
        news: &[NewsItem],
        mark: f64,
    ) -> AnalystOutcome {
        if !self.cfg.enabled { return self.disabled_outcome(); }
        let mut prompt = build_review_prompt(
            position,
            features,
            news,
            mark,
            chrono::Utc::now().timestamp_millis(),
        );
        let queries = self
            .append_retrieved_context(&mut prompt, &position.market)
            .await;
        self.call_with_retry(&prompt, queries).await
    }

    /// Adaptive tool-use entry point (max 1 tool round = 2 LLM calls).
    /// Chat-completions path only: sends `tools` + `tool_choice:"auto"` on first call.
    /// If the model returns `tool_calls`, each is executed via `ToolExecutor` (≤2s, "unavailable" on failure)
    /// and a second non-tool call is made to produce the final Decision JSON.
    /// For `responses` api_style (muse-spark) we gracefully fall back to one-shot (no tools).
    pub async fn decide_with_tools(&self, input: AnalystInput) -> AnalystOutcome {
        if !self.cfg.enabled {
            return self.disabled_outcome();
        }
        // TODO: tool-use for responses api_style (muse-spark) not yet implemented — fallback to one-shot
        if self.model.api_style != "chat" {
            return self.decide(input).await;
        }
        let Some(exec) = self.tool_executor.clone() else {
            return self.decide(input).await;
        };
        let mut prompt = build_prompt(&input);
        let queries = self
            .append_retrieved_context(&mut prompt, &input.nominee.market)
            .await;
        let total_start = Instant::now();
        match self.call_chat_with_tools(&prompt).await {
            Err(refusal) => {
                let latency = total_start.elapsed().as_millis() as u64;
                let calls = vec![AnalystCall {
                    prompt: prompt.clone(),
                    response_raw: refusal.raw.clone(),
                    outcome_kind: refusal.kind.clone(),
                    parsed_json: None,
                    latency_ms: latency,
                }];
                self.consecutive_total_failures.fetch_add(1, Ordering::SeqCst);
                AnalystOutcome {
                    decision: None,
                    model_used: self.model.id.clone(),
                    refused: true,
                    latency_ms: latency,
                    refusal_kind: Some(refusal.kind),
                    refusal_excerpt: Some(refusal.excerpt),
                    searched_queries: queries,
                    calls,
                }
            }
            Ok((tool_calls_opt, content, raw_first)) => {
                let has_tools = tool_calls_opt.as_ref().is_some_and(|v| !v.is_empty());
                if !has_tools {
                    // No tool calls — treat as single-shot JSON (fallback path). Keep existing retry semantics
                    // by delegating to call_with_retry, but we already have the first content; try to parse it directly
                    // to avoid an extra call when the model cooperated.
                    if let Some(json) = extract_json(&content) {
                        if let Ok(decision) = serde_json::from_str::<Decision>(&json) {
                            let latency = total_start.elapsed().as_millis() as u64;
                            let calls = vec![AnalystCall {
                                prompt: prompt.clone(),
                                response_raw: raw_first.clone(),
                                outcome_kind: "ok".into(),
                                parsed_json: Some(json.clone()),
                                latency_ms: latency,
                            }];
                            self.consecutive_total_failures.store(0, Ordering::SeqCst);
                            return AnalystOutcome {
                                decision: Some(decision),
                                model_used: self.model.id.clone(),
                                refused: false,
                                latency_ms: latency,
                                refusal_kind: None,
                                refusal_excerpt: None,
                                searched_queries: queries,
                                calls,
                            };
                        }
                    }
                    // If first content had no valid JSON, fall back to the normal retry path (without tools)
                    // to keep behaviour identical to the non-tool fallback.
                    return self.call_with_retry(&prompt, queries).await;
                }
                // Tool calls present — execute each (≤2s, unavailable on failure) never blocks decide
                let tool_calls = tool_calls_opt.unwrap_or_default();
                let mut tool_results: Vec<(String, String)> = Vec::new();
                for tc in &tool_calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));
                    let result = exec.dispatch(&tc.function.name, &args).await;
                    tool_results.push((tc.id.clone(), result));
                }
                // Build second prompt: original prompt + tool results JSON, no tools
                let mut second_prompt = prompt.clone();
                second_prompt.push_str("\n\nTOOL RESULTS:\n");
                for (id, res) in &tool_results {
                    second_prompt.push_str(&format!("tool_call_id={id} result={res}\n"));
                }
                second_prompt.push_str("\nUsing the tool evidence above, now produce the final Decision JSON (no tool calls).\n");
                let second_start = Instant::now();
                // Second call is non-tool, single-shot (reuse chat primary path which handles json extraction)
                let second_res = self.call_chat_primary(&second_prompt).await;
                let total_latency = total_start.elapsed().as_millis() as u64;
                match second_res {
                    Ok(success) => {
                        // If second call also returned tool_calls (ignored), we still have a Decision via parsing
                        let parsed_json = serde_json::to_string(&success.decision).ok();
                        let first_call = AnalystCall {
                            prompt: prompt.clone(),
                            response_raw: raw_first.clone(),
                            outcome_kind: "tool_calls".into(),
                            parsed_json: None,
                            latency_ms: second_start.elapsed().as_millis() as u64,
                        };
                        let second_call = AnalystCall {
                            prompt: second_prompt.clone(),
                            response_raw: success.raw.clone(),
                            outcome_kind: "ok".into(),
                            parsed_json,
                            latency_ms: second_start.elapsed().as_millis() as u64,
                        };
                        self.consecutive_total_failures.store(0, Ordering::SeqCst);
                        AnalystOutcome {
                            decision: Some(success.decision),
                            model_used: self.model.id.clone(),
                            refused: false,
                            latency_ms: total_latency,
                            refusal_kind: None,
                            refusal_excerpt: None,
                            searched_queries: queries,
                            calls: vec![first_call, second_call],
                        }
                    }
                    Err(refusal) => {
                        // Second call failed (no_json, etc) — surface as refusal but preserve first tool round for forensics
                        let first_call = AnalystCall {
                            prompt: prompt.clone(),
                            response_raw: raw_first.clone(),
                            outcome_kind: "tool_calls".into(),
                            parsed_json: None,
                            latency_ms: 0,
                        };
                        let second_call = AnalystCall {
                            prompt: second_prompt.clone(),
                            response_raw: refusal.raw.clone(),
                            outcome_kind: refusal.kind.clone(),
                            parsed_json: None,
                            latency_ms: second_start.elapsed().as_millis() as u64,
                        };
                        // Also surface the tool results as part of second call's raw? Not needed
                        self.consecutive_total_failures.fetch_add(1, Ordering::SeqCst);
                        AnalystOutcome {
                            decision: None,
                            model_used: self.model.id.clone(),
                            refused: true,
                            latency_ms: total_latency,
                            refusal_kind: Some(refusal.kind.clone()),
                            refusal_excerpt: Some(refusal.excerpt.clone()),
                            searched_queries: queries,
                            calls: vec![first_call, second_call],
                        }
                    }
                }
            }
        }
    }

    async fn call_chat_with_tools(&self, prompt: &str) -> Result<(Option<Vec<ToolCall>>, String, String), Refusal> {
        let key = std::env::var(&self.cfg.api_key_env).map_err(|_| Refusal { kind: "http_err".into(), excerpt: "http_err".into(), raw: String::new() })?;
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let mut body = serde_json::json!({
            "model": self.model.id,
            "messages": [{"role":"user","content":prompt}],
            "tools": tool_definitions(),
            "tool_choice": "auto"
        });
        if let Some(t) = self.model.effective_temperature() {
            let t = t.clamp(0.0, 2.0);
            if let Some(obj) = body.as_object_mut() { obj.insert("temperature".to_string(), serde_json::json!(t)); }
        }
        if let Some(n) = self.model.effective_max_tokens(self.cfg.max_completion_tokens) {
            if let Some(obj) = body.as_object_mut() { obj.insert("max_tokens".to_string(), serde_json::json!(n)); }
        }
        let resp = self.upstream_post(&url, &key).json(&body).send().await.map_err(|e| {
            debug!(model=%self.model.id, error=%e, "tool chat send failed");
            Refusal {
                kind: if e.is_timeout() { "timeout".into() } else { "http_err".into() },
                excerpt: "http_err".into(),
                raw: String::new(),
            }
        })?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_excerpt = resp.text().await.unwrap_or_default();
            let sanitized = sanitize_excerpt(&body_excerpt);
            warn!(model=%self.model.id, status, excerpt=%sanitized, "tool chat http_err");
            return Err(Refusal { kind: "http_err".into(), excerpt: "http_err".into(), raw: body_excerpt });
        }
        let raw = resp.text().await.map_err(|_| Refusal { kind: "http_err".into(), excerpt: "http_err".into(), raw: String::new() })?;
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|_| Refusal { kind: "http_err".into(), excerpt: "http_err".into(), raw: raw.clone() })?;
        // Extract tool_calls if any
        let tool_calls_val = v.pointer("/choices/0/message/tool_calls");
        let tool_calls: Option<Vec<ToolCall>> = if let Some(arr) = tool_calls_val.and_then(|v| v.as_array()) {
            if arr.is_empty() {
                None
            } else {
                let mut out = Vec::new();
                for item in arr {
                    if let Ok(tc) = serde_json::from_value::<ToolCall>(item.clone()) {
                        out.push(tc);
                    }
                }
                if out.is_empty() { None } else { Some(out) }
            }
        } else {
            None
        };
        let content = v.pointer("/choices/0/message/content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        // Also handle case where content is null but tool_calls present — that's valid
        if tool_calls.is_none() && content.trim().is_empty() {
            return Err(Refusal { kind: "empty_output".into(), excerpt: String::new(), raw: raw.clone() });
        }
        Ok((tool_calls, content, raw))
    }

    pub async fn chat(&self, prompt: &str) -> (String, Vec<AnalystCall>) {
        if !self.cfg.enabled { return (String::new(), vec![]); }
        let key = match std::env::var(&self.cfg.api_key_env) {
            Ok(key) => key,
            Err(_) => {
                return (
                    String::new(),
                    vec![AnalystCall {
                        prompt: prompt.into(),
                        response_raw: String::new(),
                        outcome_kind: "http_err".into(),
                        parsed_json: None,
                        latency_ms: 0,
                    }],
                );
            }
        };
        let chat_style = self.model.api_style == "chat";
        let url = format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), if chat_style { "chat/completions" } else { "responses" });
        let mut calls = Vec::new();
        for _ in 0..3 {
            let started = Instant::now();
            let body = if chat_style { serde_json::json!({"model": self.model.id, "messages":[{"role":"user","content":prompt}]}) } else { serde_json::json!({"model": self.model.id, "input": [{"role":"user","content":[{"type":"input_text","text":prompt}]}], "stream":false}) };
            let response = self.upstream_post(&url, &key).json(&body).send().await;
            let (raw, kind) = match response {
                Ok(r) if r.status().is_success() => match r.text().await {
                    Ok(text) => (text, "ok"),
                    Err(e) => (
                        String::new(),
                        if e.is_timeout() {
                            "timeout"
                        } else {
                            "http_err"
                        },
                    ),
                },
                Ok(_) => (String::new(), "http_err"),
                Err(e) => (
                    String::new(),
                    if e.is_timeout() {
                        "timeout"
                    } else {
                        "http_err"
                    },
                ),
            };
            let reply = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .map(|v| if chat_style { v.pointer("/choices/0/message/content").and_then(|x| x.as_str()).unwrap_or_default().to_string() } else { output_text(&v) })
                .unwrap_or_default();
            let outcome_kind = if kind == "ok" && reply.trim().is_empty() {
                "empty_output"
            } else {
                kind
            };
            calls.push(AnalystCall {
                prompt: prompt.into(),
                response_raw: raw,
                outcome_kind: outcome_kind.into(),
                parsed_json: None,
                latency_ms: started.elapsed().as_millis() as u64,
            });
            if outcome_kind == "ok" {
                return (reply, calls);
            }
        }
        (String::new(), calls)
    }

    /// Relay an OpenAI-compatible Responses SSE stream into the chat endpoint. The response is
    /// still accumulated by the caller for the post-stream gated action pass and audit row.
    pub async fn chat_stream(
        &self,
        prompt: &str,
        deltas: tokio::sync::mpsc::Sender<String>,
    ) -> Result<AnalystCall, String> {
        if !self.cfg.enabled { return Err("analysts disabled".into()); }
        let started = Instant::now();
        let key = std::env::var(&self.cfg.api_key_env).map_err(|_| "analyst key unavailable".to_string())?;
        let chat_style = self.model.api_style == "chat";
        let url = format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), if chat_style { "chat/completions" } else { "responses" });
        let body = if chat_style { serde_json::json!({"model": self.model.id, "messages":[{"role":"user","content":prompt}], "stream":true}) } else { serde_json::json!({"model": self.model.id, "input": [{"role":"user","content":[{"type":"input_text","text":prompt}]}], "stream":true}) };
        let response = self.upstream_post(&url, &key).json(&body).send().await.map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("upstream HTTP {}", response.status()));
        }
        let mut raw = String::new();
        let mut pending = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            let text = std::str::from_utf8(&chunk).map_err(|error| error.to_string())?;
            raw.push_str(text);
            pending.push_str(text);
            while let Some(end) = pending.find("\n\n") {
                let frame = pending[..end].to_string();
                pending.drain(..end + 2);
                for delta in if chat_style { chat_content_deltas(&frame) } else { response_output_text_deltas(&frame) } {
                    deltas.send(delta).await.map_err(|_| "client stream closed".to_string())?;
                }
            }
        }
        Ok(AnalystCall {
            prompt: prompt.into(), response_raw: raw, outcome_kind: "ok".into(), parsed_json: None,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn append_retrieved_context(&self, prompt: &mut String, market: &str) -> Vec<String> {
        if !self.cfg.web_retrieval {
            return Vec::new();
        }
        let Some(context) = self
            .retriever
            .retrieve(&self.client, &self.cfg, market)
            .await
        else {
            return Vec::new();
        };
        prompt.push_str("\n\n");
        prompt.push_str(&context.block);
        context.queries
    }

    /// Muse-only policy (user-locked): no fallback model exists. Up to RETRIES+1 attempts on the
    /// primary endpoint; refusal/invalid/error all retry with a reframe suffix; final failure -> skip.
    /// Forensics: on final failure we record refusal_kind + bounded excerpt (never on success).
    /// Decisions log reason format extension: `... refundable:<kind>:<excerpt>` plus optional ` searched:"q1","q2"` — documented here and in main.rs
    async fn call_with_retry(&self, prompt: &str, searched_queries: Vec<String>) -> AnalystOutcome {
        const RETRIES: usize = 2;
        let start = Instant::now();
        let mut attempted = 0;
        let mut saw_refusal = false;
        let mut last_refusal: Option<Refusal> = None;
        let mut calls = Vec::with_capacity(RETRIES + 1);
        for attempt in 0..=RETRIES {
            attempted = attempt + 1;
            let p = if attempt == 0 {
                prompt.to_string()
            } else {
                format!(
                    "{prompt}\n\nIMPORTANT: Your previous response was invalid. Respond with ONLY the JSON object, no prose, no disclaimer."
                )
            };
            let call_start = Instant::now();
            let res = self.call_primary(&p).await;
            match res {
                Ok(success) => {
                    let parsed_json = serde_json::to_string(&success.decision).ok();
                    calls.push(AnalystCall {
                        prompt: p,
                        response_raw: success.raw,
                        outcome_kind: "ok".into(),
                        parsed_json,
                        latency_ms: call_start.elapsed().as_millis() as u64,
                    });
                    self.consecutive_total_failures.store(0, Ordering::SeqCst);
                    return AnalystOutcome {
                        decision: Some(success.decision),
                        model_used: self.model.id.clone(),
                        refused: saw_refusal,
                        latency_ms: start.elapsed().as_millis() as u64,
                        refusal_kind: None,
                        refusal_excerpt: None,
                        searched_queries,
                        calls,
                    };
                }
                Err(r) => {
                    calls.push(AnalystCall {
                        prompt: p,
                        response_raw: r.raw.clone(),
                        outcome_kind: r.kind.clone(),
                        parsed_json: None,
                        latency_ms: call_start.elapsed().as_millis() as u64,
                    });
                    saw_refusal = true;
                    debug!(attempt, kind=%r.kind, "analyst refusal/invalid output");
                    last_refusal = Some(r);
                }
            }
        }
        warn!(attempts = attempted, kind=?last_refusal.as_ref().map(|r| r.kind.clone()), "analyst: all attempts failed, skipping");
        self.consecutive_total_failures
            .fetch_add(1, Ordering::SeqCst);
        let (kind, excerpt) = match last_refusal {
            Some(r) => (Some(r.kind), Some(r.excerpt)),
            None => (Some("empty_output".to_string()), Some(String::new())),
        };
        AnalystOutcome {
            decision: None,
            model_used: self.model.id.clone(),
            refused: true,
            latency_ms: start.elapsed().as_millis() as u64,
            refusal_kind: kind,
            refusal_excerpt: excerpt,
            searched_queries,
            calls,
        }
    }

    async fn call_primary(&self, prompt: &str) -> Result<PrimarySuccess, Refusal> {
        if self.model.api_style == "chat" { return self.call_chat_primary(prompt).await; }
        let key = match std::env::var(&self.cfg.api_key_env) {
            Ok(k) => k,
            Err(_) => {
                return Err(Refusal {
                    kind: "http_err".to_string(),
                    excerpt: "http_err".to_string(),
                    raw: String::new(),
                });
            }
        };
        let url = format!("{}/responses", self.cfg.base_url.trim_end_matches('/'));
        // text.format=json_object verified live 2026-08-09 (status completed); json_schema NOT supported here.
        // Verified live 2026-08-09 (api.meta.ai/v1/responses): omitting max_output_tokens → status completed,
        // response max_output_tokens:null (server default), work unaffected.
        // 60s/call timeout bounds wall time; money is explicitly NOT the constraint (user-locked spend).
        // Per-model overrides: hy3/mimo 800 to avoid no_json, lightning/laguna 400 to avoid 60s timeouts.
        let mut body = serde_json::json!({
            "model": self.model.id,
            "input": [{"role":"user","content":[{"type":"input_text","text": prompt}]}],
            "stream": false,
            "text": {"format": {"type": "json_object"}}
        });
        if let Some(n) = self.model.effective_max_tokens(self.cfg.max_completion_tokens) {
            if let Some(obj) = body.as_object_mut() {
                obj.insert("max_output_tokens".to_string(), serde_json::json!(n));
            }
        }
        // Per-model temperature: DeepSeek 1.0 (variety), Mimo 0.7 structure-first, Nemotron 0.8 balanced, fallback 0.7.
        // Responses API temperature is optional but supported by Zen gateway (tolerates extra key).
        if let Some(t) = self.model.effective_temperature() {
            let t = t.clamp(0.0, 2.0);
            if let Some(obj) = body.as_object_mut() {
                obj.insert("temperature".to_string(), serde_json::json!(t));
            }
        }
        let resp = match self
            .upstream_post(&url, &key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let kind = if e.is_timeout() {
                    "timeout"
                } else {
                    "http_err"
                };
                // DeepSeek 100% http_err forensics: surface timeout vs http_err distinctly.
                debug!(model=%self.model.id, kind=%kind, error=%e, "analyst upstream send failed");
                return Err(Refusal {
                    kind: kind.to_string(),
                    excerpt: kind.to_string(),
                    raw: String::new(),
                });
            }
        };
        if !resp.status().is_success() {
            // Capture body for forensics: Zen 403/404 on bad model IDs (deepseek 0% ok) surfaces here.
            let status = resp.status().as_u16();
            let body_excerpt = resp.text().await.unwrap_or_default();
            let sanitized = sanitize_excerpt(&body_excerpt);
            warn!(model=%self.model.id, status, excerpt=%sanitized, "analyst http_err (non-2xx)");
            return Err(Refusal {
                kind: "http_err".to_string(),
                excerpt: "http_err".to_string(),
                raw: body_excerpt,
            });
        }
        let text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                let kind = if e.is_timeout() {
                    "timeout"
                } else {
                    "http_err"
                };
                return Err(Refusal {
                    kind: kind.to_string(),
                    excerpt: kind.to_string(),
                    raw: String::new(),
                });
            }
        };
        // parse response json
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(val) => val,
            Err(_) => {
                return Err(Refusal {
                    kind: "http_err".to_string(),
                    excerpt: "http_err".to_string(),
                    raw: text,
                });
            }
        };
        // incomplete_details has priority: max_output_tokens starved
        if v.get("incomplete_details")
            .and_then(|d| d.get("reason"))
            .and_then(|r| r.as_str())
            == Some("max_output_tokens")
        {
            // try to collect excerpt if any
            let mut joined_tmp = String::new();
            if let Some(arr) = v.get("output").and_then(|o| o.as_array()) {
                for item in arr {
                    if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                        if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                            for c in content {
                                if c.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                                    if let Some(t) = c.get("text").and_then(|x| x.as_str()) {
                                        joined_tmp.push_str(t);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            return Err(Refusal {
                kind: "incomplete_max_tokens".to_string(),
                excerpt: sanitize_excerpt(&joined_tmp),
                raw: text,
            });
        }
        // check output array
        let output = v.get("output").and_then(|o| o.as_array());
        if output.is_none() {
            return Err(Refusal {
                kind: "empty_output".to_string(),
                excerpt: String::new(),
                raw: text,
            });
        }
        let mut joined = String::new();
        for item in output.unwrap() {
            if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    for c in content {
                        if c.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                            if let Some(t) = c.get("text").and_then(|x| x.as_str()) {
                                joined.push_str(t);
                                joined.push('\n');
                            }
                        }
                    }
                }
            }
        }
        if joined.trim().is_empty() {
            return Err(Refusal {
                kind: "empty_output".to_string(),
                excerpt: sanitize_excerpt(&joined),
                raw: text,
            });
        }
        let json_str = match extract_json(&joined) {
            Some(s) => s,
            None => {
                return Err(Refusal {
                    kind: "no_json".to_string(),
                    excerpt: sanitize_excerpt(&joined),
                    raw: text,
                });
            }
        };
        let decision: Decision = match serde_json::from_str(&json_str) {
            Ok(d) => d,
            Err(_) => {
                return Err(Refusal {
                    kind: "no_json".to_string(),
                    excerpt: sanitize_excerpt(&joined),
                    raw: text,
                });
            }
        };
        // Counter-case encouragement: thesis should state why it could fail (bear/bear case / fail / invalid).
        // Not a rejection — just log so the next prompt iteration can nudge. Keep cheap.
        {
            let lc = decision.thesis.to_ascii_lowercase();
            if !(lc.contains("fail") || lc.contains("invalid") || lc.contains("bear") || lc.contains("wrong") || lc.contains("invalidation")) {
                debug!(model=%self.model.id, thesis=%decision.thesis, "thesis missing counter-case keyword");
            }
        }
        Ok(PrimarySuccess {
            decision,
            raw: text,
        })
    }

    async fn call_chat_primary(&self, prompt: &str) -> Result<PrimarySuccess, Refusal> {
        let key = std::env::var(&self.cfg.api_key_env).map_err(|_| Refusal { kind: "http_err".into(), excerpt: "http_err".into(), raw: String::new() })?;
        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        // Per-model temperature + token budget (hy3/mimo 800, lightning/laguna 400). Temperature hints:
        // DeepSeek 1.0, Mimo 0.7 structure-first, Nemotron 0.8 balanced — set per model when present.
        let mut body = serde_json::json!({
            "model": self.model.id, "messages": [{"role":"user", "content":prompt}],
            "response_format":{"type":"json_object"}
        });
        if let Some(t) = self.model.effective_temperature() {
            let t = t.clamp(0.0, 2.0);
            if let Some(obj) = body.as_object_mut() { obj.insert("temperature".to_string(), serde_json::json!(t)); }
        }
        if let Some(n) = self.model.effective_max_tokens(self.cfg.max_completion_tokens) {
            if let Some(obj) = body.as_object_mut() { obj.insert("max_tokens".to_string(), serde_json::json!(n)); }
        }
        // DeepSeek guide: JSON mode needs literal "json" + example schema + max_tokens high; we now set both.
        // Also include reasoning normalization for deepseek flash if supported (optional).
        let response = self.upstream_post(&url, &key).json(&body).send().await.map_err(|e| {
            debug!(model=%self.model.id, error=%e, "chat send failed");
            Refusal { kind: if e.is_timeout() { "timeout".into() } else { "http_err".into() }, excerpt: "http_err".into(), raw: String::new() }
        })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body_excerpt = response.text().await.unwrap_or_default();
            let sanitized = sanitize_excerpt(&body_excerpt);
            warn!(model=%self.model.id, status, excerpt=%sanitized, "chat http_err (non-2xx)");
            return Err(Refusal { kind:"http_err".into(), excerpt:"http_err".into(), raw: body_excerpt });
        }
        let raw = response.text().await.map_err(|_| Refusal { kind:"http_err".into(), excerpt:"http_err".into(), raw:String::new() })?;
        let content = serde_json::from_str::<serde_json::Value>(&raw).ok()
            .and_then(|v| v.pointer("/choices/0/message/content").and_then(|v| v.as_str()).map(str::to_string))
            .ok_or_else(|| Refusal { kind:"empty_output".into(), excerpt:String::new(), raw:raw.clone() })?;
        // hy3 52% no_json forensics: if no JSON found, surface excerpt; retry (call_with_retry) will re-nudge with "Return ONLY json".
        let json = extract_json(&content).ok_or_else(|| Refusal { kind:"no_json".into(), excerpt:sanitize_excerpt(&content), raw:raw.clone() })?;
        let decision: Decision = serde_json::from_str(&json).map_err(|_| Refusal { kind:"no_json".into(), excerpt:sanitize_excerpt(&content), raw:raw.clone() })?;
        {
            let lc = decision.thesis.to_ascii_lowercase();
            if !(lc.contains("fail") || lc.contains("invalid") || lc.contains("bear") || lc.contains("wrong") || lc.contains("invalidation")) {
                debug!(model=%self.model.id, thesis=%decision.thesis, "thesis missing counter-case keyword");
            }
        }
        Ok(PrimarySuccess { decision, raw })
    }
}

fn chat_content_deltas(frame: &str) -> Vec<String> {
    frame.lines().filter_map(|line| line.strip_prefix("data: ")).filter(|data| *data != "[DONE]")
        .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .filter_map(|value| value.pointer("/choices/0/delta/content").and_then(|v| v.as_str()).map(str::to_string)).collect()
}

fn output_text(v: &serde_json::Value) -> String {
    v.get("output")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
        .flat_map(|item| {
            item.get("content")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|content| content.get("text").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn response_output_text_deltas(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            matches!(
                value["type"].as_str(),
                Some("response.output_text_delta" | "response.output_text.delta")
            )
        })
        .filter_map(|value| value["delta"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AnalystCfg, AnalystModelCfg};
    use crate::contracts::{Features, Nominee, Side};
    use axum::{
        Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };
    use std::net::SocketAddr;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;

    #[derive(Clone)]
    struct RetrievalMock {
        responses: Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
        tavily_hits: Arc<AtomicUsize>,
        exa_hits: Arc<AtomicUsize>,
        tavily_ok: bool,
        exa_ok: bool,
    }

    async fn mock_response(
        State(mock): State<RetrievalMock>,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        mock.responses.lock().await.push(body);
        Json(serde_json::json!({
            "status":"completed",
            "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}]
        }))
    }

    async fn mock_tavily(State(mock): State<RetrievalMock>) -> impl IntoResponse {
        mock.tavily_hits.fetch_add(1, Ordering::SeqCst);
        if mock.tavily_ok {
            Json(serde_json::json!({"results":[{"title":"Tavily SOL headline","url":"https://tavily.test/sol","published_date":"2026-08-21","content":"Tavily market summary"}]})).into_response()
        } else {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }

    async fn mock_exa(State(mock): State<RetrievalMock>) -> impl IntoResponse {
        mock.exa_hits.fetch_add(1, Ordering::SeqCst);
        if mock.exa_ok {
            Json(serde_json::json!({"results":[{"title":"Exa SOL headline","url":"https://exa.test/sol","publishedDate":"2026-08-21","text":"Exa market summary"}]})).into_response()
        } else {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }

    async fn retrieval_mock(tavily_ok: bool, exa_ok: bool) -> (SocketAddr, RetrievalMock) {
        let mock = RetrievalMock {
            responses: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            tavily_hits: Arc::new(AtomicUsize::new(0)),
            exa_hits: Arc::new(AtomicUsize::new(0)),
            tavily_ok,
            exa_ok,
        };
        let app = Router::new()
            .route("/responses", post(mock_response))
            .route("/tavily", post(mock_tavily))
            .route("/exa", post(mock_exa))
            .with_state(mock.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (addr, mock)
    }

    fn retrieval_analyst(addr: SocketAddr) -> Analyst {
        let mut cfg = test_cfg(format!("http://{addr}"));
        cfg.web_retrieval = true;
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
            std::env::set_var("TAVILY_TEST_KEY", "test");
            std::env::set_var("EXA_TEST_KEY", "test");
        }
        Analyst::new_with_retrieval_urls(
            cfg,
            format!("http://{addr}/tavily"),
            format!("http://{addr}/exa"),
        )
    }

    fn test_cfg(base: String) -> AnalystCfg {
        AnalystCfg {
            enabled: true,
            base_url: base,
            model: "muse-spark-1.2-contributor".into(),
            models: vec![],
            chat_model: None,
            headers: HashMap::new(),
            api_key_env: "ANALYST_API_KEY".into(),
            max_completion_tokens: None, // user-locked: no client-side cap → server default (None omits key)
            web_retrieval: false,
            tavily_key_env: "TAVILY_TEST_KEY".into(),
            exa_key_env: "EXA_TEST_KEY".into(),
        }
    }

    fn test_cfg_with_tokens(base: String) -> AnalystCfg {
        AnalystCfg {
            enabled: true,
            base_url: base,
            model: "muse-spark-1.2-contributor".into(),
            models: vec![],
            chat_model: None,
            headers: HashMap::new(),
            api_key_env: "ANALYST_API_KEY".into(),
            max_completion_tokens: Some(16000),
            web_retrieval: false,
            tavily_key_env: "TAVILY_TEST_KEY".into(),
            exa_key_env: "EXA_TEST_KEY".into(),
        }
    }

    #[tokio::test]
    async fn disabled_decide_and_review_skip_retrieval_and_upstream_calls() {
        let hits = Arc::new(AtomicUsize::new(0));
        let upstream_hits = hits.clone();
        let app = Router::new().route("/responses", post(move || {
            let hits = upstream_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"output":[]}))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("address");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve"); });
        let mut cfg = test_cfg(format!("http://{addr}"));
        cfg.enabled = false;
        let analyst = Analyst::new(cfg);

        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert!(outcome.calls.is_empty());
        assert!(outcome.searched_queries.is_empty());
        let (position, features) = review_fixture();
        let review = analyst.review(&position, &features, &[], 100.0).await;
        assert!(review.decision.is_none());
        assert!(review.calls.is_empty());
        assert_eq!(hits.load(Ordering::SeqCst), 0, "disabled analyst must not hit upstream");
    }

    fn dummy_input() -> AnalystInput {
        AnalystInput {
            nominee: Nominee {
                ts: 1_700_000_000_000,
                market: "SOL".into(),
                side_hint: Side::Long,
                score: 2.5,
                features: Features {
                    r5m: 0.5,
                    r1h: 1.0,
                    r24h: 1.5,
                    vol1h: 0.4,
                    funding_z: 0.2,
                    range_pos: 0.9,
                },
            },
            news: vec![],
            open_positions: vec![],
            market_hours: "open",
            atr_pct: None,
            recent: vec![],
            account: AccountState::default(),
            open_marks: std::collections::HashMap::new(),
            candles: None,
            open_interest: None,
            funding: None,
            analyst_id: String::new(),
            analyst_recent: vec![],
        }
    }

    fn pos(market: &str, side: Side, entry: f64, size: f64) -> Position {
        Position {
            id: 1,
            market: market.into(),
            side,
            entry_px: entry,
            size,
            leverage: 5.0,
            margin: entry * size / 5.0,
            sl_px: entry * 0.99,
            tp_px: entry * 1.02,
            opened_ts: 0,
            analyst: String::new(),
            horizon_hours: Some(24.0),
        }
    }

    #[tokio::test]
    async fn prose_prefixed_json_parses() {
        // Mock server for primary that returns prose + json
        let app = Router::new().route("/responses", post(|Json(_body): Json<serde_json::Value>| async {
            Json(serde_json::json!({
                "status": "completed",
                "output": [
                    {"type":"reasoning","content":[]},
                    {"type":"message","content":[{"type":"output_text","text":"As an AI, I note this is a paper simulation for research only.\n\n{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"low edge\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}
                ]
            }))
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{addr}");
        let cfg = test_cfg(base.clone());
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_some(), "decision should parse");
        let d = outcome.decision.unwrap();
        assert_eq!(d.action, "skip");
        assert_eq!(d.conviction, 0.4);
        assert!(!outcome.refused);
        assert!(
            outcome.refusal_kind.is_none(),
            "success path never emits a refusal-kind"
        );
        assert!(outcome.refusal_excerpt.is_none());
    }

    #[tokio::test]
    async fn refusal_retry_succeeds_muse() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new()
            .route("/responses", post(move |Json(_b): Json<serde_json::Value>| {
                let cc = c.clone();
                async move {
                    let n = cc.fetch_add(1, Ordering::SeqCst);
                    if n == 0 {
                        Json(serde_json::json!({
                            "status":"completed",
                            "output":[{"type":"message","content":[{"type":"output_text","text":"I cannot provide financial advice."}]}]
                        }))
                    } else {
                        Json(serde_json::json!({
                            "status":"completed",
                            "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"open\",\"side\":\"long\",\"conviction\":0.85,\"thesis\":\"retry ok\",\"horizon_hours\":24,\"stop_pct\":1.2,\"tp_pct\":2.4}"}]}]
                        }))
                    }
                }
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert_eq!(outcome.decision.unwrap().action, "open");
        assert_eq!(outcome.model_used, "muse-spark-1.2-contributor");
        assert!(outcome.refused); // first attempt refused, retry recovered
        // success path never emits a refusal-kind even though retry occurred
        assert!(
            outcome.refusal_kind.is_none(),
            "success after retry must not emit kind"
        );
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn refusal_all_attempts_yields_none() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new()
            .route("/responses", post(move |Json(_b): Json<serde_json::Value>| {
                let cc = c.clone();
                async move {
                    cc.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({
                        "status":"completed",
                        "output":[{"type":"message","content":[{"type":"output_text","text":"No JSON anywhere, ever."}]}]
                    }))
                }
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(
            outcome.decision.is_none(),
            "persistent refusal must skip (muse-only, no fallback)"
        );
        assert_eq!(outcome.model_used, "muse-spark-1.2-contributor");
        assert!(outcome.refused);
        assert_eq!(
            outcome.refusal_kind.as_deref(),
            Some("no_json"),
            "prose-only now kind=no_json"
        );
        assert!(outcome.refusal_excerpt.is_some());
        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "1 primary + 2 retries, then stop"
        );
        assert_eq!(analyst.failure_streak(), 1);
    }

    #[tokio::test]
    async fn failure_streak_resets_after_a_successful_model_decision() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route("/responses", post(move |Json(_b): Json<serde_json::Value>| {
            let c = c.clone();
            async move {
                if c.fetch_add(1, Ordering::SeqCst) < 3 {
                    Json(serde_json::json!({"status":"completed","output":[]}))
                } else {
                    Json(serde_json::json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}]}))
                }
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(test_cfg(format!("http://{addr}")));
        assert!(analyst.decide(dummy_input()).await.decision.is_none());
        assert_eq!(analyst.failure_streak(), 1);
        assert!(analyst.decide(dummy_input()).await.decision.is_some());
        assert_eq!(analyst.failure_streak(), 0);
    }

    #[tokio::test]
    async fn empty_output_all_attempts_yields_none() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route(
            "/responses",
            post(move |Json(_b): Json<serde_json::Value>| {
                let cc = c.clone();
                async move {
                    cc.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({"status":"completed","output":[]}))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert!(outcome.refused);
        assert_eq!(outcome.refusal_kind.as_deref(), Some("empty_output"));
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn timeout_all_attempts_yields_none() {
        let app = Router::new().route(
            "/responses",
            post(|Json(_b): Json<serde_json::Value>| async {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                Json(serde_json::json!({"status":"completed","output":[]}))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new_with_timeout(cfg, Duration::from_millis(100));
        let outcome = analyst.decide(dummy_input()).await;
        assert!(
            outcome.decision.is_none(),
            "timeouts on all attempts must skip (no fallback exists)"
        );
        assert!(outcome.refused);
        assert_eq!(outcome.refusal_kind.as_deref(), Some("timeout"));
        assert_eq!(outcome.refusal_excerpt.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn http_err_kind() {
        let app = Router::new().route(
            "/responses",
            post(|Json(_b): Json<serde_json::Value>| async {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":"boom"})),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert_eq!(outcome.refusal_kind.as_deref(), Some("http_err"));
        assert_eq!(outcome.refusal_excerpt.as_deref(), Some("http_err"));
    }

    #[tokio::test]
    async fn incomplete_max_tokens_kind() {
        let app = Router::new().route("/responses", post(|Json(_b): Json<serde_json::Value>| async {
            Json(serde_json::json!({
                "status":"incomplete",
                "incomplete_details":{"reason":"max_output_tokens"},
                "output":[{"type":"message","content":[{"type":"output_text","text":"partial ..."}]}]
            }))
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert_eq!(
            outcome.refusal_kind.as_deref(),
            Some("incomplete_max_tokens")
        );
    }

    #[tokio::test]
    async fn no_json_kind() {
        let app = Router::new().route("/responses", post(|Json(_b): Json<serde_json::Value>| async {
            Json(serde_json::json!({
                "status":"completed",
                "output":[{"type":"message","content":[{"type":"output_text","text":"Just prose no json here"}]}]
            }))
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert_eq!(outcome.refusal_kind.as_deref(), Some("no_json"));
        assert!(outcome.refusal_excerpt.unwrap().contains("Just prose"));
    }

    #[tokio::test]
    async fn empty_output_no_message_content() {
        let app = Router::new().route(
            "/responses",
            post(|Json(_b): Json<serde_json::Value>| async {
                Json(serde_json::json!({
                    "status":"completed",
                    "output":[{"type":"reasoning","content":[]}]
                }))
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(outcome.decision.is_none());
        assert_eq!(outcome.refusal_kind.as_deref(), Some("empty_output"));
    }

    #[tokio::test]
    async fn request_shape_pins_budget_and_responses_api() {
        // User-locked: money NOT the constraint — default cfg omits max_output_tokens entirely.
        // Verified live 2026-08-09 (api.meta.ai/v1/responses): omitting key → status completed,
        // response max_output_tokens:null (server default), work unaffected, json_object honored.
        // 60s/call timeout remains the wall-clock bound.
        let captured = Arc::new(tokio::sync::Mutex::new(None::<serde_json::Value>));
        let cap = captured.clone();
        let app = Router::new()
            .route("/responses", post(move |Json(body): Json<serde_json::Value>| {
                let cc = cap.clone();
                async move {
                    *cc.lock().await = Some(body);
                    Json(serde_json::json!({
                        "status":"completed",
                        "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}]
                    }))
                }
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let _ = analyst.decide(dummy_input()).await;
        let body = captured.lock().await.clone().expect("request captured");
        assert_eq!(body["model"], "muse-spark-1.2-contributor");
        assert!(
            body.get("max_output_tokens").is_none(),
            "when None, key must be ABSENT (server default), got {:?}",
            body.get("max_output_tokens")
        );
        assert_eq!(body["stream"], false);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["text"]["format"]["type"], "json_object");
        assert!(
            body.get("tools").is_none(),
            "hosted tools must never be sent"
        );
    }

    #[tokio::test]
    async fn request_shape_includes_max_output_tokens_when_some() {
        // When cfg.max_completion_tokens = Some(16000), payload must include max_output_tokens = 16000.
        let captured = Arc::new(tokio::sync::Mutex::new(None::<serde_json::Value>));
        let cap = captured.clone();
        let app = Router::new()
            .route("/responses", post(move |Json(body): Json<serde_json::Value>| {
                let cc = cap.clone();
                async move {
                    *cc.lock().await = Some(body);
                    Json(serde_json::json!({
                        "status":"completed",
                        "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}]
                    }))
                }
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg_with_tokens(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let _ = analyst.decide(dummy_input()).await;
        let body = captured.lock().await.clone().expect("request captured");
        assert_eq!(
            body["max_output_tokens"], 16000,
            "Some(16000) must emit max_output_tokens=16000"
        );
        assert_eq!(body["model"], "muse-spark-1.2-contributor");
        assert_eq!(body["text"]["format"]["type"], "json_object");
    }

    #[tokio::test]
    async fn request_shape_never_sends_tools() {
        let captured = Arc::new(tokio::sync::Mutex::new(None::<serde_json::Value>));
        let cap = captured.clone();
        let app = Router::new()
            .route("/responses", post(move |Json(body): Json<serde_json::Value>| {
                let cc = cap.clone();
                async move {
                    *cc.lock().await = Some(body);
                    Json(serde_json::json!({
                        "status":"completed",
                        "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null}"}]}]
                    }))
                }
            }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let _ = analyst.decide(dummy_input()).await;
        let body = captured.lock().await.clone().expect("request captured");
        assert!(
            body.get("tools").is_none(),
            "hosted tools must be absent, got {:?}",
            body.get("tools")
        );
    }

    #[tokio::test]
    async fn retrieval_success_injects_merged_context_and_records_query() {
        let (addr, mock) = retrieval_mock(true, true).await;
        let outcome = retrieval_analyst(addr).decide(dummy_input()).await;
        assert!(outcome.decision.is_some());
        assert_eq!(outcome.searched_queries, vec!["SOL crypto market news"]);
        let request = mock
            .responses
            .lock()
            .await
            .first()
            .cloned()
            .expect("response request");
        let prompt = request["input"][0]["content"][0]["text"]
            .as_str()
            .expect("prompt");
        assert!(prompt.contains("RETRIEVED WEB CONTEXT"));
        assert!(prompt.contains("Tavily SOL headline"));
        assert!(prompt.contains("Exa SOL headline"));
        assert!(request.get("tools").is_none());
    }

    #[tokio::test]
    async fn retrieval_provider_errors_do_not_block_decide() {
        let (addr, mock) = retrieval_mock(false, false).await;
        let outcome = retrieval_analyst(addr).decide(dummy_input()).await;
        assert!(outcome.decision.is_some());
        assert!(outcome.searched_queries.is_empty());
        let request = mock
            .responses
            .lock()
            .await
            .first()
            .cloned()
            .expect("response request");
        let prompt = request["input"][0]["content"][0]["text"]
            .as_str()
            .expect("prompt");
        assert!(!prompt.contains("RETRIEVED WEB CONTEXT"));
    }

    #[tokio::test]
    async fn retrieval_uses_surviving_provider_context() {
        let (addr, mock) = retrieval_mock(false, true).await;
        let outcome = retrieval_analyst(addr).decide(dummy_input()).await;
        assert_eq!(outcome.searched_queries, vec!["SOL crypto market news"]);
        let request = mock
            .responses
            .lock()
            .await
            .first()
            .cloned()
            .expect("response request");
        let prompt = request["input"][0]["content"][0]["text"]
            .as_str()
            .expect("prompt");
        assert!(prompt.contains("Exa SOL headline"));
        assert!(!prompt.contains("Tavily SOL headline"));
    }

    #[tokio::test]
    async fn retrieval_caches_market_results_for_fifteen_minutes() {
        let (addr, mock) = retrieval_mock(true, true).await;
        let analyst = retrieval_analyst(addr);
        let _ = analyst.decide(dummy_input()).await;
        let _ = analyst.decide(dummy_input()).await;
        assert_eq!(mock.tavily_hits.load(Ordering::SeqCst), 1);
        assert_eq!(mock.exa_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reasoning_plus_clean_json_parses_single_attempt() {
        // Real production shape: output[0]=reasoning, output[1]=message with clean JSON.
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route("/responses", post(move |Json(_body): Json<serde_json::Value>| {
            let cc = c.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "status": "completed",
                    "output": [
                        {"type":"reasoning","summary":[]},
                        {"type":"message","content":[{"type":"output_text","text":"{\"action\":\"open\",\"side\":\"long\",\"conviction\":0.82,\"thesis\":\"momentum + funding\",\"horizon_hours\":24,\"stop_pct\":1.2,\"tp_pct\":2.4}"}]}
                    ]
                }))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let cfg = test_cfg(format!("http://{addr}"));
        unsafe {
            std::env::set_var("ANALYST_API_KEY", "test");
        }
        let analyst = Analyst::new(cfg);
        let outcome = analyst.decide(dummy_input()).await;
        assert!(
            outcome.decision.is_some(),
            "clean JSON after reasoning should parse"
        );
        let d = outcome.decision.unwrap();
        assert_eq!(d.action, "open");
        assert_eq!(d.conviction, 0.82);
        assert!(
            !outcome.refused,
            "single clean attempt should not be marked refused"
        );
        assert!(outcome.refusal_kind.is_none(), "success never emits kind");
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "should succeed on first attempt"
        );
    }

    #[test]
    fn responses_sse_output_text_deltas_ignore_non_delta_events() {
        let stream = concat!(
            "data: {\"type\":\"response.created\",\"response\":{}}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello \"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{}}\n\n",
            "data: {\"type\":\"response.output_text_delta\",\"delta\":\"world\"}\n\n",
            "data: [DONE]\n\n",
        );

        let deltas = stream
            .as_bytes()
            .split(|byte| *byte == b'\n')
            .collect::<Vec<_>>()
            .split(|line| line.is_empty())
            .flat_map(|frame| {
                response_output_text_deltas(&String::from_utf8_lossy(&frame.join(&b'\n')))
            })
            .collect::<String>();

        assert_eq!(deltas, "hello world");
    }

    #[test]
    fn build_prompt_handles_multibyte_utf8_body() {
        // Regression: byte-slice at 200 panicked mid-codepoint on '’' (3 bytes), killing the daemon.
        let body = format!("{}{}{}", "a".repeat(199), "’", "z".repeat(500));
        let mut input = dummy_input();
        input.news = vec![crate::contracts::NewsItem {
            id: 1,
            ts: 0,
            source: "rss".into(),
            title: "T".into(),
            body,
            url: "http://x".into(),
            markets: vec![],
        }];
        let p = build_prompt(&input);
        assert!(p.contains('’'), "curly quote preserved in prompt");
        // truncated to 200 chars, includes the boundary char, no panic occurred
        let news_line = p
            .lines()
            .find(|l| l.starts_with("- rss:"))
            .expect("news line");
        let snippet = news_line.strip_prefix("- rss: T | ").expect("prefix");
        assert_eq!(snippet.chars().count(), 200);
    }

    #[test]
    fn prompt_includes_atr_when_some_and_omits_when_none() {
        let mut input = dummy_input();
        input.atr_pct = Some(0.1234);
        let p = build_prompt(&input);
        assert!(
            p.contains("atr15m=0.123%"),
            "prompt should include atr token {p}"
        );
        // three decimals
        assert!(p.contains("atr15m="));
        let mut input2 = dummy_input();
        input2.atr_pct = None;
        let p2 = build_prompt(&input2);
        assert!(!p2.contains("atr15m="), "None should omit atr token");
        // Ensure None doesn't leave stray token
        assert!(!p2.contains("atr15m"));
    }

    #[test]
    fn prompt_atr_formatting_three_decimals() {
        let mut input = dummy_input();
        input.atr_pct = Some(1.5);
        let p = build_prompt(&input);
        // 1.5 formats as 1.500%
        assert!(p.contains("atr15m=1.500%"), "got {p}");
    }

    #[test]
    fn extract_balanced_prose_prefix() {
        let text = "Disclaimer paragraph before JSON.\n\n{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"ok\"} trailing";
        let js = extract_json(text).expect("extract");
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["action"], "skip");
    }

    #[test]
    fn extract_first_balanced_nested() {
        let text = "a {\"a\":{\"b\":1},\"c\":2} b {\"d\":3}";
        let js = extract_json(text).unwrap();
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert!(v.get("a").is_some());
    }

    #[test]
    fn prompt_demands_json_object_first() {
        // Structural demand reduces 42% prose-only first attempts (~450 wasted calls/day)
        let p = build_prompt(&dummy_input());
        assert!(
            p.contains("MUST contain at least one complete JSON object"),
            "prompt must demand JSON object presence: {p}"
        );
        assert!(
            p.contains("Output the JSON object FIRST"),
            "prompt must demand JSON first: {p}"
        );
        assert!(
            p.contains("AFTER the JSON"),
            "prompt must allow commentary after JSON: {p}"
        );
        // review prompt same preamble
        let pos = crate::contracts::Position {
            id: 1,
            market: "SOL".into(),
            side: crate::contracts::Side::Long,
            entry_px: 100.0,
            size: 1.0,
            leverage: 5.0,
            margin: 20.0,
            sl_px: 98.0,
            tp_px: 104.0,
            opened_ts: 0,
            analyst: String::new(),
            horizon_hours: Some(24.0),
        };
        let feats = crate::contracts::Features {
            r5m: 0.0,
            r1h: 0.0,
            r24h: 0.0,
            vol1h: 0.4,
            funding_z: 0.0,
            range_pos: 0.5,
        };
        let rp = build_review_prompt(&pos, &feats, &[], 100.0, 3_600_000);
        assert!(
            rp.contains("MUST contain at least one complete JSON object"),
            "review prompt same demand"
        );
        // paper-simulation framing must stay exact (load-bearing for policy)
        assert!(
            p.contains("automated PAPER trading simulator"),
            "paper framing must stay"
        );
        assert!(p.contains("no real funds exist"), "paper framing must stay");
    }

    // ── Wave 2: richer entry context (same-market record, account state, open book) ───

    #[test]
    fn entry_prompt_carries_recent_outcomes_account_and_open_book() {
        let mut input = dummy_input();
        input.recent = vec![
            RecentOutcome {
                action: "sl".into(),
                net_pnl: -9.1,
                hours_ago: 3.0,
            },
            RecentOutcome {
                action: "tp".into(),
                net_pnl: 14.2,
                hours_ago: 9.4,
            },
        ];
        input.account = AccountState {
            equity: 1012.34,
            day_pnl_pct: 1.23,
            kill_budget_used_pct: 10.0,
        };
        input.open_positions = vec![
            pos("SOL", Side::Long, 100.0, 2.0),
            pos("BTC", Side::Short, 60000.0, 0.01),
        ];
        input.open_marks = [("SOL".to_string(), 101.5)].into_iter().collect();

        let p = build_prompt(&input);
        // (a) same-market record — newest first, signed net, age
        assert!(
            p.contains("recent SOL: sl -9.10 (3h ago), tp +14.20 (9h ago)\n"),
            "{p}"
        );
        // (b) account state
        assert!(
            p.contains("account: equity=$1012.34 day_pnl=+1.23% kill_budget_used=10%\n"),
            "{p}"
        );
        // (c) open-book summary: count + total entry notional + per-position uPnL
        assert!(
            p.contains("PORTFOLIO open_positions: n=2 notional=$800.00\n"),
            "{p}"
        );
        assert!(
            p.contains("SOL Long uPnL=+3.00\n"),
            "long uPnL from the live mark: {p}"
        );
        assert!(
            p.contains("BTC Short uPnL=n/a\n"),
            "a market missing from the snapshot is n/a, never 0: {p}"
        );
        // the whole addition stays inside its ~12-line budget
        let base = build_prompt(&dummy_input()).lines().count();
        assert!(
            p.lines().count() - base <= 12,
            "entry context grew past 12 lines ({} extra)",
            p.lines().count() - base
        );
    }

    #[test]
    fn empty_history_renders_none_not_blank() {
        let p = build_prompt(&dummy_input());
        assert!(
            p.contains("recent SOL: none\n"),
            "a market that has never closed must say none: {p}"
        );
        assert!(
            p.contains("PORTFOLIO open_positions: n=0 notional=$0.00\nnone\n"),
            "{p}"
        );
        assert!(
            p.contains("account: equity=$0.00 day_pnl=+0.00% kill_budget_used=0%\n"),
            "{p}"
        );
        assert!(!p.contains("recent SOL: \n"), "never an empty value");
    }

    #[test]
    fn open_book_is_capped_and_short_upnl_is_mirrored() {
        let mut input = dummy_input();
        input.open_positions = (0..8)
            .map(|i| pos(&format!("M{i}"), Side::Short, 100.0, 1.0))
            .collect();
        input.open_marks = (0..8).map(|i| (format!("M{i}"), 98.0)).collect();
        let p = build_prompt(&input);
        assert!(p.contains("n=8 notional=$800.00"), "{p}");
        assert_eq!(p.matches("uPnL=").count(), 6, "at most six position lines");
        assert!(
            p.contains("(+2 more)\n"),
            "the tail is summarised, not dropped silently: {p}"
        );
        assert!(
            p.contains("M0 Short uPnL=+2.00\n"),
            "a short profits when the mark falls: {p}"
        );
        // an invalid mark degrades exactly like the money guard elsewhere
        let mut bad = dummy_input();
        bad.open_positions = vec![pos("SOL", Side::Long, 100.0, 1.0)];
        for m in [0.0, -1.0, f64::NAN] {
            bad.open_marks = [("SOL".to_string(), m)].into_iter().collect();
            assert!(
                build_prompt(&bad).contains("SOL Long uPnL=n/a\n"),
                "mark {m} must be unknown"
            );
        }
    }

    #[test]
    fn recent_ages_step_from_minutes_to_hours() {
        assert_eq!(fmt_ago(0.0), "0m ago");
        assert_eq!(fmt_ago(0.5), "30m ago");
        assert_eq!(fmt_ago(0.99), "59m ago");
        assert_eq!(fmt_ago(1.0), "1h ago");
        assert_eq!(fmt_ago(9.4), "9h ago");
        assert_eq!(
            fmt_ago(-5.0),
            "0m ago",
            "a clock step must not report a future trade"
        );
        assert_eq!(fmt_ago(f64::NAN), "0m ago");
        // only the last three closes make the line, however many are handed over
        let many: Vec<RecentOutcome> = (0..5)
            .map(|i| RecentOutcome {
                action: "sl".into(),
                net_pnl: -1.0,
                hours_ago: i as f64 + 1.0,
            })
            .collect();
        let line = recent_line("SOL", &many);
        assert_eq!(line.matches("sl -1.00").count(), 3, "{line}");
    }

    // ── T1: fee hurdle + reviewer position context ────────────────────────────────────

    fn review_fixture() -> (Position, Features) {
        // long 2 units @ 100, stop 98 -> initial risk $4.00, entry notional $200
        let pos = Position {
            id: 1,
            market: "SOL".into(),
            side: Side::Long,
            entry_px: 100.0,
            size: 2.0,
            leverage: 5.0,
            margin: 40.0,
            sl_px: 98.0,
            tp_px: 104.0,
            opened_ts: 0,
            analyst: String::new(),
            horizon_hours: Some(24.0),
        };
        let feats = Features {
            r5m: 0.0,
            r1h: 0.0,
            r24h: 0.0,
            vol1h: 0.4,
            funding_z: 0.0,
            range_pos: 0.5,
        };
        (pos, feats)
    }

    #[test]
    fn fee_hurdle_line_is_in_both_prompts() {
        let (pos, feats) = review_fixture();
        let entry = build_prompt(&dummy_input());
        let review = build_review_prompt(&pos, &feats, &[], 100.0, 0);
        for (name, p) in [("entry", &entry), ("review", &review)] {
            assert!(
                p.contains("FEE HURDLE"),
                "{name} prompt must carry the fee hurdle: {p}"
            );
            assert!(
                p.contains("15bp"),
                "{name} prompt must state the round-trip cost"
            );
            assert!(
                p.contains("3x that cost"),
                "{name} prompt must demand 3x the cost"
            );
            assert_eq!(
                p.matches("FEE HURDLE").count(),
                1,
                "{name} prompt must state it exactly once"
            );
        }
    }

    #[test]
    fn review_prompt_carries_mark_upnl_r_multiple_held_and_fees() {
        let (pos, feats) = review_fixture();
        // mark 101 -> uPnL = (101-100)*2 = +2.00; initial risk = |100-98|*2 = 4.00 -> +0.50R
        // held 3h; entry fee = 100*2*0.00075 = 0.15
        let p = build_review_prompt(&pos, &feats, &[], 101.0, 3 * 3_600_000);
        assert!(p.contains("mark=101.00"), "mark missing: {p}");
        assert!(p.contains("unrealized_pnl=2.0000"), "uPnL missing: {p}");
        assert!(p.contains("r_multiple=0.50"), "R multiple missing: {p}");
        assert!(p.contains("held_h=3.00"), "held_h missing: {p}");
        assert!(p.contains("fees_paid=0.1500"), "fees missing: {p}");
        assert!(
            p.contains("estimate"),
            "fees must be flagged as an estimate: {p}"
        );
    }

    #[test]
    fn review_context_math_long_short_and_stop_touch() {
        let (long, _) = review_fixture();
        // long at -1R: mark exactly at the stop
        let at_stop = position_context(&long, 98.0, 0);
        assert_eq!(at_stop.unrealized_pnl, Some(-4.0));
        assert_eq!(
            at_stop.r_multiple,
            Some(-1.0),
            "mark at the stop is exactly -1R"
        );
        // long in profit
        let up = position_context(&long, 104.0, 0);
        assert_eq!(up.unrealized_pnl, Some(8.0));
        assert_eq!(up.r_multiple, Some(2.0));

        // short 2 units @ 100, stop 102 -> risk $4; mark 98 is +$4 = +1R
        let short = Position {
            side: Side::Short,
            sl_px: 102.0,
            tp_px: 96.0,
            ..long.clone()
        };
        let s = position_context(&short, 98.0, 0);
        assert_eq!(s.unrealized_pnl, Some(4.0));
        assert_eq!(s.r_multiple, Some(1.0));
        assert_eq!(position_context(&short, 102.0, 0).r_multiple, Some(-1.0));

        // held_h from opened_ts, fee from entry notional
        let held = position_context(&long, 100.0, 90 * 60 * 1000);
        assert!(
            (held.held_h - 1.5).abs() < 1e-12,
            "90 minutes is 1.5h, got {}",
            held.held_h
        );
        assert!(
            (held.fees_paid - 0.15).abs() < 1e-12,
            "entry fee 200 * 7.5bp, got {}",
            held.fees_paid
        );
        // a clock that reads before opened_ts must not report negative hold time
        assert_eq!(position_context(&long, 100.0, -5_000).held_h, 0.0);
    }

    // ── TECHNICALS block: indicator series in the entry prompt ───────────────────────────

    fn candle_series(closes: &[f64], interval: &str) -> Vec<crate::hl_rest::Candle> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| crate::hl_rest::Candle {
                t: i as i64 * 900_000,
                T: i as i64 * 900_000 + 899_999,
                s: "SOL".into(),
                i: interval.into(),
                o: c,
                c,
                h: c * 1.001,
                l: c * 0.999,
                v: 1000.0 + i as f64 * 10.0,
                n: 5,
            })
            .collect()
    }

    #[test]
    fn entry_prompt_technicals_render_series_and_context() {
        let mut input = dummy_input();
        let m15_closes: Vec<f64> = (0..64).map(|i| 100.0 + i as f64 * 0.5).collect();
        let h4_closes: Vec<f64> = (0..60).map(|i| 90.0 + i as f64).collect();
        input.candles = Some(crate::hl_rest::MarketCandles {
            m15: candle_series(&m15_closes, "15m"),
            h4: candle_series(&h4_closes, "4h"),
            fetched_ms: 1,
        });
        input.open_interest = Some(12345.0);
        input.funding = Some(0.0001);
        let p = build_prompt(&input);
        assert!(p.contains("TECHNICALS 15m (oldest→newest):\n"), "{p}");
        // last 10 closes of the ramp, 2dp, oldest→newest
        assert!(
            p.contains("close=[127.00,127.50,128.00,128.50,129.00,129.50,130.00,130.50,131.00,131.50]\n"),
            "{p}"
        );
        // strictly rising series -> RSI 100 on both windows
        assert!(p.contains("rsi7=[100.0"), "{p}");
        assert!(p.contains("rsi14=[100.0"), "{p}");
        // 4h context line + oi/funding riders
        assert!(p.contains("4h: ema20="), "{p}");
        assert!(p.contains("atr3="), "{p}");
        assert!(p.contains("oi=12345 funding=0.000100\n"), "{p}");
        // sits after the features line, before NEWS
        let features = p.find("features={").unwrap();
        let tech = p.find("TECHNICALS").unwrap();
        let news = p.find("NEWS last 6h").unwrap();
        assert!(features < tech && tech < news, "ordering: {p}");
    }

    #[test]
    fn entry_prompt_technicals_unavailable_never_blocks() {
        // No candles in the input — the prompt says so and keeps going (atr_pct rule).
        let p = build_prompt(&dummy_input());
        assert!(p.contains("TECHNICALS: series: unavailable\n"), "{p}");
        assert!(p.contains("NEWS last 6h"), "{p}");
        // Empty 15m bars degrade the same way.
        let mut input = dummy_input();
        input.candles = Some(crate::hl_rest::MarketCandles {
            m15: vec![],
            h4: vec![],
            fetched_ms: 1,
        });
        let p2 = build_prompt(&input);
        assert!(p2.contains("TECHNICALS: series: unavailable\n"), "{p2}");
    }

    #[test]
    fn entry_prompt_technicals_short_history_degrades_per_field() {
        // 5 closes only: 15m series render what exists; rsi14 is all warm-up (na), and the
        // 4h fields individually read na rather than blanking the block.
        let mut input = dummy_input();
        input.candles = Some(crate::hl_rest::MarketCandles {
            m15: candle_series(&[100.0, 101.0, 102.0, 103.0, 104.0], "15m"),
            h4: candle_series(&[100.0, 101.0], "4h"),
            fetched_ms: 1,
        });
        let p = build_prompt(&input);
        assert!(p.contains("close=[100.00,101.00,102.00,103.00,104.00]\n"), "{p}");
        // 5 closes < period+1 for both rsi windows -> empty series render as [na]
        assert!(p.contains("rsi7=[na] rsi14=[na]\n"), "{p}");
        assert!(p.contains("rsi14=na"), "{p}"); // 4h line: same for the scalar
        assert!(p.contains("atr14=na"), "{p}");
        assert!(p.contains("oi=na funding=na\n"), "{p}");
    }

    #[test]
    fn review_context_degrades_on_unknown_mark_and_zero_risk() {
        let (pos, feats) = review_fixture();
        // 0.0 is the review loop's "market missing from snapshot" sentinel (money guard)
        for bad in [0.0, -1.0, f64::NAN, f64::NEG_INFINITY] {
            let ctx = position_context(&pos, bad, 0);
            assert_eq!(ctx.mark, None, "mark {bad} must be unknown");
            assert_eq!(ctx.unrealized_pnl, None, "no uPnL without a mark");
            assert_eq!(ctx.r_multiple, None, "no R without a mark");
            assert!(ctx.fees_paid > 0.0, "fees are known regardless of mark");
        }
        let p = build_review_prompt(&pos, &feats, &[], 0.0, 0);
        assert!(
            p.contains("mark=n/a"),
            "unknown mark must print n/a, never 0.00: {p}"
        );
        assert!(p.contains("unrealized_pnl=n/a"));
        assert!(p.contains("r_multiple=n/a"));
        assert!(p.contains("fees_paid=0.1500"), "fees still reported: {p}");

        // degenerate stop (entry == sl) -> no R multiple, but uPnL still reported
        let no_risk = Position {
            sl_px: 100.0,
            ..pos.clone()
        };
        let ctx = position_context(&no_risk, 101.0, 0);
        assert_eq!(ctx.unrealized_pnl, Some(2.0));
        assert_eq!(
            ctx.r_multiple, None,
            "zero initial risk must not divide by zero"
        );
    }

    // ── Leverage preamble + prompt enrichment ─────────────────────────────────────

    #[test]
    fn system_preamble_documents_leverage_and_tool_hint() {
        assert!(SYSTEM_PREAMBLE.contains("leverage: 1.0-10.0 (1x-10x)"), "preamble must document leverage range");
        assert!(SYSTEM_PREAMBLE.contains("majors up to 10x"), "preamble must mention majors 10x");
        assert!(SYSTEM_PREAMBLE.contains("xyz up to 5x"), "preamble must mention xyz 5x");
        assert!(SYSTEM_PREAMBLE.contains("get_orderbook_imbalance"), "preamble must hint at tool-use");
        assert!(SYSTEM_PREAMBLE.contains("max 1 tool round"), "preamble must mention max 1 tool round");
        // also appears in built prompt via preamble
        let p = build_prompt(&dummy_input());
        assert!(p.contains("leverage: 1.0-10.0"), "prompt should carry leverage rule");
        assert!(p.contains("get_orderbook_imbalance"), "prompt should carry tool hint");
    }

    #[test]
    fn tool_definitions_are_valid_json() {
        let tools = tool_definitions();
        assert_eq!(tools.len(), 3, "exactly three read-only tools");
        let names: Vec<String> = tools.iter().map(|t| t["function"]["name"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&"get_orderbook_imbalance".to_string()));
        assert!(names.contains(&"get_funding_context".to_string()));
        assert!(names.contains(&"get_candle_context".to_string()));
        for t in &tools {
            assert_eq!(t["type"], "function");
            let func = &t["function"];
            assert!(func.get("name").is_some());
            assert!(func.get("description").is_some());
            assert!(func.get("parameters").is_some());
            // parameters must be valid JSON schema
            let params = &func["parameters"];
            assert_eq!(params["type"], "object");
            // each tool's required includes market
            let req = params["required"].as_array().expect("required array");
            assert!(req.iter().any(|v| v == "market"), "market required");
        }
        // ensure serialized form is valid JSON
        let serialized = serde_json::to_string(&tools).expect("serialize tools");
        let parsed: serde_json::Value = serde_json::from_str(&serialized).expect("re-parse tools");
        assert!(parsed.is_array());
    }

    #[tokio::test]
    async fn decide_with_tools_fallback_no_tool_calls_parses_single_call() {
        // Mock chat server that returns NO tool_calls, just JSON — decide_with_tools must fallback to single-call parse
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route("/chat/completions", post(move |Json(_body): Json<serde_json::Value>| {
            let cc = c.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "choices": [{"message": {"role":"assistant","content":"{\"action\":\"open\",\"side\":\"long\",\"conviction\":0.82,\"thesis\":\"no tools needed\",\"horizon_hours\":24,\"stop_pct\":1.2,\"tp_pct\":2.4,\"invalidation_condition\":\"if breaks\",\"risk_usd\":10.0,\"leverage\":4.0}"}, "finish_reason":"stop"}]
                }))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let mut cfg = test_cfg(format!("http://{addr}"));
        cfg.models = vec![AnalystModelCfg { id: "test-chat".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None }];
        cfg.model = "test-chat".into();
        unsafe { std::env::set_var("ANALYST_API_KEY", "test"); }
        let mut analyst = Analyst::new(cfg);
        // wire a dummy ToolExecutor (won't be used because no tool_calls)
        let book_cache: BookCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let engine = Arc::new(tokio::sync::Mutex::new(crate::features::FeatureEngine::new()));
        let candle_cache: CandleCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        analyst.set_tool_executor(ToolExecutor::new(book_cache, engine, candle_cache));
        let outcome = analyst.decide_with_tools(dummy_input()).await;
        assert!(outcome.decision.is_some(), "fallback single-call should parse");
        let d = outcome.decision.unwrap();
        assert_eq!(d.action, "open");
        assert_eq!(d.leverage, Some(4.0));
        assert_eq!(counter.load(Ordering::SeqCst), 1, "should have used single call, no second round");
        assert_eq!(outcome.calls.len(), 1);
    }

    #[tokio::test]
    async fn decide_with_tools_executes_orderbook_tool_and_returns_leverage() {
        // Two-round mock: first returns tool_call for get_orderbook_imbalance, second returns open with leverage 7.5
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route("/chat/completions", post(move |Json(body): Json<serde_json::Value>| {
            let cc = c.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // first call must include tools
                    assert!(body.get("tools").is_some(), "first call should have tools");
                    assert_eq!(body["tool_choice"], "auto");
                    Json(serde_json::json!({
                        "choices": [{"message": {"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_orderbook_imbalance","arguments":"{\"market\":\"BTC\",\"depth\":10}"}}]}, "finish_reason":"tool_calls"}]
                    }))
                } else {
                    // second call must NOT have tools, and must contain tool result in prompt
                    assert!(body.get("tools").is_none(), "second call should not have tools");
                    let prompt = body["messages"].as_array().and_then(|arr| {
                        // second call uses messages with tool results appended; our implementation uses single prompt string, so check messages[0] or prompt?
                        // For our impl, second call is via call_chat_primary which sends messages:[{role:user,content:second_prompt}]
                        // So just check that the content contains tool result marker
                        arr.iter().find_map(|m| m.get("content").and_then(|c| c.as_str()).map(|s| s.to_string()))
                    }).unwrap_or_default();
                    // Our decide_with_tools builds second_prompt with "TOOL RESULTS:" marker; chat primary sends it as messages[0].content
                    // So we can't easily assert here, but we can ensure second call happens
                    Json(serde_json::json!({
                        "choices": [{"message": {"role":"assistant","content":"{\"action\":\"open\",\"side\":\"long\",\"conviction\":0.88,\"thesis\":\"book shows bid pressure\",\"horizon_hours\":24,\"stop_pct\":1.2,\"tp_pct\":2.4,\"invalidation_condition\":\"if ask pressure rises\",\"risk_usd\":12.0,\"leverage\":7.5}"}, "finish_reason":"stop"}]
                    }))
                }
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let mut cfg = test_cfg(format!("http://{addr}"));
        cfg.models = vec![AnalystModelCfg { id: "test-chat".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None }];
        cfg.model = "test-chat".into();
        unsafe { std::env::set_var("ANALYST_API_KEY", "test"); }
        let mut analyst = Analyst::new(cfg);
        // Prepare ToolExecutor with real cache containing BTC book
        let book_cache: BookCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        {
            let mut cache = book_cache.write().await;
            let book = crate::contracts::L2Book {
                levels: [
                    vec![crate::contracts::L2Level { px: 99.9, sz: 5.0 }, crate::contracts::L2Level { px: 99.8, sz: 3.0 }],
                    vec![crate::contracts::L2Level { px: 100.1, sz: 4.0 }, crate::contracts::L2Level { px: 100.2, sz: 2.0 }],
                ],
                ts: chrono::Utc::now().timestamp_millis(),
            };
            cache.insert("BTC".to_string(), book);
        }
        let engine = Arc::new(tokio::sync::Mutex::new(crate::features::FeatureEngine::new()));
        // seed funding for BTC so get_funding_context would work if called
        {
            let mut eng = engine.lock().await;
            let base = chrono::Utc::now().timestamp_millis() - 30 * 3_600_000;
            for i in 0..30 {
                eng.funding_z("BTC", 0.0001 + (i as f64)*1e-6, base + i*3_600_000);
            }
        }
        let candle_cache: CandleCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        // seed candle for BTC
        {
            let mut cache = candle_cache.write().await;
            let closes: Vec<f64> = (0..64).map(|i| 100.0 + i as f64 * 0.1).collect();
            let candles: Vec<crate::hl_rest::Candle> = closes.iter().enumerate().map(|(i, &c)| crate::hl_rest::Candle {
                t: i as i64 * 900_000,
                T: i as i64 * 900_000 + 899_999,
                s: "BTC".into(),
                i: "15m".into(),
                o: c,
                c,
                h: c*1.001,
                l: c*0.999,
                v: 1000.0,
                n: 5,
            }).collect();
            cache.insert("BTC".to_string(), crate::hl_rest::MarketCandles { m15: candles.clone(), h4: candles, fetched_ms: chrono::Utc::now().timestamp_millis() });
        }
        analyst.set_tool_executor(ToolExecutor::new(book_cache.clone(), engine.clone(), candle_cache.clone()));
        let outcome = analyst.decide_with_tools(dummy_input()).await;
        assert!(outcome.decision.is_some(), "tool round should produce decision");
        let d = outcome.decision.unwrap();
        assert_eq!(d.action, "open");
        assert_eq!(d.leverage, Some(7.5));
        assert_eq!(counter.load(Ordering::SeqCst), 2, "should have made 2 LLM calls");
        assert_eq!(outcome.calls.len(), 2);
        assert_eq!(outcome.calls[0].outcome_kind, "tool_calls");
        // Verify that leverage clamping works per-market via sizing helper (integration check)
        // BTC 7.5 stays 7.5, xyz would clamp to 5
        assert!((crate::sizing::clamp_leverage("BTC", d.leverage.unwrap()) - 7.5).abs() < 1e-9);
        assert!((crate::sizing::clamp_leverage("xyz:TSLA", d.leverage.unwrap()) - 5.0).abs() < 1e-9, "xyz must clamp 7.5 to 5");
        // Simulate storing position with that leverage (paper-only, no exchange)
        let store = crate::ledger::Store::open("sqlite::memory:").await.expect("store");
        let sized = crate::sizing::Sized { leverage: crate::sizing::clamp_leverage("BTC", d.leverage.unwrap()), margin: 20.0, notional: 20.0*7.5, stop_pct: 1.2, tp_pct: 2.4 };
        let pos = store.open_position("BTC", crate::contracts::Side::Long, &sized, 100.0, false, 24.0, None).await.expect("open");
        assert!((pos.leverage - 7.5).abs() < 1e-9, "position stored with 7.5");
        let sized_xyz = crate::sizing::Sized { leverage: crate::sizing::clamp_leverage("xyz:TSLA", d.leverage.unwrap()), margin: 20.0, notional: 20.0*5.0, stop_pct: 1.2, tp_pct: 2.4 };
        let pos_xyz = store.open_position("xyz:TSLA", crate::contracts::Side::Long, &sized_xyz, 100.0, true, 24.0, None).await.expect("open xyz");
        assert!((pos_xyz.leverage - 5.0).abs() < 1e-9, "xyz clamped to 5");
    }

    #[tokio::test]
    async fn decide_with_tools_responses_api_falls_back_to_oneshot() {
        // For responses api_style (muse-spark), decide_with_tools should gracefully fallback to one-shot (no tools)
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        let app = Router::new().route("/responses", post(move |Json(_body): Json<serde_json::Value>| {
            let cc = c.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({
                    "status":"completed",
                    "output":[{"type":"message","content":[{"type":"output_text","text":"{\"action\":\"skip\",\"side\":null,\"conviction\":0.4,\"thesis\":\"fallback ok\",\"horizon_hours\":null,\"stop_pct\":null,\"tp_pct\":null,\"invalidation_condition\":null,\"risk_usd\":null,\"leverage\":null}"}]}]
                }))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let mut cfg = test_cfg(format!("http://{addr}"));
        cfg.models = vec![AnalystModelCfg { id: "muse-test".into(), api_style: "responses".into(), enabled: true, temperature: None, max_tokens: None }];
        cfg.model = "muse-test".into();
        unsafe { std::env::set_var("ANALYST_API_KEY", "test"); }
        let mut analyst = Analyst::new(cfg);
        let book_cache: BookCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let engine = Arc::new(tokio::sync::Mutex::new(crate::features::FeatureEngine::new()));
        let candle_cache: CandleCache = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        analyst.set_tool_executor(ToolExecutor::new(book_cache, engine, candle_cache));
        let outcome = analyst.decide_with_tools(dummy_input()).await;
        assert!(outcome.decision.is_some());
        assert_eq!(outcome.decision.unwrap().action, "skip");
        assert_eq!(counter.load(Ordering::SeqCst), 1, "fallback should have made one responses call");
        // ensure no tools were sent (responses path never sends tools)
        // The mock only handles /responses, so if tools were attempted via /chat/completions it would 404
    }

    #[test]
    fn per_model_stats_line_contains_win_avg_total() {
        let mut input = dummy_input();
        input.analyst_id = "laguna-s-2.1-free".into();
        // 10 closes: 3 wins, 7 losses, total -4.2, avg -0.42
        input.analyst_recent = vec![
            RecentOutcome { action: "sl".into(), net_pnl: -1.0, hours_ago: 1.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -1.0, hours_ago: 2.0 },
            RecentOutcome { action: "tp".into(), net_pnl: 2.0, hours_ago: 3.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -1.5, hours_ago: 4.0 },
            RecentOutcome { action: "tp".into(), net_pnl: 1.5, hours_ago: 5.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -2.0, hours_ago: 6.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -0.8, hours_ago: 7.0 },
            RecentOutcome { action: "tp".into(), net_pnl: 0.9, hours_ago: 8.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -1.1, hours_ago: 9.0 },
            RecentOutcome { action: "sl".into(), net_pnl: -1.2, hours_ago: 10.0 },
        ];
        // Two open positions for this analyst, with marks
        let p1 = Position { id: 1, market: "BTC".into(), side: Side::Long, entry_px: 100.0, size: 1.0, leverage: 5.0, margin: 20.0, sl_px: 98.0, tp_px: 104.0, opened_ts: 0, analyst: "laguna-s-2.1-free".into(), horizon_hours: Some(24.0) };
        let p2 = Position { id: 2, market: "SOL".into(), side: Side::Short, entry_px: 100.0, size: 2.0, leverage: 5.0, margin: 20.0, sl_px: 102.0, tp_px: 96.0, opened_ts: 0, analyst: "laguna-s-2.1-free".into(), horizon_hours: Some(24.0) };
        input.open_positions = vec![p1.clone(), p2.clone()];
        // Global mark map: BTC up 0.5, SOL down 0.5 for short profit
        input.open_marks = [("BTC".to_string(), 100.5), ("SOL".to_string(), 99.0)].into_iter().collect();
        let p = build_prompt(&input);
        // Must contain one-line stats with win 3/10, avg, total, open n=2 and uPnL
        assert!(p.contains("your stats (last 10 closed):"), "stats line header missing: {p}");
        assert!(p.contains("win 3/10"), "win rate missing: {p}");
        // avg -0.42, total -4.20 (allow both one and two decimal)
        assert!(p.contains("avg -0.42"), "avg missing: {p}");
        assert!(p.contains("total -4.2"), "total missing: {p}");
        assert!(p.contains("open n=2"), "open count missing: {p}");
        assert!(p.contains("uPnL"), "uPnL missing: {p}");
        // compact: one line
        let stats_line = p.lines().find(|l| l.starts_with("your stats")).expect("stats line");
        assert!(stats_line.contains("win") && stats_line.contains("avg") && stats_line.contains("total") && stats_line.contains("open"), "all parts on one line: {stats_line}");
    }

    #[test]
    fn decision_tree_preamble_contains_ordered_steps() {
        assert!(SYSTEM_PREAMBLE.contains("(1) 4h trend"), "step 1 missing");
        assert!(SYSTEM_PREAMBLE.contains("(2) does 15m agree"), "step 2 missing");
        assert!(SYSTEM_PREAMBLE.contains("(3) invalidation"), "step 3 missing");
        assert!(SYSTEM_PREAMBLE.contains("(4) risk/reward"), "step 4 missing");
        assert!(SYSTEM_PREAMBLE.contains("(5) verdict"), "step 5 missing");
        assert!(SYSTEM_PREAMBLE.contains("default to skip"), "no-trade default missing");
        assert!(SYSTEM_PREAMBLE.contains("bear case") || SYSTEM_PREAMBLE.contains("bear"), "counter-case bear missing");
        assert!(SYSTEM_PREAMBLE.contains("could fail"), "could fail missing");
    }

    #[test]
    fn hy3_json_anchor_present_and_json_word_twice() {
        // JSON anchor + literal "json" twice + Begin with "{"
        assert!(SYSTEM_PREAMBLE.contains("Begin your reply with \"{\""), "JSON anchor missing");
        let lower = SYSTEM_PREAMBLE.to_ascii_lowercase();
        let count = lower.matches("json").count();
        assert!(count >= 2, "need at least two literal 'json' (case-insensitive), got {count}");
        // also appears in built prompt via preamble
        let p = build_prompt(&dummy_input());
        assert!(p.contains("Begin your reply with \"{\""), "prompt must carry anchor");
    }

    #[test]
    fn per_model_temperature_defaults() {
        assert_eq!(crate::config::default_temperature_for_model("deepseek-v4-flash-free"), Some(1.0));
        assert_eq!(crate::config::default_temperature_for_model("mimo-v2.5-free"), Some(0.7));
        assert_eq!(crate::config::default_temperature_for_model("nemotron-3-ultra-free"), Some(0.8));
        assert_eq!(crate::config::default_temperature_for_model("laguna-s-2.1-free"), Some(0.7));
        let cfg = AnalystModelCfg { id: "deepseek-v4-flash-free".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None };
        assert_eq!(cfg.effective_temperature(), Some(1.0));
        let cfg2 = AnalystModelCfg { id: "hy3-free".into(), api_style: "chat".into(), enabled: true, temperature: Some(0.5), max_tokens: None };
        assert_eq!(cfg2.effective_temperature(), Some(0.5), "explicit wins");
        // per-model max_tokens fallback
        assert_eq!(cfg.effective_max_tokens(None), None);
        let hy = AnalystModelCfg { id: "hy3-free".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None };
        assert_eq!(hy.effective_max_tokens(None), Some(800));
        let lag = AnalystModelCfg { id: "laguna-s-2.1-free".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None };
        assert_eq!(lag.effective_max_tokens(None), Some(400));
        let light = AnalystModelCfg { id: "nemotron-3.5-lightning-free".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None };
        assert_eq!(light.effective_max_tokens(None), Some(400));
        let mimo = AnalystModelCfg { id: "mimo-v2.5-free".into(), api_style: "chat".into(), enabled: true, temperature: None, max_tokens: None };
        assert_eq!(mimo.effective_max_tokens(None), Some(800));
    }
}
