# Plan A: `traderd` — Rust Autonomous Paper-Trading Daemon

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax. This plan is self-contained: do NOT assume session context. Read `docs/superpowers/specs/2026-08-08-auto-trader-design.md` first — its §4 API contracts and §6 sizing formulas are normative.

**Goal:** One Rust daemon that ingests Hyperliquid mainnet market data (ws-first), computes features, screens the universe, asks an LLM analyst (OpenAI-compatible), applies risk gates + vol/conviction sizing (5–20x), executes **simulated** fills into a SQLite paper ledger with SL/TP/time-stop triggers, and serves REST+WS for the dashboard plus Telegram notifications. **No real orders anywhere in this plan.**

**Architecture:** single tokio binary; modules with owned responsibilities communicating via channels; SQLite (sqlx, WAL) for persistence; axum for API/WS.

**Tech Stack:** Rust edition 2024. Crates: tokio(full), tokio-tungstenite(rustls), reqwest(rustls-tls, json), axum(ws), serde+serde_json, sqlx(sqlite, runtime-tokio-rustls), rust_decimal, thiserror (lib errors) + anyhow (bin), tracing + tracing-subscriber, clap(derive), feed-rs, sha2, chrono, futures-util.

## Global Constraints

- Workdir: `~/botta/traderd` (new cargo project inside the existing repo; repo root has unrelated Python — do not touch it).
- **Performance is a requirement**: ws-first (never poll where a stream exists); hot paths (mid ticks → features/triggers) allocate nothing per tick (preallocated ring buffers, reused buffers); bounded channels everywhere (no unbounded); WS fan-out to dashboard throttled to ≤1 msg/s for mids. Release profile in Cargo.toml: `[profile.release] lto = "fat"\ncodegen-units = 1\nopt-level = 3\npanic = "abort"`.
- After EVERY task: `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass; then commit (concise imperative message, no co-author lines).
- No `unwrap()` outside tests; `thiserror` in modules, `anyhow` only in `main.rs`. `tracing` for all logging — never `println!` (except the CLI banner).
- **Money math**: prices/sizes as `f64` internally (matches HL JSON), PnL/equity accumulation via `rust_decimal::Decimal` in the ledger.
- **Verified external facts (do NOT re-derive from memory):**
  - REST base `https://api.hyperliquid.xyz/info`, POST JSON. `{"type":"perpDexs"}` → array, index 0 = null (native). `{"type":"metaAndAssetCtxs"}` and `{"type":"metaAndAssetCtxs","dex":"xyz"}` → `[{universe:[{name,szDecimals,...}]}, [ctx,...]]` where ctx has string fields `funding, openInterest, dayNtlVlm, prevDayPx, markPx, oraclePx, midPx`. The `xyz` dex exists on MAINNET ONLY (108 markets, e.g. `xyz:TSLA`, `xyz:GOLD`).
  - WS `wss://api.hyperliquid.xyz/ws`; subscribe frame `{"method":"subscribe","subscription":{"type":"allMids"}}`; **VERIFY at build** the per-dex form (expected `{"type":"allMids","dex":"xyz"}`) against current Hyperliquid gitbook; if unsupported, fall back to REST poll of `{"type":"allMids","dex":"xyz"}` every 2s for xyz only.
  - Analyst endpoint (already in repo `.env`): `ANALYST_BASE_URL=https://api.meta.ai/v1`, `ANALYST_MODEL=muse-spark-1.2-contributor`, `ANALYST_API_KEY=<set>`. **Use the RESPONSES API — verified live 2026-08-08**: `POST {base}/responses`, payload `{"model", "input":[{"role":"user","content":[{"type":"input_text","text":...}]}], "stream":false, "max_output_tokens":3000}`; response: `status:"completed"`, `output` array of items — text lives in items with `type:"message"` → `content[]` with `type:"output_text"` → `.text`; `usage.output_tokens_details.reasoning_tokens` shows reasoning burn. Measured: ~2.5s trivial, **~14s + ~2.2k reasoning tokens for a decision-grade prompt** (size timeouts accordingly: 45s). It is a REASONING model. The paper-trading-simulator framing (see Task 9) **passes** — the reply prefixes a compliance disclaimer paragraph before the JSON, so ALWAYS extract the first balanced `{...}` from the text rather than parsing it whole. A naive "give me a trade signal" prompt (no simulation framing) gets refused — keep the framing verbatim. `/chat/completions` also exists on this host but hides output behind reasoning-token starvation; do not use it.
  - Fallback analyst: `https://lightning.ai/api/v1`, model `openai/gpt-5-mini`, auth header `Bearer ${LIGHTNING_API_KEY}/esoteric-j7sid/inference-optimization-project` (teamspace slash-appended; key extractable from `~/.lightning/credentials.json` field `api_key`).
  - Tavily: `POST https://api.tavily.com/search` `{"query","topic":"news","days":1,"max_results":8}`, `Authorization: Bearer ${TAVILY_API_KEY}` (exported in user shell).
  - Telegram notifications: **Bot API** (`https://api.telegram.org/bot{token}/sendMessage`, JSON `{chat_id, text}`) — `TG_BOT_TOKEN` env; NOT the MTProto userbot.
