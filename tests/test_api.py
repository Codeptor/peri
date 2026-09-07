import asyncio
import json
import threading

from fastapi.testclient import TestClient
import uvicorn

from peri.api import _send_ws_json, build_app
from peri.state import State
from tests.test_engine import cfg


CLI = {"x-peri-cli": "1"}   # a deliberate script says so


def mk(tmp_path):
    state = State(str(tmp_path / "t.db"))
    return state, TestClient(build_app(state, cfg(), version="test"))


def test_status_and_empty_collections(tmp_path):
    state, c = mk(tmp_path)
    s = c.get("/api/status").json()
    assert s["mode"] == "dry" and s["open_positions"] == 0
    assert s["peri_open_positions"] == 0 and s["external_positions"] == 0
    assert s["max_concurrent"] == 3
    for ep in ("/api/decisions", "/api/positions", "/api/closes",
               "/api/refusals", "/api/daily", "/api/news", "/api/tg"):
        r = c.get(ep)
        assert r.status_code == 200 and r.json() == []


def test_status_includes_live_engine_phase(tmp_path):
    state = State(str(tmp_path / "t.db"))
    runtime = {"phase": "analyst", "trigger": "manual dashboard", "started_ts": 12.0}
    realized = {"total": 4.67, "close_count": 3, "scope": "recent venue fills"}
    c = TestClient(build_app(
        state, cfg(), version="test", runtime_status=lambda: runtime,
        realized_status=lambda: realized,
    ))

    status = c.get("/api/status").json()
    assert status["runtime"] == runtime
    assert status["realized_total"] == 4.67
    assert status["realized_close_count"] == 3
    assert status["realized_scope"] == "recent venue fills"


def test_manual_wake_is_explicit_and_idempotent(tmp_path):
    state = State(str(tmp_path / "t.db"))
    pending = False

    def request_wake():
        nonlocal pending
        already_pending = pending
        pending = True
        return already_pending

    c = TestClient(build_app(state, cfg(), version="test", request_wake=request_wake))

    assert c.post("/api/wake", headers=CLI).json() == {"ok": True, "already_pending": False}
    assert c.post("/api/wake", headers=CLI).json() == {"ok": True, "already_pending": True}


def test_manual_wake_unavailable_without_engine_callback(tmp_path):
    _, c = mk(tmp_path)
    assert c.post("/api/wake", headers=CLI).status_code == 503


def test_live_snapshot_unavailable_without_engine_callback(tmp_path):
    _, c = mk(tmp_path)
    assert c.get("/api/live").status_code == 503


def test_live_snapshot_comes_from_engine_callback(tmp_path):
    state = State(str(tmp_path / "t.db"))
    snapshot = {"as_of_ts": 123.0, "orders": [{"oid": 9}], "positions": []}
    c = TestClient(build_app(
        state, cfg(), version="test", live_snapshot=lambda: snapshot
    ))

    assert c.get("/api/live").json() == snapshot


def test_websocket_stream_sends_complete_live_snapshot(tmp_path):
    state = State(str(tmp_path / "t.db"))
    snapshot = {"as_of_ts": 123.0, "orders": [{"oid": 9}], "positions": []}
    c = TestClient(build_app(
        state, cfg(), version="test", live_snapshot=lambda: snapshot
    ))

    with c.websocket_connect("/ws") as ws:
        assert ws.receive_json() == {"type": "snapshot", "data": snapshot}


def test_websocket_disconnect_during_failed_snapshot_is_clean(tmp_path):
    state = State(str(tmp_path / "t.db"))
    started = threading.Event()
    release = threading.Event()

    def failing_snapshot():
        started.set()
        assert release.wait(timeout=2)
        raise RuntimeError("venue rate limited")

    c = TestClient(build_app(
        state, cfg(), version="test", live_snapshot=failing_snapshot
    ))

    with c.websocket_connect("/ws") as ws:
        assert started.wait(timeout=2)
        ws.close()
        release.set()


