import json

import httpx
import tg_news_pipe


def test_build_payload():
    long_text = "BREAKING: " + "x" * 200
    payload = tg_news_pipe.build_payload("WatcherGuru", 55, 1700000000000, long_text)
    assert payload["source"] == "tg:WatcherGuru"
    assert payload["ts"] == 1700000000000
    assert payload["title"] == long_text[:120]
    assert len(payload["title"]) == 120
    assert payload["body"] == long_text
    assert payload["body"] == long_text  # full text
    assert payload["url"] == "https://t.me/WatcherGuru/55"


async def test_forward_success():
    captured = {}

    def handler(request: httpx.Request):
        captured["json"] = json.loads(request.content)
        assert request.url.path == "/ingest/news"
        return httpx.Response(200, json={"ok": True})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as client:
        payload = tg_news_pipe.build_payload("WatcherGuru", 55, 1700000000000, "hello world")
        result = await tg_news_pipe.forward(client, payload)
        assert result is True
        assert captured["json"] == payload


async def test_forward_connect_error():
    def handler(request: httpx.Request):
        raise httpx.ConnectError("boom", request=request)

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(transport=transport) as client:
        payload = tg_news_pipe.build_payload("WatcherGuru", 55, 1700000000000, "hello")
        result = await tg_news_pipe.forward(client, payload)
        assert result is False


def test_load_signal_groups(tmp_path):
    toml_text = """
[news]
tg_channels = ["WatcherGuru"]
tg_signal_groups = [{ id = -1001234567890, slug = "trenchers_den", senders = ["h3rkk"] }]
"""
    p = tmp_path / "t.toml"
    p.write_text(toml_text)
    groups = tg_news_pipe.load_signal_groups(str(p))
    assert len(groups) == 1
    g = groups[0]
    assert g["id"] == -1001234567890
    assert g["slug"] == "trenchers_den"
    assert g["senders"] == ["h3rkk"]


def test_should_forward():
    assert tg_news_pipe.should_forward("h3rkk", ["h3rkk"]) is True
    assert tg_news_pipe.should_forward("H3RKK", ["h3rkk"]) is True
    assert tg_news_pipe.should_forward("other", ["h3rkk"]) is False
    assert tg_news_pipe.should_forward(None, ["h3rkk"]) is False
    assert tg_news_pipe.should_forward("anyone", []) is True
    assert tg_news_pipe.should_forward(None, []) is True


def test_group_payload_source_and_url():
    # group payload must use slug and t.me/c link without -100 prefix
    payload = tg_news_pipe.build_group_payload("trenchers_den", -1001234567890, 123, 1700000000000, "Setup 2 SOL Short Entry: 75.5 SL: 75.9 TP: 74.6")
    assert payload["source"] == "tg:trenchers_den"
    assert payload["url"] == "https://t.me/c/1234567890/123"
    assert payload["ts"] == 1700000000000
    assert payload["title"] == "Setup 2 SOL Short Entry: 75.5 SL: 75.9 TP: 74.6"[:120]
    assert payload["body"].startswith("Setup 2")
    # also verify helper _group_url
    assert tg_news_pipe._group_url(-1001234567890, 999) == "https://t.me/c/1234567890/999"
