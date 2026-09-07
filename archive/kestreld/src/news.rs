#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, warn};

use crate::config::NewsCfg;
use crate::contracts::NewsItem;
use crate::ledger::Store;

const COOLDOWN_MS: i64 = 15 * 60 * 1000;

/// Conservative, deterministic Telegram call detector. A direction plus a trade level is
/// enough to nominate a market for analyst review; the analyst and every risk gate decide
/// whether anything happens next.
pub fn is_directional_call(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("move sl") || lower.contains("tp reached") {
        return false;
    }
    let direction = ["long", "short", "buy", "sell", "entry"].iter().any(|word| lower.contains(word));
    let level = ["sl ", "sl:", "sl@", "tp ", "tp:", "tp@", "stop", "target"]
        .iter()
        .any(|word| lower.contains(word));
    direction && (level || (lower.contains("entry") && lower.split(|c: char| !c.is_ascii_digit() && c != '.').any(|part| part.contains('.'))))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestResult {
    pub id: i64,
    pub deduped: bool,
}

#[derive(Debug, Error)]
pub enum NewsError {
    #[error("store error: {0}")]
    Store(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
}

impl From<sqlx::Error> for NewsError {
    fn from(e: sqlx::Error) -> Self {
        Self::Store(e.to_string())
    }
}

/// Trait to abstract network for tests.
pub trait HttpFetch: Send + Sync {
    fn fetch_get<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>>;
    fn fetch_tavily<'a>(
        &'a self,
        query: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>>;
}

/// Real fetcher using reqwest.
pub struct ReqwestFetcher {
    client: reqwest::Client,
    tavily_key_env: String,
}

impl ReqwestFetcher {
    pub fn new(tavily_key_env: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("kestreld/0.1")
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest build");
        Self {
            client,
            tavily_key_env,
        }
    }
}

impl HttpFetch for ReqwestFetcher {
    fn fetch_get<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
        let url = url.to_string();
        let client = self.client.clone();
        Box::pin(async move {
            let resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| NewsError::Http(e.to_string()))?;
            let text = resp
                .text()
                .await
                .map_err(|e| NewsError::Http(e.to_string()))?;
            Ok(text)
        })
    }

    fn fetch_tavily<'a>(
        &'a self,
        query: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
        let query = query.to_string();
        let client = self.client.clone();
        let env_name = self.tavily_key_env.clone();
        Box::pin(async move {
            if env_name.is_empty() {
                return Err(NewsError::Http("tavily_key_env not set".into()));
            }
            let key = std::env::var(&env_name)
                .map_err(|_| NewsError::Http(format!("env {env_name} not set")))?;
            let body = serde_json::json!({
                "query": query,
                "topic": "news",
                "days": 1,
                "max_results": 8
            });
            let resp = client
                .post("https://api.tavily.com/search")
                .header("Authorization", format!("Bearer {key}"))
                .json(&body)
                .send()
                .await
                .map_err(|e| NewsError::Http(e.to_string()))?;
            let text = resp
                .text()
                .await
                .map_err(|e| NewsError::Http(e.to_string()))?;
            Ok(text)
        })
    }
}

