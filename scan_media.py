"""Non-interactive media scan of the calls group (reuses peri.session).

Reports image messages (sender + caption), and downloads @caller1's recent photos
to media/ so we can see whether he posts trade info as screenshots.
"""

import asyncio
import os

from telethon import TelegramClient


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
CALLER = "caller1"


async def main():
    client = TelegramClient("peri", API_ID, API_HASH)
    await client.connect()
    if not await client.is_user_authorized():
        print("ERROR: session not authorized — run fetch_messages.py in a terminal first.")
        return

    target = None
    async for d in client.iter_dialogs():
        if d.id == GROUP_ID:
            target = d.entity
            break
    if target is None:
        print("ERROR: group not in dialogs")
        return

    os.makedirs("media", exist_ok=True)
    total = photos = from_caller = 0
    caller_photos = []
    async for msg in client.iter_messages(target, limit=400):
        total += 1
        is_img = bool(msg.photo) or (
            msg.document and (getattr(msg.document, "mime_type", "") or "").startswith("image")
        )
        if not is_img:
            continue
        photos += 1
        sender = await msg.get_sender()
        uname = getattr(sender, "username", None) or getattr(sender, "first_name", None) or str(msg.sender_id)
        cap = (msg.text or "").replace("\n", " / ")
        print(f"[IMG] {msg.date:%m-%d %H:%M} @{uname}  caption: {cap!r}")
        if (uname or "").lower() == CALLER:
            from_caller += 1
            caller_photos.append(msg)

    for msg in caller_photos[:6]:
        path = await msg.download_media(file=f"media/caller1_{msg.id}.jpg")
        print(f"  downloaded {path}")

    print(f"\nSCANNED {total} msgs · {photos} image msgs · {from_caller} from @{CALLER}")
    await client.disconnect()


asyncio.run(main())
