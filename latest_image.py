"""Precise: find the single most recent photo in the group (reuses peri.session)."""

import asyncio
import os
from datetime import datetime, timezone

from telethon import TelegramClient
from telethon.tl.types import InputMessagesFilterPhotos


def load_env(path=".env"):
    if not os.path.exists(path):
        return
    for line in open(path):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip())


load_env()
API_ID = int(os.environ["TG_API_ID"])
API_HASH = os.environ["TG_API_HASH"]
GROUP_ID = -1001234567890


async def main():
    client = TelegramClient("peri", API_ID, API_HASH)
    await client.connect()
    if not await client.is_user_authorized():
        print("ERROR: not authorized")
        return
    target = None
    async for d in client.iter_dialogs():
        if d.id == GROUP_ID:
            target = d.entity
            break
    async for msg in client.iter_messages(target, filter=InputMessagesFilterPhotos, limit=1):
        sender = await msg.get_sender()
        uname = getattr(sender, "username", None) or getattr(sender, "first_name", None)
        now = datetime.now(timezone.utc)
        ago = now - msg.date
        hrs = ago.total_seconds() / 3600
        print(f"latest_photo_utc = {msg.date.isoformat()}")
        print(f"now_utc          = {now.isoformat()}")
        print(f"ago              = {ago.days}d {hrs % 24:.1f}h  (~{hrs:.1f}h total)")
        print(f"sender           = @{uname}")
        print(f"caption          = {(msg.text or '')!r}")
    await client.disconnect()


asyncio.run(main())