/// Build built-in alias table.
fn builtin_aliases(market: &str) -> Vec<String> {
    let stripped = market.split(':').next_back().unwrap_or(market).to_uppercase();
    match stripped.as_str() {
        "BTC" => vec!["btc".into(), "bitcoin".into()],
        "ETH" => vec!["eth".into(), "ethereum".into()],
        "SOL" => vec!["sol".into(), "solana".into()],
        "TSLA" => vec!["tsla".into(), "tesla".into(), "musk".into()],
        "NVDA" => vec!["nvda".into(), "nvidia".into()],
        "GOLD" => vec!["gold".into(), "xau".into()],
        "HOOD" => vec!["hood".into(), "robinhood".into()],
        "INTC" => vec!["intc".into(), "intel".into()],
        "PLTR" => vec!["pltr".into(), "palantir".into()],
        "COIN" => vec!["coin".into(), "coinbase".into()],
        "META" => vec!["meta".into(), "facebook".into()],
        "AAPL" => vec!["aapl".into(), "apple".into()],
        "MSFT" => vec!["msft".into(), "microsoft".into()],
        "ORCL" => vec!["orcl".into(), "oracle".into()],
        "GOOGL" => vec!["googl".into(), "google".into(), "alphabet".into()],
        "AMZN" => vec!["amzn".into(), "amazon".into()],
        "AMD" => vec!["amd".into()],
        "MU" => vec!["mu".into(), "micron".into()],
        "SNDK" => vec!["sndisk".into(), "sandisk".into(), "sndk".into()],
        "MSTR" => vec!["mstr".into(), "microstrategy".into(), "strategy".into()],
        "CRCL" => vec!["crcl".into(), "circle".into()],
        "NFLX" => vec!["nflx".into(), "netflix".into()],
        "COST" => vec!["cost".into(), "costco".into()],
        "LLY" => vec!["lly".into(), "lilly".into(), "eli lilly".into()],
        "SKHX" => vec!["skhx".into(), "sk hynix".into()],
        "TSM" => vec!["tsm".into(), "tsmc".into()],
        "JPY" => vec!["jpy".into(), "yen".into(), "japanese yen".into()],
        "EUR" => vec!["eur".into(), "euro".into()],
        "SILVER" => vec!["silver".into(), "xag".into()],
        "RIVN" => vec!["rivn".into(), "rivian".into()],
        "BABA" => vec!["baba".into(), "alibaba".into()],
        "CL" => vec!["cl".into(), "crude".into(), "oil".into(), "wti".into()],
        "COPPER" => vec!["copper".into()],
        "NATGAS" => vec!["natgas".into(), "natural gas".into()],
        "URANIUM" => vec!["uranium".into()],
        "ALUMINIUM" => vec!["aluminium".into(), "aluminum".into()],
        "SMSN" => vec!["smsn".into(), "samsung".into()],
        "PLATINUM" => vec!["platinum".into(), "xpt".into()],
        "PALLADIUM" => vec!["palladium".into()],
        "GME" => vec!["gme".into(), "gamestop".into()],
        "KR200" => vec!["kr200".into(), "kospi".into()],
        "VIX" => vec!["vix".into(), "volatility".into()],
        "HIMS" => vec!["hims".into()],
        "SP500" => vec!["sp500".into(), "s&p".into(), "s&p 500".into()],
        "DKNG" => vec!["dkng".into(), "draftkings".into()],
        "CORN" => vec!["corn".into()],
        "WHEAT" => vec!["wheat".into()],
        "TTF" => vec!["ttf".into(), "dutch ttf".into(), "gas".into()],
        "BRENTOIL" => vec!["brentoil".into(), "brent".into(), "oil".into()],
        "DXY" => vec!["dxy".into(), "dollar index".into()],
        "GBP" => vec!["gbp".into(), "pound".into()],
        "KRW" => vec!["krw".into(), "korean won".into()],
        _ => vec![stripped.to_lowercase()],
    }
}

