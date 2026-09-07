# peri

An autonomous LLM perpetual-futures trader for [Hyperliquid](https://hyperliquid.xyz),
routed through the Trench builder. One Python daemon, one language model making
every trading decision, and code that owns risk so the model cannot talk its way
past a stop.

It trades crypto majors on native Hyperliquid plus every configured builder dex:
equities, commodities and index futures on `xyz`, private-company synthetics on
`io`. Around 355 markets through one wallet, routed automatically by coin prefix.

> **This moves real money.** `mode = "live"` places real orders on a real
> account. The repository ships `mode = "dry"`, which paper-trades against live
> mainnet data. Run it that way until you have watched it decide for a few days
> and disagreed with it out loud at least once.

## How it works

Each cycle the analyst model gets account state, its own open positions with the
rationale it wrote when it opened them, screened candidates with dex-aware
features, raw Telegram messages from a group you choose, RSS headlines, and its
own recent closed trades. It returns strict JSON: open, close, adjust a stop, or
write itself a lesson.

Then code decides whether that is allowed. The gate order is frozen and every
gate refuses rather than adapts:

```
kill switch -> operator pause -> day-loss halt -> max concurrent -> daily cap
-> duplicate market -> venue order -> cooldown -> conviction floor
-> stale mirror -> market session -> range edge -> stop sanity -> RR floor
-> leverage -> isolated liquidation band -> sizing -> min notional -> margin
-> projected net take-profit
```

Position size comes from stop distance, not leverage: `notional = equity ×
risk% / stop_distance`, priced at the entry rather than the mark, so a resting
order is judged where it sits. Entries can rest as maker limits with stop and
take-profit attached in one signed action, so protection arms the instant the
order fills. Once a position is open the engine manages it without the model:
breakeven at +1R, a trailing stop behind the high-water mark, a time stop at
three hours below +0.5R.

The design document is `docs/superpowers/specs/2026-08-27-peri-design.md`.
`AGENTS.md` is the working reference for anyone, human or model, changing code.

## What it learned

`docs/memory/` holds the bot's accumulated memory, exported from a live ledger:
lessons the analyst wrote for itself, and a measured record computed from every
closed trade. Two findings worth reading before you change anything:

- Resting entries beat market orders. The same call, taken twice on the same
  day, returned +$0.67 chased at market versus +$3.19 rested at the level.
- Position sizing from leverage instead of stop distance is what actually blew
  up trades. A correctly placed stop on an oversized position still costs 31%
  of the account.

## Setup

You need Python 3.12 or newer, [uv](https://docs.astral.sh/uv/), a funded
Hyperliquid account, and an OpenAI-compatible chat endpoint.

```bash
git clone <this repo> peri && cd peri
uv sync --group dev
cp .env.example .env && chmod 600 .env   # then fill it in
uv run --group dev pytest -q             # 431 tests, no network
```

**Hyperliquid.** Mint an agent wallet that can sign orders but cannot withdraw:

```bash
uv run python make_agent.py              # prints the key -> HL_AGENT_KEY
uv run python approve_builder.py         # one-time, needs HL_MASTER_KEY
```

The builder approval is what lets orders carry the Trench builder fee. It has to
be signed by the main wallet once per network, because an agent key cannot sign
it. Remove `HL_MASTER_KEY` from `.env` afterwards.

**Telegram.** Get an API id and hash from https://my.telegram.org, then log in
once interactively. This writes `peri.session`, which is a real credential:

```bash
uv run python fetch_messages.py          # interactive login
uv run python wire_telegram.py           # finds your chat id for TG_NOTIFY_CHAT_ID
```

Telegram is optional. Without it the analyst still trades on price, news and
Trench positioning, it just loses the group as an input.

**Configuration.** `config.toml` carries tuning and nothing else. Identity lives
in `.env`, so the committed config never says whose account it is. Read the
comments in `config.toml` before changing a risk number: most of them record
what went wrong when that number was different.

## Running

```bash
uv run peri --once      # one real decision cycle, then exit
uv run peri             # the daemon: Telegram feed, price wakes, decision loop
```

The dashboard is a separate app in `periboard/`, served on
`127.0.0.1:3475` against the daemon's API. `--once` is the honest smoke test:
it builds the whole prompt, calls the model, runs every gate, and either places
a paper fill or tells you exactly which gate refused.

Operator controls, from a phone, authorised to one Telegram user:
`/status /book /why /memory /wake /pause /resume /close`. There is deliberately
no command that opens a trade. Everything you can do from chat either reduces
risk or asks the analyst to think again.

## Trading by hand

The daemon can be stopped and the account driven manually with the same rails
and the same executor:

```bash
uv run python rest_entry.py ETH short --entry 2519 --stop 2540 --tp 2400 \
    --notional 340 --leverage 20            # dry print; add --yes to send
uv run python manage_trade.py BTC --stop 80550 --tp 82500:0.00643 --yes
uv run python close_trade.py BTC --yes
```

`docs/superpowers/manual-trading-runbook.md` is the full procedure, written to
be read cold: how to read account state, how to size from a stop, where to put
one, what fees do to a small account, and the failures that have actually
happened.

## Contributing

The gates that must be green before every commit:

```bash
uv run --group dev pytest -q             # all tests
uv run --group dev ruff check src tests  # line length 110
```

Tests never hit the network. Trench and Hyperliquid fetchers are injected on the
engine so the suite stays offline, and fixtures are extended from real sampled
data rather than invented formats.

Some conventions that are not negotiable, because each one is a scar:

- Risk rails are load-bearing. Do not weaken a gate, a cooldown, the conviction
  floor or the reward-risk floor to make something pass. Refuse instead.
- Prices go through `hl_sizing.format_price`. Never round a price straight into
  an order; Hyperliquid rejects it.
- Every open carries its stop and take-profit in the same signed action.
- The venue is the source of truth, the ledger is a record. When they disagree,
  fix the ledger.
- Restarts use exact-PID signals. A pattern-wide `pkill -f` matches the shell
  running it.

`AGENTS.md` carries the architecture, the invariants and the repository layout
in more detail. This repository is also indexed for
[graft](https://github.com/nanonets/graft), which answers "where does X live"
from a prebuilt graph rather than a grep:

```bash
npm i -g @nanonets/graft && graft build   # optional, regenerates graft/
graft ask "how does the risk gate order work" --source
```

## Layout

```
src/peri/          the daemon
  engine.py        the loop: reconcile, decide, gate, execute, record
  analyst.py       prompt construction and the model call
  risk.py          the frozen gates and stop-distance sizing
  hl_adapter.py    live execution, brackets, builder fee, dex routing
  state.py         SQLite ledger: positions, decisions, refusals, lessons
  market.py        dex-aware market data and candidate screening
  feed.py          Telegram ingest; vision.py reads posted charts
  trench.py        cohort positioning and the economic calendar
  control.py       Telegram slash commands
docs/superpowers/  specs, plans, the runbook, STATUS.md
docs/memory/       what the bot learned, exported from a live ledger
tests/             pytest suite, offline
archive/           retired systems kept for reference. Do not extend.
```

## Security

Secrets live in `.env` at mode 600 and nowhere else. The agent wallet signs
trades and cannot withdraw, so `HL_ACCOUNT_ADDRESS` must be the main wallet
rather than the agent's. `peri.session` is a Telegram login: treat it like a
password and never commit it. Mutating API endpoints refuse a request with no
provenance, because a bare `curl -X POST /api/pause` once paused a live trader.

Runtime artifacts stay out of git: the ledger, sessions, logs and downloaded
media are all ignored.