- Paper fill model (frozen): `fill_px = mark * (1 ± slip)`, slip = 2bp native / 5bp xyz, sign adverse to the trade; fee per fill = notional × (4.5bp taker + 3bp builder). Both open and close pay fee+slip.
- Sizing formulas: spec §6 verbatim — implement exactly, unit-test with pinned numbers.
- Risk gate order: kill-switch → max-concurrent(5) → daily-cap(20) → dup-market → cooldown(30m) → conviction≥0.65 → veto. Kill switch: equity ≤ day-open × 0.88 → halt entries + notify (positions keep managing).
- All `/api/*` + `/ws` response shapes: spec §4 verbatim. Server binds `127.0.0.1:7411`.

## File Structure

```
traderd/
  Cargo.toml
  traderd.toml              # runtime config (spec §7 sections)
  src/
    main.rs                 # CLI, assembly, orchestrator loops, shutdown
    config.rs               # toml + env loading
    contracts.rs            # ALL serde API/domain types (single source of truth)
    sizing.rs               # pure sizing/stop math (spec §6)
    hl_rest.rs              # info endpoint client
    hl_ws.rs                # mids stream client + reconnect
    features.rs             # ring buffers + feature computation
    screener.rs             # scoring + nominee selection
    news.rs                 # store, dedupe, matcher, RSS, tavily
    analyst.rs              # prompt build + LLM call + refusal/fallback
    ledger.rs               # paper positions/trades/equity (sqlx)
    triggers.rs             # SL/TP/time-stop/veto_close evaluation
    risk.rs                 # gate chain + kill switch
    api.rs                  # axum REST + WS broadcast
    notify.rs               # Telegram Bot API + digest
  tests/fixtures/*.json     # captured real HL responses (Task 3 records them)
  migrations/0001_init.sql  # sqlx migrations
```

---

### Task 1: Scaffold + config

**Files:** `Cargo.toml`, `traderd.toml`, `src/main.rs`, `src/config.rs`

**Interfaces produced:** `Config` struct mirroring spec §7 (`ServerCfg{port}`, `UniverseCfg{dexs:Vec<String>, min_vlm_native:f64, min_vlm_dex:f64}`, `ScreenerCfg{interval_s, top_k, min_score}`, `SizingCfg{bankroll, vol_ref, margin_min, margin_max}`, `RiskCfg{max_concurrent, daily_cap, cooldown_min, kill_switch_pct, conviction_min, review_interval_min, time_stop_hours}`, `AnalystCfg{base_url, model, api_key_env, fallback_base_url, fallback_model, fallback_api_key_env, max_completion_tokens}`, `NewsCfg{rss:Vec<String>, tavily_key_env, extra_keywords:HashMap<String,Vec<String>>}`, `NotifyCfg{bot_token_env, chat_id}`), loaded by `Config::load(path) -> Result<Config, ConfigError>`; env secrets resolved lazily by name.