def test_websocket_send_treats_closed_connection_as_disconnect():
    class ClosedSocket:
        async def send_json(self, payload):
            raise RuntimeError("Cannot call send once a close message has been sent")

    assert asyncio.run(_send_ws_json(ClosedSocket(), {"type": "error"})) is False


def test_production_server_has_a_websocket_protocol(tmp_path):
    state = State(str(tmp_path / "t.db"))
    server_config = uvicorn.Config(build_app(state, cfg()), ws="auto")

    server_config.load()

    assert server_config.ws_protocol_class is not None


def test_decision_trace_roundtrip(tmp_path):
    state, c = mk(tmp_path)
    actions = json.dumps([{"kind": "close", "market": "BTC", "rationale": "done"}])
    tool_log = json.dumps([{"tool": "web_search", "args": {"query": "q"},
                            "result": "r"}])
    state.record_decision("scheduled", "the view", actions, "qwen", 1234, "ok",
                          reasoning="long chain of thought", prompt="BIG PROMPT",
                          tool_log=tool_log)
    lst = c.get("/api/decisions").json()
    assert lst[0]["market_view"] == "the view"
    assert lst[0]["actions"][0]["market"] == "BTC"
    assert "prompt" not in lst[0]              # list view is light
    full = c.get(f"/api/decisions/{lst[0]['id']}").json()
    assert full["prompt"] == "BIG PROMPT"
    assert full["reasoning"] == "long chain of thought"
    assert full["tool_log"][0]["tool"] == "web_search"
    assert c.get("/api/decisions/999").status_code == 404


def test_positions_and_closes(tmp_path):
    state, c = mk(tmp_path)
    state.add_position("SOL", "long", 100.0, 5.0, 500.0, 5.0, 97.0, 107.0, 0.8,
                       "own", "momo", "loses 97")
    assert c.get("/api/positions").json()[0]["market"] == "SOL"
    state.close_position(1, "tp", 107.0, 30.0)
    assert c.get("/api/positions").json() == []
    assert c.get("/api/closes").json()[0]["realized_pnl"] == 30.0
    state.record_refusal("BTC", "{}", "cooldown")
    assert c.get("/api/refusals").json()[0]["reason"] == "cooldown"


def test_chat_history_is_paginated_and_includes_proposal(tmp_path):
    state = State(str(tmp_path / "t.db"))
    proposal = state.create_trade_proposal(
        "proposal-1",
        action={"kind": "close", "market": "BTC", "rationale": "thesis done"},
        preview={"kind": "close", "market": "BTC", "full_size": 0.1},
        context_ts=100.0,
        expires_ts=220.0,
        now=100.0,
    )
    first = state.add_chat_message("user", "What is open?", ts=101.0)
    second = state.add_chat_message(
        "assistant", "BTC is open.", proposal_id=proposal["id"], ts=102.0
    )
    state.add_chat_message("user", "Why?", ts=103.0)
    c = TestClient(build_app(state, cfg(), version="test"))

    page = c.get("/api/chat", params={"limit": 2}).json()
    assert [m["content"] for m in page] == ["BTC is open.", "Why?"]
    assert page[0]["proposal"]["id"] == "proposal-1"
    older = c.get("/api/chat", params={"limit": 2, "before_id": second["id"]}).json()
    assert [m["id"] for m in older] == [first["id"]]


def test_chat_history_allows_full_two_hundred_message_page(tmp_path):
    state = State(str(tmp_path / "t.db"))
    for index in range(150):
        state.add_chat_message("user", f"message {index}")
    c = TestClient(build_app(state, cfg(), version="test"))

    page = c.get("/api/chat", params={"limit": 150}).json()

    assert len(page) == 150
    assert page[0]["content"] == "message 0"
    assert page[-1]["content"] == "message 149"


