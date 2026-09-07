"""Scale-out: bank part of the position on the way to a target that rarely lands.

The rails FORCE distant targets. min_stop_pct 2% against min_rr 2.0 puts every
take-profit at least 4% away — on a major that is 8-9x ATR15m — while
time_stop_secs closes anything under +0.5R at 3h. Across the 27 live trades to
2026-09-07, exactly ONE take-profit was ever reached.

The RR floor is not the thing to weaken: it exists because 2026-08-28 lost 12.8%
on seven trades that never cleared their own costs. So the analyst's target
stays exactly where it put it, and part of the position banks on the way there.
"""

from peri.risk import plan_scale_out
from tests.test_engine import mk

# entry 100, stop 96 -> 1R = 4.0; target 108 = +2R
LONG = dict(side="long", entry_px=100.0, stop_px=96.0, tp_px=108.0,
            size=1.0, sz_decimals=2)


def plan(**over):
    kw = dict(LONG, at_r=1.0, frac=0.5, min_notional=10.0)
    kw.update(over)
    return plan_scale_out(**kw)


def test_the_tranche_lands_at_the_configured_r():
    p = plan()
    assert p.tp1_px == 104.0                      # entry + 1.0 x 4.0
    assert p.tp1_size == 0.5 and p.runner_size == 0.5


def test_a_short_banks_below_the_entry():
    p = plan(side="short", entry_px=100.0, stop_px=104.0, tp_px=92.0)
    assert p.tp1_px == 96.0
    assert p.tp1_size + p.runner_size == 1.0


def test_the_pieces_always_sum_to_the_approved_lot():
    """Rounding must never leave a sliver of the position unprotected: the
    runner takes the remainder rather than being floored independently."""
    for size in (1.0, 0.33, 0.07, 2.51, 9.99):
        p = plan(size=size, min_notional=0.0)
        if p is None:
            continue
        assert abs((p.tp1_size + p.runner_size) - size) < 1e-9, size


def test_no_split_when_the_target_is_nearer_than_the_tranche():
    """Two orders at the same level is not a scale-out."""
    assert plan(tp_px=103.0) is None
    assert plan(tp_px=104.0) is None


def test_no_split_when_a_piece_would_floor_to_nothing():
    assert plan(sz_decimals=0) is None             # 0.5 floors to 0 lots


def test_no_split_into_dust_below_the_venue_minimum():
    """A tranche too small to be worth a fill pays a full builder fee to bank
    pennies and leaves the runner under-sized."""
    assert plan(size=0.1, min_notional=10.0) is None


def test_disabled_by_default():
    assert plan(at_r=0.0) is None
    assert plan(frac=0.0) is None
    assert plan(frac=1.0) is None


def test_the_projection_uses_the_blended_reward_not_the_full_target():
    """Half at +1R and half at +2R is +1.5R, not +2R. A floor that projected
    the un-blended figure would be inflated by construction."""
    p = plan()
    assert p.blended_r(2.0) == 1.5
    assert p.blended_r(3.0) == 2.0


def test_the_tp_floor_refuses_a_setup_that_only_clears_un_blended(tmp_path):
    """The gate must not become more permissive because part of the position
    now exits early — refuse rather than inflate."""
    from peri.models import OpenAction
    eng, _state, _n, _a = mk(tmp_path, [{"actions": []}])
    guard = eng.guard
    guard.cfg.scale_out_at_r = 1.0
    guard.cfg.scale_out_frac = 0.5
    guard.cfg.min_stop_pct = 0.0
    guard.cfg.atr_stop_mult = 0.0
    guard.cfg.max_range_pos_long = 1.0
    guard.cfg.min_range_pos_short = 0.0
    action = OpenAction(market="SOL", side="long", conviction=0.8, stop=96.0,
                        take_profit=108.0, leverage=10, margin_mode="cross",
                        rationale="r", invalidation="i")

    guard.cfg.tp_net_floor_usd = 0.0
    approved = guard.gate_open(action, 1000.0, 100.0, 20.0, "2026-09-07",
                               available_margin=1000.0,
                               reserved_order_markets=frozenset(), sz_decimals=2)
    assert not hasattr(approved, "reason"), getattr(approved, "reason", "")
    risk = approved.size_usd_risk
    # blended 1.5R clears a floor set just under it, but not one above it
    guard.cfg.tp_net_floor_usd = 1.4 * risk
    assert not hasattr(
        guard.gate_open(action, 1000.0, 100.0, 20.0, "2026-09-07",
                        available_margin=1000.0,
                        reserved_order_markets=frozenset(), sz_decimals=2),
        "reason")
    guard.cfg.tp_net_floor_usd = 1.8 * risk
    refused = guard.gate_open(action, 1000.0, 100.0, 20.0, "2026-09-07",
                              available_margin=1000.0,
                              reserved_order_markets=frozenset(), sz_decimals=2)
    assert hasattr(refused, "reason") and "projected net TP" in refused.reason


