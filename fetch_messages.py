"""Read-only discovery probe: log in, list your groups, dump recent messages.

Does NOT trade anything. Reads messages so we can see the real call formats and
identify the callers before writing the bot. Creates a `peri.session` file that
the real bot will reuse later.

Run:  uv run --with telethon python fetch_messages.py
"""

import asyncio
import os
from collections import Counter

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
DUMP_LIMIT = 300  # how many recent messages to pull from the chosen group


async def main():
    client = TelegramClient("peri", API_ID, API_HASH)
    await client.start()  # interactive: asks for phone number + login code (+ 2FA)

    me = await client.get_me()
    handle = f"@{me.username}" if me.username else me.first_name
    print(f"\n✓ Logged in as {handle}\n")

    # List groups / channels you're in
    dialogs = []
    async for d in client.iter_dialogs():
        if d.is_group or d.is_channel:
            dialogs.append(d)

    print("Your groups & channels:\n")
    for i, d in enumerate(dialogs):
        kind = "group" if d.is_group else "channel"
        print(f"  [{i:>2}] {d.name}  ({kind}, id={d.id})")

    raw = input("\nEnter the number of the calls group to dump: ").strip()
    target = dialogs[int(raw)]
    print(f"\nDumping last {DUMP_LIMIT} messages from “{target.name}” …")

    lines = []
    senders = Counter()
    async for msg in client.iter_messages(target.entity, limit=DUMP_LIMIT):
        if not msg.text:
            continue
        sender = await msg.get_sender()
        if sender is None:
            uname = "channel"
        else:
            uname = getattr(sender, "username", None) or getattr(sender, "first_name", None) or str(msg.sender_id)
        senders[uname] += 1
        ts = msg.date.strftime("%Y-%m-%d %H:%M")
        text = msg.text.replace("\n", " ⏎ ")
        lines.append(f"{ts} | @{uname} | {text}")

    lines.reverse()  # oldest first, reads like the chat

    with open("messages_dump.txt", "w") as f:
        f.write(f"# {target.name}  (id={target.id})\n")
        f.write(f"# {len(lines)} text messages\n\n")
        f.write("## most active senders\n")
        for name, n in senders.most_common(15):
            f.write(f"#   @{name}: {n}\n")
        f.write("\n## messages (oldest first)\n")
        f.write("\n".join(lines))

    print(f"\n✓ Wrote {len(lines)} messages to messages_dump.txt")
    print("  Top senders:", ", ".join(f"@{n}({c})" for n, c in senders.most_common(6)))
    await client.disconnect()


if __name__ == "__main__":
    asyncio.run(main())
