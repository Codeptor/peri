"""Price-triggered wakes: the analyst must see a move as it starts."""

from peri.config import WatchCfg
from peri.market import Ctx
from peri.pricewatch import PriceWatcher


class Clock:
    def __init__(self, t=1_000_000.0):
        self.t = t

    def __call__(self):
        return self.t

    def tick(self, secs):
        self.t += secs


class Marks:
    def __init__(self, marks):
        self.marks = dict(marks)

    def ctxs(self):
        return {n: Ctx(n, px, 0.0, 0.0, 0.0, 0.0) for n, px in self.marks.items()}


def mk(move_pct=1.5, window=300, rewake=900, breakout=True, features=None):
    clock = Clock()
    market = Marks({"BTC": 80000.0, "SOL": 100.0})
    woke = []
    watcher = PriceWatcher(
        market, WatchCfg(True, 30, window, move_pct, rewake, breakout, 0.3),
        wake=woke.append,
        universe=lambda: features if features is not None else {"BTC": {}, "SOL": {}},
        now=clock,
    )
    return watcher, market, clock, woke


def test_a_quiet_tape_never_wakes_the_analyst():
    w, market, clock, woke = mk()
    for _ in range(20):
        clock.tick(30)
        market.marks["BTC"] *= 1.0002      # +0.02% a poll — drift, not a move
        w.poll()
    assert woke == []


def test_a_fast_move_wakes_the_analyst_with_the_reason():
    w, market, clock, woke = mk()
    for _ in range(10):                     # fill the window at a flat price
        clock.tick(30)
        w.poll()
    market.marks["BTC"] = 78400.0           # -2% inside the window
    clock.tick(30)
    w.poll()
    assert len(woke) == 1
    assert "price move: BTC -2.00% in 5m" in woke[0]


def test_a_partial_window_is_never_judged():
    w, market, clock, woke = mk()
    clock.tick(30)
    w.poll()
    market.marks["BTC"] = 78400.0
    clock.tick(30)                          # only 60s of history, window is 300s
    w.poll()
    assert woke == []


def test_one_wake_per_market_per_debounce():
    w, market, clock, woke = mk(rewake=900)
    for _ in range(10):
        clock.tick(30)
        w.poll()
    market.marks["BTC"] = 78400.0
    clock.tick(30)
    w.poll()
    for _ in range(10):                     # still down, still inside the debounce
        clock.tick(30)
        w.poll()
    assert len(woke) == 1
    for _ in range(31):                     # ride out the debounce at the new level
        clock.tick(30)
        w.poll()
    assert len(woke) == 1
    market.marks["BTC"] = 76800.0           # a second leg down, debounce expired
    clock.tick(30)
    w.poll()
    assert len(woke) == 2 and "76800" in woke[1]


def test_a_24h_breakout_wakes_even_without_a_fast_move():
    w, market, clock, woke = mk(features={"BTC": {"hi_24h": 80500.0, "lo_24h": 76000.0}})
    clock.tick(30)
    w.poll()
    market.marks["BTC"] = 80900.0          # 0.50% clear of the high
    clock.tick(30)
    w.poll()
    assert len(woke) == 1 and "cleared its 24h high" in woke[0]


def test_only_markets_the_analyst_is_watching_are_polled():
    w, market, clock, woke = mk(features={"SOL": {}})
    for _ in range(10):
        clock.tick(30)
        w.poll()
    market.marks["BTC"] = 60000.0           # -25%, but BTC is not in the universe
    clock.tick(30)
    w.poll()
    assert woke == []
    assert "BTC" not in w.history


def test_a_gap_in_polling_needs_a_fresh_window_before_judging():
    """After an outage the watcher has one sample; it waits for real history
    rather than comparing prices minutes apart."""
    w, market, clock, woke = mk()
    for _ in range(10):
        clock.tick(30)
        w.poll()
    clock.tick(3600)                        # the poller was down for an hour
    market.marks["BTC"] = 78400.0
    w.poll()
    assert woke == []
    for _ in range(10):
        clock.tick(30)
        w.poll()
    market.marks["BTC"] = 76800.0
    clock.tick(30)
    w.poll()
    assert len(woke) == 1


def test_a_breakout_must_clear_the_level_by_a_margin():
    """2026-08-30: a name grinding higher re-broke the high it had just set,
    poll after poll — 64 wakes, 61 of them under 0.2% past the level, one at
    0.5bp. 1.8 hours of analyst time and not one profitable trade."""
    w, market, clock, woke = mk(features={"BTC": {"hi_24h": 80000.0, "lo_24h": 76000.0}})
    clock.tick(30)
    w.poll()
    for marginal in (80001.0, 80040.0, 80200.0):        # 0.001% .. 0.25% past
        market.marks["BTC"] = marginal
        clock.tick(30)
        w.poll()
    assert woke == []

    market.marks["BTC"] = 80400.0                        # 0.5% past — a real break
    clock.tick(30)
    w.poll()
    assert len(woke) == 1
    assert "cleared its 24h high" in woke[0] and "0.50%" in woke[0]


def test_a_low_break_needs_the_same_margin():
    w, market, clock, woke = mk(features={"BTC": {"hi_24h": 80000.0, "lo_24h": 76000.0}})
    clock.tick(30)
    w.poll()
    market.marks["BTC"] = 75990.0                        # 0.01% below — noise
    clock.tick(30)
    w.poll()
    assert woke == []
    market.marks["BTC"] = 75600.0                        # 0.53% below — real
    clock.tick(30)
    w.poll()
    assert len(woke) == 1 and "broke its 24h low" in woke[0]
