"""peri daemon wiring. `uv run peri` starts the loop; `uv run peri --once`
runs a single decision cycle (no telegram feed) — the smoke path."""

import argparse
import asyncio
import os
import signal
import sys
from typing import Callable

from peri.analyst import Analyst, SearchTools
from peri.config import Config, load_config
from peri.engine import Engine
from peri.feed import Feed
from peri.market import Market, http_post
from peri.notifier import Notifier
from peri.risk import Guard
from peri.router import DryRunAdapter
from peri.state import State


def build_engine(cfg: Config, wake: asyncio.Event) -> Engine:
    from peri.fees import HL_SCHEDULE, ZERO
    state = State("peri.db")
    if cfg.venue == "lighter":
        from peri.lighter_market import LighterMarket
        market = LighterMarket(cfg.lighter.host)
        fee_schedule = ZERO
    else:
        market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
        fee_schedule = HL_SCHEDULE
    notifier = Notifier(cfg.tg_bot_token, cfg.notify.chat_id)
    guard = Guard(cfg.risk, state, cfg.analyst.conviction_min, fees=fee_schedule)
    analyst = Analyst(cfg.analyst, cfg.analyst_api_key, cfg.analyst_base_url,
                      cfg.analyst_model, cfg.analyst.conviction_min, cfg.risk.min_rr,
                      cfg.risk.max_leverage,
                      tools=SearchTools(cfg.tavily_api_key, cfg.exa_api_key),
                      risk_pct=cfg.risk.risk_pct,
                      tp_floor=cfg.risk.tp_net_floor_usd,
                      rails=cfg.risk,
                      fallback_model=cfg.analyst_fallback_model)

    if cfg.mode == "live":
        if cfg.venue == "lighter":
            from peri.lighter_adapter import LighterAdapter, SdkOps
            from peri.lighter_sync import Bridge
            if not cfg.lighter_api_key:
                raise RuntimeError(
                    "live lighter mode needs LIGHTER_API_KEY in .env — "
                    "generate it at https://app.lighter.xyz/apikeys, index 4+")
            ops = SdkOps(cfg.lighter.host, cfg.lighter.chain_id,
                         cfg.lighter.account_index, cfg.lighter.api_key_index,
                         cfg.lighter_api_key)
            adapter = LighterAdapter(
                ops, Bridge(), cfg.lighter.account_index, market,
                slippage=cfg.risk.slippage_pct / 100)
        else:
            from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
            from peri.hl_client import build_clients
            exchange, info, acct = build_clients(cfg)
            builder = TRENCH_BUILDER if cfg.route_builder_fee else None
            adapter = HyperliquidAdapter(exchange, info, acct, market, builder=builder,
                                         slippage=cfg.risk.slippage_pct / 100,
                                         dexes=cfg.universe.dexes)
            if cfg.hl_network == "mainnet":
                adapter.enable_dex_abstraction()
    else:
        adapter = DryRunAdapter(state, cfg.risk.paper_bankroll)

    return Engine(cfg, state, market, analyst, guard, adapter, notifier, wake,
                  fees=fee_schedule)


