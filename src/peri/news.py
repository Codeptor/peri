"""RSS headlines for the analyst context. A dead feed is skipped and reported in
the bundle as missing — never invented, never fatal (containment, not fallback)."""

import time
from concurrent.futures import ThreadPoolExecutor
from typing import Callable, Optional

import feedparser


def _age_str(now: float, ts: Optional[float]) -> str:
    if ts is None:
        return "?"
    m = int((now - ts) / 60)
    if m < 0:
        m = 0
    return f"{m}m" if m < 120 else f"{m // 60}h"


def fetch_headlines(rss_urls: list[str], limit: int,
                    parse: Callable = feedparser.parse,
                    now: Optional[float] = None) -> list[dict]:
    now = now or time.time()

    def _one(url: str) -> list[dict]:
        out: list[dict] = []
        try:
            feed = parse(url)
            src = (feed.feed.get("title") or url)[:40]
            for e in feed.entries[:20]:
                ts = None
                for key in ("published_parsed", "updated_parsed"):
                    t = e.get(key)
                    if t:
                        ts = time.mktime(t)
                        break
                out.append({"source": src, "title": e.get("title", "").strip(),
                            "ts": ts, "age": _age_str(now, ts)})
        except Exception as exc:  # noqa: BLE001 — one dead feed must not kill the cycle
            print(f"[news] feed error {url}: {exc!r}")
        return out

    items: list[dict] = []
    if rss_urls:
        # feeds are independent network calls — fetch concurrently
        with ThreadPoolExecutor(max_workers=min(8, len(rss_urls))) as pool:
            for chunk in pool.map(_one, rss_urls):
                items.extend(chunk)
    items = [i for i in items if i["title"]]
    items.sort(key=lambda i: i["ts"] or 0, reverse=True)
    return items[:limit]