- [ ] Step 1: `cargo init traderd` inside `~/botta` (add `traderd/target/` to repo root `.gitignore`). Fill `Cargo.toml` with the crates + release profile from Global Constraints.
- [ ] Step 2: Write `traderd.toml` with every default from spec §5–§7 (port 7411; dexs `["","xyz"]`; min_vlm 500000/200000; interval 45; top_k 6; min_score 1.8; bankroll 1000; vol_ref 0.4; margin 10–50; max_concurrent 5; daily_cap 20; cooldown 30; kill 12; conviction 0.65; review 15; time_stop 24; analyst envs `ANALYST_*` + fallback per Global Constraints; max_completion_tokens 3000; rss = CoinDesk + CoinTelegraph feed URLs; notify envs).
- [ ] Step 3: Failing test in `config.rs`: `Config::load("traderd.toml")` parses; assert 4 representative values (port 7411, top_k 6, kill_switch_pct 12.0, analyst.model from file).
- [ ] Step 4: Implement `config.rs` (serde + toml crate), `main.rs` = clap skeleton (`--config` arg) + tracing init + `println!` banner only.
- [ ] Step 5: `cargo clippy --all-targets -- -D warnings && cargo test` → green. Commit `feat(traderd): scaffold + config`.

### Task 2: Contracts + sizing math

**Files:** `src/contracts.rs`, `src/sizing.rs`

**Interfaces produced (later tasks import these; do not redefine):**
```rust
// contracts.rs — serde Serialize/Deserialize on all; field names exactly as spec §4 JSON
pub struct Features { pub r5m: f64, pub r1h: f64, pub r24h: f64, pub vol1h: f64, pub funding_z: f64, pub range_pos: f64 }
pub struct MarketRow { pub market: String, pub mid: f64, pub mark: f64, pub oracle: f64, pub funding: f64, pub open_interest: f64, pub day_ntl_vlm: f64, pub prev_day_px: f64, pub features: Option<Features> }
pub struct Snapshot { pub ts: i64, pub markets: Vec<MarketRow> }
pub enum Side { Long, Short }              // serde rename_all lowercase
pub struct Nominee { pub ts: i64, pub market: String, pub side_hint: Side, pub score: f64, pub features: Features }
pub struct Decision { pub action: String, pub side: Option<Side>, pub conviction: f64, pub thesis: String, pub horizon_hours: Option<f64>, pub stop_pct: Option<f64>, pub tp_pct: Option<f64> }
pub struct Position { pub id: i64, pub market: String, pub side: Side, pub entry_px: f64, pub size: f64, pub leverage: f64, pub margin: f64, pub sl_px: f64, pub tp_px: f64, pub opened_ts: i64 }
pub struct NewsItem { pub id: i64, pub ts: i64, pub source: String, pub title: String, pub body: String, pub url: String, pub markets: Vec<String> }
pub enum WsMsg { Mids{..}, Position{..}, Decision{..}, News{..}, Equity{..} }   // tagged "type"

// sizing.rs — pure, no IO
pub struct Sized { pub leverage: f64, pub margin: f64, pub notional: f64, pub stop_pct: f64, pub tp_pct: f64 }
pub fn size_position(cfg: &SizingCfg, vol1h_pct: f64, conviction: f64) -> Sized
```
- [ ] Step 1: Failing tests with **pinned numbers** (from spec §6): `size_position(bankroll=1000, vol_ref=0.4 | vol1h=0.4, conviction=1.0)` → leverage 20, margin 50, stop 0.6 (1.5·0.4), tp 1.2; `(vol1h=0.8, conviction=0.5)` → lev_raw = 5+15·0.5·clamp(0.5,0.4,1.6)=8.75 → leverage 9, margin 1000·0.03=30, stop 1.2, tp 2.4; `(vol1h=0.1, conviction=0.2)` → clamp(4.0,0.4,1.6)=1.6 → lev_raw=9.8 → 10, stop clamped 0.6.
- [ ] Step 2: Implement both modules. Serde round-trip test for `Decision` from the exact JSON the analyst prompt demands.
- [ ] Step 3: clippy+test green → commit `feat(traderd): contracts + sizing math`.

