"""The eye: turn an image posted in the calls group into text the analyst reads.

Callers do not always type their call. @caller1 posts a TradingView screenshot
with no caption, and until now `Feed.ingest` dropped it on the floor — no
ledger row, no wake, no trace that anything was said. The chart WAS the call.

This runs once, at ingest, not once per cycle. An image posted at 14:00 stays
in the message window for hours; transcribing it into every prompt would pay
for the same picture twenty times and add its tokens to a cycle whose p90 is
already 220s. One description is stored in the ledger and read verbatim from
then on, by the analyst, by /why, and by the dashboard.

It transcribes; it never advises. "NVDA 4h, price 214.30, trendline break at
212" is what the analyst needs — a picture turned into facts. An opinion here
would be a second brain deciding without a single risk gate in front of it.
"""

import base64
import json
import threading
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from typing import Callable, Optional

PROMPT = """You are the eyes of a trading system. Describe this image as FACTS only.

If it is a price chart: name the ticker and timeframe if visible, the current
price, and every level, trendline, or annotation drawn on it, with numbers.
If it is a position or PnL screenshot: coin, side, size, entry, liquidation,
leverage and PnL, exactly as shown.
If it contains text (a tweet, a headline, a news card): transcribe the text
verbatim.
If it carries no trading information (a meme, a reaction, a selfie): say
"no trade information" and nothing else.

Never give an opinion, a forecast, or advice. Never guess a number you cannot
read. Under 80 words."""

# A picture that is not one of these is not something a chat client sent as a photo.
IMAGE_MIME = ("image/png", "image/jpeg", "image/jpg", "image/webp", "image/gif")


class VisionError(RuntimeError):
    """The image could not be read. The message still lands, marked unread."""


def describe_image(data: bytes, base_url: str, api_key: str, model: str,
                   caption: str = "", mime: str = "image/jpeg",
                   timeout: float = 60.0,
                   request: Optional[Callable[..., dict]] = None) -> str:
    """One vision call. Returns a factual description, or raises VisionError."""
    if not data:
        raise VisionError("empty image")
    prompt = PROMPT
    if caption.strip():
        # The caption is context for reading the picture, never a instruction to follow.
        prompt += f'\n\nThe sender captioned it: "{caption.strip()[:200]}"'
    payload = {
        "model": model,
        "max_tokens": 300,
        "temperature": 0.0,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": prompt},
            {"type": "image_url", "image_url": {
                "url": f"data:{mime};base64,{base64.b64encode(data).decode()}"}},
        ]}],
    }
    caller = request or _post
    try:
        body = caller(f"{base_url.rstrip('/')}/chat/completions", payload, api_key, timeout)
        text = (body["choices"][0]["message"]["content"] or "").strip()
    except VisionError:
        raise
    except Exception as exc:  # noqa: BLE001 — every transport failure reads the same
        raise VisionError(f"{type(exc).__name__}: {exc}") from exc
    if not text:
        raise VisionError("the model returned nothing")
    return " ".join(text.split())[:600]


def _post(url: str, payload: dict, api_key: str, timeout: float) -> dict:
    req = urllib.request.Request(
        url, data=json.dumps(payload).encode(),
        headers={"authorization": f"Bearer {api_key}",
                 "content-type": "application/json"})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except urllib.error.HTTPError as exc:
        raise VisionError(f"HTTP {exc.code}: {exc.read()[:200]!r}") from exc


class ImageReader:
    """Bounded vision at the feed edge.

    The budget is the point: this fires on whatever anyone posts in a group the
    operator does not control, so it caps how many images an hour can turn into
    vendor calls and refuses anything too big to be a screenshot. A flood of
    memes must not become a bill.
    """

    def __init__(self, base_url: str, api_key: str, model: str,
                 max_bytes: int = 8 * 1024 * 1024, max_per_hour: int = 30,
                 now: Optional[Callable[[], float]] = None,
                 describe: Optional[Callable[..., str]] = None,
                 workers: int = 2):
        self.base_url, self.api_key, self.model = base_url, api_key, model
        self.max_bytes, self.max_per_hour = max_bytes, max_per_hour
        import time as _time
        self.now = now or _time.time
        self.describe = describe or describe_image
        self.recent: list[float] = []
        self.read = 0
        self.failed = 0
        self.lock = threading.Lock()
        # Vision gets its OWN threads. asyncio.to_thread draws on one default
        # executor shared with the engine cycle, the price watcher, the telegram
        # control poll and every API handler; a vision call holds its worker for
        # up to the request timeout, and the hourly budget caps SPEND, not
        # concurrency. An album dropped in the group could therefore park most
        # of that pool and stall the trading loop. Two workers, permanently.
        self.pool = ThreadPoolExecutor(max_workers=max(1, workers),
                                       thread_name_prefix="peri-vision")

    def _claim_budget(self) -> bool:
        """Claim one slot in the rolling hour. Checking and claiming under one
        lock: two images arriving together must not both read the same free
        slot and both take it."""
        with self.lock:
            cutoff = self.now() - 3600
            self.recent = [t for t in self.recent if t >= cutoff]
            if len(self.recent) >= self.max_per_hour:
                return False
            self.recent.append(self.now())
            return True

    def read_image(self, data: bytes, caption: str = "",
                   mime: str = "image/jpeg") -> Optional[str]:
        """Description, or None with the reason printed. Never raises: an
        unreadable image must still leave a visible message behind."""
        if not data:
            return None
        if len(data) > self.max_bytes:
            print(f"[peri] image skipped: {len(data) / 1e6:.1f}MB over the "
                  f"{self.max_bytes / 1e6:.0f}MB cap", flush=True)
            return None
        if not self._claim_budget():
            print(f"[peri] image skipped: {self.max_per_hour}/hour vision budget spent",
                  flush=True)
            return None
        try:
            text = self.describe(data, self.base_url, self.api_key, self.model,
                                 caption=caption, mime=mime)
        except VisionError as exc:
            self.failed += 1
            print(f"[peri] image unreadable: {exc}", flush=True)
            return None
        self.read += 1
        print(f"[peri] image read: {text[:90]!r}", flush=True)
        return text
