import time

from peri.news import fetch_headlines


class FakeFeed:
    def __init__(self, title, entries):
        self.feed = {"title": title}
        self.entries = entries


def entry(title, mins_ago, now):
    t = time.localtime(now - mins_ago * 60)
    return {"title": title, "published_parsed": t}


def test_headlines_sorted_and_limited():
    now = time.time()

    def parse(url):
        if "a" in url:
            return FakeFeed("Feed A", [entry("old story", 300, now),
                                       entry("fresh story", 5, now)])
        return FakeFeed("Feed B", [entry("mid story", 60, now)])

    out = fetch_headlines(["http://a", "http://b"], 2, parse=parse, now=now)
    assert [h["title"] for h in out] == ["fresh story", "mid story"]
    assert out[0]["source"] == "Feed A"
    assert out[0]["age"].endswith("m")


def test_dead_feed_contained():
    def parse(url):
        if "dead" in url:
            raise OSError("connection refused")
        return FakeFeed("OK", [entry("alive", 1, time.time())])

    out = fetch_headlines(["http://dead", "http://ok"], 5, parse=parse)
    assert [h["title"] for h in out] == ["alive"]


def test_untimed_entries_kept_with_unknown_age():
    def parse(url):
        return FakeFeed("X", [{"title": "no timestamp"}])

    out = fetch_headlines(["http://x"], 5, parse=parse)
    assert out[0]["age"] == "?"


def test_empty_titles_dropped():
    def parse(url):
        return FakeFeed("X", [{"title": "  "}])

    assert fetch_headlines(["http://x"], 5, parse=parse) == []