### Task 3: HL REST client (+ recorded fixtures)

**Files:** `src/hl_rest.rs`, `tests/fixtures/meta_native.json`, `tests/fixtures/meta_xyz.json`

**Interfaces produced:** `HlRest::new(base:&str)`, `async fn meta_and_ctxs(&self, dex: Option<&str>) -> Result<Vec<CtxRow>,HlError>` where `CtxRow{market:String, mark:f64, oracle:f64, mid:f64, funding:f64, open_interest:f64, day_ntl_vlm:f64, prev_day_px:f64}` (market name prefixed `dex:` for HIP-3); `async fn all_mids(&self, dex: Option<&str>) -> Result<HashMap<String,f64>,HlError>`.

- [ ] Step 1: Record real fixtures NOW (they are the tests' ground truth): `curl -s https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' -d '{"type":"metaAndAssetCtxs"}' > tests/fixtures/meta_native.json` and the same with `,"dex":"xyz"` → `meta_xyz.json`.
- [ ] Step 2: Failing tests: parse both fixtures; assert native row for `"SOL"` and xyz row for `"xyz:TSLA"` exist with numeric fields > 0; all string-numbers parsed to f64.
- [ ] Step 3: Implement with reqwest; string→f64 via `.parse()` with field-name-carrying errors.
- [ ] Step 4: clippy+test green → commit `feat(traderd): HL REST client + live fixtures`.

### Task 4: HL WS mids stream

**Files:** `src/hl_ws.rs`

**Interfaces produced:** `spawn_mids_stream(url:String, dexs:Vec<String>, tx: tokio::sync::mpsc::Sender<MidsUpdate>) -> JoinHandle<()>` where `MidsUpdate = HashMap<String, f64>` (market→mid, dex-prefixed); reconnects forever with capped expo backoff (1s→30s); sets a shared `AtomicBool ws_connected`.

- [ ] Step 1: VERIFY per-dex `allMids` subscription against current HL docs (gitbook "websocket → subscriptions"). Document findings in a comment. If per-dex ws unsupported → implement xyz via 2s REST poll inside this module (same `tx` interface; callers never know).
- [ ] Step 2: Failing test: spawn a local mock ws server (tokio-tungstenite accept loop) that sends one recorded `allMids` frame then closes; assert (a) update received & parsed, (b) client reconnects (server sees 2nd connection within 5s).
- [ ] Step 3: Implement. Bounded channel (cap 64); on lag, drop-oldest (log at debug — mids are idempotent).
- [ ] Step 4: clippy+test green → commit `feat(traderd): HL websocket mids stream + reconnect`.

### Task 5: SQLite store + migrations

**Files:** `migrations/0001_init.sql`, `src/ledger.rs` (store half)

Tables: `positions(id INTEGER PK, market TEXT, side TEXT, entry_px REAL, size REAL, leverage REAL, margin REAL, sl_px REAL, tp_px REAL, opened_ts INTEGER, status TEXT, closed_ts INTEGER, horizon_hours REAL)`, `trades(id PK, position_id, market, action TEXT, px REAL, size REAL, fee REAL, realized_pnl REAL, ts INTEGER)`, `equity(ts INTEGER PK, equity REAL)`, `decisions(id PK, ts, market, action, side, conviction REAL, thesis TEXT, horizon_hours REAL, vetoed INTEGER, executed INTEGER, reason TEXT)`, `news(id PK, ts, source, title, body, url)`, `news_markets(news_id, market)`, `news_seen(hash TEXT PK, ts INTEGER)`, `meta(k TEXT PK, v TEXT)` (day-open equity, day trade count, day key).

- [ ] Step 1: Failing test: `Store::open("sqlite::memory:")` runs migrations; insert+fetch a position round-trips.
- [ ] Step 2: Implement `Store` (sqlx Pool, WAL pragma on file DBs, prepared queries). 
- [ ] Step 3: clippy+test green → commit `feat(traderd): sqlite store + migrations`.

### Task 6: Feature engine

**Files:** `src/features.rs`

**Interfaces produced:** `FeatureEngine::new()`, `fn on_mid(&mut self, market:&str, ts_ms:i64, mid:f64)`, `fn on_ctx(&mut self, row:&CtxRow, funding_hist_z:f64)` (stores latest ctx), `fn features(&self, market:&str) -> Option<Features>`, `fn funding_z(&mut self, market:&str, funding:f64) -> f64` (z vs that market's trailing 7d funding samples — maintain small ring). Memory bound: raw 1s mids ring for 90min (5400 slots f64) + 1m bars (1440) per market — preallocated on first sight, zero alloc per tick after.

- [ ] Step 1: Failing tests with synthetic series (pin exact expected): feed linear ramp mids 100→101 over 3600s → `r1h == 1.0` (%), `r5m` ≈ 0.0834; vol of constant series = 0; `range_pos` of mid at 24h-high = 1.0. Feature `None` before 5m of data.
- [ ] Step 2: Implement (ring buffers; returns as pct; vol1h = stddev of 1m log-or-simple returns ×100 — use simple, document).
- [ ] Step 3: clippy+test green → commit `feat(traderd): rolling feature engine`.

### Task 7: Screener

**Files:** `src/screener.rs`

**Interfaces produced:** `fn screen(rows:&[MarketRow], cfg:&ScreenerCfg, universe:&UniverseCfg, excluded:&HashSet<String>) -> Vec<Nominee>` — implements spec §5 formula verbatim (cross-sectional z-scores over the vlm-filtered set; score = 2·|z(r1h)| + 1·|z(r5m)| + 1·|funding_z| + 0.5·range_edge; side = sign(r1h), flipped to fade when |funding_z| > 2.5 and it opposes; drop excluded (open/cooldown); sort desc; top_k above min_score).

- [ ] Step 1: Failing tests: synthetic 10-market universe with one obvious momentum outlier → it ranks #1 with side long; funding-extreme case flips side; below-vlm market never nominated; excluded set respected.
- [ ] Step 2: Implement (pure; no IO). clippy+test green → commit `feat(traderd): quant screener`.

### Task 8: News subsystem

**Files:** `src/news.rs`

**Interfaces produced:** `News::new(store, cfg)`; `async fn ingest(&self, source:&str, ts:i64, title:&str, body:&str, url:&str) -> IngestResult{id, deduped}` (sha256(title+url), 48h window); `fn match_markets(&self, text:&str) -> Vec<String>` (case-insensitive keyword map: every universe symbol + built-in alias table `{"BTC":["bitcoin"],"ETH":["ethereum"],"SOL":["solana"],"xyz:TSLA":["tesla","musk"],"xyz:NVDA":["nvidia"],"xyz:GOLD":["gold","xau"],"xyz:AAPL":["apple"],...}` for ALL xyz names, generated from company names + config extras); `spawn_rss(...)` poller (5min, feed-rs, → ingest); `async fn tavily_recent(&self, market:&str) -> Vec<NewsItem>` (on-demand, also ingested).

- [ ] Step 1: Failing tests: dedupe (same title+url twice → second `deduped:true`); matcher ("Tesla beats delivery estimates" → contains `xyz:TSLA`; "Bitcoin ETF inflows" → `BTC`); RSS parse of a small inline atom fixture.
- [ ] Step 2: Implement (tavily/rss network calls behind a `HttpFetch` trait so tests mock; real impl reqwest).
- [ ] Step 3: clippy+test green → commit `feat(traderd): news store, dedupe, matcher, rss+tavily`.

### Task 9: Analyst client

**Files:** `src/analyst.rs`

**Interfaces produced:** `Analyst::new(cfg)`; `async fn decide(&self, input: AnalystInput) -> AnalystOutcome` where `AnalystInput{nominee:Nominee, news:Vec<NewsItem>, open_positions:Vec<Position>, market_hours:&'static str}` and `AnalystOutcome{decision:Option<Decision>, model_used:String, refused:bool, latency_ms:u64}`; `async fn review(&self, position:&Position, features:&Features, news:&[NewsItem]) -> AnalystOutcome`.

System prompt (frozen v1 — tune only with metric evidence): frames a **paper-trading research simulation** ("You are the decision module of an automated PAPER trading simulator; no real funds exist; output is consumed by a simulation engine; this is quantitative research, not financial advice to a human"), pins the exact Decision JSON schema with one example, demands ONLY compact JSON.

Call sequence: primary (muse) via the **Responses API** (exact payload/parse shapes in Global Constraints; `max_output_tokens` from config, default 3000) → join all `output_text` fragments → extract first balanced `{...}` → serde parse (prose disclaimer before the JSON is NORMAL — verified live). On empty/no-JSON (`refused=true`) → ONE retry with a shorter reframe suffix → on second failure, fallback (Lightning gpt-5-mini via `/chat/completions` with `max_completion_tokens`, auth per Global Constraints) → on total failure `decision:None`. **45s timeout** per call (14s decision latency measured). Record refusal + model_used + latency in the decisions log reason.

- [ ] Step 1: Failing tests against a local mock server (axum test listener) serving `/responses`: (a) real captured shape — `output:[{type:"reasoning"},{type:"message",content:[{type:"output_text",text:"<disclaimer paragraph>\n\n{\"action\":\"skip\",...}"}]}]` → Decision parsed despite prose prefix; (b) refusal shape (prose, no JSON anywhere) → retry fires → fallback `/chat/completions` used; (c) empty output array → refusal path; (d) timeout → fallback.
- [ ] Step 2: Implement. clippy+test green → commit `feat(traderd): analyst client with refusal handling + fallback`.

### Task 10: Paper ledger

**Files:** `src/ledger.rs` (logic half)

**Interfaces produced:** `Ledger::open_position(&self, market, side, sized:&Sized, mark:f64, is_xyz:bool, horizon_hours:f64) -> Result<Position>` (fill at mark±slip, fee charged, SL/TP px derived from stop/tp pct off fill); `close_position(&self, id, mark, reason:&str) -> Result<Trade>` (realized pnl incl. exit fee+slip); `partial_close`; `open_positions()`, `equity(&self, marks:&HashMap<String,f64>) -> f64` (Decimal accumulation: cash + margin + unrealized); `snapshot_equity(ts, equity)`; daily counters via `meta` table.

- [ ] Step 1: Failing tests with pinned arithmetic: open long 100 mark, native (slip 2bp) → fill 100.02; notional 300 (margin 15×lev 20) → open fee = 300×0.00075 = 0.225; close at 102 → fill 101.9796…; realized pnl = size×(exit−entry) − fees, assert to 1e-9. Short case symmetric. Equity identity: bankroll − fees + unrealized.
- [ ] Step 2: Implement. clippy+test green → commit `feat(traderd): paper ledger + fill/fee model`.

### Task 11: Triggers + risk gates

**Files:** `src/triggers.rs`, `src/risk.rs`

**Interfaces produced:** `fn check_triggers(pos:&Position, mid:f64, now_ms:i64) -> Option<TriggerAction>` (`Sl|Tp|TimeStop`); `Risk::gate_entry(&self, market, conviction, state) -> Result<(), GateRefusal>` implementing the frozen order from Global Constraints; `Risk::on_equity(&self, equity, day_open) -> KillState`.

- [ ] Step 1: Failing tests: long SL hit at mid ≤ sl_px; TP at ≥ tp_px; short inverted; time-stop after horizon; each gate refuses at its boundary and passes just inside; kill switch at exactly −12% halts, at −11.9% doesn't.
- [ ] Step 2: Implement. clippy+test green → commit `feat(traderd): triggers + risk gate chain`.

### Task 12: axum API + WS

**Files:** `src/api.rs`

**Interfaces produced:** `build_router(state: AppState) -> axum::Router` serving EVERY route in spec §4 (shapes verbatim from `contracts.rs`); `AppState` holds `Arc<RwLock<SnapshotCache>>`, `Store`, `tokio::sync::broadcast::Sender<WsMsg>` (cap 256); `/ws` upgrades and forwards broadcast (mids throttled ≤1/s by a dedicated forwarder task); `POST /ingest/news` → `News::ingest` → broadcast.

- [ ] Step 1: Failing integration tests: spawn router on ephemeral port; GET each endpoint → 200 + shape asserts (serde round-trip into contracts types); POST /ingest/news twice → second `deduped:true`; ws client receives a broadcast mids frame.
- [ ] Step 2: Implement. clippy+test green → commit `feat(traderd): REST+WS API`.

### Task 13: Notifier

**Files:** `src/notify.rs`

**Interfaces produced:** `Notify::send(&self, text:&str)` (Bot API, fire-and-forget with 1 retry, no-op if token/chat unset); `fn digest(trades:&[Trade], equity_open:f64, equity_now:f64) -> String` (daily 21:00 IST summary: PnL, win-rate, n trades, best/worst market).

- [ ] Step 1: Failing tests: digest formatting from a fixed trade set (pin exact string); send() hits mock server with correct JSON body.
- [ ] Step 2: Implement. clippy+test green → commit `feat(traderd): telegram notifier + daily digest`.

### Task 14: Orchestrator (`main.rs`) + live smoke

**Interfaces consumed:** everything above.

Assembly (all in `main.rs`, one `tokio::select!`-driven supervisor):
1. Load config + env; open store; hydrate day-open equity (meta table; roll at 00:00 UTC).
2. Universe bootstrap + hourly refresh (hl_rest meta both dexs → vlm filter).
3. Spawn: mids ws (Task 4) → fan into (a) FeatureEngine.on_mid, (b) trigger scan over open positions (execute via Ledger + broadcast + notify), (c) throttled ws broadcast. 30s ctx poll → features.on_ctx + SnapshotCache refresh. 45s screener tick → nominees (excluded = open ∪ cooldown) → per nominee (serialized, max 2 concurrent analyst calls): gather news (matched last 6h + tavily), market_hours flag (xyz: 09:30–16:00 ET Mon–Fri = "open" else "closed") → analyst.decide → risk.gate → sizing → ledger.open → decisions log + broadcast + notify. 15min review tick per position → analyst.review → hold/move-stop/veto_close. 60s equity snapshot + kill-switch eval. 21:00 IST digest timer. Graceful shutdown on SIGINT (flush store).
4. `ANALYST_STUB=1` env: replace analyst with deterministic stub (conviction 0.9 long for top nominee — for pipeline smokes without LLM spend).

- [ ] Step 1: Wire it. `cargo clippy -- -D warnings && cargo test` green.
- [ ] Step 2: **Live smoke (no LLM):** `ANALYST_STUB=1 cargo run --release -- --config traderd.toml` for 10 minutes: assert via curl — `/api/health` ws_connected true, markets_tracked > 100; `/api/snapshot` has features non-null after ~6min; `/api/nominees` non-empty; a stub position opens and appears in `/api/positions`; `/ws` streams mids. Kill and restart → positions persist.
- [ ] Step 3: **Live smoke (real analyst):** unset stub, run 30 min supervised; verify decisions log shows muse calls (or clean fallbacks) with latency + refusal metrics.
- [ ] Step 4: Commit `feat(traderd): orchestrator + live smoke verified`.

## Self-review checklist (executor runs after Task 14)

Spec §4 route-by-route conformance; §6 formulas match unit tests; no `unwrap()` in src/; no unbounded channels; release profile present; refusal-rate visible in `/api/decisions` reasons; zero real-order code paths exist.
