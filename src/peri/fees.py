"""The fee schedule — ONE definition, imported everywhere.

Verified against the live account 2026-08-29 (`userFees`): HL taker 4.5bp, HL
maker 1.5bp, and the Trench builder fee 3bp on every order regardless of side or
dex. The earlier 10.5bp model assumed a 7.5bp taker and overstated costs by 40%,
which made the projected-net-TP floor refuse trades that would have cleared it.

This module exists because the schedule had drifted into three separate copies:
`router` held the named constants, `risk.gate_open` inlined the same two numbers
as literals (with a comment explaining it could not import router without a
cycle), and `market.close_fee_rate()` returned a third hardcoded value. Nothing
bound them together, so the gate that decides whether a trade clears its costs
could disagree with the ledger that books them.

`fees` imports nothing from peri, so every module can depend on it.
"""

HL_TAKER_RATE = 0.00045          # HL taker, one side
HL_MAKER_RATE = 0.00015          # HL maker, one side
BUILDER_RATE = 0.0003            # Trench builder, on EVERY order

# One side of a round trip.
TAKER_FEE_RATE = HL_TAKER_RATE + BUILDER_RATE     # 7.5bp
MAKER_FEE_RATE = HL_MAKER_RATE + BUILDER_RATE     # 4.5bp

# The default when a side is not known to be a resting maker fill.
FEE_RATE = TAKER_FEE_RATE


def entry_rate(resting: bool) -> float:
    """What getting IN costs: a resting limit pays the maker side, a market
    order the taker side."""
    return MAKER_FEE_RATE if resting else TAKER_FEE_RATE


def round_trip_rate(resting_entry: bool) -> float:
    """Both sides. The exit is always a taker: a stop or take-profit is a
    triggered market order, and an analyst close crosses the spread."""
    return entry_rate(resting_entry) + TAKER_FEE_RATE
