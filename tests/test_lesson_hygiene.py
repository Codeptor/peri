"""Lessons stay causal; statistics come from the ledger.

By the 2026-09-07 export the 42-lesson corpus held six versions of the
long/short split — "shorts 67% / longs 33%", "shorts 80% / +0.38R", "longs 36%
/ -0.27R" — written at different sample sizes against a final measured 56%/39%.
Every stale one was still replayed into the prompt each cycle as hard-won fact,
so the analyst was reading its own out-of-date arithmetic as evidence while the
recomputed digest sat directly above it saying something else.
"""

from peri.models import Decision, RememberAction, restates_a_statistic
from tests.test_engine import mk

CAUSAL = [
    "post-earnings gap-downs bounce for the first hour - short the retest, not the low",
    "HYPE longs keep failing at the 24h high; wait for the reclaim instead",
    "a 2% stop on xyz:CL is inside the noise band; use the session low",
    "oil moves on the Sunday Globex open, so do not treat the dex as shut",
]
STATISTICAL = [
    "shorts win 67% and longs 33%, so prefer shorts",
    "my win rate of 44 means I should be more selective",
    "avg +0.20R on shorts, lean short",
    "I have taken 27 trades and most lost",
    "longs won only 39% of the time",
]


def test_causal_lessons_are_kept():
    for lesson in CAUSAL:
        assert not restates_a_statistic(lesson), lesson


def test_remembered_statistics_are_recognised():
    for lesson in STATISTICAL:
        assert restates_a_statistic(lesson), lesson


def test_a_stale_statistic_is_refused_rather_than_stored(tmp_path):
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.exec_remember(RememberAction(lesson="shorts win 67% and longs 33%"))
    assert state.lessons() == []
    reasons = [r["reason"] for r in state.db.execute("SELECT reason FROM refusals")]
    assert any("restates a statistic" in r for r in reasons), reasons


def test_a_causal_lesson_is_still_stored(tmp_path):
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    eng.exec_remember(RememberAction(lesson=CAUSAL[0]))
    assert [lesson["text"] for lesson in state.lessons()] == [CAUSAL[0]]


def test_a_bad_lesson_never_invalidates_the_decision_around_it():
    """The check deliberately does NOT live in pydantic. A Decision validates as
    a whole, so raising there would fail the trades in the same response and
    burn two retries before a loud AnalystError — a skipped cycle over a
    wasted line of prose."""
    decision = Decision.model_validate({
        "market_view": "chop",
        "actions": [
            {"kind": "remember", "lesson": "shorts win 67% and longs 33%"},
            {"kind": "close", "market": "SOL", "rationale": "invalidation hit"},
        ],
    })
    assert len(decision.actions) == 2


def test_the_whole_cycle_survives_a_statistical_lesson(tmp_path):
    """End to end: the lesson is dropped, the close still executes."""
    eng, state, _n, _a = mk(tmp_path, [{"actions": []}])
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 108.0,
                             0.8, "own")
    decision = Decision.model_validate({
        "market_view": "chop",
        "actions": [
            {"kind": "remember", "lesson": "longs won only 39% of the time"},
            {"kind": "close", "market": "SOL", "rationale": "invalidation hit"},
        ],
    })
    decision_id = state.record_decision(
        "test", decision.market_view, decision.model_dump_json(), "m", 0, "ok")
    for action in decision.actions:
        eng.execute(action, {"SOL": 101.0}, 1000.0, "2026-09-07", frozenset(),
                    decision_id=decision_id)

    assert state.lessons() == []
    closed = state.db.execute(
        "SELECT status FROM positions WHERE id=?", (pos.id,)).fetchone()["status"]
    assert closed == "closed", "the trade must not be lost to a bad lesson"
