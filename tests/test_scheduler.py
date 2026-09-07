"""The async shell: wakes must cross threads, and the schedule must not free-run.

Both tests measure OBSERVED cycle timing rather than inspecting internals, so
they fail against the shapes they were written for.
"""

import asyncio
import threading
import time

from tests.test_engine import mk


def _driver(tmp_path, *, cadence: float, cycle_secs: float, stop_after: int):
    """An engine whose cycle takes `cycle_secs` and which stops after N cycles.
    Returns (engine, starts) where `starts` collects each cycle's start time."""
    eng, _state, _notes, _ = mk(tmp_path, [{"actions": []}])
    eng.next_cycle_secs = lambda: cadence
    starts: list[tuple[str, float]] = []

    def body(trigger):
        starts.append((trigger, time.monotonic()))
        if cycle_secs:
            time.sleep(cycle_secs)
        if len(starts) >= stop_after:
            raise KeyboardInterrupt

    eng.cycle = body
    eng.startup = lambda: None
    return eng, starts


def _run(eng):
    async def scenario():
        try:
            await eng.run()
        except KeyboardInterrupt:
            pass
    asyncio.run(scenario())


def test_a_wake_from_a_worker_thread_reaches_the_event_loop(tmp_path):
    """PriceWatcher and TelegramControl both poll inside asyncio.to_thread, so
    they call request_wake from a worker thread. asyncio.Event.set() is not
    thread-safe: it resolves the waiter's future via call_soon without waking
    the loop's selector, so the wake was only noticed at the loop's next
    scheduled wakeup — defeating the point of deciding at the START of a move.
    """
    # a 60s cadence: if the wake does not cross threads, nothing runs in time
    eng, starts = _driver(tmp_path, cadence=60.0, cycle_secs=0.0, stop_after=2)

    def waker():
        time.sleep(0.1)
        eng.request_wake("price move: SOL +1.2%")

    threading.Thread(target=waker, daemon=True).start()
    began = time.monotonic()
    _run(eng)

    assert len(starts) == 2
    trigger, at = starts[1]
    assert trigger == "price move: SOL +1.2%"
    assert at - began < 5.0, "the worker-thread wake did not reach the loop"


def test_a_cycle_slower_than_the_cadence_still_leaves_an_idle_gap(tmp_path):
    """next_scheduled was computed BEFORE the cycle body, so a cycle slower than
    the cadence left it already in the past: the timeout became 0 and the daemon
    ran LLM decisions back to back with no idle gap. At cycle_secs_active=300
    against a 180-225s analyst that is the routine case, not the edge case.
    """
    eng, starts = _driver(tmp_path, cadence=0.30, cycle_secs=0.40, stop_after=3)
    _run(eng)

    assert len(starts) == 3
    gaps = [starts[i + 1][1] - starts[i][1] for i in range(len(starts) - 1)]
    # each cycle costs 0.40s; a real gap means start-to-start exceeds that
    assert all(g > 0.40 + 0.20 for g in gaps), gaps


def test_a_wake_triggered_cycle_also_defers_the_next_scheduled_one(tmp_path):
    """A wake late in the cadence used to be followed almost immediately by the
    scheduled cycle — two full analyst decisions back to back on the same tape,
    because only startup/scheduled triggers reset the schedule."""
    eng, starts = _driver(tmp_path, cadence=1.0, cycle_secs=0.0, stop_after=3)

    def waker():
        time.sleep(0.8)               # late in the 1.0s cadence
        eng.request_wake("price move: SOL")

    threading.Thread(target=waker, daemon=True).start()
    _run(eng)

    assert len(starts) == 3
    assert starts[1][0] == "price move: SOL"
    # the cycle after the wake must wait a full cadence, not the 0.2s remnant
    assert starts[2][1] - starts[1][1] > 0.8, starts
