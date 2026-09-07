"""Regressions from the 2026-08-31 production audit (F2-F7)."""

import asyncio
import threading
import time

import pytest


from peri.app import SHUTDOWN_GRACE_SECS, _run_until_signalled


def _journal(state, execution_id: str) -> dict:
    return [j for j in state.unfinished_action_executions()
            if j["id"] == execution_id][0]


def _status(state, execution_id: str) -> str:
    return state.db.execute("SELECT status FROM action_executions WHERE id=?",
                            (execution_id,)).fetchone()["status"]


def _recover(eng, state, execution_id: str, orders: list) -> None:
    """Drive the open-execution recovery directly: reconcile_action_executions
    fetches fills/orders off a live adapter, and the unit under test is what it
    does with them."""
    from peri.engine import ACTION_ADAPTER

    journal = _journal(state, execution_id)
    action = ACTION_ADAPTER.validate_python(journal["action"])
    eng._reconcile_open_execution(journal, action, [], orders)


# -- F2: a kill between placing an order and recording it must be survivable --

def test_recovery_adopts_an_entry_still_resting_at_the_venue(tmp_path):
    """The oid is only known AFTER place_resting_entry returns and is written to
    the ledger on the next line. A process killed in between leaves an order
    whose id was never recorded, so recovery has to match it on what the journal
    DID capture before submitting: market, side, price and size."""
    from tests.test_engine import mk

    eng, state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    action = {
        "kind": "open", "market": "SOL", "side": "long", "conviction": 0.8,
        "stop": 102.6, "take_profit": 109.3, "leverage": 10,
        "margin_mode": "isolated", "source": "own", "entry": 104.8,
        "rationale": "r", "invalidation": "i", "mirror_msg_id": None,
    }
    expected = {"resting": True, "entry_px": 104.8, "size": 1.34, "notional": 140.432,
                "leverage": 10, "margin_mode": "isolated", "stop_px": 102.6,
                "tp_px": 109.3}
    eid = "recovery-test-1"
    state.create_action_execution(eid, origin="autonomous", kind="open",
                                  proposal_id=None, action=action,
                                  pre_state={"position": None}, expected=expected,
                                  decision_id=None)
    state.update_action_execution(eid, stage="entry_submitted", status="executing",
                                  submission_ts=time.time() - 120)
    orders = [
        {"coin": "SOL", "side": "B", "sz": "1.34", "limitPx": "104.8",
         "reduceOnly": False, "oid": 999111},
        {"coin": "SOL", "side": "A", "sz": "1.34", "limitPx": "102.0",
         "reduceOnly": True, "oid": 999112, "isTrigger": True},
    ]

    _recover(eng, state, eid, orders)

    assert state.resting_entry_for("SOL") is not None
    entry = state.resting_entry_for("SOL")
    assert entry["oid"] == 999111 and entry["entry_px"] == 104.8
    assert state.unfinished_action_executions() == []


def test_recovery_still_fails_when_nothing_is_resting(tmp_path):
    """No fill, no position, no order — the entry really did not land."""
    from tests.test_engine import mk

    eng, state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    action = {
        "kind": "open", "market": "SOL", "side": "long", "conviction": 0.8,
        "stop": 102.6, "take_profit": 109.3, "leverage": 10,
        "margin_mode": "isolated", "source": "own", "entry": 104.8,
        "rationale": "r", "invalidation": "i", "mirror_msg_id": None,
    }
    expected = {"resting": True, "entry_px": 104.8, "size": 1.34, "notional": 140.432,
                "leverage": 10, "margin_mode": "isolated", "stop_px": 102.6,
                "tp_px": 109.3}
    state.create_action_execution("gone", origin="autonomous", kind="open",
                                  proposal_id=None, action=action,
                                  pre_state={"position": None}, expected=expected,
                                  decision_id=None)
    state.update_action_execution("gone", stage="entry_submitted", status="executing",
                                  submission_ts=time.time() - 120)

    _recover(eng, state, "gone", [])

    assert state.resting_entry_for("SOL") is None
    assert _status(state, "gone") == "failed"