fn build_keyword_map(extra: &HashMap<String, Vec<String>>) -> HashMap<String, Vec<String>> {
    let xyz = vec![
            "xyz:XYZ100","xyz:TSLA","xyz:NVDA","xyz:GOLD","xyz:HOOD","xyz:INTC","xyz:PLTR","xyz:COIN","xyz:META","xyz:AAPL","xyz:MSFT","xyz:ORCL","xyz:GOOGL","xyz:AMZN","xyz:AMD","xyz:MU","xyz:SNDK","xyz:MSTR","xyz:CRCL","xyz:NFLX","xyz:COST","xyz:LLY","xyz:SKHX","xyz:TSM","xyz:JPY","xyz:EUR","xyz:SILVER","xyz:RIVN","xyz:BABA","xyz:CL","xyz:COPPER","xyz:NATGAS","xyz:URANIUM","xyz:ALUMINIUM","xyz:SMSN","xyz:PLATINUM","xyz:USAR","xyz:CRWV","xyz:URNM","xyz:PALLADIUM","xyz:DXY","xyz:GME","xyz:KR200","xyz:SOFTBANK","xyz:JP225","xyz:HYUNDAI","xyz:KIOXIA","xyz:EWY","xyz:EWJ","xyz:BRENTOIL","xyz:VIX","xyz:HIMS","xyz:SP500","xyz:DKNG","xyz:LITE","xyz:CORN","xyz:XLE","xyz:WHEAT","xyz:TTF","xyz:BX","xyz:PURRDAT","xyz:MRVL","xyz:RKLB","xyz:BIRD","xyz:VOL","xyz:DRAM","xyz:CBRS","xyz:EWZ","xyz:KRW","xyz:ZM","xyz:EBAY","xyz:H100","xyz:NIFTY","xyz:ARM","xyz:EWT","xyz:GBP","xyz:SPCX","xyz:IBOV","xyz:ASML","xyz:MINIMAX","xyz:BB","xyz:QNT","xyz:DELL","xyz:IBM","xyz:AVGO","xyz:NOW","xyz:NBIS","xyz:WDC","xyz:NOK","xyz:SMH","xyz:BE","xyz:ZHIPU","xyz:QCOM","xyz:STRC","xyz:BOT","xyz:AMAT","xyz:IBIDEN","xyz:GIGADEV","xyz:SHAZ","xyz:SKHY","xyz:KSTR","xyz:CXMT","xyz:GEV","xyz:KORU","xyz:UNITREE","xyz:LYTE","xyz:NCLD","xyz:SOXL",
    ];
    let native = vec![
            "BTC","ETH","SOL","AVAX","BNB","DOGE","XRP","ADA","LINK","UNI","ATOM","MATIC","LTC","ARB","OP","APT","SUI","ENA","WIF","PEPE","SHIB"
    ];
    let mut all_markets: Vec<String> = Vec::new();
    for m in xyz { all_markets.push(m.to_string()); }
    for m in native { all_markets.push(m.to_string()); }

    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for m in all_markets {
        let mut kws = builtin_aliases(&m);
        let lower = m.to_lowercase();
        if !kws.contains(&lower) {
            kws.push(lower);
        }
        let stripped = m.split(':').next_back().unwrap_or(&m).to_lowercase();
        if !kws.contains(&stripped) {
            kws.push(stripped);
        }
        let mut uniq: Vec<String> = Vec::new();
        for k in kws {
            let kl = k.to_lowercase();
            if !uniq.contains(&kl) {
                uniq.push(kl);
            }
        }
        if let Some(extra_vals) = extra.get(&m) {
            for e in extra_vals {
                let el = e.to_lowercase();
                if !uniq.contains(&el) {
                    uniq.push(el);
                }
            }
        }
        map.insert(m, uniq);
    }
    for (k, vals) in extra {
        if !map.contains_key(k) {
            let mut kws = builtin_aliases(k);
            for v in vals {
                let vl = v.to_lowercase();
                if !kws.contains(&vl) {
                    kws.push(vl);
                }
            }
            map.insert(k.clone(), kws);
        }
    }
    map
}

pub struct News {
    store: Store,
    keyword_map: HashMap<String, Vec<String>>,
    fetcher: Arc<dyn HttpFetch>,
    rss_urls: Vec<String>,
    tavily_last: tokio::sync::Mutex<HashMap<String, i64>>,
}

impl News {
    pub fn new(store: Store, cfg: NewsCfg) -> Self {
        let fetcher: Arc<dyn HttpFetch> = Arc::new(ReqwestFetcher::new(cfg.tavily_key_env.clone()));
        Self::new_with_fetcher(store, cfg, fetcher)
    }

