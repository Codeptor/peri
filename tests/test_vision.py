"""The eye: a posted chart becomes text the analyst reads, or an honest gap."""

import asyncio
import time

import pytest

from peri.feed import Feed
from peri.state import State
from peri.vision import ImageReader, VisionError, describe_image

PNG = b"\x89PNG\r\n\x1a\n" + b"x" * 64


class FakeMessage:
    """The shape telethon hands the feed, with only what read_media touches."""

    def __init__(self, text="", photo=object(), document=None, data=PNG, boom=None):
        self.text, self.photo, self.document = text, photo, document
        self._data, self._boom = data, boom

    async def download_media(self, file=bytes):
        if self._boom:
            raise self._boom
        return self._data


class FakeDoc:
    def __init__(self, mime):
        self.mime_type = mime


def reader(describe=None, **kw):
    return ImageReader("https://x/v1", "key", "m",
                       describe=describe or (lambda *a, **k: "NVDA 4h, 214.30"), **kw)


def feed_with(reader_obj, state=None, callers=("h3rkk",)):
    return Feed(state or State(":memory:"), list(callers), asyncio.Event(),
                reader=reader_obj)


# -- the gap this closes ---------------------------------------------------

def test_captionless_caller_chart_lands_and_wakes():
    """Before the eye, `if not text` dropped this message entirely: no ledger
    row, no wake. The chart WAS the call."""
    f = feed_with(reader())
    woke = f.ingest(1, time.time(), "h3rkk", "", image_desc="NVDA 4h, 214.30")
    assert woke is True and f.wake.is_set()
    row = f.state.tg_message(1)
    assert row["image_desc"] == "NVDA 4h, 214.30" and row["is_caller"] == 1


def test_a_message_with_neither_text_nor_image_is_still_dropped():
    f = feed_with(reader())
    assert f.ingest(2, time.time(), "h3rkk", "") is False
    assert f.state.tg_message(2) is None


def test_editing_a_caption_does_not_re_read_the_picture():
    """A caption edit arrives with image_desc=None. COALESCE must keep what the
    eye already read, or an edit would blank the chart and bill for a re-read."""
    f = feed_with(reader())
    f.ingest(3, 1000.0, "h3rkk", "chart", image_desc="BTC 1h, support 108k")
    assert f.ingest(3, 1001.0, "h3rkk", "chart — long here") is True
    row = f.state.tg_message(3)
    assert row["text"] == "chart — long here"
    assert row["image_desc"] == "BTC 1h, support 108k"


def test_an_unchanged_replay_is_not_fresh():
    f = feed_with(reader())
    assert f.ingest(4, 1000.0, "h3rkk", "", image_desc="d") is True
    assert f.ingest(4, 1000.0, "h3rkk", "", image_desc="d") is False


# -- reading media off a telethon message ----------------------------------

def test_read_media_transcribes_a_photo():
    f = feed_with(reader())
    assert asyncio.run(f.read_media(FakeMessage(text="look"))) == "NVDA 4h, 214.30"


def test_the_caption_is_passed_to_the_eye_as_context():
    seen = {}

    def describe(data, base, key, model, caption="", mime="image/jpeg"):
        seen.update(caption=caption, mime=mime, size=len(data))
        return "read"

    f = feed_with(reader(describe))
    asyncio.run(f.read_media(FakeMessage(text="NVDA breaking out")))
    assert seen == {"caption": "NVDA breaking out", "mime": "image/jpeg",
                    "size": len(PNG)}


@pytest.mark.parametrize("doc,expected", [
    (FakeDoc("image/png"), "NVDA 4h, 214.30"),
    (FakeDoc("video/mp4"), None),
    (FakeDoc("application/pdf"), None),
    (None, None),
])
def test_only_images_reach_the_eye(doc, expected):
    f = feed_with(reader())
    msg = FakeMessage(photo=None, document=doc)
    assert asyncio.run(f.read_media(msg)) == expected


def test_no_reader_means_no_vision_calls_at_all():
    f = feed_with(None)
    assert asyncio.run(f.read_media(FakeMessage())) is None


def test_a_failed_download_does_not_kill_the_feed():
    f = feed_with(reader())
    msg = FakeMessage(boom=OSError("connection reset"))
    assert asyncio.run(f.read_media(msg)) is None


# -- the budget ------------------------------------------------------------

def test_an_oversized_image_is_skipped_not_paid_for():
    calls = []
    r = reader(lambda *a, **k: calls.append(1) or "x", max_bytes=100)
    assert r.read_image(b"y" * 101) is None
    assert calls == []


