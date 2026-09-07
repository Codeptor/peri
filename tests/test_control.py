"""Telegram slash commands — the operator's controls from a phone."""

from peri.control import TelegramControl
from tests.test_engine import REST_SOL, mk_rails

OWNER = 4242
STRANGER = 9999


class FakeBot:
    """Stands in for the Bot API: queue updates, capture replies."""

    def __init__(self, updates=None):
        self.updates = list(updates or [])
        self.sent = []

    def __call__(self, method, params):
        if method == "getUpdates":
            batch = [u for u in self.updates
                     if u["update_id"] >= params.get("offset", 0)]
            return {"result": batch}
        self.sent.append(params["text"])
        return {"ok": True}


def msg(text, uid=OWNER, update_id=1):
    return {"update_id": update_id,
            "message": {"text": text, "chat": {"id": 77}, "from": {"id": uid}}}


def mk(tmp_path, decisions=None):
    eng, state, notes, analyst = mk_rails(tmp_path, decisions or [])
    bot = FakeBot()
    control = TelegramControl(eng, "token", OWNER, transport=bot)
    return eng, state, control, bot


def test_only_the_control_user_can_issue_commands(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/pause", uid=STRANGER)]
    control.poll_once()
    assert bot.sent == ["not authorised"]
    assert state.paused() is False          # the command did NOT run


def test_pause_and_resume_from_a_phone(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/pause")]
    control.poll_once()
    assert state.paused() is True
    assert "PAUSED" in bot.sent[-1] and "keep their venue" in bot.sent[-1]

    bot.updates = [msg("/resume", update_id=2)]
    control.poll_once()
    assert state.paused() is False
    assert "resumed" in bot.sent[-1]


def test_pause_from_telegram_cancels_resting_orders(tmp_path):
    eng, state, control, bot = mk(tmp_path, [REST_SOL])
    eng.cycle("scheduled")
    assert len(state.resting_entries()) == 1
    bot.updates = [msg("/pause")]
    control.poll_once()
    assert state.resting_entries() == []
    assert "cancelled resting: SOL" in bot.sent[-1]


def test_status_reports_the_things_you_would_want_on_a_bus(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    eng._account_snapshot = {"equity": 59.31, "available_margin": 44.0}
    bot.updates = [msg("/status")]
    control.poll_once()
    out = bot.sent[-1]
    assert "$59.31" in out and "positions 0/3" in out and "live" in out


def test_status_shouts_when_an_action_is_blocking_entries(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    eng._account_snapshot = {"equity": 59.31, "available_margin": 44.0}
    state.create_action_execution(
        "wedged", origin="autonomous", kind="close", proposal_id=None,
        action={"kind": "close", "market": "SOL", "rationale": "x"},
        pre_state={"position": None}, expected={}, decision_id=None)
    state.update_action_execution("wedged", stage="x", status="manual_review")
    bot.updates = [msg("/status")]
    control.poll_once()
    assert "BLOCKED" in bot.sent[-1]


def test_book_shows_positions_and_resting_levels(tmp_path):
    eng, state, control, bot = mk(tmp_path, [REST_SOL])
    eng.cycle("scheduled")
    state.add_position("BTC", "short", 78000.0, 0.001, 78.0, 10.0, 79500.0,
                       75000.0, 0.8, "own", margin_mode="isolated")
    bot.updates = [msg("/book")]
    control.poll_once()
    out = bot.sent[-1]
    assert "BTC short" in out and "SL 79500.0" in out
    assert "RESTING SOL long @ 97.5" in out and "expires" in out


def test_book_is_explicit_when_flat(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/book")]
    control.poll_once()
    assert "flat" in bot.sent[-1]


def test_close_cancels_a_resting_entry(tmp_path):
    eng, state, control, bot = mk(tmp_path, [REST_SOL])
    eng.cycle("scheduled")
    bot.updates = [msg("/close SOL")]
    control.poll_once()
    assert "cancelling the resting entry" in bot.sent[-1]
    assert state.resting_entries() == []


def test_close_refuses_a_market_with_nothing_on_it(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/close BTC")]
    control.poll_once()
    assert "nothing open or resting on BTC" in bot.sent[-1]
    bot.updates = [msg("/close", update_id=2)]
    control.poll_once()
    assert "usage:" in bot.sent[-1]


def test_there_is_no_way_to_OPEN_a_trade_from_chat(tmp_path):
    """Every mutating command reduces risk or asks the analyst to think.
    Opening from a chat window would bypass the analyst and every gate."""
    eng, state, control, bot = mk(tmp_path)
    for attempt in ("/open BTC long", "/buy BTC", "/long BTC 10x"):
        bot.updates = [msg(attempt)]
        control.poll_once()
        assert "unknown command" in bot.sent[-1]
    assert state.open_positions() == []


def test_wake_queues_a_cycle(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/wake")]
    control.poll_once()
    assert bot.sent[-1] == "cycle queued"
    assert eng.wake.is_set()


def test_why_and_memory_read_back_what_it_is_thinking(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    state.record_decision("scheduled", "Risk-off; standing down.", "[]", "m", 0, "ok")
    state.add_lesson("Never chase the range edge.", source="operator", pinned=True)
    bot.updates = [msg("/why")]
    control.poll_once()
    assert "Risk-off; standing down." in bot.sent[-1]
    bot.updates = [msg("/memory", update_id=2)]
    control.poll_once()
    assert "PIN Never chase the range edge." in bot.sent[-1]


def test_the_offset_advances_so_a_command_never_runs_twice(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/pause", update_id=7)]
    assert control.poll_once() == 1
    assert control.offset == 8
    assert control.poll_once() == 0        # same batch, already consumed


def test_a_failing_command_replies_instead_of_dying(tmp_path):
    eng, state, control, bot = mk(tmp_path)

    def boom():
        raise RuntimeError("ledger on fire")

    control.cmd_status = boom
    bot.updates = [msg("/status")]
    control.poll_once()
    assert "command failed" in bot.sent[-1] and "ledger on fire" in bot.sent[-1]


def test_help_lists_the_commands_and_says_what_is_impossible(tmp_path):
    eng, state, control, bot = mk(tmp_path)
    bot.updates = [msg("/start")]
    control.poll_once()
    out = bot.sent[-1]
    for cmd in ("/status", "/book", "/why", "/memory", "/wake", "/pause",
                "/resume", "/close"):
        assert cmd in out
    assert "not possible" in out
