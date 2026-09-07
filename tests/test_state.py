import json
import sqlite3
import time

import pytest

from peri.state import State


def mk(tmp_path):
    return State(str(tmp_path / "t.db"))


def test_position_lifecycle(tmp_path):
    s = mk(tmp_path)
    p = s.add_position("xyz:NVDA", "long", 218.15, 0.596, 130.0, 20.0, 214.9, 226.0,
                       0.8, "own", "beat", "loses shelf", margin_mode="cross")
    assert p.status == "open" and p.market == "xyz:NVDA"
    assert p.margin_mode == "cross"
    assert s.open_position_for("xyz:NVDA").id == p.id
    assert len(s.open_positions()) == 1
    s.update_stop(p.id, 216.0)
    assert s.open_position_for("xyz:NVDA").stop_px == 216.0
    s.update_size(p.id, 0.3)
    assert s.open_position_for("xyz:NVDA").size == 0.3
    s.close_position(p.id, "tp", 226.0, 4.4)
    assert s.open_position_for("xyz:NVDA") is None
    closes = s.recent_closes()
    assert closes[0]["close_reason"] == "tp" and closes[0]["realized_pnl"] == 4.4
    assert s.realized_total() == 4.4


def test_tg_dedupe_and_recent(tmp_path):
    s = mk(tmp_path)
    assert s.add_tg_message(1, 1000.0, "caller1", "SOL long", True) is True
    assert s.add_tg_message(1, 1000.0, "caller1", "SOL long", True) is False
    assert s.add_tg_message(1, 1002.0, "caller1", "SOL long — edited", True) is True
    s.add_tg_message(2, 1001.0, "rando", "gm", False)
    msgs = s.recent_tg(10)
    assert [m["msg_id"] for m in msgs] == [2, 1]  # edit time makes msg 1 newest
    assert msgs[1]["is_caller"] == 1
    assert s.tg_message(1)["sender"] == "caller1"
    assert s.tg_message(1)["text"] == "SOL long — edited"
    assert s.tg_message(1)["ts"] == 1002.0
    assert s.tg_message(99) is None


def test_daily_and_kill(tmp_path):
    s = mk(tmp_path)
    assert s.day_row("2026-08-27") is None
    s.open_day("2026-08-27", 1000.0)
    s.open_day("2026-08-27", 999.0)  # idempotent — first open wins
    assert s.day_row("2026-08-27")["open_equity"] == 1000.0
    assert s.entries_today("2026-08-27") == 0
    s.bump_entries("2026-08-27")
    assert s.entries_today("2026-08-27") == 1
    assert s.kill_tripped("2026-08-27") is False
    s.trip_kill("2026-08-27")
    assert s.kill_tripped("2026-08-27") is True
    assert s.kill_tripped("2026-08-28") is False  # day roll resets by keying on day


def test_cooldowns(tmp_path):
    s = mk(tmp_path)
    now = time.time()
    assert s.cooldown_until("SOL", now) is None
    s.set_cooldown("SOL", now + 60, "sl")
    assert s.cooldown_until("SOL", now) is not None
    assert s.cooldown_until("SOL", now + 61) is None


def test_fills_seen(tmp_path):
    s = mk(tmp_path)
    assert s.fill_history_initialized() is False
    assert s.fill_seen("t1") is False
    s.mark_fill("t1")
    s.mark_fill("t1")
    assert s.fill_seen("t1") is True
    s.mark_fill_history_initialized()
    assert s.fill_history_initialized() is True


def test_existing_fill_ledger_migrates_as_initialized(tmp_path):
    import sqlite3

    db_path = str(tmp_path / "old-fills.db")
    state = State(db_path)
    state.mark_fill("old")
    state.db.close()

    conn = sqlite3.connect(db_path)
    conn.execute("DELETE FROM runtime_meta WHERE key='fill_history_initialized'")
    conn.commit()
    conn.close()

    migrated = State(db_path)
    assert migrated.fill_history_initialized() is True


def test_decisions_and_refusals(tmp_path):
    s = mk(tmp_path)
    s.record_decision("scheduled", "chop", "[]", "m", 1200, "ok")
    s.record_refusal("BTC", "{}", "conviction 0.60 < floor 0.75")
    assert s.db.execute("SELECT COUNT(*) c FROM decisions").fetchone()["c"] == 1
    assert s.db.execute("SELECT reason FROM refusals").fetchone()["reason"].startswith("conviction")


