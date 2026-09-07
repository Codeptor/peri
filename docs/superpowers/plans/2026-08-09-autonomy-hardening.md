# Autonomy hardening — Tiers 0–2 implementation plan

> **For agentic workers:** executed as four sequential Opus workflows (R1→R2→T1→T2), lead-reviewed between phases. Rust phases are strictly sequential (one crate, shared files). Every traderd phase ends: `cargo test` green (≥147 + new) + `cargo clippy --all-targets -- -D warnings` clean → lead rebuilds, restarts, verifies endpoints live.

**Goal:** the paper trader keeps itself alive, refuses to trade blind, tells the operator when it's sick, stops churning its edge away, and starts measuring its own decision quality.
**Basis:** user-approved improvement list (2026-08-09 ~14:05Z, "go ahead with all"). Evidence: two same-day wedges (ws-close trigger 10:42Z; REST `hl meta Shape` trigger 12:52Z → hang by 14:00Z), analyst decisions on frozen features 11:29–11:31Z, daily cap exhausted 11:30Z, fees 2× gross edge loss.
**Non-goals:** no live trading paths (PAPER invariant), no strategy overhaul beyond the listed knobs, no dashboard redesign (only additive panels in T2).

## Phase R1 — Reliability core (traderd: F-ws0..3)

**Stage 1, Investigator (read-only):** produce a diagnosis of the wedge with file:line evidence. Known facts: `/api/positions` + `/api/snapshot` hang forever while health/equity/trades/decisions/news serve; trigger #1 = both ws streams ended 10:42–10:45Z without reconnect; trigger #2 = `hl meta Shape("expected top-level array")` REST failures 12:52Z, NO ws death, wedge by 14:00Z; `health.ws_connected` stayed `true` through both. Hypothesis to test first: a lock/RwLock over shared market state (mids/ctxs/positions enrichment) held across an `.await` or an error path in `hl_ws.rs` / ctx-poll / universe-refresh, poisoning or starving the two handlers that need it. Deliverable: root cause + minimal-fix proposal + blast radius. Lead gates on confidence before Stage 2 runs.

**Stage 2, Fixer:** implement per diagnosis, PLUS (same files, one coherent change set):
- ws auto-reconnect: both HL streams reconnect with exponential backoff (1s→30s cap, jitter), forever; log INFO on reconnect.
- `ws_connected` truthful: derived from last-message age (<30s on either stream), not a set-once flag.
- Handler timeouts: `/api/positions` + `/api/snapshot` (and any handler awaiting ws-fed state) bound state access at 3s → HTTP 503 `{"error":"state unavailable"}`; never hang.
- Tests: reconnect scheduling, truthful-flag transitions, handler-timeout path (fake state that never resolves → 503). No weakening of existing risk tests.

**Acceptance:** gates green; lead deploys; kill one ws stream artificially impossible live — instead lead verifies reconnect via log after a forced restart and monitors for recurrence; 503-not-hang verified by inspection of tests + code.

## Phase R2 — Safety + ops (traderd: F-ws4..6)

- **Staleness gate:** new `[risk] max_feature_age_s = 120`. Entry path (screener→analyst→gate) refuses entries when the newest snapshot/feature update is older; refusal reason `StaleData` in decisions/gate logs. Reviews (manage/exit) still run — closing on stale data is safer than being stuck. Tests: fresh passes, stale blocks, boundary.
- **Watchdog task:** every 60s: mids age check (>120s → WARN + reconnect kick; >300s → TG alert once per episode), self-probe `GET 127.0.0.1:<port>/api/snapshot` with 4s timeout (2 consecutive fails → TG alert `wedge detected`). Episode-deduped alerts (no spam).
- **TG alerts via existing notify:** stream death, wedge detection, kill-switch trip, daily-cap exhaustion, boot ("traderd started, equity $X"). Config-gated by existing notify settings.
- **systemd user unit** `deploy/traderd.service`: Restart=on-failure, RestartSec=5, WorkingDirectory=repo/traderd, ExecStart release binary with config; docs: enable/start/status commands; STATUS ops table updated (setsid/nohup path deprecated).

## Phase T1 — Edge (traderd: items 6–9; config knobs all in traderd.toml with parse tests)

- **Reviewer context (analyst.rs):** review prompt gains `mark`, `unrealized_pnl`, `r_multiple` (uPnL ÷ initial $risk to SL), `held_h`, `fees_paid`. Fee-aware hurdle line added to BOTH prompts: round-trip cost ≈ 15bp of notional; open only when expected move clears ≥3× cost.
- **Churn control (risk.rs):** `per_market_daily_cap = 3` (entries per market per UTC day); asymmetric cooldown: `cooldown_min = 30` after TP/veto-profit, `cooldown_after_sl_min = 120` after SL; cap pacing: `morning_entry_budget = 12` entries max before 12:00 UTC (gate refusal `Paced`). New GateRefusal variants + frozen-order position documented + tests for each boundary.
- **Regime gate (screener or risk):** `regime_vol_max = 1.5` — when BTC `vol1h` exceeds it, block NEW entries (refusal `Regime`); reviews unaffected. BTC row read from the same snapshot; absent BTC row → gate inactive (log WARN once).
- STATUS amendment #9 recorded (user-directed 2026-08-09): churn knobs + staleness + regime gate values.

