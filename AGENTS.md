# AGENTS.md — peri

This file orients AI coding agents working in this repository. The reader knows
nothing about the project.

## Project overview

**peri** is an autonomous LLM perpetual-futures trader for Hyperliquid, trading
through **Trench** (a builder on HL — every order carries the Trench builder fee
and the account is the owner's Trench wallet). It also trades Lighter.xyz
behind `venue = "lighter"` (HL stays the default). One Python daemon, one brain:

- **Universe**: crypto majors on native HL (`BTC ETH SOL HYPE XRP DOGE`) plus
  every configured **builder dex** (`universe.dexes = ["xyz", "io"]`): equities,
  commodities and index futures on `xyz` (117 markets), private-company
  synthetics on `io` (`io:ANTH`). ~355 markets, mainnet-only, one wallet,
  automatic routing by coin prefix. Trench is a *builder* on HL, so any dex HL
  lists is reachable with the same builder code — the limit is config, not venue.
- **Venues**: `venue = "hl"` (default) trades HL as above. `venue = "lighter"`
  swaps in `lighter_adapter.py` (grouped OTOCO entries whose legs inherit the
  fill, in-place stop ratchet), `lighter_market.py` (same engine surface over
  Lighter REST), and the `ZERO` fee schedule — analyst, gates, sizing, ledger
  and memory move unchanged. Configure plain Lighter symbols (`ETH`, `NVDA`,
  `ANTHROPIC`); ambiguous HL names (`xyz:SKHX`, `xyz:SP500`, inverted-FX pairs)
  stay unmapped and raise like unknown markets. Known v1 gaps: Lighter exposes
  no per-market funding rate (reads zero = unavailable), and its names carry no
  session mapping, so non-crypto markets trade without a home-hours check.
- **Every market keeps ITS OWN session** (`risk.py`): Globex for commodities and
  index futures (Sun 18:00 ET → Fri 17:00 ET, daily 17:00 halt), the home
  exchange for foreign equities (KRX/TSE/HKEX — `xyz:SKHX` is the largest market
  on the dex), the US cash session for US single names. Symbols that cannot be
  mapped with confidence stay on the conservative US rule.
- **One analyst LLM** (muse via `ANALYST_*` in `.env`, OpenAI-compatible chat)
  decides everything. It is **never blind**: each cycle it sees account state,
  its open positions (with its own prior rationale + invalidation), dex-aware
  candidate features, raw Telegram group messages (allowlisted callers like
  @caller1 flagged — it may mirror their calls or trade its own view), RSS news
  headlines, and its own recent closed trades. Caller messages wake the engine
  immediately; otherwise it cycles every 15 min.
- **Code owns risk** (frozen gate order: kill → operator pause → day-loss halt →
  max-concurrent → daily-cap → dup-market → venue-order → cooldown →
  conviction ≥ 0.75 → stale-mirror → session → range-edge → stop-sanity
  (≥2% and ≥4×ATR15m) → RR ≥ 2.0 → leverage → **isolated-liquidation band** →
  sizing → min-notional → margin → projected-net-TP). Sizing derives from stop
  distance (`notional = equity × risk% / stop_dist`) **priced at the entry, not
  the mark** — and so is the range-edge test, so a resting order is judged where
  IT sits, not where the market is. Lots FLOOR to the venue step and the
  invariants are re-asserted after rounding, and the floored lot is what the
  adapter sends — there is no second rounding site (an `Approved` that carries
  no lot raises rather than re-deriving one). **Zero fallbacks**: the range and
  ATR gates REFUSE when their features are missing rather than skipping.
- **Nothing is gated on a stale price.** The decision takes 180-225s and the
  cycle is often woken *because* the tape moved, so between deciding and acting
  the engine re-reads marks, account and open orders
  (`Engine.refresh_execution_context`) and every rail is judged against those.
  A MARKET entry whose mark drifted more than `risk.max_mark_drift_pct` (0.5%)
  while the analyst was thinking is refused — priced at the old mark it is a
  chase by the time it lands. A RESTING entry is exempt (its level is an
  explicit price) but is still re-gated, so a limit that has ended up on the
  wrong side of the mark is caught. Each executed action also updates the
  reserved-market set and the account the NEXT one is judged against; reusing
  one pre-decision snapshot let two opens in a single decision both see an
  empty reserve and the same available margin.
- **The daily cap counts orders on the book**, not only filled entries: a
  resting order reserves its slot at placement and gives it back if it expires.
  The kill switch and the day-loss halt both WITHDRAW resting entries, the way
  operator pause always has — otherwise an order parked before the halt fills
  straight through it.
- **Entries can rest.** `OpenAction.entry` parks a maker limit at the analyst's
  level (below mark for longs, above for shorts) with SL+TP attached in ONE
  signed action (`bulk_orders(..., grouping="normalTpsl")`), so the brackets arm
  the instant it fills. Unfilled entries expire (`entry_expiry_secs`); a `close`
  on a market that has a resting entry and no position CANCELS it. This is the
  cure for the 2026-08-28 chase-and-stop-out pattern. Re-opening a market that
  already carries YOUR resting entry **replaces** it — the old order is
  withdrawn and the new one placed, judged by every rail from scratch. That is
  how the analyst keeps a level alive as it nears expiry or re-prices it; a
  venue order that is not peri's still refuses the open.
- **Fees are real and were wrong.** Verified against `userFees`: HL taker 4.5bp,
  maker 1.5bp, Trench builder 3bp on every order. `FEE_RATE` is 7.5bp/side
  (was 10.5, a 40% overstatement that refused trades clearing the TP floor), and
  HL reports `closedPnl` GROSS with the entry fee on the OPENING fill — so
  `positions.entry_fee` is attributed from the real fill and subtracted at close,
  **including on a round trip reconstructed between two reconciles**, which used
  to be booked gross of what it cost to get in. The schedule has exactly ONE
  definition, `peri/fees.py` (it had drifted into three copies: router's
  constants, literals inlined in `risk.gate_open`, and `market.close_fee_rate`).
  Per-venue pricing lives in the same module as `FeeSchedule`: `HL_SCHEDULE`
  and `ZERO` (Lighter standard tier, no maker/taker fee). `Guard` and `Engine`
  take one and default to HL, so a zero-fee venue prices at zero with no
  special case at any call site.
- **The engine manages positions without the LLM**: breakeven at +1R (stop →
  entry + round-trip fees) and a time stop at 3h below +0.5R.
- **Wakes are price-driven**, not just scheduled: `pricewatch.py` polls marks
  every 30s and wakes the analyst on a 1.0%/5m move, or a 24h breakout that
  CLEARS the level by `breakout_pct` (0.15%). Without that margin a market
  grinding higher re-breaks its own high every poll — 61 of 64 wakes on
  2026-08-30 were under 0.2% past the level and cost 2 compute-hours for nothing.
  Cadence adapts (900s quiet / 300s when the tape is hot).
- **It sees Trench's own market bias** (`trench.py`). Two public endpoints the
  Trench app uses, reachable server-side with no auth — the sentiment one is a
  POST (a GET 404s): every HL trader bucketed by realised PnL with how each
  cohort is positioned, plus `/market-sentiment/asset?coin=X` for ONE market
  (works for `xyz:` names too). Rendered as smart-minus-crowd in percentage
  points on every candidate, and callable on demand as the `market_bias` tool.
  Divergence is evidence about positioning, never a thesis.
- **It knows the calendar.** Trench's economic calendar is synced each cycle
  (USD high/medium, deduped) into `calendar_events`, and it is load-bearing:
  `risk.event_blackout_mins` REFUSES a new entry inside the window before a
  high-impact print. Operator context with a shelf life goes in `/api/notes`
  (ages out at 72h); a durable rule goes in lessons. `/api/bias` sets a standing
  operator view — advisory, never a gate.
- **The operator has controls from a phone** (`control.py`): Telegram slash
  commands `/status /book /why /memory /wake /pause /resume /close`, authorised
  to `telegram.control_user` alone. There is deliberately NO way to open a trade
  from chat — every mutating command reduces risk or asks the analyst to think.
- **It has memory** (`state.lessons` + `state.performance_digest`). Two halves:
  a MEASURED RECORD computed from the ledger every cycle (every closed trade by
  entry style / 24h-range position / side / outcome, with n, win rate, net $,
  avg R against the risk actually taken) — it cannot be hallucinated; and
  LESSONS the analyst writes via a `remember` action (capped 30, deduped,
  ranked pinned > markets-in-play > newest). Operator lessons via
  `POST /api/lessons` are pinned forever. Positions capture the setup at
  decision time (`entry_style/entry_range_pos/entry_atr_pct/entry_trigger`) so a
  loss attributes to a PATTERN, not just a trade. Dashboard: `/memory`.
- **Protection is code-owned, not hoped for.** A stop must sit outside the
  isolated liquidation band (`1/L − 1/(2·Lmax)`, 1.3× safety); a live position
  found without venue brackets is re-armed from `init_stop_px` and reported;
  `cancel_brackets` is never called while a position is open on that market;
  closes are classified by side, not price proximity, so a slipped stop still
  earns the 4h cooldown.
- **Operator pause** (`POST /api/pause`, dashboard button) halts every new entry
  as gate 2, cancels resting entries, and survives restarts (ledger-backed).
  Open positions keep their venue brackets and stay manageable.
- **Modes**: `mode="dry"` paper-trades against live mainnet data (adverse-slip
  fills, 10.5bp/side fees, engine-simulated brackets). `mode="live"` places real
  orders via the agent-key-signed Hyperliquid SDK (testnet-verified 2026-08-26).

The spec is `docs/superpowers/specs/2026-08-27-peri-design.md`. History: peri
grew out of the botta copy-trader (its proven HL execution engine survives in
`hl_adapter.py`); the old two-tier regex/LLM Telegram classifier is retired.

## Repository layout

```
src/peri/          the daemon (hatchling package, src layout)
  app.py           wiring + CLI (`uv run peri`, `--once` = single smoke cycle)
  engine.py        the loop: reconcile → equity/kill/day-roll → bundle → decide
                   → gate → execute → record+notify; caller-wake + scheduled
  analyst.py       the ONE brain: prompt builder + OpenAI-compatible call,
                   strict JSON → pydantic, 2 retries then loud AnalystError
  market.py        dex-aware HL info layer: merged native+xyz meta/ctxs/candles,
                   candle features, candidate screening
  risk.py          the guard: frozen gates + stop-distance sizing
  models.py        Decision schema (OpenAction/CloseAction/AdjustStopAction)
  state.py         SQLite ledger (peri.db): positions, decisions, refusals,
                   fills_seen, tg_messages, daily, cooldowns
  hl_adapter.py    live executor: brackets on every open, trench builder fee on
                   every order, dex-abstraction, segregated-collateral equity
  hl_client.py     SDK client construction (agent key, dex-aware on mainnet)
  hl_sizing.py     HL tick rules (≤5 sig figs, ≤6−szDecimals decimals)
  lighter_adapter.py  live Lighter executor: one-action OTOCO entries, in-place
                   ratchet, HL-shaped normalization so the engine is untouched
  lighter_market.py   Lighter REST data layer on the same engine surface
                   (meta/ctxs/candles/features/candidates) + symbol aliases
  lighter_sync.py  one background event loop driving the async SDK
  router.py        Adapter protocol + DryRunAdapter (paper fills)
  fees.py          HL_SCHEDULE + ZERO FeeSchedules, ONE definition (imports
                   nothing from peri, so risk/engine can all depend on it)
  feed.py          telethon ingest: raw msgs stored, callers flagged, wake event;
                   attached images transcribed at ingest via vision.py
  vision.py        the eye: one bounded vision call turns a posted chart into
                   facts the analyst reads (same model, budgeted, never advises)
  news.py          RSS headlines (feedparser), per-feed containment
  trench.py        Trench cohort bias (by cohort + by asset) and its calendar
  control.py       telegram slash commands, authorised to one operator
  pricewatch.py    cheap mark poller -> price-triggered analyst wakes
  notifier.py      stdout + optional Telegram bot alerts (UTC+IST stamps)
  config.py        config.toml + .env (.env file wins over stale shell env)
tests/             pytest suite (483 tests, no network — Trench/HL/Lighter fetchers are
                   injected on the Engine so the suite stays offline; fixtures/messages.py has
                   synthetic message-shape fixtures)
config.toml        runtime config (mode, universe, risk knobs, analyst, news)
peri.db            runtime ledger (gitignored); peri.session = Telegram login
Root scripts       hl_smoke.py (TESTNET live-adapter smoke, hard-refuses mainnet),
                   approve_builder.py (one-time builder-fee approval, master key),
                   make_agent.py (mint HL agent wallet), fetch_messages.py
                   (interactive TG login), manual_trade.py (one operator-authorized
                   bracketed order, no gates), manage_trade.py (replace protection:
                   full-size stop + split TP tranches), backfill_entry_fees.py,
                   wire_telegram.py (discover the operator chat id for alerts)
docs/superpowers/  STATUS.md (living truth) + specs/ + plans/ + the manual
                   trading runbook + dated handoffs
docs/memory/       what the bot learned, exported from a live ledger: its own
                   lessons plus the measured record of every closed trade
README.md          setup from zero for a new contributor; this file is the
                   architecture reference behind it
archive/           RETIRED systems, kept for reference: kestreld (Rust paper
                   trader — pattern source for peri's gates/fills), databoard
                   (LLM arena handoff, never built), dashboard + dash-plumbing
                   (kestrel's Next.js terminal), tg_news_pipe.py. Do not extend.
```

## Build and test

```bash
uv run --group dev pytest -q               # 483 tests, must stay green
uv run --group dev ruff check src tests    # lint (line-length 110)
uv run peri --once                         # one real decision cycle (dry: paper)
uv run peri                                # the daemon (telegram feed + loop)
uv run python hl_smoke.py                  # TESTNET live-order smoke (flip
                                           #   hl_network=testnet first)
```

Gates that must stay green before every commit: full pytest + ruff. Never trust
a claimed pass without rerunning it.

## Config & secrets

- `config.toml` — TUNING ONLY, committed and shareable. Identity (mode,
  network, group id, callers, control user, notify chat) reads from `.env`
  first via `PERI_MODE`/`HL_NETWORK`/`TG_GROUP_ID`/`TG_CALLERS`/
  `TG_CONTROL_USER`/`TG_NOTIFY_CHAT_ID`, falling back to the file. The shipped
  file has the ids zeroed and `mode="dry"`; this box carries its real values in
  `.env`. Everything else lives here: `[analyst]` cadence (`cycle_secs_quiet`/`cycle_secs_active`/
  `heat_atr_pct`)/timeouts/conviction floor, `[universe]` native allowlist + xyz
  volume floor/movers, `[risk]` all knobs (risk_pct 5.0, max_concurrent 3,
  max_leverage 20, RR ≥ 2, 45m blackout before a high-impact print,
  kill 15%, day-loss halt 6%, cap 3/day, min stop 2% & 4×ATR, range-edge
  0.20/0.80, 30m equity open/close blackout, breakeven 1R, 3h time stop,
  asymmetric cooldowns 1h/4h, min notional $10, 0.5% max mark drift between
  deciding and acting), `[news]` RSS list, `[watch]`
  price-wake poller, `[notify]` chat id.
- `.env` (gitignored, 0600; template in `.env.example`) — `TG_API_ID/HASH`,
  `TG_BOT_TOKEN`, `HL_ACCOUNT_ADDRESS` + `HL_AGENT_KEY` (agent key signs, cannot
  withdraw), `ANALYST_API_KEY/BASE_URL/MODEL` (muse), `TAVILY_API_KEY` (unused
  v1), plus the identity overrides listed above.
  **The .env file wins over inherited shell env** (kestrel scar inverted
  deliberately — a stale exported var must never silently override the file).
- `peri.session` — Telegram user login (precious, created interactively by
  `fetch_messages.py`).

## Trading invariants

- **Dry until proven.** `mode="dry"` is the default; live requires an explicit
  config flip. hl_smoke.py hard-refuses non-testnet. The first live xyz fill is
  a deliberate tiny mainnet order (xyz does not exist on testnet).
- **Every order carries the Trench builder fee** (`TRENCH_BUILDER`, 0.03%) when
  `route_builder_fee = true`. The account must approve the builder once per
  network (`approve_builder.py`, user-signed — an agent key cannot).
- **Prices obey HL tick rules** via `hl_sizing.format_price` — never `round()`
  a price straight into an order.
- **Builder-dex collateral is segregated** (verified on mainnet 2026-08-27):
  equity = native + xyz clearinghouse sum; live startup enables dex abstraction
  so margin routes automatically.
- **Risk rails are load-bearing.** Don't weaken gates, cooldowns, the conviction
  floor, or the RR floor to make anything pass. Refuse rather than inflate
  (min-notional). The kill switch halts entries, never manages positions.
- Reconciliation is the source of truth for closes: live bracket fills are
  detected via `userFills` (dedupe by tid in `fills_seen`), realized PnL =
  closedPnl − fees; unknown venue positions are adopted at startup as
  `source="external"` so the brain always sees the whole account.

## Security

- Secrets live only in `.env` (0600). Never print key material; the analyst
  base URL/model are config, not secrets.
- The agent wallet signs trades but **cannot withdraw**; `HL_ACCOUNT_ADDRESS`
  must be the main wallet, not the agent's.
- `TG_BOT_TOKEN` lives only in `.env`. Rotate through BotFather if one is ever
  exposed.
- Restart discipline: exact-PID signals. SIGTERM and SIGINT are both handled —
  the daemon takes the trade lock before exiting, so it leaves BETWEEN trade
  actions rather than between placing an order and recording it. Never
  pattern-wide `pkill -f` (self-match trap — bracket trick if you must).
- Mutating API endpoints refuse a request with no provenance: a browser is
  identified by `sec-fetch-site`/`Origin`, and a deliberate script must send
  `x-peri-cli: 1`. A bare `curl -X POST /api/pause` paused the live trader on
  2026-08-31.

## Working conventions

- Spec → plan → build, documents in `docs/superpowers/`; STATUS.md amendments
  are user-locked and normative. Improve → test → commit; small revertible
  commits, gates green each time.
- Extend test fixtures from reality (synthetic message shapes), don't invent
  formats. Unit tests never hit the network.
- Runtime artifacts (DBs, sessions, logs, media/) are gitignored.

<!-- graft:start -->
## Graft — repo context graph

This repo is indexed in `graft/`: small linked markdown nodes that explain each
system and carry exact file:line spans, kept in sync with the code through git.

For ANY task here — understanding how something works, finding where code lives,
or scoping a change — get context from the graph before grepping or opening
source files. Re-ask freely (it's cheap) and reuse literal identifiers you
already have (symbol, error string, file name) as the query. New to this repo?
Run `graft map` first — a token-budgeted orientation (dir clusters, hubs,
hotspots), no LLM, no key.

- Run `graft ask "<your question>" --source` → ranked nodes with the relevant
  code spans inlined (each hit's ≤8-line crux by default; `--full` for whole
  definitions when the crux isn't enough). Match the tool to the task shape:
  for understanding or editing, the top node IS the answer — cite its
  `covers:` file:line spans and edit straight from `--source`. For
  exhaustive tasks ("every occurrence / every caller of this pattern"), ranked
  results are top-N, not complete — run `graft grep "<literal>"` instead
  (exhaustive over indexed files, grouped by enclosing symbol), falling back
  to raw `grep -rn` only for unindexed files.
- `graft skeleton <file>` → every definition's signature + span, ~10× cheaper
  than reading the file; use it to skim an API surface.
- `graft callers <symbol>` gives precomputed, exact edges — who calls this.
  Add `--direction out` for what it calls, or `--depth N` to walk
  transitively for the full blast radius. For structural questions, skip
  ranking and use this directly.
- Or browse: `graft/INDEX.md` lists every node; follow the links.
- Monorepos and folders of multiple repos rank fairly across sub-projects —
  hits carry `[scope/]` labels naming which one they're from. Narrow with
  `graft ask "<task>" --in <scope>/` once you know where you're working.

If a returned span is truncated ("+N more lines"), open the file at that exact
range before finalizing. Only open source files when a node genuinely lacks a
needed detail, and then at the exact file:line the node points to — never
re-read whole files.

After big code changes, refresh the graph with `graft build` (deterministic,
no API key, $0).
<!-- graft:end -->
