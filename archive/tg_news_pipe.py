"""Telegram news channels -> kestreld /ingest/news. Dumb pipe; no trading logic.
Run once interactively to log in:  uv run --with telethon --with httpx python tg_news_pipe.py
"""

import asyncio
import os
import tomllib

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


def channels_from_config(path="kestreld/kestreld.toml") -> list[str]:
    with open(path, "rb") as f:
        return list(tomllib.load(f).get("news", {}).get("tg_channels", []))


def load_signal_groups(path="kestreld/kestreld.toml") -> list[dict]:
    with open(path, "rb") as f:
        data = tomllib.load(f)
    raw = data.get("news", {}).get("tg_signal_groups", [])
    out: list[dict] = []
    for g in raw:
        # g is expected to be {id=int, slug=str, senders=list[str]}
        out.append(
            {
                "id": int(g["id"]),
                "slug": str(g["slug"]),
                "senders": [str(s).lower() for s in g.get("senders", [])],
            }
        )
    return out


def should_forward(sender_username: str | None, senders: list[str]) -> bool:
    if not senders:
        return True
    if sender_username is None:
        return False
    return sender_username.lower() in [s.lower() for s in senders]


def build_payload(channel: str, msg_id: int, ts_ms: int, text: str) -> dict:
    return {
        "source": f"tg:{channel}",
        "ts": ts_ms,
        "title": text[:120],
        "body": text,
        "url": f"https://t.me/{channel}/{msg_id}",
    }


def _group_url(group_id: int, msg_id: int) -> str:
    """Telegram private supergroup link: https://t.me/c/<channelId>/<msg_id>

    group_id is the full Telegram id including -100 prefix, e.g. -1001234567890.
    The public t.me/c link uses the channelId without the -100 prefix:
      channelId = abs(group_id) - 1000000000000  ==  str(abs(group_id))[3:] when leading '100'.
    We strip leading '100' if present (covers all supergroups/channels); otherwise use abs value.
    Documenting choice: we use abs(group_id) stripping '100' rather than raw abs, to match
    Telegram's internal link format (tested for -1001234567890 -> 1234567890).
    """
    abs_s = str(abs(group_id))
    if abs_s.startswith("100"):
        abs_s = abs_s[3:]
    return f"https://t.me/c/{abs_s}/{msg_id}"


def build_group_payload(slug: str, group_id: int, msg_id: int, ts_ms: int, text: str) -> dict:
    return {
        "source": f"tg:{slug}",
        "ts": ts_ms,
        "title": text[:120],
        "body": text,
        "url": _group_url(group_id, msg_id),
    }


async def forward(http: httpx.AsyncClient, payload: dict) -> bool:
    for delay in (0.0, 0.5, 2.0):
        if delay:
            await asyncio.sleep(delay)
        try:
            r = await http.post(INGEST, json=payload, timeout=5)
            if r.status_code == 200:
                return True
        except httpx.HTTPError:
            continue
    print(f"[warn] drop news (kestreld down?): {payload['title'][:60]!r}")
    return False


async def main():
    load_env()
    channels = channels_from_config()
    signal_groups = load_signal_groups()
    if not channels and not signal_groups:
        raise SystemExit("no [news].tg_channels or tg_signal_groups configured in kestreld/kestreld.toml")
    client = TelegramClient("botta_news", int(os.environ["TG_API_ID"]), os.environ["TG_API_HASH"])
    await client.start()  # interactive on first run
    for ch in channels:
        try:
            await client(JoinChannelRequest(ch))
        except Exception as e:
            print(f"[warn] cannot join {ch}: {e}")
    http = httpx.AsyncClient()

    # Resolve private signal groups: iterate dialogs, match by id or slug fallback.
    # Do NOT JoinChannelRequest — private, already member.
    signal_by_dialog_id: dict[int, dict] = {}
    if signal_groups:
        try:
            dialogs: list = []
            async for d in client.iter_dialogs():
                dialogs.append(d)
        except Exception as e:
            print(f"[warn] iter_dialogs failed: {e}")
            dialogs = []
        for g in signal_groups:
            matched = None
            for d in dialogs:
                if d.id == g["id"]:
                    matched = d
                    break
            if matched is None:
                slug_words = g["slug"].replace("_", " ").casefold()
                for d in dialogs:
                    title = getattr(getattr(d, "entity", None), "title", None) or getattr(d, "name", "") or ""
                    if slug_words and slug_words in title.casefold():
                        matched = d
                        break
            if matched is None:
                print(f"[warn] signal group {g['slug']} ({g['id']}) not found — skip")
                continue
            # map dialog id -> group config
            signal_by_dialog_id[matched.id] = g
            title = getattr(getattr(matched, "entity", None), "title", None) or getattr(matched, "name", None) or g["slug"]
            print(f"[ok] listening group '{title}' senders={g['senders']}")

    if channels:

        @client.on(events.NewMessage(chats=channels))
        async def on_channel_msg(event):
            text = event.message.text or ""
            if not text:
                return
            ch = getattr(event.chat, "username", None) or str(event.chat_id)
            await forward(
                http,
                build_payload(ch, event.message.id, int(event.message.date.timestamp() * 1000), text),
            )

    if signal_by_dialog_id:

        @client.on(events.NewMessage(chats=list(signal_by_dialog_id.keys())))
        async def on_group_msg(event):
            text = event.message.text or ""
            if not text:
                return
            grp = signal_by_dialog_id.get(event.chat_id)
            if grp is None:
                # fallback direct id match (handles slug-fallback case where dialog id differs from config id)
                for g in signal_groups:
                    if g["id"] == event.chat_id:
                        grp = g
                        break
                if grp is None:
                    return
            try:
                sender = await event.get_sender()
            except Exception:
                sender = None
            username = getattr(sender, "username", None) if sender else None
            if not should_forward(username, grp["senders"]):
                print(f"[debug] skip group {grp['slug']} msg {event.message.id} sender={username!r}")
                return
            payload = build_group_payload(
                grp["slug"], grp["id"], event.message.id, int(event.message.date.timestamp() * 1000), text
            )
            await forward(http, payload)

    chans_n = len(channels)
    groups_n = len(signal_by_dialog_id)
    print(f"tg_news_pipe running · {chans_n} channels + {groups_n} signal groups -> {INGEST}")
    await client.run_until_disconnected()


if __name__ == "__main__":
    asyncio.run(main())
