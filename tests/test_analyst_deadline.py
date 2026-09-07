"""A slow decision must not become 900 seconds of nothing.

2026-08-31 09:16Z: a caller-message wake — the path that is supposed to be
FAST — spent 900s in the analyst and produced no decision. Three attempts, each
sending the identical prompt with the identical 24,000-token budget, each
reasoning to the same ceiling and timing out at 300s. Measured that day: p50
latency 124s at ~16k chars of reasoning, but every cycle over 300s carried
42,000-54,000 chars. The thinking ceiling WAS the latency.
"""

import time

import httpx
import pytest

from peri.analyst import MIN_ATTEMPT_SECS, Analyst, AnalystError
from peri.config import AnalystCfg, RiskCfg
from tests.test_analyst import BUNDLE as _BASE

BUNDLE = {**_BASE, "trigger": "caller message"}
RAILS = RiskCfg(5.0, 20.0, 1, 3, 15.0, 2.0, 900, 3600, 14400, 10.0, 5.0, 1000.0)


def cfg(**kw):
    base = dict(cycle_secs=900, timeout_secs=240, retries=2, temperature=0.2,
                max_output_tokens=9000, conviction_min=0.75, max_tool_rounds=0,
                total_deadline_secs=420)
    base.update(kw)
    return AnalystCfg(**base)


DECISION = '{"market_view":"flat","actions":[]}'   # transport returns a message


def mk(transport, fallback="fast-model", **kw):
    return Analyst(cfg(**kw), "k", "http://x/v1", "m", 0.75, 2.0, 20.0,
                   transport=transport, rails=RAILS, fallback_model=fallback)





def test_a_retry_swaps_to_the_faster_model_not_a_smaller_budget():
    """Measured 2026-08-31 on the live endpoint: the SAME prompt at
    max_tokens=3000 produced 8,176 completion tokens (the cap is not applied to
    reasoning) and ran 225.8s versus 178.6s at 9000 — shrinking the budget made
    it SLOWER. deepseek-v4-flash answered that prompt in 80.7s against
    qwen3.8-max's 178.6s. The lever is the model."""
    models, budgets = [], []

    def transport(payload):
        models.append(payload["model"])
        budgets.append(payload["max_tokens"])
        if len(models) < 2:
            raise httpx.ReadTimeout("read timed out")
        return DECISION

    mk(transport).decide(BUNDLE)
    assert models == ["m", "fast-model"], models
    assert budgets == [9000, 9000], "the budget must NOT be cut — it is not a latency knob"


def test_without_a_fallback_the_retry_simply_repeats_on_the_same_model():
    models = []

    def transport(payload):
        models.append(payload["model"])
        if len(models) < 2:
            raise httpx.ReadTimeout("slow")
        return DECISION

    mk(transport, fallback="").decide(BUNDLE)
    assert models == ["m", "m"]


def test_the_fallback_is_not_swapped_back_and_forth():
    models = []

    def transport(payload):
        models.append(payload["model"])
        raise httpx.ReadTimeout("slow")

    with pytest.raises(AnalystError):
        mk(transport, retries=3, total_deadline_secs=9999).decide(BUNDLE)
    assert models == ["m", "fast-model", "fast-model", "fast-model"], models


def test_retries_drop_the_search_rounds():
    """Each tool round is another full reasoning pass — the last thing a
    decision that is already too slow needs."""
    payloads = []

    class Tools:
        def available(self):
            return True

    def transport(payload):
        payloads.append(payload)
        if len(payloads) < 2:
            raise httpx.ReadTimeout("slow")
        return DECISION

    a = Analyst(cfg(max_tool_rounds=1), "k", "http://x/v1", "m", 0.75, 2.0, 20.0,
                transport=transport, tools=Tools(), rails=RAILS)
    a.decide({**BUNDLE, "trigger": "scheduled"})
    assert "tools" in payloads[0], "first attempt may search"
    assert all("tools" not in p for p in payloads[1:]), "retries must not search"


def test_the_total_deadline_stops_a_doomed_retry_chain():
    """The real bound used to be retries * timeout_secs. A caller wake is
    worthless 15 minutes later; give up inside the deadline instead."""
    clock = [0.0]
    calls = []

    def transport(payload):
        calls.append(payload)
        clock[0] += 200.0        # every attempt burns 200s
        raise httpx.ReadTimeout("slow")

    a = mk(transport, retries=5, total_deadline_secs=420)
    a_time = time.monotonic
    try:
        import peri.analyst as mod
        mod.time.monotonic = lambda: clock[0]
        with pytest.raises(AnalystError, match="no time left"):
            a.decide(BUNDLE)
    finally:
        import peri.analyst as mod
        mod.time.monotonic = a_time
    # attempt 1 at t=0 -> 200s, attempt 2 at t=200 -> 400s. Only 20s of the 420s
    # deadline remain, under the 45s floor, so the third attempt is never made.
    # The old code would have run all six, for 1200s of nothing.
    assert len(calls) == 2, calls
    assert clock[0] == 400.0


def test_the_per_attempt_timeout_is_clipped_to_what_remains():
    """An attempt must never be allowed to run past the whole decision's
    deadline — that is how 300s attempts summed to 900s."""
    clock = [0.0]
    timeouts = []

    def transport(payload):
        timeouts.append(payload.get("_timeout"))
        clock[0] += 300.0
        raise httpx.ReadTimeout("slow")

    a = mk(transport, retries=3, timeout_secs=240, total_deadline_secs=420)
    import peri.analyst as mod
    real = mod.time.monotonic
    try:
        mod.time.monotonic = lambda: clock[0]
        with pytest.raises(AnalystError):
            a.decide(BUNDLE)
    finally:
        mod.time.monotonic = real
    assert timeouts[0] == 240
    assert all(t <= 240 for t in timeouts if t is not None)


def test_a_min_attempt_floor_exists_so_we_never_start_a_doomed_call():
    assert MIN_ATTEMPT_SECS > 0


def test_a_fast_success_still_works_untouched():
    a = mk(lambda payload: DECISION)
    res = a.decide(BUNDLE)
    assert res.decision.actions == []


def test_the_ledger_records_which_brain_actually_answered():
    """A retry can swap to the fallback. Filing that decision under the
    configured model hides who decided — decision #552 on 2026-08-31 was
    answered by deepseek after qwen timed out, and recorded as qwen3.8-max."""
    calls = []

    def transport(payload):
        calls.append(payload["model"])
        if len(calls) < 2:
            raise httpx.ReadTimeout("slow")
        return DECISION

    res = mk(transport).decide(BUNDLE)
    assert res.model == "fast-model"


def test_a_first_try_success_records_the_primary_model():
    res = mk(lambda p: DECISION).decide(BUNDLE)
    assert res.model == "m"
