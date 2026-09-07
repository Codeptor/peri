#![allow(dead_code)]
#![allow(clippy::collapsible_if)]
#![allow(unused_comparisons)]
#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{
        Query, State,
        ws::{WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast, mpsc};
use tracing::{debug, warn};

use crate::contracts::Side;
use crate::contracts::{
    DecisionLog, EquityPoint, Health, NewsItem, Nominee, Position, Snapshot, Trade, WsMsg,
};
use crate::ledger::Store;
use crate::news::News;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub snapshot: Arc<RwLock<Snapshot>>,
    pub nominees: Arc<RwLock<Vec<Nominee>>>,
    pub tx: broadcast::Sender<WsMsg>,
    pub start: Instant,
    pub ws_fresh: Arc<crate::hl_ws::WsFreshness>,
    pub markets_tracked: Arc<AtomicUsize>,
    pub news: Arc<News>,
    /// The very gate the screener trades through — `/api/gates` reports it rather than
    /// re-deriving it, so the endpoint cannot describe rails the trader is not running.
    pub entry_gate: crate::EntryGate,
    /// Candle source for `/api/analytics` veto counterfactuals.
    pub hl: crate::hl_rest::HlRest,
    pub force_nominees: mpsc::Sender<Vec<String>>,
    pub analyst_failure_streak: Arc<AtomicU64>,
    pub analysts_enabled: bool,
    pub analyst: Option<Arc<crate::analyst::Analyst>>,
    pub analyst_model: String,
    pub analyst_roster: Vec<String>,
    pub sizing: crate::config::SizingCfg,
    pub chat_gate: Arc<tokio::sync::Mutex<crate::risk::GateState>>,
}

/// Every handler that awaits ws-fed shared state bounds the lock acquisition with this.
///
/// Post-mortem 2026-08-09: an ABBA inversion between the snapshot `RwLock` and the
/// feature-engine `Mutex` parked a writer forever; because `tokio::sync::RwLock` is
/// task-fair, a queued writer starves every subsequent reader, so `/api/snapshot` and
/// `/api/positions` hung indefinitely (curl never returned) while the rest of the API
/// kept serving. The inversion itself is fixed in `main.rs`; this bound guarantees that
/// any future regression surfaces as a fast 503 the dashboard can render, never a hang.
pub const STATE_TIMEOUT: Duration = Duration::from_secs(3);

fn state_unavailable() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({"error": "state unavailable"})),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
struct HealthResp {
    ok: bool,
    uptime_s: u64,
    ws_connected: bool,
    markets_tracked: usize,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/snapshot", get(snapshot_handler))
        .route("/api/nominees", get(nominees_handler))
        .route("/api/positions", get(positions_handler))
        .route("/api/trades", get(trades_handler))
        .route("/api/equity", get(equity_handler))
        .route("/api/decisions", get(decisions_handler))
        .route("/api/gates", get(gates_handler))
        .route("/api/analytics", get(analytics_handler))
        .route("/api/news", get(news_handler))
        .route("/api/analyst/chat", post(chat_handler))
        .route("/api/analyst/chat/history", get(chat_history_handler))
        .route("/api/analyst/calls", get(analyst_calls_handler))
        .route("/api/analyst/leaderboard", get(analyst_leaderboard_handler))
        .route("/ingest/news", post(ingest_handler))
        .route("/api/ingest/news", post(ingest_handler))
        .route("/ws", get(ws_handler))
        .with_state(state)
}

#[derive(Deserialize)]
struct ChatReq {
    message: String,
}
#[derive(Serialize)]
struct ChatResp {
    reply: String,
    action: Option<ChatAction>,
    ts: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatAction {
    #[serde(rename = "type")]
    kind: String,
    market: String,
    side: Option<Side>,
    sl_pct: Option<f64>,
    tp_pct: Option<f64>,
    conviction: Option<f64>,
    #[serde(default)]
    executed: bool,
    #[serde(default)]
    gate_refusals: Vec<String>,
}
#[derive(Serialize, sqlx::FromRow)]
struct ChatRow {
    role: String,
    ts: i64,
    text: String,
    action_json: Option<String>,
}
#[derive(sqlx::FromRow)]
struct CallRow {
    id: i64,
    ts: i64,
    market: String,
    trigger: String,
    prompt: String,
    response_raw: String,
    outcome_kind: String,
    parsed_json: Option<String>,
    latency_ms: i64,
    analyst: String,
}

#[derive(Deserialize)]
struct UiMessage {
    role: String,
    #[serde(default)]
    parts: Vec<UiPart>,
}

#[derive(Deserialize)]
struct UiPart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

fn chat_prompt(body: serde_json::Value) -> Option<String> {
    if let Some(message) = body.get("message").and_then(|value| value.as_str()) {
        return Some(message.trim().chars().take(8_000).collect());
    }
    let messages: Vec<UiMessage> = serde_json::from_value(body.get("messages")?.clone()).ok()?;
    messages.into_iter().rev().find(|message| message.role == "user").map(|message| {
        message.parts.into_iter().filter(|part| part.kind == "text").map(|part| part.text).collect::<Vec<_>>().join("").trim().chars().take(8_000).collect()
    })
}

fn sse_frame(value: serde_json::Value) -> String {
    serde_json::to_string(&value).expect("SSE protocol frame serializes")
}

async fn chat_handler(
    State(s): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> axum::response::Response {
    let Some(message) = chat_prompt(body) else {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"message required"}))).into_response();
    };
    if message.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error":"message required"}))).into_response();
    }
    let (tx, rx) = mpsc::channel::<String>(64);
    tokio::spawn(run_chat_turn(s, message, tx));
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(frame)), rx))
    });
    axum::response::sse::Sse::new(stream).into_response()
}

async fn run_chat_turn(s: AppState, message: String, tx: mpsc::Sender<String>) {
    let ts = chrono::Utc::now().timestamp_millis();
    let _ = tx.send(sse_frame(serde_json::json!({"type":"start"}))).await;
    let _ = tx.send(sse_frame(serde_json::json!({"type":"text-start","id":"0"}))).await;
    let _ = sqlx::query("INSERT INTO chat_messages (ts,role,text) VALUES (?1,'user',?2)").bind(ts).bind(&message).execute(s.store.pool()).await;
    if !s.analysts_enabled {
        let reply = "Analysts are currently disabled (manage-only mode).";
        let _ = sqlx::query("INSERT INTO chat_messages (ts,role,text) VALUES (?1,'analyst',?2)").bind(ts).bind(reply).execute(s.store.pool()).await;
        let _ = tx.send(sse_frame(serde_json::json!({"type":"text-delta","id":"0","delta":reply}))).await;
        let _ = tx.send(sse_frame(serde_json::json!({"type":"text-end","id":"0"}))).await;
        let _ = tx.send(sse_frame(serde_json::json!({"type":"finish"}))).await;
        let _ = tx.send("[DONE]".into()).await;
        return;
    }
    let Some(analyst) = s.analyst.as_ref() else {
        let _ = tx.send(sse_frame(serde_json::json!({"type":"error"}))).await;
        let _ = tx.send("[DONE]".into()).await;
        return;
    };
    let marks: HashMap<String, f64> = { let snap = s.snapshot.read().await; snap.markets.iter().map(|m| (m.market.clone(), m.mid)).collect() };
    let positions = s.store.open_positions().await.unwrap_or_default();
    let equity = s.store.equity(&marks).await.unwrap_or(s.sizing.bankroll);
    let decisions: Vec<(String, String, f64, String)> = sqlx::query_as("SELECT market, action, conviction, reason FROM decisions ORDER BY ts DESC LIMIT 10").fetch_all(s.store.pool()).await.unwrap_or_default();
    let history: Vec<ChatRow> = sqlx::query_as("SELECT role,ts,text,action_json FROM chat_messages ORDER BY ts DESC LIMIT 20").fetch_all(s.store.pool()).await.unwrap_or_default().into_iter().rev().collect();
    let names: Vec<String> = marks.keys().filter(|market| message.to_uppercase().contains(&market.to_uppercase())).cloned().collect();
    let mut context = format!("You are Kestrel's PAPER-only analyst. No real orders or funds exist. Portfolio equity: {equity:.2}. Open positions: {:?}. Recent decisions: {:?}. If the user asks to trade, end your reply with exactly one fenced json action: {{\"type\":\"open\",\"market\":\"SOL\",\"side\":\"long\",\"sl_pct\":1.2,\"tp_pct\":2.4,\"conviction\":0.8}} or {{\"type\":\"close\",\"market\":\"SOL\"}}. The action is gated and may be refused.\n", positions.iter().map(|p| format!("{} {:?} uPnL {:.2}", p.market, p.side, marks.get(&p.market).map(|m| match p.side { Side::Long => (m-p.entry_px)*p.size, Side::Short => (p.entry_px-m)*p.size }).unwrap_or(0.0))).collect::<Vec<_>>(), decisions);
    for market in &names { let news = s.news.matched_recent(market, ts - 6 * 3_600_000, 10).await.unwrap_or_default(); context.push_str(&format!("Market {market}: features/mark {:?}; news {:?}\n", marks.get(market), news)); }
    context.push_str("Dialogue:\n");
    for row in history { context.push_str(&format!("{}: {}\n", row.role, row.text)); }
    context.push_str(&format!("user: {message}"));
    let (delta_tx, mut delta_rx) = mpsc::channel::<String>(64);
    let analyst = analyst.clone();
    let prompt = context.clone();
    let upstream = tokio::spawn(async move { analyst.chat_stream(&prompt, delta_tx).await });
    let mut reply = String::new();
    while let Some(delta) = delta_rx.recv().await {
        reply.push_str(&delta);
        let _ = tx.send(sse_frame(serde_json::json!({"type":"text-delta","id":"0","delta":delta}))).await;
    }
    match upstream.await {
        Ok(Ok(call)) => {
            let _ = sqlx::query("INSERT INTO analyst_calls (ts,market,trigger,prompt,response_raw,outcome_kind,parsed_json,latency_ms,analyst) VALUES (?1,'','chat',?2,?3,?4,NULL,?5,?6)").bind(ts).bind(call.prompt).bind(&reply).bind(call.outcome_kind).bind(call.latency_ms as i64).bind(&s.analyst_model).execute(s.store.pool()).await;
            let mut action = parse_chat_action(&reply);
            if let Some(action) = action.as_mut() {
                let reason = chat_reason(&s.analyst_model, call.latency_ms);
                execute_chat_action(&s, action, &reason).await;
            }
            let action_json = action.as_ref().and_then(|action| serde_json::to_string(action).ok());
            let _ = sqlx::query("INSERT INTO chat_messages (ts,role,text,action_json) VALUES (?1,'analyst',?2,?3)").bind(ts).bind(&reply).bind(action_json).execute(s.store.pool()).await;
            let _ = tx.send(sse_frame(serde_json::json!({"type":"text-end","id":"0"}))).await;
            if let Some(action) = action { let _ = tx.send(sse_frame(serde_json::json!({"type":"data-action","data":action}))).await; }
            let _ = tx.send(sse_frame(serde_json::json!({"type":"finish"}))).await;
        }
        _ => { let _ = tx.send(sse_frame(serde_json::json!({"type":"error"}))).await; }
    }
    let _ = tx.send("[DONE]".into()).await;
}

