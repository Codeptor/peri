"""Local dashboard API. Binds 127.0.0.1 only; no CORS headers by design — the
dashboard proxies through its own origin.

Mutating routes: wake, pause/resume (which cancel resting venue orders), chat
persistence, confirmation of an immutable server-stored proposal, and lessons
(write/delete). There is no auth — reachability is the control, so this must
stay bound to loopback behind the tailnet."""

import asyncio
from urllib.parse import urlparse
import json
import secrets
from typing import Callable, Optional

from fastapi import FastAPI, HTTPException, Request, WebSocket, WebSocketDisconnect
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel, Field, field_validator

from peri.config import Config
from peri.engine import ProposalError
from peri.state import State


class ChatRequest(BaseModel):
    message: str = Field(min_length=1, max_length=4000)

    @field_validator("message")
    @classmethod
    def nonempty_message(cls, value: str) -> str:
        value = value.strip()
        if not value:
            raise ValueError("message must not be blank")
        return value


StreamEvent = Callable[[str, dict], None]
ChatMessage = Callable[[str, StreamEvent], dict]


class CalendarRequest(BaseModel):
    ts: float
    title: str
    impact: str = "medium"
    scope: Optional[str] = None

    @field_validator("impact")
    @classmethod
    def known_impact(cls, value: str) -> str:
        if value not in ("high", "medium", "low"):
            raise ValueError("impact must be high, medium or low")
        return value

    @field_validator("title")
    @classmethod
    def nonempty_title(cls, value: str) -> str:
        value = value.strip()
        if not value:
            raise ValueError("title must not be empty")
        return value


class LessonRequest(BaseModel):
    text: str
    market: Optional[str] = None

    @field_validator("text")
    @classmethod
    def nonempty_text(cls, value: str) -> str:
        value = value.strip()
        if not value:
            raise ValueError("lesson text must not be empty")
        return value


ALLOWED_ORIGIN_HOSTS = ("127.0.0.1", "localhost", "[::1]")


CLI_HEADER = "x-peri-cli"


def _reject_foreign_origin(request: Request) -> None:
    """`/api/pause` and `/api/wake` take no body and no custom header, so they
    are CORS *simple* requests: any page open in the operator's browser could
    fire one at a live-money trader and cancel its resting orders. The response
    is unreadable cross-origin, but the side effect lands.

    A browser always states where it came from, so a same-origin or allowlisted
    Origin passes and a foreign one is refused. A request with NO provenance at
    all is not a browser — it is a script — and it used to pass silently. On
    2026-08-31 a bare `curl -X POST /api/pause`, meant only to test this guard,
    paused the live trader and cancelled its resting xyz:CL entry. A tool that
    means to move money can say so in one header; an accident cannot."""
    site = request.headers.get("sec-fetch-site")
    if site in ("same-origin", "same-site"):
        return
    origin = request.headers.get("origin")
    if origin is not None:
        host = urlparse(origin).hostname or ""
        if host in ALLOWED_ORIGIN_HOSTS:
            return
        raise HTTPException(status_code=403,
                            detail="cross-origin mutation refused")
    if request.headers.get(CLI_HEADER):
        return
    raise HTTPException(
        status_code=403,
        detail=("mutation without an Origin refused — this endpoint moves real "
                f"money. A deliberate script must send the {CLI_HEADER} header; "
                "the dashboard sends an Origin and is unaffected."))


def _sse(data: dict) -> str:
    return f"data: {json.dumps(data, separators=(',', ':'))}\n\n"


async def _send_ws_json(websocket: WebSocket, payload: dict) -> bool:
    try:
        await websocket.send_json(payload)
    except (WebSocketDisconnect, RuntimeError):
        return False
    return True


def _history_with_proposals(state: State, limit: int,
                            before_id: Optional[int]) -> list[dict]:
    messages = state.chat_history(limit=limit, before_id=before_id)
    proposals = state.trade_proposals(
        [m.get("proposal_id") for m in messages if m.get("proposal_id")])
    for message in messages:
        proposal_id = message.get("proposal_id")
        message["proposal"] = proposals.get(proposal_id) if proposal_id else None
    return messages


def _parse(row: dict) -> dict:
    out = dict(row)
    for key in ("actions_json", "tool_log"):
        if out.get(key):
            try:
                out[key.replace("_json", "")] = json.loads(out[key])
            except json.JSONDecodeError:
                out[key.replace("_json", "")] = []
        else:
            out[key.replace("_json", "")] = []
    out.pop("actions_json", None)
    return out