## Phase T2 — Learning loop (traderd API + dashboard panels; items 10–12)

- **`GET /api/gates`:** live gate state — kill (+day-open/threshold), daily count/cap, morning budget state, per-market counts, active cooldowns (market, until_ts, cause), regime gate state, staleness state. Cheap, DB+memory read, 3s-bounded like all handlers.
- **`GET /api/analytics`:** computed from ledger — conviction buckets (0.7–0.75/0.75–0.8/0.8+) × {closes, win rate, net PnL}; per-market {trades, net, fees}; exit-mix by day; veto counterfactuals: for each veto_close, replay stored SL/TP against HL candles (existing REST client, `candleSnapshot`, cached in a new `counterfactuals` table keyed by position id) → what the bracket would have realized vs actual. Compute lazily on request, cache forever per closed position.
- **Dashboard (small workflow, v3 blocks reused):** intel page gains a "Gates" SectionCard (why benched — live from `/api/gates`) and an "Analytics" card (conviction table + veto counterfactual net + per-market strip). Types added to `lib/api.ts` mirroring the new endpoints (this is an ADDITIVE exception to the frozen contract, lead-approved). `bun run typecheck` + both builds green.
- **Calibration note (#11):** analytics gives conviction-vs-outcome; STATUS follow-up records "revisit conviction_min after ≥100 closes" — no threshold change now.

## Gates & ops (every phase)

Rust: `cargo test` + `cargo clippy --all-targets -- -D warnings` from traderd/. Dashboard: `bun run typecheck` + `bun run build` + fixtures build. Lead between phases: rebuild release, `kill -INT` + relaunch (or systemd restart once R2 lands), verify health/positions/snapshot 200, tail log, commit with scoped message. User-locked values (conviction_min 0.70, stop floor 1.0/3.0, vlm floors, kill 12%) are UNTOUCHED throughout.

---

## CONTINUATION (2026-08-09 evening, user-selected slate)

**Wave 1 (quick wins):** conviction_min 0.70→0.75 (user-directed → amendment #10) · lib/ws.ts lint debt (dashboard + dash-plumbing) · blue primary across both dashboard themes (canonical design; orange stays for charts/data) · tg pipe systemd unit · smoke.log rotation + journald caps.
**Wave 2 (features):** daily TG digest at UTC day-roll (traderd notify) + nightly ledger append cron · dashboard: real ⌘K palette, equity-hero fill markers + drawdown shading + kill-floor line, counterfactual per-position detail · richer analyst entry context (same-market recent outcomes, drawdown state, open-book summary).
**Wave 2.5 (charts, user-selected full slate; launches after Wave 2 lands — file overlap):** shared SVG chart primitives first (BarChart signed+cumulative-line, Histogram, ScatterPlot numeric-axes, RollingLine — components/blocks/, v3 language, no new deps), then four parallel page agents: overview PnL anatomy (signed daily bars + cumulative, trade-net histogram, fees-vs-gross area), intel discipline (R-multiple timeline by exit kind, rolling win-rate/expectancy, conviction buckets as bars), markets breakdown (ranked per-market net bars, hold-time × net scatter), positions candle extras (volume histogram via lightweight-charts, uPnL-over-hold mini area). MANDATE (user-directed): REPLACE any existing chart whose form limits what the data can say (30d PnL matrix → signed bars + cumulative is decided; evaluate decisions/day triple-matrix → stacked bars, conviction table → bars), improve the rest — every replace-vs-improve call justified in the report. Dot-matrix remains the brand idiom where counts/presence is the story. Existing-chart polish: shared hover TooltipCard block replacing <title>-only tooltips everywhere (DotMatrix column hover, CrowdScatter dots, SegmentBar segments), crosshair OHLC/Δ readout row on the candle panel, consistent axis/tick/grid styling tokens across all charts, RingStat animated sweep (reduced-motion aware), min/max/last annotations on TrendArea heroes.
**Wave 3 (big rock):** backtest/replay harness — own spec first.
**RENAME → KESTREL (user-locked, executes between Wave 2 and Wave 2.5):** the autonomous system is named kestrel. Scope: `traderd/` dir → `kestreld/` (git mv), crate+binary `traderd`→`kestreld`, config `kestreld.toml`, DB file moved `traderd.db`→`kestreld.db` at cutover (hardcoded Store path updated; data preserved), systemd unit `kestreld.service` (old unit disabled/removed), dashboard branding Botta→Kestrel (wordmark, titles, PAPER pill context; falcon glyph if hugeicons has one), operative doc references updated (STATUS header/ops table; historical entries keep old names). tg-news-pipe unit unchanged (shared infra, port-addressed). The Python copytrader keeps the name botta; the repo directory stays ~/botta (hosts both). Lead performs the live cutover: stop old unit → mv DB → install+enable kestreld.service → verify endpoints → commit.
