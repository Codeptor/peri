"""Telegram slash commands — the operator's controls, from a phone.

Alerts already flow out; this is the way back in. It long-polls the Bot API and
accepts commands ONLY from `telegram.control_user`, because these move real
money on a live account: anyone else is answered with a refusal and logged.

Nothing here can OPEN a position. Every mutating command either reduces risk
(pause, close) or asks the analyst to think (wake) — opening a trade from a chat
window would bypass the analyst and every gate that exists to protect it.
"""

import time
from typing import Callable, Optional

import httpx

HELP = """peri controls

/status    account, book, and what the engine is doing right now
/book      open positions and resting entries, with levels
/why       the analyst's latest market view and what it did
/memory    the lessons it carries between cycles
/wake      make it decide now instead of at the next cycle
/pause     halt every new entry and cancel resting orders
/resume    let it trade again
/close X   close position X, or cancel a resting entry on X
/help      this

Opening a trade from chat is deliberately not possible."""


def _usd(value) -> str:
    return f"${value:,.2f}" if isinstance(value, (int, float)) else "?"


class TelegramControl:
    """Long-polls for commands and dispatches them at the engine."""

    def __init__(self, engine, bot_token: str, control_user: int,
                 transport: Optional[Callable[[str, dict], dict]] = None,
                 now: Optional[Callable[[], float]] = None):
        self.engine = engine
        self.bot_token = bot_token
        self.control_user = control_user
        self.transport = transport or self._http
        self.now = now or time.time
        self.offset = 0
        self.errors = 0

    # -- transport ---------------------------------------------------------
    def _http(self, method: str, params: dict) -> dict:
        r = httpx.post(f"https://api.telegram.org/bot{self.bot_token}/{method}",
                       json=params, timeout=40)
        r.raise_for_status()
        return r.json()

    def reply(self, chat_id: int, text: str) -> None:
        try:
            self.transport("sendMessage", {"chat_id": chat_id, "text": text[:3900]})
        except Exception as exc:  # noqa: BLE001 — a failed reply never kills the loop
            print(f"[peri] control reply failed: {exc!r}", flush=True)

    # -- polling -----------------------------------------------------------
    def poll_once(self) -> int:
        """Fetch and handle one batch. Returns how many commands were handled."""
        body = self.transport("getUpdates",
                              {"offset": self.offset, "timeout": 25,
                               "allowed_updates": ["message"]})
        handled = 0
        for update in body.get("result", []):
            self.offset = max(self.offset, int(update.get("update_id", 0)) + 1)
            message = update.get("message") or {}
            text = (message.get("text") or "").strip()
            chat = (message.get("chat") or {}).get("id")
            sender = (message.get("from") or {}).get("id")
            if not text.startswith("/") or chat is None:
                continue
            if self.control_user and sender != self.control_user:
                print(f"[peri] control command from unauthorised user {sender}: "
                      f"{text[:40]!r}", flush=True)
                self.reply(chat, "not authorised")
                continue
            handled += 1
            try:
                self.reply(chat, self.handle(text))
            except Exception as exc:  # noqa: BLE001 — report, never die
                self.reply(chat, f"command failed: {exc!r}")
        return handled

    # -- commands ----------------------------------------------------------
    def handle(self, text: str) -> str:
        parts = text.split()
        command = parts[0].split("@")[0].lower()
        arg = parts[1] if len(parts) > 1 else ""
        table = {
            "/start": self.cmd_help, "/help": self.cmd_help,
            "/status": self.cmd_status, "/book": self.cmd_book,
            "/why": self.cmd_why, "/memory": self.cmd_memory,
            "/wake": self.cmd_wake, "/pause": self.cmd_pause,
            "/resume": self.cmd_resume, "/close": self.cmd_close,
        }
        handler = table.get(command)
        if handler is None:
            return f"unknown command {command}\n\n{HELP}"
        return handler(arg) if command == "/close" else handler()

    def cmd_help(self) -> str:
        return HELP

    def cmd_status(self) -> str:
        state, cfg = self.engine.state, self.engine.cfg
        runtime = self.engine.runtime_status()
        snapshot = self.engine._account_snapshot or {}
        positions = state.open_positions()
        mine = sum(1 for p in positions if p.source != "external")
        realized = self.engine.realized_status()
        lines = [
            f"{cfg.mode} on {cfg.hl_network} · {'PAUSED' if state.paused() else 'live'}",
            f"equity {_usd(snapshot.get('equity'))} · "
            f"available {_usd(snapshot.get('available_margin'))}",
            f"positions {mine}/{cfg.risk.max_concurrent} · "
            f"resting {len(state.resting_entries())} · "
            f"entries today {state.entries_today(runtime.get('day') or '')}",
            f"realized {_usd(realized.get('total'))} ({realized.get('scope')})",
            f"engine {runtime.get('phase')} · last {runtime.get('last_trigger') or '-'}",
        ]
        stuck = state.unfinished_action_executions()
        if stuck:
            lines.append(f"BLOCKED: {len(stuck)} unfinished action — /help then clear "
                         "it on the dashboard")
        return "\n".join(lines)

    def cmd_book(self) -> str:
        state = self.engine.state
        lines = []
        for p in state.open_positions():
            lines.append(f"{p.market} {p.side} {p.size:g} @ {p.entry_px:g} "
                         f"SL {p.stop_px} TP {p.tp_px} [{p.source}]")
        for e in state.resting_entries():
            mins = max(0, int((e["expires_ts"] - self.now()) / 60))
            lines.append(f"RESTING {e['market']} {e['side']} @ {e['entry_px']:g} "
                         f"SL {e['stop_px']:g} TP {e['tp_px']:g} · expires {mins}m")
        return "\n".join(lines) if lines else "flat — nothing open, nothing resting"

    def cmd_why(self) -> str:
        recent = self.engine.state.recent_decisions(1)
        if not recent:
            return "no decisions yet"
        d = recent[0]
        actions = d.get("actions") or []
        kinds = ", ".join(a.get("kind", "?") for a in actions) if actions else "no action"
        return (f"[{time.strftime('%H:%MZ', time.gmtime(d['ts']))} {d['trigger']}] "
                f"{kinds}\n\n{d.get('market_view') or '(no view)'}")

    def cmd_memory(self) -> str:
        lessons = self.engine.state.lessons(limit=8)
        if not lessons:
            return "no lessons yet"
        return "\n\n".join(
            f"{'PIN ' if row['pinned'] else ''}{row['text']}" for row in lessons)

    def cmd_wake(self) -> str:
        already = self.engine.request_wake("telegram command")
        return "already queued" if already else "cycle queued"

    def cmd_pause(self) -> str:
        result = self.engine.set_paused(True, "telegram")
        cancelled = result.get("cancelled_entries") or []
        extra = f" · cancelled resting: {', '.join(cancelled)}" if cancelled else ""
        return ("PAUSED — no new entries. Open positions keep their venue "
                f"brackets and stay managed.{extra}")

    def cmd_resume(self) -> str:
        self.engine.set_paused(False, "telegram")
        return "resumed — entries allowed again"

    def cmd_close(self, market: str) -> str:
        if not market:
            return "usage: /close BTC   (or xyz:NVDA)"
        market = market.strip()
        state = self.engine.state
        has_position = state.open_position_for(market) is not None
        has_entry = state.resting_entry_for(market) is not None
        if not (has_position or has_entry):
            return f"nothing open or resting on {market}"
        self.engine.request_close(market, "closed by operator from telegram")
        return (f"closing {market}" if has_position
                else f"cancelling the resting entry on {market}")

    # -- async shell -------------------------------------------------------
    async def run(self) -> None:
        import asyncio
        while True:
            try:
                await asyncio.to_thread(self.poll_once)
                self.errors = 0
            except Exception as exc:  # noqa: BLE001 — a dead poll never kills the daemon
                self.errors += 1
                if self.errors in (1, 5, 25):
                    print(f"[peri] control poll error ({self.errors}): {exc!r}",
                          flush=True)
                await asyncio.sleep(min(60, 2 ** min(self.errors, 5)))