def test_news_items_dedupe_and_recent(tmp_path):
    s = mk(tmp_path)
    assert s.add_news_item("WatcherGuru", 1, time.time(), "BREAKING: something") is True
    assert s.add_news_item("WatcherGuru", 1, time.time(), "BREAKING: something") is False
    s.add_news_item("TreeNewsFeed", 9, time.time() - 60, "older item")
    s.add_news_item("TreeNewsFeed", 10, time.time() - 90000, "too old")  # >24h
    items = s.recent_news(10)
    assert [i["source"] for i in items] == ["WatcherGuru", "TreeNewsFeed"]


def test_decisions_migration_adds_columns(tmp_path):
    db_path = str(tmp_path / "old.db")
    conn = sqlite3.connect(db_path)
    conn.execute("""CREATE TABLE decisions (
        id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL,
        trigger TEXT NOT NULL, market_view TEXT, actions_json TEXT NOT NULL,
        model TEXT, latency_ms INTEGER, status TEXT NOT NULL)""")
    conn.execute("INSERT INTO decisions (ts,trigger,actions_json,status)"
                 " VALUES (1,'t','[]','ok')")
    conn.commit()
    conn.close()
    s = State(db_path)  # must migrate, not crash
    s.record_decision("t2", "v", "[]", "m", 5, "ok", reasoning="r", prompt="p",
                      tool_log="[]")
    rows = s.recent_decisions(5)
    assert len(rows) == 2 and rows[0]["reasoning"] == "r"


def test_legacy_positions_migrate_to_unknown_margin_mode(tmp_path):
    db_path = str(tmp_path / "old-positions.db")
    conn = sqlite3.connect(db_path)
    conn.execute("""CREATE TABLE positions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        market TEXT NOT NULL, side TEXT NOT NULL,
        entry_px REAL NOT NULL, size REAL NOT NULL, notional REAL NOT NULL,
        leverage REAL NOT NULL, stop_px REAL, tp_px REAL,
        conviction REAL, source TEXT NOT NULL,
        rationale TEXT, invalidation TEXT,
        status TEXT NOT NULL DEFAULT 'open',
        opened_ts REAL NOT NULL, closed_ts REAL,
        close_reason TEXT, close_px REAL, realized_pnl REAL)""")
    conn.execute(
        "INSERT INTO positions "
        "(market,side,entry_px,size,notional,leverage,source,status,opened_ts) "
        "VALUES ('SOL','long',100,1,100,4,'external','open',1)"
    )
    conn.commit()
    conn.close()

    s = State(db_path)
    assert s.open_position_for("SOL").margin_mode == "unknown"
    s.sync_position_snapshot(1, "long", 101, 1, 101, 10, "isolated")
    assert s.open_position_for("SOL").margin_mode == "isolated"


def test_chat_history_is_durable_bounded_and_paginated(tmp_path):
    s = mk(tmp_path)
    first = s.add_chat_message(
        "user", "How is NVDA?", context_ts=100.0, metadata={"target": "xyz:NVDA"}
    )
    second = s.add_chat_message(
        "assistant", "It is protected.", context_ts=101.0, proposal_id="p1",
        metadata={"tools": [{"url": "https://example.test"}]},
    )
    newest = s.chat_history(limit=1)
    assert [m["id"] for m in newest] == [second["id"]]
    assert newest[0]["metadata"]["tools"][0]["url"] == "https://example.test"
    older = s.chat_history(limit=100, before_id=second["id"])
    assert [m["id"] for m in older] == [first["id"]]
    assert older[0]["context_ts"] == 100.0


def _proposal(s, proposal_id="p1"):
    return s.create_trade_proposal(
        proposal_id,
        action={"kind": "close", "market": "SOL", "rationale": "invalidated"},
        preview={"mode": "paper", "market": "SOL"},
        context_ts=100.0,
        expires_ts=220.0,
    )


def test_proposal_claim_is_atomic_single_use_and_terminal(tmp_path):
    s = mk(tmp_path)
    created = _proposal(s)
    assert created["action"]["kind"] == "close"
    assert s.claim_trade_proposal("p1", now=150.0)["status"] == "authorizing"
    assert s.claim_trade_proposal("p1", now=151.0) is None
    finished = s.finish_trade_proposal(
        "p1", "refused", {"reason": "position changed"}, now=152.0
    )
    assert finished["status"] == "refused"
    assert finished["result"]["reason"] == "position changed"
    assert s.claim_trade_proposal("p1", now=153.0) is None