def test_a_wrong_priced_order_is_not_mistaken_for_ours(tmp_path):
    from tests.test_engine import mk

    eng, state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    action = {
        "kind": "open", "market": "SOL", "side": "long", "conviction": 0.8,
        "stop": 102.6, "take_profit": 109.3, "leverage": 10,
        "margin_mode": "isolated", "source": "own", "entry": 104.8,
        "rationale": "r", "invalidation": "i", "mirror_msg_id": None,
    }
    expected = {"resting": True, "entry_px": 104.8, "size": 1.34, "notional": 140.432,
                "leverage": 10, "margin_mode": "isolated", "stop_px": 102.6,
                "tp_px": 109.3}
    state.create_action_execution("x", origin="autonomous", kind="open",
                                  proposal_id=None, action=action,
                                  pre_state={"position": None}, expected=expected,
                                  decision_id=None)
    state.update_action_execution("x", stage="entry_submitted", status="executing",
                                  submission_ts=time.time() - 120)
    # someone else's order on the same market, different level
    orders = [{"coin": "SOL", "side": "B", "sz": "1.34", "limitPx": "99.0",
               "reduceOnly": False, "oid": 4242}]

    _recover(eng, state, "x", orders)
    assert state.resting_entry_for("SOL") is None
    assert _status(state, "x") == "failed"


# -- F2b: shutdown leaves between trade actions ------------------------------

def test_sigterm_shuts_down_cleanly_instead_of_killing_the_process():
    """systemd sends SIGTERM on every restart. Python's default handler kills
    the process where it stands — including between placing a venue order and
    writing its ledger row. Prove the signal is caught and the daemon returns
    normally, with every task cancelled."""
    import os
    import signal as _signal

    class FakeEngine:
        def __init__(self):
            self._execution_lock = threading.RLock()

    engine = FakeEngine()
    cancelled = []

    async def never_ends(tag):
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            cancelled.append(tag)
            raise

    async def drive():
        task = asyncio.ensure_future(
            _run_until_signalled([never_ends("a"), never_ends("b")], engine,
                                 on_quiesced=lambda: None))
        await asyncio.sleep(0.1)          # let the handlers install
        os.kill(os.getpid(), _signal.SIGTERM)
        await asyncio.wait_for(task, timeout=10)

    # A sentinel so that if the code under test fails to install its handler the
    # signal lands here and fails the test, instead of terminating pytest.
    tripped = []
    previous = _signal.signal(_signal.SIGTERM, lambda *_: tripped.append(1))
    try:
        asyncio.run(drive())
    finally:
        _signal.signal(_signal.SIGTERM, previous)

    assert not tripped, "SIGTERM reached the default handler — the daemon would die"
    assert sorted(cancelled) == ["a", "b"]


def test_shutdown_holds_until_an_in_flight_trade_action_finishes():
    """The execution lock is held across the venue round trip and the ledger
    write, so taking it is what proves no order is half-recorded."""
    import os
    import signal as _signal

    class FakeEngine:
        def __init__(self):
            self._execution_lock = threading.RLock()

    engine = FakeEngine()
    released_at = []
    holding = threading.Event()

    def in_flight_action():
        # an RLock is owned by the thread that took it: acquire and release in
        # the same one, exactly as a real cycle does
        with engine._execution_lock:
            holding.set()
            time.sleep(0.4)
            released_at.append(time.monotonic())

    async def drive():
        threading.Thread(target=in_flight_action, daemon=True).start()
        holding.wait(2)
        task = asyncio.ensure_future(
            _run_until_signalled([], engine, on_quiesced=lambda: None))
        await asyncio.sleep(0.1)
        os.kill(os.getpid(), _signal.SIGTERM)
        await asyncio.wait_for(task, timeout=10)
        return time.monotonic()

    previous = _signal.signal(_signal.SIGTERM, lambda *_: None)
    try:
        finished = asyncio.run(drive())
    finally:
        _signal.signal(_signal.SIGTERM, previous)

    assert released_at, "the in-flight action never released the lock"
    assert finished >= released_at[0], "shutdown did not wait for it"


def test_the_grace_window_is_long_enough_to_outlast_an_order_round_trip():
    """systemd's default TimeoutStopSec is 90s; the grace must fit inside it."""
    assert 15 <= SHUTDOWN_GRACE_SECS <= 45


# -- F5: a bare script cannot move money by accident -------------------------

def _guard(headers: dict):
    from fastapi import HTTPException

    from peri.api import _reject_foreign_origin

    class R:
        def __init__(self, h):
            self.headers = {k.lower(): v for k, v in h.items()}

    try:
        _reject_foreign_origin(R(headers))
        return 200
    except HTTPException as exc:
        return exc.status_code


def test_the_dashboard_and_the_phone_still_pass():
    # measured on 2026-08-31: the periboard rewrite forwards BOTH headers, and a
    # browser fetch to a same-origin page sends sec-fetch-site: same-origin
    assert _guard({"Sec-Fetch-Site": "same-origin"}) == 200
    assert _guard({"Sec-Fetch-Site": "same-origin",
                   "Origin": "http://box.tail31dae8.ts.net:3475"}) == 200
    assert _guard({"Origin": "http://127.0.0.1:3475"}) == 200


