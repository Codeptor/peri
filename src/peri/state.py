"""SQLite ledger (peri.db): positions, decisions, refusals, fills, tg messages,
daily equity marks, cooldowns. WAL mode; every write commits.

Concurrency: the daemon touches the ledger from several threads (engine worker,
API event loop, telethon loop). sqlite cursors are NOT safe across threads on a
shared connection (observed live: an aggregate fetchone() returning None), so
each thread gets its own connection to the same WAL file."""

import json
import sqlite3
import threading
import time
from dataclasses import dataclass
from typing import Optional

_TERMINAL_PROPOSAL_STATUSES = frozenset(
    {"executed", "refused", "failed", "expired", "cancelled"}
)
_TERMINAL_ACTION_STATUSES = frozenset({"executed", "failed", "refused", "cancelled"})


def _json(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def _decoded(value: Optional[str], fallback):
    return fallback if value is None else json.loads(value)


_SCHEMA = """
CREATE TABLE IF NOT EXISTS positions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market TEXT NOT NULL, side TEXT NOT NULL,
    entry_px REAL NOT NULL, size REAL NOT NULL, notional REAL NOT NULL,
    leverage REAL NOT NULL,
    margin_mode TEXT NOT NULL DEFAULT 'unknown',
    stop_px REAL, tp_px REAL, init_stop_px REAL,
    entry_style TEXT, entry_range_pos REAL, entry_atr_pct REAL, entry_trigger TEXT,
    entry_fee REAL NOT NULL DEFAULT 0,
    peak_px REAL,
    conviction REAL, source TEXT NOT NULL,
    rationale TEXT, invalidation TEXT,
    status TEXT NOT NULL DEFAULT 'open',
    opened_ts REAL NOT NULL, closed_ts REAL,
    close_reason TEXT, close_px REAL, realized_pnl REAL);
CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL, trigger TEXT NOT NULL,
    market_view TEXT, actions_json TEXT NOT NULL,
    model TEXT, latency_ms INTEGER, status TEXT NOT NULL,
    reasoning TEXT, prompt TEXT, tool_log TEXT);
CREATE TABLE IF NOT EXISTS refusals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL, market TEXT, action_json TEXT NOT NULL, reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS fills_seen (tid TEXT PRIMARY KEY, ts REAL NOT NULL);
CREATE TABLE IF NOT EXISTS runtime_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tg_messages (
    msg_id INTEGER PRIMARY KEY, ts REAL NOT NULL,
    sender TEXT, text TEXT NOT NULL, is_caller INTEGER NOT NULL,
    image_desc TEXT);
CREATE TABLE IF NOT EXISTS daily (
    day TEXT PRIMARY KEY, open_equity REAL NOT NULL,
    entries INTEGER NOT NULL DEFAULT 0, kill_tripped INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS cooldowns (
    market TEXT PRIMARY KEY, until_ts REAL NOT NULL, why TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS news_items (
    source TEXT NOT NULL, msg_id INTEGER NOT NULL,
    ts REAL NOT NULL, text TEXT NOT NULL,
    PRIMARY KEY (source, msg_id));
CREATE TABLE IF NOT EXISTS operator_notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL, text TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chat_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    context_ts REAL,
    proposal_id TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}');
CREATE INDEX IF NOT EXISTS idx_chat_messages_id ON chat_messages(id);
CREATE TABLE IF NOT EXISTS trade_proposals (
    id TEXT PRIMARY KEY,
    created_ts REAL NOT NULL,
    context_ts REAL NOT NULL,
    expires_ts REAL NOT NULL,
    action_json TEXT NOT NULL,
    preview_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    claimed_ts REAL,
    finished_ts REAL,
    result_json TEXT);
CREATE INDEX IF NOT EXISTS idx_trade_proposals_status
    ON trade_proposals(status, created_ts);
CREATE TABLE IF NOT EXISTS action_executions (
    id TEXT PRIMARY KEY,
    created_ts REAL NOT NULL,
    updated_ts REAL NOT NULL,
    origin TEXT NOT NULL,
    kind TEXT NOT NULL,
    proposal_id TEXT,
    decision_id INTEGER,
    action_json TEXT NOT NULL,
    pre_state_json TEXT NOT NULL,
    expected_json TEXT NOT NULL,
    stage TEXT NOT NULL DEFAULT 'prepared',
    status TEXT NOT NULL DEFAULT 'prepared',
    submission_ts REAL,
    response_ts REAL,
    fill_id TEXT,
    fill_px REAL,
    result_json TEXT);
CREATE INDEX IF NOT EXISTS idx_action_executions_status
    ON action_executions(status, created_ts);
CREATE TABLE IF NOT EXISTS calendar_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    title TEXT NOT NULL,
    impact TEXT NOT NULL DEFAULT 'medium',
    scope TEXT,
    created_ts REAL NOT NULL,
    norm TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS idx_calendar_norm ON calendar_events(norm);
CREATE INDEX IF NOT EXISTS idx_calendar_ts ON calendar_events(ts);
CREATE TABLE IF NOT EXISTS lessons (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts REAL NOT NULL,
    market TEXT,
    text TEXT NOT NULL,
    source TEXT NOT NULL DEFAULT 'analyst',
    decision_id INTEGER,
    pinned INTEGER NOT NULL DEFAULT 0,
    norm TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS idx_lessons_norm ON lessons(norm);
CREATE TABLE IF NOT EXISTS pending_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    market TEXT NOT NULL, side TEXT NOT NULL,
    entry_px REAL NOT NULL, size REAL NOT NULL, notional REAL NOT NULL,
    leverage REAL NOT NULL, margin_mode TEXT NOT NULL,
    stop_px REAL NOT NULL, tp_px REAL NOT NULL,
    conviction REAL, rationale TEXT, invalidation TEXT,
    oid INTEGER, decision_id INTEGER, entry_context_json TEXT,
    placed_ts REAL NOT NULL, expires_ts REAL NOT NULL,
    status TEXT NOT NULL DEFAULT 'resting',
    settled_ts REAL, outcome TEXT);
CREATE INDEX IF NOT EXISTS idx_pending_entries_status
    ON pending_entries(status, placed_ts);
"""


@dataclass
class Position:
    id: int
    market: str
    side: str
    entry_px: float
    size: float
    notional: float
    leverage: float
    margin_mode: str
    stop_px: Optional[float]
    tp_px: Optional[float]
    conviction: Optional[float]
    source: str
    rationale: Optional[str]
    invalidation: Optional[str]
    status: str
    opened_ts: float


class State:
    def __init__(self, db_path: str = "peri.db"):
        self.db_path = db_path
        self._local = threading.local()
        self.db.executescript(_SCHEMA)
        self._migrate()
        self.db.commit()
        self._verify_schema()

    @property
    def db(self) -> sqlite3.Connection:
        conn = getattr(self._local, "conn", None)
        if conn is None:
            conn = sqlite3.connect(self.db_path, timeout=10)
            conn.row_factory = sqlite3.Row
            conn.execute("PRAGMA journal_mode=WAL")
            self._local.conn = conn
        return conn

    def _migrate(self) -> None:
        """Additive column migrations for ledgers created by older builds."""
        cols = {r["name"] for r in self.db.execute("PRAGMA table_info(decisions)")}
        for col in ("reasoning", "prompt", "tool_log"):
            if col not in cols:
                self.db.execute(f"ALTER TABLE decisions ADD COLUMN {col} TEXT")
        position_cols = {
            r["name"] for r in self.db.execute("PRAGMA table_info(positions)")
        }
        if "margin_mode" not in position_cols:
            self.db.execute(
                "ALTER TABLE positions ADD COLUMN margin_mode TEXT NOT NULL DEFAULT 'unknown'"
            )
        if "init_stop_px" not in position_cols:
            # left NULL on purpose for pre-migration rows: stop_px is mutable
            # (breakeven/trailing move it), so copying it would invent a risk
            # unit that was never taken. Those rows are simply excluded from R.
            self.db.execute("ALTER TABLE positions ADD COLUMN init_stop_px REAL")
        for col, decl in (("entry_style", "TEXT"), ("entry_range_pos", "REAL"),
                          ("entry_atr_pct", "REAL"), ("entry_trigger", "TEXT"),
                          ("entry_fee", "REAL NOT NULL DEFAULT 0"),
                          ("peak_px", "REAL")):
            if col not in position_cols:
                self.db.execute(f"ALTER TABLE positions ADD COLUMN {col} {decl}")
        tg_cols = {r["name"] for r in self.db.execute("PRAGMA table_info(tg_messages)")}
        if "image_desc" not in tg_cols:
            # what the eye read off an attached picture, kept apart from the
            # caption so an edited caption never re-runs the vision call
            self.db.execute("ALTER TABLE tg_messages ADD COLUMN image_desc TEXT")
        entry_cols = {
            r["name"] for r in self.db.execute("PRAGMA table_info(pending_entries)")
        }
        if "entry_context_json" not in entry_cols:
            self.db.execute(
                "ALTER TABLE pending_entries ADD COLUMN entry_context_json TEXT")
        self.db.execute(
            "INSERT OR IGNORE INTO runtime_meta (key,value)"
            " SELECT 'fill_history_initialized','1'"
            " WHERE EXISTS (SELECT 1 FROM fills_seen LIMIT 1)"
        )

    def _verify_schema(self) -> None:
        """Fail at BOOT if any table drifts from _SCHEMA.

        CREATE TABLE IF NOT EXISTS silently does nothing to an existing table,
        so a column added to _SCHEMA without a matching ALTER is invisible until
        the first INSERT that needs it. On 2026-08-29 that INSERT was the one
        recording a resting entry — the order was already live at the venue, and
        the ledger row that would have managed it never existed. A missing
        column must stop the daemon starting, not surface mid-trade."""
        import sqlite3 as _sqlite3
        reference = _sqlite3.connect(":memory:")
        reference.row_factory = _sqlite3.Row
        reference.executescript(_SCHEMA)
        drift = {}
        for row in reference.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"):
            table = row["name"]
            want = {r["name"] for r in reference.execute(f"PRAGMA table_info({table})")}
            have = {r["name"] for r in self.db.execute(f"PRAGMA table_info({table})")}
            if want - have:
                drift[table] = sorted(want - have)
        reference.close()
        if drift:
            raise RuntimeError(
                f"ledger schema is behind the code: {drift}. Add the ALTER TABLE "
                "to State._migrate — CREATE TABLE IF NOT EXISTS will not do it.")

    # -- positions ---------------------------------------------------------
    def _pos(self, r: sqlite3.Row) -> Position:
        return Position(r["id"], r["market"], r["side"], r["entry_px"], r["size"],
                        r["notional"], r["leverage"], r["margin_mode"], r["stop_px"],
                        r["tp_px"], r["conviction"], r["source"], r["rationale"],
                        r["invalidation"], r["status"], r["opened_ts"])

    def add_position(self, market: str, side: str, entry_px: float, size: float,
                     notional: float, leverage: float, stop_px: Optional[float],
                     tp_px: Optional[float], conviction: Optional[float], source: str,
                     rationale: Optional[str] = None,
                     invalidation: Optional[str] = None,
                     margin_mode: str = "unknown",
                     entry_context: Optional[dict] = None) -> Position:
        if margin_mode not in {"cross", "isolated", "unknown"}:
            raise ValueError(f"invalid margin mode: {margin_mode!r}")
        cur = self.db.execute(
            "INSERT INTO positions (market,side,entry_px,size,notional,leverage,margin_mode,"
            "stop_px,tp_px,init_stop_px,entry_style,entry_range_pos,entry_atr_pct,"
            "entry_trigger,conviction,source,rationale,invalidation,status,opened_ts)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'open',?)",
            (market, side, entry_px, size, notional, leverage, margin_mode, stop_px, tp_px,
             stop_px, *self._entry_context(entry_context),
             conviction, source, rationale, invalidation, time.time()))
        self.db.commit()
        r = self.db.execute("SELECT * FROM positions WHERE id=?", (cur.lastrowid,)).fetchone()
        return self._pos(r)

    def open_positions(self) -> list[Position]:
        rows = self.db.execute(
            "SELECT * FROM positions WHERE status='open' ORDER BY opened_ts").fetchall()
        return [self._pos(r) for r in rows]

    def open_position_for(self, market: str) -> Optional[Position]:
        r = self.db.execute(
            "SELECT * FROM positions WHERE status='open' AND market=?"
            " ORDER BY opened_ts DESC LIMIT 1", (market,)).fetchone()
        return self._pos(r) if r else None

    def update_stop(self, pos_id: int, stop_px: float) -> None:
        self.db.execute("UPDATE positions SET stop_px=? WHERE id=?", (stop_px, pos_id))
        self.db.commit()

    def update_peak(self, pos_id: int, px: float) -> float:
        """Advance the high-water mark and return it. Monotonic per side: the
        peak is what the trail measures back from, so it must never retreat."""
        row = self.db.execute("SELECT side, peak_px, entry_px FROM positions WHERE id=?",
                              (pos_id,)).fetchone()
        if row is None:
            return px
        current = row["peak_px"]
        if current is None:
            current = row["entry_px"]
        best = max(current, px) if row["side"] == "long" else min(current, px)
        if best != current:
            self.db.execute("UPDATE positions SET peak_px=? WHERE id=?", (best, pos_id))
            self.db.commit()
        return best

    def update_brackets(self, pos_id: int, stop_px: Optional[float],
                        tp_px: Optional[float]) -> None:
        self.db.execute(
            "UPDATE positions SET stop_px=?, tp_px=? WHERE id=? AND status='open'",
            (stop_px, tp_px, pos_id),
        )
        self.db.commit()

    def update_size(self, pos_id: int, size: float) -> None:
        self.db.execute("UPDATE positions SET size=? WHERE id=?", (size, pos_id))
        self.db.commit()

    def sync_position_snapshot(self, pos_id: int, side: str, entry_px: float,
                               size: float, notional: float, leverage: float,
                               margin_mode: str = "unknown") -> None:
        if margin_mode not in {"cross", "isolated", "unknown"}:
            raise ValueError(f"invalid margin mode: {margin_mode!r}")
        self.db.execute(
            "UPDATE positions SET side=?, entry_px=?, size=?, notional=?, leverage=?,"
            " margin_mode=?"
            " WHERE id=? AND status='open'",
            (side, entry_px, size, notional, leverage, margin_mode, pos_id),
        )
        self.db.commit()

    def mark_position_missing(self, pos_id: int) -> None:
        self.db.execute(
            "UPDATE positions SET status='missing', closed_ts=?, close_reason='venue_missing'"
            " WHERE id=? AND status='open'",
            (time.time(), pos_id),
        )
        self.db.commit()

    def note_position_absent(self, pos_id: int) -> int:
        """Count consecutive venue reads that did not show this position.

        Marking 'missing' is terminal and hides the row from the close fill that
        is usually moments behind it, discarding the realized PnL. One absence
        is not proof."""
        key = f"absent:{pos_id}"
        row = self.db.execute(
            "SELECT value FROM runtime_meta WHERE key=?", (key,)).fetchone()
        count = (int(row["value"]) if row else 0) + 1
        self.db.execute(
            "INSERT INTO runtime_meta (key,value) VALUES (?,?)"
            " ON CONFLICT(key) DO UPDATE SET value=excluded.value", (key, str(count)))
        self.db.commit()
        return count

    def clear_position_absent(self, pos_id: int) -> None:
        self.db.execute("DELETE FROM runtime_meta WHERE key=?", (f"absent:{pos_id}",))
        self.db.commit()

    def close_position(self, pos_id: int, reason: str, close_px: float,
                       realized_pnl: float) -> None:
        self.db.execute(
            "UPDATE positions SET status='closed', closed_ts=?, close_reason=?,"
            " close_px=?, realized_pnl=? WHERE id=?",
            (time.time(), reason, close_px, realized_pnl, pos_id))
        self.db.commit()

    def record_partial_close(self, pos: "Position", size: float, close_px: float,
                             realized_pnl: float, reason: str) -> int:
        """Write a closed row for the tranche that just filled.

        Split take-profits (manage_trade.py) close a position in pieces; only
        the final piece used to be recorded, so every earlier tranche's profit
        was invisible to realized_total() and to the measured record."""
        cur = self.db.execute(
            "INSERT INTO positions (market,side,entry_px,size,notional,leverage,"
            "margin_mode,stop_px,tp_px,init_stop_px,entry_style,entry_range_pos,"
            "entry_atr_pct,entry_trigger,conviction,source,rationale,invalidation,"
            "status,opened_ts,closed_ts,close_reason,close_px,realized_pnl)"
            " SELECT market,side,entry_px,?,?,leverage,margin_mode,stop_px,tp_px,"
            "init_stop_px,entry_style,entry_range_pos,entry_atr_pct,entry_trigger,"
            "conviction,source,rationale,invalidation,'closed',opened_ts,?,?,?,?"
            " FROM positions WHERE id=?",
            (size, close_px * size, time.time(), reason, close_px, realized_pnl, pos.id))
        self.db.commit()
        return int(cur.lastrowid)

    def recent_closes(self, n: int = 8) -> list[dict]:
        rows = self.db.execute(
            "SELECT market, side, entry_px, close_px, size, realized_pnl, close_reason,"
            " conviction, closed_ts FROM positions WHERE status='closed'"
            " ORDER BY closed_ts DESC LIMIT ?", (n,)).fetchall()
        return [dict(r) for r in rows]

    def closed_position_count(self) -> int:
        r = self.db.execute(
            "SELECT COUNT(*) c FROM positions WHERE status='closed'").fetchone()
        return int(r["c"])

    def realized_total(self) -> float:
        r = self.db.execute(
            "SELECT COALESCE(SUM(realized_pnl),0) s FROM positions"
            " WHERE status='closed'").fetchone()
        return float(r["s"])

    # -- decisions / refusals ---------------------------------------------
    def record_decision(self, trigger: str, market_view: str, actions_json: str,
                        model: str, latency_ms: int, status: str,
                        reasoning: str = "", prompt: str = "",
                        tool_log: str = "") -> int:
        cur = self.db.execute(
            "INSERT INTO decisions (ts,trigger,market_view,actions_json,model,latency_ms,"
            "status,reasoning,prompt,tool_log) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (time.time(), trigger, market_view, actions_json, model, latency_ms, status,
             reasoning, prompt, tool_log))
        self.db.commit()
        return int(cur.lastrowid)

    def recent_decisions(self, n: int = 50) -> list[dict]:
        rows = self.db.execute(
            "SELECT * FROM decisions ORDER BY ts DESC LIMIT ?", (n,)).fetchall()
        return [dict(r) for r in rows]

    # -- analyst chat / proposals -----------------------------------------
    @staticmethod
    def _chat_message(r: sqlite3.Row) -> dict:
        out = dict(r)
        out["metadata"] = _decoded(out.pop("metadata_json"), {})
        return out

    def add_chat_message(self, role: str, content: str, *,
                         context_ts: Optional[float] = None,
                         proposal_id: Optional[str] = None,
                         metadata: Optional[dict] = None,
                         ts: Optional[float] = None) -> dict:
        if role not in {"user", "assistant"}:
            raise ValueError(f"invalid chat role: {role!r}")
        cur = self.db.execute(
            "INSERT INTO chat_messages "
            "(ts,role,content,context_ts,proposal_id,metadata_json) VALUES (?,?,?,?,?,?)",
            (time.time() if ts is None else ts, role, content, context_ts, proposal_id,
             _json(metadata or {})),
        )
        self.db.commit()
        row = self.db.execute(
            "SELECT * FROM chat_messages WHERE id=?", (cur.lastrowid,)
        ).fetchone()
        return self._chat_message(row)

    def chat_history(self, limit: int = 100,
                     before_id: Optional[int] = None) -> list[dict]:
        limit = max(1, min(int(limit), 200))
        if before_id is None:
            rows = self.db.execute(
                "SELECT * FROM chat_messages ORDER BY id DESC LIMIT ?", (limit,)
            ).fetchall()
        else:
            rows = self.db.execute(
                "SELECT * FROM chat_messages WHERE id < ? ORDER BY id DESC LIMIT ?",
                (before_id, limit),
            ).fetchall()
        return [self._chat_message(r) for r in reversed(rows)]

    @staticmethod
    def _proposal(r: sqlite3.Row) -> dict:
        out = dict(r)
        out["action"] = _decoded(out.pop("action_json"), {})
        out["preview"] = _decoded(out.pop("preview_json"), {})
        out["result"] = _decoded(out.pop("result_json"), None)
        return out

    def create_trade_proposal(self, proposal_id: str, *, action: dict, preview: dict,
                              context_ts: float, expires_ts: float,
                              now: Optional[float] = None) -> dict:
        created_ts = time.time() if now is None else now
        self.db.execute(
            "INSERT INTO trade_proposals "
            "(id,created_ts,context_ts,expires_ts,action_json,preview_json,status) "
            "VALUES (?,?,?,?,?,?,'pending')",
            (proposal_id, created_ts, context_ts, expires_ts, _json(action), _json(preview)),
        )
        self.db.commit()
        return self.trade_proposal(proposal_id)

    def trade_proposals(self, proposal_ids: list[str]) -> dict[str, dict]:
        """Batch lookup: rendering chat history issued one query per message."""
        ids = [pid for pid in dict.fromkeys(proposal_ids) if pid]
        if not ids:
            return {}
        placeholders = ",".join("?" for _ in ids)
        rows = self.db.execute(
            f"SELECT * FROM trade_proposals WHERE id IN ({placeholders})", ids
        ).fetchall()
        return {row["id"]: self._proposal(row) for row in rows}

    def trade_proposal(self, proposal_id: str) -> Optional[dict]:
        row = self.db.execute(
            "SELECT * FROM trade_proposals WHERE id=?", (proposal_id,)
        ).fetchone()
        return self._proposal(row) if row else None

    def claim_trade_proposal(self, proposal_id: str,
                             now: Optional[float] = None) -> Optional[dict]:
        claimed_ts = time.time() if now is None else now
        conn = self.db
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE trade_proposals SET status='authorizing', claimed_ts=? "
                "WHERE id=? AND status='pending'",
                (claimed_ts, proposal_id),
            )
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return self.trade_proposal(proposal_id) if cur.rowcount == 1 else None

    def finish_trade_proposal(self, proposal_id: str, status: str, result: dict,
                              now: Optional[float] = None) -> dict:
        if status not in _TERMINAL_PROPOSAL_STATUSES | {
            "needs_reconciliation", "manual_review"
        }:
            raise ValueError(f"invalid proposal status: {status!r}")
        terminal_ts = time.time() if now is None else now
        placeholders = ",".join("?" for _ in _TERMINAL_PROPOSAL_STATUSES)
        cur = self.db.execute(
            f"UPDATE trade_proposals SET status=?, finished_ts=?, result_json=? "
            f"WHERE id=? AND status NOT IN ({placeholders})",
            (status, terminal_ts, _json(result), proposal_id,
             *sorted(_TERMINAL_PROPOSAL_STATUSES)),
        )
        self.db.commit()
        if cur.rowcount != 1:
            raise RuntimeError(f"proposal {proposal_id} is missing or terminal")
        return self.trade_proposal(proposal_id)

    # -- durable action execution journal ---------------------------------
    @staticmethod
    def _execution(r: sqlite3.Row) -> dict:
        out = dict(r)
        out["action"] = _decoded(out.pop("action_json"), {})
        out["pre_state"] = _decoded(out.pop("pre_state_json"), {})
        out["expected"] = _decoded(out.pop("expected_json"), {})
        out["result"] = _decoded(out.pop("result_json"), None)
        return out

    def create_action_execution(self, execution_id: str, *, origin: str, kind: str,
                                proposal_id: Optional[str], action: dict,
                                pre_state: dict, expected: dict,
                                decision_id: Optional[int] = None,
                                now: Optional[float] = None) -> dict:
        created_ts = time.time() if now is None else now
        self.db.execute(
            "INSERT INTO action_executions "
            "(id,created_ts,updated_ts,origin,kind,proposal_id,decision_id,action_json,"
            "pre_state_json,expected_json,stage,status) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,'prepared','prepared')",
            (execution_id, created_ts, created_ts, origin, kind, proposal_id, decision_id,
             _json(action), _json(pre_state), _json(expected)),
        )
        self.db.commit()
        return self.action_execution(execution_id)

    def action_execution(self, execution_id: str) -> Optional[dict]:
        row = self.db.execute(
            "SELECT * FROM action_executions WHERE id=?", (execution_id,)
        ).fetchone()
        return self._execution(row) if row else None

    def update_action_execution(
        self,
        execution_id: str,
        *,
        stage: Optional[str] = None,
        status: Optional[str] = None,
        submission_ts: Optional[float] = None,
        response_ts: Optional[float] = None,
        fill_id: Optional[str] = None,
        fill_px: Optional[float] = None,
        result: Optional[dict] = None,
    ) -> dict:
        fields = ["updated_ts=?"]
        values: list = [time.time()]
        for name, value in (
            ("stage", stage),
            ("status", status),
            ("submission_ts", submission_ts),
            ("response_ts", response_ts),
            ("fill_id", fill_id),
            ("fill_px", fill_px),
        ):
            if value is not None:
                fields.append(f"{name}=?")
                values.append(value)
        if result is not None:
            fields.append("result_json=?")
            values.append(_json(result))
        values.append(execution_id)
        cur = self.db.execute(
            f"UPDATE action_executions SET {', '.join(fields)} WHERE id=?", values
        )
        self.db.commit()
        if cur.rowcount != 1:
            raise RuntimeError(f"action execution {execution_id} not found")
        return self.action_execution(execution_id)

    def unfinished_action_executions(self) -> list[dict]:
        placeholders = ",".join("?" for _ in _TERMINAL_ACTION_STATUSES)
        rows = self.db.execute(
            f"SELECT * FROM action_executions WHERE status NOT IN ({placeholders}) "
            "ORDER BY created_ts",
            tuple(sorted(_TERMINAL_ACTION_STATUSES)),
        ).fetchall()
        return [self._execution(r) for r in rows]

    @staticmethod
    def _insert_decision(conn: sqlite3.Connection, decision: dict, ts: float) -> int:
        cur = conn.execute(
            "INSERT INTO decisions (ts,trigger,market_view,actions_json,model,latency_ms,"
            "status,reasoning,prompt,tool_log) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                ts,
                decision["trigger"],
                decision.get("market_view", ""),
                decision["actions_json"],
                decision.get("model", ""),
                decision.get("latency_ms", 0),
                decision.get("status", "ok"),
                decision.get("reasoning", ""),
                decision.get("prompt", ""),
                decision.get("tool_log", ""),
            ),
        )
        return int(cur.lastrowid)

    @classmethod
    def _resolve_decision_id(cls, conn: sqlite3.Connection,
                             decision: Optional[dict],
                             decision_id: Optional[int], ts: float) -> int:
        if (decision is None) == (decision_id is None):
            raise ValueError("provide exactly one of decision or decision_id")
        if decision_id is not None:
            row = conn.execute(
                "SELECT 1 FROM decisions WHERE id=?", (decision_id,)
            ).fetchone()
            if row is None:
                raise RuntimeError(f"decision {decision_id} not found")
            return decision_id
        return cls._insert_decision(conn, decision, ts)

    @staticmethod
    def _finalize_execution(conn: sqlite3.Connection, execution_id: str,
                            decision_id: int, result: dict, ts: float) -> None:
        placeholders = ",".join("?" for _ in _TERMINAL_ACTION_STATUSES)
        cur = conn.execute(
            f"UPDATE action_executions SET updated_ts=?, decision_id=?, stage='done', "
            f"status='executed', result_json=? WHERE id=? "
            f"AND status NOT IN ({placeholders})",
            (ts, decision_id, _json(result), execution_id,
             *sorted(_TERMINAL_ACTION_STATUSES)),
        )
        if cur.rowcount != 1:
            raise RuntimeError(f"action execution {execution_id} is missing or terminal")

    @staticmethod
    def _finalize_proposal(conn: sqlite3.Connection, proposal_id: Optional[str],
                           result: dict, ts: float) -> None:
        if proposal_id is None:
            return
        placeholders = ",".join("?" for _ in _TERMINAL_PROPOSAL_STATUSES)
        cur = conn.execute(
            f"UPDATE trade_proposals SET status='executed', finished_ts=?, result_json=? "
            f"WHERE id=? AND status NOT IN ({placeholders})",
            (ts, _json(result), proposal_id, *sorted(_TERMINAL_PROPOSAL_STATUSES)),
        )
        if cur.rowcount != 1:
            raise RuntimeError(f"proposal {proposal_id} is missing or terminal")

    def finalize_open_action(self, execution_id: str, proposal_id: Optional[str], *,
                             position: dict, result: dict, day: Optional[str],
                             decision: Optional[dict] = None,
                             decision_id: Optional[int] = None) -> dict:
        conn = self.db
        ts = time.time()
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "INSERT INTO positions "
                "(market,side,entry_px,size,notional,leverage,margin_mode,stop_px,tp_px,"
                "init_stop_px,entry_style,entry_range_pos,entry_atr_pct,entry_trigger,"
                "conviction,source,rationale,invalidation,status,opened_ts) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'open',?)",
                (
                    position["market"], position["side"], position["entry_px"],
                    position["size"], position["notional"], position["leverage"],
                    position["margin_mode"], position.get("stop_px"), position.get("tp_px"),
                    position.get("stop_px"),
                    *self._entry_context(position.get("entry_context")),
                    position.get("conviction"), position["source"],
                    position.get("rationale"), position.get("invalidation"), ts,
                ),
            )
            position_id = int(cur.lastrowid)
            decision_id = self._resolve_decision_id(
                conn, decision, decision_id, ts
            )
            self._finalize_execution(conn, execution_id, decision_id, result, ts)
            self._finalize_proposal(conn, proposal_id, result, ts)
            if day is not None:
                conn.execute("UPDATE daily SET entries=entries+1 WHERE day=?", (day,))
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return {"position_id": position_id, "decision_id": decision_id}

    def finalize_recovered_open_action(
        self,
        execution_id: str,
        proposal_id: Optional[str],
        *,
        position_id: int,
        position: dict,
        result: dict,
        day: Optional[str],
        decision: Optional[dict] = None,
        decision_id: Optional[int] = None,
    ) -> dict:
        conn = self.db
        ts = time.time()
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE positions SET side=?, entry_px=?, size=?, notional=?, leverage=?,"
                " margin_mode=?, stop_px=?, tp_px=?, conviction=?, source=?, rationale=?,"
                " invalidation=?, status='open', closed_ts=NULL, close_reason=NULL,"
                " close_px=NULL, realized_pnl=NULL WHERE id=?",
                (
                    position["side"], position["entry_px"], position["size"],
                    position["notional"], position["leverage"], position["margin_mode"],
                    position.get("stop_px"), position.get("tp_px"),
                    position.get("conviction"), position["source"],
                    position.get("rationale"), position.get("invalidation"), position_id,
                ),
            )
            if cur.rowcount != 1:
                raise RuntimeError(f"position {position_id} not found")
            resolved_decision_id = self._resolve_decision_id(
                conn, decision, decision_id, ts
            )
            self._finalize_execution(
                conn, execution_id, resolved_decision_id, result, ts
            )
            self._finalize_proposal(conn, proposal_id, result, ts)
            if day is not None:
                conn.execute("UPDATE daily SET entries=entries+1 WHERE day=?", (day,))
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return {
            "position_id": position_id,
            "decision_id": resolved_decision_id,
        }

    def finalize_close_action(self, execution_id: str, proposal_id: Optional[str], *,
                              position_id: int, close_reason: str, close_px: float,
                              realized_pnl: float, result: dict,
                              decision: Optional[dict] = None,
                              decision_id: Optional[int] = None) -> dict:
        conn = self.db
        ts = time.time()
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE positions SET status='closed', closed_ts=?, close_reason=?,"
                " close_px=?, realized_pnl=? WHERE id=? AND status='open'",
                (ts, close_reason, close_px, realized_pnl, position_id),
            )
            if cur.rowcount != 1:
                raise RuntimeError(f"position {position_id} is missing or not open")
            decision_id = self._resolve_decision_id(
                conn, decision, decision_id, ts
            )
            self._finalize_execution(conn, execution_id, decision_id, result, ts)
            self._finalize_proposal(conn, proposal_id, result, ts)
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return {"position_id": position_id, "decision_id": decision_id}

    def finalize_recovered_close_action(
        self,
        execution_id: str,
        proposal_id: Optional[str],
        *,
        position_id: int,
        close_reason: str,
        close_px: float,
        realized_pnl: float,
        result: dict,
        decision: Optional[dict] = None,
        decision_id: Optional[int] = None,
    ) -> dict:
        conn = self.db
        ts = time.time()
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE positions SET status='closed', closed_ts=COALESCE(closed_ts, ?),"
                " close_reason=?, close_px=?, realized_pnl=? WHERE id=?",
                (ts, close_reason, close_px, realized_pnl, position_id),
            )
            if cur.rowcount != 1:
                raise RuntimeError(f"position {position_id} not found")
            resolved_decision_id = self._resolve_decision_id(
                conn, decision, decision_id, ts
            )
            self._finalize_execution(
                conn, execution_id, resolved_decision_id, result, ts
            )
            self._finalize_proposal(conn, proposal_id, result, ts)
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return {
            "position_id": position_id,
            "decision_id": resolved_decision_id,
        }

    def finalize_adjust_action(self, execution_id: str, proposal_id: Optional[str], *,
                               position_id: int, stop_px: float, tp_px: float,
                               result: dict, decision: Optional[dict] = None,
                               decision_id: Optional[int] = None) -> dict:
        conn = self.db
        ts = time.time()
        conn.execute("BEGIN IMMEDIATE")
        try:
            cur = conn.execute(
                "UPDATE positions SET stop_px=?, tp_px=? "
                "WHERE id=? AND status='open'",
                (stop_px, tp_px, position_id),
            )
            if cur.rowcount != 1:
                raise RuntimeError(f"position {position_id} is missing or not open")
            decision_id = self._resolve_decision_id(
                conn, decision, decision_id, ts
            )
            self._finalize_execution(conn, execution_id, decision_id, result, ts)
            self._finalize_proposal(conn, proposal_id, result, ts)
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        return {"position_id": position_id, "decision_id": decision_id}

    def recent_refusals(self, n: int = 5, max_age_secs: int = 7200) -> list[dict]:
        rows = self.db.execute(
            "SELECT ts, market, reason FROM refusals WHERE ts >= ?"
            " ORDER BY ts DESC LIMIT ?", (time.time() - max_age_secs, n)).fetchall()
        return [dict(r) for r in rows]

    def record_refusal(self, market: Optional[str], action_json: str, reason: str) -> None:
        self.db.execute("INSERT INTO refusals (ts,market,action_json,reason) VALUES (?,?,?,?)",
                        (time.time(), market, action_json, reason))
        self.db.commit()

    # -- fills (live reconciliation dedupe) --------------------------------
    def fill_seen(self, tid: str) -> bool:
        return self.db.execute("SELECT 1 FROM fills_seen WHERE tid=?", (tid,)).fetchone() is not None

    def mark_fill(self, tid: str) -> None:
        self.db.execute("INSERT OR IGNORE INTO fills_seen VALUES (?,?)", (tid, time.time()))
        self.db.commit()

    def fill_history_initialized(self) -> bool:
        row = self.db.execute(
            "SELECT 1 FROM runtime_meta WHERE key='fill_history_initialized' AND value='1'"
        ).fetchone()
        return row is not None

    def mark_fill_history_initialized(self) -> None:
        self.db.execute(
            "INSERT INTO runtime_meta (key,value) VALUES ('fill_history_initialized','1')"
            " ON CONFLICT(key) DO UPDATE SET value=excluded.value"
        )
        self.db.commit()

    @staticmethod
    def _entry_context(ctx: Optional[dict]) -> tuple:
        """(style, range_pos, atr_pct, trigger) — what the setup LOOKED like at
        entry. Without it a closed trade cannot be attributed to a pattern."""
        c = ctx or {}
        return (c.get("style"), c.get("range_pos"), c.get("atr_pct"), c.get("trigger"))

    def attribute_entry_fee(self, pos_id: int, fee: float, tid: str) -> None:
        """Add the fee AND mark the fill seen in ONE transaction.

        Two statements meant a crash between them re-added the same fee on the
        next pass, quietly understating that trade's realized PnL."""
        conn = self.db
        conn.execute("BEGIN IMMEDIATE")
        try:
            conn.execute(
                "UPDATE positions SET entry_fee=COALESCE(entry_fee,0)+? WHERE id=?",
                (fee, pos_id))
            conn.execute("INSERT OR IGNORE INTO fills_seen VALUES (?,?)",
                         (tid, time.time()))
            conn.commit()
        except Exception:
            conn.rollback()
            raise

    def add_entry_fee(self, pos_id: int, fee: float) -> None:
        """Accumulate what it actually COST to get in. HL reports closedPnl gross
        and charges the entry fee on the opening fill, so a close that subtracts
        only its own fee overstates every trade by the entry side."""
        self.db.execute(
            "UPDATE positions SET entry_fee=COALESCE(entry_fee,0)+? WHERE id=?",
            (fee, pos_id))
        self.db.commit()

    def entry_fee(self, pos_id: int) -> float:
        r = self.db.execute(
            "SELECT entry_fee FROM positions WHERE id=?", (pos_id,)).fetchone()
        return float(r["entry_fee"] or 0.0) if r else 0.0

    def initial_stop(self, pos_id: int) -> Optional[float]:
        """The stop the position was SIZED with. Never moves — breakeven and
        trailing adjust stop_px, so R must be measured against this."""
        r = self.db.execute(
            "SELECT init_stop_px FROM positions WHERE id=?", (pos_id,)).fetchone()
        return None if r is None else r["init_stop_px"]

    def claim_position(self, pos_id: int, *, conviction: Optional[float],
                       rationale: Optional[str], invalidation: Optional[str],
                       stop_px: Optional[float], tp_px: Optional[float],
                       entry_context: Optional[dict] = None) -> None:
        """Adopt a position the venue already shows as ours: an entry that
        filled while the daemon was down came back as source='external'."""
        style, range_pos, atr_pct, trigger = self._entry_context(entry_context)
        self.db.execute(
            "UPDATE positions SET source='own', conviction=?, rationale=?, invalidation=?,"
            " stop_px=?, tp_px=?, init_stop_px=COALESCE(init_stop_px,?),"
            " entry_style=COALESCE(?,entry_style), entry_range_pos=COALESCE(?,entry_range_pos),"
            " entry_atr_pct=COALESCE(?,entry_atr_pct), entry_trigger=COALESCE(?,entry_trigger)"
            " WHERE id=? AND status='open'",
            (conviction, rationale, invalidation, stop_px, tp_px, stop_px,
             style, range_pos, atr_pct, trigger, pos_id))
        self.db.commit()

    def count_entry(self, day: str) -> None:
        self.db.execute("UPDATE daily SET entries=entries+1 WHERE day=?", (day,))
        self.db.commit()

    # -- the economic calendar: dated events the tape will react to --------
    IMPACTS = ("high", "medium", "low")

    def add_calendar_event(self, ts: float, title: str, *, impact: str = "medium",
                           scope: Optional[str] = None) -> Optional[int]:
        """One dated event. Deduped on (day, normalised title) so re-seeding the
        week is idempotent."""
        if impact not in self.IMPACTS:
            raise ValueError(f"impact must be one of {self.IMPACTS}, got {impact!r}")
        title = " ".join(title.split())[:200]
        if not title:
            return None
        norm = (time.strftime("%Y-%m-%d", time.gmtime(ts)) + "|"
                + " ".join(title.lower().split())[:120])
        cur = self.db.execute(
            "INSERT OR IGNORE INTO calendar_events (ts,title,impact,scope,created_ts,norm)"
            " VALUES (?,?,?,?,?,?)",
            (ts, title, impact, scope, time.time(), norm))
        self.db.commit()
        return int(cur.lastrowid) if cur.rowcount else None

    def upcoming_events(self, within_secs: float = 10 * 86400,
                        now: Optional[float] = None) -> list[dict]:
        now = now or time.time()
        rows = self.db.execute(
            "SELECT * FROM calendar_events WHERE ts >= ? AND ts <= ?"
            " ORDER BY ts", (now - 3600, now + within_secs)).fetchall()
        return [dict(r) for r in rows]

    def next_high_impact(self, now: Optional[float] = None) -> Optional[dict]:
        now = now or time.time()
        r = self.db.execute(
            "SELECT * FROM calendar_events WHERE ts >= ? AND impact='high'"
            " ORDER BY ts LIMIT 1", (now,)).fetchone()
        return dict(r) if r else None

    def forget_calendar_event(self, event_id: int) -> bool:
        cur = self.db.execute("DELETE FROM calendar_events WHERE id=?", (event_id,))
        self.db.commit()
        return cur.rowcount > 0

    # -- operator market bias ----------------------------------------------
    def set_bias(self, text: str, who: str = "operator") -> dict:
        """The operator's standing directional view. Advisory, not a gate: it is
        rendered prominently for the analyst to weigh, never enforced — a stance
        that silently refused trades would be a rail masquerading as an opinion."""
        payload = {"text": " ".join(text.split())[:600], "ts": time.time(), "by": who}
        self.db.execute(
            "INSERT INTO runtime_meta (key,value) VALUES ('market_bias',?)"
            " ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (json.dumps(payload),))
        self.db.commit()
        return payload

    def bias(self) -> Optional[dict]:
        r = self.db.execute(
            "SELECT value FROM runtime_meta WHERE key='market_bias'").fetchone()
        if r is None:
            return None
        payload = json.loads(r["value"])
        return payload if payload.get("text") else None

    def clear_bias(self) -> None:
        self.db.execute("DELETE FROM runtime_meta WHERE key='market_bias'")
        self.db.commit()

    # -- memory: lessons the analyst writes and always re-reads ------------
    LESSON_CAP = 30

    @staticmethod
    def _normalize_lesson(text: str) -> str:
        return " ".join(text.lower().split())[:200]

    def add_lesson(self, text: str, *, market: Optional[str] = None,
                   source: str = "analyst", decision_id: Optional[int] = None,
                   pinned: bool = False) -> Optional[int]:
        """Record one durable lesson. Returns None when it duplicates one we
        already hold — a model repeating itself must not crowd out real memory."""
        text = " ".join(text.split())[:400]
        if not text:
            return None
        norm = self._normalize_lesson(text)
        cur = self.db.execute(
            "INSERT OR IGNORE INTO lessons (ts,market,text,source,decision_id,pinned,norm)"
            " VALUES (?,?,?,?,?,?,?)",
            (time.time(), market, text, source, decision_id, int(pinned), norm))
        self.db.commit()
        if cur.rowcount == 0:
            incumbent = self.db.execute(
                "SELECT id, pinned FROM lessons WHERE norm=?", (norm,)).fetchone()
            if pinned and incumbent is not None and not incumbent["pinned"]:
                # the analyst had already phrased it: promote the incumbent
                # rather than letting a model's wording outrank the operator
                # (an unpinned row is prunable; a human rule must not be)
                self.db.execute(
                    "UPDATE lessons SET pinned=1, source=?, ts=?, text=? WHERE norm=?",
                    (source, time.time(), text, norm))
                self.db.commit()
                return int(incumbent["id"])
            return None
        lesson_id = int(cur.lastrowid)
        self._prune_lessons()
        return lesson_id

    def _prune_lessons(self) -> None:
        """Keep the newest LESSON_CAP unpinned lessons; operator-pinned ones stay
        forever. Memory has to forget, or the prompt becomes the whole ledger."""
        self.db.execute(
            "DELETE FROM lessons WHERE pinned=0 AND id NOT IN ("
            " SELECT id FROM lessons WHERE pinned=0 ORDER BY ts DESC LIMIT ?)",
            (self.LESSON_CAP,))
        self.db.commit()

    def lessons(self, markets: Optional[list[str]] = None,
                limit: int = 20) -> list[dict]:
        """Pinned first, then market-relevant, then general — newest within each."""
        rows = [dict(r) for r in self.db.execute(
            "SELECT * FROM lessons ORDER BY ts DESC")]
        relevant = set(markets or [])

        def rank(row: dict) -> tuple:
            return (0 if row["pinned"] else 1,
                    0 if (row["market"] and row["market"] in relevant) else 1,
                    -row["ts"])

        return sorted(rows, key=rank)[:limit]

    def forget_lesson(self, lesson_id: int) -> bool:
        cur = self.db.execute("DELETE FROM lessons WHERE id=?", (lesson_id,))
        self.db.commit()
        return cur.rowcount > 0

    # -- memory: what the ledger PROVES (computed, never remembered) --------
    def performance_digest(self, max_age_secs: Optional[float] = None) -> dict:
        """Aggregate every closed trade into the dimensions that decide whether a
        setup is worth repeating. Computed from the ledger each cycle, so it can
        never drift from the truth or be hallucinated."""
        where = "status='closed' AND realized_pnl IS NOT NULL"
        params: list = []
        if max_age_secs:
            where += " AND closed_ts >= ?"
            params.append(time.time() - max_age_secs)
        rows = [dict(r) for r in self.db.execute(
            f"SELECT market, side, entry_px, close_px, size, realized_pnl, close_reason,"
            f" conviction, opened_ts, closed_ts, init_stop_px, entry_style,"
            f" entry_range_pos, entry_atr_pct FROM positions WHERE {where}", params)]

        def bucket(rows_in: list[dict]) -> dict:
            n = len(rows_in)
            if not n:
                return {"n": 0, "wins": 0, "win_rate": None, "pnl": 0.0,
                        "avg_r": None, "median_hold_mins": None}
            wins = sum(1 for r in rows_in if r["realized_pnl"] > 0)
            pnl = sum(r["realized_pnl"] for r in rows_in)
            rs = []
            holds = []
            for r in rows_in:
                # a stop within 0.05% of entry is not a risk unit — dividing by it
                # renders "avg +999.99R" into the prompt
                meaningful = (r["init_stop_px"] and r["size"] and r["entry_px"]
                              and abs(r["entry_px"] - r["init_stop_px"]) / r["entry_px"]
                              >= 5e-4)
                risk = (abs(r["entry_px"] - r["init_stop_px"]) * r["size"]
                        if meaningful else None)
                if risk:
                    rs.append(r["realized_pnl"] / risk)
                if r["closed_ts"] and r["opened_ts"]:
                    holds.append((r["closed_ts"] - r["opened_ts"]) / 60.0)
            holds.sort()
            return {
                "n": n, "wins": wins, "win_rate": wins / n, "pnl": pnl,
                "avg_r": (sum(rs) / len(rs)) if rs else None,
                "median_hold_mins": holds[len(holds) // 2] if holds else None,
            }

        def group(key) -> dict:
            out: dict[str, list] = {}
            for r in rows:
                label = key(r)
                if label is not None:
                    out.setdefault(label, []).append(r)
            return {k: bucket(v) for k, v in sorted(out.items())}

        def range_label(r: dict) -> Optional[str]:
            pos = r.get("entry_range_pos")
            if not isinstance(pos, (int, float)):
                return None
            if pos >= 0.8:
                return "top of range (>=0.80)"
            if pos <= 0.2:
                return "bottom of range (<=0.20)"
            return "mid range"

        by_market = group(lambda r: r["market"])
        # sign-filter, or with few traded markets the SAME market appears in both
        # lists and a winner is rendered as "markets that have cost you most"
        worst = [kv for kv in sorted(by_market.items(), key=lambda kv: kv[1]["pnl"])
                 if kv[1]["pnl"] < 0][:3]
        best = [kv for kv in sorted(by_market.items(), key=lambda kv: -kv[1]["pnl"])
                if kv[1]["pnl"] > 0][:3]
        return {
            "overall": bucket(rows),
            "by_entry_style": group(lambda r: r.get("entry_style") or "market (unknown)"),
            "by_side": group(lambda r: r["side"]),
            "by_range_position": group(range_label),
            "by_close_reason": group(lambda r: r["close_reason"] or "?"),
            "worst_markets": dict(worst),
            "best_markets": dict(best),
        }

    # -- operator pause ----------------------------------------------------
    def paused(self) -> bool:
        """Operator pause survives restarts: it lives in the ledger, not memory."""
        row = self.db.execute(
            "SELECT value FROM runtime_meta WHERE key='paused'"
        ).fetchone()
        return row is not None and row["value"] == "1"

    def set_paused(self, paused: bool, who: str = "dashboard") -> None:
        self.db.execute(
            "INSERT INTO runtime_meta (key,value) VALUES ('paused',?)"
            " ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            ("1" if paused else "0",))
        self.db.execute(
            "INSERT INTO runtime_meta (key,value) VALUES ('paused_ts',?)"
            " ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (json.dumps({"ts": time.time(), "paused": bool(paused), "by": who}),))
        self.db.commit()

    def pause_state(self) -> dict:
        row = self.db.execute(
            "SELECT value FROM runtime_meta WHERE key='paused_ts'"
        ).fetchone()
        detail = json.loads(row["value"]) if row else {}
        # runtime_meta.paused is authoritative; the detail blob only annotates it
        return {**detail, "paused": self.paused()}

    # -- resting entries ---------------------------------------------------
    def add_pending_entry(self, *, market: str, side: str, entry_px: float, size: float,
                          notional: float, leverage: float, margin_mode: str,
                          stop_px: float, tp_px: float, conviction: Optional[float],
                          rationale: Optional[str], invalidation: Optional[str],
                          oid: Optional[int], decision_id: Optional[int],
                          expires_ts: float,
                          entry_context: Optional[dict] = None) -> int:
        cur = self.db.execute(
            "INSERT INTO pending_entries (market,side,entry_px,size,notional,leverage,"
            "margin_mode,stop_px,tp_px,conviction,rationale,invalidation,oid,decision_id,"
            "entry_context_json,placed_ts,expires_ts) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (market, side, entry_px, size, notional, leverage, margin_mode, stop_px, tp_px,
             conviction, rationale, invalidation, oid, decision_id,
             _json(entry_context or {}), time.time(), expires_ts))
        self.db.commit()
        return int(cur.lastrowid)

    def resting_entries(self) -> list[dict]:
        return [dict(r) for r in self.db.execute(
            "SELECT * FROM pending_entries WHERE status='resting' ORDER BY placed_ts")]

    def resting_entry_for(self, market: str) -> Optional[dict]:
        r = self.db.execute(
            "SELECT * FROM pending_entries WHERE status='resting' AND market=?"
            " ORDER BY placed_ts DESC LIMIT 1", (market,)).fetchone()
        return dict(r) if r else None

    def settle_pending_entry(self, entry_id: int, outcome: str) -> None:
        if outcome not in {"filled", "expired", "cancelled", "vanished"}:
            raise ValueError(f"invalid pending entry outcome: {outcome!r}")
        self.db.execute(
            "UPDATE pending_entries SET status='settled', settled_ts=?, outcome=?"
            " WHERE id=? AND status='resting'", (time.time(), outcome, entry_id))
        self.db.commit()

    def recent_pending_entries(self, limit: int = 10) -> list[dict]:
        return [dict(r) for r in self.db.execute(
            "SELECT * FROM pending_entries ORDER BY placed_ts DESC LIMIT ?", (limit,))]

    # -- telegram ----------------------------------------------------------
    def add_tg_message(self, msg_id: int, ts: float, sender: Optional[str],
                       text: str, is_caller: bool,
                       image_desc: Optional[str] = None) -> bool:
        """Insert or update changed text; return False for an unchanged replay.

        A caption edit arrives with image_desc=None; COALESCE keeps the
        description already read off the picture rather than blanking it, so
        the vision call is paid for once per photo, not once per edit."""
        cur = self.db.execute(
            "INSERT INTO tg_messages (msg_id,ts,sender,text,is_caller,image_desc)"
            " VALUES (?,?,?,?,?,?)"
            " ON CONFLICT(msg_id) DO UPDATE SET ts=excluded.ts, sender=excluded.sender,"
            " text=excluded.text, is_caller=excluded.is_caller,"
            " image_desc=COALESCE(excluded.image_desc, tg_messages.image_desc)"
            " WHERE tg_messages.text != excluded.text"
            " OR tg_messages.sender IS NOT excluded.sender"
            " OR tg_messages.is_caller != excluded.is_caller"
            " OR (excluded.image_desc IS NOT NULL"
            "     AND tg_messages.image_desc IS NOT excluded.image_desc)",
            (msg_id, ts, sender, text, int(is_caller), image_desc))
        self.db.commit()
        return cur.rowcount == 1

    def recent_tg(self, n: int = 20) -> list[dict]:
        rows = self.db.execute(
            "SELECT msg_id, ts, sender, text, is_caller, image_desc FROM tg_messages"
            " ORDER BY ts DESC LIMIT ?", (n,)).fetchall()
        return [dict(r) for r in reversed(rows)]

    def tg_message(self, msg_id: int) -> Optional[dict]:
        r = self.db.execute("SELECT msg_id, ts, sender, text, is_caller, image_desc"
                            " FROM tg_messages WHERE msg_id=?", (msg_id,)).fetchone()
        return dict(r) if r else None

    # -- news (telegram channels) ------------------------------------------
    def add_news_item(self, source: str, msg_id: int, ts: float, text: str) -> bool:
        cur = self.db.execute("INSERT OR IGNORE INTO news_items VALUES (?,?,?,?)",
                              (source, msg_id, ts, text))
        self.db.commit()
        return cur.rowcount == 1

    def recent_news(self, n: int = 20, max_age_secs: int = 24 * 3600) -> list[dict]:
        rows = self.db.execute(
            "SELECT source, ts, text FROM news_items WHERE ts >= ?"
            " ORDER BY ts DESC LIMIT ?", (time.time() - max_age_secs, n)).fetchall()
        return [dict(r) for r in rows]

    # -- operator notes (briefings from the humans, shown in every prompt) --
    def add_note(self, text: str) -> None:
        self.db.execute("INSERT INTO operator_notes (ts, text) VALUES (?, ?)",
                        (time.time(), text))
        self.db.commit()

    def recent_notes(self, n: int = 5, max_age_secs: int = 72 * 3600) -> list[dict]:
        rows = self.db.execute(
            "SELECT ts, text FROM operator_notes WHERE ts >= ?"
            " ORDER BY ts DESC LIMIT ?", (time.time() - max_age_secs, n)).fetchall()
        return [dict(r) for r in reversed(rows)]

    def update_position_thesis(self, pos_id: int, rationale: str, invalidation: str,
                               stop_px=None, tp_px=None) -> None:
        self.db.execute(
            "UPDATE positions SET rationale=?, invalidation=?,"
            " stop_px=COALESCE(?, stop_px), tp_px=COALESCE(?, tp_px) WHERE id=?",
            (rationale, invalidation, stop_px, tp_px, pos_id))
        self.db.commit()

    # -- daily / kill ------------------------------------------------------
    def day_row(self, day: str) -> Optional[dict]:
        r = self.db.execute("SELECT * FROM daily WHERE day=?", (day,)).fetchone()
        return dict(r) if r else None

    def open_day(self, day: str, equity: float) -> None:
        self.db.execute("INSERT OR IGNORE INTO daily (day, open_equity) VALUES (?,?)",
                        (day, equity))
        self.db.commit()

    def bump_entries(self, day: str) -> None:
        self.db.execute("UPDATE daily SET entries = entries + 1 WHERE day=?", (day,))
        self.db.commit()

    def entries_today(self, day: str) -> int:
        r = self.db.execute("SELECT entries FROM daily WHERE day=?", (day,)).fetchone()
        return int(r["entries"]) if r else 0

    def trip_kill(self, day: str) -> None:
        self.db.execute("UPDATE daily SET kill_tripped=1 WHERE day=?", (day,))
        self.db.commit()

    def kill_tripped(self, day: str) -> bool:
        r = self.db.execute("SELECT kill_tripped FROM daily WHERE day=?", (day,)).fetchone()
        return bool(r and r["kill_tripped"])

    # -- cooldowns ---------------------------------------------------------
    def set_cooldown(self, market: str, until_ts: float, why: str) -> None:
        self.db.execute("INSERT OR REPLACE INTO cooldowns VALUES (?,?,?)",
                        (market, until_ts, why))
        self.db.commit()

    def cooldown_until(self, market: str, now: float) -> Optional[float]:
        r = self.db.execute("SELECT until_ts FROM cooldowns WHERE market=?", (market,)).fetchone()
        if r and r["until_ts"] > now:
            return float(r["until_ts"])
        return None


def dumps_actions(actions) -> str:
    return json.dumps([a.model_dump() for a in actions])