def test_dry_mode_banks_the_tranche_and_lets_the_runner_run(tmp_path):
    eng, state, notes, _a = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.scale_out_at_r = 1.0
    eng.cfg.risk.scale_out_frac = 0.5
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0,
                             96.0, 108.0, 0.8, "own")

    eng.reconcile_dry({"SOL": 104.0})              # tranche level
    row = state.db.execute("SELECT size, status, scaled_out FROM positions"
                           " WHERE id=?", (pos.id,)).fetchone()
    assert row["status"] == "open", "the runner must survive"
    assert abs(row["size"] - 0.5) < 1e-9
    assert row["scaled_out"] == 1
    assert any("tp tranche" in line for line in notes.lines), notes.lines

    eng.reconcile_dry({"SOL": 108.0})              # runner reaches the target
    assert state.db.execute("SELECT status FROM positions WHERE id=?",
                            (pos.id,)).fetchone()["status"] == "closed"


def test_a_banked_position_is_exempt_from_the_time_stop(tmp_path):
    """It already paid for its slot. Closing the runner on the clock would take
    the cheap half of the trade and leave the expensive half."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.time_stop_secs = 1
    eng.cfg.risk.time_stop_min_r = 0.5
    eng.cfg.risk.trail_start_r = 0.0
    eng.cfg.risk.breakeven_at_r = 0.0
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0,
                             96.0, 108.0, 0.8, "own")
    state.db.execute("UPDATE positions SET opened_ts=opened_ts-9999 WHERE id=?",
                     (pos.id,))
    state.db.commit()

    eng.manage_positions({"SOL": 100.5})           # +0.125R, long past the clock
    assert state.db.execute("SELECT status FROM positions WHERE id=?",
                            (pos.id,)).fetchone()["status"] == "closed"

    other = tmp_path / "b"
    other.mkdir()
    eng2, state2, _n2, _a2 = mk(other, [{"actions": []}])
    eng2.cfg.risk.time_stop_secs = 1
    eng2.cfg.risk.trail_start_r = 0.0
    eng2.cfg.risk.breakeven_at_r = 0.0
    pos2 = state2.add_position("SOL", "long", 100.0, 0.5, 50.0, 10.0,
                               96.0, 108.0, 0.8, "own")
    state2.db.execute("UPDATE positions SET opened_ts=opened_ts-9999 WHERE id=?",
                      (pos2.id,))
    state2.db.commit()
    state2.mark_scaled_out(pos2.id)

    eng2.manage_positions({"SOL": 100.5})
    assert state2.db.execute("SELECT status FROM positions WHERE id=?",
                             (pos2.id,)).fetchone()["status"] == "open"


def test_a_banked_position_is_not_split_again(tmp_path):
    """The remainder is a runner. Re-splitting would sell the same profit twice."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.scale_out_at_r = 1.0
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0,
                             96.0, 108.0, 0.8, "own")
    assert eng._scale_out_for(state.open_position_for("SOL")) is not None
    state.mark_scaled_out(pos.id)
    assert eng._scale_out_for(state.open_position_for("SOL")) is None
