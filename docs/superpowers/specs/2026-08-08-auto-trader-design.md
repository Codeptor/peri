# Autonomous Paper Trader ("traderd") — Design Spec

**Date:** 2026-08-08 · **Status:** Approved (user-locked decisions) → three implementation plans
**Executors:** plans are written for independent agents/models with zero session context.

## 1. Goal

A fully automated **paper** trading system that longs/shorts Hyperliquid **native perps
and HIP-3 markets** (e.g. `xyz:TSLA`, `xyz:GOLD` — 108 markets on the mainnet `xyz` dex)
from live market data + news. Quant screener nominates; an LLM analyst decides;
vol+conviction sizing at **5–20x**; simulated fills on **mainnet market data** (testnet has
no HIP-3 dexs — verified 2026-08-08). Fully custom dashboard. No real orders in v1.

## 2. Locked decisions (do not relitigate in plans)

| Decision | Value |
|---|---|
| Brain | Quant screener nominates → LLM analyst decides (news-aware) |
| Language | **Max Rust**: one `traderd` daemon owns feed/features/screener/news store/analyst calls/risk/paper ledger/triggers/API/notifications. Exception: Telegram news ingestion = minimal Python Telethon sidecar (MTProto in Rust not worth it) pushing into traderd's HTTP ingest. Existing Python copy-trader (`src/botta/`) untouched. |
| Sizing | leverage 5–20x from volatility + conviction (exact formulas §7) |
| Horizon | intraday swing (hours); time-stop default 24h |
| Analyst LLM | config-driven **OpenAI-compatible** endpoint (user supplies muse-spark base URL/model/key; fallback `openai/gpt-5-mini` via Lightning gateway) |
| News | TG channels (sidecar) + Tavily on-demand + RSS + generic `POST /ingest/news` for future scrapers (user will supply more sources) |
| Dashboard | Next.js + shadcn **blocks from multiple registries**, local, live via WS |
| Mode | paper only; live execution is a later plan (mainnet = fresh wallet — the 0xab8832 key is transcript-burnt, testnet-only) |
| Performance | user requirement "full speed, efficient, optimized": ws-first data (no polling where a stream exists), zero-alloc hot paths (preallocated ring buffers), bounded channels, release profile `lto=fat`/`codegen-units=1`/`opt-level=3` — enforced in Plan A globals |
| Analyst API (verified live) | `POST https://api.meta.ai/v1/responses` (Responses API; `input_text` parts; text in `output[].content[].output_text`); muse-spark is a reasoning model: ~2.5s trivial, ~14s + ~2.2k reasoning tokens per decision; paper-sim framing passes (disclaimer prose precedes the JSON — extract first balanced `{}`); naive signal prompts get refused; fallback gpt-5-mini via Lightning |

## 3. System layout (monorepo `~/botta`)

```
botta/
  src/botta/            # existing Python copy-trader (unchanged)
  traderd/              # NEW: Rust daemon (cargo project)         → Plan A
  tg_news_pipe.py       # NEW: Telethon → traderd news pipe        → Plan B
  dash/                 # NEW: Next.js dashboard                   → Plan C
  docs/superpowers/     # specs + plans
```

Data flow:

```
HL mainnet ws (allMids, native+xyz) ─┐
HL mainnet REST (ctxs: funding/OI/vlm) ─┤→ [feature engine] → [screener] → nominees
TG sidecar / RSS / Tavily / POST ────→ [news store] ──────────┐
                                                              ▼
                            [analyst: OpenAI-compat LLM] → Decision{side,conviction,veto}
                                                              ▼
                     [risk gates] → [sizing 5–20x] → [paper ledger (SQLite)]
                              ▲                            │
              [trigger engine: SL/TP/time-stop on live mids]│
                                                              ▼
                    [axum API+WS] → Next.js dash · [TG Bot notifier] → user DMs
```

## 4. API contracts (source of truth; all three plans conform)

`traderd` serves on `127.0.0.1:7411`. All JSON. Timestamps = unix millis (i64).
Market keys are strings: native `"SOL"`, HIP-3 `"xyz:TSLA"`.

- `GET /api/health` → `{ok:true, uptime_s, ws_connected, markets_tracked}`
- `GET /api/snapshot` → `{ts, markets:[{market, mid, mark, oracle, funding, open_interest, day_ntl_vlm, prev_day_px, features:{r5m, r1h, r24h, vol1h, funding_z, range_pos} | null}]}`
  (features null until enough history; r* = pct returns; vol1h = stddev of 1m returns over 60m, pct; range_pos = (mid−24h low)/(24h high−24h low) in [0,1])