def test_nonterminal_action_journal_enumeration(tmp_path):
    s = mk(tmp_path)
    s.create_action_execution(
        "a1", origin="autonomous", kind="open", proposal_id=None,
        action={"kind": "open", "market": "BTC"},
        pre_state={"position": None}, expected={"size": 0.01},
    )
    s.create_action_execution(
        "a2", origin="chat", kind="close", proposal_id=None,
        action={"kind": "close", "market": "SOL"},
        pre_state={"position_id": 1}, expected={},
    )
    s.update_action_execution(
        "a1", stage="entry_submitted", status="needs_reconciliation",
        submission_ts=10.0, response_ts=11.0, fill_id="f1",
        result={"reason": "timeout"},
    )
    assert {j["id"] for j in s.unfinished_action_executions()} == {"a1", "a2"}
    s.update_action_execution("a2", stage="done", status="failed", result={"reason": "no fill"})
    assert [j["id"] for j in s.unfinished_action_executions()] == ["a1"]
    assert s.action_execution("a1")["expected"]["size"] == 0.01


def _decision(action):
    return {
        "trigger": "chat authorized p-open",
        "market_view": "operator authorized",
        "actions_json": json.dumps([action]),
        "model": "qwen",
        "latency_ms": 5,
        "status": "ok",
        "reasoning": "",
        "prompt": "",
        "tool_log": "[]",
    }


def _position_payload():
    return {
        "market": "SOL", "side": "long", "entry_px": 100.5, "size": 0.6,
        "notional": 60.3, "leverage": 10, "margin_mode": "isolated",
        "stop_px": 97.0, "tp_px": 107.0, "conviction": 0.8, "source": "own",
        "rationale": "momo", "invalidation": "loses 97",
    }


def test_finalize_open_updates_journal_proposal_position_and_decision_atomically(tmp_path):
    s = mk(tmp_path)
    _proposal(s, "p-open")
    assert s.claim_trade_proposal("p-open", now=150.0)
    action = {"kind": "open", "market": "SOL"}
    s.create_action_execution(
        "a-open", origin="chat", kind="open", proposal_id="p-open",
        action=action, pre_state={"position": None}, expected={"size": 0.6},
    )
    out = s.finalize_open_action(
        "a-open", "p-open", position=_position_payload(),
        decision=_decision(action), result={"entry_px": 100.5, "size": 0.6},
        day=None,
    )
    assert s.open_position_for("SOL").margin_mode == "isolated"
    assert s.trade_proposal("p-open")["status"] == "executed"
    journal = s.action_execution("a-open")
    assert journal["status"] == "executed"
    assert journal["decision_id"] == out["decision_id"]
    assert s.recent_decisions(1)[0]["trigger"] == "chat authorized p-open"


def test_finalize_open_rolls_back_every_table_if_proposal_update_fails(tmp_path):
    s = mk(tmp_path)
    action = {"kind": "open", "market": "SOL"}
    s.create_action_execution(
        "a-open", origin="chat", kind="open", proposal_id="missing",
        action=action, pre_state={"position": None}, expected={"size": 0.6},
    )
    with pytest.raises(RuntimeError, match="proposal"):
        s.finalize_open_action(
            "a-open", "missing", position=_position_payload(),
            decision=_decision(action), result={"entry_px": 100.5}, day=None,
        )
    assert s.open_positions() == []
    assert s.recent_decisions() == []
    assert s.action_execution("a-open")["status"] == "prepared"


def test_close_and_adjust_terminal_finalizers(tmp_path):
    s = mk(tmp_path)
    position = s.add_position(
        "SOL", "long", 100, 0.6, 60, 10, 97, 107, 0.8, "own",
        margin_mode="isolated",
    )
    action = {"kind": "adjust_stop", "market": "SOL", "stop": 99, "take_profit": 108}
    s.create_action_execution(
        "a-adjust", origin="autonomous", kind="adjust_stop", proposal_id=None,
        action=action, pre_state={"position_id": position.id}, expected={},
    )
    s.finalize_adjust_action(
        "a-adjust", None, position_id=position.id, stop_px=99, tp_px=108,
        decision=_decision(action), result={"stop_px": 99, "tp_px": 108},
    )
    adjusted = s.open_position_for("SOL")
    assert (adjusted.stop_px, adjusted.tp_px) == (99, 108)

    close_action = {"kind": "close", "market": "SOL"}
    s.create_action_execution(
        "a-close", origin="autonomous", kind="close", proposal_id=None,
        action=close_action, pre_state={"position_id": position.id}, expected={},
    )
    s.finalize_close_action(
        "a-close", None, position_id=position.id, close_reason="analyst",
        close_px=102, realized_pnl=1.2, decision=_decision(close_action),
        result={"close_px": 102, "realized_pnl": 1.2},
    )
    assert s.open_position_for("SOL") is None
    assert s.action_execution("a-close")["status"] == "executed"


