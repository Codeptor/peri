"""Trailing stops: convert a peak into money instead of giving it back.

2026-08-31, the xyz:CL long: filled 85.6 at 13:21Z, peaked 86.388 at 13:35Z
(+$1.22, +8.5% on margin), and was back to +$0.23 by 14:00Z — 80% of the gain
returned. Its take-profit sat at 89.5, +4.56% away, about 9x the market's own
ATR15m of 0.49%. The RR>=2.0 floor against a 2% minimum stop FORCES a target
that far out, so on anything short of a runaway move the position round-trips
to its stop rather than ever reaching the target.
"""


from tests.test_engine import mk


def setup(tmp_path, *, side="long", entry=85.6, stop=83.8, tp=89.5, atr=0.49,
          trail_start_r=0.5, trail_atr_mult=1.5, breakeven=1.0):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.trail_start_r = trail_start_r
    eng.cfg.risk.trail_atr_mult = trail_atr_mult
    eng.cfg.risk.breakeven_at_r = breakeven
    eng.cfg.risk.time_stop_secs = 0
    eng._features_cache["xyz:CL"] = {"atr15m_pct": atr}
    pos = state.add_position("xyz:CL", side, entry, 1.546, entry * 1.546, 10.0,
                             stop, tp, 0.8, "own")
    state.set_initial_stop(pos.id, stop) if hasattr(state, "set_initial_stop") else None
    return eng, state, notes, pos


def stop_of(state, pos_id):
    return state.db.execute("SELECT stop_px FROM positions WHERE id=?",
                            (pos_id,)).fetchone()["stop_px"]


def test_the_stop_follows_the_high_water_mark(tmp_path):
    eng, state, _notes, pos = setup(tmp_path)
    # +0.92% to the real peak: 1.8 risk, so 0.788 move = +0.44R... push further
    eng.manage_positions({"xyz:CL": 87.0})       # +1.4/1.8 = +0.78R, past 0.5R
    trailed = stop_of(state, pos.id)
    # 1.5 x 0.49% of 87.0 = 0.639 behind the peak
    assert trailed is not None and abs(trailed - (87.0 - 0.639)) < 0.05, trailed
    assert trailed > 85.6, "must lock in profit above entry"


def test_the_stop_never_retreats_when_price_falls_back(tmp_path):
    eng, state, _notes, pos = setup(tmp_path)
    eng.manage_positions({"xyz:CL": 87.0})
    high = stop_of(state, pos.id)
    eng.manage_positions({"xyz:CL": 86.2})       # give-back
    eng.manage_positions({"xyz:CL": 85.9})
    assert stop_of(state, pos.id) == high, "a trailed stop must ratchet, never loosen"


def test_the_peak_is_the_high_water_mark_not_the_last_price(tmp_path):
    eng, state, _notes, pos = setup(tmp_path)
    eng.manage_positions({"xyz:CL": 87.4})
    eng.manage_positions({"xyz:CL": 86.0})       # fell back; peak stays 87.4
    peak = state.db.execute("SELECT peak_px FROM positions WHERE id=?",
                            (pos.id,)).fetchone()["peak_px"]
    assert peak == 87.4


def test_nothing_trails_before_the_start_threshold(tmp_path):
    eng, state, _notes, pos = setup(tmp_path, trail_start_r=0.5, breakeven=99)
    eng.manage_positions({"xyz:CL": 86.0})       # +0.4/1.8 = +0.22R
    assert stop_of(state, pos.id) == 83.8, "must not touch the stop this early"


def test_a_short_trails_downward(tmp_path):
    eng, state, _notes, pos = setup(tmp_path, side="short", entry=85.6,
                                    stop=87.4, tp=81.0)
    eng.manage_positions({"xyz:CL": 84.2})       # +1.4/1.8 = +0.78R
    trailed = stop_of(state, pos.id)
    assert trailed is not None and trailed < 85.6, trailed
    assert abs(trailed - (84.2 + 84.2 * 0.0049 * 1.5)) < 0.05, trailed


def test_a_missing_atr_leaves_the_stop_alone_rather_than_guessing(tmp_path):
    """The trail width has to clear this market's own noise. With no ATR there
    is no honest width, and protection already in place must not be replaced by
    a guess — but breakeven still applies at its own threshold."""
    eng, state, _notes, pos = setup(tmp_path, breakeven=99)
    eng._features_cache["xyz:CL"] = {}
    eng.manage_positions({"xyz:CL": 87.0})
    assert stop_of(state, pos.id) == 83.8


def test_the_trail_never_lands_on_the_wrong_side_of_the_mark(tmp_path):
    """A stop at or above the mark for a long would trigger instantly."""
    eng, state, _notes, pos = setup(tmp_path, atr=0.001, trail_atr_mult=0.0)
    eng.manage_positions({"xyz:CL": 87.0})
    trailed = stop_of(state, pos.id)
    assert trailed is None or trailed < 87.0


def test_trailing_disabled_falls_back_to_plain_breakeven(tmp_path):
    eng, state, _notes, pos = setup(tmp_path, trail_start_r=0.0, breakeven=1.0)
    eng.manage_positions({"xyz:CL": 87.5})       # +1.9/1.8 = +1.05R
    trailed = stop_of(state, pos.id)
    assert trailed is not None and abs(trailed - 85.6 * (1 + 2 * 0.00075)) < 0.02, trailed


def test_the_alert_says_which_mechanism_moved_the_stop(tmp_path):
    """Breakeven and trailing move the same stop for different reasons; the
    operator needs to know which one fired."""
    eng, _state, notes, _pos = setup(tmp_path)
    eng.manage_positions({"xyz:CL": 87.0})
    assert any("trailing" in line and "behind" in line for line in notes.lines), notes.lines

    other = tmp_path / "b"
    other.mkdir()
    eng2, _s2, notes2, _p2 = setup(other, trail_start_r=0.0, breakeven=1.0)
    eng2.manage_positions({"xyz:CL": 87.5})
    assert any("breakeven" in line for line in notes2.lines), notes2.lines


def test_a_position_whose_venue_stop_vanished_still_ratchets_without_killing_the_cycle(
        tmp_path):
    """`better` deliberately admits stop_px=None (the venue stop is gone and
    sync_live_brackets wrote NULL). The notify line then formatted it with :g,
    OUTSIDE the try that guards the venue call — so the stop moved and the
    TypeError propagated out of context_snapshot, aborting the whole cycle as
    "context DOWN": no analyst, no time stop, no orphan recovery.
    """
    eng, state, notes, pos = setup(tmp_path)
    state.update_brackets(pos.id, None, 89.5)
    assert stop_of(state, pos.id) is None

    changed = eng.manage_positions({"xyz:CL": 87.0})

    assert changed is True
    trailed = stop_of(state, pos.id)
    assert trailed is not None and trailed > 85.6
    assert any("none ->" in line for line in notes.lines), notes.lines