- `GET /api/nominees` → `[{ts, market, side_hint:"long"|"short", score, features:{...}}]` (last screener pass)
- `GET /api/positions` → `[{id, market, side, entry_px, size, leverage, margin, sl_px, tp_px, opened_ts, mark_px, unrealized_pnl, roe}]`
- `GET /api/trades?limit=100` → `[{id, position_id, market, action:"open"|"partial_close"|"sl"|"tp"|"time_stop"|"close"|"veto_close", px, size, fee, realized_pnl, ts}]`
- `GET /api/equity?points=500` → `[{ts, equity}]`
- `GET /api/decisions?limit=50` → `[{ts, market, action:"open"|"skip", side, conviction, thesis, horizon_hours, vetoed:bool, executed:bool, reason}]`
- `GET /api/news?limit=100` → `[{id, ts, source, title, body, url, markets:[..]}]`
- `POST /ingest/news` body `{source:string, ts?:i64, title:string, body?:string, url?:string}` → `{ok:true, id, deduped:bool}` (dedupe = sha256(title+url) seen in 48h)
- `GET /ws` (websocket) → server pushes `{type:"mids", data:{market:mid,...}}` (throttled ≤1/s), and events `{type:"position"|"decision"|"news"|"equity", data:<same shapes as REST>}`

## 5. traderd internals (Rust)

