"""Which mechanism actually ended a trade — and how far it ran before it did.

The 2026-09-07 export of the live ledger showed 27 closed trades, 44% win, and
exactly ONE take-profit. It could not show why, because `close_reason` collapses
three different outcomes into 'sl': a thesis that was wrong and paid -1R, a
winner parked at breakeven, and a winner the trail took out in profit. A time
stop, meanwhile, was filed as 'analyst' — indistinguishable from a discretionary
close.

The measured record is the only thing the analyst carries between cycles, so
with those merged it could read "my stops keep getting hit" and never "I keep
being trailed out of winners". These tests pin the distinction.
"""

from tests.test_engine import mk


def open_pos(state, *, entry=100.0, stop=96.0, tp=108.0, side="long", size=1.0):
    pos = state.add_position("SOL", side, entry, size, entry * size, 10.0,
                             stop, tp, 0.8, "own")
    return pos


def test_an_untouched_stop_is_an_initial_stop_out(tmp_path):
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    assert eng._exit_kind(pos, "sl") == "initial_stop"


def test_a_stop_parked_at_breakeven_is_not_a_stop_out(tmp_path):
    """It cost nothing. Filing it as a loss teaches the wrong lesson."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    state.update_brackets(pos.id, 100.0 * (1 + 2 * 0.00075), 108.0)
    assert eng._exit_kind(state.open_position_for("SOL"), "sl") == "breakeven_stop"


def test_a_stop_that_followed_the_move_is_a_trail_out(tmp_path):
    """The exact case the record could not surface: a WINNER, closed by the
    stop, in profit — booked identically to a -1R loss."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    state.update_brackets(pos.id, 104.0, 108.0)     # +1R, well past breakeven
    assert eng._exit_kind(state.open_position_for("SOL"), "sl") == "trail_stop"


def test_a_take_profit_is_left_alone(tmp_path):
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    assert eng._exit_kind(pos, "tp") == "tp"
    assert eng._exit_kind(pos, "external") == "external"


def test_an_adopted_position_with_no_risk_unit_is_not_guessed_at(tmp_path):
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    state.db.execute("UPDATE positions SET init_stop_px=NULL WHERE id=?", (pos.id,))
    state.db.commit()
    assert eng._exit_kind(state.open_position_for("SOL"), "sl") == "stop"


def test_a_dry_bracket_close_records_the_mechanism(tmp_path):
    """End to end: the trail moves the stop, price falls back through it, and
    the ledger says trail_stop rather than a bare 'sl'."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.trail_start_r = 1.0
    eng.cfg.risk.trail_giveback_r = 0.5
    eng.cfg.risk.time_stop_secs = 0
    eng._features_cache["SOL"] = {"atr15m_pct": 0.5}
    pos = open_pos(state)

    eng.manage_positions({"SOL": 105.0})            # +1.25R -> trail arms
    trailed = state.open_position_for("SOL").stop_px
    assert trailed > 100.0

    eng.reconcile_dry({"SOL": trailed - 0.5})       # pull back through it
    row = state.db.execute(
        "SELECT close_reason, exit_kind, realized_pnl FROM positions WHERE id=?",
        (pos.id,)).fetchone()
    assert row["close_reason"] == "sl"
    assert row["exit_kind"] == "trail_stop"
    assert row["realized_pnl"] > 0, "a trail-out above entry is a WINNER"


def test_excursion_is_recorded_even_for_a_trade_that_never_trails(tmp_path):
    """peak_px only advances past trail_start_r, so it could never answer 'how
    far did this go against me first'. MAE/MFE are tracked for every position
    on every pass, including ones no rule touches."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.cfg.risk.trail_start_r = 1.0
    eng.cfg.risk.breakeven_at_r = 1.0
    eng.cfg.risk.time_stop_secs = 0
    eng._features_cache["SOL"] = {"atr15m_pct": 0.5}
    pos = open_pos(state)

    eng.manage_positions({"SOL": 98.0})     # -0.5R
    eng.manage_positions({"SOL": 102.0})    # +0.5R, still short of any rule
    eng.manage_positions({"SOL": 99.0})

    row = state.db.execute(
        "SELECT mfe_r, mae_r, stop_px FROM positions WHERE id=?", (pos.id,)).fetchone()
    assert row["stop_px"] == 96.0, "no rule should have fired"
    assert abs(row["mfe_r"] - 0.5) < 1e-6
    assert abs(row["mae_r"] - -0.5) < 1e-6


def test_the_digest_separates_a_trail_out_from_a_stop_out(tmp_path):
    _eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    lost = open_pos(state)
    state.close_position(lost.id, "sl", 96.0, -4.0, exit_kind="initial_stop")
    won = state.add_position("BTC", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 108.0,
                             0.8, "own")
    state.close_position(won.id, "sl", 104.0, 4.0, exit_kind="trail_stop")

    digest = state.performance_digest()
    by_kind = digest["by_exit_kind"]
    assert by_kind["initial_stop"]["n"] == 1
    assert by_kind["trail_stop"]["n"] == 1
    assert by_kind["trail_stop"]["win_rate"] == 1.0
    # the old view merges them into one meaningless 50%-win 'sl' bucket
    assert digest["by_close_reason"]["sl"]["n"] == 2


def test_the_digest_reports_how_much_of_the_best_move_was_returned(tmp_path):
    """avg_r alone cannot tell 'never worked' from 'worked, then gave it back'."""
    _eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    state.update_excursion(pos.id, 2.0)     # ran to +2R
    state.update_excursion(pos.id, 0.25)
    state.close_position(pos.id, "sl", 101.0, 1.0, exit_kind="trail_stop")

    overall = state.performance_digest()["overall"]
    assert abs(overall["avg_mfe_r"] - 2.0) < 1e-6
    assert overall["avg_giveback_r"] > 1.0, "a 2R peak booked at 0.25R gave back 1.75R"


def test_free_text_close_reasons_do_not_become_their_own_category(tmp_path):
    """The 09-07 export carried a bucket literally named
    'stop 1.413 (+1R ratchet) filled @ 1.4129' — a typo rendered as a finding."""
    _eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = open_pos(state)
    state.close_position(pos.id, "stop 1.413 (+1R ratchet) filled @ 1.4129", 96.0, -4.0)
    assert "other" in state.performance_digest()["by_close_reason"]
