"""A halt must reach the orders ALREADY on the book, and the daily cap must
count them.

A resting entry can sit for entry_expiry_secs (2h) and fill long after the
conditions that justified it stopped holding. The fill path adopts it
unconditionally, so the kill switch and the day-loss halt — both of which exist
to stop the bleeding — did not reach the orders already parked. And because a
resting entry was only counted against the daily cap when it FILLED, several
could be placed against a cap that was already nearly spent.
"""

import copy
import time

from tests.test_engine import OPEN_SOL, mk, utc_day


def park(state, market, entry_px, *, oid=1):
    return state.add_pending_entry(
        market=market, side="long", entry_px=entry_px, size=1.0, notional=100.0,
        leverage=10.0, margin_mode="isolated", stop_px=entry_px * 0.96,
        tp_px=entry_px * 1.12, conviction=0.8, rationale="pullback",
        invalidation="loses the shelf", oid=oid, decision_id=None,
        expires_ts=time.time() + 3600, entry_context={"style": "resting"},
    )


def test_the_kill_switch_withdraws_orders_already_on_the_book(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}])
    day = utc_day(time.time())
    state.open_day(day, 1000.0)
    park(state, "SOL", 99.0)
    assert len(state.resting_entries()) == 1

    # equity 15% below the day open trips the kill switch on the next cycle
    eng.adapter.bankroll = 800.0
    eng.cycle("scheduled")

    assert state.kill_tripped(day)
    assert state.resting_entries() == []
    assert any("KILL SWITCH" in line and "withdrew resting" in line
               for line in notes.lines), notes.lines


def test_the_day_loss_halt_withdraws_orders_already_on_the_book(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.day_loss_halt_pct = 6.0
    day = utc_day(time.time())
    state.open_day(day, 1000.0)
    park(state, "SOL", 99.0)

    eng.adapter.bankroll = 920.0          # -8%: past the halt, short of the kill
    eng.cycle("scheduled")

    assert not state.kill_tripped(day)
    assert state.resting_entries() == []
    assert any("DAY-LOSS HALT" in line for line in notes.lines), notes.lines


def test_the_halt_is_announced_once_not_every_cycle(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}, {"actions": []}])
    eng.cfg.risk.day_loss_halt_pct = 6.0
    state.open_day(utc_day(time.time()), 1000.0)
    eng.adapter.bankroll = 920.0

    eng.cycle("scheduled")
    eng.cycle("scheduled")

    assert sum("DAY-LOSS HALT" in line for line in notes.lines) == 1


def test_resting_orders_count_against_the_daily_cap(tmp_path):
    """Three orders parked against a cap already at 2/3 all used to clear the
    gate, so a three-entry day could take five."""
    eng, state, _notes, _ = mk(tmp_path, [{"actions": []}])
    day = utc_day(time.time())
    state.open_day(day, 1000.0)
    eng.cfg.risk.daily_entry_cap = 2
    state.count_entry(day)                # one filled entry today
    park(state, "ETH", 198.0)             # one still resting

    action = copy.deepcopy(OPEN_SOL["actions"][0])
    action.update(market="SOL", entry=99.0)
    from peri.models import OpenAction
    verdict = eng.guard.gate_open(
        OpenAction.model_validate(action), 1000.0, 100.0, 20.0, day,
        available_margin=1000.0, reserved_order_markets=frozenset(),
        features={"range24h_pos": 0.4, "atr15m_pct": 0.5,
                  "hi_24h": 110.0, "lo_24h": 90.0},
        day_pnl_pct=0.0, sz_decimals=2)

    assert getattr(verdict, "reason", "").startswith("daily entry cap")
    assert "2 entered or resting today" in verdict.reason


def test_an_expired_resting_order_gives_its_cap_slot_back(tmp_path):
    eng, state, _notes, _ = mk(tmp_path, [{"actions": []}])
    day = utc_day(time.time())
    state.open_day(day, 1000.0)
    entry_id = park(state, "ETH", 198.0)
    assert state.resting_entries_today(day) == 1

    state.settle_pending_entry(entry_id, "expired")

    assert state.resting_entries_today(day) == 0