def test_operator_notes_and_thesis_update(tmp_path):
    s = mk(tmp_path)
    s.add_note("first briefing")
    s.add_note("second briefing")
    notes = s.recent_notes(5)
    assert [n["text"] for n in notes] == ["first briefing", "second briefing"]
    p = s.add_position("xyz:KIOXIA", "short", 333.76, 0.117, 39.0, 3.0, None, None,
                       None, "external")
    s.update_position_thesis(p.id, "carry short", "funding flips positive",
                             stop_px=342.0, tp_px=315.0)
    q = s.open_position_for("xyz:KIOXIA")
    assert q.rationale == "carry short" and q.stop_px == 342.0 and q.tp_px == 315.0


# -- memory: lessons + the measured record --------------------------------
def closed(state, market, side, pnl, *, range_pos=0.5, style="market",
           reason="sl", entry=100.0, stop=98.0, size=1.0, held_mins=60):
    import time as _t
    pos = state.add_position(
        market, side, entry, size, entry * size, 10.0, stop,
        entry * 1.1 if side == "long" else entry * 0.9, 0.8, "own",
        entry_context={"style": style, "range_pos": range_pos, "atr_pct": 0.4,
                       "trigger": "scheduled"})
    state.db.execute("UPDATE positions SET opened_ts=? WHERE id=?",
                     (_t.time() - held_mins * 60, pos.id))
    state.db.commit()
    state.close_position(pos.id, reason, entry + (pnl / size), pnl)
    return pos


def test_lessons_dedupe_rank_and_prune(tmp_path):
    s = State(str(tmp_path / "m.db"))
    first = s.add_lesson("Gap-downs bounce for an hour; short the retest.",
                         market="xyz:MRVL")
    assert first is not None
    # same lesson in different words of the same words -> not new knowledge
    assert s.add_lesson("gap-downs  BOUNCE for an hour; short the retest.") is None
    s.add_lesson("Operator rule: never trade the first 30 minutes.",
                 source="operator", pinned=True)
    for i in range(40):
        s.add_lesson(f"filler lesson number {i} about a market I no longer trade")
    kept = s.db.execute("SELECT COUNT(*) c FROM lessons").fetchone()["c"]
    assert kept == State.LESSON_CAP + 1          # the pinned one is never pruned
    top = s.lessons(["xyz:MRVL"], limit=5)
    assert top[0]["pinned"] == 1                 # operator first
    assert s.forget_lesson(top[0]["id"]) is True
    assert s.forget_lesson(999999) is False


def test_lessons_surface_the_markets_in_play_first(tmp_path):
    s = State(str(tmp_path / "m.db"))
    s.add_lesson("SOL respects the 4h base.", market="SOL")
    for i in range(5):
        s.add_lesson(f"a newer general lesson {i}")
    s.add_lesson("NVDA earnings gaps are untradeable for an hour.", market="xyz:NVDA")
    ranked = s.lessons(["SOL"], limit=3)
    assert ranked[0]["market"] == "SOL"          # older, but it is about SOL