async def main(once: bool) -> None:
    cfg = load_config()
    wake = asyncio.Event()
    engine = build_engine(cfg, wake)

    if once:
        engine.startup()
        engine.cycle("manual --once")
        return

    from telethon import TelegramClient
    client = TelegramClient("peri", cfg.tg_api_id, cfg.tg_api_hash)
    await client.connect()
    if not await client.is_user_authorized():
        raise SystemExit("telegram session not authorized — run fetch_messages.py "
                         "in a terminal first")
    reader = None
    if cfg.telegram.read_images and cfg.analyst_api_key:
        from peri.vision import ImageReader
        reader = ImageReader(cfg.analyst_base_url, cfg.analyst_api_key,
                             cfg.telegram.vision_model or cfg.analyst_model,
                             max_bytes=int(cfg.telegram.max_image_mb * 1024 * 1024),
                             max_per_hour=cfg.telegram.vision_max_per_hour)
        print(f"[peri] images read by {reader.model} "
              f"(<={cfg.telegram.max_image_mb:g}MB, "
              f"{cfg.telegram.vision_max_per_hour}/hour)", flush=True)
    # Route the feed's wake through request_wake so a caller message is LABELLED
    # as one. Setting the raw Event left _wake_trigger at whatever a pending
    # price wake had written, and `caller_wake_skip_tools` matches the trigger
    # string exactly — so the scalp-call fast path was silently lost whenever a
    # price wake was already queued.
    class _CallerWake:
        @staticmethod
        def set() -> None:
            engine.request_wake("caller message")

    feed = Feed(engine.state, cfg.telegram.callers, _CallerWake(), reader=reader)
    feed.attach(client, cfg.telegram.group_id)
    # telethon matches username chat filters only for resolved entities —
    # resolve explicitly and loudly skip any channel that won't resolve
    from telethon.tl.functions.channels import JoinChannelRequest
    resolved = []
    for ch in cfg.news.tg_channels:
        try:
            ent = await client.get_entity(ch)
            # updates are only delivered for channels the account is IN —
            # join (idempotent for already-joined public channels)
            try:
                await client(JoinChannelRequest(ent))
            except Exception as e:  # noqa: BLE001 — already-in / flood → still attach
                print(f"[peri] join {ch}: {e!r}")
            # seed the freshest posts so the brain has news from boot, and so a
            # dead live-event path can never hide (seeded ids visible vs live ids)
            try:
                name = getattr(ent, "username", None) or getattr(ent, "title", ch)
                for m in await client.get_messages(ent, limit=3):
                    if m.text:
                        feed.ingest_news(name, m.id, m.date.timestamp(), m.text)
            except Exception as e:  # noqa: BLE001
                print(f"[peri] seed {ch}: {e!r}")
            resolved.append(ent)
        except Exception as e:  # noqa: BLE001 — one bad channel must not kill boot
            print(f"[peri] news channel unresolvable, skipped: {ch}: {e!r}")
    feed.attach_news(client, resolved)
    await client.catch_up()  # pull any updates missed while offline
    print(f"[peri] news channels attached: {len(resolved)}/{len(cfg.news.tg_channels)}")

    tasks = [engine.run(), client.run_until_disconnected()]
    if cfg.watch.enabled:
        from peri.pricewatch import PriceWatcher
        watcher = PriceWatcher(
            engine.market, cfg.watch,
            wake=lambda reason: engine.request_wake(reason),
            universe=lambda: engine._features_cache,
        )
        watcher.cfg.native_allow = cfg.universe.native_allow
        tasks.append(watcher.run())
        print(f"[peri] price watch on: every {cfg.watch.poll_secs}s, "
              f"wake at {cfg.watch.move_pct:g}% / {cfg.watch.window_secs // 60}m",
              flush=True)

    if cfg.tg_bot_token and cfg.telegram.control_user:
        from peri.control import TelegramControl
        control = TelegramControl(engine, cfg.tg_bot_token, cfg.telegram.control_user)
        tasks.append(control.run())
        print(f"[peri] telegram controls on for user {cfg.telegram.control_user} "
              "(/status /book /why /memory /wake /pause /resume /close)", flush=True)

    from peri.api import api_port, serve
    tasks.append(serve(engine.state, cfg, api_port(cfg), version="peri",
                       request_wake=engine.request_wake,
                       live_snapshot=engine.dashboard_snapshot,
                       runtime_status=engine.runtime_status,
                       realized_status=engine.realized_status,
                       chat_message=engine.chat,
                       confirm_trade=engine.confirm_trade,
                       set_paused=engine.set_paused,
                       resolve_execution=engine.resolve_execution,
                       bias_snapshot=engine.bias_snapshot))
    await _run_until_signalled(tasks, engine)


