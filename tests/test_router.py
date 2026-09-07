import pytest

from peri.risk import Approved
from peri.router import DryRunAdapter, FEE_RATE, bracket_hit, realized_pnl
from peri.state import State


def ap(**kw):
    base = dict(market="BTC", side="long", notional=600.0, size_usd_risk=15.0,
                leverage=10.0, margin=60.0, margin_mode="isolated",
                stop_px=78000.0, tp_px=84000.0)
    base.update(kw)
    return Approved(**base)


def test_dry_open_adverse_slip_and_fees(tmp_path):
    s = State(str(tmp_path / "t.db"))
    a = DryRunAdapter(s, 1000.0)
    fill = a.open(ap(), mark=80000.0)
    assert fill["entry_px"] > 80000.0          # long pays up
    assert fill["size"] == pytest.approx(600.0 / 80000.0, abs=5e-7)
    assert a.fees_paid == pytest.approx(600.0 * FEE_RATE)
    fill_s = a.open(ap(side="short"), mark=80000.0)
    assert fill_s["entry_px"] < 80000.0        # short sells down


def test_dry_equity_includes_unrealized(tmp_path):
    s = State(str(tmp_path / "t.db"))
    a = DryRunAdapter(s, 1000.0)
    s.add_position("BTC", "long", 80000.0, 0.0075, 600.0, 5.0, None, None, 0.8, "own")
    eq = a.equity({"BTC": 81000.0})
    assert eq == pytest.approx(1000.0 + 1000.0 * 0.0075)
    s.close_position(1, "tp", 81000.0, 7.5)
    assert a.equity({}) == pytest.approx(1007.5)


def test_dry_account_snapshot_exposes_equity_and_available_margin(tmp_path):
    state = State(str(tmp_path / "t.db"))
    adapter = DryRunAdapter(state, 1000.0)
    state.add_position("BTC", "long", 80000.0, 0.0075, 600.0, 5.0,
                       78000.0, 84000.0, 0.8, "own", margin_mode="cross")

    snapshot = adapter.account_snapshot({"BTC": 81000.0})

    assert snapshot["equity"] == pytest.approx(1007.5)
    assert snapshot["total_margin_used"] == pytest.approx(120.0)
    assert snapshot["available_margin"] == pytest.approx(887.5)
    assert snapshot["positions"][0]["margin_mode"] == "cross"


def test_realized_pnl_fees_both_sides():
    pnl = realized_pnl("long", 100.0, 102.0, 1.0)
    assert pnl == pytest.approx(2.0 - (100.0 + 102.0) * FEE_RATE)
    pnl_s = realized_pnl("short", 100.0, 98.0, 2.0)
    assert pnl_s == pytest.approx(4.0 - (100.0 + 98.0) * 2.0 * FEE_RATE)


def test_bracket_hit_long_short():
    assert bracket_hit("long", 77999.0, 78000.0, 84000.0) == "sl"
    assert bracket_hit("long", 84000.0, 78000.0, 84000.0) == "tp"
    assert bracket_hit("long", 80000.0, 78000.0, 84000.0) is None
    assert bracket_hit("short", 82000.0, 82000.0, 76000.0) == "sl"
    assert bracket_hit("short", 76000.0, 82000.0, 76000.0) == "tp"
    assert bracket_hit("short", 80000.0, None, None) is None