- **Feed**: ws `wss://api.hyperliquid.xyz/ws`, subscribe `allMids` (and per-dex variant for `xyz` — VERIFY exact subscription shape at build; fallback: 2s REST poll of `allMids?dex`). REST `metaAndAssetCtxs` (native + `dex:"xyz"`) every 30s for funding/OI/volume/prevDayPx. Auto-reconnect with expo backoff; `ws_connected` health flag.
- **Universe**: native perps + `xyz` dex only. Filter: `day_ntl_vlm ≥ $500k` (native) / `$200k` (xyz). Refresh universe hourly.
- **Feature engine**: per market ring buffers of (ts, mid) at 1s resolution (24h retention ⇒ 86400 slots worst case — store 1m aggregates after first hour to bound memory: keep raw 1s for 90min + 1m bars for 24h).
- **Screener** (every 45s): score = `2.0*|z(r1h)| + 1.0*|z(r5m)| + 1.0*|funding_z| + 0.5*range_edge` where `range_edge = max(0, |range_pos−0.5|·2 − 0.6)` (breakout proximity), z() = cross-sectional z-score over the filtered universe. side_hint = direction of r1h (momentum) unless `funding_z` extreme (>2.5) opposes → side_hint flips to fade. Top-K (default 6) above min score (default 1.8), minus markets in cooldown/open.
- **News store**: SQLite. Sources: RSS poller (default feeds: CoinDesk, CoinTelegraph — config list), Tavily search on nominee at analysis time (`topic=news, days=1`), `POST /ingest/news`. Market matching: keyword map (symbol, name, underlying for xyz e.g. "TSLA"→"Tesla") — table in code, extendable via config.
- **Analyst**: on nominee: build prompt = features + last-6h matched news (≤10 items) + open portfolio + market-closed flag for xyz (US RTH check) → `POST {base_url}/chat/completions` `{model, messages, response_format:{type:"json_object"}, max_completion_tokens:400}` → strict-parse Decision JSON; 1 retry on invalid; on 2nd failure → skip + log. Config: `[analyst] base_url, model, api_key_env, fallback_base_url, fallback_model, fallback_api_key_env`. Timeout 30s → fallback model → skip.
- **Risk gates** (evaluate in order, all pinned): global kill switch (paper equity ≤ day-open −12% → halt entries, notify, flag in /api/health); max 5 concurrent; 20 entries/day; no duplicate market; per-market 30min cooldown after close; conviction < 0.65 → skip; veto → skip (and if analyst vetoes an OPEN position on re-review → close it, action `veto_close`).
- **Position review**: every 15min per open position (and on matched breaking news): re-run analyst with position context; it may hold / move stop / veto_close.
- **Trigger engine**: on every mids tick: SL/TP cross → simulate fill; time-stop at `horizon_hours` (default 24h) → close.
- **Paper fills**: `fill_px = mark ± slippage` (2bp native, 5bp xyz, direction-adverse); fee = 4.5bp taker + 3bp builder on notional, both directions. Equity = cash + Σ unrealized. Snapshot equity to SQLite every 60s + on every trade.
- **Notifier**: Telegram **Bot API** (BotFather token in config; user's chat id) — every open/close with thesis, kill-switch trips, daily 21:00 IST digest (PnL, win rate, best/worst). Plain reqwest, no MTProto.
- **Persistence** (SQLite via sqlx, WAL): `positions, trades, equity, decisions, news, news_seen(hash,ts)`.

## 6. Sizing (exact, v1-frozen)

```
vol1h_pct   = stddev(1m returns over 60m) * 100          # e.g. 0.35 (%)
stop_pct    = clamp(1.5 * vol1h_pct, 0.6, 2.5)           # SL distance in %
tp_pct      = 2.0 * stop_pct                             # 2R target
vol_ref     = 0.4                                        # config: reference 1h vol (%)
lev_raw     = 5.0 + 15.0 * conviction * clamp(vol_ref / vol1h_pct, 0.4, 1.6)
leverage    = clamp(round(lev_raw), 5, 20)
margin_usd  = clamp(bankroll * (0.01 + 0.04*conviction), 10, 50)   # $1k bankroll → $10–50
notional    = margin_usd * leverage
```
Analyst may override stop/tp via `stop_pct`/`tp_pct` in its Decision (clamped to [0.4%, 4%]).

## 7. Config (`traderd/traderd.toml` + env for secrets)

Sections: `[server]` port · `[universe]` dexs=["","xyz"], min_vlm native/xyz · `[screener]` interval_s=45, top_k=6, min_score=1.8 · `[sizing]` bankroll=1000, vol_ref=0.4, caps · `[risk]` all §5 gate values · `[analyst]` endpoints/models/key-envs, conviction_min=0.65, review_interval_min=15 · `[news]` rss=[...], tavily_key_env, keyword extras · `[notify]` tg_bot_token_env, chat_id · Secrets in env/.env: `ANALYST_API_KEY`, `LIGHTNING_API_KEY`, `TAVILY_API_KEY`, `TG_BOT_TOKEN`.

## 8. Crates (user Rust prefs apply)

tokio(full), tokio-tungstenite+rustls, reqwest(rustls, json), axum(+ws), serde/serde_json,
sqlx(sqlite,runtime-tokio), rust_decimal, thiserror(+anyhow in bin), tracing(+subscriber),
clap(derive), feed-rs (RSS), sha2, chrono. Edition 2024. `cargo clippy -- -D warnings` + `cargo test` green per task.

## 9. Out of scope v1

Live order execution (later plan: hyperliquid-rust-sdk or the proven Python adapter),
backtesting, candles in API, auth on dashboard (localhost only), Solana venues,
mobile. The Python copy-trader keeps running independently.

## 10. Verify-at-build (executors: check against live docs/API, do not trust memory)

1. HL ws subscription shapes for `allMids` (+ per-dex form) — hyperliquid gitbook.
2. `metaAndAssetCtxs` response field names (assetCtxs: funding/openInterest/dayNtlVlm/prevDayPx/markPx/oraclePx/midPx) — capture ONE real curl per dex as test fixture in-repo.
3. OpenAI-compat: gpt-5-* family rejects `max_tokens` (use `max_completion_tokens`); Lightning auth = `Bearer <key>/esoteric-j7sid/inference-optimization-project`, base `https://lightning.ai/api/v1`.
4. Tavily: `POST https://api.tavily.com/search` `{query, topic:"news", days:1, max_results:8}`, `Authorization: Bearer`.
5. Telegram Bot API sendMessage shape.
6. xyz US-RTH calendar: v1 = fixed 09:30–16:00 ET Mon–Fri check, no holiday table (flag `market_hours:"closed"` to analyst).
```

---

## Amendment log (user-locked; normative over this doc — full living version in `docs/superpowers/STATUS.md`)

| Date | Change | Commit |
|---|---|---|
| 2026-08-08 | Analyst = **muse-only** — fallback model/endpoints removed (§2 row, §5 analyst fallback, §7 `[analyst] fallback_*` deleted); budget 16000 `max_output_tokens`; 60s/call; 2 retries then skip | `56c200c` |
| 2026-08-08 | WS/data contracts CONFIRMED live: per-dex `allMids {"type":"allMids","dex":"xyz"}` ws-supported; REST ctx string fields; `midPx` nullable (delisted) | `4a3f13c`,`0eebc0b` |
| 2026-08-08 | Fill model v2: l2Book impact VWAP with never-better-than-flat cap; flat 2bp/5bp stays as fallback (spec §5 fill text extended) | `ed0581a` |
| 2026-08-08/09 | Real-time data upgrades: `allDexsAssetCtxs` ws (ctxs ~1s), `fundingHistory` 7d boot seed, 15m-candle ATR in analyst prompt | `2562e004`,`8addb79`,`522e67f` |
| 2026-08-09 | Universe floors raised `min_vlm_native 2M / min_vlm_dex 500k`; `conviction_min 0.70` (day-1 live-data tuning, §5/§7 defaults superseded) | `b29d645` |
| 2026-08-08/09 | News surface: 10 tg channels + 9 RSS + Tavily (dedicated key in `.env`); TG notify wired to `yourbot_bot` chat 123456789 | `cfa376b`,`c0793aa`,`bf7082d` |
| 2026-08-09 | Sizing §6 amended: `stop_pct = clamp(1.5·vol1h, 1.0, 3.0)` (floor 0.6→1.0, ceiling 2.5→3.0; TP stays 2R); analyst stop override clamp → [1.0, 4.0] (day-2 tape: 8 stops −$110 in noise bands) | `53a5a64` |
| 2026-08-09 | Dash serves API via same-origin proxy `/traderd/*` (browser CORS blocks cross-origin localhost:3474→7411); traderd adds NO CORS headers | `e82baeb` |