fn parse_chat_action(reply: &str) -> Option<ChatAction> {
    let start = reply.rfind("```json")? + 7;
    let end = reply[start..].find("```")? + start;
    let mut action: ChatAction = serde_json::from_str(reply[start..end].trim()).ok()?;
    if !matches!(action.kind.as_str(), "open" | "close") || action.market.is_empty() {
        return None;
    }
    action.executed = false;
    action.gate_refusals.clear();
    Some(action)
}

fn chat_reason(model: &str, latency_ms: u64) -> String {
    format!("chat; model {model} refused:false latency:{latency_ms}")
}

async fn log_chat_decision(s: &AppState, action: &ChatAction, decision_action: &str, side: &str, reason: &str) {
    let mut reason = reason.to_string();
    for refusal in &action.gate_refusals { reason.push_str(&format!(" gate_refused:{refusal}")); }
    let _ = s.store.log_decision(
        chrono::Utc::now().timestamp_millis(), &action.market, decision_action, side,
        action.conviction.unwrap_or(0.0), "chat action", 24.0,
        decision_action == "veto_close", action.executed, &reason,
    ).await;
}

async fn execute_chat_action(s: &AppState, action: &mut ChatAction, reason: &str) {
    let now = chrono::Utc::now().timestamp_millis();
    if action.kind == "close" {
        let positions = s.store.open_positions().await.unwrap_or_default();
        if let Some(pos) = positions.into_iter().find(|p| p.market == action.market) {
            let side = side_name(pos.side);
            let mark = {
                let snap = s.snapshot.read().await;
                snap.markets
                    .iter()
                    .find(|m| m.market == action.market)
                    .map(|m| m.mid)
                    .unwrap_or(0.0)
            };
            action.executed = mark.is_finite()
                && mark > 0.0
                && s.store
                    .close_position(pos.id, mark, "veto_close", None)
                    .await
                    .is_ok();
            if action.executed {
                crate::record_close_cooldown(&s.chat_gate, &action.market, now).await;
            }
            log_chat_decision(s, action, if action.executed { "veto_close" } else { "skip" }, side, reason).await;
        } else {
            action.gate_refusals.push("no_open_position".into());
            log_chat_decision(s, action, "skip", "skip", reason).await;
        }
        return;
    }
    let conviction = action.conviction.unwrap_or(0.0).min(0.95);
    action.conviction = Some(conviction);
    let side = match action.side {
        Some(side) => side,
        None => {
            action.gate_refusals.push("invalid_action".into());
            log_chat_decision(s, action, "skip", "skip", reason).await;
            return;
        }
    };
    let row = {
        let snap = s.snapshot.read().await;
        snap.markets
            .iter()
            .find(|m| m.market == action.market)
            .cloned()
    };
    let Some(row) = row else {
        action.gate_refusals.push("unknown_market".into());
        log_chat_decision(s, action, "skip", side_name(side), reason).await;
        return;
    };
    if let Err(refusal) = s.entry_gate.check("", &action.market, conviction, now).await {
        action.gate_refusals.push(refusal.as_str().into());
        log_chat_decision(s, action, "skip", side_name(side), reason).await;
        return;
    }
    let Some(features) = row.features else {
        action.gate_refusals.push("missing_features".into());
        log_chat_decision(s, action, "skip", side_name(side), reason).await;
        return;
    };
    let mut sized = crate::sizing::size_position(&s.sizing, features.vol1h, conviction);
    if let Some(sl) = action.sl_pct {
        sized.stop_pct = sl.clamp(0.4, 4.0);
        action.sl_pct = Some(sized.stop_pct);
    }
    if let Some(tp) = action.tp_pct {
        sized.tp_pct = tp.clamp(0.4, 4.0);
        action.tp_pct = Some(sized.tp_pct);
    }
    action.executed = s
        .store
        .open_position(
            &action.market,
            side,
            &sized,
            row.mid,
            action.market.starts_with("xyz:"),
            24.0,
            None,
        )
        .await
        .is_ok();
    log_chat_decision(s, action, "open", side_name(side), reason).await;
}

fn side_name(side: Side) -> &'static str { match side { Side::Long => "long", Side::Short => "short" } }

async fn chat_history_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let mut rows: Vec<ChatRow> = sqlx::query_as(
        "SELECT role,ts,text,action_json FROM chat_messages ORDER BY ts DESC LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(s.store.pool())
    .await
    .unwrap_or_default();
    rows.reverse();
    Json(serde_json::json!({"messages":rows.into_iter().map(|row| serde_json::json!({
        "role": row.role,
        "ts": row.ts,
        "text": row.text,
        "action_json": row.action_json,
        "analyst": s.analyst_model,
    })).collect::<Vec<_>>() }))
}
async fn analyst_calls_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 1000);
    let rows:Vec<CallRow>=sqlx::query_as("SELECT id,ts,market,trigger,prompt,response_raw,outcome_kind,parsed_json,latency_ms,analyst FROM analyst_calls ORDER BY ts DESC,id DESC LIMIT ?1").bind(limit).fetch_all(s.store.pool()).await.unwrap_or_default();
    Json(
        serde_json::json!({"calls":rows.into_iter().map(|r|serde_json::json!({"id":r.id,"ts":r.ts,"market":r.market,"trigger":r.trigger,"prompt":r.prompt,"response_raw":r.response_raw,"outcome_kind":r.outcome_kind,"parsed":r.parsed_json,"latency_ms":r.latency_ms,"analyst":r.analyst})).collect::<Vec<_>>() }),
    )
}

/// Model-level paper performance. Trades inherit their position's analyst, including the
/// historical empty string so the dashboard can label it as its legacy bucket.
async fn analyst_leaderboard_handler(State(s): State<AppState>) -> impl IntoResponse {
    #[derive(Default)]
    struct Stats {
        positions_open: i64,
        closes: i64,
        wins: i64,
        realized_pnl: f64,
        unrealized_pnl: f64,
        decides: i64,
        avg_entry_conviction: Option<f64>,
    }
    #[derive(sqlx::FromRow)]
    struct PositionRow {
        model: String,
        positions_open: i64,
        closes: i64,
        wins: i64,
        realized_pnl: f64,
        unrealized_pnl: f64,
    }
    #[derive(sqlx::FromRow)]
    struct DecisionRow {
        model: String,
        decides: i64,
        avg_entry_conviction: Option<f64>,
    }

    let positions_sql = "SELECT p.analyst AS model, \
        COALESCE(SUM(CASE WHEN p.status='open' THEN 1 ELSE 0 END),0) AS positions_open, \
        COALESCE(SUM(CASE WHEN t.action!='open' THEN 1 ELSE 0 END),0) AS closes, \
        COALESCE(SUM(CASE WHEN t.action!='open' AND t.realized_pnl-t.fee>0 THEN 1 ELSE 0 END),0) AS wins, \
        COALESCE(SUM(CASE WHEN t.action!='open' THEN t.realized_pnl-t.fee ELSE 0.0 END),0.0) AS realized_pnl, \
        0.0 AS unrealized_pnl \
        FROM positions p LEFT JOIN trades t ON t.position_id=p.id GROUP BY p.analyst ORDER BY realized_pnl DESC";
    let mut stats: HashMap<String, Stats> = sqlx::query_as::<_, PositionRow>(positions_sql)
        .fetch_all(s.store.pool())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            (
                row.model,
                Stats {
                    positions_open: row.positions_open,
                    closes: row.closes,
                    wins: row.wins,
                    realized_pnl: row.realized_pnl,
                    unrealized_pnl: row.unrealized_pnl,
                    ..Default::default()
                },
            )
        })
        .collect();
    let decisions_sql = "SELECT analyst AS model, COUNT(*) AS decides, \
        AVG(CASE WHEN action='open' AND executed=1 THEN conviction END) AS avg_entry_conviction \
        FROM decisions GROUP BY analyst";
    for row in sqlx::query_as::<_, DecisionRow>(decisions_sql)
        .fetch_all(s.store.pool())
        .await
        .unwrap_or_default()
    {
        let model_stats = stats.entry(row.model).or_default();
        model_stats.decides = row.decides;
        model_stats.avg_entry_conviction = row.avg_entry_conviction.map(|value| {
            (value * 100.0).round() / 100.0
        });
    }
    let mut models = Vec::with_capacity(s.analyst_roster.len() + stats.len());
    for model in &s.analyst_roster {
        let stat = stats.remove(model).unwrap_or_default();
        models.push(serde_json::json!({
            "model": model,
            "enabled": true,
            "positions_open": stat.positions_open,
            "closes": stat.closes,
            "wins": stat.wins,
            "win_rate": if stat.closes == 0 { 0.0 } else { stat.wins as f64 / stat.closes as f64 },
            "realized_pnl": stat.realized_pnl,
            "unrealized_pnl": stat.unrealized_pnl,
            "decides": stat.decides,
            "avg_entry_conviction": stat.avg_entry_conviction,
        }));
    }
    for (model, stat) in stats {
        models.push(serde_json::json!({
            "model": model,
            "enabled": false,
            "positions_open": stat.positions_open,
            "closes": stat.closes,
            "wins": stat.wins,
            "win_rate": if stat.closes == 0 { 0.0 } else { stat.wins as f64 / stat.closes as f64 },
            "realized_pnl": stat.realized_pnl,
            "unrealized_pnl": stat.unrealized_pnl,
            "decides": stat.decides,
            "avg_entry_conviction": stat.avg_entry_conviction,
        }));
    }
    Json(serde_json::json!({"models":models}))
}

