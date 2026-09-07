# Trench Copy-Trade Bot — Design Spec

**Date:** 2026-07-08
**Status:** Approved (rev 3) — writing implementation plan

## 1. Goal

A always-on service that reads the Telegram group **"the caller group"**, mirrors the
perp calls of one trusted caller (**@caller1**), and executes the equivalent positions
automatically on **Hyperliquid and Solana (Drift perps / Jupiter spot), routed per
asset** — including live management (partial closes, stop moves, exits) — with hard
risk guardrails and a dry-run mode.

## 2. Context (from a 400-message sample, 2026-06-27 → 07-08)

- **Trench itself is now Hyperliquid perps only** (memecoin/spot sunsetted 2026-07-07).
  The bot executes on the underlying venues directly, not the trench.ag UI (non-custodial,
  Cloudflare-walled, no public API). Per the user's requirement, the bot supports **both
  Hyperliquid and Solana** so it isn't locked to one venue (and covers a memecoin-szn return).
- **Signal source:** @caller1 posts structured entries and manages them live. Everyone else
  reacts/asks — excluded.
- **Assets observed:** SOL, BTC, HYPE, GOLD (+ occasional ETH/AAVE mentions).
- **Call style:** loose, multi-line, heavy Hinglish. Examples:
  - Entry: `AVAX LONG` / `TP: 9.15` / `SL: put it where you're comfortable` (SL = "keep whatever")
  - Entry: `GOLD SHORT` / `TP: 4144` / `SL: 4169`
  - Entry (bare): `BTC SHORT — calm down on leverage`
  - Manage: `going good, book 50% profit here — move SL to 82.35`
  - Manage: `book 50% here — move sl to entry`
  - Exit: `hit full tp` / `closed the rest` / `flat on this`
  - Follow-up TP/SL as a **separate** message shortly after entry: `Conservative TP: 82.52`
- **Images are frequent** (95 of 400 msgs; 55 from @caller1; latest 2026-07-08 09:27 UTC).
  They are **charts + stats banners for human context** — the actionable call lives in the
  **caption** (Telethon captures it as message text). No OCR needed for v1 (see §6).
- **No size/leverage in calls** — qualitative only ("low lev"/"high lev"). Bot uses
  configured fixed size + leverage.

## 3. Scope

**In:** Telegram ingest, LLM signal parsing, a **venue router** with three execution
adapters (Hyperliquid perps, Drift perps, Jupiter spot), position tracking, risk limits,
kill switch, dry-run, Telegram notifications + control commands.

**Out (v1):** other callers, trench.ag UI automation, a web dashboard, backtesting, image
OCR (deferred to v2). Memecoin spot is wired (Jupiter) but dormant until such calls appear.
An asset with no market on its routed venue → skipped with an alert.

## 4. Architecture

```
Telegram (the caller group)
      │  new/edited messages (text + captions)
      ▼
[Listener]  Telethon user client · filters to @caller1 · stale-signal guard
      │  raw message + context (open positions, last N msgs)
      ▼
[Parser]  regex fast-path → LLM fallback → Intent{ENTRY|MANAGE|EXIT|NOOP} + confidence
      │  validated intent
      ▼
[Risk Manager]  idempotency · sizing · caps · kill switch · ambiguity gate
      │  approved action
      ▼
[Position Manager]  resolves "here"→position · updates state · picks venue for the asset
      │
      ▼
[Venue Router]  per-asset map → one adapter (shared interface)
      ├── [Hyperliquid Adapter]  agent wallet · market open · reduce-only TP/SL · leverage
      ├── [Drift Adapter]        Solana perps · driftpy · subaccount · TP/SL triggers
      └── [Jupiter Adapter]      Solana spot swaps (memes) · quote → swap → sign
      │
      ├──▶ [State store]  SQLite: processed msgs, positions(+venue), trades, daily pnl, killswitch
      └──▶ [Notifier/Control]  DM fills/errors · /status /pause /resume /flat · ambiguity confirms
```

