# kestreld — Kestrel's autonomous PAPER trading daemon (Rust)

**Invariant: PAPER ONLY.** No live order execution, no wallet signing, no real-funds paths exist in this crate. Every fill is simulated against live mainnet data (`fill_px = book-impact or mark ± slip`, fee = 7.5bp both directions). Any PR/commit introducing `/exchange`, signing, or transfer calls fails review.

## Components (one file, one job, bounded channels)

| File | Job |
|---|---|
| `config.rs` | toml+env loading. **dotenvy precedence: process env WINS over .env file** (vars already set are never overwritten) |
| `contracts.rs` | ALL serde API/domain types — spec §4 field names verbatim (`Side` is Copy, lowercase serde) |
| `hl_rest.rs` | info client: `meta_and_ctxs`, `all_mids`, `universe_names`, `funding_history`, `candle_snapshot`, `merge_ctx_rows` |
| `hl_ws.rs` | 3 reconnecting ws streams (**jittered 1s→30s backoff, forever**, resub-on-connect, INFO per cycle with attempt count): mids (allMids native+xyz), books (l2Book per open position/nominee), ctxs (allDexsAssetCtxs). Also owns `WsFreshness` — per-stream last-message clock behind `health.ws_connected` |
| `features.rs` | ring-buffer features (r5m/r1h/r24h/vol1h/range_pos), funding_z (trailing ring, **change-guarded**: dup values never append), ATR (Wilder) |
| `screener.rs` | cross-sectional z-score momentum+funding-fade, top-K above min_score |
| `news.rs` | ingest/dedupe (sha256 title+url, 48h), market matcher, RSS poller (5 min), Tavily per-nominee |
| `analyst.rs` | **muse-only** (user-locked): Responses API, Tavily+Exa prompt retrieval, prose-tolerant first-balanced-{} extraction, 2 retries then skip, 60s timeout, 16k output budget |
| `ledger.rs` | paper positions/trades/equity (SQLite WAL, Decimal equity). Fills: **l2Book impact VWAP with never-better-than-flat cap**, flat 2bp/5bp fallback (None/stale>3s/empty/exhausted book) |
| `triggers.rs` | SL/TP/time-stop on live mids |
| `risk.rs` | frozen gate order: kill → max-concurrent 5 → daily-cap 20 → dup-market → cooldown 30m → conviction ≥ cfg → veto |
| `api.rs` | axum REST+WS, `127.0.0.1:7411` (spec §4 shapes; server sends NO CORS headers by design). Handlers that read shared state are bounded by `STATE_TIMEOUT` (3s) → `503 {"error":"state unavailable"}`; `/api/health` always answers |
| `notify.rs` | Telegram transport (**every message `parse_mode=HTML`**) + the pure `tpl::*` message templates. `esc()` escapes `& < >` on anything model- or venue-derived — an unbalanced tag 400s the whole send. No-op if token/chat unset |
| `digest.rs` | daily digest at the UTC day-roll: composes the ended day from the ledger (equity open→close, net/fees, exit mix, win rate, top/bottom market, gate refusals by kind, veto counterfactuals), sends one rich TG message and appends a `## YYYY-MM-DD` section to `../docs/ledger/YYYY-MM.md`. The markdown file IS the idempotency ledger, so a roll and the boot back-fill can never double-send |
| `main.rs` | orchestrator: universe hourly refresh, 45s screener tick, 15min position review (hold/tighten-stop/veto_close), 60s equity+kill eval, day-roll at 00:00 UTC (kill latch resets, day-open equity rolls, daily digest fires for the ended day). Enforces the **shared-state lock invariant** (see below) |

## Shared-state lock invariant (load-bearing)

Two locks guard the hot path: `snapshot: Arc<RwLock<Snapshot>>` and `engine: Arc<Mutex<FeatureEngine>>`.

1. **Never hold both at once.**
2. **Never hold either across an `.await` that does I/O** (store, HTTP, ws).

Required shape for every task that rebuilds snapshot state — short `snapshot.read()` → clone out → engine work under the engine guard alone (pure, no awaits) → assignment-only `snapshot.write()`.

The split costs atomicity, so the one path whose semantics are *"preserve the snapshot's mids"* (the 30s ctx poll) re-applies live mids **inside** its write guard via `hl_rest::overlay_live_mids` — `mid` stays authoritative from the mids feed, while `mark`/`oracle`/ctx fields belong to the REST/ctx source (same ownership rule as `merge_ctx_rows`). The universe rebuilds (bootstrap, hourly) deliberately do **not** overlay: replacing mids wholesale from REST is their hourly heal for a market whose ws mid stopped ticking.

