"""Discover the operator's Telegram chat id and write it into config.toml.

Alerts (`notify.chat_id = 0`) go nowhere until this runs. Message the bot once —
`/start` in a DM to @trenchbotta_bot — then:

    uv run python wire_telegram.py            # show what it found
    uv run python wire_telegram.py --yes      # write it into config.toml

Telegram only keeps unacknowledged updates for 24h, so if it finds nothing,
message the bot again and re-run.
"""

import argparse
import json
import pathlib
import re
import sys
import urllib.parse
import urllib.request

from peri.config import load_config


def get_updates(token: str) -> list[dict]:
    url = f"https://api.telegram.org/bot{token}/getUpdates"
    body = json.loads(urllib.request.urlopen(url, timeout=20).read())
    if not body.get("ok"):
        raise SystemExit(f"telegram rejected the token: {body}")
    return body.get("result", [])


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--yes", action="store_true", help="write the id into config.toml")
    ap.add_argument("--chat-id", type=int, help="skip discovery and use this id")
    args = ap.parse_args()

    cfg = load_config("config.toml", ".env")
    if not cfg.tg_bot_token:
        raise SystemExit("TG_BOT_TOKEN is not set in .env")

    chat_id = args.chat_id
    if chat_id is None:
        chats: dict[int, tuple] = {}
        for update in get_updates(cfg.tg_bot_token):
            message = update.get("message") or update.get("edited_message") or {}
            chat = message.get("chat") or {}
            if chat.get("id") and chat.get("type") == "private":
                chats[chat["id"]] = (chat.get("username") or chat.get("first_name"),
                                     (message.get("text") or "")[:40])
        if not chats:
            print("no private chat found. DM @trenchbotta_bot (send /start), then "
                  "re-run. Telegram drops unacknowledged updates after 24h.")
            return 1
        for cid, (who, text) in chats.items():
            print(f"  chat_id={cid}  who={who}  last={text!r}")
        if len(chats) > 1:
            print("\nmore than one private chat — re-run with --chat-id <id>")
            return 1
        chat_id = next(iter(chats))

    print(f"\nchat_id to wire: {chat_id}")
    if not args.yes:
        print("report only — add --yes to write it into config.toml")
        return 0

    path = pathlib.Path("config.toml")
    text = path.read_text()
    updated, n = re.subn(r"(?m)^chat_id = .*$", f"chat_id = {chat_id}", text, count=1)
    if n != 1:
        raise SystemExit("could not find a chat_id line in config.toml")
    path.write_text(updated)
    print("config.toml updated — restart peri and alerts will reach that chat")

    send = (f"https://api.telegram.org/bot{cfg.tg_bot_token}/sendMessage"
            f"?chat_id={chat_id}&text=" + urllib.parse.quote(
                "peri alerts are wired. You will get: entries and exits, stop-outs, "
                "an UNPROTECTED warning if a live position loses its venue brackets, "
                "kill-switch and pause changes, and any execution that needs a human."))
    try:
        urllib.request.urlopen(send, timeout=20)
        print("sent a confirmation message")
    except Exception as exc:  # noqa: BLE001 — the config write is what matters
        print(f"config written, but the test message failed: {exc!r}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