def test_a_foreign_origin_is_still_refused():
    assert _guard({"Origin": "https://evil.example"}) == 403
    assert _guard({"Sec-Fetch-Site": "cross-site",
                   "Origin": "https://evil.example"}) == 403


def test_a_bare_post_with_no_provenance_is_refused():
    """The 2026-08-31 audit paused the live trader with exactly this request."""
    assert _guard({}) == 403


def test_a_deliberate_script_says_so_and_is_allowed():
    assert _guard({"X-Peri-Cli": "1"}) == 200


def test_shutdown_does_not_wait_for_a_thinking_analyst():
    """_trade_lock spans the whole cycle, LLM call included. Waiting on it timed
    out at 75s in production on 2026-08-31 and earned a SIGKILL — the exact
    outcome the graceful path exists to avoid. An analyst call interrupted
    mid-thought has placed nothing."""
    import os
    import signal as _signal

    class FakeEngine:
        def __init__(self):
            self._trade_lock = threading.RLock()
            self._execution_lock = threading.RLock()

    engine = FakeEngine()
    engine._trade_lock.acquire()          # a cycle is mid-analyst

    async def drive():
        task = asyncio.ensure_future(
            _run_until_signalled([], engine, on_quiesced=lambda: None))
        await asyncio.sleep(0.1)
        started = time.monotonic()
        os.kill(os.getpid(), _signal.SIGTERM)
        await asyncio.wait_for(task, timeout=10)
        return time.monotonic() - started

    previous = _signal.signal(_signal.SIGTERM, lambda *_: None)
    try:
        elapsed = asyncio.run(drive())
    finally:
        _signal.signal(_signal.SIGTERM, previous)
        engine._trade_lock.release()

    assert elapsed < 5, f"waited {elapsed:.1f}s on a cycle that placed nothing"


def test_execute_holds_the_execution_lock(tmp_path):
    """Every venue round trip plus its ledger write goes through execute(), so
    one lock there is what shutdown can wait on. Probe from ANOTHER thread — an
    RLock is reentrant, so asking from the holding thread proves nothing."""
    from tests.test_engine import mk

    eng, _state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    free_to_others = []

    def spy(*a, **k):
        result = []

        def probe():
            got = eng._execution_lock.acquire(blocking=False)
            result.append(got)
            if got:
                eng._execution_lock.release()

        t = threading.Thread(target=probe)
        t.start()
        t.join(2)
        free_to_others.append(result[0])

    eng._execute_locked = spy
    eng.execute(object(), {}, 0.0, "2026-08-31", frozenset())

    assert free_to_others == [False], "execute() did not hold the execution lock"
    # and it is released afterwards
    assert eng._execution_lock.acquire(blocking=False)
    eng._execution_lock.release()


def test_shutdown_exits_at_the_proven_safe_point():
    """Returning from the coroutine is not enough: asyncio.run then joins the
    default executor, and a to_thread call cannot be interrupted — the analyst
    request and the telegram long-poll both live there. Waiting took 100s in
    production on 2026-08-31 and earned a SIGKILL anyway."""
    import os
    import signal as _signal

    class FakeEngine:
        def __init__(self):
            self._execution_lock = threading.RLock()

    exited = []

    async def drive():
        task = asyncio.ensure_future(
            _run_until_signalled([], FakeEngine(),
                                 on_quiesced=lambda: exited.append(True)))
        await asyncio.sleep(0.1)
        os.kill(os.getpid(), _signal.SIGTERM)
        await asyncio.wait_for(task, timeout=10)

    previous = _signal.signal(_signal.SIGTERM, lambda *_: None)
    try:
        asyncio.run(drive())
    finally:
        _signal.signal(_signal.SIGTERM, previous)

    assert exited == [True], "the daemon never reached its exit point"


def test_a_task_crashing_on_its_own_still_raises_and_does_not_exit():
    """The exit hook must fire on a signal, never on a task blowing up — that
    has to surface so systemd restarts on the failure."""
    class FakeEngine:
        def __init__(self):
            self._execution_lock = threading.RLock()

    exited = []

    async def boom():
        raise RuntimeError("upstream died")

    async def drive():
        await _run_until_signalled([boom()], FakeEngine(),
                                   on_quiesced=lambda: exited.append(True))

    with pytest.raises(RuntimeError, match="upstream died"):
        asyncio.run(drive())
    assert exited == []