async fn health_handler(State(s): State<AppState>) -> impl IntoResponse {
    let uptime_s = s.start.elapsed().as_secs();
    // Truthful: derived from last-message age on either stream, not a set-once connect bool.
    let ws_connected = s
        .ws_fresh
        .connected_at(chrono::Utc::now().timestamp_millis());
    let markets_tracked = s.markets_tracked.load(Ordering::SeqCst);
    // fallback to snapshot len if tracked 0 — bounded, and health always answers:
    // if the snapshot is unreachable we report what we know (0) instead of hanging.
    let markets_tracked = if markets_tracked == 0 {
        match tokio::time::timeout(STATE_TIMEOUT, s.snapshot.read()).await {
            Ok(snap) => snap.markets.len(),
            Err(_) => 0,
        }
    } else {
        markets_tracked
    };
    Json(Health {
        ok: true,
        uptime_s,
        ws_connected,
        markets_tracked,
        analyst_failure_streak: s.analyst_failure_streak.load(Ordering::SeqCst),
    })
}

async fn snapshot_handler(State(s): State<AppState>) -> axum::response::Response {
    let Ok(guard) = tokio::time::timeout(STATE_TIMEOUT, s.snapshot.read()).await else {
        warn!(
            endpoint = "/api/snapshot",
            "snapshot lock unavailable within timeout"
        );
        return state_unavailable();
    };
    Json(guard.clone()).into_response()
}

async fn nominees_handler(State(s): State<AppState>) -> axum::response::Response {
    let Ok(guard) = tokio::time::timeout(STATE_TIMEOUT, s.nominees.read()).await else {
        warn!(
            endpoint = "/api/nominees",
            "nominees lock unavailable within timeout"
        );
        return state_unavailable();
    };
    Json(guard.clone()).into_response()
}

#[derive(Serialize, Deserialize)]
struct PositionResp {
    id: i64,
    market: String,
    side: String,
    entry_px: f64,
    size: f64,
    leverage: f64,
    margin: f64,
    sl_px: f64,
    tp_px: f64,
    opened_ts: i64,
    analyst: String,
    mark_px: f64,
    unrealized_pnl: f64,
    roe: f64,
}

async fn positions_handler(State(s): State<AppState>) -> axum::response::Response {
    let positions = s.store.open_positions().await.unwrap_or_default();
    let Ok(snap) = tokio::time::timeout(STATE_TIMEOUT, s.snapshot.read()).await else {
        warn!(
            endpoint = "/api/positions",
            "snapshot lock unavailable within timeout"
        );
        return state_unavailable();
    };
    let mut map: HashMap<String, f64> = HashMap::new();
    for m in snap.markets.iter() {
        map.insert(m.market.clone(), m.mid);
    }
    drop(snap);
    let resp: Vec<PositionResp> = positions
        .into_iter()
        .map(|p| {
            let mark = map.get(&p.market).copied().unwrap_or(p.entry_px);
            let unreal = match p.side {
                crate::contracts::Side::Long => (mark - p.entry_px) * p.size,
                crate::contracts::Side::Short => (p.entry_px - mark) * p.size,
            };
            let roe = if p.margin.abs() > 1e-9 {
                unreal / p.margin * 100.0
            } else {
                0.0
            };
            let side_str = match p.side {
                crate::contracts::Side::Long => "long",
                crate::contracts::Side::Short => "short",
            }
            .to_string();
            PositionResp {
                id: p.id,
                market: p.market,
                side: side_str,
                entry_px: p.entry_px,
                size: p.size,
                leverage: p.leverage,
                margin: p.margin,
                sl_px: p.sl_px,
                tp_px: p.tp_px,
                opened_ts: p.opened_ts,
                analyst: p.analyst,
                mark_px: mark,
                unrealized_pnl: unreal,
                roe,
            }
        })
        .collect();
    Json(resp).into_response()
}

async fn trades_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let rows: Vec<TradesRow> = sqlx::query_as("SELECT id, position_id, market, action, px, size, fee, realized_pnl, ts, fill_mode FROM trades ORDER BY ts DESC LIMIT ?1")
        .bind(limit)
        .fetch_all(s.store.pool())
        .await
        .unwrap_or_default();
    let out: Vec<Trade> = rows
        .into_iter()
        .map(|r| Trade {
            id: r.id,
            position_id: r.position_id,
            market: r.market,
            action: r.action,
            px: r.px,
            size: r.size,
            fee: r.fee,
            realized_pnl: r.realized_pnl,
            ts: r.ts,
            fill_mode: r.fill_mode,
        })
        .collect();
    Json(out)
}

#[derive(sqlx::FromRow)]
struct TradesRow {
    id: i64,
    position_id: i64,
    market: String,
    action: String,
    px: f64,
    size: f64,
    fee: f64,
    realized_pnl: f64,
    ts: i64,
    fill_mode: String,
}

async fn equity_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let points: i64 = q
        .get("points")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
        .clamp(1, 5000);
    let rows: Vec<EquityRow> =
        sqlx::query_as("SELECT ts, equity FROM equity ORDER BY ts DESC LIMIT ?1")
            .bind(points)
            .fetch_all(s.store.pool())
            .await
            .unwrap_or_default();
    let mut out: Vec<EquityPoint> = rows
        .into_iter()
        .map(|r| EquityPoint {
            ts: r.ts,
            equity: r.equity,
        })
        .collect();
    out.reverse();
    Json(out)
}
#[derive(sqlx::FromRow)]
struct EquityRow {
    ts: i64,
    equity: f64,
}

async fn decisions_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 1000);
    let rows: Vec<DecisionRow> = sqlx::query_as("SELECT ts, market, action, side, conviction, thesis, horizon_hours, vetoed, executed, reason, analyst FROM decisions ORDER BY ts DESC LIMIT ?1")
        .bind(limit)
        .fetch_all(s.store.pool())
        .await
        .unwrap_or_default();
    let out: Vec<DecisionLog> = rows
        .into_iter()
        .map(|r| {
            let side = match r.side.as_deref() {
                Some("long") => Some(crate::contracts::Side::Long),
                Some("short") => Some(crate::contracts::Side::Short),
                _ => None,
            };
            DecisionLog {
                ts: r.ts,
                market: r.market,
                action: r.action,
                side,
                conviction: r.conviction,
                thesis: r.thesis,
                horizon_hours: r.horizon_hours,
                vetoed: r.vetoed != 0,
                executed: r.executed != 0,
                reason: r.reason,
                analyst: r.analyst,
            }
        })
        .collect();
    Json(out)
}
#[derive(sqlx::FromRow)]
struct DecisionRow {
    ts: i64,
    market: String,
    action: String,
    side: Option<String>,
    conviction: f64,
    thesis: String,
    horizon_hours: Option<f64>,
    vetoed: i64,
    executed: i64,
    reason: String,
    analyst: String,
}