def test_performance_digest_attributes_losses_to_the_pattern(tmp_path):
    s = State(str(tmp_path / "m.db"))
    # the 2026-08-28 shape: chased the range edge at market, all stopped out
    closed(s, "xyz:CRWD", "long", -2.62, range_pos=0.88, style="market")
    closed(s, "xyz:MRVL", "short", -1.39, range_pos=0.05, style="market")
    closed(s, "HYPE", "long", -1.32, range_pos=0.90, style="market")
    # ...against patient mid-range maker entries
    closed(s, "SOL", "long", 2.18, range_pos=0.52, style="resting", reason="tp")
    closed(s, "BTC", "short", 1.10, range_pos=0.45, style="resting", reason="tp")

    d = s.performance_digest()
    assert d["overall"]["n"] == 5
    assert d["overall"]["wins"] == 2
    assert d["overall"]["pnl"] == pytest.approx(-2.05)
    assert d["by_entry_style"]["market"]["n"] == 3
    assert d["by_entry_style"]["market"]["win_rate"] == 0.0
    assert d["by_entry_style"]["resting"]["win_rate"] == 1.0
    assert d["by_range_position"]["top of range (>=0.80)"]["pnl"] == pytest.approx(-3.94)
    assert d["by_range_position"]["mid range"]["pnl"] == pytest.approx(3.28)
    assert d["by_close_reason"]["sl"]["n"] == 3
    assert list(d["worst_markets"])[0] == "xyz:CRWD"
    assert list(d["best_markets"])[0] == "SOL"
    # R is measured against the risk actually taken (entry -> initial stop = $2)
    assert d["by_entry_style"]["resting"]["avg_r"] == pytest.approx(
        ((2.18 / 2.0) + (1.10 / 2.0)) / 2, abs=0.01)


def test_performance_digest_windows_by_age(tmp_path):
    s = State(str(tmp_path / "m.db"))
    old = closed(s, "SOL", "long", -5.0)
    s.db.execute("UPDATE positions SET closed_ts=? WHERE id=?",
                 (time.time() - 10 * 86400, old.id))
    s.db.commit()
    closed(s, "BTC", "short", 1.0)
    assert s.performance_digest()["overall"]["n"] == 2
    assert s.performance_digest(3 * 86400)["overall"]["n"] == 1


def test_digest_is_empty_not_wrong_before_any_trade(tmp_path):
    s = State(str(tmp_path / "m.db"))
    d = s.performance_digest()
    assert d["overall"] == {"n": 0, "wins": 0, "win_rate": None, "pnl": 0.0,
                            "avg_r": None, "median_hold_mins": None}
    assert d["by_entry_style"] == {} and d["worst_markets"] == {}


def test_the_digest_never_labels_a_winner_as_a_loss(tmp_path):
    """With few traded markets, worst/best were the SAME list — the prompt read
    'markets that have cost you most: xyz:NVDA $+0.60'."""
    s = State(str(tmp_path / "m.db"))
    closed(s, "xyz:NVDA", "long", 0.60)
    closed(s, "SOL", "long", 2.14)
    closed(s, "BTC", "short", 0.31)
    d = s.performance_digest()
    assert d["worst_markets"] == {}                     # nothing has cost anything
    assert set(d["best_markets"]) == {"xyz:NVDA", "SOL", "BTC"}
    closed(s, "xyz:CRWD", "long", -2.67)
    d = s.performance_digest()
    assert set(d["worst_markets"]) == {"xyz:CRWD"}
    assert set(d["worst_markets"]) & set(d["best_markets"]) == set()
    assert all(b["pnl"] < 0 for b in d["worst_markets"].values())
    assert all(b["pnl"] > 0 for b in d["best_markets"].values())


def test_a_breakeven_stop_does_not_explode_avg_r(tmp_path):
    s = State(str(tmp_path / "m.db"))
    pos = closed(s, "BTC", "long", 0.50, entry=100.0, stop=100.01)
    assert s.performance_digest()["overall"]["avg_r"] is None
    s.db.execute("UPDATE positions SET init_stop_px=98.0 WHERE id=?", (pos.id,))
    s.db.commit()
    assert s.performance_digest()["overall"]["avg_r"] == pytest.approx(0.25)


def test_an_operator_pin_outranks_an_analyst_duplicate(tmp_path):
    s = State(str(tmp_path / "m.db"))
    analyst_id = s.add_lesson("Do not chase breakouts", source="analyst")
    promoted = s.add_lesson("do   not CHASE breakouts", source="operator", pinned=True)
    assert promoted == analyst_id                       # upgraded, not swallowed
    row = dict(s.db.execute("SELECT * FROM lessons").fetchone())
    assert row["pinned"] == 1 and row["source"] == "operator"
    # an operator repeating their own pinned rule is a genuine duplicate
    assert s.add_lesson("DO NOT chase breakouts", source="operator", pinned=True) is None
    # and the promoted rule now survives pruning
    for i in range(40):
        s.add_lesson(f"filler {i} about nothing in particular")
    assert any(le["pinned"] == 1 for le in s.lessons(limit=100))