def build_app(state: State, cfg: Config, version: str = "dev",
              request_wake: Optional[Callable[[], bool]] = None,
              live_snapshot: Optional[Callable[[], dict]] = None,
              runtime_status: Optional[Callable[[], dict]] = None,
              realized_status: Optional[Callable[[], dict]] = None,
              chat_message: Optional[ChatMessage] = None,
              confirm_trade: Optional[Callable[[str], dict]] = None,
              set_paused: Optional[Callable[..., dict]] = None,
              resolve_execution: Optional[Callable[..., dict]] = None,
              bias_snapshot: Optional[Callable[[], dict]] = None) -> FastAPI:
    app = FastAPI(title="peri", docs_url=None, redoc_url=None, openapi_url=None)

    @app.get("/api/status")
    def status():
        d = state.recent_decisions(1)
        positions = state.open_positions()
        realized = (realized_status() if realized_status else {
            "total": state.realized_total(),
            "close_count": state.closed_position_count(),
            "scope": "Peri ledger",
        })
        return {"mode": cfg.mode, "network": cfg.hl_network,
                "model": cfg.analyst_model, "cycle_secs": cfg.analyst.cycle_secs,
                "cycle_secs_quiet": cfg.analyst.quiet_secs(),
                "cycle_secs_active": cfg.analyst.active_secs(),
                "version": version,
                "paused": state.paused(),
                "pause_state": state.pause_state(),
                "resting_entries": state.resting_entries(),
                "last_decision_ts": d[0]["ts"] if d else None,
                "open_positions": len(positions),
                "peri_open_positions": sum(p.source != "external" for p in positions),
                "external_positions": sum(p.source == "external" for p in positions),
                "max_concurrent": cfg.risk.max_concurrent,
                "realized_total": realized["total"],
                "realized_close_count": realized["close_count"],
                "realized_scope": realized["scope"],
                "runtime": runtime_status() if runtime_status else {"phase": "idle"}}

    @app.post("/api/wake")
    async def wake(request: Request):
        _reject_foreign_origin(request)
        if request_wake is None:
            raise HTTPException(status_code=503, detail="engine wake unavailable")
        return {"ok": True, "already_pending": request_wake()}

    @app.post("/api/pause")
    async def pause(request: Request):
        _reject_foreign_origin(request)
        """Operator kill switch: halt every NEW entry and cancel anything
        resting. Open positions keep their venue brackets. Survives restarts."""
        if set_paused is None:
            await asyncio.to_thread(state.set_paused, True, "dashboard")
            return {"ok": True, **state.pause_state()}
        # set_paused takes the adapter lock and makes venue calls: never on the
        # event loop, or the kill switch freezes the dashboard it lives on
        return {"ok": True, **await asyncio.to_thread(set_paused, True, "dashboard")}

    @app.post("/api/resume")
    async def resume(request: Request):
        _reject_foreign_origin(request)
        if set_paused is None:
            await asyncio.to_thread(state.set_paused, False, "dashboard")
            return {"ok": True, **state.pause_state()}
        return {"ok": True, **await asyncio.to_thread(set_paused, False, "dashboard")}

    @app.get("/api/pause")
    def pause_status():
        return state.pause_state()

    @app.get("/api/executions")
    def executions():
        """Unfinished action journals — any one of these blocks every entry."""
        return state.unfinished_action_executions()

    @app.post("/api/executions/{execution_id}/resolve")
    async def resolve_execution_route(execution_id: str):
        if resolve_execution is None:
            raise HTTPException(status_code=503, detail="engine unavailable")
        try:
            return {"ok": True, **await asyncio.to_thread(resolve_execution, execution_id)}
        except Exception as exc:  # noqa: BLE001 — the reason belongs to the operator
            raise HTTPException(status_code=404, detail=str(exc)) from exc

    @app.get("/api/cohort-bias")
    def cohort_bias():
        """Trench positioning as of the last cycle. Served from the engine's
        cache — a dashboard poll must never trigger a vendor call."""
        return bias_snapshot() if bias_snapshot else {
            "cohorts": [], "assets": {}, "fetched_ts": None}

    @app.get("/api/calendar")
    def calendar(days: int = 10):
        return state.upcoming_events(min(max(days, 1), 60) * 86400)

    @app.post("/api/calendar")
    async def add_calendar(request: CalendarRequest):
        event_id = await asyncio.to_thread(
            state.add_calendar_event, request.ts, request.title,
            impact=request.impact, scope=request.scope)
        return {"ok": True, "id": event_id, "duplicate": event_id is None}

    @app.delete("/api/calendar/{event_id}")
    async def forget_calendar(event_id: int):
        if not await asyncio.to_thread(state.forget_calendar_event, event_id):
            raise HTTPException(status_code=404, detail="no such event")
        return {"ok": True}

    @app.get("/api/bias")
    def get_bias():
        return state.bias() or {}

    @app.post("/api/bias")
    async def set_bias(request: LessonRequest):
        """The operator's standing directional view. Advisory: it is rendered
        prominently for the analyst, never enforced as a gate."""
        return {"ok": True, **await asyncio.to_thread(state.set_bias, request.text)}

    @app.delete("/api/bias")
    async def clear_bias():
        await asyncio.to_thread(state.clear_bias)
        return {"ok": True}

    @app.get("/api/notes")
    def notes(limit: int = 20):
        return state.recent_notes(min(max(limit, 1), 50))

    @app.post("/api/notes")
    async def add_note(request: LessonRequest):
        """Operator context with a shelf life — this week's calendar, a heads-up
        about an event — as opposed to a lesson, which is a durable rule.
        Notes age out of the analyst's bundle after 72h; lessons never do."""
        await asyncio.to_thread(state.add_note, request.text)
        return {"ok": True}

    @app.get("/api/lessons")
    def lessons(limit: int = 50):
        return state.lessons(limit=min(max(limit, 1), 200))

    @app.get("/api/performance")
    def performance(days: int = 0):
        days = max(days, 0)
        return state.performance_digest(days * 86400 if days else None)

    @app.post("/api/lessons")
    async def add_lesson(request: LessonRequest):
        """Operator lessons are pinned: they outrank and outlive anything the
        analyst writes for itself."""
        lesson_id = await asyncio.to_thread(
            state.add_lesson, request.text, market=request.market,
            source="operator", pinned=True)
        if lesson_id is None:
            return {"ok": True, "id": None, "duplicate": True}
        return {"ok": True, "id": lesson_id, "duplicate": False}

    @app.delete("/api/lessons/{lesson_id}")
    async def forget_lesson(lesson_id: int):
        if not await asyncio.to_thread(state.forget_lesson, lesson_id):
            raise HTTPException(status_code=404, detail="no such lesson")
        return {"ok": True}

    @app.get("/api/live")
    def live():
        if live_snapshot is None:
            raise HTTPException(status_code=503, detail="live snapshot unavailable")
        return live_snapshot()

    @app.get("/api/chat")
    def chat_history(limit: int = 100, before_id: Optional[int] = None):
        return _history_with_proposals(state, min(max(limit, 1), 200), before_id)

    @app.post("/api/chat")
    async def chat(request: ChatRequest):
        if chat_message is None:
            raise HTTPException(status_code=503, detail="analyst chat unavailable")

        async def ui_message_stream():
            loop = asyncio.get_running_loop()
            events: asyncio.Queue[tuple[str, object]] = asyncio.Queue()
            stream_id = secrets.token_urlsafe(12)

            def emit(kind: str, data: dict) -> None:
                loop.call_soon_threadsafe(events.put_nowait, (kind, data))

            async def run_chat() -> None:
                try:
                    result = await asyncio.to_thread(chat_message, request.message, emit)
                except Exception as exc:  # noqa: BLE001 — stream an explicit failure
                    await events.put(("fatal", exc))
                else:
                    await events.put(("done", result))

            worker = asyncio.create_task(run_chat())
            emitted = ""
            finish_reason = "stop"
            yield _sse({"type": "start", "messageId": stream_id})
            yield _sse({"type": "text-start", "id": stream_id})
            try:
                while True:
                    try:
                        kind, data = await asyncio.wait_for(events.get(), timeout=15.0)
                    except asyncio.TimeoutError:
                        yield ": keep-alive\n\n"
                        continue

                    if kind == "delta":
                        delta = str(data.get("delta", "")) if isinstance(data, dict) else ""
                        if delta:
                            emitted += delta
                            yield _sse({"type": "text-delta", "id": stream_id,
                                        "delta": delta})
                        continue

                    if kind in {"context", "tool", "proposal", "status", "error"}:
                        yield _sse({"type": f"data-{kind}", "data": data,
                                    "transient": kind != "proposal"})
                        continue

                    if kind == "fatal":
                        finish_reason = "error"
                        error_text = f"Analyst chat failed: {data}"
                        yield _sse({"type": "error", "errorText": error_text})
                        break

                    if kind == "done":
                        result = data if isinstance(data, dict) else {}
                        answer = str(result.get("message", {}).get("content", ""))
                        if not emitted:
                            emitted = answer
                            if answer:
                                yield _sse({"type": "text-delta", "id": stream_id,
                                            "delta": answer})
                        elif answer.startswith(emitted):
                            suffix = answer[len(emitted):]
                            if suffix:
                                emitted = answer
                                yield _sse({"type": "text-delta", "id": stream_id,
                                            "delta": suffix})
                        elif answer != emitted:
                            yield _sse({
                                "type": "data-error",
                                "data": {"error": "streamed answer did not match persisted answer"},
                                "transient": True,
                            })
                        yield _sse({"type": "data-result", "data": result})
                        break
            finally:
                if not worker.done():
                    worker.cancel()

            yield _sse({"type": "text-end", "id": stream_id})
            yield _sse({"type": "finish", "finishReason": finish_reason})
            yield "data: [DONE]\n\n"

        return StreamingResponse(
            ui_message_stream(),
            media_type="text/event-stream",
            headers={
                "Cache-Control": "no-cache, no-transform",
                "Connection": "keep-alive",
                "x-vercel-ai-ui-message-stream": "v1",
            },
        )

    @app.post("/api/chat/proposals/{proposal_id}/confirm")
    async def confirm_chat_trade(proposal_id: str):
        if confirm_trade is None:
            raise HTTPException(status_code=503, detail="trade confirmation unavailable")
        try:
            result = await asyncio.to_thread(confirm_trade, proposal_id)
        except ProposalError as exc:
            status_code = 404 if "not found" in str(exc) else 409
            raise HTTPException(status_code=status_code, detail=str(exc)) from exc
        status_code = 409 if result.get("status") in {
            "refused", "failed", "expired", "needs_reconciliation", "manual_review"
        } else 200
        return JSONResponse(result, status_code=status_code)

    @app.websocket("/ws")
    async def stream(websocket: WebSocket):
        await websocket.accept()
        if live_snapshot is None:
            await websocket.close(code=1013, reason="live snapshot unavailable")
            return
        try:
            while True:
                try:
                    snapshot = await asyncio.to_thread(live_snapshot)
                except Exception as exc:  # noqa: BLE001 — retain the socket and last UI snapshot
                    if not await _send_ws_json(
                        websocket, {"type": "error", "error": str(exc)}
                    ):
                        return
                else:
                    if not await _send_ws_json(
                        websocket, {"type": "snapshot", "data": snapshot}
                    ):
                        return
                try:
                    await asyncio.wait_for(websocket.receive_text(), timeout=5.0)
                except asyncio.TimeoutError:
                    continue
        except (WebSocketDisconnect, RuntimeError):
            return

    @app.get("/api/decisions")
    def decisions(limit: int = 50, full: int = 0):
        rows = [_parse(r) for r in state.recent_decisions(min(max(limit, 1), 200))]
        if not full:  # list view: drop the heavy fields
            for r in rows:
                r.pop("prompt", None)
                r["reasoning"] = (r.get("reasoning") or "")[:400]
        return rows

    @app.get("/api/decisions/{decision_id}")
    def decision(decision_id: int):
        r = state.db.execute("SELECT * FROM decisions WHERE id=?",
                             (decision_id,)).fetchone()
        if r is None:
            return JSONResponse({"error": "not found"}, status_code=404)
        return _parse(dict(r))

    @app.get("/api/positions")
    def positions():
        return [p.__dict__ for p in state.open_positions()]

    @app.get("/api/closes")
    def closes(limit: int = 50):
        return state.recent_closes(min(max(limit, 1), 200))

    @app.get("/api/refusals")
    def refusals(limit: int = 50):
        rows = state.db.execute(
            "SELECT * FROM refusals ORDER BY ts DESC LIMIT ?",
            (min(max(limit, 1), 200),)).fetchall()
        return [dict(r) for r in rows]

    @app.get("/api/daily")
    def daily(limit: int = 60):
        rows = state.db.execute(
            "SELECT * FROM daily ORDER BY day DESC LIMIT ?", (min(max(limit, 1), 366),)).fetchall()
        return [dict(r) for r in rows]

    @app.get("/api/news")
    def news(limit: int = 30):
        return state.recent_news(min(max(limit, 1), 100))

    @app.get("/api/tg")
    def tg(limit: int = 50):
        return state.recent_tg(min(max(limit, 1), 200))

    return app


async def serve(state: State, cfg: Config, port: int, version: str,
                request_wake: Optional[Callable[[], bool]] = None,
                live_snapshot: Optional[Callable[[], dict]] = None,
                runtime_status: Optional[Callable[[], dict]] = None,
                realized_status: Optional[Callable[[], dict]] = None,
                chat_message: Optional[ChatMessage] = None,
                confirm_trade: Optional[Callable[[str], dict]] = None,
                set_paused: Optional[Callable[..., dict]] = None,
                resolve_execution: Optional[Callable[..., dict]] = None,
                bias_snapshot: Optional[Callable[[], dict]] = None) -> None:
    import uvicorn
    server = uvicorn.Server(uvicorn.Config(
        build_app(state, cfg, version, request_wake=request_wake,
                  live_snapshot=live_snapshot, runtime_status=runtime_status,
                  realized_status=realized_status, chat_message=chat_message,
                  confirm_trade=confirm_trade, set_paused=set_paused,
                  resolve_execution=resolve_execution,
                  bias_snapshot=bias_snapshot),
        host="127.0.0.1", port=port,
        log_level="warning", access_log=False))
    await server.serve()


def api_port(cfg: Optional[Config] = None) -> int:
    return 7411