Each unit is independently testable: the Parser is pure `(message, context) → Intent`;
each Adapter implements one interface (`open / partial_close / move_sl / close /
get_position`) with no Telegram knowledge; the Risk Manager is pure predicates over state.

## 5. Signal model

Parser emits one discriminated-union intent (Pydantic), always with `confidence` (0–1)
and `source_msg_id`:

- **EntryIntent** — `asset`, `side` (long|short), `entry` (price | "market"),
  `tps: [price]` (0–2, ordered), `sl` (price | pct | null).
- **ManageIntent** — `op` (book_partial | move_sl | trail_sl), `pct` (default 50),
  `sl_target` (price | "entry" | "+Npct"), `position_ref` (asset | "current" | null).
- **ExitIntent** — `position_ref`, closes 100%.
- **NoOp** — chatter, questions ("SOL long?"), bias ("only look for shorts here"),
  news, trade recaps. Never executed.

## 6. Parser design

- **Regex fast-path** catches obvious `^<ASSET> (LONG|SHORT)` + `TP:`/`SL:` lines cheaply;
  escalates anything ambiguous.
- **LLM fallback** — cheap fast model via the Lightning gateway (`openai/gpt-5-nano`),
  Anthropic Haiku as fallback. System prompt pins the intent schema, the asset universe,
  and Hinglish examples. Receives **context**: currently-open positions + last 5 messages,
  so it can resolve `book 50% here` → a specific position.
- **Confidence gate:** below threshold → NoOp in dry-run, or a confirm-DM in live mode.
  Never trade on a guess.
- **Follow-up augmentation:** a message with only TP/SL shortly (≤5 min) after an entry
  attaches to that position rather than opening a new one.
- **Edit handling:** Telethon `MessageEdited` re-parses recent calls (he corrects typos);
  if the position isn't acted on yet, use the corrected version.
- **Images (v1):** parse the **caption** only; the chart image is human context. **Vision
  OCR is deferred to v2** — his charts don't label which line is TP vs SL, so image
  extraction is unreliable and low-value. The bot *can* download + vision-read images
  (proven), reserved for a later "levels only on chart" enrichment.

## 7. Execution — venue router + adapters

**Router:** a per-asset config map picks exactly one venue per call, e.g.
`{SOL, BTC, HYPE, GOLD, ETH → hyperliquid}`, with Drift/Jupiter assignable per asset.
Default perps venue = Hyperliquid; a management/exit command always routes to the **venue
the position was opened on** (stored with the position).

**Shared adapter interface:** `open(asset, side, margin, leverage, tps, sl)`,
`partial_close(pos, pct)`, `move_sl(pos, target)`, `close(pos)`, `get_position(asset)`.

- **Hyperliquid** — **agent/API wallet** (approved by master, **cannot withdraw**). Market
  open at configured leverage/margin, isolated margin; reduce-only trigger orders for TP/SL.
- **Drift (Solana perps)** — `driftpy` with the user's Solana keypair + a dedicated
  subaccount; perp market order, trigger orders for TP/SL, per-market leverage.
- **Jupiter (Solana spot)** — quote → swap tx → sign → send via Helius RPC; for memecoin
  spot ("long"=buy, exit=sell). Dormant until such calls appear.
- **TP/SL logic (all venues):** two TPs → 50% reduce-only at conservative TP, remainder at
  full TP; one TP → single reduce-only (or configured partial); SL from message else
  configured default (never none).
- **Asset map:** unmapped/unavailable market on the routed venue → skip + alert.

## 8. Risk management (real money)

All configurable; values below are **defaults to tune before going live**:

- **Fixed margin/trade:** 5 USDC (default, = user minimum) · **leverage:** 3x (default).
  A per-venue **min-notional guard** ensures margin×leverage clears each venue's minimum
  order value (Hyperliquid ~$10). At 5 USDC × 3x = $15 notional this passes; the bot rejects
  + alerts rather than sending a sub-minimum order.
