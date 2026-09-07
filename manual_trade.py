"""Place ONE operator-authorized bracketed order through the live adapter and record
it in peri.db as source="manual" so the daemon adopts it on restart.

    uv run python manual_trade.py BTC short --notional 175 --stop 78950 --tp 75700 \
        --leverage 3 --why "post-Warsh risk-off, failed 2-month breakout" --yes

Refuses without --yes. Uses the same adapter path as the engine: leverage set,
market entry with the Trench builder fee, SL+TP brackets, cleanup-close if the
brackets fail. No gates are applied — the operator IS the gate here.
"""

import argparse
import sys

from peri.app import http_post
from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.hl_client import build_clients
from peri.market import Market
from peri.risk import Approved
from peri.state import State


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("market")
    ap.add_argument("side", choices=["long", "short"])
    ap.add_argument("--notional", type=float, required=True)
    ap.add_argument("--stop", type=float, required=True)
    ap.add_argument("--tp", type=float, required=True)
    ap.add_argument("--leverage", type=int, required=True)
    ap.add_argument("--why", default="operator manual trade")
    ap.add_argument("--yes", action="store_true")
    a = ap.parse_args()

    cfg = load_config("config.toml", ".env")
    if cfg.mode != "live":
        print("config mode is not live — this script is for the real account only")
        return 2
    market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
    ctx = market.ctxs().get(a.market)
    if ctx is None:
        print(f"unknown market {a.market}")
        return 2
    mark = ctx.mark
    long = a.side == "long"
    if long and not (a.stop < mark < a.tp) or (not long and not (a.tp < mark < a.stop)):
        print(f"stop/tp on the wrong side of mark {mark}")
        return 2
    stop_dist = abs(mark - a.stop) / mark
    tp_dist = abs(a.tp - mark) / mark
    fee_rt = 0.0015   # 7.5bp/side: HL taker 4.5 + trench builder 3
    risk = a.notional * (stop_dist + fee_rt)
    reward = a.notional * (tp_dist - fee_rt)
    print(f"{a.market} {a.side} mark={mark} notional=${a.notional:.0f} lev={a.leverage}x "
          f"stop={a.stop} ({stop_dist*100:.2f}%) tp={a.tp} ({tp_dist*100:.2f}%) "
          f"RR={tp_dist/stop_dist:.2f} | max loss≈${risk:.2f} | TP nets≈${reward:.2f}")
    if not a.yes:
        print("dry print only — add --yes to send")
        return 0

    exchange, info, acct = build_clients(cfg)
    adapter = HyperliquidAdapter(exchange, info, acct, market,
                                 builder=TRENCH_BUILDER if cfg.route_builder_fee else None,
                                 slippage=cfg.risk.slippage_pct / 100, dexes=cfg.universe.dexes)
    if cfg.hl_network == "mainnet":
        adapter.enable_dex_abstraction()
    approved = Approved(
        market=a.market, side=a.side, notional=a.notional, size_usd_risk=risk,
        leverage=float(a.leverage), margin=a.notional / a.leverage, margin_mode="isolated",
        stop_px=a.stop, tp_px=a.tp,
    )
    fill = adapter.open(approved, mark)
    print(f"FILLED entry={fill['entry_px']} size={fill['size']}")
    state = State("peri.db")
    pos = state.add_position(
        a.market, a.side, fill["entry_px"], fill["size"], a.notional, float(a.leverage),
        a.stop, a.tp, None, "manual", rationale=a.why,
        invalidation=f"bracket stop {a.stop}", margin_mode="isolated",
    )
    print(f"recorded position #{pos.id} source=manual")
    for o in adapter.bracket_orders(a.market):
        print("bracket:", o)
    return 0


if __name__ == "__main__":
    sys.exit(main())
