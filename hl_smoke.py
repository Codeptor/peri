"""Hyperliquid TESTNET smoke: open a tiny SOL long through peri's live adapter
(brackets included), show it, close it. The real verification of signing and
response shapes — run before any mainnet flip.

Run:  uv run python hl_smoke.py
"""

from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.hl_client import build_clients
from peri.market import Market, http_post
from peri.risk import Approved


def main():
    cfg = load_config()
    if cfg.hl_network != "testnet":
        raise SystemExit('refusing to smoke on non-testnet — set hl_network = "testnet"')

    ex, info, acct = build_clients(cfg)
    builder = TRENCH_BUILDER if cfg.route_builder_fee else None
    if builder:
        approved = info.post("/info", {"type": "maxBuilderFee", "user": acct.lower(),
                                       "builder": builder["b"]})
        print(f"trench builder fee approved: {approved} tenths-bp (need ≥{builder['f']})")
        if not (isinstance(approved, int) and approved >= builder["f"]):
            raise SystemExit("builder fee not approved — run approve_builder.py first")

    market = Market(http_post(cfg.hl_network), cfg.universe.dex)
    adapter = HyperliquidAdapter(ex, info, acct, market, builder=builder,
                                 slippage=cfg.risk.slippage_pct / 100,
                                 dex=cfg.universe.dex)

    mark = market.mark("SOL")
    ap = Approved(market="SOL", side="long", notional=15.0, size_usd_risk=0.3,
                  leverage=10.0, margin=1.5, margin_mode="isolated",
                  stop_px=round(mark * 0.98, 2),
                  tp_px=round(mark * 1.02, 2))
    print(f"SOL mark={mark} · opening ${ap.notional} at {ap.leverage:g}x "
          f"· SL {ap.stop_px} TP {ap.tp_px}")
    fill = adapter.open(ap, mark)
    print("OPEN ->", fill)

    input("\nCheck testnet UI (position + both brackets). Enter to close...")
    from peri.state import Position
    pos = Position(0, "SOL", "long", fill["entry_px"], fill["size"], ap.notional,
                   ap.leverage, ap.margin_mode, ap.stop_px, ap.tp_px, None, "smoke", None, None,
                   "open", 0.0)
    print("CLOSE ->", adapter.close(pos, mark))


if __name__ == "__main__":
    main()