def test_chat_stream_uses_vercel_ui_protocol_off_request_thread(tmp_path):
    state = State(str(tmp_path / "t.db"))
    request_thread = threading.get_ident()
    worker_threads = []

    def chat_message(message, emit):
        worker_threads.append(threading.get_ident())
        assert message == "Assess NVDA"
        emit("context", {"as_of_ts": 123.0, "sources": ["venue"]})
        emit("delta", {"delta": "NVDA "})
        emit("delta", {"delta": "is live."})
        row = state.add_chat_message(
            "assistant", "NVDA is live.", context_ts=123.0
        )
        return {"message": row, "proposal": None}

    c = TestClient(build_app(
        state, cfg(), version="test", chat_message=chat_message
    ))
    response = c.post("/api/chat", json={"message": "Assess NVDA"})

    assert response.status_code == 200
    assert response.headers["x-vercel-ai-ui-message-stream"] == "v1"
    assert response.headers["content-type"].startswith("text/event-stream")
    assert worker_threads and worker_threads[0] != request_thread
    chunks = [
        json.loads(line.removeprefix("data: "))
        for line in response.text.splitlines()
        if line.startswith("data: {")
    ]
    assert chunks[0]["type"] == "start"
    assert {chunk["type"] for chunk in chunks} >= {
        "text-start", "text-delta", "text-end", "data-context", "data-result", "finish"
    }
    assert "".join(
        chunk["delta"] for chunk in chunks if chunk["type"] == "text-delta"
    ) == "NVDA is live."
    assert response.text.rstrip().endswith("data: [DONE]")


def test_chat_stream_falls_back_to_final_answer_when_transport_has_no_deltas(tmp_path):
    state = State(str(tmp_path / "t.db"))

    def chat_message(message, emit):
        row = state.add_chat_message("assistant", f"Answer: {message}")
        return {"message": row, "proposal": None}

    c = TestClient(build_app(
        state, cfg(), version="test", chat_message=chat_message
    ))
    response = c.post("/api/chat", json={"message": "hello"})

    assert '"delta":"Answer: hello"' in response.text


def test_chat_validates_message_and_requires_engine_callback(tmp_path):
    state, c = mk(tmp_path)
    assert c.post("/api/chat", json={"message": "hello"}).status_code == 503
    assert c.post("/api/chat", json={"message": "   "}).status_code == 422
    assert c.post("/api/chat", json={"message": "x" * 4001}).status_code == 422
    assert c.post("/api/chat/proposals/nope/confirm").status_code == 503


def test_chat_confirmation_maps_terminal_refusal_to_conflict(tmp_path):
    state = State(str(tmp_path / "t.db"))
    seen = []

    def confirm(proposal_id):
        seen.append(proposal_id)
        return {"id": proposal_id, "status": "refused", "result": {
            "reason": "mark moved"
        }}

    c = TestClient(build_app(
        state, cfg(), version="test", confirm_trade=confirm
    ))
    response = c.post("/api/chat/proposals/p-1/confirm")

    assert response.status_code == 409
    assert response.json()["result"]["reason"] == "mark moved"
    assert seen == ["p-1"]


# -- operator pause (the dashboard kill switch) ---------------------------
def test_pause_and_resume_round_trip_without_an_engine(tmp_path):
    state, c = mk(tmp_path)
    assert c.get("/api/status").json()["paused"] is False
    assert c.post("/api/pause", headers=CLI).json()["paused"] is True
    assert state.paused() is True
    assert c.get("/api/status").json()["paused"] is True
    assert c.get("/api/pause").json()["paused"] is True
    assert c.post("/api/resume", headers=CLI).json()["paused"] is False
    assert state.paused() is False


def test_pause_delegates_to_the_engine_when_wired(tmp_path):
    state = State(str(tmp_path / "t.db"))
    calls = []

    def set_paused(paused, who="dashboard"):
        calls.append((paused, who))
        state.set_paused(paused, who)
        return {"paused": paused, "changed": True, "cancelled_entries": ["SOL"]}

    c = TestClient(build_app(state, cfg(), version="test", set_paused=set_paused))
    body = c.post("/api/pause", headers=CLI).json()
    assert body["cancelled_entries"] == ["SOL"]
    assert calls == [(True, "dashboard")]
    c.post("/api/resume", headers=CLI)
    assert calls[-1] == (False, "dashboard")


