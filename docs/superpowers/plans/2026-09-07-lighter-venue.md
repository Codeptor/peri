# Plan: Lighter venue support (shipped 2026-09-07 as 658dba6)

## Context

The operator drained the HL account and moved to Lighter.xyz. peri was
HL-only: the adapter, market data layer, tick rules and fee schedule all
encoded Hyperliquid. Research (three parallel tracks, 2026-09-06) found the
port is viable: grouped OTOCO orders attach brackets in one signed action,
fees are zero at the standard tier, 216 markets map near 1:1 with HL names,
and a testnet spike proved the bracket payload end to end (signed, sent,
rejected only at auth; then completed against a funded testnet account with a
live read-back and an in-place ratchet).

## Decision

Port behind a venue switch, not a fork. `venue = "hl" | "lighter"`
(default HL). The engine keeps consuming HL-shaped dicts; the new venue
normalizes at its boundary. Anything that prices a trade takes a
`FeeSchedule` (`HL_SCHEDULE` / `ZERO`) instead of reading module constants.

## What changed (files)

- `src/peri/lighter_market.py` — Lighter REST behind the Market surface
  (meta/ctxs/candles/features/affordable/candidates). `candle_features` is
  shared, not reimplemented. Symbol rule: strip prefix, plus an alias table
  containing ONLY certain mappings (verified listings, ISO metal codes, WTI,
  EUR/GBP-USD). Ambiguous names stay unmapped and raise KeyError.
- `src/peri/lighter_adapter.py` — the 14-method protocol over lighter-sdk
  1.1.2: grouped OTOCO resting entries (legs at size zero, inherit the fill),
  in-place `modify_order` ratchet, leverage set before every open (the venue
  reserves at the market default otherwise — found live), market-order fills
  read back off trades, fills normalized with open/close dirs and my-side
  realized PnL (`bid/ask_account_pnl`), fees at 1e6-USDC dust.
- `src/peri/lighter_sync.py` — one background loop driving the async SDK.
- `fees.py` — `FeeSchedule` + `HL_SCHEDULE` + `ZERO`; `Guard`/`Engine` take
  one (default HL). `config.py`/`app.py`/`config.toml`/`.env.example` carry
  `venue` + `[lighter]` + `LIGHTER_API_KEY`.
- 30 new tests (market/adapter/bridge/schedule/guard-zero/config); suite at
  483 green, ruff clean.

## Deliberate v1 gaps

1. No funding input on Lighter (no per-market current rate over the API;
   `funding_apr_pct` reads 0.0 = unavailable, not free).
2. No session gate on Lighter names (no dex prefix for the guard to key on).
   Crypto is unaffected (24/7); non-crypto trades without a home-hours check.
3. Testnet has no volume, so the analyst path was proven against mainnet
   reads, not testnet fills.

## Verification

- Full suite + ruff green before commit and before push.
- `LighterMarket` probed live read-only: 216 markets, ETH mark/ATR/range and
  the `io:ANTH` alias resolve off real responses.
- No live orders were placed from this work; the operator's manual ETH rest
  was untouched throughout.
