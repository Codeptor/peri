import asyncio

from peri.feed import Feed
from peri.state import State
from tests.fixtures.messages import ENTRIES

ENTRY_SOL_LONG = ENTRIES[0]  # real @h3rkk call: "SOL Long\n\nTP: ..."


def mk(tmp_path):
    state = State(str(tmp_path / "t.db"))
    wake = asyncio.Event()
    return Feed(state, ["h3rkk"], wake), state, wake


def test_caller_message_stored_flagged_and_wakes(tmp_path):
    feed, state, wake = mk(tmp_path)
    assert feed.ingest(1, 1000.0, "h3rkk", ENTRY_SOL_LONG) is True
    assert wake.is_set()
    m = state.tg_message(1)
    assert m["is_caller"] == 1 and "SOL" in m["text"]


def test_non_caller_stored_not_waking(tmp_path):
    feed, state, wake = mk(tmp_path)
    assert feed.ingest(2, 1000.0, "rando", "gm frens") is False
    assert not wake.is_set()
    assert state.tg_message(2)["is_caller"] == 0


def test_caller_match_case_insensitive(tmp_path):
    feed, _, wake = mk(tmp_path)
    assert feed.ingest(3, 1000.0, "H3RKK", "BTC short") is True
    assert wake.is_set()


def test_duplicate_message_ignored(tmp_path):
    feed, _, wake = mk(tmp_path)
    feed.ingest(4, 1000.0, "h3rkk", "SOL long")
    wake.clear()
    assert feed.ingest(4, 1000.0, "h3rkk", "SOL long") is False
    assert not wake.is_set()


def test_edited_caller_message_upserts_and_wakes(tmp_path):
    feed, state, wake = mk(tmp_path)
    feed.ingest(7, 1000.0, "h3rkk", "chart soon")
    wake.clear()

    assert feed.ingest(7, 1010.0, "h3rkk", ENTRY_SOL_LONG) is True

    assert wake.is_set()
    message = state.tg_message(7)
    assert message["ts"] == 1010.0
    assert message["text"] == ENTRY_SOL_LONG


def test_attach_subscribes_to_new_and_edited_messages(tmp_path):
    feed, _, _ = mk(tmp_path)

    class Client:
        def __init__(self):
            self.events = []

        def on(self, event):
            self.events.append(event)
            return lambda handler: handler

    client = Client()
    feed.attach(client, -100)

    assert {type(event).__name__ for event in client.events} == {
        "NewMessage", "MessageEdited",
    }


def test_empty_text_and_none_sender(tmp_path):
    feed, state, _ = mk(tmp_path)
    assert feed.ingest(5, 1000.0, "h3rkk", "") is False
    assert state.tg_message(5) is None
    assert feed.ingest(6, 1000.0, None, "anon msg") is False
    assert state.tg_message(6)["is_caller"] == 0


def test_news_channel_ingest(tmp_path):
    feed, state, wake = mk(tmp_path)
    assert feed.ingest_news("WatcherGuru", 5, 1000.0, "JUST IN: fed cuts rates") is True
    assert feed.ingest_news("WatcherGuru", 5, 1000.0, "JUST IN: fed cuts rates") is False
    assert feed.ingest_news("WatcherGuru", 6, 1000.0, "") is False
    assert not wake.is_set()  # news never wakes the engine