def test_status_exposes_resting_entries_and_the_adaptive_cadence(tmp_path):
    state, c = mk(tmp_path)
    state.add_pending_entry(
        market="SOL", side="long", entry_px=97.5, size=1.0, notional=97.5,
        leverage=10.0, margin_mode="isolated", stop_px=94.0, tp_px=108.0,
        conviction=0.8, rationale="pullback", invalidation="loses 94",
        oid=42, decision_id=None, expires_ts=9e9)
    s = c.get("/api/status").json()
    assert len(s["resting_entries"]) == 1
    assert s["resting_entries"][0]["market"] == "SOL"
    assert s["cycle_secs_quiet"] == 900 and s["cycle_secs_active"] == 900


# -- memory over the API ---------------------------------------------------
def test_lessons_endpoint_round_trip(tmp_path):
    state, c = mk(tmp_path)
    assert c.get("/api/lessons").json() == []
    body = c.post("/api/lessons", json={
        "text": "Never trade the first 30 minutes of the US session.",
        "market": None}).json()
    assert body["ok"] is True and body["duplicate"] is False
    lessons = c.get("/api/lessons").json()
    assert len(lessons) == 1
    assert lessons[0]["source"] == "operator" and lessons[0]["pinned"] == 1
    # the operator writing the same thing twice is not two lessons
    assert c.post("/api/lessons", json={
        "text": "never trade the FIRST 30 minutes of the us session."}).json()["duplicate"]
    assert c.delete(f"/api/lessons/{lessons[0]['id']}").json() == {"ok": True}
    assert c.get("/api/lessons").json() == []
    assert c.delete("/api/lessons/424242").status_code == 404
    assert c.post("/api/lessons", json={"text": "   "}).status_code == 422


def test_performance_endpoint_reports_the_ledger(tmp_path):
    state, c = mk(tmp_path)
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 98.0, 110.0,
                             0.8, "own", entry_context={"style": "resting",
                                                        "range_pos": 0.4,
                                                        "atr_pct": 0.5,
                                                        "trigger": "price move"})
    state.close_position(pos.id, "tp", 110.0, 9.4)
    body = c.get("/api/performance").json()
    assert body["overall"]["n"] == 1 and body["overall"]["pnl"] == 9.4
    assert body["by_entry_style"]["resting"]["win_rate"] == 1.0


def test_a_cross_origin_page_cannot_pause_or_wake_a_live_trader(tmp_path):
    """pause/resume/wake take no body and no custom header, so they are CORS
    simple requests: any page in the operator's browser could fire one."""
    state, c = mk(tmp_path)
    evil = {"origin": "https://evil.example", "sec-fetch-site": "cross-site"}
    assert c.post("/api/pause", headers=evil).status_code == 403
    assert c.post("/api/resume", headers=evil).status_code == 403
    assert c.post("/api/wake", headers=evil).status_code == 403
    assert state.paused() is False

    # the dashboard's own origin still works
    ok = {"origin": "http://localhost:3475", "sec-fetch-site": "same-origin"}
    assert c.post("/api/pause", headers=ok).json()["paused"] is True

    # ...and so does the phone: measured 2026-08-31, the periboard rewrite
    # forwards both headers, and a browser fetch to a same-origin page sends
    # sec-fetch-site: same-origin even though the tailnet host is not allowlisted
    phone = {"origin": "http://box.tail31dae8.ts.net:3475",
             "sec-fetch-site": "same-origin"}
    assert c.post("/api/resume", headers=phone).json()["paused"] is False

    # A request with no provenance at all is NOT a browser, it is a script, and
    # it used to pass silently. On 2026-08-31 a bare `curl -X POST /api/pause`
    # meant only to test this guard paused the live trader and cancelled its
    # resting xyz:CL entry.
    assert c.post("/api/pause").status_code == 403
    assert state.paused() is False
    assert c.post("/api/pause", headers=CLI).json()["paused"] is True
    c.post("/api/resume", headers=CLI)


