#![allow(dead_code)]

use std::collections::HashMap;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct CtxRow {
    pub market: String,
    pub mark: f64,
    pub oracle: f64,
    pub mid: f64,
    pub funding: f64,
    pub open_interest: f64,
    pub day_ntl_vlm: f64,
    pub prev_day_px: f64,
}

#[allow(non_snake_case)]
#[derive(Debug, Clone, PartialEq)]
pub struct Candle {
    pub t: i64,
    pub T: i64,
    pub s: String,
    pub i: String,
    pub o: f64,
    pub c: f64,
    pub h: f64,
    pub l: f64,
    pub v: f64,
    pub n: i64,
}

/// Per-market candle cache entry (main.rs `candle_cache`): 15m intraday bars + 4h context
/// bars, both oldest→newest as `candle_snapshot` returns them, plus the fetch stamp the
/// >30min staleness gate reads before the bars are allowed into an analyst prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketCandles {
    pub m15: Vec<Candle>,
    pub h4: Vec<Candle>,
    pub fetched_ms: i64,
}

#[derive(Debug, Error)]
pub enum HlError {
    #[error("http error: {0}")]
    Http(String),
    #[error("json error: {0}")]
    Json(String),
    #[error("parse error field {field} value {value}: {why}")]
    Parse {
        field: String,
        value: String,
        why: String,
    },
    #[error("unexpected response shape: {0}")]
    Shape(String),
}

#[derive(Clone)]
pub struct HlRest {
    base: String,
    client: reqwest::Client,
}

impl HlRest {
    /// Hyperliquid mainnet REST base — the daemon's only venue, and the backtest harness's
    /// only data source.
    pub const MAINNET: &'static str = "https://api.hyperliquid.xyz";

    /// Every request is bounded. Without a timeout, one hung TCP connection to
    /// `api.hyperliquid.xyz` stalls its caller forever — the hourly refresh and the 30s
    /// ctx poll would simply stop ticking with nothing in the log to show for it.
    pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    pub fn new(base: &str) -> Self {
        let client = reqwest::Client::builder()
            .user_agent("kestreld/0.1")
            .timeout(Self::REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client build");
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        }
    }

