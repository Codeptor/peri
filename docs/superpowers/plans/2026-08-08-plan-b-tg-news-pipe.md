# Plan B: `tg_news_pipe` — Telegram → traderd News Pipe

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Self-contained; read spec `docs/superpowers/specs/2026-08-08-auto-trader-design.md` §4 (`POST /ingest/news` contract). Small plan: one module, two tasks.

**Goal:** A minimal Python sidecar that listens to configured Telegram news channels (MTProto userbot) and forwards every text/caption into traderd's `POST http://127.0.0.1:7411/ingest/news`. It is a dumb pipe: NO parsing, NO trading logic — traderd owns dedupe/matching.

**Tech Stack:** Python 3.12, uv, Telethon, httpx. Lives at repo root `~/botta/tg_news_pipe.py` beside the existing copy-trader (do not modify `src/botta/`).

## Global Constraints

- **Own session file** `botta_news.session` (config name `botta_news`) — NEVER reuse `botta.session` (the copy-trader daemon may hold it; concurrent Telethon use of one session corrupts it). First run performs an interactive login (user's phone) — instruct the user to run it in a real terminal.
- Credentials from repo `.env`: `TG_API_ID`, `TG_API_HASH` (already present). File perms: any file this writes must be 0600.
- Channels list from `traderd/traderd.toml` `[news] tg_channels = [...]` (usernames without @). If the account isn't a member of a channel, JOIN it (public channels) at startup via Telethon `JoinChannelRequest`; log and skip privates it can't join.
- Forward shape (spec §4): `{"source": "tg:<channel>", "ts": <msg unix millis>, "title": "<first 120 chars>", "body": "<full text>", "url": "https://t.me/<channel>/<msg_id>"}`. Fire-and-forget with 2 retries (0.5s/2s backoff); if traderd is down, drop and log at warning — never crash, never queue unbounded.
- `uv run --with telethon --with httpx python tg_news_pipe.py` must be the only invocation needed. Commit after each task; tests via `uv run --group dev pytest tests/test_tg_news_pipe.py -q` from repo root.

### Task 1: The pipe

**Files:** Create `tg_news_pipe.py`, `tests/test_tg_news_pipe.py`

**Interfaces produced:** `build_payload(channel: str, msg_id: int, ts_ms: int, text: str) -> dict` (pure, tested); `async def forward(client_http, payload) -> bool`; `main()` wiring Telethon `events.NewMessage(chats=channels)`.

- [ ] Step 1: Failing tests: `build_payload("WatcherGuru", 55, 1700000000000, "BREAKING: …long text…")` → source `"tg:WatcherGuru"`, title truncated to 120 chars, url `"https://t.me/WatcherGuru/55"`, body full text; `forward` posts JSON to a mocked httpx transport and returns False (no raise) on connect error.
- [ ] Step 2: Implement:

```python
"""Telegram news channels -> traderd /ingest/news. Dumb pipe; no trading logic.
Run once interactively to log in:  uv run --with telethon --with httpx python tg_news_pipe.py
"""
import asyncio, os, tomllib
import httpx
from telethon import TelegramClient, events
from telethon.tl.functions.channels import JoinChannelRequest

INGEST = "http://127.0.0.1:7411/ingest/news"

def load_env(path=".env"):
    for line in open(path):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip())

def channels_from_config(path="traderd/traderd.toml") -> list[str]:
    with open(path, "rb") as f:
        return list(tomllib.load(f).get("news", {}).get("tg_channels", []))

def build_payload(channel: str, msg_id: int, ts_ms: int, text: str) -> dict:
    return {"source": f"tg:{channel}", "ts": ts_ms,
            "title": text[:120], "body": text,
            "url": f"https://t.me/{channel}/{msg_id}"}

async def forward(http: httpx.AsyncClient, payload: dict) -> bool:
    for delay in (0.0, 0.5, 2.0):
        if delay: await asyncio.sleep(delay)
        try:
            r = await http.post(INGEST, json=payload, timeout=5)
            if r.status_code == 200: return True
        except httpx.HTTPError: continue
    print(f"[warn] drop news (traderd down?): {payload['title'][:60]!r}")
    return False

async def main():
    load_env()
    channels = channels_from_config()
    if not channels: raise SystemExit("no [news].tg_channels configured in traderd/traderd.toml")
    client = TelegramClient("botta_news", int(os.environ["TG_API_ID"]), os.environ["TG_API_HASH"])
    await client.start()  # interactive on first run
    for ch in channels:
        try: await client(JoinChannelRequest(ch))
        except Exception as e: print(f"[warn] cannot join {ch}: {e}")
    http = httpx.AsyncClient()

    @client.on(events.NewMessage(chats=channels))
    async def on_msg(event):
        text = event.message.text or ""
        if not text: return
        ch = getattr(event.chat, "username", None) or str(event.chat_id)
        await forward(http, build_payload(ch, event.message.id, int(event.message.date.timestamp() * 1000), text))

    print(f"tg_news_pipe running · {len(channels)} channels -> {INGEST}")
    await client.run_until_disconnected()

if __name__ == "__main__":
    asyncio.run(main())
```

- [ ] Step 3: Tests green (`uv run --group dev pytest tests/test_tg_news_pipe.py -q`). Commit `feat: tg news pipe sidecar`.

### Task 2: Channel list + live validation

- [ ] Step 1: Add to `traderd/traderd.toml`: `[news] tg_channels = ["WatcherGuru", "TreeNewsFeed"]` (starter set — the user will supply more channels + scrapeable sources later; anything else can push via `POST /ingest/news` directly).
- [ ] Step 2: USER-RUN: `uv run --with telethon --with httpx python tg_news_pipe.py` in a terminal (login code → phone). Verify: with traderd running, a new post in any subscribed channel appears in `GET /api/news` within seconds (`curl -s localhost:7411/api/news?limit=3`).
- [ ] Step 3: Commit `chore: starter news channel config`.
