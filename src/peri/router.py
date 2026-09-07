"""Adapter protocol + the dry-run adapter (paper fills, engine-simulated
brackets). Live is hl_adapter.HyperliquidAdapter."""

import time
from typing import Optional, Protocol

from peri.risk import Approved
from peri.state import Position, State

# The schedule lives in peri.fees — one definition, imported everywhere. These
# re-exports keep every existing `from peri.router import FEE_RATE` working.
from peri.fees import FEE_RATE, MAKER_FEE_RATE, TAKER_FEE_RATE  # noqa: F401
# Paper fills cross the spread: 2bp adverse to the order direction.
PAPER_SLIP = 0.0002


class Adapter(Protocol):
    def account_snapshot(self, marks: Optional[dict[str, float]] = None) -> dict: ...
    def equity(self, marks: dict[str, float]) -> float: ...
    def open(self, ap: Approved, mark: float) -> dict: ...
    def open_entry(self, ap: Approved, mark: float) -> dict: ...
    def place_resting_entry(self, ap: Approved, size: float) -> dict: ...
    def place_brackets(self, market: str, side: str, size: float,
                       stop_px: float, tp_px: float) -> list[dict]: ...
    def close(self, pos: Position, mark: float) -> dict: ...
    def close_position_only(self, pos: Position) -> dict: ...
    def adjust_stop(self, pos: Position, stop_px: float, tp_px: float,
                    scale_out=None) -> dict: ...
    def bracket_orders(self, market: str) -> list[dict]: ...
    def cancel_orders(self, market: str, order_ids: list[int]) -> None: ...
    def cancel_brackets(self, market: str) -> None: ...
    def fills(self) -> list[dict]: ...


class DryRunAdapter:
    """Paper execution: adverse-slip fills at mark, fees accounted, no venue.
    Brackets are enforced by the engine each cycle (mark vs stop/tp)."""

    def __init__(self, state: State, bankroll: float):
        self.state = state
        self.bankroll = bankroll
        self.fees_paid = 0.0
        self._resting_oid = 900000
        self._resting: list[dict] = []

    def equity(self, marks: dict[str, float]) -> float:
        unrealized = 0.0
        for p in self.state.open_positions():
            mark = marks.get(p.market)
            if mark is None:
                continue
            d = mark - p.entry_px if p.side == "long" else p.entry_px - mark
            unrealized += d * p.size
        return self.bankroll + self.state.realized_total() + unrealized

    def account_snapshot(self, marks: Optional[dict[str, float]] = None) -> dict:
        marks = marks or {}
        equity = self.equity(marks)
        positions = []
        total_margin = 0.0
        for position in self.state.open_positions():
            mark = marks.get(position.market, position.entry_px)
            direction = (mark - position.entry_px if position.side == "long"
                         else position.entry_px - mark)
            margin = position.notional / position.leverage
            total_margin += margin
            positions.append({
                "market": position.market, "side": position.side,
                "size": position.size, "entry_px": position.entry_px,
                "leverage": position.leverage, "margin_mode": position.margin_mode,
                "margin": margin,
                "position_value": mark * position.size,
                "upnl": direction * position.size,
                "liquidation_px": None, "roe": None,
            })
        return {
            "abstraction": "paper",
            "equity": equity,
            "spot_usdc_total": equity,
            "held_collateral": total_margin,
            "available_margin": max(0.0, equity - total_margin),
            "total_margin_used": total_margin,
            "account_value_by_dex": {"paper": equity},
            "margin_used_by_dex": {"paper": total_margin},
            "withdrawable_by_dex": {"paper": max(0.0, equity - total_margin)},
            "positions": positions,
        }

    def open(self, ap: Approved, mark: float) -> dict:
        entry_px = mark * (1 + PAPER_SLIP) if ap.side == "long" else mark * (1 - PAPER_SLIP)
        # Honour the guard's floored venue lot. Deriving size from notional here
        # let paper fills use a size the venue would never accept, so dry mode
        # silently modelled a different trade from the one live would place.
        size = ap.size if ap.size > 0 else ap.notional / mark
        self.fees_paid += ap.notional * FEE_RATE
        return {"entry_px": entry_px, "size": round(size, 6)}

    def open_entry(self, ap: Approved, mark: float) -> dict:
        return self.open(ap, mark)

    def place_resting_entry(self, ap: Approved, size: float) -> dict:
        """Paper: the order rests on a simulated book until the mark crosses it.

        It has to be visible in open_orders_all — reconciliation settles an
        entry it can no longer see, so a paper resting entry with no book would
        be cancelled as vanished before it ever had a chance to fill."""
        self._resting_oid += 1
        self._resting.append({
            "coin": ap.market,
            "oid": self._resting_oid,
            "side": "B" if ap.side == "long" else "A",
            "sz": size,
            "limitPx": ap.entry_px,
            "orderType": "Limit",
            "reduceOnly": False,
            "isTrigger": False,
            "triggerPx": None,
            "timestamp": int(time.time() * 1000),
        })
        return {"oid": self._resting_oid, "entry_px": ap.entry_px, "size": size,
                "children": []}

    def place_brackets(self, market: str, side: str, size: float,
                       stop_px: float, tp_px: float) -> list[dict]:
        return []

    def close(self, pos: Position, mark: float) -> dict:
        close_px = mark * (1 - PAPER_SLIP) if pos.side == "long" else mark * (1 + PAPER_SLIP)
        self.fees_paid += abs(close_px * pos.size) * FEE_RATE
        return {"close_px": close_px}

    def close_position_only(self, pos: Position) -> dict:
        mark = pos.entry_px
        return self.close(pos, mark)

    def adjust_stop(self, pos: Position, stop_px: float, tp_px: float,
                    scale_out=None) -> dict:
        return {"old_oids": [], "new_orders": []}

    def bracket_orders(self, market: str) -> list[dict]:
        return []

    def cancel_orders(self, market: str, order_ids: list[int]) -> None:
        ids = set(order_ids)
        self._resting = [o for o in self._resting
                         if not (o["coin"] == market and o["oid"] in ids)]

    def cancel_brackets(self, market: str) -> None:
        return None   # paper brackets are simulated by the engine, not resting

    def fills(self) -> list[dict]:
        return []

    def open_orders_all(self) -> list[dict]:
        return [dict(o) for o in self._resting]


def realized_pnl(side: str, entry_px: float, close_px: float, size: float,
                 fee_rate: float = FEE_RATE) -> float:
    gross = (close_px - entry_px) * size if side == "long" else (entry_px - close_px) * size
    fees = (entry_px + close_px) * size * fee_rate
    return gross - fees


def bracket_hit(side: str, mark: float, stop_px: Optional[float],
                tp_px: Optional[float]) -> Optional[str]:
    """Dry-mode bracket check: which bracket (if any) does this mark trigger."""
    if side == "long":
        if stop_px is not None and mark <= stop_px:
            return "sl"
        if tp_px is not None and mark >= tp_px:
            return "tp"
    else:
        if stop_px is not None and mark >= stop_px:
            return "sl"
        if tp_px is not None and mark <= tp_px:
            return "tp"
    return None
