"""Close ONE open position at market and clean up its orphaned triggers.

    uv run python close_trade.py BTC            # dry print
    uv run python close_trade.py BTC --yes      # send it

Closes against the VENUE position, not the ledger, so it works on positions
peri never recorded — opened from the Trench app, or while the daemon was
stopped. It then cancels the leftover reduce-only triggers (a take-profit left
behind after a manual close will sit on the book forever) and records the close
in peri.db when a matching ledger row exists.

That last part matters: on 2026-08-31 a throwaway close script skipped the
ledger write and left TWO open rows on xyz:CL. On the next start
`open_position_for()` would have matched the old close fill to the LIVE row and
marked the wrong position closed.
"""

import argparse
import json
import sys

from peri.app import http_post
from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.hl_client import build_clients
from peri.market import Market
from peri.state import State


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("market")
    ap.add_argument("--why", default="closed by operator")
    ap.add_argument("--yes", action="store_true")
    a = ap.parse_args()

    cfg = load_config("config.toml", ".env")
    if cfg.mode != "live":
        print("config mode is not live — this script is for the real account only")
        return 2
    market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
    exchange, info, acct = build_clients(cfg)
    adapter = HyperliquidAdapter(
        exchange, info, acct, market,
        builder=TRENCH_BUILDER if cfg.route_builder_fee else None,
        slippage=cfg.risk.slippage_pct / 100, dexes=cfg.universe.dexes)
    state = State("peri.db")

    snapshot = adapter.account_snapshot()
    venue = next((p for p in snapshot["positions"] if p["market"] == a.market), None)
    if venue is None:
        print(f"no venue position on {a.market}")
        return 2
    mark = market.ctxs()[a.market].mark
    size = float(venue["size"])
    entry = float(venue["entry_px"])
    gross = (mark - entry) * size * (1 if venue["side"] == "long" else -1)
    close_fee = mark * size * 0.00075
    ledger = state.open_position_for(a.market)
    entry_fee = state.entry_fee(ledger.id) if ledger else 0.0

    print(f"{a.market} {venue['side']} {size:g} @ entry {entry:g}   mark {mark:g}")
    print(f"  uPnL ${venue['upnl']:+.2f}   margin ${venue['margin']:.2f}")
    print(f"  gross ${gross:+.2f} - close fee ~${close_fee:.2f} - entry fee "
          f"${entry_fee:.2f}  =>  net ~${gross - close_fee - entry_fee:+.2f}")
    print(f"  ledger row: {'#%d' % ledger.id if ledger else 'NONE (venue-only position)'}")
    if not a.yes:
        print("dry print only — add --yes to send")
        return 0

    if ledger is not None:
        response = adapter.close_position_only(ledger)
    else:
        response = adapter.ex.market_close(a.market, sz=size, slippage=adapter.slippage,
                                          builder=adapter.builder)
    print("close:", json.dumps(response)[:260])

    close_px = mark
    if isinstance(response, dict):
        try:
            close_px = adapter._fill_px(response)
        except Exception:  # noqa: BLE001 — the mark is a fine fallback for the print
            pass
    adapter.cancel_brackets(a.market)
    print("orphan triggers cancelled")

    if ledger is not None:
        pnl = (close_px - entry) * size * (1 if venue["side"] == "long" else -1)
        pnl -= close_px * size * 0.00075 + entry_fee
        state.close_position(ledger.id, a.why, close_px, pnl)
        print(f"ledger position #{ledger.id} closed @ {close_px:g}, recorded ${pnl:+.2f}"
              f"  (reconcile will correct this from the real fill)")
    else:
        print("no ledger row — nothing to record")

    left = [p for p in adapter.account_snapshot()["positions"] if p["market"] == a.market]
    orders = [o for o in adapter.open_orders_all() if o.get("coin") == a.market]
    print(f"remaining on {a.market}: {len(left)} position(s), {len(orders)} order(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
