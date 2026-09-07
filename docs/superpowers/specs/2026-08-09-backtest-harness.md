# kestrel backtest/replay harness (Wave 3)

**Date**: 2026-08-09 · **Status**: user-approved ("go ahead with all"), lead-specced · **Location**: `kestreld/` (same crate, new subcommand). PAPER-ONLY invariant: the harness reads public candles/funding and simulates — no order surface.

## Purpose

Replay historical market data through the real strategy machinery (screener → gates → sizing → brackets) to (a) sweep parameters before they touch the live paper book, and (b) validate changes with evidence instead of tape-waiting. First target questions: stop floor 0.6/1.0/1.4, conviction 0.70/0.75/0.80, regime_vol_max on/off, cooldown_after_sl 30/120m.

## Decisions (locked)

1. **Subcommand, not a new binary**: `kestreld backtest --from 2026-08-02 --to 2026-08-09 [--sweep key=v1,v2,...]... [--out docs/backtests/]`. The screener/sizing/risk/triggers modules are already pure — the harness is their second consumer. The daemon path is untouched (subcommand exits before any daemon setup).
2. **No LLM in the loop (v1)**: the analyst is replaced by a fixed `--conviction` parameter (default 0.75, sweepable). This tests the mechanical edge (screener+gates+brackets). Decisions recorded by the live system are NOT replayed in v1. Consequence stated in every report header: "mechanical replay — no analyst judgment, no news".
3. **Data**: 1m candles via the existing `hl_rest` candleSnapshot client (throttled, chunked ≤5000/request) + fundingHistory for the funding ring. Cached forever in `backtest_cache.db` (sqlite, separate file from kestreld.db; tables candles(market,t PRIMARY KEY,o,h,l,c,v), funding(market,t,rate)). Cache-first: refetch only missing ranges.
4. **Feature recomputation from candles**: r5m/r1h/r24h from closes; vol1h = stddev of 1m returns ×100 (same formula as live); range_pos from rolling 24h high/low; funding_z from the funding ring (same ring logic). **Known deviations from live, documented in the report**: day_ntl_vlm proxied by rolling 24h candle quote-volume (historical OI/vlm not served); mids cadence is 1m not 1/5s. Universe filter uses the volume proxy with the live thresholds.
5. **Fill model = the counterfactual walker's rules**, extracted to a shared module: entry at next 1m open after the signal candle + flat slip (2bp native / 5bp xyz); SL checked before TP within a candle (conservative); fees 7.5bp per side; time-stop and review-loop veto DO NOT exist in v1 (no analyst) — brackets and horizon expiry only. Gates that ARE replayed: kill switch, daily cap, per-market cap, cooldowns (incl. post-SL), morning budget, dup, max-concurrent, regime, staleness (trivially fresh).
6. **Engine**: single-threaded time loop over 1m steps across the cached universe; screener tick every step (live is 45s — 1m is the closest candle-aligned cadence, documented); in-memory ledger (positions/trades/equity) mirroring the live schema shapes; deterministic (no clock, no RNG — ties broken by market name).
7. **Output**: per-run `RunReport` — net, gross, fees, closes, win rate, expectancy/close, payoff, maxDD, kill-switch days, exit mix, gate-refusal counts, per-market table, equity curve (downsampled). Sweep mode: one report per combo + a ranked summary table (by net, then maxDD). Written as JSON + markdown to `docs/backtests/YYYY-MM-DD-<label>/`; markdown committed by the lead, JSON gitignored if large.
8. **Validation**: golden-run test on a synthetic 3-market fixture universe (hand-computable outcomes pinned exactly); walker edge cases inherit the counterfactual test patterns; feature-parity smoke — recomputed features vs live `/api/snapshot` features for the current hour within stated tolerances (r's exact, vol1h ±10%, funding_z ±0.1) run as an ignored-by-default integration test.

## Build stages (sequential — one crate)

- **B1 Data layer**: cache schema + chunked backfill fetcher (progress logging, resumable) + feature recomputation module with parity test. CLI: `kestreld backtest fetch --from --to` usable standalone.
- **B2 Engine**: replay loop + gates integration + fills + in-memory ledger + RunReport + single-run CLI.
- **B3 Sweep + report + docs**: `--sweep` grid runner (sequential runs, shared cache), markdown/JSON writers, ranked summary, README section, STATUS entry, the four target-question sweeps EXECUTED against the last 7 days with results committed as the first real reports.

Gates every stage: `cargo test` (248 baseline + new) + `cargo clippy --all-targets -- -D warnings`; daemon untouched (no restarts needed — subcommand only). Lead reviews between stages and runs B3's sweeps interactively if the agent's runs look off.