    pub fn new_with_fetcher(store: Store, cfg: NewsCfg, fetcher: Arc<dyn HttpFetch>) -> Self {
        let keyword_map = build_keyword_map(&cfg.extra_keywords);
        Self {
            store,
            keyword_map,
            fetcher,
            rss_urls: cfg.rss,
            tavily_last: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Test helper: manually set last-fetch timestamp for a market (to age cooldown in tests).
    #[cfg(test)]
    pub async fn set_tavily_last_for_test(&self, market: &str, ts: i64) {
        let mut guard = self.tavily_last.lock().await;
        guard.insert(market.to_string(), ts);
    }

    pub fn match_markets(&self, text: &str) -> Vec<String> {
        let lower = text.to_lowercase();
        let mut matched = Vec::new();
        for (market, kws) in &self.keyword_map {
            for kw in kws {
                if lower.contains(kw) {
                    matched.push(market.clone());
                    break;
                }
            }
        }
        matched.sort();
        matched
    }

    pub async fn ingest(
        &self,
        source: &str,
        ts: i64,
        title: &str,
        body: &str,
        url: &str,
    ) -> Result<IngestResult, NewsError> {
        let mut hasher = Sha256::new();
        hasher.update(title.as_bytes());
        hasher.update(url.as_bytes());
        let hash = hex_encode(&hasher.finalize());

        let now = chrono::Utc::now().timestamp_millis();
        let effective_ts = if ts == 0 { now } else { ts };
        let cutoff = effective_ts - 48 * 60 * 60 * 1000;

        let existing: Option<(String,)> =
            sqlx::query_as("SELECT hash FROM news_seen WHERE hash = ?1 AND ts > ?2")
                .bind(&hash)
                .bind(cutoff)
                .fetch_optional(self.store.pool())
                .await
                .map_err(|e| NewsError::Store(e.to_string()))?;

        if existing.is_some() {
            let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM news WHERE title = ?1 AND url = ?2 LIMIT 1")
                .bind(title)
                .bind(url)
                .fetch_optional(self.store.pool())
                .await
                .map_err(|e| NewsError::Store(e.to_string()))?;
            let id = row.map(|r| r.0).unwrap_or(0);
            return Ok(IngestResult { id, deduped: true });
        }

        let combined = format!("{title} {body}");
        let markets = self.match_markets(&combined);

        let res = sqlx::query("INSERT INTO news (ts, source, title, body, url) VALUES (?1, ?2, ?3, ?4, ?5)")
            .bind(effective_ts)
            .bind(source)
            .bind(title)
            .bind(body)
            .bind(url)
            .execute(self.store.pool())
            .await
            .map_err(|e| NewsError::Store(e.to_string()))?;
        let id = res.last_insert_rowid();

        for m in &markets {
            let _ = sqlx::query("INSERT OR IGNORE INTO news_markets (news_id, market) VALUES (?1, ?2)")
                .bind(id)
                .bind(m)
                .execute(self.store.pool())
                .await
                .map_err(|e| NewsError::Store(e.to_string()))?;
        }

        sqlx::query("INSERT OR REPLACE INTO news_seen (hash, ts) VALUES (?1, ?2)")
            .bind(&hash)
            .bind(effective_ts)
            .execute(self.store.pool())
            .await
            .map_err(|e| NewsError::Store(e.to_string()))?;

        Ok(IngestResult { id, deduped: false })
    }

    pub async fn tavily_recent(&self, market: &str) -> Result<Vec<NewsItem>, NewsError> {
        // per-market cooldown: skip if same market fetched within COOLDOWN_MS
        let now = chrono::Utc::now().timestamp_millis();
        {
            let guard = self.tavily_last.lock().await;
            if let Some(last) = guard.get(market)
                && now - *last < COOLDOWN_MS
            {
                let remaining = (COOLDOWN_MS - (now - *last)) / 1000;
                debug!(market=%market, remaining_s=remaining, "tavily cooldown skip");
                return Ok(Vec::new());
            }
        }
        // record fetch time optimistically (window starts at fetch start; prevents parallel duplicate fetches)
        {
            let mut guard = self.tavily_last.lock().await;
            guard.insert(market.to_string(), now);
        }

        let stripped = market.split(':').next_back().unwrap_or(market);
        let aliases = builtin_aliases(market);
        let alias_part = aliases.iter().find(|a| a.to_lowercase() != stripped.to_lowercase() && a.to_lowercase() != market.to_lowercase()).cloned().unwrap_or_default();
        let query = if alias_part.is_empty() {
            format!("{stripped} news")
        } else {
            format!("{stripped} {alias_part} news")
        };

        let text = match self.fetcher.fetch_tavily(&query).await {
            Ok(t) => t,
            Err(e) => {
                debug!(error=%e, "tavily fetch failed");
                return Ok(Vec::new());
            }
        };

        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| NewsError::Parse(e.to_string()))?;
        let results = v.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();

        let mut out = Vec::new();
        for item in results.iter().take(8) {
            let title = item.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let body = item.get("content").or_else(|| item.get("body")).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let url = item.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let ts_raw = item.get("published_date").or_else(|| item.get("publishedDate")).and_then(|x| x.as_str()).unwrap_or("");
            let ts = if ts_raw.is_empty() {
                chrono::Utc::now().timestamp_millis()
            } else {
                chrono::DateTime::parse_from_rfc3339(ts_raw)
                    .map(|dt| dt.timestamp_millis())
                    .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis())
            };
            if title.is_empty() && url.is_empty() {
                continue;
            }
            match self.ingest("tavily", ts, &title, &body, &url).await {
                Ok(res) => {
                    let markets = self.match_markets(&format!("{title} {body}"));
                    out.push(NewsItem {
                        id: res.id,
                        ts,
                        source: "tavily".to_string(),
                        title: title.clone(),
                        body: body.clone(),
                        url: url.clone(),
                        markets,
                    });
                }
                Err(e) => {
                    debug!(error=%e, "tavily ingest failed");
                }
            }
        }
        Ok(out)
    }

