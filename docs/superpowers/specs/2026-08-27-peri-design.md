# peri — autonomous LLM perp trader (design, 2026-08-27)

Locked by Bhanu 2026-08-26/27 (chat): single LLM brain, production pipeline, zero
fallbacks, live mainnet endgame on the Trench wallet, universe = crypto (HL native
majors) + equities + commodities (xyz builder dex), Trench builder fee on every
order. The old copy-trader's proven HL execution engine is retained; its two-tier
regex/LLM message-classifier pipeline is retired. Telegram calls (@caller1) become
one input among many to the one brain, which may mirror them or trade its own view.

## Non-negotiables

- **One brain.** One analyst LLM (`ANALYST_*` in .env, OpenAI-compatible). No
  fallback model, no regex decision path, no Noop degradation. Analyst failure
  after bounded retries = cycle skipped loudly (refusal recorded + notified),
  never a degraded decision.
- **Never blind.** Every decision cycle the LLM sees: account (equity, kill/caps
  state), open positions (live marks, uPnL, age, stops, its own prior rationale +
  invalidation), candidate markets with features (px, day%, funding, OI, vol,
  returns, ATR, range position), recent Telegram group messages verbatim (callers
  flagged, ages shown), news headlines with ages, and recent closed-trade
  outcomes (its own track record).
- **Code owns risk; the LLM owns direction.** Gate order (frozen, kestrel
  lineage): kill → max-concurrent → daily-cap → dup-market → cooldown →
  conviction ≥ 0.75 → stale-mirror → stop-sanity → RR ≥ 2.0 → sizing. Sizing is
  derived from stop distance: notional = (equity × risk_pct) / stop_dist_pct;
  margin = notional / leverage; leverage clamped to min(action, config, market
  max). Never the reverse. Min notional $10 refuses rather than inflates.
- **Every entry ships with a bracket** — stop + take-profit resolved to absolute
  prices at approval, placed as reduce-only triggers with the entry.
- **Paper history is the prior**: kestrel's tape showed fees (~0.21% RT via
  taker+builder) eating gross at churn. The prompt states the fee hurdle and the
  guard enforces cooldowns (asymmetric: longer after a stop-out).

## Architecture (src/peri/)

```
telethon feed (group msgs, caller flag, wake)  ──┐
HL info (native + xyz: ctxs, candles, meta)  ────┤
RSS news (headlines + ages)  ────────────────────┼──► context bundle ──► analyst (ONE LLM)
sqlite ledger (positions, closes, refusals)  ────┘                          │ Decision{actions[]}
                                                                            ▼
                                        risk guard (gates + sizing) ──► adapter
                                                                            │
                                        live: HyperliquidAdapter (builder fee, brackets)
                                        dry:  DryRunAdapter (mark fills, engine-simulated brackets)
```

- `config.py` — toml + .env (file wins over stale shell env; explicit load).
- `models.py` — pydantic Decision schema: OpenAction (market, side, conviction,
  stop, take_profit, leverage, source own|mirror, rationale, invalidation),
  CloseAction, AdjustStopAction.
- `market.py` — dex-aware data layer over HL info: merged meta (szDecimals, max
  leverage) + mids across native and xyz, asset ctxs, candle features,
  candidate screening (allowlist majors + xyz movers over volume floor +
  position markets + caller-mentioned markets).
- `state.py` — sqlite ledger: positions, decisions, refusals, fills_seen,
  tg_messages, daily, cooldowns.
- `analyst.py` — prompt builder + OpenAI-compatible chat call, strict JSON
  parse → pydantic; ≤2 retries; then AnalystError (loud).
- `risk.py` — the guard (gates above), returns Approved(sizing) | Refusal(reason).
- `feed.py` — telethon ingest: store raw msgs, flag callers, set wake event.
- `news.py` — RSS headlines (feedparser), per-feed containment.
- `router.py` — Adapter protocol + DryRunAdapter.
- `hl_adapter.py` — proven live executor (testnet-verified 2026-08-26), extended:
  market-layer injection for dex-aware sizing, per-trade leverage.
- `engine.py` — the loop: reconcile → equity/kill/day-roll → bundle → decide →
  gate → execute → record+notify. Cycle every cycle_secs AND immediate wake on
  caller message. Live reconcile via userFills (bracket closes detected, realized
  PnL recorded, cooldowns applied). Startup adopts unknown venue positions as
  source=external (visible + manageable).
- `app.py` — wiring + `--once` (single cycle) for smokes.

## Ops

- Testnet first (native majors prove the full loop; xyz is mainnet-only —
  first xyz fill is a deliberate tiny mainnet order at go-live).
- Mainnet go-live needs: Trench wallet address + an agent key valid for it,
  config flip (hl_network=mainnet, mode=live), bankroll decision ($250+
  recommended floor).
- Notifications: TG bot (TG_BOT_TOKEN) to control chat; stdout always.
- Restart: `kill -INT $(pgrep -x peri)` equivalent (uv run peri).

## Out of scope v1 (recorded, not forgotten)

- Analyst tool-calls (orderbook/candle pulls on demand), Tavily search context,
  multi-timeframe indicator series, dashboards, systemd unit, VPS deploy.
