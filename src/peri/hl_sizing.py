import math
from decimal import ROUND_HALF_UP, Decimal


def sz_decimals(meta: dict, coin: str) -> int:
    for a in meta["universe"]:
        if a["name"] == coin:
            return int(a["szDecimals"])
    raise ValueError(f"unknown coin {coin}")


def notional_to_size(notional: float, mark: float, sz_decimals: int) -> float:
    """FLOOR to the venue lot. Rounding to nearest can round UP, and on a coarse
    lot that silently inflates the risk the guard approved (a $29.50 target on a
    szDecimals=0 market at $50 rounds to 1 lot = $50, +69%). Flooring can only
    ever risk less than approved; a lot that floors to zero is refused upstream."""
    step = 10 ** -sz_decimals
    lots = math.floor((notional / mark) / step + 1e-9)
    return round(lots * step, sz_decimals)


def format_price(px: float, sz_decimals: int) -> float:
    """Clamp a price to Hyperliquid's tick rules: integers always pass; otherwise
    max 5 significant figures AND max (6 - szDecimals) decimal places."""
    p = Decimal(str(px))
    if p == p.to_integral_value():
        return float(p)
    p = p.quantize(Decimal(1).scaleb(p.adjusted() - 4), rounding=ROUND_HALF_UP)
    max_dp = 6 - sz_decimals
    if -p.normalize().as_tuple().exponent > max_dp:
        p = p.quantize(Decimal(1).scaleb(-max_dp), rounding=ROUND_HALF_UP)
    return float(p)


def tpsl_limit_price(trigger_px: float, close_is_buy: bool, sz_decimals: int) -> float:
    """Slippage-guarded limit for the market execution of a triggered TP/SL:
    closes that sell may fill down to 0.92x trigger, buys up to 1.08x."""
    mult = 1.08 if close_is_buy else 0.92
    return format_price(trigger_px * mult, sz_decimals)