def test_the_hourly_budget_bounds_a_flood():
    calls = []
    clock = [1000.0]
    r = reader(lambda *a, **k: calls.append(1) or "x", max_per_hour=3,
               now=lambda: clock[0])
    for _ in range(6):
        r.read_image(PNG)
    assert len(calls) == 3
    clock[0] += 3601                      # the window rolls, the budget returns
    assert r.read_image(PNG) == "x"
    assert len(calls) == 4


def test_an_unreadable_image_returns_none_never_a_guess():
    def boom(*a, **k):
        raise VisionError("HTTP 500")

    r = reader(boom)
    assert r.read_image(PNG) is None
    assert r.failed == 1 and r.read == 0


# -- the vision call itself ------------------------------------------------

def test_describe_image_sends_one_image_block_and_returns_facts():
    sent = {}

    def request(url, payload, api_key, timeout):
        sent.update(url=url, payload=payload, api_key=api_key)
        return {"choices": [{"message": {"content": " BTC 1h,\n  support 108000 "}}]}

    out = describe_image(PNG, "https://x/v1/", "k", "qwen", caption="btc",
                         request=request)
    assert out == "BTC 1h, support 108000"
    assert sent["url"] == "https://x/v1/chat/completions"
    assert sent["api_key"] == "k"
    content = sent["payload"]["messages"][0]["content"]
    assert content[1]["image_url"]["url"].startswith("data:image/jpeg;base64,")
    assert "facts only" in content[0]["text"].lower()
    assert 'captioned it: "btc"' in content[0]["text"]
    assert sent["payload"]["temperature"] == 0.0


def test_an_empty_completion_is_an_error_not_an_empty_description():
    def request(url, payload, api_key, timeout):
        return {"choices": [{"message": {"content": "   "}}]}

    with pytest.raises(VisionError):
        describe_image(PNG, "https://x/v1", "k", "m", request=request)


def test_a_transport_failure_becomes_a_vision_error():
    def request(url, payload, api_key, timeout):
        raise TimeoutError("read timed out")

    with pytest.raises(VisionError, match="TimeoutError"):
        describe_image(PNG, "https://x/v1", "k", "m", request=request)


# -- what the analyst is shown --------------------------------------------

def test_the_prompt_shows_the_image_under_its_message():
    import copy

    from peri.analyst import build_prompt
    from tests.test_analyst import BUNDLE

    bundle = copy.deepcopy(BUNDLE)
    bundle["telegram"] = [{
        "msg_id": 9, "ts": 990.0, "sender": "h3rkk", "is_caller": True,
        "text": "", "image_desc": "NVDA 4h, price 214.30, trendline 212"}]
    out = build_prompt(bundle, now=1000.0)
    assert "IMAGE: NVDA 4h, price 214.30, trendline 212" in out


# -- vision must never compete with the trading loop for threads -----------

def test_vision_has_its_own_pool_not_the_shared_default():
    """asyncio.to_thread draws on one executor shared with the engine cycle,
    the price watcher and every API handler. A vision call holds its worker for
    the whole request, so an album dropped in the group must not be able to
    park that pool."""
    r = reader()
    assert r.pool._max_workers == 2
    assert "peri-vision" in r.pool._thread_name_prefix


def test_read_media_uses_that_pool_and_not_to_thread(monkeypatch):
    used = {}
    f = feed_with(reader())

    async def go():
        loop = asyncio.get_running_loop()
        real = loop.run_in_executor

        def spy(executor, fn, *a):
            used["executor"] = executor
            return real(executor, fn, *a)

        monkeypatch.setattr(loop, "run_in_executor", spy)
        monkeypatch.setattr(asyncio, "to_thread", lambda *a, **k:
                            (_ for _ in ()).throw(AssertionError("used the shared pool")))
        return await f.read_media(FakeMessage())

    assert asyncio.run(go()) == "NVDA 4h, 214.30"
    assert used["executor"] is f.reader.pool


def test_the_budget_is_claimed_atomically_under_concurrency():
    """Check-then-append without a lock lets two threads read the same last
    free slot and both take it."""
    import threading
    calls = []
    r = reader(lambda *a, **k: calls.append(1) or "x", max_per_hour=5)
    start = threading.Barrier(12)

    def hit():
        start.wait()
        r.read_image(PNG)

    threads = [threading.Thread(target=hit) for _ in range(12)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert len(calls) == 5
