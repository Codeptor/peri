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

Lighter charges no maker or taker fee at the standard tier (verified against
mainnet orderBookDetails 2026-09-07: taker_fee and maker_fee both "0.0000" on
every market), so its schedule is ZERO. Anything that prices a trade takes a
schedule; the module-level functions below are the HL schedule, kept so the
default path reads exactly as before.

`fees` imports nothing from peri, so every module can depend on it.
"""

from dataclasses import dataclass

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


@dataclass(frozen=True)
class FeeSchedule:
    """Both sides of what one venue charges. The guard and the engine take one
    of these instead of reading the module constants, so a zero-fee venue
    prices its trades at zero without a special case at every call site."""
    taker_rate: float
    maker_rate: float

    def entry_rate(self, resting: bool) -> float:
        return self.maker_rate if resting else self.taker_rate

    def round_trip_rate(self, resting_entry: bool) -> float:
        return self.entry_rate(resting_entry) + self.taker_rate


HL_SCHEDULE = FeeSchedule(taker_rate=TAKER_FEE_RATE, maker_rate=MAKER_FEE_RATE)
ZERO = FeeSchedule(taker_rate=0.0, maker_rate=0.0)   # Lighter standard tier