def test_a_pre_migration_row_is_excluded_from_r_rather_than_invented(tmp_path):
    s = State(str(tmp_path / "m.db"))
    pos = closed(s, "SOL", "long", 1.0, entry=100.0, stop=96.0)
    s.db.execute("UPDATE positions SET init_stop_px=NULL WHERE id=?", (pos.id,))
    s.db.commit()
    d = s.performance_digest()
    assert d["overall"]["n"] == 1 and d["overall"]["avg_r"] is None


def test_a_column_added_to_the_schema_without_an_alter_fails_at_boot(tmp_path):
    """2026-08-29: entry_context_json was added to _SCHEMA but CREATE TABLE IF
    NOT EXISTS does nothing to an existing table, so the column was missing in
    production. It surfaced as an INSERT error at the exact moment a resting
    entry was recorded — after the order was already live at the venue."""
    from peri.state import _SCHEMA
    path = str(tmp_path / "legacy.db")
    legacy = sqlite3.connect(path)
    legacy.executescript(_SCHEMA)
    # simulate the old table: rebuild it without the column
    legacy.execute("DROP TABLE pending_entries")
    legacy.execute("""CREATE TABLE pending_entries (
        id INTEGER PRIMARY KEY AUTOINCREMENT, market TEXT NOT NULL, side TEXT NOT NULL,
        entry_px REAL NOT NULL, size REAL NOT NULL, notional REAL NOT NULL,
        leverage REAL NOT NULL, margin_mode TEXT NOT NULL, stop_px REAL NOT NULL,
        tp_px REAL NOT NULL, conviction REAL, rationale TEXT, invalidation TEXT,
        oid INTEGER, decision_id INTEGER, placed_ts REAL NOT NULL,
        expires_ts REAL NOT NULL, status TEXT NOT NULL DEFAULT 'resting',
        settled_ts REAL, outcome TEXT)""")
    legacy.commit()
    legacy.close()

    state = State(path)                      # migrates, then verifies
    cols = {r["name"] for r in state.db.execute("PRAGMA table_info(pending_entries)")}
    assert "entry_context_json" in cols
    # and the insert that failed in production now works
    entry_id = state.add_pending_entry(
        market="BTC", side="short", entry_px=79200.0, size=0.00174, notional=137.8,
        leverage=10.0, margin_mode="isolated", stop_px=80900.0, tp_px=74950.0,
        conviction=0.77, rationale="supply wall", invalidation="15m close above 80.9k",
        oid=1, decision_id=None, expires_ts=9e9,
        entry_context={"style": "resting", "range_pos": 0.8, "atr_pct": 0.5,
                       "trigger": "manual dashboard"})
    assert state.resting_entries()[0]["id"] == entry_id


def test_schema_drift_is_refused_loudly_rather_than_at_the_first_insert(tmp_path):
    path = str(tmp_path / "drift.db")
    State(path)                              # create it properly
    conn = sqlite3.connect(path)
    conn.execute("ALTER TABLE lessons RENAME TO lessons_old")
    conn.execute("CREATE TABLE lessons (id INTEGER PRIMARY KEY, ts REAL NOT NULL,"
                 " text TEXT NOT NULL, norm TEXT NOT NULL)")   # missing columns
    conn.commit()
    conn.close()
    with pytest.raises(RuntimeError, match="ledger schema is behind the code"):
        State(path)


def test_the_calendar_dedupes_and_orders(tmp_path):
    s = State(str(tmp_path / "c.db"))
    now = time.time()
    first = s.add_calendar_event(now + 3600, "August jobs report", impact="high")
    assert first is not None
    assert s.add_calendar_event(now + 4000, "august  JOBS  report") is None   # same day
    s.add_calendar_event(now + 7200, "ISM Services", impact="medium")
    s.add_calendar_event(now + 30 * 86400, "far future", impact="low")
    upcoming = s.upcoming_events(within_secs=10 * 86400)
    assert [e["title"] for e in upcoming] == ["August jobs report", "ISM Services"]
    assert s.next_high_impact()["title"] == "August jobs report"
    assert s.forget_calendar_event(first) is True
    assert s.next_high_impact() is None
    with pytest.raises(ValueError):
        s.add_calendar_event(now, "bad", impact="enormous")


def test_bias_is_stored_read_back_and_clearable(tmp_path):
    s = State(str(tmp_path / "b.db"))
    assert s.bias() is None
    payload = s.set_bias("Risk-off into Friday's jobs report; favour shorts on retests.")
    assert payload["by"] == "operator"
    assert "favour shorts" in s.bias()["text"]
    s.clear_bias()
    assert s.bias() is None
