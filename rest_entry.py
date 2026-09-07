"""Park ONE maker limit at a chosen level with SL+TP attached in a single signed
action, so the brackets arm the instant it fills.

    uv run python rest_entry.py ETH short --entry 2519 --stop 2540 --tp 2400 \
        --notional 1400 --leverage 25 --expiry-mins 720 --why "..." --yes

Why this exists rather than manual_trade.py: chasing is what loses. A market
order pays the taker rate AND whatever the price has already moved, and on a
tight stop that is most of the edge. Measured 2026-08-31 on the same caller's
same SOL trade, twice:

    chased to 102.58 (his level 102.37)  ->  RR 2.11 collapsed to 1.05, +$0.67
    rested at his 103.12                 ->  RR 1.50 held,               +$3.19

Refuses, loudly, when:
  - the limit is on the wrong side of the mark (it would fill as a taker)
  - stop/tp are on the wrong side of entry
  - the stop sits inside the isolated liquidation band, i.e. the venue would
    liquidate before the stop ever triggered

No gates beyond those. The operator IS the gate here — this places real orders
on the live account.
"""

import argparse
import json
import sys
import time

from peri.app import http_post
from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.hl_client import build_clients
from peri.hl_sizing import format_price, notional_to_size
from peri.market import Market
from peri.risk import LIQ_SAFETY, Approved, isolated_liq_distance
from peri.state import State


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("market")
    ap.add_argument("side", choices=["long", "short"])
    ap.add_argument("--entry", type=float, required=True, help="the resting limit price")
    ap.add_argument("--stop", type=float, required=True)
    ap.add_argument("--tp", type=float, required=True)
    ap.add_argument("--notional", type=float, required=True)
    ap.add_argument("--leverage", type=int, required=True)
    ap.add_argument("--expiry-mins", type=int, default=720,
                    help="ledger expiry; peri cancels it after this if running")
    ap.add_argument("--why", default="operator resting entry")
    ap.add_argument("--yes", action="store_true")
    a = ap.parse_args()

    cfg = load_config("config.toml", ".env")
    if cfg.mode != "live":
        print("config mode is not live — this script is for the real account only")
        return 2
    market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
    if not market.known(a.market):
        print(f"unknown market {a.market}")
        return 2
    mi = market.info(a.market)
    mark = market.ctxs()[a.market].mark
    long = a.side == "long"

    if long and not (a.stop < a.entry < a.tp):
        print(f"long needs stop < entry < tp, got {a.stop}/{a.entry}/{a.tp}")
        return 2
    if not long and not (a.tp < a.entry < a.stop):
        print(f"short needs tp < entry < stop, got {a.tp}/{a.entry}/{a.stop}")
        return 2
    if long and a.entry >= mark:
        print(f"a long limit at {a.entry} is at/above mark {mark} — it fills as a taker")
        return 2
    if not long and a.entry <= mark:
        print(f"a short limit at {a.entry} is at/below mark {mark} — it fills as a taker")
        return 2

    entry = format_price(a.entry, mi.sz_decimals)
    stop = format_price(a.stop, mi.sz_decimals)
    tp = format_price(a.tp, mi.sz_decimals)
    size = notional_to_size(a.notional, entry, mi.sz_decimals)
    if size <= 0:
        print(f"one venue lot of {a.market} costs more than ${a.notional:.2f}")
        return 2
    notional = entry * size
    risk_px = abs(entry - stop)
    dist = risk_px / entry
    liq = isolated_liq_distance(float(a.leverage), mi.max_leverage)
    liq_px = entry * (1 - liq) if long else entry * (1 + liq)

    print(f"{a.market} {a.side.upper()} resting @ {entry} "
          f"(mark {mark}, {(entry / mark - 1) * 100:+.2f}%)")
    print(f"  size {size:g}  notional ${notional:.2f}  {a.leverage}x isolated  "
          f"margin ${notional / a.leverage:.2f}")
    print(f"  stop {stop} ({dist * 100:.2f}%)   tp {tp} "
          f"({abs(tp - entry) / entry * 100:.2f}%)   RR {abs(tp - entry) / risk_px:.2f}")
    print(f"  risk ${risk_px * size:.2f}   reward ${abs(tp - entry) * size:.2f}   "
          f"round-trip fees ~${notional * 0.0012:.2f}")
    print(f"  liquidation ~{liq_px:.6g} ({liq * 100:.2f}% away) — stop is "
          f"{'INSIDE it, safe' if dist * LIQ_SAFETY < liq else 'BEYOND it'}")
    if dist * LIQ_SAFETY >= liq:
        safe = max((lev for lev in range(2, int(mi.max_leverage) + 1)
                    if dist * LIQ_SAFETY < isolated_liq_distance(float(lev),
                                                                 mi.max_leverage)),
                   default=0)
        print(f"REFUSING: the venue would liquidate before this stop fires. "
              f"Use {safe}x or lower for a {dist * 100:.2f}% stop.")
        return 2
    if not a.yes:
        print("dry print only — add --yes to send")
        return 0

    exchange, info, acct = build_clients(cfg)
    adapter = HyperliquidAdapter(
        exchange, info, acct, market,
        builder=TRENCH_BUILDER if cfg.route_builder_fee else None,
        slippage=cfg.risk.slippage_pct / 100, dexes=cfg.universe.dexes)
    if cfg.hl_network == "mainnet":
        adapter.enable_dex_abstraction()
    approved = Approved(
        market=a.market, side=a.side, notional=notional,
        size_usd_risk=risk_px * size, leverage=float(a.leverage),
        margin=notional / a.leverage, margin_mode="isolated",
        stop_px=stop, tp_px=tp, entry_px=entry, resting=True, size=size)
    placed = adapter.place_resting_entry(approved, size)
    print("placed:", json.dumps(placed)[:220])

    state = State("peri.db")
    entry_id = state.add_pending_entry(
        market=a.market, side=a.side, entry_px=entry, size=size, notional=notional,
        leverage=float(a.leverage), margin_mode="isolated", stop_px=stop, tp_px=tp,
        conviction=None, rationale=a.why, invalidation=f"stop {stop}",
        oid=placed.get("oid"), decision_id=None,
        expires_ts=time.time() + a.expiry_mins * 60)
    print(f"ledger pending_entry #{entry_id}, expires in {a.expiry_mins}m")
    for o in adapter.open_orders_all():
        if o.get("coin") == a.market:
            print(f"  live: {o.get('orderType'):<20} sz={o['sz']} px={o['limitPx']} "
                  f"trig={o.get('triggerPx')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
