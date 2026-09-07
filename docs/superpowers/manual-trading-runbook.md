# Manual trading runbook — Hyperliquid via Trench

Self-contained instructions for an agent trading this account **by hand**, with
peri stopped. Assumes no prior knowledge of the codebase.

**This moves real money.** Every command marked `--yes` places a live order on a
mainnet account. There is no paper mode here and no gate except you.

---

## 0. The account

| | |
|---|---|
| venue | Hyperliquid mainnet, via the Trench builder (3 bp on every order) |
| wallet | `HL_ACCOUNT_ADDRESS` in `.env` — signs with an **agent key** that cannot withdraw |
| balance | **$34.14** as of 2026-09-05 (check it, don't trust this line) |
| markets | native (`BTC ETH SOL HYPE XRP DOGE`) + builder dexes `xyz` (equities, commodities) and `io` |
| everything runs on | `remote-ubuntu` (SSH alias), repo at `~/botta` |

---

## 1. Two rules that are not negotiable

**Every position gets a stop, placed in the same action as the entry.** Not
after, not "when I get to it". Two unprotected positions on 2026-09-03/04 went
+$4.32 → −$9.92 and +$2.02 → −$13.06 because there was no mechanism to exit
between those two numbers. The one position that had a stop lost a *known*
$13.06 while BTC fell another 834 points past it.

**Size from the stop, not from the leverage.** Risk budget is **5% of equity**.
On $34.14 that is **$1.71**. Notional follows:

```
notional = risk_budget / stop_distance_as_fraction
$1.71 / 0.005  =  $342 of notional for a 0.5% stop
```

Leverage only decides how much margin that notional locks up. It does **not**
increase profit. A $1,680 position on a $47 account lost 31% of the account on
one correctly-placed stop.

---

## 2. Read the state before anything

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python - <<PY
import json, urllib.request, os
for line in open(".env"):
    line = line.strip()
    if line and not line.startswith("#") and "=" in line:
        k, v = line.split("=", 1); os.environ.setdefault(k.strip(), v.strip())
A = os.environ["HL_ACCOUNT_ADDRESS"]
def info(p):
    r = urllib.request.Request("https://api.hyperliquid.xyz/info",
        data=json.dumps(p).encode(), headers={"content-type": "application/json"})
    return json.loads(urllib.request.urlopen(r, timeout=20).read())
for dex in ("", "xyz", "io"):
    b = {"type": "clearinghouseState", "user": A}
    if dex: b["dex"] = dex
    for ap in info(b).get("assetPositions") or []:
        p = ap["position"]
        print("POS", p["coin"], p["szi"], "@", p["entryPx"],
              "uPnL", p["unrealizedPnl"], "liq", p.get("liquidationPx"))
    b = {"type": "frontendOpenOrders", "user": A}
    if dex: b["dex"] = dex
    for o in info(b):
        print("ORD", o["coin"], o.get("orderType"), "trig", o.get("triggerPx"),
              "reduceOnly", o.get("reduceOnly"))
sp = [x for x in info({"type": "spotClearinghouseState", "user": A})["balances"]
      if x["coin"] == "USDC"][0]
print("USDC", sp["total"], "hold", sp["hold"])
PY'
```

**Reading it correctly:**

- `hold` is margin locked in positions. **free = total − hold.**
- A reduce-only trigger **below** entry on a long (or **above** entry on a
  short) is a **stop**. The other side is a take-profit. A take-profit protects
  nothing on the downside — check the side, not just the presence of an order.
- `sz: "0.0"` with `isPositionTpsl: true` is a **position-level** trigger
  covering the whole position, not a zero-size order.

---

## 3. Place a trade

### Preferred: rest a maker limit at a level

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python rest_entry.py \
  ETH short --entry 2519 --stop 2540 --tp 2400 \
  --notional 340 --leverage 20 --expiry-mins 720 --why "reason"'
# inspect the print, then re-run with --yes
```

Entry, stop and take-profit go on in **one signed action**, so the brackets arm
the instant it fills. It refuses if the limit is on the wrong side of the mark,
if stop/tp are on the wrong side of entry, or if the stop sits inside the
liquidation band (and tells you the highest leverage that would work).

**Why resting beats market.** Same caller, same SOL trade, twice on 2026-08-31:

| | RR | net |
|---|---|---|
| chased to 102.58 (his level was 102.37) | 2.11 → **1.05** | +$0.67 |
| rested at his 103.12 | 1.50 held | **+$3.19** |

Nearly 5× the result on a smaller move, purely from the entry. Across the whole
period **every winner waited for a level and every large loser bought or sold at
market into a local extreme.**

The cost of resting is that it may not fill. That is the correct trade-off — an
unfilled order loses nothing.

### When you genuinely need immediate fill

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python manual_trade.py \
  BTC long --notional 340 --stop 80200 --tp 82500 --leverage 10 --why "reason" --yes'
```

Market order with brackets attached. Pays taker (7.5 bp vs 4.5 bp maker) plus
whatever the price has already moved.

---

## 4. Manage and close

**Replace protection** (new triggers placed *before* the old are cancelled, so
the position is never naked):

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python manage_trade.py \
  BTC --stop 80550 --tp 82500:0.00643 --yes'
```

`--tp PRICE:SIZE`, repeatable for tranches; sizes must sum to the venue position
size.

**Close at market and clean up:**

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python close_trade.py BTC --yes'
```

Closes against the **venue** position, so it works on positions peri never
recorded. Cancels leftover reduce-only triggers — a take-profit left behind
after a manual close sits on the book forever and will block re-entry.

---

## 5. Placing the stop

**Structural, not arbitrary.** Put it beyond the level that would prove the idea
wrong — under a shelf that has held, over a swing high — not at a round number
or a fixed percentage.

**Outside the noise band.** Compute ATR15m and require the stop to clear roughly
**1× ATR15m** minimum; 2× is safer. A stop at 0.80× ATR is a coin flip that
resolves against you. On 2026-09-04 a stop 0.80× ATR was moved to 2.6× ATR
below real support and that is the one that behaved correctly.

**Inside the liquidation band.** At isolated margin the liquidation distance is
approximately:

```
liq_distance = 1/leverage − 1/(2 × venue_max_leverage)
```

The stop must be **closer than that**, with margin — `rest_entry.py` enforces
`stop_distance × 1.3 < liq_distance` and refuses otherwise. At 25x on a 25x-max
market the band is 2.00%, so a 2.00% stop liquidates first. This is not
theoretical: a palladium trade at 20x with a 2.75% stop would have liquidated
before its stop ever fired.

---

## 6. Fees decide what is worth trading

Round trip ≈ **0.15% of notional** (HL taker 4.5 bp + Trench builder 3 bp per
side; maker is 1.5 + 3). Measured on `xyz` fills it came in lower, 3.3–3.9 bp.

- On a **0.5% scalp target** that is a third of the move. The 40x scalp on
  2026-09-03 made **+$1.66 gross → +$0.30 net**.
- On a **4% swing** it is negligible.
- A short-then-long flip on $1,680 of notional cost **$2.39 in fees alone**,
  before the market moved at all.

Do not scalp small moves on a small account. The arithmetic does not work.

---

## 7. Reading the market

**Smart-money divergence** — the profitable cohorts versus everyone else. This
is the one signal with a real edge:

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python -c "
from peri.trench import fetch_cohort_bias, fetch_many_asset_bias
for c in fetch_cohort_bias()[\"cohorts\"]:
    print(c[\"label\"], c[\"traders\"], c.get(\"long_pct\"))
for m, b in fetch_many_asset_bias([\"BTC\",\"ETH\",\"SOL\",\"HYPE\",\"XRP\",\"DOGE\"]).items():
    print(m, b[\"smart_long_pct\"], b[\"crowd_long_pct\"], b[\"divergence\"])"'
```

Negative divergence = smart money short while the crowd is long → short
candidate. Positive = the reverse. **±20pp or more is a signal; under ±10pp is
noise.** It tells you *who is exposed*, not *when it breaks* — it is a
positioning read, not a timing one.

**Range position.** Compute where price sits in its 24h range. Do not buy above
**0.80** or sell below **0.20** — that is chasing, and six such entries lost
12.8% in one day on 2026-08-28.

**Funding.** +11% APR means longs are paying to hold; crowded and expensive.
Near zero means the move is spot-driven and cleaner.

**The calendar.** High-impact prints (NFP, CPI, FOMC) move crypto 2–4% in
minutes. Check before opening anything you intend to hold through one:

```bash
ssh remote-ubuntu 'cd ~/botta && $HOME/.local/bin/uv run python -c "
import time
from peri.trench import fetch_economic_calendar
now = time.time()
for e in fetch_economic_calendar():
    if e[\"ts\"] > now:
        print(round((e[\"ts\"]-now)/3600, 1), \"h\", e[\"impact\"], e[\"title\"])"'
```

**Sessions.** Builder-dex markets keep their *underlying's* hours. `xyz` equities
follow their home exchange (KRX/TSE/HKEX for foreign names, US cash for US
ones); commodities and index futures follow CME Globex (Sun 18:00 → Fri 17:00
ET). Outside those hours the perp trades against a stale reference. `peri.risk`
has `cme_session_open()` and `foreign_session_open()` if you want the check.

---

## 8. Gotchas that have actually bitten

- **Use the full uv path.** Remote `python3` has no project deps, and
  `timeout N uv ...` fails with `No such file or directory` on a
  non-interactive shell. Always `$HOME/.local/bin/uv run python`.
- **`frontendOpenOrders` needs a `dex` param** per builder dex or you will not
  see `xyz`/`io` orders and will conclude an order vanished. `userFills` does
  **not** — it returns every dex regardless.
- **`clearinghouseState` and `frontendOpenOrders` are not atomic.** Seconds
  after a fill the order has left the book while the position has not appeared.
  Check `userFills` before concluding an order was cancelled.
- **Trigger-market take-profits slip.** One triggered at 103.15 and filled at
  102.99 — $0.42, or 39% of that trade's gross profit. A trigger-*limit* avoids
  it at the cost of sometimes not filling.
- **Never leave a manual close without cancelling brackets.** `close_trade.py`
  does it; ad-hoc scripts have not, and left orphan triggers that blocked
  re-entry.

---

## 9. Before you send: the checklist

1. **State read?** Positions, orders, free margin — not remembered, re-read.
2. **Is there a real edge?** Divergence ≥ ±20pp, or a structural level. "It's
   going up" is not an edge.
3. **Range position** ≤ 0.80 for a long, ≥ 0.20 for a short.
4. **Stop is structural** and ≥ 1× ATR15m.
5. **Stop clears the liquidation band** — let `rest_entry.py` check it.
6. **Risk ≤ 5% of equity.** On $34 that is $1.71.
7. **RR ≥ 2** after fees. Below that you need a >50% hit rate to break even.
8. **Event check** — is a high-impact print inside your holding period?
9. **Resting or market?** Default to resting.
10. **Dry run first** (omit `--yes`), read the numbers, then send.

---

## 10. If something goes wrong

**Unprotected position discovered** → `manage_trade.py MARKET --stop X --tp
PRICE:SIZE --yes` immediately. A stop above liquidation is always better than
none.

**Position approaching liquidation** → three options, in order of preference:
add margin (pushes liquidation away, raises total at risk), close part (banks
half the loss and halves the remaining size against the same margin, so
liquidation moves far away), or close all.

**Ledger and venue disagree** → the venue is the truth. `peri.db` is a record,
not an authority. Do not let a stale ledger row stop you acting on the real
position — but do fix the row, because peri will reconcile against it if it is
ever restarted.

**Ledger has stale open rows** (peri was stopped while trading happened) → see
`docs/superpowers/2026-09-05-handoff.md` §1.