    pub async fn meta_and_ctxs(&self, dex: Option<&str>) -> Result<Vec<CtxRow>, HlError> {
        let url = format!("{}/info", self.base);
        let mut body = serde_json::json!({"type":"metaAndAssetCtxs"});
        if let Some(d) = dex
            && !d.is_empty()
        {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        parse_meta_and_ctxs(&text)
    }

    pub async fn all_mids(&self, dex: Option<&str>) -> Result<HashMap<String, f64>, HlError> {
        let url = format!("{}/info", self.base);
        let mut body = serde_json::json!({"type":"allMids"});
        if let Some(d) = dex
            && !d.is_empty()
        {
            body["dex"] = serde_json::Value::String(d.to_string());
        }
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        parse_all_mids(&text)
    }

    /// Ordered dex-prefixed market names for positional allDexsAssetCtxs mapping.
    /// Source: HlRest::meta_and_ctxs universe ordering (hl_rest.rs already pairs
    /// universe[i] with ctxs[i]). Seeded from the existing 30s poll path — main.rs
    /// populates a HashMap<String, Vec<String>> via this helper / fetch_universe.
    pub async fn universe_names(&self, dex: Option<&str>) -> Result<Vec<String>, HlError> {
        let mut rows = self.meta_and_ctxs(dex).await?;
        // Ensure xyz markets are dex-prefixed even if server ever returns bare names.
        if let Some(d) = dex
            && !d.is_empty()
        {
            for r in &mut rows {
                if !r.market.contains(':') {
                    r.market = format!("{d}:{}", r.market);
                }
            }
        }
        Ok(rows.into_iter().map(|r| r.market).collect())
    }

    /// Fetch hourly funding history for `coin`, inclusive window [start_ms, end_ms].
    /// Live verification (2026-08-08): coin MUST be dex-prefixed for HIP-3
    /// (e.g. "xyz:TSLA" returns array; bare "TSLA" returns `null`). Sample live curl:
    /// ```text
    /// curl -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
    ///   -d '{"type":"fundingHistory","coin":"xyz:TSLA","startTime":1785608186000,"endTime":1786212986000}'
    /// # -> [{"coin":"xyz:TSLA","fundingRate":"-0.000034125","premium":"-0.0008459994","time":1785610800030}, ...]
    /// curl -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
    ///   -d '{"type":"fundingHistory","coin":"TSLA","startTime":...,"endTime":...}'
    /// # -> null
    /// curl -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
    ///   -d '{"type":"fundingHistory","coin":"BTC","startTime":...,"endTime":...}'
    /// # -> [{"coin":"BTC","fundingRate":"0.0000125","premium":"-0.0003778671","time":1786208400045}, ...]
    /// # empty window -> []
    /// ```
    /// Response rows have STRING fundingRate/premium and numeric time; coin mirrors input.
    pub async fn funding_history(
        &self,
        coin: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<(i64, f64)>, HlError> {
        let url = format!("{}/info", self.base);
        let body = serde_json::json!({
            "type": "fundingHistory",
            "coin": coin,
            "startTime": start_ms,
            "endTime": end_ms,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        parse_funding_history(&text)
    }

    /// Fetch 15m (or other interval) candles for `coin` in window [start_ms, end_ms].
    /// Live verification 2026-08-09 — coin naming MUST be dex-prefixed for HIP-3:
    /// ```text
    /// # xyz:TSLA returns array of candles
    /// curl -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
    ///   -d '{"type":"candleSnapshot","req":{"coin":"xyz:TSLA","interval":"15m","startTime":1786192525000,"endTime":1786214125000}}'
    /// # -> [{"t":1786192200000,"T":1786193099999,"s":"xyz:TSLA","i":"15m","o":"329.51","c":"329.55","h":"329.58","l":"329.47","v":"43.237","n":38}, ...]
    /// # bare TSLA returns null (same as fundingHistory HIP-3 rule)
    /// curl -X POST https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
    ///   -d '{"type":"candleSnapshot","req":{"coin":"TSLA","interval":"15m","startTime":...,"endTime":...}}'
    /// # -> null
    /// # BTC (native) returns array with same shape, fields t,T,s,i,o,c,h,l,v,n as string-numbers
    /// # empty window -> []
    /// ```
    /// Fields arrive short-named; `t,T,n` are integers, `o,c,h,l,v` are string-numbers
    /// but we parse defensively accepting either string or numeric for all.
    pub async fn candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Candle>, HlError> {
        let url = format!("{}/info", self.base);
        let body = serde_json::json!({
            "type": "candleSnapshot",
            "req": {
                "coin": coin,
                "interval": interval,
                "startTime": start_ms,
                "endTime": end_ms,
            }
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        let text = resp
            .text()
            .await
            .map_err(|e| HlError::Http(e.to_string()))?;
        parse_candle_snapshot(&text)
    }
}

fn parse_f64(field: &str, v: &str) -> Result<f64, HlError> {
    v.parse::<f64>().map_err(|e| HlError::Parse {
        field: field.to_string(),
        value: v.to_string(),
        why: e.to_string(),
    })
}

fn parse_field_f64(c: &serde_json::Value, field: &str) -> Result<f64, HlError> {
    let v = c
        .get(field)
        .ok_or_else(|| HlError::Shape(format!("missing {field}")))?;
    match v {
        serde_json::Value::String(s) => parse_f64(field, s),
        serde_json::Value::Null => Ok(0.0),
        serde_json::Value::Number(n) => n.as_f64().ok_or_else(|| HlError::Parse {
            field: field.to_string(),
            value: n.to_string(),
            why: "invalid number".to_string(),
        }),
        _ => Err(HlError::Shape(format!("unexpected type for {field}"))),
    }
}

fn parse_meta_and_ctxs(text: &str) -> Result<Vec<CtxRow>, HlError> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| HlError::Json(e.to_string()))?;
    // expected [ {universe:[...]}, [ctx,...] ]
    let arr = val
        .as_array()
        .ok_or_else(|| HlError::Shape("expected top-level array".into()))?;
    if arr.len() != 2 {
        return Err(HlError::Shape(format!("expected array len2 got {}", arr.len())));
    }
    let meta = &arr[0];
    let ctxs = &arr[1];

    let universe = meta
        .get("universe")
        .and_then(|v| v.as_array())
        .ok_or_else(|| HlError::Shape("missing universe array".into()))?;
    let ctx_arr = ctxs
        .as_array()
        .ok_or_else(|| HlError::Shape("expected ctxs array".into()))?;

    if universe.len() != ctx_arr.len() {
        return Err(HlError::Shape(format!(
            "universe len {} != ctx len {}",
            universe.len(),
            ctx_arr.len()
        )));
    }

    let mut out = Vec::with_capacity(universe.len());
    for (u, c) in universe.iter().zip(ctx_arr.iter()) {
        let name = u
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HlError::Shape("missing name".into()))?
            .to_string();

        let row = CtxRow {
            market: name,
            mark: parse_field_f64(c, "markPx")?,
            oracle: parse_field_f64(c, "oraclePx")?,
            mid: parse_field_f64(c, "midPx")?,
            funding: parse_field_f64(c, "funding")?,
            open_interest: parse_field_f64(c, "openInterest")?,
            day_ntl_vlm: parse_field_f64(c, "dayNtlVlm")?,
            prev_day_px: parse_field_f64(c, "prevDayPx")?,
        };
        out.push(row);
    }
    Ok(out)
}

fn parse_all_mids(text: &str) -> Result<HashMap<String, f64>, HlError> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| HlError::Json(e.to_string()))?;
    let obj = val
        .as_object()
        .ok_or_else(|| HlError::Shape("allMids expected object".into()))?;
    let mut map = HashMap::with_capacity(obj.len());
    for (k, v) in obj {
        let s = v
            .as_str()
            .ok_or_else(|| HlError::Shape(format!("value for {k} not string")))?;
        let f = parse_f64(k, s)?;
        map.insert(k.clone(), f);
    }
    Ok(map)
}

/// Merge ctx rows into snapshot markets — update-only (no inserts).
/// The ws `allDexsAssetCtxs` fan may UPDATE ctx fields (funding, open_interest,
/// day_ntl_vlm, mark, oracle, prev_day_px) of markets ALREADY present in the
/// filtered-universe snapshot, but must NEVER insert a market that isn't already
/// there. Membership is written only by the universe bootstrap/hourly refresh/
/// 30s REST poll. This keeps `markets_tracked` equal to the filtered universe
/// count (native vlm ≥500k / xyz vlm ≥200k) instead of ballooning to unfiltered
/// size (observed regression: 128→340 when fan inserted every ws market).
/// `mid` is intentionally not touched — authoritative from the mids feed.
/// `features` are updated separately via `FeatureEngine` (outside this pure helper).
pub fn merge_ctx_rows(
    snap_markets: &mut [crate::contracts::MarketRow],
    rows: &[CtxRow],
) {
    // O(n+m) via index map; builds String keys to avoid borrow conflicts with &mut.
    let mut pos_by_market: HashMap<String, usize> = HashMap::with_capacity(snap_markets.len());
    for (idx, m) in snap_markets.iter().enumerate() {
        pos_by_market.insert(m.market.clone(), idx);
    }
    for row in rows {
        if let Some(&idx) = pos_by_market.get(&row.market) {
            let e = &mut snap_markets[idx];
            e.funding = row.funding;
            e.open_interest = row.open_interest;
            e.day_ntl_vlm = row.day_ntl_vlm;
            e.mark = row.mark;
            e.oracle = row.oracle;
            e.prev_day_px = row.prev_day_px;
        }
    }
}

/// Re-apply the live snapshot's mids onto a freshly rebuilt market vector.
///
/// Callers must not hold the snapshot lock while they talk to the feature engine, the
/// store or the network — that ABBA inversion is what wedged `/api/snapshot` and
/// `/api/positions` on 2026-08-09. The price of that discipline is a read → compute →
/// write split, during which the mids fan may have landed a newer mid.
///
/// This helper closes that window for the **30s ctx poll**, whose semantics are "keep the
/// snapshot's mid, let REST fill only zeros" (`entry.mid == 0.0`) — called INSIDE its
/// write guard (pure, no awaits) it makes the split as exact as the old single-guard
/// merge. The universe rebuilds (bootstrap / hourly refresh) deliberately do NOT use it:
/// replacing mids wholesale from REST is their hourly heal for a market whose ws mid
/// stopped ticking.
///
/// Only `mid` is overlaid: `mark`/`oracle`/ctx fields belong to the REST/ctx sources being
/// written, and the next mids frame re-derives `mark` from `mid` anyway. A live mid of
/// `0.0` or non-finite is ignored (never overwrite a real price with a placeholder).
pub fn overlay_live_mids(
    markets: &mut [crate::contracts::MarketRow],
    live: &[crate::contracts::MarketRow],
) {
    let live_mids: HashMap<&str, f64> = live
        .iter()
        .filter(|m| m.mid > 0.0 && m.mid.is_finite())
        .map(|m| (m.market.as_str(), m.mid))
        .collect();
    for m in markets.iter_mut() {
        if let Some(&mid) = live_mids.get(m.market.as_str()) {
            m.mid = mid;
        }
    }
}

/// Position markets are always tracked — UNION of filtered universe ∪ open positions.
///
/// Rationale: positions opened before a filter change (e.g. min_vlm_native raised
/// from 500k to 2M) must keep live marks for equity/triggers/veto_close. Before this
/// fix, after raising min_vlm_native=2M/min_vlm_dex=500k markets like INIT, NIL,
/// xyz:LYTE were filtered OUT of the universe snapshot while 5 open positions still
/// referred to them. Consequence: /api/snapshot lacked those rows, /api/positions
/// mark_px fell back to entry ($0.00/0.00%), equity() marks map from snapshot missed
/// them → unrealized PnL computed at entry_px, and apply_review_action's veto_close
/// mark (from snapshot) would fill at entry instead of true mark. The snapshot must
/// therefore be the UNION of (vlm-filtered universe) and (markets of open positions).
/// Extra rows enter AFTER filtering (not subject to vlm), seeded with mid from the
/// latest known — snapshot mid if present else engine last mid else ctx mid — and
/// features left as-is from engine. markets_tracked counts only the filtered universe
/// (snapshot may be larger). This logic is used by every task that rebuilds snapshot
/// membership (bootstrap, hourly refresh, 30s ctx poll map-merge) so it self-heals on
/// daemon boot without relying on position hydration order.
///
/// markets_tracked writer: each universe-refresh task (bootstrap/hourly/30s poll) sets
/// the counter to the filtered-universe len AFTER injection. The ws ctx fan and mids
/// fan never touch the counter — they only update fields of existing rows. The mids
/// fan iterates snap.markets and refreshes mids for all rows, including injected
/// position rows, automatically.
///
/// INVARIANT (source guard): snapshot rows injected for positions NEVER carry `mid == 0.0`.
/// If a position market has NO known mid in ANY source (not in existing snapshot,
/// not in engine `latest_mid()`, not in incoming `ctx_mids` / `mark` field), the
/// injection is SKIPPED this pass entirely — `mid` is never seeded as `0.0` while
/// features are `None`. The row will appear on a subsequent refresh/tick as soon as
/// ANY source has a real `mid > 0`. This prevents the bootstrap-before-ws-connect
/// window (injection happens before mids ws connects) from sampling `mid: 0.0` and
/// persisting a `3249.88`-class equity spike (short with mark `0` computes +100%
/// unrealized; long with mark `0` would false-trip the `-12%` kill latch).
pub fn ensure_position_markets(
    markets: &mut Vec<crate::contracts::MarketRow>,
    open_markets: &[String],
    snapshot_mids: &HashMap<String, f64>,
    engine_mids: &HashMap<String, f64>,
    ctx_mids: &HashMap<String, f64>,
    features_fn: impl Fn(&str) -> Option<crate::contracts::Features>,
) {
    use std::collections::HashSet;
    let present: HashSet<String> = markets.iter().map(|m| m.market.clone()).collect();
    for om in open_markets {
        if present.contains(om) {
            continue;
        }
        let mid_opt = snapshot_mids
            .get(om)
            .copied()
            .filter(|v| *v > 0.0 && v.is_finite())
            .or_else(|| engine_mids.get(om).copied().filter(|v| *v > 0.0 && v.is_finite()))
            .or_else(|| ctx_mids.get(om).copied().filter(|v| *v > 0.0 && v.is_finite()));
        let Some(mid) = mid_opt else {
            // No valid mid anywhere yet — do NOT inject a row with mid 0.0.
            // It will be injected on the next pass once any source has a real mid.
            continue;
        };
        let feats = features_fn(om);
        markets.push(crate::contracts::MarketRow {
            market: om.clone(),
            mid,
            mark: mid,
            oracle: mid,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: mid,
            features: feats,
        });
    }
}

/// Map-merge variant for the 30s ctx poll path which builds a HashMap<String,MarketRow>.
/// Same UNION rationale as ensure_position_markets — inject missing open-position
/// markets into the map AFTER merging filtered rows, seeding mid with priority
/// snapshot > engine > ctx. Keeps position rows alive even though poll only iterates
/// filtered rows.
///
/// INVARIANT (source guard): same as `ensure_position_markets` — NEVER seed `mid == 0.0`.
/// If no source has a valid `mid > 0` for a position market, the map is left without
/// that key this pass (it will be inserted on a later poll/tick once a real mid
/// appears). As a fallback, if `ctx_mids` is empty but a `CtxRow.mark > 0` exists for
/// that market in the current poll batch, callers should populate `ctx_mids` from
/// `mark` before calling; otherwise injection is deferred.
pub fn ensure_position_markets_map(
    map: &mut HashMap<String, crate::contracts::MarketRow>,
    open_markets: &[String],
    snapshot_mids: &HashMap<String, f64>,
    engine_mids: &HashMap<String, f64>,
    ctx_mids: &HashMap<String, f64>,
    features_fn: impl Fn(&str) -> Option<crate::contracts::Features>,
) {
    for om in open_markets {
        if map.contains_key(om) {
            continue;
        }
        let mid_opt = snapshot_mids
            .get(om)
            .copied()
            .filter(|v| *v > 0.0 && v.is_finite())
            .or_else(|| engine_mids.get(om).copied().filter(|v| *v > 0.0 && v.is_finite()))
            .or_else(|| ctx_mids.get(om).copied().filter(|v| *v > 0.0 && v.is_finite()));
        let Some(mid) = mid_opt else {
            // No valid mid anywhere — do NOT insert a 0.0 row; defer to next pass with real mid.
            continue;
        };
        let feats = features_fn(om);
        map.insert(
            om.clone(),
            crate::contracts::MarketRow {
                market: om.clone(),
                mid,
                mark: mid,
                oracle: mid,
                funding: 0.0,
                open_interest: 0.0,
                day_ntl_vlm: 0.0,
                prev_day_px: mid,
                features: feats,
            },
        );
    }
}

fn parse_funding_history(text: &str) -> Result<Vec<(i64, f64)>, HlError> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| HlError::Json(e.to_string()))?;
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| HlError::Shape("fundingHistory expected array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let obj = entry
            .as_object()
            .ok_or_else(|| HlError::Shape("fundingHistory entry not object".into()))?;
        let time = obj
            .get("time")
            .ok_or_else(|| HlError::Shape("missing time".into()))?;
        let ts = match time {
            serde_json::Value::Number(n) => n.as_i64().ok_or_else(|| HlError::Parse {
                field: "time".to_string(),
                value: n.to_string(),
                why: "invalid i64".to_string(),
            })?,
            serde_json::Value::String(s) => s.parse::<i64>().map_err(|e| HlError::Parse {
                field: "time".to_string(),
                value: s.clone(),
                why: e.to_string(),
            })?,
            _ => {
                return Err(HlError::Shape("unexpected type for time".into()));
            }
        };
        // fundingRate arrives as string number (e.g. "0.0000125"); accept numeric fallback too.
        let rate = parse_field_f64(entry, "fundingRate")?;
        out.push((ts, rate));
    }
    // Ensure oldest→newest order (API already returns asc; sort to guarantee).
    out.sort_by_key(|(ts, _)| *ts);
    Ok(out)
}

