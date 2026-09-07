"""Offline dry-run replay: run REAL @caller1 messages through the actual pipeline
(regex parser -> risk -> dry-run router) and print what the bot WOULD do. No orders.

Regex handles structured entries with no API key; set LIGHTNING_API_KEY to also
resolve manage/exit phrasing via the LLM fallback.
"""

import asyncio
import os
from types import SimpleNamespace

from telethon import TelegramClient

from peri.app import NoopLLM, SafeLLM
from peri.config import load_config
from peri.listener import Deps, handle_message
from peri.llm import LLMClient
from peri.risk import RiskManager
from peri.router import DryRunAdapter
from peri.state import State

GROUP_ID = -1001234567890
CALLER = "caller1"
N = 60


async def main():
    cfg = load_config()
    cfg.risk.max_concurrent = 999   # relax caps so we see raw parse decisions, not cap noise
    cfg.risk.daily_cap = 999

    client = TelegramClient("peri", cfg.tg_api_id, cfg.tg_api_hash)
    await client.connect()
    if not await client.is_user_authorized():
        raise SystemExit("session not authorized")

    key = os.environ.get("LIGHTNING_API_KEY", "")
    llm = SafeLLM(LLMClient(api_key=key)) if key else NoopLLM()
    state = State(":memory:")
    rec, buf = [], []
    deps = Deps(config=cfg, state=state, risk=RiskManager(cfg.risk, cfg.venues, state),
                router=DryRunAdapter(rec), llm=llm, recorder=rec, notify=buf.append)

    target = None
    async for d in client.iter_dialogs():
        if d.id == GROUP_ID:
            target = d.entity
            break

    msgs = [m async for m in client.iter_messages(target, from_user=CALLER, limit=N) if m.text]
    msgs.reverse()

    print(f"parser={'LLM+regex' if key else 'regex-only'} · replaying {len(msgs)} @{CALLER} messages\n")
    acted = 0
    for m in msgs:
        buf.clear()
        sm = SimpleNamespace(id=m.id, text=m.text, sender_username=CALLER, epoch=m.date.timestamp())
        handle_message(sm, deps, now=m.date.timestamp())
        if buf:
            acted += 1
            head = m.text.replace("\n", " / ")[:58]
            print(f"  {head!r}")
            for line in buf:
                print(f"      -> {line}")

    open_now = ", ".join(f"{p.asset} {p.side}" for p in state.open_positions()) or "none"
    print(f"\n{acted}/{len(msgs)} messages actionable · open positions: {open_now}")
    await client.disconnect()


asyncio.run(main())