def test_list_endpoints_reject_a_negative_limit(tmp_path):
    """SQLite reads a negative LIMIT as *no* limit, so ?limit=-1 dumped every
    decision row — analyst prompts included."""
    state, c = mk(tmp_path)
    for i in range(5):
        state.record_decision("scheduled", f"view {i}", "[]", "m", 0, "ok",
                              prompt="x" * 100)
    assert len(c.get("/api/decisions?limit=-1").json()) == 1
    assert len(c.get("/api/decisions?limit=3").json()) == 3
    assert c.get("/api/performance?days=-5").json()["overall"]["n"] == 0


def test_chat_history_batches_its_proposal_lookups(tmp_path):
    state, c = mk(tmp_path)
    queries = {"n": 0}
    original = state.trade_proposal

    def counting(pid):
        queries["n"] += 1
        return original(pid)

    state.trade_proposal = counting
    for i in range(10):
        state.add_chat_message("assistant", f"msg {i}", proposal_id=f"p{i}")
    body = c.get("/api/chat?limit=20").json()
    assert len(body) == 10
    assert queries["n"] == 0            # one batched query, not one per message


def test_operator_notes_round_trip_and_age_out(tmp_path):
    """Notes carry time-bound context (this week's calendar); lessons carry
    durable rules. Notes leave the bundle after 72h, lessons never do."""
    state, c = mk(tmp_path)
    assert c.get("/api/notes").json() == []
    assert c.post("/api/notes", json={"text": "Fri 04 Sep 08:30 ET: August jobs report."}).json()["ok"]
    body = c.get("/api/notes").json()
    assert len(body) == 1 and "jobs report" in body[0]["text"]
    state.db.execute("UPDATE operator_notes SET ts = ts - ?", (80 * 3600,))
    state.db.commit()
    assert state.recent_notes(5) == []          # aged out of the analyst's view
    assert c.post("/api/notes", json={"text": "  "}).status_code == 422


def test_calendar_and_bias_over_the_api(tmp_path):
    import time as _t
    state, c = mk(tmp_path)
    assert c.get("/api/calendar").json() == []
    nfp = _t.time() + 4 * 3600
    body = c.post("/api/calendar", json={
        "ts": nfp, "title": "August jobs report", "impact": "high",
        "scope": "macro"}).json()
    assert body["ok"] and body["duplicate"] is False
    assert c.post("/api/calendar", json={
        "ts": nfp, "title": "august jobs report", "impact": "high"}).json()["duplicate"]
    assert c.post("/api/calendar", json={
        "ts": nfp, "title": "x", "impact": "enormous"}).status_code == 422

    events = c.get("/api/calendar").json()
    assert len(events) == 1 and events[0]["impact"] == "high"
    assert c.delete(f"/api/calendar/{events[0]['id']}").json() == {"ok": True}
    assert c.delete("/api/calendar/9999").status_code == 404

    assert c.get("/api/bias").json() == {}
    c.post("/api/bias", json={"text": "Risk-off into Friday; favour shorts on retests."})
    assert "favour shorts" in c.get("/api/bias").json()["text"]
    c.delete("/api/bias")
    assert c.get("/api/bias").json() == {}


def test_cohort_bias_is_served_from_cache_not_a_vendor_call(tmp_path):
    """A dashboard poll must never trigger a Trench request — the cycle owns the
    network."""
    state = State(str(tmp_path / "t.db"))
    calls = {"n": 0}

    def snapshot():
        calls["n"] += 1
        return {"cohorts": [{"id": "rekt", "label": "Rekt", "long_pct": 31.0,
                             "sentiment": "Very Bearish", "traders": 269,
                             "range": "-$1M+"}],
                "total_traders": 121854,
                "assets": {"HYPE": {"smart_long_pct": 74.0, "crowd_long_pct": 32.0,
                                    "divergence": 42.0}},
                "fetched_ts": 1788150000.0}

    c = TestClient(build_app(state, cfg(), version="test", bias_snapshot=snapshot))
    body = c.get("/api/cohort-bias").json()
    assert body["total_traders"] == 121854
    assert body["assets"]["HYPE"]["divergence"] == 42.0
    assert calls["n"] == 1

    bare = TestClient(build_app(state, cfg(), version="test"))
    assert bare.get("/api/cohort-bias").json()["cohorts"] == []