fn parse_field_i64(c: &serde_json::Value, field: &str) -> Result<i64, HlError> {
    let v = c
        .get(field)
        .ok_or_else(|| HlError::Shape(format!("missing {field}")))?;
    match v {
        serde_json::Value::String(s) => s.parse::<i64>().map_err(|e| HlError::Parse {
            field: field.to_string(),
            value: s.clone(),
            why: e.to_string(),
        }),
        serde_json::Value::Number(n) => n.as_i64().ok_or_else(|| HlError::Parse {
            field: field.to_string(),
            value: n.to_string(),
            why: "invalid i64".to_string(),
        }),
        serde_json::Value::Null => Ok(0),
        _ => Err(HlError::Shape(format!("unexpected type for {field}"))),
    }
}

fn parse_candle_snapshot(text: &str) -> Result<Vec<Candle>, HlError> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| HlError::Json(e.to_string()))?;
    if val.is_null() {
        return Ok(Vec::new());
    }
    let arr = val
        .as_array()
        .ok_or_else(|| HlError::Shape("candleSnapshot expected array".into()))?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let obj = entry
            .as_object()
            .ok_or_else(|| HlError::Shape("candle entry not object".into()))?;
        // t, T, n are i64; accept string or numeric. s,i are strings; o,c,h,l,v are f64 string/number.
        let t = parse_field_i64(entry, "t")?;
        let t_end = parse_field_i64(entry, "T")?;
        let s = obj
            .get("s")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HlError::Shape("missing s".into()))?
            .to_string();
        let i = obj
            .get("i")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HlError::Shape("missing i".into()))?
            .to_string();
        let o = parse_field_f64(entry, "o")?;
        let c = parse_field_f64(entry, "c")?;
        let h = parse_field_f64(entry, "h")?;
        let l = parse_field_f64(entry, "l")?;
        let v = parse_field_f64(entry, "v")?;
        let n = parse_field_i64(entry, "n")?;
        out.push(Candle {
            t,
            T: t_end,
            s,
            i,
            o,
            c,
            h,
            l,
            v,
            n,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> String {
        let manifest = env!("CARGO_MANIFEST_DIR");
        let p = format!("{manifest}/tests/fixtures/{name}");
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p}: {e}"))
    }

    #[test]
    fn parse_native_fixture_sol_exists() {
        let text = load_fixture("meta_native.json");
        let rows = parse_meta_and_ctxs(&text).expect("parse native");
        assert!(!rows.is_empty());
        let sol = rows.iter().find(|r| r.market == "SOL").expect("SOL row");
        assert!(sol.mid > 0.0, "mid {}", sol.mid);
        assert!(sol.mark > 0.0);
        assert!(sol.oracle > 0.0);
        assert!(sol.day_ntl_vlm > 0.0);
        assert!(sol.prev_day_px > 0.0);
        // funding can be small negative/positive but should be parsed
        // open interest >0
        assert!(sol.open_interest > 0.0);
    }

    #[test]
    fn parse_xyz_fixture_tsla_exists() {
        let text = load_fixture("meta_xyz.json");
        let rows = parse_meta_and_ctxs(&text).expect("parse xyz");
        assert!(!rows.is_empty());
        let tsla = rows
            .iter()
            .find(|r| r.market == "xyz:TSLA")
            .expect("xyz:TSLA row");
        assert!(tsla.mid > 0.0, "mid {}", tsla.mid);
        assert!(tsla.mark > 0.0);
        assert!(tsla.oracle > 0.0);
        assert!(tsla.day_ntl_vlm > 0.0);
        assert!(tsla.prev_day_px > 0.0);
        assert!(tsla.open_interest > 0.0);
    }

    #[test]
    fn all_mids_parse() {
        let sample = r#"{"SOL":"73.7","BTC":"64854.5","xyz:TSLA":"328.2"}"#;
        let m = parse_all_mids(sample).expect("parse");
        assert!((m["SOL"] - 73.7).abs() < 1e-9);
        assert!((m["xyz:TSLA"] - 328.2).abs() < 1e-9);
    }

    #[test]
    fn parse_string_numbers_error_has_field() {
        let err = parse_f64("funding", "not_a_number").unwrap_err();
        match err {
            HlError::Parse { field, .. } => assert_eq!(field, "funding"),
            _ => panic!("wrong error"),
        }
    }

    #[test]
    fn parse_funding_history_synthetic() {
        // Synthetic response mimicking live shape: string fundingRate, numeric time.
        let sample = r#"[{"coin":"BTC","fundingRate":"0.0000125","premium":"-0.0001","time":1700000000000},{"coin":"BTC","fundingRate":"-0.00002","premium":"0.0002","time":1700003600000}]"#;
        let rows = parse_funding_history(sample).expect("parse");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 1700000000000);
        assert!((rows[0].1 - 0.0000125).abs() < 1e-12);
        assert_eq!(rows[1].0, 1700003600000);
        assert!((rows[1].1 - (-0.00002)).abs() < 1e-12);
    }

    #[test]
    fn parse_funding_history_xyz_synthetic() {
        let sample = r#"[{"coin":"xyz:TSLA","fundingRate":"0.00000625","premium":"-0.00001","time":1785610800030},{"coin":"xyz:TSLA","fundingRate":"-0.000034125","premium":"-0.0008459994","time":1785614400033}]"#;
        let rows = parse_funding_history(sample).expect("parse xyz");
        assert_eq!(rows.len(), 2);
        assert!((rows[0].1 - 0.00000625).abs() < 1e-12);
        assert!((rows[1].1 - (-0.000034125)).abs() < 1e-12);
    }

    #[test]
    fn parse_funding_history_empty_array() {
        let rows = parse_funding_history("[]").expect("empty");
        assert!(rows.is_empty());
    }

    #[test]
    fn parse_funding_history_null_is_empty() {
        // Bare coin name returns null per live curl; treat as empty.
        let rows = parse_funding_history("null").expect("null");
        assert!(rows.is_empty());
    }

    #[test]
    fn parse_funding_history_numeric_rate_fallback() {
        // Defensive: server could send numeric fundingRate.
        let sample = r#"[{"coin":"BTC","fundingRate":0.0000125,"premium":"0","time":1700000000000}]"#;
        let rows = parse_funding_history(sample).expect("numeric rate");
        assert!((rows[0].1 - 0.0000125).abs() < 1e-12);
    }

    fn mr(market: &str, mid: f64) -> crate::contracts::MarketRow {
        crate::contracts::MarketRow {
            market: market.to_string(),
            mid,
            mark: mid,
            oracle: mid,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: mid,
            features: None,
        }
    }

    /// The 30s ctx poll computes its new market vector with NO snapshot lock held
    /// (lock-order invariant, main.rs) even though its semantics are "keep the snapshot's
    /// mid". A mids frame landing in that window would be reverted on write —
    /// `overlay_live_mids` runs inside the write guard to prevent exactly that.
    #[test]
    fn overlay_live_mids_keeps_the_newer_mid_from_the_mids_feed() {
        // rebuilt from REST (stale 100.0); live snapshot already advanced to 101.5
        let mut rebuilt = vec![mr("BTC", 100.0), mr("SOL", 50.0)];
        let live = vec![mr("BTC", 101.5), mr("SOL", 50.25)];
        overlay_live_mids(&mut rebuilt, &live);
        assert!((rebuilt[0].mid - 101.5).abs() < 1e-9, "live mid must win over rebuild mid");
        assert!((rebuilt[1].mid - 50.25).abs() < 1e-9);
    }

    #[test]
    fn overlay_live_mids_ignores_unknown_and_invalid_live_rows() {
        let mut rebuilt = vec![mr("BTC", 100.0), mr("NEW", 7.0)];
        // NEW is not in the live snapshot yet (freshly injected row) — must keep its mid.
        // ZOMBIE is gone from the universe — must not be re-added.
        // A live 0.0 / NaN mid is a placeholder and must never overwrite a real price.
        let mut live = vec![mr("BTC", 0.0), mr("ZOMBIE", 5.0)];
        live.push(mr("NAN", f64::NAN));
        overlay_live_mids(&mut rebuilt, &live);
        assert_eq!(rebuilt.len(), 2, "overlay is update-only, never inserts");
        assert!((rebuilt[0].mid - 100.0).abs() < 1e-9, "zero live mid must not clobber");
        assert!((rebuilt[1].mid - 7.0).abs() < 1e-9, "row absent from live keeps its seeded mid");
        assert!(rebuilt.iter().all(|m| m.market != "ZOMBIE"));

        let mut nan_target = vec![mr("NAN", 3.0)];
        overlay_live_mids(&mut nan_target, &live);
        assert!((nan_target[0].mid - 3.0).abs() < 1e-9, "non-finite live mid must not clobber");
    }

    #[test]
    fn overlay_live_mids_leaves_ctx_fields_to_their_owner() {
        // mark/oracle/funding belong to the REST/ctx source being written; only mid is
        // authoritative from the mids feed (same ownership rule as merge_ctx_rows).
        let mut rebuilt = vec![crate::contracts::MarketRow {
            market: "BTC".into(),
            mid: 100.0,
            mark: 100.2,
            oracle: 100.1,
            funding: 0.05,
            open_interest: 42.0,
            day_ntl_vlm: 900_000.0,
            prev_day_px: 98.0,
            features: None,
        }];
        overlay_live_mids(&mut rebuilt, &[mr("BTC", 101.0)]);
        assert!((rebuilt[0].mid - 101.0).abs() < 1e-9);
        assert!((rebuilt[0].mark - 100.2).abs() < 1e-9);
        assert!((rebuilt[0].oracle - 100.1).abs() < 1e-9);
        assert!((rebuilt[0].funding - 0.05).abs() < 1e-12);
        assert!((rebuilt[0].day_ntl_vlm - 900_000.0).abs() < 1e-9);
    }

    #[test]
    fn merge_ctx_rows_updates_existing_only() {
        use crate::contracts::MarketRow;
        let mut snap = vec![
            MarketRow {
                market: "BTC".to_string(),
                mid: 100.0,
                mark: 100.0,
                oracle: 99.0,
                funding: 0.01,
                open_interest: 1000.0,
                day_ntl_vlm: 600_000.0,
                prev_day_px: 99.5,
                features: None,
            },
            MarketRow {
                market: "SOL".to_string(),
                mid: 50.0,
                mark: 50.0,
                oracle: 49.0,
                funding: 0.02,
                open_interest: 2000.0,
                day_ntl_vlm: 700_000.0,
                prev_day_px: 49.5,
                features: None,
            },
        ];
        let rows = vec![
            CtxRow {
                market: "BTC".to_string(),
                mark: 101.0,
                oracle: 100.5,
                mid: 101.0,
                funding: 0.05,
                open_interest: 9999.0,
                day_ntl_vlm: 1_000_000.0,
                prev_day_px: 100.0,
            },
            CtxRow {
                market: "UNKNOWN".to_string(),
                mark: 999.0,
                oracle: 999.0,
                mid: 999.0,
                funding: 0.99,
                open_interest: 9999.0,
                day_ntl_vlm: 9_999_999.0,
                prev_day_px: 999.0,
            },
        ];
        let len_before = snap.len();
        merge_ctx_rows(&mut snap, &rows);
        // (a) existing market gets funding/OI updated
        let btc = snap.iter().find(|m| m.market == "BTC").expect("BTC");
        assert!((btc.funding - 0.05).abs() < 1e-12);
        assert!((btc.open_interest - 9999.0).abs() < 1e-9);
        assert!((btc.mark - 101.0).abs() < 1e-9);
        // (b) unknown market is NOT inserted
        assert!(!snap.iter().any(|m| m.market == "UNKNOWN"));
        // (c) snapshot len unchanged
        assert_eq!(snap.len(), len_before);
        // mid must NOT be changed by ctx fan (authoritative from mids feed)
        assert!((btc.mid - 100.0).abs() < 1e-9);
        // SOL unchanged
        let sol = snap.iter().find(|m| m.market == "SOL").expect("SOL");
        assert!((sol.funding - 0.02).abs() < 1e-12);
    }

    #[test]
    fn merge_ctx_rows_empty_snap_or_rows() {
        use crate::contracts::MarketRow;
        let mut snap: Vec<MarketRow> = vec![];
        let rows = vec![CtxRow {
            market: "BTC".to_string(),
            mark: 1.0,
            oracle: 1.0,
            mid: 1.0,
            funding: 0.01,
            open_interest: 1.0,
            day_ntl_vlm: 1.0,
            prev_day_px: 1.0,
        }];
        merge_ctx_rows(&mut snap, &rows);
        assert!(snap.is_empty(), "empty snapshot stays empty — no inserts");

        let mut snap2 = vec![MarketRow {
            market: "BTC".to_string(),
            mid: 10.0,
            mark: 10.0,
            oracle: 10.0,
            funding: 0.01,
            open_interest: 100.0,
            day_ntl_vlm: 100.0,
            prev_day_px: 10.0,
            features: None,
        }];
        merge_ctx_rows(&mut snap2, &[]);
        assert_eq!(snap2.len(), 1);
        assert!((snap2[0].funding - 0.01).abs() < 1e-12);
    }

    #[test]
    fn parse_candle_snapshot_synthetic_and_empty() {
        let sample = r#"[{"t":1786192200000,"T":1786193099999,"s":"xyz:TSLA","i":"15m","o":"329.51","c":"329.55","h":"329.58","l":"329.47","v":"43.237","n":38},{"t":1786193100000,"T":1786193999999,"s":"BTC","i":"15m","o":64974,"c":64970,"h":64990,"l":64970,"v":23.97477,"n":"1221"}]"#;
        let candles = parse_candle_snapshot(sample).expect("parse");
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].s, "xyz:TSLA");
        assert_eq!(candles[0].t, 1786192200000);
        assert!((candles[0].o - 329.51).abs() < 1e-9);
        assert!((candles[0].c - 329.55).abs() < 1e-9);
        assert!((candles[0].v - 43.237).abs() < 1e-9);
        assert_eq!(candles[0].n, 38);
        // numeric fallback for o/c/h/l/v/n (second candle mixes numeric and string)
        assert_eq!(candles[1].s, "BTC");
        assert!((candles[1].o - 64974.0).abs() < 1e-9);
        assert_eq!(candles[1].n, 1221);
        // empty array
        let empty = parse_candle_snapshot("[]").expect("empty");
        assert!(empty.is_empty());
        // null -> empty (bare coin returns null per live curl)
        let null = parse_candle_snapshot("null").expect("null");
        assert!(null.is_empty());
    }

    #[test]
    fn parse_candle_snapshot_numeric_string_defensive() {
        // Ensure string-number and numeric both parse for o,c,h,l,v (defensive)
        let sample = r#"[{"t":"1786192200000","T":"1786193099999","s":"BTC","i":"15m","o":"100.0","c":"101.0","h":102.0,"l":"99.0","v":"1000.5","n":10}]"#;
        let c = parse_candle_snapshot(sample).expect("defensive");
        assert_eq!(c[0].t, 1786192200000);
        assert_eq!(c[0].T, 1786193099999);
        assert!((c[0].h - 102.0).abs() < 1e-9);
        assert!((c[0].l - 99.0).abs() < 1e-9);
    }

    #[test]
    fn ensure_position_markets_injects_missing_and_ctx_refresh_persists() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        // Factored merge helper test: snapshot missing "FOO" + open position on FOO + engine mid known
        let mut snap = vec![MarketRow {
            market: "BTC".to_string(),
            mid: 100.0,
            mark: 100.0,
            oracle: 99.0,
            funding: 0.01,
            open_interest: 1000.0,
            day_ntl_vlm: 600_000.0,
            prev_day_px: 99.5,
            features: None,
        }];
        let open = vec!["FOO".to_string()];
        let snapshot_mids: HashMap<String, f64> = HashMap::new(); // missing
        let mut engine_mids = HashMap::new();
        engine_mids.insert("FOO".to_string(), 42.0);
        let ctx_mids: HashMap<String, f64> = HashMap::new();
        ensure_position_markets(
            &mut snap,
            &open,
            &snapshot_mids,
            &engine_mids,
            &ctx_mids,
            |_| None,
        );
        assert_eq!(snap.len(), 2, "FOO should be injected");
        let foo = snap.iter().find(|m| m.market == "FOO").expect("FOO row");
        assert!((foo.mid - 42.0).abs() < 1e-9, "mid seeded from engine {}", foo.mid);
        assert!((foo.mark - 42.0).abs() < 1e-9);
        // On ctx refresh with CtxRow for FOO: funding should update, mid untouched
        let ctx_rows = vec![CtxRow {
            market: "FOO".to_string(),
            mark: 43.0,
            oracle: 42.5,
            mid: 99.9, // should be ignored for mid
            funding: 0.05,
            open_interest: 9999.0,
            day_ntl_vlm: 5000.0,
            prev_day_px: 41.0,
        }];
        merge_ctx_rows(&mut snap, &ctx_rows);
        let foo2 = snap.iter().find(|m| m.market == "FOO").expect("FOO still");
        assert!((foo2.mid - 42.0).abs() < 1e-9, "mid untouched by ctx refresh, got {}", foo2.mid);
        assert!((foo2.funding - 0.05).abs() < 1e-12, "funding refreshed");
        assert!((foo2.mark - 43.0).abs() < 1e-9, "mark refreshed from ctx");
        // Second refresh with no FOO row in batch: FOO persists with mid untouched
        let ctx_only_btc = vec![CtxRow {
            market: "BTC".to_string(),
            mark: 101.0,
            oracle: 100.0,
            mid: 101.0,
            funding: 0.09,
            open_interest: 1111.0,
            day_ntl_vlm: 700_000.0,
            prev_day_px: 100.0,
        }];
        merge_ctx_rows(&mut snap, &ctx_only_btc);
        assert_eq!(snap.len(), 2);
        let foo3 = snap.iter().find(|m| m.market == "FOO").expect("FOO persists");
        assert!((foo3.mid - 42.0).abs() < 1e-9, "still 42 after unrelated ctx");
        // also test snapshot_mids priority: if snapshot had mid 55, engine 42, ctx 99 -> use snapshot
        let mut snap2 = vec![MarketRow {
            market: "BTC".to_string(),
            mid: 100.0,
            mark: 100.0,
            oracle: 100.0,
            funding: 0.01,
            open_interest: 1000.0,
            day_ntl_vlm: 600_000.0,
            prev_day_px: 100.0,
            features: None,
        }];
        let mut sm = HashMap::new();
        sm.insert("BAR".to_string(), 55.0);
        let mut em = HashMap::new();
        em.insert("BAR".to_string(), 42.0);
        let mut cm = HashMap::new();
        cm.insert("BAR".to_string(), 99.9);
        ensure_position_markets(&mut snap2, &["BAR".to_string()], &sm, &em, &cm, |_| None);
        let bar = snap2.iter().find(|m| m.market == "BAR").unwrap();
        assert!((bar.mid - 55.0).abs() < 1e-9, "snapshot mid takes priority, got {}", bar.mid);
    }

    #[test]
    #[allow(clippy::useless_vec)]
    fn union_filtered_vs_snapshot_len_and_icp_regression() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        // Union test: universe of 2 + 1 external position market -> snapshot len 3, markets_tracked stays 2
        let filtered = [
            CtxRow {
                market: "BTC".to_string(),
                mark: 100.0,
                oracle: 99.0,
                mid: 100.0,
                funding: 0.01,
                open_interest: 1000.0,
                day_ntl_vlm: 2_000_000.0,
                prev_day_px: 99.0,
            },
            CtxRow {
                market: "ETH".to_string(),
                mark: 200.0,
                oracle: 199.0,
                mid: 200.0,
                funding: 0.02,
                open_interest: 2000.0,
                day_ntl_vlm: 2_000_000.0,
                prev_day_px: 199.0,
            },
        ];
        let mut markets: Vec<MarketRow> = filtered
            .iter()
            .map(|r| MarketRow {
                market: r.market.clone(),
                mid: r.mid,
                mark: r.mark,
                oracle: r.oracle,
                funding: r.funding,
                open_interest: r.open_interest,
                day_ntl_vlm: r.day_ntl_vlm,
                prev_day_px: r.prev_day_px,
                features: None,
            })
            .collect();
        let filtered_len = markets.len();
        assert_eq!(filtered_len, 2);
        let open = vec!["FOO".to_string()];
        let snapshot_mids: HashMap<String, f64> = HashMap::new();
        let mut engine_mids = HashMap::new();
        engine_mids.insert("FOO".to_string(), 42.0);
        let ctx_mids: HashMap<String, f64> = HashMap::new();
        ensure_position_markets(&mut markets, &open, &snapshot_mids, &engine_mids, &ctx_mids, |_| None);
        assert_eq!(markets.len(), 3, "snapshot holds filtered+external");
        // markets_tracked should stay 2 (filtered_len) — writer documented in ensure_position_markets
        let markets_tracked = filtered_len;
        assert_eq!(markets_tracked, 2);
        assert!(markets.iter().any(|m| m.market == "FOO"));
        // Regression: ICP (in-universe) behavior unchanged (row updated normally via merge)
        // Setup already has ICP in universe, open position on ICP should not duplicate.
        let mut snap_icp = vec![MarketRow {
            market: "ICP".to_string(),
            mid: 10.0,
            mark: 10.0,
            oracle: 9.9,
            funding: 0.01,
            open_interest: 1000.0,
            day_ntl_vlm: 3_000_000.0,
            prev_day_px: 9.8,
            features: None,
        }];
        let open_icp = vec!["ICP".to_string()];
        let empty: HashMap<String, f64> = HashMap::new();
        let before_len = snap_icp.len();
        ensure_position_markets(&mut snap_icp, &open_icp, &empty, &empty, &empty, |_| None);
        assert_eq!(snap_icp.len(), before_len, "ICP already present should not duplicate");
        // updating via merge_ctx_rows should update funding/mark but preserve mid
        let rows = vec![CtxRow {
            market: "ICP".to_string(),
            mark: 11.0,
            oracle: 10.9,
            mid: 999.0,
            funding: 0.05,
            open_interest: 9999.0,
            day_ntl_vlm: 3_500_000.0,
            prev_day_px: 10.0,
        }];
        merge_ctx_rows(&mut snap_icp, &rows);
        let icp = snap_icp.iter().find(|m| m.market == "ICP").unwrap();
        assert!((icp.funding - 0.05).abs() < 1e-12, "ICP funding updated");
        assert!((icp.mark - 11.0).abs() < 1e-9, "ICP mark updated");
        assert!((icp.mid - 10.0).abs() < 1e-9, "ICP mid preserved (mids feed authoritative)");
    }

    #[test]
    fn ensure_position_markets_map_injects_and_preserves_filtered_len() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        // Map variant used by 30s poll: start from existing snapshot map (BTC) + filtered rows (ETH)
        // open position on FOO external -> map gains FOO, filtered_len stays 1.
        let mut map: HashMap<String, MarketRow> = HashMap::new();
        map.insert(
            "BTC".to_string(),
            MarketRow {
                market: "BTC".to_string(),
                mid: 100.0,
                mark: 100.0,
                oracle: 99.0,
                funding: 0.01,
                open_interest: 1000.0,
                day_ntl_vlm: 600_000.0,
                prev_day_px: 99.5,
                features: None,
            },
        );
        // simulate filtered row ETH not yet in map
        let filtered_rows = vec![CtxRow {
            market: "ETH".to_string(),
            mark: 200.0,
            oracle: 199.0,
            mid: 200.0,
            funding: 0.02,
            open_interest: 2000.0,
            day_ntl_vlm: 2_000_000.0,
            prev_day_px: 199.0,
        }];
        let filtered_len = filtered_rows.len();
        // merge filtered first (poll logic)
        for r in filtered_rows {
            let entry = map.entry(r.market.clone()).or_insert_with(|| MarketRow {
                market: r.market.clone(),
                mid: r.mid,
                mark: r.mark,
                oracle: r.oracle,
                funding: r.funding,
                open_interest: r.open_interest,
                day_ntl_vlm: r.day_ntl_vlm,
                prev_day_px: r.prev_day_px,
                features: None,
            });
            entry.mark = r.mark;
            entry.oracle = r.oracle;
            entry.funding = r.funding;
            entry.open_interest = r.open_interest;
            entry.day_ntl_vlm = r.day_ntl_vlm;
            entry.prev_day_px = r.prev_day_px;
            if entry.mid == 0.0 {
                entry.mid = r.mid;
            }
        }
        assert_eq!(map.len(), 2, "BTC+ETH");
        // now inject FOO via map variant
        let open = vec!["FOO".to_string()];
        let snapshot_mids: HashMap<String, f64> = map.iter().map(|(k, v)| (k.clone(), v.mid)).collect();
        let mut engine_mids = HashMap::new();
        engine_mids.insert("FOO".to_string(), 55.0);
        let ctx_mids: HashMap<String, f64> = HashMap::new();
        ensure_position_markets_map(&mut map, &open, &snapshot_mids, &engine_mids, &ctx_mids, |_| None);
        assert_eq!(map.len(), 3, "FOO injected -> BTC+ETH+FOO");
        assert!(map.contains_key("FOO"));
        assert!((map["FOO"].mid - 55.0).abs() < 1e-9);
        // markets_tracked still filtered_len (1) if ETH was only filtered row, but injection added FOO.
        // In real poll, filtered_len is from fetch_universe BEFORE injection.
        assert_eq!(filtered_len, 1);
    }

    // ── Source guard: never inject zero/unknown marks (3249.88 spike fix) ──────────

    #[test]
    fn ensure_position_markets_skips_when_no_mids_known_and_injects_after_mid_arrives() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        // Bootstrap-before-ws-connect: injection happens before mids ws connects → no mids known anywhere.
        // Must NOT inject a row with mid 0.0 (would cause short +100% equity spike or long -100% kill false-trip).
        let mut snap = vec![MarketRow {
            market: "BTC".to_string(),
            mid: 100.0,
            mark: 100.0,
            oracle: 99.0,
            funding: 0.01,
            open_interest: 1000.0,
            day_ntl_vlm: 600_000.0,
            prev_day_px: 99.5,
            features: None,
        }];
        let open = vec!["UNKNOWN".to_string()];
        let empty: HashMap<String, f64> = HashMap::new();
        ensure_position_markets(&mut snap, &open, &empty, &empty, &empty, |_| None);
        assert_eq!(snap.len(), 1, "should NOT inject UNKNOWN when no mid known anywhere (source guard)");
        assert!(!snap.iter().any(|m| m.market == "UNKNOWN"));
        // Guard confirmation: no injected row ever has mid == 0.0
        for m in &snap {
            assert!(m.mid > 0.0, "invariant: injected mid never 0, got {} for {}", m.mid, m.market);
        }
        // After a mid arrives via engine (first tick), next refresh DOES inject with that mid.
        let mut engine_mids = HashMap::new();
        engine_mids.insert("UNKNOWN".to_string(), 42.5);
        ensure_position_markets(&mut snap, &open, &empty, &engine_mids, &empty, |_| None);
        assert_eq!(snap.len(), 2, "after mid arrives, should inject");
        let unk = snap.iter().find(|m| m.market == "UNKNOWN").expect("UNKNOWN injected");
        assert!((unk.mid - 42.5).abs() < 1e-9, "seeded from engine mid");
        assert!(unk.mid > 0.0, "invariant holds after injection");
        assert!((unk.mark - 42.5).abs() < 1e-9);
    }

    #[test]
    fn ensure_position_markets_map_skips_zero_and_injects_with_valid() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        let mut map: HashMap<String, MarketRow> = HashMap::new();
        map.insert(
            "BTC".to_string(),
            MarketRow {
                market: "BTC".to_string(),
                mid: 100.0,
                mark: 100.0,
                oracle: 99.0,
                funding: 0.01,
                open_interest: 1000.0,
                day_ntl_vlm: 600_000.0,
                prev_day_px: 99.5,
                features: None,
            },
        );
        let open = vec!["FOOMAP".to_string()];
        let empty: HashMap<String, f64> = HashMap::new();
        // No mids known → must NOT inject FOO with 0.0
        ensure_position_markets_map(&mut map, &open, &empty, &empty, &empty, |_| None);
        assert_eq!(map.len(), 1, "map source guard: should not inject with no valid mid");
        assert!(!map.contains_key("FOOMAP"));
        // Zero-valued mids in maps must also be treated as unknown (not valid).
        let mut zero_mids = HashMap::new();
        zero_mids.insert("FOOMAP".to_string(), 0.0);
        ensure_position_markets_map(&mut map, &open, &zero_mids, &empty, &empty, |_| None);
        assert!(!map.contains_key("FOOMAP"), "mid 0.0 must not count as valid source");
        // Valid ctx mid should inject.
        let mut ctx_mids = HashMap::new();
        ctx_mids.insert("FOOMAP".to_string(), 77.7);
        ensure_position_markets_map(&mut map, &open, &empty, &empty, &ctx_mids, |_| None);
        assert_eq!(map.len(), 2);
        assert!((map["FOOMAP"].mid - 77.7).abs() < 1e-9);
        assert!(map["FOOMAP"].mid > 0.0, "invariant: map injected mid never 0");
    }

    #[test]
    fn ensure_position_markets_zero_mark_never_seeds_zero_mid() {
        use crate::contracts::MarketRow;
        use std::collections::HashMap;
        // If	ctx mids somehow contain 0, engine empty, snapshot empty → still no injection.
        let mut snap: Vec<MarketRow> = vec![];
        let open = vec!["ZEROTEST".to_string()];
        let mut snapshot_mids = HashMap::new();
        snapshot_mids.insert("ZEROTEST".to_string(), 0.0);
        let mut engine_mids = HashMap::new();
        engine_mids.insert("ZEROTEST".to_string(), 0.0);
        let mut ctx_mids = HashMap::new();
        ctx_mids.insert("ZEROTEST".to_string(), 0.0);
        ensure_position_markets(&mut snap, &open, &snapshot_mids, &engine_mids, &ctx_mids, |_| None);
        assert!(snap.is_empty(), "all zero mids must be treated as unknown — no injection");
    }
}