    /// Recent news items matched to a market (join via news_markets) since `since_ms`, newest first.
    /// Used to feed the analyst per spec §5 (last-6h matched news, <= limit items).
    pub async fn matched_recent(&self, market: &str, since_ms: i64, limit: i64) -> Result<Vec<NewsItem>, NewsError> {
        let rows = sqlx::query_as::<_, (i64, i64, String, String, String, String)>(
            "SELECT n.id, n.ts, n.source, n.title, n.body, n.url FROM news n \
             JOIN news_markets nm ON nm.news_id = n.id \
             WHERE nm.market = ?1 AND n.ts > ?2 ORDER BY n.ts DESC LIMIT ?3",
        )
        .bind(market)
        .bind(since_ms)
        .bind(limit)
        .fetch_all(self.store.pool())
        .await
        .map_err(|e| NewsError::Store(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|(id, ts, source, title, body, url)| NewsItem {
                id,
                ts,
                source,
                title: title.clone(),
                body: body.clone(),
                url: url.clone(),
                markets: self.match_markets(&format!("{title} {body}")),
            })
            .collect())
    }

    pub fn spawn_rss(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
            loop {
                interval.tick().await;
                for url in self.rss_urls.clone() {
                    let text = match self.fetcher.fetch_get(&url).await {
                        Ok(t) => t,
                        Err(e) => {
                            debug!(error=%e, url=%url, "rss fetch failed");
                            continue;
                        }
                    };
                    match parse_rss_text(&text) {
                        Ok(entries) => {
                            for e in entries {
                                let ts = e.published.map(|dt| dt.timestamp_millis()).unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                                let title = e.title.unwrap_or_default();
                                let body = e.summary.unwrap_or_default();
                                let link = e.link.unwrap_or_default();
                                if let Err(err) = self.ingest("rss", ts, &title, &body, &link).await {
                                    debug!(error=%err, "rss ingest failed");
                                }
                            }
                        }
                        Err(e) => {
                            warn!(error=%e, "rss parse failed");
                        }
                    }
                }
            }
        })
    }
}

struct RssEntry {
    title: Option<String>,
    summary: Option<String>,
    link: Option<String>,
    published: Option<chrono::DateTime<chrono::Utc>>,
}

