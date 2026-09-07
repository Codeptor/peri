# Plan D: Richer Data for `traderd` (Batches 2–4)

> **For agentic workers:** Self-contained; read the spec `docs/superpowers/specs/2026-08-08-auto-trader-design.md` §5 first. Execute ONE batch at a time; the orchestrator (leader) verifies between batches. Scope: `traderd/**` only. **Paper-only invariant: no live orders, no wallet signing, no real-funds paths — ever.**

**State baseline:** master @ `6b589fe`. Batch 1 (l2Book impact fills, `ed0581a`) merged and live. Daemon runs 24/7 (leader manages restart/deploy — workers never kill/restart it). Existing pinned tests MUST keep passing; fallbacks must degrade gracefully when new data is absent/stale.

**Leader gates (enforced after every batch commit):**
1. Worker commits batch → leader re-runs `cargo test` + `cargo clippy --all-targets -- -D warnings` independently (never trusts reports).
2. Leader inspects full diff: scope `traderd/**` only, no forbidden codepaths, no dropped invariants.
3. Leader rebuilds release, restarts daemon, watches `smoke.log` 5–10 min for panics/errors.
4. Only then does the next batch begin.

---

## Batch 2 — `allDexsAssetCtxs` ws streaming (commit: `feat(traderd): allDexsAssetCtxs streaming ctxs (real-time funding/OI/vlm)`)

**Goal:** funding/OI/vlm flow in real time into `FeatureEngine`; keep 30s REST poll as seed + fallback (also the universe-name source).

- [ ] Step 1 (verify-at-build): fetch `https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/websocket/subscriptions.md`; pin the exact subscription frame + data shape for `allDexsAssetCtxs` in a code comment (expected `{"method":"subscribe","subscription":{"type":"allDexsAssetCtxs"}}`; per-dex positional arrays of `PerpsAssetCtx` with NUMERIC `funding, openInterest, oraclePx, markPx, midPx?, dayNtlVlm, prevDayPx`). If actual differs — adapt, document, report the deviation.
- [ ] Step 2: name mapping. Ctx arrays are positional: build `dex -> ordered dex-prefixed market names` from `HlRest::meta_and_ctxs` (already zips `universe[i]`/`ctxs[i]`). Add `HlRest::universe_names(dex: Option<&str>) -> Vec<String>`. main.rs seeds `Arc<RwLock<HashMap<String, Vec<String>>>>` on bootstrap + hourly refresh.
- [ ] Step 3: `hl_ws::spawn_ctxs_stream(url, name_maps, tx: mpsc::Sender<Vec<CtxRow>>) -> JoinHandle` — same reconnect pattern as books stream (1s→30s backoff, resubscribe on reconnect), bounded channel 64. Positional decode → `CtxRow` (hl_rest type); skip out-of-range indices silently; degrade to no-op until name_maps seeded.
- [ ] Step 4: **funding_z ring hygiene** — ws ctxs arrive ~1/s. The trailing funding ring must append ONLY when the funding value actually CHANGES from last stored sample (guard `last_funding: Option<f64>` per market) — duplicates poison z-scores. Test: 500 identical pushes then a step change → ring has exactly 2 distinct samples, z reflects the step, not duplicates.
- [ ] Step 5: main.rs wiring — fan batches into `engine.on_ctx` + Snapshot ctx-field refresh (same fields the 30s poll updates: funding/OI/day_ntl_vlm/mark/oracle; mids stay authoritative from mids feed).
- [ ] Step 6: tests — parse verified-shape frame; positional mapping incl. out-of-range skip; ring hygiene; mock-ws integration (subscription frame asserted, batch emitted).

## Batch 3 — `fundingHistory` boot seed (commit: `feat(traderd): fundingHistory boot seed for funding_z`)

- [ ] Step 1: `HlRest::funding_history(coin, start_ms, end_ms) -> Vec<(i64, f64)>` — POST `{"type":"fundingHistory","coin":X,"startTime":...,"endTime":...}` → `[{coin,fundingRate,premium,time}]` (string numbers). VERIFY xyz coin naming (expected `xyz:TSLA`) with one live curl captured as test fixture comment.
- [ ] Step 2: `FeatureEngine::seed_funding_history(market, samples: &[(i64, f64)])` — fills ring oldest→newest (respects the Batch-2 change-guard semantics).
- [ ] Step 3: main.rs boot seed after universe bootstrap: last-7d history per tracked market, throttled (100ms between calls, 1s pause every 20), warn-and-continue on failure. `ANALYST_STUB=1` smokes must still boot fast — skip seed when stub is set.
- [ ] Step 4: tests — response parse; seeded z-score vs unseeded; seeded ring count.

## Batch 4 — candles → ATR in analyst prompt (commit: `feat(traderd): candle ATR in analyst prompt`)

- [ ] Step 1: `HlRest::candle_snapshot(coin, interval, start_ms, end_ms) -> Vec<Candle>` — POST `{"type":"candleSnapshot","req":{"coin","interval":"15m","startTime","endTime"}}`. Fields arrive short-named string numbers (`t,T,s,i,o,c,h,l,v,n`) — parse defensively.
- [ ] Step 2: `atr_pct(candles: &[Candle], period: usize) -> Option<f64>` (Wilder ATR on high/low/close, as % of last close) in features.rs.
- [ ] Step 3: `AnalystInput` gains `atr_pct: Option<f64>` (update all construction sites: analyst.rs tests + main.rs decide path). Prompt line `atr15m=<x.xxx>%` when Some, omitted when None.
- [ ] Step 4: main.rs — at decide time per nominee: 15m candles, 6h window, ATR(14) → AnalystInput. Failure → None, never blocks decide.
- [ ] Step 5: tests — candle parse; pinned ATR math on synthetic series (hand-computed expected); prompt includes/omits line correctly.

## Global constraints (all batches)

- No new crates; no `unwrap()` outside tests; `tracing` not `println!`; bounded channels only; per-batch: fail-test → implement → clippy `-D warnings` + `cargo test` green → commit (messages exactly as above, no co-author lines).
- Worker must NOT kill/restart the running daemon and MUST report: per-batch commit hash, final test count, clippy result, verified-shape findings/deviations (esp. `allDexsAssetCtxs` frame shape + xyz fundingHistory coin naming), and anything not verifiable. Never claim an unrun pass.