SHUTDOWN_GRACE_SECS = 30


def _hard_exit() -> None:
    """Leave now, from a point already proven safe.

    Returning from the coroutine is not enough: asyncio.run then joins the
    default executor, and a to_thread call cannot be interrupted — the engine
    cycle's analyst request and the telegram control long-poll both live there.
    Waiting for them took 100s on 2026-08-31 and earned a SIGKILL anyway.

    Every ledger write is committed as it happens and the execution lock has
    already confirmed no order is half-recorded, so there is nothing left to
    flush but the console."""
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(0)


async def _run_until_signalled(tasks: list, engine,
                               on_quiesced: Callable[[], None] = _hard_exit) -> None:
    """Run the daemon, and leave BETWEEN trade actions rather than mid-flight.

    Python's default SIGTERM handler kills the process where it stands. That is
    fine for a web server and wrong for this one: a resting entry is placed at
    the venue on one line and written to the ledger on the next, and a kill in
    between leaves a live order with brackets that no ledger row owns — nothing
    expires it, pause cannot cancel it, and if it fills the position is adopted
    as external and goes unmanaged. systemd sends SIGTERM on every restart, so
    that window opened on every deploy.

    Taking the EXECUTION lock is the whole point: it is held across the venue
    round trip and the ledger write that records it, so acquiring it means no
    order is half-recorded. Not the trade lock — that spans a whole cycle, LLM
    call included, so waiting on it just times out and earns a SIGKILL, which
    is the thing being avoided. An analyst call interrupted mid-thought has
    placed nothing and costs nothing.

    The recovery path in the engine covers a kill we do NOT get to handle; this
    covers the one we do.
    """
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    caught: list[str] = []
    for sig in (signal.SIGTERM, signal.SIGINT):
        try:
            loop.add_signal_handler(sig, lambda s=sig: (caught.append(s.name),
                                                        stop.set()))
        except (NotImplementedError, RuntimeError):  # pragma: no cover — non-unix
            pass

    running = [asyncio.ensure_future(t) for t in tasks]
    waiter = asyncio.ensure_future(stop.wait())
    try:
        await asyncio.wait([*running, waiter], return_when=asyncio.FIRST_COMPLETED)
        if not stop.is_set():
            # a task exited on its own — surface its exception, do not swallow it
            for task in running:
                if task.done() and not task.cancelled() and task.exception():
                    raise task.exception()
            return
        print(f"[peri] {caught[-1] if caught else 'signal'} — waiting up to "
              f"{SHUTDOWN_GRACE_SECS}s for any in-flight trade action", flush=True)

        def _quiesce() -> bool:
            """An RLock is owned by the thread that took it, so acquire and
            release must happen in the SAME one — releasing from the event loop
            after acquiring in a worker raises, which would turn every clean
            shutdown into a crash."""
            if engine._execution_lock.acquire(True, SHUTDOWN_GRACE_SECS):
                engine._execution_lock.release()
                return True
            return False

        acquired = await asyncio.to_thread(_quiesce)
        if acquired:
            print("[peri] clean shutdown — nothing was mid-flight", flush=True)
        else:
            print(f"[peri] WARNING: an order was still being recorded after "
                  f"{SHUTDOWN_GRACE_SECS}s; exiting anyway. Recovery adopts a live "
                  "venue order with no ledger row on the next start.", flush=True)
        for task in running:
            task.cancel()
        await asyncio.gather(*running, return_exceptions=True)
        on_quiesced()
    finally:
        waiter.cancel()
        for task in running:
            task.cancel()
        await asyncio.gather(*running, waiter, return_exceptions=True)


def run() -> None:
    ap = argparse.ArgumentParser(prog="peri")
    ap.add_argument("--once", action="store_true",
                    help="single decision cycle, no telegram feed (smoke)")
    args = ap.parse_args()
    asyncio.run(main(args.once))


if __name__ == "__main__":
    run()
