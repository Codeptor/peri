"""Price-triggered wakes.

The 15-minute cycle is why peri kept arriving late: by the time a scheduled
decision ran, the move it was reacting to had already happened, and the only
entry left was a chase. This watcher polls marks cheaply (one info call per dex,
no candles) and wakes the analyst the moment a market it already cares about
starts moving — so the decision happens at the start of the move, not after it.

It never trades. Its whole output is a wake with a reason attached.
"""

import time
from collections import deque
from typing import Callable, Optional


class PriceWatcher:
    def __init__(self, market, cfg, wake: Callable[[str], object],
                 universe: Callable[[], dict], now: Optional[Callable[[], float]] = None):
        """`universe` returns {market: features} from the last bundle — the exact
        set the analyst is already considering, with hi/lo levels for breakouts."""
        self.market = market
        self.cfg = cfg
        self.wake = wake
        self.universe = universe
        self.now = now or time.time
        self.history: dict[str, deque] = {}
        self.last_wake: dict[str, float] = {}
        self.polls = 0
        self.errors = 0

    def _watched(self, ctxs: dict) -> list[str]:
        names = [n for n in self.universe() if n in ctxs]
        if names:
            return names
        allow = getattr(self.cfg, "native_allow", None) or []
        return [n for n in allow if n in ctxs]

    def poll(self) -> list[str]:
        """One sweep. Returns the markets that triggered a wake."""
        ctxs = self.market.ctxs()
        now = self.now()
        features = self.universe()
        window = self.cfg.window_secs
        fired = []
        for name in self._watched(ctxs):
            mark = ctxs[name].mark
            if not mark or mark <= 0:
                continue
            series = self.history.setdefault(name, deque())
            series.append((now, mark))
            while series and now - series[0][0] > window:
                series.popleft()
            reason = self._trigger(name, mark, series, features.get(name) or {}, window)
            if reason is None:
                continue
            if now - self.last_wake.get(name, 0.0) < self.cfg.rewake_secs:
                continue
            self.last_wake[name] = now
            fired.append(name)
            self.wake(f"price move: {reason}")
        self.polls += 1
        return fired

    def _trigger(self, name: str, mark: float, series: deque, features: dict,
                 window: int) -> Optional[str]:
        if len(series) >= 2:
            oldest_ts, oldest = series[0]
            # only judge a full window: a fresh series would compare seconds apart
            if oldest > 0 and self.now() - oldest_ts >= window * 0.8:
                move = (mark / oldest - 1) * 100
                if abs(move) >= self.cfg.move_pct:
                    return (f"{name} {move:+.2f}% in {int(window / 60)}m "
                            f"({oldest:g} -> {mark:g})")
        if self.cfg.breakout:
            # A break must CLEAR the level by a margin. Without one, a market
            # grinding higher re-breaks the high it just set, poll after poll —
            # 2026-08-30 produced 64 such wakes, 61 of them under 0.2% past the
            # level, for 1.8 hours of analyst time and no profitable trade.
            margin = getattr(self.cfg, "breakout_pct", 0.3) / 100.0
            hi, lo = features.get("hi_24h"), features.get("lo_24h")
            if isinstance(hi, (int, float)) and hi > 0 and mark > hi * (1 + margin):
                return (f"{name} cleared its 24h high {hi:g} by "
                        f"{(mark / hi - 1) * 100:.2f}% (now {mark:g})")
            if isinstance(lo, (int, float)) and lo > 0 and mark < lo * (1 - margin):
                return (f"{name} broke its 24h low {lo:g} by "
                        f"{(1 - mark / lo) * 100:.2f}% (now {mark:g})")
        return None

    async def run(self) -> None:
        import asyncio
        while True:
            try:
                await asyncio.to_thread(self.poll)
            except Exception as exc:  # noqa: BLE001 — a dead poll never kills the daemon
                self.errors += 1
                if self.errors in (1, 10, 100):
                    print(f"[peri] price watch error ({self.errors}): {exc!r}", flush=True)
            await asyncio.sleep(self.cfg.poll_secs)