**Why (2026-08-09, two production wedges):** the mids fan took `snapshot.write → engine.lock` while the ctxs fan and 30s ctx poll took `engine.lock → snapshot.write`. That ABBA inversion parked both tasks permanently; since `tokio::sync::RwLock` is task-fair, a queued writer starves every later reader, so `/api/snapshot` and `/api/positions` hung forever while health/trades/equity/decisions/news (which never touch the snapshot) kept serving. Equity rows stop dead at 12:30:00Z (run 1) and 13:24:33Z (run 2) — those are the onsets. The ws `stream ended` warns and the REST `Shape` errors in the log were coincident, not causal.

Backstops: `api::STATE_TIMEOUT` (3s → 503) turns any future regression into a visible error instead of a hang, and `HlRest::REQUEST_TIMEOUT` (10s) bounds every HL info request.

## Health semantics

`GET /api/health` never blocks. `ws_connected` is **derived from last-message age** (`hl_ws::WsFreshness`): true iff the mids *or* ctxs stream produced a frame within `WS_FRESH_WINDOW_MS` (30s). It is not a connect flag — the old set-once `WS_CONNECTED` bool read `true` straight through both outages because the supervisor re-set it on every reconnect attempt. `markets_tracked` still counts only the vlm-filtered universe; if the snapshot is unreachable, health reports what it knows rather than waiting.

## Config (`kestreld.toml`) + env

Sections: `[server] port` · `[universe] dexs, min_vlm_native, min_vlm_dex` · `[screener] interval_s, top_k, min_score` · `[sizing] bankroll, vol_ref, margin_min/max` · `[risk] max_concurrent, daily_cap, cooldown_min, kill_switch_pct, conviction_min, review_interval_min, time_stop_hours` · `[analyst] base_url, model, api_key_env, max_completion_tokens, web_retrieval, tavily_key_env, exa_key_env` · `[news] rss[], tavily_key_env, extra_keywords, tg_channels[]` · `[notify] bot_token_env, chat_id`.

