#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Features {
    pub r5m: f64,
    pub r1h: f64,
    pub r24h: f64,
    pub vol1h: f64,
    pub funding_z: f64,
    pub range_pos: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketRow {
    pub market: String,
    pub mid: f64,
    pub mark: f64,
    pub oracle: f64,
    pub funding: f64,
    pub open_interest: f64,
    pub day_ntl_vlm: f64,
    pub prev_day_px: f64,
    pub features: Option<Features>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub ts: i64,
    pub markets: Vec<MarketRow>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Long,
    Short,
}

/// One order-book level (HL l2Book levels come as string numbers: {px, sz, n}).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct L2Level {
    pub px: f64,
    pub sz: f64,
}

/// Order book snapshot for one coin (dex-prefixed market key like "xyz:TSLA" or "SOL").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct L2Book {
    pub levels: [Vec<L2Level>; 2], // [bids, asks]
    pub ts: i64,                   // ms epoch of last update
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Nominee {
    pub ts: i64,
    pub market: String,
    pub side_hint: Side,
    pub score: f64,
    pub features: Features,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub action: String,
    pub side: Option<Side>,
    pub conviction: f64,
    pub thesis: String,
    pub horizon_hours: Option<f64>,
    pub stop_pct: Option<f64>,
    pub tp_pct: Option<f64>,
    /// The model's stated falsifier (e.g. "if 4h RSI14 breaks below 40") — what observation
    /// would kill the thesis. Optional: decisions written before 2026-08-24 parse with None.
    #[serde(default)]
    pub invalidation_condition: Option<String>,
    /// The model's own dollar-risk estimate for the plan. Advisory only — the sizer
    /// (`sizing::size_position`) remains the authority on size; this is forensic context.
    #[serde(default)]
    pub risk_usd: Option<f64>,
    /// Per-trade leverage requested by the model (1.0–10.0, 1.0 = 1x). `None` → default 3.0
    /// (kept via `sizing::size_position` fallback). Clamped in sizing: majors max 10x,
    /// `xyz:` max 5x, otherwise 10x.
    #[serde(default)]
    pub leverage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub id: i64,
    pub market: String,
    pub side: Side,
    pub entry_px: f64,
    pub size: f64,
    pub leverage: f64,
    pub margin: f64,
    pub sl_px: f64,
    pub tp_px: f64,
    pub opened_ts: i64,
    #[serde(default)]
    pub analyst: String,
    #[serde(default)]
    pub horizon_hours: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NewsItem {
    pub id: i64,
    pub ts: i64,
    pub source: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub markets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Health {
    pub ok: bool,
    pub uptime_s: u64,
    pub ws_connected: bool,
    pub markets_tracked: usize,
    pub analyst_failure_streak: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Trade {
    pub id: i64,
    pub position_id: i64,
    pub market: String,
    pub action: String,
    pub px: f64,
    pub size: f64,
    pub fee: f64,
    pub realized_pnl: f64,
    pub ts: i64,
    #[serde(default = "default_fill_mode")]
    pub fill_mode: String,
}

fn default_fill_mode() -> String { "flat".to_string() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EquityPoint {
    pub ts: i64,
    pub equity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionLog {
    pub ts: i64,
    pub market: String,
    pub action: String,
    pub side: Option<Side>,
    pub conviction: f64,
    pub thesis: String,
    pub horizon_hours: Option<f64>,
    pub vetoed: bool,
    pub executed: bool,
    pub reason: String,
    #[serde(default)]
    pub analyst: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum WsMsg {
    #[serde(rename = "mids")]
    Mids(HashMap<String, f64>),
    #[serde(rename = "position")]
    Position(Position),
    #[serde(rename = "decision")]
    Decision(DecisionLog),
    #[serde(rename = "news")]
    News(NewsItem),
    #[serde(rename = "equity")]
    Equity(EquityPoint),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_serde_roundtrip() {
        let json = r#"{"action":"open","side":"long","conviction":0.85,"thesis":"momentum + funding","horizon_hours":24.0,"stop_pct":1.2,"tp_pct":2.4}"#;
        let d: Decision = serde_json::from_str(json).expect("parse");
        assert_eq!(d.action, "open");
        assert_eq!(d.side, Some(Side::Long));
        assert!((d.conviction - 0.85).abs() < 1e-9);
        let out = serde_json::to_string(&d).expect("ser");
        let d2: Decision = serde_json::from_str(&out).expect("reparse");
        assert_eq!(d, d2);
    }

    #[test]
    fn decision_skip_without_side() {
        let json = r#"{"action":"skip","side":null,"conviction":0.4,"thesis":"low conviction","horizon_hours":null,"stop_pct":null,"tp_pct":null}"#;
        let d: Decision = serde_json::from_str(json).expect("parse");
        assert_eq!(d.action, "skip");
        assert_eq!(d.side, None);
    }

    #[test]
    fn decision_without_plan_fields_still_parses() {
        // Pre-2026-08-24 schema: no invalidation_condition / risk_usd. Old model outputs and
        // old persisted parsed_json must never fail to parse after the fields landed.
        let json = r#"{"action":"open","side":"long","conviction":0.85,"thesis":"momentum","horizon_hours":24.0,"stop_pct":1.2,"tp_pct":2.4}"#;
        let d: Decision = serde_json::from_str(json).expect("parse legacy decision");
        assert_eq!(d.invalidation_condition, None);
        assert_eq!(d.risk_usd, None);
    }

    #[test]
    fn decision_with_plan_fields_parses() {
        let json = r#"{"action":"open","side":"short","conviction":0.8,"thesis":"fade spike","horizon_hours":12.0,"stop_pct":1.5,"tp_pct":3.0,"invalidation_condition":"if 4h RSI14 breaks above 60","risk_usd":18.5}"#;
        let d: Decision = serde_json::from_str(json).expect("parse");
        assert_eq!(d.invalidation_condition.as_deref(), Some("if 4h RSI14 breaks above 60"));
        assert_eq!(d.risk_usd, Some(18.5));
        let out = serde_json::to_string(&d).expect("ser");
        let d2: Decision = serde_json::from_str(&out).expect("reparse");
        assert_eq!(d, d2);
    }

    #[test]
    fn decision_without_leverage_parses_as_none() {
        // Legacy decision without leverage field must parse as None (backwards compat → default 3.0 via sizing)
        let json = r#"{"action":"open","side":"long","conviction":0.85,"thesis":"momentum","horizon_hours":24.0,"stop_pct":1.2,"tp_pct":2.4}"#;
        let d: Decision = serde_json::from_str(json).expect("parse legacy without leverage");
        assert_eq!(d.leverage, None);
    }

    #[test]
    fn decision_with_leverage_parses() {
        let json = r#"{"action":"open","side":"long","conviction":0.8,"thesis":"momentum","horizon_hours":24.0,"stop_pct":1.2,"tp_pct":2.4,"leverage":7.5}"#;
        let d: Decision = serde_json::from_str(json).expect("parse with leverage");
        assert_eq!(d.leverage, Some(7.5));
        let out = serde_json::to_string(&d).expect("ser");
        let d2: Decision = serde_json::from_str(&out).expect("reparse");
        assert_eq!(d2.leverage, Some(7.5));
    }

    #[test]
    fn ws_msg_mids_roundtrip() {
        let mut m = HashMap::new();
        m.insert("SOL".to_string(), 123.45);
        m.insert("xyz:TSLA".to_string(), 250.0);
        let msg = WsMsg::Mids(m);
        let s = serde_json::to_string(&msg).expect("ser");
        assert!(s.contains(r#""type":"mids""#));
        let back: WsMsg = serde_json::from_str(&s).expect("de");
        assert_eq!(msg, back);
    }

    #[test]
    fn features_field_names_exact() {
        // Ensure JSON field names match spec §4
        let f = Features {
            r5m: 0.1,
            r1h: 0.5,
            r24h: 1.0,
            vol1h: 0.4,
            funding_z: 1.2,
            range_pos: 0.9,
        };
        let v = serde_json::to_value(&f).expect("val");
        assert!(v.get("r5m").is_some());
        assert!(v.get("r1h").is_some());
        assert!(v.get("r24h").is_some());
        assert!(v.get("vol1h").is_some());
        assert!(v.get("funding_z").is_some());
        assert!(v.get("range_pos").is_some());
    }
}