/// Live entry-gate state (Phase T2) — "why is the trader benched right now", one read.
///
/// Every field is a projection of the SAME rails the entry path runs (`crate::EntryGate`),
/// never a re-derivation: `staleness.stale` is `gate_data_age`'s verdict, `regime.active` is
/// `gate_regime`'s, the caps are the live `RiskCfg`. A number here and a refusal in the log can
/// therefore never disagree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatesResp {
    /// Whether the manual halt permits NEW entries. Existing-position management is unaffected.
    pub entries_enabled: bool,
    /// Whether analyst calls and analyst-driven position management are active.
    pub analysts_enabled: bool,
    pub kill: KillGate,
    pub per_analyst_positions: Vec<AnalystPositionGate>,
    pub global_positions: PositionGate,
    pub daily: DailyGate,
    pub morning: MorningGate,
    pub staleness: StalenessGate,
    pub regime: RegimeGate,
    /// Markets with at least one entry today, so the dashboard shows the churn budget being
    /// spent — not only the markets already capped out.
    pub per_market: Vec<PerMarketGate>,
    /// Only cooldowns still open at request time.
    pub cooldowns: Vec<CooldownGate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillGate {
    /// Whether the kill latch is enabled for this run. Disabled means it can never block.
    pub enabled: bool,
    /// The latch itself: true means every entry is refused until 00:00 UTC.
    pub active: bool,
    /// Equity the day was opened at (`day_open:<utc-day>` in meta); the kill floor is
    /// `day_open * (1 - threshold_px_pct/100)`.
    pub day_open: f64,
    /// Drawdown threshold in percent — user-locked at 12.
    pub threshold_px_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyGate {
    pub count: usize,
    pub cap: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PositionGate {
    pub count: usize,
    pub cap: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalystPositionGate {
    pub analyst: String,
    pub count: usize,
    pub cap: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MorningGate {
    /// The budget only binds while this is true (12:00:00.000 UTC releases it).
    pub before_noon_utc: bool,
    /// Entries taken today — before noon that IS the morning count, which is why the pacing
    /// gate needs no separate counter.
    pub count: usize,
    pub budget: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StalenessGate {
    /// Age of the state an entry would rest on: max(snapshot stamp age, ws frame age).
    pub age_ms: i64,
    pub max_ms: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegimeGate {
    /// `null` when the BTC row or its features are absent — the gate is then inactive.
    pub btc_vol1h: Option<f64>,
    pub max: f64,
    /// True only while the gate is REFUSING entries (vol above max), not merely configured.
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerMarketGate {
    pub market: String,
    pub entries_today: i64,
    pub cap: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CooldownGate {
    pub market: String,
    pub until_ts: i64,
    /// `"sl"` when the longer post-stop window is what is holding the market out, else
    /// `"other"` (the base window after a tp / veto / time stop).
    pub cause: String,
}

/// Assemble the payload. Ledger reads and lock reads are interleaved by the helpers, each of
/// which releases its guard before returning — nothing is held across I/O.
async fn collect_gates(s: &AppState, now_ms: i64) -> GatesResp {
    let status = s.entry_gate.status(now_ms).await;
    let cooldowns = s.entry_gate.cooldowns(now_ms).await;
    let per_market = s.entry_gate.per_market_entries(now_ms).await;
    let day_open = s
        .store
        .day_open_equity(&crate::utc_day_key(now_ms))
        .await
        .unwrap_or(None)
        .unwrap_or(0.0);
    let cfg = &status.cfg;
    GatesResp {
        entries_enabled: cfg.entries_enabled,
        analysts_enabled: s.analysts_enabled,
        kill: KillGate {
            enabled: cfg.kill_enabled,
            active: cfg.kill_enabled && status.kill_active,
            day_open,
            threshold_px_pct: cfg.kill_switch_pct,
        },
        per_analyst_positions: status
            .per_analyst_open_counts
            .into_iter()
            .map(|(analyst, count)| AnalystPositionGate {
                analyst,
                count,
                cap: cfg.max_concurrent,
            })
            .collect(),
        global_positions: PositionGate {
            count: status.global_open_count,
            cap: cfg.global_max_concurrent,
        },
        daily: DailyGate {
            count: status.daily_count,
            cap: cfg.daily_cap,
        },
        morning: MorningGate {
            before_noon_utc: crate::risk::is_before_utc_noon(now_ms),
            count: status.daily_count,
            budget: cfg.morning_entry_budget,
        },
        staleness: StalenessGate {
            age_ms: status.data_age_ms,
            max_ms: (cfg.max_feature_age_s as i64).saturating_mul(1000),
            stale: status.stale,
        },
        regime: RegimeGate {
            btc_vol1h: status.btc_vol1h,
            max: cfg.regime_vol_max,
            active: status.regime_blocking,
        },
        per_market: per_market
            .into_iter()
            .map(|(market, entries_today)| PerMarketGate {
                market,
                entries_today,
                cap: cfg.per_market_daily_cap,
            })
            .collect(),
        cooldowns: cooldowns
            .into_iter()
            .map(|c| CooldownGate {
                market: c.market,
                until_ts: c.until_ts,
                cause: c.cause.as_str().to_string(),
            })
            .collect(),
    }
}

async fn gates_handler(State(s): State<AppState>) -> axum::response::Response {
    let now_ms = chrono::Utc::now().timestamp_millis();
    match tokio::time::timeout(STATE_TIMEOUT, collect_gates(&s, now_ms)).await {
        Ok(resp) => Json(resp).into_response(),
        Err(_) => {
            warn!(
                endpoint = "/api/gates",
                "gate state unavailable within timeout"
            );
            state_unavailable()
        }
    }
}

/// Decision-quality analytics. Deliberately NOT bounded by `STATE_TIMEOUT`: it touches no
/// ws-fed shared state (so it cannot wedge), and its one slow phase — replaying veto
/// counterfactuals against HL candles — carries its own work and time budget in
/// `analytics::compute_pending`.
async fn analytics_handler(State(s): State<AppState>) -> impl IntoResponse {
    let now_ms = chrono::Utc::now().timestamp_millis();
    Json(crate::analytics::build(&s.store, &s.hl, now_ms).await)
}

async fn news_handler(
    State(s): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let rows: Vec<NewsRow> = sqlx::query_as(
        "SELECT id, ts, source, title, body, url FROM news ORDER BY ts DESC LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(s.store.pool())
    .await
    .unwrap_or_default();
    let mut out = Vec::new();
    for r in rows {
        let markets: Vec<(String,)> =
            sqlx::query_as("SELECT market FROM news_markets WHERE news_id=?1")
                .bind(r.id)
                .fetch_all(s.store.pool())
                .await
                .unwrap_or_default();
        let ms = markets.into_iter().map(|x| x.0).collect();
        out.push(NewsItem {
            id: r.id,
            ts: r.ts,
            source: r.source,
            title: r.title,
            body: r.body,
            url: r.url,
            markets: ms,
        });
    }
    Json(out)
}
#[derive(sqlx::FromRow)]
struct NewsRow {
    id: i64,
    ts: i64,
    source: String,
    title: String,
    body: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct IngestReq {
    source: String,
    ts: Option<i64>,
    title: String,
    body: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IngestResp {
    ok: bool,
    id: i64,
    deduped: bool,
}

async fn ingest_handler(
    State(s): State<AppState>,
    Json(req): Json<IngestReq>,
) -> axum::response::Response {
    let ts = req
        .ts
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let body = req.body.unwrap_or_default();
    let url = req.url.unwrap_or_default();
    match s
        .news
        .ingest(&req.source, ts, &req.title, &body, &url)
        .await
    {
        Ok(res) => {
            if !res.deduped {
                let markets = s.news.match_markets(&format!("{} {}", req.title, body));
                if req.source == "tg:trenchers_den" && crate::news::is_directional_call(&req.title)
                {
                    let force_markets = s.news.match_markets(&req.title);
                    if let Err(e) = s.force_nominees.try_send(force_markets) {
                        warn!(error = %e, "force nominee queue unavailable");
                    }
                }
                let item = NewsItem {
                    id: res.id,
                    ts,
                    source: req.source,
                    title: req.title,
                    body,
                    url,
                    markets,
                };
                let _ = s.tx.send(WsMsg::News(item));
            }
            Json(IngestResp {
                ok: true,
                id: res.id,
                deduped: res.deduped,
            })
            .into_response()
        }
        Err(e) => {
            debug!(error=%e, "ingest failed");
            let body = serde_json::json!({"ok":false, "error": e.to_string()});
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

async fn ws_handler(State(s): State<AppState>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, s))
}

async fn handle_ws(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();
    let mut pending_mids: Option<WsMsg> = None;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // send initial ping? not needed
    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(m) => {
                        match &m {
                            WsMsg::Mids(_) => {
                                pending_mids = Some(m);
                                // try to send immediately if interval already ticked?
                                // We'll let interval tick send it to throttle
                            }
                            _ => {
                                let txt = serde_json::to_string(&m).unwrap_or_default();
                                if socket.send(axum::extract::ws::Message::Text(txt.into())).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
            _ = interval.tick() => {
                if let Some(m) = pending_mids.take() {
                    let txt = serde_json::to_string(&m).unwrap_or_default();
                    if socket.send(axum::extract::ws::Message::Text(txt.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NewsCfg;
    use crate::ledger::Store;
    use crate::news::News;
    use std::collections::HashMap;
    use std::time::Instant;
    use tokio::net::TcpListener;

    fn test_risk_cfg() -> crate::config::RiskCfg {
        crate::config::RiskCfg {
            max_concurrent: 5,
            global_max_concurrent: 12,
            daily_cap: 20,
            cooldown_min: 30,
            kill_switch_pct: 12.0,
            kill_enabled: false,
            entries_enabled: true,
            conviction_min: 0.50,
            review_interval_min: 15,
            time_stop_hours: 24.0,
            max_feature_age_s: 120,
            per_market_daily_cap: 3,
            cooldown_after_sl_min: 120,
            morning_entry_budget: 12,
            regime_vol_max: 1.5,
            min_rr: 2.0,
        }
    }

    async fn spawn_test_server() -> (String, AppState, broadcast::Sender<WsMsg>) {
        spawn_test_server_with(test_risk_cfg(), "http://127.0.0.1:1").await
    }

    #[test]
    fn chat_action_uses_the_last_fenced_json_block() {
        let reply = "text\n```json\n{\"type\":\"open\",\"market\":\"ETH\",\"side\":\"short\",\"sl_pct\":1.2,\"tp_pct\":2.4,\"conviction\":0.8}\n```\n```json\n{\"type\":\"close\",\"market\":\"SOL\"}\n```";
        let action = parse_chat_action(reply).expect("action");
        assert_eq!(action.kind, "close");
        assert_eq!(action.market, "SOL");
    }

    #[test]
    fn chat_prompt_accepts_ui_messages_and_legacy_body() {
        let ui = serde_json::json!({"id":"turn","messages":[
            {"role":"user","parts":[{"type":"text","text":"first"}]},
            {"role":"assistant","parts":[{"type":"text","text":"ignored"}]},
            {"role":"user","parts":[{"type":"text","text":"last "},{"type":"file"},{"type":"text","text":"prompt"}]}
        ],"trigger":"submit-message"});
        assert_eq!(chat_prompt(ui).as_deref(), Some("last prompt"));
        assert_eq!(chat_prompt(serde_json::json!({"message":" legacy "})).as_deref(), Some("legacy"));
    }

    #[test]
    fn chat_reason_names_the_configured_model() {
        assert_eq!(chat_reason("test-model", 1234), "chat; model test-model refused:false latency:1234");
    }

    fn chat_market(market: &str) -> crate::contracts::MarketRow {
        crate::contracts::MarketRow {
            market: market.into(), mid: 100.0, mark: 100.0, oracle: 100.0, funding: 0.0,
            open_interest: 0.0, day_ntl_vlm: 1_000_000.0, prev_day_px: 100.0,
            features: Some(crate::contracts::Features { r5m: 0.0, r1h: 0.0, r24h: 0.0, vol1h: 1.0, funding_z: 0.0, range_pos: 0.5 }),
        }
    }

    #[tokio::test]
    async fn executed_chat_open_is_logged_once_as_a_decision() {
        let (_url, state, _tx) = spawn_test_server().await;
        state.snapshot.write().await.markets = vec![chat_market("SOL")];
        let mut action = ChatAction {
            kind: "open".into(), market: "SOL".into(), side: Some(Side::Long),
            sl_pct: Some(1.2), tp_pct: Some(2.4), conviction: Some(0.8),
            executed: false, gate_refusals: vec![],
        };

        execute_chat_action(&state, &mut action, "chat; model test refused:false latency:1").await;

        let rows: Vec<(String, String, f64, i64, String)> = sqlx::query_as(
            "SELECT action, side, conviction, executed, reason FROM decisions WHERE market='SOL'",
        ).fetch_all(state.store.pool()).await.expect("decision row");
        assert_eq!(rows.len(), 1, "the chat path writes exactly one decision row");
        assert_eq!(rows[0].0, "open");
        assert_eq!(rows[0].1, "long");
        assert_eq!(rows[0].2, 0.8);
        assert_eq!(rows[0].3, 1);
        assert!(rows[0].4.starts_with("chat;"));
    }

    #[tokio::test]
    async fn refused_chat_open_is_logged_as_skip_with_its_gate_kind() {
        let (_url, state, _tx) = spawn_test_server().await;
        state.snapshot.write().await.markets = vec![chat_market("SOL")];
        let mut action = ChatAction {
            kind: "open".into(), market: "SOL".into(), side: Some(Side::Long),
            sl_pct: None, tp_pct: None, conviction: Some(0.49), executed: false, gate_refusals: vec![],
        };

        execute_chat_action(&state, &mut action, "chat; model test refused:false latency:1").await;

        let row: (String, i64, String) = sqlx::query_as(
            "SELECT action, executed, reason FROM decisions WHERE market='SOL'",
        ).fetch_one(state.store.pool()).await.expect("refused decision row");
        assert_eq!(row.0, "skip");
        assert_eq!(row.1, 0);
        assert!(row.2.starts_with("chat;"));
        assert!(row.2.contains("gate_refused:LowConviction"));
    }

    #[tokio::test]
    async fn manual_halt_refuses_chat_open_but_allows_chat_close() {
        let (_url, state, _tx) = spawn_test_server_with(
            crate::config::RiskCfg { entries_enabled: false, ..test_risk_cfg() },
            "http://127.0.0.1:1",
        )
        .await;
        state.snapshot.write().await.markets = vec![chat_market("SOL")];
        let mut open = ChatAction {
            kind: "open".into(), market: "SOL".into(), side: Some(Side::Long),
            sl_pct: None, tp_pct: None, conviction: Some(0.8), executed: false, gate_refusals: vec![],
        };
        execute_chat_action(&state, &mut open, "chat; model test refused:false latency:1").await;
        assert!(!open.executed);
        assert_eq!(open.gate_refusals, vec!["EntriesHalted"]);

        let position = state.store.open_position("SOL", Side::Long, &crate::sizing::Sized {
            leverage: 10.0, margin: 20.0, notional: 200.0, stop_pct: 1.0, tp_pct: 2.0,
        }, 100.0, false, 24.0, None).await.expect("pre-existing position");
        let mut close = ChatAction {
            kind: "close".into(), market: "SOL".into(), side: None,
            sl_pct: None, tp_pct: None, conviction: None, executed: false, gate_refusals: vec![],
        };
        execute_chat_action(&state, &mut close, "chat; model test refused:false latency:1").await;
        assert!(close.executed);
        assert!(close.gate_refusals.is_empty());
        assert!(state.store.open_positions().await.expect("positions").is_empty());
        assert_eq!(position.market, "SOL");
    }

    #[tokio::test]
    async fn disabled_analysts_chat_returns_local_notice_persists_history_and_skips_upstream() {
        let hits = Arc::new(AtomicUsize::new(0));
        let upstream_hits = hits.clone();
        let upstream = Router::new().route("/responses", post(move || {
            let hits = upstream_hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                Json(serde_json::json!({"output_text":"unexpected upstream response"}))
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind upstream");
        let addr = listener.local_addr().expect("upstream address");
        tokio::spawn(async move { axum::serve(listener, upstream).await.expect("serve upstream"); });

        let (_url, mut state, _tx) = spawn_test_server().await;
        state.analysts_enabled = false;
        state.analyst = Some(Arc::new(crate::analyst::Analyst::new(crate::config::AnalystCfg {
            enabled: true,
            base_url: format!("http://{addr}"),
            model: "test-model".into(),
            models: vec![],
            chat_model: None,
            headers: HashMap::new(),
            api_key_env: "ANALYST_API_KEY".into(),
            max_completion_tokens: None,
            web_retrieval: false,
            tavily_key_env: "TAVILY_API_KEY".into(),
            exa_key_env: "EXA_API_KEY".into(),
        })));
        let (tx, mut rx) = mpsc::channel(16);
        run_chat_turn(state.clone(), "status?".into(), tx).await;
        let mut frames = Vec::new();
        while let Ok(frame) = rx.try_recv() { frames.push(frame); }

        assert_eq!(hits.load(Ordering::SeqCst), 0, "disabled chat must not call upstream");
        assert!(frames.iter().any(|frame| frame.contains("Analysts are currently disabled (manage-only mode).")));
        assert!(!frames.iter().any(|frame| frame.contains("data-action")), "local response has action=null");
        let history: Vec<(String, String, Option<String>)> = sqlx::query_as("SELECT role,text,action_json FROM chat_messages ORDER BY rowid")
            .fetch_all(state.store.pool()).await.expect("chat history");
        assert_eq!(history, vec![
            ("user".into(), "status?".into(), None),
            ("analyst".into(), "Analysts are currently disabled (manage-only mode).".into(), None),
        ]);
    }

    #[tokio::test]
    async fn transparency_migration_creates_tables() {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let calls: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM analyst_calls")
            .fetch_one(store.pool())
            .await
            .expect("calls");
        let messages: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chat_messages")
            .fetch_one(store.pool())
            .await
            .expect("messages");
        assert_eq!((calls.0, messages.0), (0, 0));
    }

    /// `hl_base` is unroutable by default: a test that accidentally triggers a counterfactual
    /// fetch must fail fast locally, never reach the real venue.
    async fn spawn_test_server_with(
        risk_cfg: crate::config::RiskCfg,
        hl_base: &str,
    ) -> (String, AppState, broadcast::Sender<WsMsg>) {
        let store = Store::open("sqlite::memory:").await.expect("store");
        let (tx, _rx) = broadcast::channel::<WsMsg>(256);
        let cfg = NewsCfg {
            rss: vec![],
            tavily_key_env: "".into(),
            extra_keywords: HashMap::new(),
        };
        let news = Arc::new(News::new(store.clone(), cfg));
        let now = chrono::Utc::now().timestamp_millis();
        let snapshot = Arc::new(RwLock::new(Snapshot {
            ts: now,
            markets: vec![],
        }));
        let ws_fresh = {
            let f = Arc::new(crate::hl_ws::WsFreshness::new());
            f.mark_mids(now);
            f
        };
        let entry_gate = crate::EntryGate {
            risk: Arc::new(crate::risk::Risk::new(risk_cfg)),
            store: store.clone(),
            gate: Arc::new(tokio::sync::Mutex::new(crate::risk::GateState::new())),
            snapshot: snapshot.clone(),
            ws_fresh: ws_fresh.clone(),
            regime_once: Arc::new(crate::alerts::DayOnce::new()),
            boot_ms: now,
        };
        let chat_gate = entry_gate.gate.clone();
        let state = AppState {
            store,
            snapshot,
            nominees: Arc::new(RwLock::new(vec![])),
            tx: tx.clone(),
            start: Instant::now(),
            ws_fresh,
            markets_tracked: Arc::new(AtomicUsize::new(0)),
            news,
            entry_gate,
            hl: crate::hl_rest::HlRest::new(hl_base),
            force_nominees: mpsc::channel(64).0,
            analyst_failure_streak: Arc::new(AtomicU64::new(0)),
            analysts_enabled: true,
            analyst: None,
            analyst_model: "test-model".into(),
            analyst_roster: vec!["test-model".into()],
            sizing: crate::config::SizingCfg {
                bankroll: 1000.0,
                vol_ref: 1.0,
                margin_min: 10.0,
                margin_max: 100.0,
                stop_floor_pct: 1.0,
                tp_mult: 2.0,
            },
            chat_gate,
        };
        let app = build_router(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        // give server a moment
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        (url, state, tx)
    }

    #[tokio::test]
    async fn every_get_200_and_shape() {
        let (url, _state, _tx) = spawn_test_server().await;
        let client = reqwest::Client::new();
        // health
        let resp = client
            .get(format!("{url}/api/health"))
            .send()
            .await
            .expect("health");
        assert_eq!(resp.status(), 200);
        let h: Health = resp.json().await.expect("health json");
        assert!(h.ok);
        // snapshot
        let resp = client
            .get(format!("{url}/api/snapshot"))
            .send()
            .await
            .expect("snap");
        assert_eq!(resp.status(), 200);
        let snap: Snapshot = resp.json().await.expect("snap json");
        assert!(snap.ts > 0);
        // nominees
        let resp = client
            .get(format!("{url}/api/nominees"))
            .send()
            .await
            .expect("nom");
        assert_eq!(resp.status(), 200);
        let nom: Vec<Nominee> = resp.json().await.expect("nom json");
        assert!(nom.is_empty() || !nom.is_empty());
        // positions
        let resp = client
            .get(format!("{url}/api/positions"))
            .send()
            .await
            .expect("pos");
        assert_eq!(resp.status(), 200);
        let pos: serde_json::Value = resp.json().await.expect("pos json");
        assert!(pos.is_array());
        // trades
        let resp = client
            .get(format!("{url}/api/trades?limit=10"))
            .send()
            .await
            .expect("trades");
        assert_eq!(resp.status(), 200);
        let trades: Vec<Trade> = resp.json().await.expect("trades json");
        assert!(trades.len() <= 10);
        // equity
        let resp = client
            .get(format!("{url}/api/equity?points=10"))
            .send()
            .await
            .expect("equity");
        assert_eq!(resp.status(), 200);
        let eq: Vec<EquityPoint> = resp.json().await.expect("eq json");
        assert!(eq.len() <= 10);
        // decisions
        let resp = client
            .get(format!("{url}/api/decisions?limit=5"))
            .send()
            .await
            .expect("dec");
        assert_eq!(resp.status(), 200);
        let dec: Vec<DecisionLog> = resp.json().await.expect("dec json");
        assert!(dec.len() <= 5);
        // news
        let resp = client
            .get(format!("{url}/api/news?limit=5"))
            .send()
            .await
            .expect("news");
        assert_eq!(resp.status(), 200);
        let news: Vec<NewsItem> = resp.json().await.expect("news json");
        assert!(news.len() <= 5);
        // gates
        let resp = client
            .get(format!("{url}/api/gates"))
            .send()
            .await
            .expect("gates");
        assert_eq!(resp.status(), 200);
        let gates: GatesResp = resp.json().await.expect("gates json");
        assert_eq!(gates.daily.cap, 20, "caps come from the live RiskCfg");
        assert!(
            gates.per_market.is_empty() && gates.cooldowns.is_empty(),
            "empty ledger, empty rows"
        );
        // analytics (empty ledger: no counterfactual work, so no venue call)
        let resp = client
            .get(format!("{url}/api/analytics"))
            .send()
            .await
            .expect("analytics");
        assert_eq!(resp.status(), 200);
        let an: crate::analytics::AnalyticsResp = resp.json().await.expect("analytics json");
        assert_eq!(
            an.conviction_buckets.len(),
            3,
            "all buckets always reported"
        );
        assert_eq!(
            an.counterfactuals,
            crate::analytics::CounterfactualSummary {
                computed: 0,
                pending: 0,
                net_actual: 0.0,
                net_bracket: 0.0,
                rows: vec![],
            },
            "empty ledger: totals zeroed and the detail table empty, never absent"
        );
    }

    #[tokio::test]
    async fn positions_payload_carries_the_persisted_analyst() {
        let (_url, state, _tx) = spawn_test_server().await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        state
            .store
            .open_position_for_analyst(
                "SOL",
                Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
                "alpha",
        )
        .await
        .expect("open position");
        state
            .store
            .open_position("ETH", Side::Long, &sized, 100.0, false, 24.0, None)
            .await
            .expect("legacy open position");

        let positions = body_json(positions_handler(State(state)).await).await;
        let analysts: HashMap<_, _> = positions
            .as_array()
            .expect("positions array")
            .iter()
            .map(|position| {
                (
                    position["market"].as_str().expect("market"),
                    position["analyst"].as_str().expect("analyst"),
                )
            })
            .collect();
        assert_eq!(analysts["SOL"], "alpha");
        assert_eq!(analysts["ETH"], "", "legacy analysts stay empty");
    }

    #[tokio::test]
    async fn analyst_transparency_payloads_all_include_an_analyst() {
        let (_url, state, _tx) = spawn_test_server().await;
        state
            .store
            .log_decision_for_analyst(
                1,
                "SOL",
                "open",
                "long",
                0.9,
                "thesis",
                24.0,
                false,
                true,
                "ok",
                "alpha",
                None,
            )
            .await
            .expect("decision");
        sqlx::query("INSERT INTO analyst_calls (ts,market,trigger,prompt,response_raw,outcome_kind,latency_ms,analyst) VALUES (1,'SOL','decide','prompt','reply','ok',1,'bravo')")
            .execute(state.store.pool())
            .await
            .expect("call");
        sqlx::query("INSERT INTO chat_messages (ts,role,text) VALUES (1,'analyst','reply')")
            .execute(state.store.pool())
            .await
            .expect("chat message");

        let decisions = body_json(
            decisions_handler(State(state.clone()), Query(HashMap::new()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(decisions[0]["analyst"], serde_json::json!("alpha"));

        let calls = body_json(
            analyst_calls_handler(State(state.clone()), Query(HashMap::new()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(calls["calls"][0]["analyst"], serde_json::json!("bravo"));

        let history = body_json(
            chat_history_handler(State(state), Query(HashMap::new()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(history["messages"][0]["analyst"], serde_json::json!("test-model"));
    }

    #[tokio::test]
    async fn post_ingest_dedupe() {
        let (url, _state, _tx) = spawn_test_server().await;
        let client = reqwest::Client::new();
        let body = serde_json::json!({"source":"test","title":"Hello","body":"world","url":"http://example.com"});
        let resp = client
            .post(format!("{url}/ingest/news"))
            .json(&body)
            .send()
            .await
            .expect("ingest1");
        assert_eq!(resp.status(), 200);
        let v: IngestResp = resp.json().await.expect("resp1");
        assert!(!v.deduped);
        let resp2 = client
            .post(format!("{url}/ingest/news"))
            .json(&body)
            .send()
            .await
            .expect("ingest2");
        assert_eq!(resp2.status(), 200);
        let v2: IngestResp = resp2.json().await.expect("resp2");
        assert!(v2.deduped, "second should be deduped");
    }

    #[tokio::test]
    async fn directional_trenchers_call_forwards_its_matched_market_only() {
        let (_url, mut state, _tx) = spawn_test_server().await;
        let (force_tx, mut force_rx) = mpsc::channel(1);
        state.force_nominees = force_tx;
        let response = ingest_handler(
            State(state.clone()),
            Json(IngestReq {
                source: "tg:trenchers_den".into(),
                ts: None,
                title: "SOL LONG entry 90.16 sl 89.85 tp 91.75".into(),
                body: None,
                url: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(force_rx.recv().await, Some(vec!["SOL".into()]));

        let response = ingest_handler(
            State(state),
            Json(IngestReq {
                source: "tg:trenchers_den".into(),
                ts: None,
                title: "cons tp reached gg, move sl to entry".into(),
                body: None,
                url: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(force_rx.try_recv().is_err());
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn leaderboard_includes_enabled_models_without_positions() {
        let (_url, mut state, _tx) = spawn_test_server().await;
        state.analyst_roster = vec!["alpha".into(), "bravo".into()];
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        state
            .store
            .open_position_for_analyst("SOL", Side::Long, &sized, 100.0, false, 24.0, None, "alpha")
            .await
            .expect("open alpha position");
        for (action, conviction, executed) in [("open", 0.812, true), ("open", 0.863, true), ("skip", 0.9, false)] {
            state
                .store
                .log_decision_for_analyst(0, "SOL", action, "long", conviction, "", 24.0, false, executed, "", "alpha", None)
                .await
                .expect("log alpha decision");
        }

        let body = body_json(
            analyst_leaderboard_handler(State(state)).await.into_response(),
        )
        .await;

        assert_eq!(
            body["models"],
            serde_json::json!([
                {
                    "model": "alpha",
                    "enabled": true,
                    "positions_open": 1,
                    "closes": 0,
                    "wins": 0,
                    "win_rate": 0.0,
                    "realized_pnl": 0.0,
                    "unrealized_pnl": 0.0,
                    "decides": 3,
                    "avg_entry_conviction": 0.84,
                },
                {
                    "model": "bravo",
                    "enabled": true,
                    "positions_open": 0,
                    "closes": 0,
                    "wins": 0,
                    "win_rate": 0.0,
                    "realized_pnl": 0.0,
                    "unrealized_pnl": 0.0,
                    "decides": 0,
                    "avg_entry_conviction": null,
                },
            ])
        );
    }

    /// Regression for the 2026-08-09 wedge: with the shared state locked out from under
    /// them, the state-reading handlers must answer 503 fast instead of hanging forever,
    /// and /api/health must keep answering 200 regardless.
    #[tokio::test]
    async fn wedged_state_returns_503_and_health_still_answers() {
        let (_url, state, _tx) = spawn_test_server().await;
        // A writer that never lets go — exactly what the lock inversion produced.
        let _snap_guard = state.snapshot.write().await;
        let _nom_guard = state.nominees.write().await;

        let started = Instant::now();
        let (snap_resp, pos_resp, nom_resp, health_resp) = tokio::join!(
            snapshot_handler(State(state.clone())),
            positions_handler(State(state.clone())),
            nominees_handler(State(state.clone())),
            async { health_handler(State(state.clone())).await.into_response() },
        );
        let elapsed = started.elapsed();

        assert_eq!(
            snap_resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "/api/snapshot must 503"
        );
        assert_eq!(
            pos_resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "/api/positions must 503"
        );
        assert_eq!(
            nom_resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "/api/nominees must 503"
        );
        assert_eq!(
            health_resp.status(),
            StatusCode::OK,
            "/api/health must never fail on a wedge"
        );

        assert_eq!(
            body_json(snap_resp).await,
            serde_json::json!({"error": "state unavailable"})
        );
        assert_eq!(
            body_json(pos_resp).await,
            serde_json::json!({"error": "state unavailable"})
        );
        assert_eq!(
            body_json(nom_resp).await,
            serde_json::json!({"error": "state unavailable"})
        );
        // health degrades to what it knows (markets_tracked 0) rather than blocking
        let h = body_json(health_resp).await;
        assert_eq!(h["ok"], serde_json::json!(true));
        assert_eq!(h["markets_tracked"], serde_json::json!(0));

        // bounded: every handler gave up at STATE_TIMEOUT, none waited on another's timeout
        assert!(
            elapsed >= STATE_TIMEOUT && elapsed < STATE_TIMEOUT * 2,
            "handlers must bound at STATE_TIMEOUT, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn healthy_state_reads_snapshot_normally() {
        let (_url, state, _tx) = spawn_test_server().await;
        state.snapshot.write().await.markets = vec![crate::contracts::MarketRow {
            market: "SOL".into(),
            mid: 100.0,
            mark: 100.0,
            oracle: 100.0,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: 100.0,
            features: None,
        }];
        let resp = snapshot_handler(State(state.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["markets"][0]["market"], serde_json::json!("SOL"));
        // markets_tracked 0 -> health takes the snapshot fallback and sees the row
        let h = body_json(health_handler(State(state.clone())).await.into_response()).await;
        assert_eq!(h["markets_tracked"], serde_json::json!(1));
    }

    #[tokio::test]
    async fn health_ws_connected_follows_stream_freshness() {
        let (_url, state, _tx) = spawn_test_server().await;
        // spawn_test_server stamps a fresh mids message
        let h = body_json(health_handler(State(state.clone())).await.into_response()).await;
        assert_eq!(
            h["ws_connected"],
            serde_json::json!(true),
            "fresh stream -> connected"
        );
        // streams die: last message ages past the window, flag must flip with no other input
        let stale = chrono::Utc::now().timestamp_millis() - crate::hl_ws::WS_FRESH_WINDOW_MS - 1;
        state.ws_fresh.mark_mids(stale);
        let h = body_json(health_handler(State(state.clone())).await.into_response()).await;
        assert_eq!(
            h["ws_connected"],
            serde_json::json!(false),
            "stale stream -> disconnected"
        );
        // ctxs alone reviving is enough
        state
            .ws_fresh
            .mark_ctxs(chrono::Utc::now().timestamp_millis());
        let h = body_json(health_handler(State(state.clone())).await.into_response()).await;
        assert_eq!(
            h["ws_connected"],
            serde_json::json!(true),
            "either stream fresh -> connected"
        );
        state.analyst_failure_streak.store(12, Ordering::SeqCst);
        let h = body_json(health_handler(State(state)).await.into_response()).await;
        assert_eq!(h["analyst_failure_streak"], serde_json::json!(12));
    }

    // ── T2: /api/gates ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn gates_payload_round_trips_with_the_field_names_the_dashboard_reads() {
        let resp = GatesResp {
            entries_enabled: false,
            analysts_enabled: false,
            kill: KillGate {
                enabled: true,
                active: true,
                day_open: 1000.0,
                threshold_px_pct: 12.0,
            },
            per_analyst_positions: vec![AnalystPositionGate {
                analyst: "alpha".into(),
                count: 7,
                cap: 0,
            }],
            global_positions: PositionGate { count: 9, cap: 0 },
            daily: DailyGate { count: 7, cap: 20 },
            morning: MorningGate {
                before_noon_utc: true,
                count: 7,
                budget: 12,
            },
            staleness: StalenessGate {
                age_ms: 4_000,
                max_ms: 120_000,
                stale: false,
            },
            regime: RegimeGate {
                btc_vol1h: Some(0.42),
                max: 1.5,
                active: false,
            },
            per_market: vec![PerMarketGate {
                market: "SOL".into(),
                entries_today: 3,
                cap: 3,
            }],
            cooldowns: vec![CooldownGate {
                market: "SOL".into(),
                until_ts: 1_786_284_000_000,
                cause: "sl".into(),
            }],
        };
        let json = serde_json::to_value(&resp).expect("serialize");
        assert_eq!(json["entries_enabled"], serde_json::json!(false));
        assert_eq!(json["analysts_enabled"], serde_json::json!(false));
        assert_eq!(json["kill"]["threshold_px_pct"], serde_json::json!(12.0));
        assert_eq!(json["per_analyst_positions"][0]["count"], serde_json::json!(7));
        assert_eq!(json["per_analyst_positions"][0]["cap"], serde_json::json!(0));
        assert_eq!(json["global_positions"], serde_json::json!({"count": 9, "cap": 0}));
        assert_eq!(json["daily"]["cap"], serde_json::json!(20));
        assert_eq!(json["morning"]["before_noon_utc"], serde_json::json!(true));
        assert_eq!(json["staleness"]["max_ms"], serde_json::json!(120_000));
        assert_eq!(json["regime"]["btc_vol1h"], serde_json::json!(0.42));
        assert_eq!(json["per_market"][0]["entries_today"], serde_json::json!(3));
        assert_eq!(json["cooldowns"][0]["cause"], serde_json::json!("sl"));
        // an absent BTC row must serialize as null, not 0.0 (0.0 would read as "very calm")
        let mut blind = resp.clone();
        blind.regime.btc_vol1h = None;
        assert_eq!(
            serde_json::to_value(&blind).unwrap()["regime"]["btc_vol1h"],
            serde_json::Value::Null
        );
        let back: GatesResp = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, resp);
    }

    /// The endpoint must report the rails as the entry path sees them: real ledger counts, the
    /// live kill latch, the same staleness/regime verdicts.
    #[tokio::test]
    async fn gates_handler_reports_the_live_chain() {
        let (_url, state, _tx) = spawn_test_server_with(
            crate::config::RiskCfg {
                kill_enabled: true,
                entries_enabled: false,
                max_concurrent: 0,
                global_max_concurrent: 0,
                ..test_risk_cfg()
            },
            "http://127.0.0.1:1",
        )
        .await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let now = chrono::Utc::now().timestamp_millis();
        state
            .store
            .set_day_open_equity(&crate::utc_day_key(now), 1234.5)
            .await
            .expect("day open");
        // two SOL entries today, the last of them stopped out a minute ago
        let p1 = state
            .store
            .open_position(
                "SOL",
                crate::contracts::Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("p1");
        state
            .store
            .close_position(p1.id, 99.0, "sl", None)
            .await
            .expect("sl close");
        state
            .store
            .open_position(
                "SOL",
                crate::contracts::Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("p2");
        // kill latch on, and a hot BTC row so the regime gate is refusing
        state.entry_gate.gate.lock().await.kill_active = true;
        state.snapshot.write().await.markets = vec![crate::contracts::MarketRow {
            market: "BTC".into(),
            mid: 60_000.0,
            mark: 60_000.0,
            oracle: 60_000.0,
            funding: 0.0,
            open_interest: 0.0,
            day_ntl_vlm: 0.0,
            prev_day_px: 60_000.0,
            features: Some(crate::contracts::Features {
                r5m: 0.0,
                r1h: 0.0,
                r24h: 0.0,
                vol1h: 3.0,
                funding_z: 0.0,
                range_pos: 0.5,
            }),
        }];

        let v = body_json(gates_handler(State(state.clone())).await).await;
        assert_eq!(v["entries_enabled"], serde_json::json!(false));
        assert_eq!(v["analysts_enabled"], serde_json::json!(true));
        assert_eq!(
            v["kill"]["active"],
            serde_json::json!(true),
            "kill latch must surface"
        );
        assert_eq!(v["kill"]["enabled"], serde_json::json!(true));
        assert_eq!(v["kill"]["day_open"], serde_json::json!(1234.5));
        assert_eq!(
            v["per_analyst_positions"],
            serde_json::json!([{"analyst": "", "count": 1, "cap": 0}]),
            "per-analyst open count stays truthful when zero disables the cap"
        );
        assert_eq!(
            v["global_positions"],
            serde_json::json!({"count": 1, "cap": 0}),
            "global open count stays truthful when zero disables the cap"
        );
        assert_eq!(
            v["kill"]["threshold_px_pct"],
            serde_json::json!(12.0),
            "user-locked 12% untouched"
        );
        assert_eq!(
            v["daily"]["count"],
            serde_json::json!(2),
            "daily count is the ledger's, not a memory counter"
        );
        assert_eq!(
            v["morning"]["count"],
            serde_json::json!(2),
            "morning count is the same day counter"
        );
        assert_eq!(v["morning"]["budget"], serde_json::json!(12));
        assert_eq!(v["regime"]["btc_vol1h"], serde_json::json!(3.0));
        assert_eq!(
            v["regime"]["active"],
            serde_json::json!(true),
            "vol 3.0 > 1.5 must read as blocking"
        );
        assert_eq!(
            v["staleness"]["stale"],
            serde_json::json!(false),
            "fresh snapshot is not stale"
        );
        assert!(v["staleness"]["age_ms"].as_i64().unwrap() < 120_000);
        // per-market: SOL took two entries today against a cap of 3
        assert_eq!(v["per_market"].as_array().unwrap().len(), 1);
        assert_eq!(v["per_market"][0]["market"], serde_json::json!("SOL"));
        assert_eq!(v["per_market"][0]["entries_today"], serde_json::json!(2));
        assert_eq!(v["per_market"][0]["cap"], serde_json::json!(3));
        // cooldown: the stop-out owns the window, and it is the 120m one
        assert_eq!(v["cooldowns"].as_array().unwrap().len(), 1);
        assert_eq!(v["cooldowns"][0]["market"], serde_json::json!("SOL"));
        assert_eq!(v["cooldowns"][0]["cause"], serde_json::json!("sl"));
        let until = v["cooldowns"][0]["until_ts"].as_i64().expect("until_ts");
        assert!(
            until > now + 118 * 60_000 && until < now + 121 * 60_000,
            "post-SL window ~120m, got {}",
            until - now
        );
    }

    #[tokio::test]
    async fn gates_daily_count_aggregates_entries_across_analysts() {
        let (_url, state, _tx) = spawn_test_server().await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        for (market, analyst) in [
            ("SOL", "alpha"),
            ("ETH", "bravo"),
            ("HYPE", "alpha"),
            ("BTC", "charlie"),
            ("GOLD", "bravo"),
        ] {
            state
                .store
                .open_position_for_analyst(
                    market,
                    Side::Long,
                    &sized,
                    100.0,
                    false,
                    24.0,
                    None,
                    analyst,
                )
                .await
                .expect("open position");
        }

        let gates = body_json(gates_handler(State(state)).await).await;
        assert_eq!(gates["daily"]["count"], serde_json::json!(5));
    }

    #[tokio::test]
    async fn gates_handler_hides_a_latch_when_kill_is_disabled() {
        let (_url, state, _tx) = spawn_test_server().await;
        state.entry_gate.gate.lock().await.kill_active = true;
        let v = body_json(gates_handler(State(state)).await).await;
        assert_eq!(v["kill"]["enabled"], serde_json::json!(false));
        assert_eq!(v["kill"]["active"], serde_json::json!(false));
    }

    /// A market whose last close was a TP keeps the SHORT window, and it disappears from the
    /// list once the window lapses — the endpoint reports live state, not history.
    #[tokio::test]
    async fn gates_cooldowns_expire_and_non_sl_closes_use_the_base_window() {
        let (_url, state, _tx) = spawn_test_server().await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let now = chrono::Utc::now().timestamp_millis();
        let p = state
            .store
            .open_position(
                "ETH",
                crate::contracts::Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        state
            .store
            .close_position(p.id, 102.0, "tp", None)
            .await
            .expect("tp close");
        // the base window lives in memory (what gate_entry compares against)
        state
            .entry_gate
            .gate
            .lock()
            .await
            .last_close_ts
            .insert("ETH".into(), now);

        let v = body_json(gates_handler(State(state.clone())).await).await;
        assert_eq!(
            v["cooldowns"][0]["cause"],
            serde_json::json!("other"),
            "a tp keeps the base window"
        );
        let until = v["cooldowns"][0]["until_ts"].as_i64().expect("until_ts");
        assert!(
            until > now + 28 * 60_000 && until < now + 31 * 60_000,
            "base window ~30m, got {}",
            until - now
        );

        // rewind the stamp past the window: nothing active, nothing reported
        state
            .entry_gate
            .gate
            .lock()
            .await
            .last_close_ts
            .insert("ETH".into(), now - 31 * 60_000);
        let v2 = body_json(gates_handler(State(state.clone())).await).await;
        assert!(
            v2["cooldowns"].as_array().unwrap().is_empty(),
            "lapsed cooldown must not linger, got {v2:?}"
        );
    }

    /// Same contract as the other state-reading handlers: a wedged lock is a fast 503.
    #[tokio::test]
    async fn gates_handler_503_when_state_is_wedged() {
        let (_url, state, _tx) = spawn_test_server().await;
        let _snap_guard = state.snapshot.write().await;
        let started = Instant::now();
        let resp = gates_handler(State(state.clone())).await;
        let elapsed = started.elapsed();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "/api/gates must 503, never hang"
        );
        assert_eq!(
            body_json(resp).await,
            serde_json::json!({"error": "state unavailable"})
        );
        assert!(
            elapsed >= STATE_TIMEOUT && elapsed < STATE_TIMEOUT * 2,
            "bounded at STATE_TIMEOUT, took {elapsed:?}"
        );
    }

    // ── T2: /api/analytics ────────────────────────────────────────────────────────────

    /// Fake HL `/info` that always answers with the same candle array.
    async fn spawn_candle_server(body: serde_json::Value) -> String {
        let app = Router::new().route(
            "/info",
            post(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn candle_json(t: i64, o: f64, h: f64, l: f64, c: f64) -> serde_json::Value {
        serde_json::json!({
            "t": t, "T": t + 59_999, "s": "SOL", "i": "1m",
            "o": o.to_string(), "c": c.to_string(), "h": h.to_string(), "l": l.to_string(),
            "v": "1.0", "n": 1
        })
    }

    /// End to end: an executed decision, a veto close, one lazily-replayed counterfactual —
    /// cached, so the second request neither refetches nor double-counts.
    #[tokio::test]
    async fn analytics_computes_and_caches_a_veto_counterfactual() {
        let now = chrono::Utc::now().timestamp_millis();
        // three 1m candles after the close; the second tags the take-profit
        let candles = serde_json::json!([
            candle_json(now, 100.0, 100.5, 99.9, 100.2),
            candle_json(now + 60_000, 100.2, 105.0, 100.0, 104.0),
            candle_json(now + 120_000, 104.0, 104.5, 103.5, 104.2),
        ]);
        let hl_base = spawn_candle_server(candles).await;
        let (_url, state, _tx) = spawn_test_server_with(test_risk_cfg(), &hl_base).await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };

        let decision_id = state
            .store
            .log_decision(
                now - 1_000,
                "SOL",
                "open",
                "long",
                0.83,
                "momentum",
                24.0,
                false,
                false,
                "model x refused:false latency:1",
            )
            .await
            .expect("decision");
        let pos = state
            .store
            .open_position(
                "SOL",
                crate::contracts::Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        state
            .store
            .mark_decision_executed(decision_id)
            .await
            .expect("mark");
        let closed = state
            .store
            .close_position(pos.id, 100.0, "veto_close", None)
            .await
            .expect("veto close");

        let v = body_json(
            analytics_handler(State(state.clone()))
                .await
                .into_response(),
        )
        .await;
        // conviction 0.83 lands in the top bucket, as a loss (vetoed flat, paid two fees)
        assert_eq!(
            v["conviction_buckets"][2]["bucket"],
            serde_json::json!("0.80+")
        );
        assert_eq!(v["conviction_buckets"][2]["closes"], serde_json::json!(1));
        assert_eq!(v["conviction_buckets"][2]["wins"], serde_json::json!(0));
        assert_eq!(
            v["conviction_buckets"][0]["closes"],
            serde_json::json!(0),
            "lower buckets stay empty"
        );
        // per-market: one round trip, fees from both legs
        assert_eq!(v["per_market"][0]["market"], serde_json::json!("SOL"));
        assert_eq!(
            v["per_market"][0]["trades"],
            serde_json::json!(1),
            "an open+close is ONE round trip"
        );
        assert!(v["per_market"][0]["fees"].as_f64().unwrap() > 0.0);
        // exit mix: today shows the single veto close
        let today = crate::utc_day_key(now);
        let day = v["exit_mix_daily"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["date"] == serde_json::json!(today))
            .expect("today row");
        assert_eq!(day["veto_close"], serde_json::json!(1));
        assert_eq!(day["tp"], serde_json::json!(0));
        // counterfactual: the bracket would have hit tp, so it beats the flat veto exit
        let cf = &v["counterfactuals"];
        assert_eq!(cf["computed"], serde_json::json!(1));
        assert_eq!(cf["pending"], serde_json::json!(0));
        let net_actual = cf["net_actual"].as_f64().unwrap();
        let net_bracket = cf["net_bracket"].as_f64().unwrap();
        assert!(
            (net_actual - (closed.realized_pnl - closed.fee)).abs() < 1e-9,
            "actual is the close trade net of its own fee"
        );
        assert!(
            net_bracket > net_actual,
            "tp bracket {net_bracket} must beat the vetoed exit {net_actual}"
        );
        let cached: (String, f64) = sqlx::query_as(
            "SELECT bracket_outcome, bracket_pnl FROM counterfactuals WHERE position_id=?1",
        )
        .bind(pos.id)
        .fetch_one(state.store.pool())
        .await
        .expect("cache row");
        assert_eq!(cached.0, "tp");
        assert!((cached.1 - net_bracket).abs() < 1e-9);
        let expected = crate::analytics::exit_pnl(
            crate::contracts::Side::Long,
            pos.size,
            pos.entry_px,
            pos.tp_px,
        );
        assert!(
            (cached.1 - expected).abs() < 1e-9,
            "bracket pnl fills at the tp trigger minus 7.5bp: {} vs {expected}",
            cached.1
        );
        // per-position detail: the same replay, attributable to the position it judges
        let rows = cf["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 1, "one row per computed counterfactual");
        let row = &rows[0];
        assert_eq!(row["position_id"], serde_json::json!(pos.id));
        assert_eq!(row["market"], serde_json::json!("SOL"));
        assert_eq!(row["side"], serde_json::json!("long"));
        assert_eq!(
            row["closed_ts"],
            serde_json::json!(closed.ts),
            "stamped when the reviewer closed it"
        );
        assert_eq!(row["bracket_outcome"], serde_json::json!("tp"));
        assert!(
            (row["actual_pnl"].as_f64().unwrap() - net_actual).abs() < 1e-9,
            "row reconciles with the totals"
        );
        assert!((row["bracket_pnl"].as_f64().unwrap() - net_bracket).abs() < 1e-9);

        // second request: cached forever — nothing recomputed, nothing double-counted
        let v2 = body_json(
            analytics_handler(State(state.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(
            v2["counterfactuals"], *cf,
            "cached counterfactual must be stable across requests"
        );
        let rows: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM counterfactuals")
            .fetch_one(state.store.pool())
            .await
            .expect("count");
        assert_eq!(rows.0, 1, "one row per position, forever");
    }

    /// A venue that cannot serve candles must not poison the endpoint: the veto stays pending
    /// and everything else still answers.
    #[tokio::test]
    async fn analytics_leaves_uncomputable_counterfactuals_pending() {
        let (_url, state, _tx) =
            spawn_test_server_with(test_risk_cfg(), "http://127.0.0.1:1").await;
        let sized = crate::sizing::Sized {
            leverage: 10.0,
            margin: 20.0,
            notional: 200.0,
            stop_pct: 1.0,
            tp_pct: 2.0,
        };
        let pos = state
            .store
            .open_position(
                "SOL",
                crate::contracts::Side::Long,
                &sized,
                100.0,
                false,
                24.0,
                None,
            )
            .await
            .expect("open");
        state
            .store
            .close_position(pos.id, 100.0, "veto_close", None)
            .await
            .expect("veto close");

        let v = body_json(
            analytics_handler(State(state.clone()))
                .await
                .into_response(),
        )
        .await;
        assert_eq!(v["counterfactuals"]["computed"], serde_json::json!(0));
        assert_eq!(
            v["counterfactuals"]["pending"],
            serde_json::json!(1),
            "failed replay stays pending for the next request"
        );
        assert!(
            v["counterfactuals"]["rows"]
                .as_array()
                .expect("rows array")
                .is_empty(),
            "a pending veto has no row to show yet"
        );
        assert_eq!(
            v["per_market"][0]["trades"],
            serde_json::json!(1),
            "the rest of the payload is unaffected"
        );
    }

    #[tokio::test]
    async fn ws_receives_broadcast() {
        let (url, _state, tx) = spawn_test_server().await;
        // connect ws
        let ws_url = url.replace("http://", "ws://") + "/ws";
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .expect("ws connect");
        // send broadcast
        let mut map = HashMap::new();
        map.insert("SOL".to_string(), 100.0);
        tx.send(WsMsg::Mids(map)).expect("send");
        // wait for message (throttled up to 1s)
        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            use futures_util::StreamExt;
            while let Some(Ok(m)) = ws_stream.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(t) = m
                    && t.contains("\"type\":\"mids\"")
                {
                    return t.to_string();
                }
            }
            panic!("no mids");
        })
        .await
        .expect("timeout ws");
        assert!(msg.contains("SOL"));
    }
}
