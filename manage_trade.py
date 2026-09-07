"""Replace the protection on an open position: one stop for the full size plus one
or more take-profit tranches. New triggers are placed BEFORE the old ones are
cancelled (same discipline as HyperliquidAdapter.adjust_stop), so the position
is never unprotected.

    uv run python manage_trade.py BTC --stop 77720 --tp 75700:0.0026 --tp 73000:0.0025 --yes

Sizes of the --tp tranches must sum to the venue position size. Refuses without
--yes. Records the new stop / first TP on the matching open peri.db position.
"""

import argparse
import sys

from peri.app import http_post
from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.hl_client import build_clients
from peri.hl_sizing import format_price
from peri.market import Market
from peri.state import State


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("market")
    ap.add_argument("--stop", type=float, required=True)
    ap.add_argument("--tp", action="append", required=True, help="price:size, repeatable")
    ap.add_argument("--yes", action="store_true")
    a = ap.parse_args()

    cfg = load_config("config.toml", ".env")
    if cfg.mode != "live":
        print("config mode is not live — this script is for the real account only")
        return 2
    market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
    exchange, info, acct = build_clients(cfg)
    adapter = HyperliquidAdapter(exchange, info, acct, market,
                                 builder=TRENCH_BUILDER if cfg.route_builder_fee else None,
                                 slippage=cfg.risk.slippage_pct / 100, dexes=cfg.universe.dexes)
    mi = market.info(a.market)
    szd = mi.sz_decimals

    snap = adapter.account_snapshot()
    venue = next((p for p in snap.get("positions", []) if p.get("market") == a.market), None)
    if venue is None:
        print(f"no open venue position in {a.market}")
        return 2
    size, side = abs(float(venue["size"])), venue["side"]
    mark = market.ctxs()[a.market].mark
    tps = []
    for item in a.tp:
        px, sz = item.split(":")
        tps.append((float(px), round(float(sz), szd)))
    if abs(sum(sz for _, sz in tps) - size) > 10 ** -szd:
        print(f"tp sizes {[sz for _, sz in tps]} do not sum to position size {size}")
        return 2
    long = side == "long"
    bad = (a.stop >= mark or any(px <= mark for px, _ in tps)) if long else (
        a.stop <= mark or any(px >= mark for px, _ in tps))
    if bad:
        print(f"stop/tp on the wrong side of mark {mark} for a {side}")
        return 2
    old = adapter.bracket_orders(a.market)
    print(f"{a.market} {side} size {size} mark {mark} | old triggers: "
          f"{[(o.get('orderType'), o.get('triggerPx')) for o in old]}")
    print(f"new: stop {format_price(a.stop, szd)} x{size} | "
          + " | ".join(f"tp {format_price(px, szd)} x{sz}" for px, sz in tps))
    if not a.yes:
        print("dry print only — add --yes to send")
        return 0

    close_is_buy = not long
    placed = []
    try:
        resp = adapter._trigger(a.market, close_is_buy, size, a.stop, "sl", szd)
        placed.append(adapter._resting_oid(resp))
        for px, sz in tps:
            resp = adapter._trigger(a.market, close_is_buy, sz, px, "tp", szd)
            placed.append(adapter._resting_oid(resp))
    except Exception:
        adapter.cancel_orders(a.market, placed)
        raise
    adapter.cancel_orders(a.market, [o["oid"] for o in old])
    print("placed oids:", placed, "| cancelled:", [o["oid"] for o in old])
    for o in adapter.bracket_orders(a.market):
        print("  live:", o.get("orderType"), "trigger", o.get("triggerPx"), "sz", o.get("sz"))

    state = State("peri.db")
    pos = state.open_position_for(a.market)
    if pos is not None:
        state.update_brackets(pos.id, a.stop, tps[0][0])
        print(f"peri.db position #{pos.id}: stop {a.stop} tp {tps[0][0]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