Secrets live ONLY in repo `.env` (0600, gitignored): `ANALYST_API_KEY`, `TAVILY_API_KEY`, `EXA_API_KEY`, `TG_BOT_TOKEN` (+ copy-trader's `TG_API_ID/HASH`, `HL_AGENT_KEY` — never used here).

## Build / test / run

```bash
cargo test && cargo clippy --all-targets -- -D warnings   # gates, must stay green (321 tests @ 2026-08-09 — grows per batch)
cargo build --release                                     # ~2-3 min (lto=fat, cgu=1, panic=abort)
ANALYST_STUB=1 ./target/release/kestreld --config kestreld.toml  # deterministic smoke, no LLM
./target/release/kestreld --config kestreld.toml                 # live paper run
```

**Restart discipline (leader only):** `kill -INT $(pgrep -x kestreld)`. Never `pkill -f '<pattern>'` from a shell whose command line contains the pattern (self-match trap). Positions/decisions/equity persist in `kestreld/kestreld.db` (SQLite WAL) across restarts.

## Backtest harness (`kestreld backtest`)

Replays cached history through the **real** strategy modules — screener, sizing, the whole gate chain, the counterfactual walker's fill rules — so a knob can be argued about with evidence instead of tape-waiting. Spec: `docs/superpowers/specs/2026-08-09-backtest-harness.md`. It is a **subcommand**: it runs and exits before any daemon setup, never opens `kestreld.db`, and has no order surface at all (PAPER invariant, one step further).

```bash
# 1. cache the window (cache-first, resumable, throttled 100ms + 1s/20 like the funding seed)
kestreld backtest fetch --from 2026-08-06 --to 2026-08-08

# 2. one replay
kestreld backtest run --from 2026-08-07 --to 2026-08-08 --conviction 0.75
kestreld backtest run --from 2026-08-07 --to 2026-08-08 --set risk.daily_cap=10   # any knob the replay reads

# 3. a grid — repeatable --sweep, cartesian, one report per combo + a ranked summary
kestreld backtest sweep --from 2026-08-06 --to 2026-08-08 \
  --sweep stop_floor=0.6,1.0,1.4 --sweep cooldown_after_sl=30,120

# 4. a read-only distribution report over the cache — no replay, no gates, no pnl
kestreld backtest stats --from 2026-08-06 --to 2026-08-08
```

**Cache** — `kestreld/backtest_cache.db`, a separate sqlite file (`candles(market,t,o,h,l,c,v)`, `funding(market,t,rate)`, `coverage`). Cache-first: a range already covered is never refetched, and coverage records *what was asked for*, not what came back (a minute with no trades has no candle). **Hyperliquid serves only ~3.6 days of 1m history** — measured 2026-08-09 — so a window older than that answers empty forever and no backfill can recover it. Delete the file to start over; it holds nothing but public market data.

**Sweep keys** (short names; any dotted key `--set` accepts also works as an axis):

| key | override it applies | what moves |
|---|---|---|
| `stop_floor` | `sizing.stop_floor_pct` | the `stop_pct` clamp's lower bound in `sizing::size_position` — and `tp_pct` with it, since tp stays `tp_mult`R (default 2R) |
| `conviction` | *run parameter* + `risk.conviction_min` | leverage `5+15·c·vol_ratio` and margin `bankroll·(0.01+0.04·c)` — see below |
| `regime_vol_max` (`regime`) | `risk.regime_vol_max` | `risk::gate_regime`'s BTC-vol ceiling; the value `off` = an infinite ceiling, i.e. the gate never refuses |
| `cooldown_after_sl` | `risk.cooldown_after_sl_min` | `risk::gate_churn`'s post-stop window **and** the screener pre-filter's exclusion clock |
| `daily_cap` | `risk.daily_cap` | `risk::gate_entry`'s daily entry counter — the capacity rail the first sweeps found dominates every outcome (60 entries = `daily_cap`×days, in every run) |
| `max_concurrent` | `risk.max_concurrent` | `risk::gate_entry`'s open-position ceiling — the other capacity rail |
| `min_score` | `screener.min_score` | `screener::screen`'s nomination threshold — signal selectivity |
| `margin` | `sizing.margin_min` **and** `sizing.margin_max`, pinned to the SAME value | `sizing::size_position`'s margin clamp — see below |
| `tp_mult` | `sizing.tp_mult` | `size_position`'s `tp_pct = tp_mult · stop_pct` — the take-profit multiple of the stop distance (default `2.0`, i.e. 2R; was a literal until this batch) |

**What a conviction sweep means in mechanical mode.** There is no analyst, so every entry of a run carries the *same* conviction — which makes `risk.conviction_min` not a threshold but a switch that passes all entries or refuses all of them (`LowConviction`). Against a fixed 0.75 floor, `conviction=0.70` scores zero trades and the rest differ only in sizing. So a conviction axis **moves the floor with it**: each run is "the machine, as if the analyst always answered *X*", and what the axis measures is the notional at risk per trade. Name `risk.conviction_min` yourself (as a `--set` or as its own axis) and the alignment is off — that gets the gate-binding reading back.

**What a margin sweep means.** `sizing.margin_min`/`sizing.margin_max` are normally a clamp around the conviction-driven raw margin (`bankroll·(0.01+0.04·conviction)`); in mechanical replay every entry already carries one fixed conviction, so that raw value is already constant, and sweeping the clamp bounds independently mostly tests whether the clamp bites rather than acting as a real size lever. So `margin=X` pins **both** ends of the clamp to `X` (`clamp(v, X, X) == X` for any `v`), making every position's margin exactly `X` dollars regardless of conviction — the position-size lever the deliverable asked for. Pair it with a `daily_cap`/`max_concurrent` sweep moved the *other* way to compare **fewer, larger** positions against **more, smaller** ones at a similar aggregate notional. As with conviction, naming `sizing.margin_min`/`sizing.margin_max` yourself (via `--set` or as their own axis) leaves that end of the clamp alone instead of pinning it.

## Distribution report (`kestreld backtest stats`)

```bash
kestreld backtest stats --from 2026-08-06 --to 2026-08-08 [--markets BTC,ETH,...]
```

Read-only: walks the same cached tape and feature/funding recomputation `run`/`sweep` do, but takes no `--set`, opens no positions and applies no gates — it reports what the cached window's DATA looks like, not a strategy outcome. Written to `docs/backtests/<date>-stats-<window>/stats.{json,md}`, and printed to stdout. Contents:

- **BTC vol1h% percentiles** (p50/p75/p90/p95/p99/max) — the regime gate's `risk.regime_vol_max` (1.5) is compared against this directly; the first sweeps measured BTC vol1h peaking at **0.079%** over a 3-day window, ~19× under the threshold (`docs/backtests/2026-08-09-first-sweeps`), which is why the gate has never fired live.
- the same percentiles pooled across the **whole cached universe**, and **per-market for the top ~15 by the volume proxy** (mean `day_ntl_vlm` across the window).
- **funding_z distribution** — share of market-minutes at `|z|>1`, `>2`, `>2.5` (the screener's own side-flip threshold), computed through the real ring (`features::funding_z_at`), so `screener.min_score`'s funding term is measured against real data rather than assumed.
- **screener score distribution** — `screener::screen` run with an open floor/top_k so every vlm-eligible, feature-warmed market-minute is pooled, then bucketed against the config's real `min_score` (the share of market-minutes that clear it).
- **turnover/fee reference** — `sizing::size_position` at the window's own measured median vol1h, at default and full conviction: notional, leverage, margin, and the round-trip fee in $ and bp (7.5bp taker × 2 sides = **15bp of notional**, independent of position size).

Every report states the window it actually covered vs. what was requested — HL's ~3.6-day 1m retention means an older request predictably comes back thin, and the report says so rather than silently reporting on less data than asked for.

**Deviations from live** (verbatim from spec Decisions 2/4/5/6; every report reprints them in its own header):

- mechanical replay — no analyst judgment, no news: conviction is fixed for every entry and the screener's `side_hint` is taken as the side
- no review loop: no veto closes, no stop moves, no analyst time-stop — brackets and the horizon are the only exits
- exits: SL is checked before TP inside a candle (a candle touching both is scored as the stop) and fills AT the trigger; the horizon fills at the candle close
- entries fill at the next 1m open plus flat slip (2bp native / 5bp dex); 7.5bp taker fee per side
- `day_ntl_vlm` is proxied by rolling 24h candle quote volume — the venue does not serve historical `dayNtlVlm`, and this proxy decides WHICH markets clear the universe filter
- features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints cross the spread, so `vol1h` runs slightly hotter than live
- `open_interest` is 0.0 throughout — historical OI is not served (nothing in the screener, sizing or the gates reads it)
- the screener ticks every 1m (live: 45s) — the closest candle-aligned cadence

**Output** — `docs/backtests/<run-date>-<label>/`: `report.{json,md}` for a single run; for a sweep, `summary.{json,md}` (ranked by net, ties to the shallower drawdown, then label) plus one `<combo-slug>/report.{json,md}` per grid point. Every sweep summary also carries a **fee anatomy** table — `gross` (= net + fees), the fee bill, the `turnover` it was charged on (fee is flat, so turnover = fees ÷ 0.00075) and `fees ÷ |net|` for losing runs — because on this strategy the interesting question is not which run lost least but how much of the loss was the turnover. Runs are deterministic — no clock, no RNG, markets visited in name order — so two replays of one cache produce byte-identical reports. First real sweeps: `docs/backtests/2026-08-09-first-sweeps/`; regime recalibration + fee/capacity sweeps: `docs/backtests/2026-08-10-strategy-sweeps/`.

## Ops map

- API: `http://127.0.0.1:7411/api/*`, ws `/ws`. Dashboard (separate Next.js app in `dashboard/`) proxies via same-origin `/traderd/*` (route prefix unchanged; do NOT add CORS to this binary). A `503 {"error":"state unavailable"}` means shared state was unreachable for 3s — the dashboard's `!res.ok` throw already routes it to the existing error path.
- Wedge triage: if `/api/snapshot` or `/api/positions` 503s, the reader bound tripped — look for a task holding the snapshot lock (invariant above), not for a dead websocket. If `health.ws_connected` is `false`, the streams are genuinely stale (>30s since any frame); reconnect attempts log at INFO with `stream`/`attempt`/`delay_ms`.
- Logs: `kestreld/smoke.log` (append ops log, gitignored). Kill-switch trips + opens/closes + the daily digest go to TG bot (`yourbot_bot`) only if `TG_BOT_TOKEN` + `[notify] chat_id` set — all as HTML. Individual gate refusals are deliberately NOT pushed (they are normal, dozens a day); they are counted by kind in the digest. The digest is also appended to `docs/ledger/YYYY-MM.md`, which doubles as its delivery ledger: delete a day's section to force a re-send on the next boot.
- DB tables: positions, trades, equity, decisions, news, news_markets, news_seen, meta (bankroll, daily_count:, day_open:, day_key).
