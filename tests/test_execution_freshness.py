"""Nothing may be gated against the marks the analyst started from.

The decision takes 180-225s against a 420s deadline, and the cycle is very often
triggered BECAUSE a market just moved 1%. Executing on the bundle's marks meant
the range-edge rail, the resting-entry side check, stop-distance sizing, the
margin fit and the liquidation band all judged a price minutes old.
"""

import copy

import pytest

from peri.market import Ctx
from tests.test_engine import OPEN_SOL, mk


def moving_analyst(eng, scripted, moves):
    """Wrap the scripted analyst so the tape moves DURING the decision, exactly
    as it does in production while the model is thinking."""
    inner = scripted.decide

    def decide(bundle):
        result = inner(bundle)
        for name, mark in moves.items():
            old = eng.market._ctxs[name]
            eng.market._ctxs[name] = Ctx(old.name, mark, old.day_pct,
                                         old.funding_apr_pct, old.oi_usd, old.vol_usd)
        return result

    scripted.decide = decide


def resting(market, entry, **kw):
    action = copy.deepcopy(OPEN_SOL["actions"][0])
    action.update(market=market, entry=entry, **kw)
    return {"actions": [action]}


def test_a_market_entry_is_refused_when_the_tape_ran_away_mid_decision(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [OPEN_SOL])
    moving_analyst(eng, analyst, {"SOL": 102.5})     # +2.5%, past the 0.5% limit

    eng.cycle("scheduled")

    assert state.open_position_for("SOL") is None
    reasons = [r["reason"] for r in state.recent_refusals(5, 3600)]
    assert any("while" in r and "decision was being made" in r for r in reasons), reasons
    assert any("stale entry refused" in line for line in notes.lines)


def test_a_market_entry_inside_the_drift_limit_still_executes(tmp_path):
    eng, state, _notes, analyst = mk(tmp_path, [OPEN_SOL])
    moving_analyst(eng, analyst, {"SOL": 100.2})     # +0.2%, inside the limit

    eng.cycle("scheduled")

    pos = state.open_position_for("SOL")
    assert pos is not None
    # and it was sized against the FRESH mark, not the one the analyst saw
    assert pos.entry_px == pytest.approx(100.2, rel=0.01)


def test_a_resting_entry_is_re_gated_against_the_fresh_mark(tmp_path):
    """A resting long parked under a stale mark can already sit ABOVE the live
    one — filling as the taker chase the whole design exists to avoid. The level
    is explicit so it is exempt from the drift refusal, but the guard must judge
    it against where the market actually is."""
    eng, state, _notes, analyst = mk(tmp_path, [resting("SOL", 99.0)])
    moving_analyst(eng, analyst, {"SOL": 98.0})      # mark fell below the 99.0 bid

    eng.cycle("scheduled")

    assert state.open_position_for("SOL") is None
    assert not state.resting_entries()
    reasons = [r["reason"] for r in state.recent_refusals(5, 3600)]
    assert any("at/above mark" in r for r in reasons), reasons


def test_a_resting_entry_survives_a_move_that_would_refuse_a_market_order(tmp_path):
    eng, state, _notes, analyst = mk(tmp_path, [resting("SOL", 99.0)])
    # +1.5%: past the 0.5% drift limit a market order would be refused on, but
    # the 99.0 bid is still below the new mark and inside the 5% offset band
    moving_analyst(eng, analyst, {"SOL": 101.5})

    eng.cycle("scheduled")

    parked = state.resting_entries()
    assert len(parked) == 1 and parked[0]["market"] == "SOL"


def test_two_resting_entries_in_one_decision_respect_max_concurrent(tmp_path):
    """Resting entries create pending orders, not positions, and the reserved-
    market set came from a frozenset captured BEFORE the decision. So every open
    in one decision saw an empty reserve and the cap simply did not apply."""
    first = copy.deepcopy(OPEN_SOL["actions"][0])
    first.update(market="SOL", entry=99.0)
    second = copy.deepcopy(OPEN_SOL["actions"][0])
    second.update(market="ETH", entry=198.0, stop=190.0, take_profit=220.0)
    eng, state, _notes, _ = mk(tmp_path, [{"actions": [first, second]}])
    eng.cfg.risk.max_concurrent = 1

    eng.cycle("scheduled")

    parked = state.resting_entries()
    assert len(parked) == 1, [p["market"] for p in parked]
    reasons = [r["reason"] for r in state.recent_refusals(5, 3600)]
    assert any("max concurrent" in r for r in reasons), reasons


def test_a_stale_mark_made_a_losing_trade_look_like_a_winning_one(tmp_path):
    """The concrete shape of the bug, not just its mechanism.

    SOL is decided at 100.0 with stop 97.0 and target 107.0 — reward:risk 2.33,
    comfortably over the 2.0 floor. While the analyst thinks, SOL runs to 102.5.
    Gated on the stale mark the trade still reads 2.33 and is approved; gated on
    the mark it will actually fill at, the SAME stop and target are worth 0.82.
    The old code opened that position believing it was a 2.33R setup.
    """
    eng, state, _notes, analyst = mk(tmp_path, [OPEN_SOL])
    eng.cfg.risk.max_mark_drift_pct = 0.0        # isolate the RR rail
    moving_analyst(eng, analyst, {"SOL": 102.5})

    eng.cycle("scheduled")

    assert state.open_position_for("SOL") is None
    reasons = [r["reason"] for r in state.recent_refusals(5, 3600)]
    assert any("RR 0.82" in r for r in reasons), reasons
