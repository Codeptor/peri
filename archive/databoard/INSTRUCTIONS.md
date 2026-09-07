# databoard — bootstrap instructions (read this first, then build nothing until §3 is answered)

> **Provenance:** handoff written 2026-08-25 by a Claude session running in
> `/home/esoteric/stratboard`, after Bhanu pitched the idea and the pitch-back was
> accepted in principle. This file is the complete context transfer — the next
> session should be able to work from here plus the referenced repos, with zero
> knowledge of the originating conversation.
>
> **Location decision (made by Bhanu):** databoard lives HERE, inside the botta
> monorepo, at `~/botta/databoard/`, alongside `kestreld/` and `dashboard/`.
> You are in the botta repo: read `../AGENTS.md` and
> `../docs/superpowers/STATUS.md` before anything else — STATUS.md is the
> living source of truth for the whole monorepo and its amendments are
> user-locked and normative.

## 1. What databoard is

A **fully automatic, LLM-driven, PAPER-ONLY trading arena for the four
Hyperliquid xyz-dex commodity perps** — `xyz:GOLD`, `xyz:SILVER`, `xyz:COPPER`,
`xyz:CL` (crude) — whose analysts are fed **desk-grade context from stratboard**
(Bhanu's commodities market-monitoring desk, `/home/esoteric/stratboard`,
production dashboard https://stratboard-teal.vercel.app/).

The one-line research question that makes this measurable instead of vibes:

> **Does desk-grade commodities context (cross-venue basis, funding/OI, macro
> calendar, news sentiment, IV/RV) make LLMs trade commodity perps better than
> the same LLMs without it?**

Deliverable = a measured answer with receipts, not profit promises. Kestrel
(the sibling system in this repo) already proved the infrastructure patterns;
databoard's genuinely new invention is the **context-bundle layer** and the
**multi-model arena with control arms**.

## 2. Decisions already made (locked unless Bhanu reopens them)

1. **Paper-only, forever-until-the-data-argues.** The kestreld PAPER-ONLY
   invariant (see `../AGENTS.md` §Security) extends verbatim to `databoard/`:
   no live order execution, no wallet signing, no real-funds paths, no
   `/exchange` calls, ever. Any change introducing them fails review.
   Stronger still: **databoard needs zero wallet material** — it only reads
   public HL info endpoints and stratboard data. No `HL_*` keys in its config.
2. **Four assets only** — the xyz commodity perps above. No majors, no
   screening the whole universe; that's Kestrel's job.
3. **Context-bundle architecture with receipts.** Every LLM decision stores
   the exact context bundle it saw (content-hashed). Auditability is
   non-negotiable — same ethos as stratboard's receipt-backed evidence and
   kestreld's decision rows.
4. **Arena design.** N models get **identical context and identical risk
   rails**, each with its own isolated paper book. The dashboard is the
   leaderboard.
5. **Control arms.** At minimum: (a) a **no-context ablation arm** — same
   model, HL-native data only, no stratboard bundle — and (b) a
   **buy-and-hold basket benchmark**. Optionally a random-entry control.
   Without these the research question is unanswerable.
6. **Read-only consumption of stratboard.** Stratboard's repo does not change
   for this project (beyond possibly minting one read-only service credential).
   databoard is a consumer, never a writer.
7. **Honest data.** Stratboard's "no fake data" convention applies: a missing
   or stale source appears in the context bundle as its status, never as an
   invented number. The LLM must see staleness flags.
8. **Dashboard stack** = Next.js + shadcn registries (Bhanu: "don't handroll
   things, get components from shadcn registries"), dark-first, prices in
   `font-mono tabular-nums`, local-only like the existing `dashboard/`
   (kestrel's is `127.0.0.1:3474`; pick a distinct port, suggest `3475`).
   Bhanu also likes IST shown alongside UTC timestamps (he had this added
   across stratboard).

## 3. OPEN decisions — ask Bhanu these FIRST, before scaffolding

1. **Daemon stack:** fresh **Python** service (recommendation from the pitch:
   the value is the context/LLM layer, not daemon perf; 4 assets × 15-min
   cadence is trivial load; botta already has a Python package + uv) **vs
   forking the kestreld Rust crate** (proven, 311 tests, but heavy to
   specialize). Bhanu never answered.
2. **Arena roster + budget:** which models, how many books, acceptable $/day.
   Rough estimate given at pitch: ~$5–20/day at a 15-min review cadence
   depending on models. (Anthropic default for new AI apps is the latest
   Claude; botta precedent also has a Lightning gateway key + an
   `ANALYST_API_KEY` for kestrel's muse — see `../.env`, never print it.)
3. **Cadence:** 15-min position reviews + event-triggered wakes (macro
   release, news spike) was the pitched default. Confirm or adjust.
4. **Stratboard access path** (see §8 — three options, pick after verifying
   reachability).
5. **Naming:** "databoard" was Bhanu's word. Keep it unless he renames.

## 4. The two parent systems — read before designing

### 4a. stratboard (`/home/esoteric/stratboard`) — the data source

Commodities desk: **gold, silver, copper, crude** across ten venues
(`comex nymex ice_brent london_spot lbma lme mcx hyperliquid shfe ine`).
Monorepo: Next.js 16 web app (Vercel) + FastAPI feed service on the VPS
(port 8000) + strategy services. Read its `AGENTS.md` for the full picture.
Key facts for databoard:

- **All four assets trade on Hyperliquid's `xyz` builder dex** and stratboard
  already captures HL **funding / open interest / volume / premium** per
  `VenueQuote` (contract: `apps/web/lib/contracts.ts`, mirrored by
  `services/feed/src/feed/models.py`).
- Statuses are data-driven: `live · stale · delayed · reference · down` — the
  bundle must forward these.
- The feed has a real access layer: `AccessManager` with Bearer-token auth +
  per-key/per-IP rate limiting (`services/feed/src/feed/access.py`, class at
  ~line 883, token parse ~line 914; storage in `access_store.py`; the web BFF
  exchanges sessions via `/api/access/*`). Service-credential minting exists
  for the web tier — the exact procedure for a third-party service consumer
  must be read from that module before wiring (§8, verify step).
- Relevant conventions that carry over: no mock data anywhere; timestamps
  UTC (+IST display); deploys of the feed only via
  `deploy/deploy-exact-sha.sh` (you will NOT be deploying stratboard — but
  never poke the VPS casually either).

### 4b. botta / Kestrel (`/home/esoteric/botta`) — the pattern source

Kestrel (`kestreld/`, Rust) is the existing autonomous paper trader for the
broad HL universe with an LLM "muse" analyst. **Do not modify it.** Inherit
its hard-won semantics (all verifiable in `../AGENTS.md`,
`../docs/superpowers/STATUS.md`, and `kestreld/src/`):

| Lesson | Value to copy |
|---|---|
| Fill simulation | l2Book **impact VWAP** with never-better-than-flat cap; flat 2bp/5bp fallback; **7.5bp fees each direction (~15bp round trip)** |
| Gate chain (frozen order) | data-age (pre) → entry gates → regime → churn; kill → max-concurrent → daily-cap → dup-market → cooldown → conviction ≥ floor → veto |
| Churn control | per-market daily cap; **asymmetric cooldown** (longer after a stop-loss close); morning entry budget |
| Analyst contract | "muse-only": structured JSON decision, 2 retries then `decision: None`, 60s timeout, **no fallback model**; refusal-kind forensics recorded on the decision row |
| Fee hurdle in prompt | tell the model the round trip costs ~15bp of notional and to only open when expected move ≥ 3× cost |
| Conviction evidence | kestrel's real tape: conviction 0.70–0.75 entries bled (16 closes, 38% win, −$62.72) while 0.75–0.80 made money (4 closes, 75%, +$41.89) → floor raised to 0.75 |
| Fee-drag evidence | net −71.30 on gross **+15.80** vs **fees 87.10** over the only obtainable 3.7-day tape — fees eat everything at high churn; databoard must trade LESS, not more |
| Ops scars | ABBA deadlock (never hold two locks across await); dotenvy: **process env WINS over `.env`**; `pkill -f` self-match trap (bracket trick); watchdog + truthful health; systemd user units supervise daemons |
| HL API limits | `candleSnapshot` caps ~5000 candles/call; **HL serves only ~3.6 days of 1m history** — a 7-day 1m backtest window is unobtainable, ever; some indexed docs show a non-existent `place_order(order_type='trigger_stop_loss')` SDK API — irrelevant here anyway (paper-only, info endpoints only) |
| Repo hygiene | `docs/superpowers/` spec → plan → STATUS.md living truth; gates green per commit; graft index (`graft ask "..."` before grepping) |

Also in this repo: `dashboard/` (kestrel's accepted v3 terminal, port 3474,
reference-locked design — good stylistic reference), `dash-plumbing/`,
`tg_news_pipe.py` (news sidecar), and the Python copy-trader in `src/botta/`
(unrelated; can trade live; don't touch).

## 5. Verified facts (probed 2026-08-25 from the stratboard session)

- **HL official info API returns the FULL xyz listing history free.**
  `POST https://api.hyperliquid.xyz/info` with
  `{"type":"candleSnapshot","req":{"coin":"xyz:GOLD","interval":"1d","startTime":0,"endTime":<now_ms>}}`
  → 239 daily candles spanning **2025-12-22 → present** in one call, fields
  `t/T/o/h/l/c/v/n` (native volume + trade count). xyz commodities listed
  2025-12-22; `xyz:` coin names map 1:1 (verified during kestrel dash v2).
- **Finer intervals paginate** (~5000 candles/call cap) and **1m retention is
  ~3.6 days** (measured twice in kestrel backtests).
- stratboard's own DB has HL ticks since 2026-06-22 and MCX ticks since
  2026-06-17 (~230k rows/asset) — deep intraday history for the *context*
  side beyond what HL serves.
- xyz perps are thinner than majors — impact/slippage worse than kestrel's
  BTC/ETH experience; model fills conservatively.

## 6. Proposed architecture (pitched and accepted in principle)

```
stratboard feed (VPS :8000, read-only Bearer)   HL info API (public)
        │                                            │
        ▼                                            ▼
  context_builder ──── per-asset bundle (~2–4k tok, hashed, stored)
        │
        ▼
  arena daemon ── N model analysts × isolated paper books
        │            + ablation arm (no-context) + buy-hold benchmark
        │            gates: data-age → entry → churn → conviction → kill
        │            fills: impact-VWAP + 15bp RT fees (kestrel semantics)
        ▼
  SQLite ledger (books, decisions, bundles, receipts)
        │
        ▼
  Next.js dashboard (127.0.0.1:3475) — leaderboard, equity curves,
  positions, decision log w/ full context receipt per trade, gate funnel
```

### 6a. Context bundle (the new invention — spec this first)

Per decision cycle, per asset, assemble a timestamped brief with **provenance
and `asOf` on every section**, content-hash it, store it, pass it to every
arena model identically:

1. **HL state** — mark, funding, OI, premium, 24h volume (stratboard
   `VenueQuote` for `hyperliquid`), 1m/1h candle stats from HL API direct.
2. **Cross-venue** — consolidated quote per asset; basis vs COMEX/MCX/LBMA
   (`/api/basis`, `/api/reference-spreads`); USDINR (`/api/fx`) for the MCX leg.
3. **Macro** — today's + imminent calendar events, next FOMC/EIA/COT, key FRED
   deltas (`/api/macro`).
4. **News** — top-N last-24h headlines + sentiment for the asset (`/api/news`).
5. **Vol** — IV level/percentile, IV−RV premium (`/api/vol`).
6. **Yesterday** — the per-asset daily digest summary (`/api/daily`).
7. **Health** — source statuses (`/api/sources`); anything stale/down is
   stated as such. No fake data, ever.

Token budget ≈ 2–4k/asset. The ablation arm receives section 1 only.

### 6b. Risk rails (inherit, don't reinvent)

Kill switch on max daily drawdown; daily + per-market entry caps; asymmetric
cooldowns; conviction floor (start 0.75 like kestrel's evidence says);
data-age gate = `max(bundle_age, feed_status)` — stale stratboard ⇒ refuse
entries, reviews still allowed; every refusal recorded with kind. Bankroll
per book fixed and equal (kestrel uses 1000).

### 6c. Storage

SQLite (WAL) in `databoard/` — books, positions, fills, decisions (with
bundle hash FK), bundles, equity marks, refusals. Runtime artifacts
gitignored like the rest of botta.

## 7. Stratboard feed endpoint reference (the menu)

| endpoint | returns |
|---|---|
| `GET /api/snapshot` | full `MarketSnapshot` (all assets/venues + statuses) |
| `GET /api/candles?asset=&venue=&interval=1m\|5m\|1h` | stratboard-captured candles (has `before=&limit=` cursor pagination) |
| `GET /api/sources` | `SourceStatus[]` |
| `GET /api/macro` | FRED indicators + economic calendar |
| `GET /api/news` | multi-source headlines + sentiment |
| `GET /api/vol` | implied-vol indices + IV/RV premium |
| `GET /api/daily?date=YYYY-MM-DD` | per-asset day digest |
| `GET /api/fx` · `/api/basis` · `/api/reference-spreads` | normalized cross-venue comparison evidence |
| `GET /api/depth` · `/api/options` | market depth · option chain |
| `WS /ws/quotes` | live `ServerMessage` stream (snapshot + deltas) — likely unnecessary for a 15-min cadence; polling is fine |

Wire shapes: `stratboard/apps/web/lib/contracts.ts` (TS) ↔
`stratboard/services/feed/src/feed/models.py` (Python source of truth).

## 8. Stratboard access — verify before wiring (nothing here is built yet)

Three candidate paths; **the next session must verify reachability and the
exact token-minting procedure by reading
`stratboard/services/feed/src/feed/access.py` + `access_store.py`** (do not
guess; do not touch the VPS beyond read-only checks):

1. **Direct VPS `:8000` + service Bearer token** — cleanest if the port is
   reachable from this machine and a service credential can be minted in the
   feed's access store. (The feed binds `0.0.0.0:8000`; firewall state
   unverified from here.)
2. **SSH tunnel to VPS localhost:8000** — works regardless of firewall;
   needs a supervised tunnel unit.
3. **Through the public web BFF with a session** — most moving parts;
   last resort.

Rate limits exist per key/IP in `AccessLimiter` — a 15-min × 4-asset cadence
is trivially within any sane limit. If a credential is minted, it goes in
botta's `.env` (0600, gitignored) as e.g. `STRATBOARD_TOKEN`; remember
kestrel's scar: **process env beats `.env`** under dotenvy — Python should
load `.env` explicitly and deterministically.

## 9. Honesty rails and expectations (repeat these to Bhanu when relevant)

- Kestrel's own tape shows fee drag exceeding gross at high churn; stratboard's
  S2/S3 strategy research (see `stratboard/AGENTS.md` "honest-findings
  architecture") measured **no tradable directional edge** and a traded policy
  that **loses to buy-and-hold under costs**. An LLM reading dashboards does
  not automatically clear that bar. The Alpha-Arena-style public experiments
  (late 2025) mostly lost money live.
- Therefore: success = the ablation comparison answered with receipts
  (context vs no-context vs buy-hold, same rails, adequate sample), not P&L.
- Sample honesty: at ~a handful of closes/week/book, differences under ~$100
  are noise (kestrel measured ~$90 sd per 77-close run). Don't declare winners
  early; surface confidence intervals on the leaderboard.

## 10. Security invariants (copy into the eventual AGENTS.md section)

- PAPER-ONLY invariant, extended: no `/exchange` calls, no signing, no wallet
  keys, no transfer paths anywhere in `databoard/`. Info endpoints only.
- No secrets in code, args, logs, or chat; `.env` 0600; sessions gitignored.
- Dashboard binds localhost only, no auth, never exposed.
- Read-only posture toward stratboard prod: GETs only, respect rate limits,
  never write endpoints, never casual VPS access.

## 11. Working conventions for this subsystem

- Follow botta's flow: **spec → plan → build**, documents in
  `../docs/superpowers/` (`specs/2026-XX-XX-databoard-design.md`, then a plan,
  then STATUS.md rows as work lands). Read the existing kestrel spec/plan for
  the house style. Task-by-task commits, gates green each commit
  (whatever the chosen stack's equivalents of `cargo test`/`pytest -q`/
  `pnpm build` are). Update `../AGENTS.md` repo-layout section when the
  subsystem exists. Refresh graft (`graft build`) after big changes.
- One dashboard = one port, one supervised process eventually (systemd user
  units like kestreld; see `kestreld/deploy/README.md`).
- Don't bloat: smallest end-to-end working layer first (one model, one asset,
  one bundle, one paper book, one page), then widen the arena.

## 12. First-session checklist

1. Read `../AGENTS.md`, `../docs/superpowers/STATUS.md`, this file.
2. Ask Bhanu the §3 open decisions (stack, roster+budget, cadence, access
   path preference, name).
3. Verify stratboard access (§8) read-only; confirm one live
   `GET /api/snapshot` with credentials before designing around it.
4. Probe HL candles for all four xyz coins (1d + 1h) to confirm current
   listing depth and pagination behavior.
5. Write the design spec in `../docs/superpowers/specs/` (context-bundle
   schema, arena/book model, gates, fills, storage DDL, dashboard pages,
   benchmark definitions, evaluation criteria + sample-size honesty).
6. Get the spec approved, write the plan, then build task-by-task with green
   gates — smallest end-to-end slice first.