- **Default SL** when none given: 1.5% of entry price.
- **Max concurrent positions:** 3 (aggregate across venues) · **daily entry cap:** 15.
- **Idempotency:** every acted `message_id` persisted; never double-execute (survives restart).
- **Dedupe:** ignore a repeat entry for an asset+side already open.
- **Slippage cap:** 0.5% on market orders.
- **Drawdown kill switch:** aggregate equity (HL + Solana) down ≥15% from the day's open →
  halt new entries, alert; optional auto-flatten (config, default off).
- **Stale-signal guard:** ignore any call older than 120s (no replaying backlog as fresh trades).
- **Ambiguity gate:** a MANAGE/EXIT intent not tied to exactly one open position → do not act,
  DM the user to confirm.

## 9. State (SQLite)

- `processed_messages(msg_id, chat_id, acted, intent_json, ts)`
- `positions(id, venue, asset, side, entry, size, leverage, sl_price, tp_orders_json, venue_ref, status, opened_ts)`
- `trades(id, position_id, action, price, size, pnl, fill_json, ts)`
- `daily(date, start_equity, realized_pnl, entries_count, killswitch_tripped)`

## 10. Control & notifications

Bot DMs the user (or a private channel):
- On every action: `Opened SOL LONG 3x · 5 USDC @ 81.2 · TP 83.4 · SL 79.9 · [Hyperliquid]`.
- On errors, kill-switch trips, and ambiguity confirmations (inline yes/no).
- Commands (from the user only): `/status` (open positions + day PnL per venue), `/pause`,
  `/resume`, `/flat` (close all), `/mode dry|live`.

## 11. Modes

- **dry-run (default on first boot):** reads, parses, logs *what it would do*, DMs the same
  notifications tagged `[DRY]`, places **zero** orders. Validate parsing + sizing + routing on
  live signals for a day.
- **live:** flip one config flag. Places real orders.

## 12. Failure handling

- Order reject / RPC error → bounded exponential-backoff retry, then alert (never silent drop).
- Partial fill → record actual filled size; TP/SL sized to real position.
- Telegram disconnect → Telethon auto-reconnect; backlog marked seen (stale guard), not traded.
- Reduce-only "would increase position" (seen in group) → re-fetch live size before any close.

## 13. Tech stack & deployment

- **Python 3.12 + uv.** `telethon` (MTProto), `hyperliquid-python-sdk`, `driftpy` +
  `solders`/`solana` (Solana), `httpx` (Jupiter API), `pydantic`, `sqlite3` (stdlib), an LLM
  client (Lightning gateway / Anthropic).
- Single asyncio daemon. Config via `.env` + `config.toml` (incl. the asset→venue map).
  Deploy as a **systemd** service on the VPS. Secrets (`.env`, `botta.session`, agent key,
  Solana keypair) gitignored, never committed.

## 14. Verify at build (read current docs first)

- Hyperliquid: agent-wallet approval, `market_open`, reduce-only trigger (TP/SL) orders,
  `update_leverage`, isolated margin. Confirm GOLD/HYPE market symbols.
- Drift: `driftpy` perp order + trigger orders, subaccount/collateral deposit, market indexes.
- Jupiter: current quote/swap endpoints + tx signing flow.
- Lightning gateway billing string + `gpt-5-nano` params (no `max_tokens`).
- Telethon session persistence + `MessageEdited` event.

## 15. Build order (full plan follows from writing-plans)

1. Telethon listener + allowlist + dump→parser plumbing (dry-run, no execution).
2. Parser (regex + LLM) with the sampled messages as test fixtures.
3. SQLite state + idempotency + position tracking (with venue).
4. Venue router + adapter interface; **Hyperliquid adapter on testnet** first
   (open/close/TP/SL/partial/leverage), then **Drift on devnet**, then **Jupiter**.
5. Risk manager + kill switch + ambiguity gate (aggregate across venues).
6. Notifier + control commands.
7. Wire dry-run end-to-end against live group; validate; flip to live.