# -- re-opening a market you already have resting REPLACES the order ---------
#
# 2026-08-31 07:57Z: eleven cycles in a row correctly did nothing while an
# xyz:CL maker entry rested. Sixteen minutes before it expired the analyst
# proposed the identical entry again — the only verb it had for "that level is
# still right, keep it alive" — and the gate refused it. The prompt showed the
# resting order but never said an order blocks an open, and there was no renew.

def _open(market="SOL", side="long", entry=104.8, stop=102.6, tp=109.3):
    from peri.models import OpenAction

    return OpenAction(kind="open", market=market, side=side, conviction=0.8,
                      stop=stop, take_profit=tp, leverage=10,
                      margin_mode="isolated", source="own", entry=entry,
                      rationale="r", invalidation="i")


def test_the_analyst_is_told_that_re_opening_replaces(tmp_path):
    from peri.analyst import _SYSTEM

    assert "One order per market too" in _SYSTEM
    assert "REPLACES that order" in _SYSTEM


def test_reopening_a_market_we_already_rest_on_is_not_refused(tmp_path):
    """The slot the gate sees occupied IS the order being replaced."""
    from tests.test_engine import mk

    eng, state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    eng._account_snapshot = {"equity": 1000.0, "available_margin": 500.0}
    eng._features_cache["SOL"] = {"hi_24h": 110.0, "lo_24h": 100.0,
                                  "atr15m_pct": 0.2, "range24h_pos": 0.54}
    entry_id = state.add_pending_entry(
        market="SOL", side="long", entry_px=104.8, size=1.34, notional=140.4,
        leverage=10, margin_mode="isolated", stop_px=102.6, tp_px=109.3,
        conviction=0.8, rationale="old", invalidation="old", oid=555,
        decision_id=None, expires_ts=time.time() + 60)

    _canonical, preview = eng._preview_action(
        _open(entry=105.4, tp=111.5), {"SOL": 106.0}, 1000.0, "2026-08-31",
        frozenset({"SOL"}))          # the venue reports our own order resting

    assert preview["replacing_entry_id"] == entry_id
    assert preview["replacing_entry_oid"] == 555


def test_a_venue_order_that_is_not_ours_is_still_refused(tmp_path):
    """Only OUR unfilled maker entry frees the slot. An unknown venue order on
    that market is someone else's and must not be cancelled out from under it."""
    from peri.engine import ProposalError
    from tests.test_engine import mk

    eng, _state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    eng._account_snapshot = {"equity": 1000.0, "available_margin": 500.0}

    with pytest.raises(ProposalError, match="venue order already open"):
        eng._preview_action(_open(), {"SOL": 106.0}, 1000.0, "2026-08-31",
                            frozenset({"SOL"}))


def test_the_old_order_is_withdrawn_before_the_new_one_is_placed(tmp_path):
    """Cancel first, place second — two live entries on one market is the
    doubling-up the venue-order gate exists to stop."""
    from tests.test_engine import mk

    eng, state, notes, _analyst = mk(tmp_path, [{"actions": []}])
    entry_id = state.add_pending_entry(
        market="SOL", side="long", entry_px=104.8, size=1.34, notional=140.4,
        leverage=10, margin_mode="isolated", stop_px=102.6, tp_px=109.3,
        conviction=0.8, rationale="old", invalidation="old", oid=555,
        decision_id=None, expires_ts=time.time() + 60)

    eng._withdraw_replaced_entry(
        {"market": "SOL", "replacing_entry_id": entry_id, "replacing_entry_oid": 555})

    assert state.resting_entry_for("SOL") is None
    row = [r for r in state.recent_pending_entries(5) if r["id"] == entry_id][0]
    assert row["status"] == "settled" and row["outcome"] == "cancelled"
    assert any("replacing the resting entry" in line for line in notes.lines)


def test_nothing_is_withdrawn_when_there_is_no_replacement(tmp_path):
    from tests.test_engine import mk

    eng, state, _notes, _analyst = mk(tmp_path, [{"actions": []}])
    entry_id = state.add_pending_entry(
        market="SOL", side="long", entry_px=104.8, size=1.34, notional=140.4,
        leverage=10, margin_mode="isolated", stop_px=102.6, tp_px=109.3,
        conviction=0.8, rationale="keep", invalidation="keep", oid=555,
        decision_id=None, expires_ts=time.time() + 60)

    eng._withdraw_replaced_entry({"market": "SOL", "replacing_entry_id": None})

    assert state.resting_entry_for("SOL") is not None
    assert state.resting_entry_for("SOL")["id"] == entry_id