fn parse_rss_text(text: &str) -> Result<Vec<RssEntry>, String> {
    let feed = feed_rs::parser::parse(text.as_bytes()).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for entry in feed.entries {
        let title = entry.title.map(|t| t.content);
        let summary = entry.summary.map(|s| s.content).or_else(|| entry.content.and_then(|c| c.body));
        let link = entry.links.first().map(|l| l.href.clone());
        let published = entry.published;
        out.push(RssEntry {
            title,
            summary,
            link,
            published,
        });
    }
    Ok(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    struct MockFetcher {
        get_response: String,
        tavily_response: String,
    }

    impl HttpFetch for MockFetcher {
        fn fetch_get<'a>(&'a self, _url: &'a str) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
            let s = self.get_response.clone();
            Box::pin(async move { Ok(s) })
        }
        fn fetch_tavily<'a>(&'a self, _query: &'a str) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
            let s = self.tavily_response.clone();
            Box::pin(async move { Ok(s) })
        }
    }

    #[tokio::test]
    async fn dedupe_second_is_deduped() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: "".into(), tavily_response: "".into() });
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let r1 = news.ingest("test", 1_700_000_000_000, "Hello", "body", "http://example.com").await.expect("ingest1");
        assert!(!r1.deduped, "first not deduped");
        let r2 = news.ingest("test", 1_700_000_000_100, "Hello", "body", "http://example.com").await.expect("ingest2");
        assert!(r2.deduped, "second should be deduped");
        let r3 = news.ingest("test", 1_700_000_000_200, "Hello", "body", "http://example.com/2").await.expect("ingest3");
        assert!(!r3.deduped);
    }

    #[tokio::test]
    async fn matched_recent_filters_market_and_window() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: "".into(), tavily_response: "".into() });
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let t0 = chrono::Utc::now().timestamp_millis();
        news.ingest("test", t0 - 1000, "Tesla beats delivery estimates", "Tesla deliveries beat", "http://a/1").await.expect("ingest tsla");
        news.ingest("test", t0 - 2000, "Gold surges on safe haven", "gold rally", "http://a/2").await.expect("ingest gold");
        news.ingest("test", t0 - 8 * 60 * 60 * 1000, "Tesla old news", "stale", "http://a/3").await.expect("ingest stale");
        let six_h_ago = t0 - 6 * 60 * 60 * 1000;
        let items = news.matched_recent("xyz:TSLA", six_h_ago, 10).await.expect("matched");
        assert_eq!(items.len(), 1, "only fresh TSLA item, got {items:?}");
        assert_eq!(items[0].title, "Tesla beats delivery estimates");
        assert!(items[0].markets.contains(&"xyz:TSLA".to_string()));
        let gold_items = news.matched_recent("xyz:GOLD", six_h_ago, 10).await.expect("gold");
        assert_eq!(gold_items.len(), 1);
        assert_eq!(gold_items[0].title, "Gold surges on safe haven");
    }

    #[tokio::test]
    async fn matcher_hits() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: "".into(), tavily_response: "".into() });
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let hits = news.match_markets("Tesla beats delivery estimates");
        assert!(hits.contains(&"xyz:TSLA".to_string()), "expected xyz:TSLA in {hits:?}");
        let hits2 = news.match_markets("Bitcoin ETF inflows");
        assert!(hits2.contains(&"BTC".to_string()), "expected BTC got {hits2:?}");
        let hits3 = news.match_markets("Gold surges on safe haven");
        assert!(hits3.contains(&"xyz:GOLD".to_string()));
        let hits4 = news.match_markets("TESLA");
        assert!(hits4.contains(&"xyz:TSLA".to_string()));
    }

    #[test]
    fn directional_calls_require_direction_and_trade_level() {
        assert!(is_directional_call("SOL LONG entry 90.16 sl 89.85 tp 91.75"));
        assert!(!is_directional_call("cons tp reached gg, move sl to entry"));
        assert!(!is_directional_call("bts of this setup, used standard deviations and volume profile"));
        assert!(is_directional_call("eth long 2500 target 2700"));
        assert!(is_directional_call("BTC buy entry 103450.5"));
        assert!(!is_directional_call("SOL long looks strong"));
    }

    #[tokio::test]
    async fn rss_atom_fixture_parse() {
        let atom = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Example Feed</title>
  <entry>
    <title>Bitcoin rallies</title>
    <link href="http://example.com/btc"/>
    <id>1</id>
    <updated>2026-08-08T00:00:00Z</updated>
    <summary>BTC up 5%</summary>
  </entry>
  <entry>
    <title>Tesla earnings</title>
    <link href="http://example.com/tsla"/>
    <id>2</id>
    <updated>2026-08-08T01:00:00Z</updated>
    <summary>Tesla beats</summary>
  </entry>
</feed>
"#;
        let entries = parse_rss_text(atom).expect("parse");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title.as_deref().unwrap(), "Bitcoin rallies");
        assert_eq!(entries[1].title.as_deref().unwrap(), "Tesla earnings");
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec!["http://example.com/rss".into()], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: atom.to_string(), tavily_response: "".into() });
        let news = Arc::new(News::new_with_fetcher(store, cfg, fetcher.clone()));
        let text = fetcher.fetch_get("http://example.com/rss").await.expect("fetch");
        let parsed = parse_rss_text(&text).expect("parse2");
        assert_eq!(parsed.len(), 2);
        for e in parsed {
            let ts = e.published.map(|d| d.timestamp_millis()).unwrap_or(0);
            news.ingest("rss", ts, &e.title.unwrap_or_default(), &e.summary.unwrap_or_default(), &e.link.unwrap_or_default()).await.expect("ingest");
        }
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM news")
            .fetch_one(news.store.pool())
            .await
            .expect("count");
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn tavily_recent_ingests() {
        let tavily_json = serde_json::json!({
            "results": [
                {"title": "Tesla up", "content": "Tesla shares rise", "url": "http://example.com/tesla1", "published_date": "2026-08-08T00:00:00Z"},
                {"title": "Random", "content": "Unrelated", "url": "http://example.com/rand", "published_date": "2026-08-08T00:00:00Z"}
            ]
        })
        .to_string();
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "TAVILY_API_KEY".into(), extra_keywords: HashMap::new() };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: "".into(), tavily_response: tavily_json });
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let items = news.tavily_recent("xyz:TSLA").await.expect("tavily");
        assert_eq!(items.len(), 2);
        assert!(items[0].title.contains("Tesla"));
    }

    #[tokio::test]
    async fn extra_keywords_match() {
        let mut extra = HashMap::new();
        extra.insert("xyz:TSLA".to_string(), vec!["elon".to_string()]);
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: extra };
        let fetcher: Arc<dyn HttpFetch> = Arc::new(MockFetcher { get_response: "".into(), tavily_response: "".into() });
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let hits = news.match_markets("Elon announces new model");
        assert!(hits.contains(&"xyz:TSLA".to_string()));
    }

    // ── per-market tavily cooldown (COMMIT 3) ──

    struct CountingFetcher {
        tavily_response: String,
        hit: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl HttpFetch for CountingFetcher {
        fn fetch_get<'a>(&'a self, _url: &'a str) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
            Box::pin(async move { Ok(String::new()) })
        }
        fn fetch_tavily<'a>(&'a self, _query: &'a str) -> Pin<Box<dyn Future<Output = Result<String, NewsError>> + Send + 'a>> {
            let s = self.tavily_response.clone();
            let h = self.hit.clone();
            Box::pin(async move {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(s)
            })
        }
    }

    #[tokio::test]
    async fn tavily_cooldown_same_market_second_skipped() {
        let tavily_json = serde_json::json!({
            "results": [
                {"title": "Tesla up", "content": "Tesla shares rise", "url": "http://example.com/tesla1", "published_date": "2026-08-08T00:00:00Z"}
            ]
        }).to_string();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fetcher: Arc<dyn HttpFetch> = Arc::new(CountingFetcher { tavily_response: tavily_json, hit: hits.clone() });
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let first = news.tavily_recent("xyz:TSLA").await.expect("first");
        assert_eq!(first.len(), 1, "first should fetch");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        let second = news.tavily_recent("xyz:TSLA").await.expect("second");
        assert!(second.is_empty(), "second immediate same market must be cooldown empty, got {second:?}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "fetcher hit count must stay 1 after cooldown skip");
    }

    #[tokio::test]
    async fn tavily_cooldown_different_market_fetches() {
        let tavily_json = serde_json::json!({
            "results": [
                {"title": "News", "content": "body", "url": "http://example.com/1", "published_date": "2026-08-08T00:00:00Z"}
            ]
        }).to_string();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fetcher: Arc<dyn HttpFetch> = Arc::new(CountingFetcher { tavily_response: tavily_json, hit: hits.clone() });
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let a = news.tavily_recent("xyz:TSLA").await.expect("a");
        assert_eq!(a.len(), 1);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        let b = news.tavily_recent("BTC").await.expect("b");
        assert_eq!(b.len(), 1, "different market must fetch despite cooldown on first");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn tavily_cooldown_aged_refetches() {
        let tavily_json = serde_json::json!({
            "results": [
                {"title": "Tesla up", "content": "Tesla shares rise", "url": "http://example.com/tesla1", "published_date": "2026-08-08T00:00:00Z"},
                {"title": "Tesla 2", "content": "more", "url": "http://example.com/tesla2", "published_date": "2026-08-08T01:00:00Z"}
            ]
        }).to_string();
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fetcher: Arc<dyn HttpFetch> = Arc::new(CountingFetcher { tavily_response: tavily_json.clone(), hit: hits.clone() });
        let store = Store::open("sqlite::memory:").await.expect("store");
        let cfg = NewsCfg { rss: vec![], tavily_key_env: "".into(), extra_keywords: HashMap::new() };
        let news = News::new_with_fetcher(store, cfg, fetcher);
        let first = news.tavily_recent("xyz:TSLA").await.expect("first");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // manually age the timestamp to bypass cooldown (provide way to inject/age clock)
        let aged_ts = chrono::Utc::now().timestamp_millis() - super::COOLDOWN_MS - 1_000;
        news.set_tavily_last_for_test("xyz:TSLA", aged_ts).await;
        let second = news.tavily_recent("xyz:TSLA").await.expect("second after aged");
        // second should have refetched (hit count 2). Even though deduped ingest may cause empty Vec, we verify fetcher was hit.
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2, "aged cooldown must allow refetch");
        // behavior on cooldown skip is Ok(empty) — we also verify that hits tracked
        let _ = (first, second);
    }
}
