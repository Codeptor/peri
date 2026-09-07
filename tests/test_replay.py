"""The exit-parameter replay: re-score the real trades, without the tape.

27 closed trades, net -$24.98, per-close sd around $10 — so the sd of the whole
run is about $52 and the entire loss sits inside one standard deviation. Nothing
in that record is a finding, and tuning an exit by watching three trades a day on
a $34 account is waiting, not measuring.

These tests pin the mechanics against hand-computable candles (the kestrel
harness's golden-run discipline), and pin the two caveats that make a report
readable: exits only, and the noise bar.
"""

import math

from peri.config import RiskCfg
from peri.replay import (CandleCache, Report, Trade, format_report, run,
                         simulate)


def cfg(**over) -> RiskCfg:
    base = dict(risk_pct=5.0, max_leverage=20.0, max_concurrent=3,
                daily_entry_cap=3, kill_switch_pct=15.0, min_rr=2.0,
                stale_call_secs=900, cooldown_secs=3600, stop_cooldown_secs=14400,
                min_notional=10.0, slippage_pct=5.0, paper_bankroll=1000.0)
    base.update(over)
    return RiskCfg(**base)


def trade(**over) -> Trade:
    base = dict(id=1, market="SOL", side="long", entry_px=100.0, size=1.0,
                init_stop_px=96.0, tp_px=108.0, opened_ts=0.0, closed_ts=None,
                realized_pnl=None, entry_style="market", entry_atr_pct=0.5)
    base.update(over)
    return Trade(**base)


def bars(path, *, start_ms=60_000, step_ms=60_000):
    """One 1m candle per price, each spanning [p-0.05, p+0.05]."""
    return [{"t": start_ms + i * step_ms, "o": p, "h": p + 0.05,
             "l": p - 0.05, "c": p, "v": 1.0}
            for i, p in enumerate(path)]


def test_a_losing_trade_books_its_initial_stop():
    out = simulate(trade(), bars([99, 98, 97, 95]), cfg())
    assert out.exit_kind == "initial_stop"
    assert out.exit_px == 96.0
    assert out.r < -0.9                     # a full -1R plus fees


def test_a_runaway_trade_reaches_the_target():
    out = simulate(trade(), bars([102, 105, 109]), cfg(scale_out_at_r=0.0))
    assert out.exit_kind == "tp" and out.exit_px == 108.0
    assert out.r > 1.9


def test_the_stop_is_checked_before_the_target_within_a_candle():
    """A bar spanning both brackets is booked as the loss — the conservative
    reading, and the one the kestrel harness used."""
    spanning = [{"t": 60_000, "o": 100, "h": 109.0, "l": 95.0, "c": 100, "v": 1}]
    assert simulate(trade(), spanning, cfg()).exit_kind == "initial_stop"


def test_the_old_geometry_cuts_a_winner_at_a_quarter_of_an_r():
    """The defect, reproduced. Trail armed at +0.5R with a raw 1x-ATR band
    against a 4x-ATR stop: price runs to +0.5R, pulls back, and the trade is
    closed for a fraction of an R while a loser would have paid the full one."""
    path = bars([100.5, 101.5, 102.0, 101.0, 100.2, 99.5])
    old = simulate(trade(), path,
                   cfg(trail_start_r=0.5, trail_atr_mult=1.0,
                       trail_giveback_r=0.0, breakeven_at_r=1.0,
                       scale_out_at_r=0.0, atr_stop_mult=4.0))
    assert old.exit_kind == "trail_stop"
    assert 0.0 < old.r < 0.45, old.r


OLD = dict(trail_start_r=0.5, trail_atr_mult=1.0, trail_giveback_r=0.0,
           breakeven_at_r=1.0, scale_out_at_r=0.0)
NEW = dict(trail_start_r=1.0, trail_atr_mult=1.0, trail_giveback_r=0.5,
           breakeven_at_r=1.0, scale_out_at_r=0.0)


def test_the_new_geometry_survives_a_shakeout_the_old_one_is_stopped_by():
    """The actual mechanism, and it is narrower than it first looks.

    With a 4% risk and ATR15m 0.5%, a 1x-ATR band is 0.125R — four times
    tighter than a 0.5R giveback. The gain is NOT that the wide band returns
    less at the end; it returns more. The gain is that an ordinary mid-trend
    pullback (here 0.7, wider than 1x ATR but well inside 0.5R) takes the tight
    trail out at 104.5 and it misses the run to the target entirely."""
    path = bars([101, 103, 105, 104.3, 106, 109, 112, 111.8])
    old = simulate(trade(), path, cfg(**OLD))
    new = simulate(trade(), path, cfg(**NEW))
    assert old.exit_kind == "trail_stop" and old.exit_px < 105
    assert new.exit_kind == "tp"
    assert new.net_pnl > old.net_pnl, (old, new)


def test_the_wider_band_returns_more_at_the_top_when_a_move_simply_reverses():
    """The cost side of the same coin, stated plainly. A move that peaks and
    turns straight round is exited FURTHER from the peak by the wider band."""
    path = bars([101, 103, 105, 107, 106, 105.5, 105])
    old = simulate(trade(), path, cfg(**OLD))
    new = simulate(trade(), path, cfg(**NEW))
    assert new.exit_px < old.exit_px
    assert new.net_pnl < old.net_pnl


def test_but_the_tighter_trail_wins_when_a_move_stalls_below_one_r():
    """The honest other half, and the reason the ledger replay matters.

    A move that peaks around +0.5R and reverses is BANKED by the old tight
    trail and missed entirely by the new one, which never arms. The change is
    not a free win: it trades certainty on small moves for the ability to hold
    large ones. Which way that nets out is a question about the distribution of
    MFE across real trades — exactly what mfe_r was added to measure and what
    this harness exists to answer. Do not assume; replay the ledger."""
    path = bars([100.5, 101.5, 102.0, 101.0, 100.2, 99.5])
    old = simulate(trade(), path, cfg(**OLD))
    new = simulate(trade(), path, cfg(**NEW))
    assert old.exit_kind == "trail_stop" and old.net_pnl > 0
    assert new.net_pnl < old.net_pnl


def test_a_banked_tranche_survives_a_stop_out_on_the_runner():
    """Half paid at +1R, the rest stopped — the outcome scale-out exists for."""
    path = bars([102, 104.5, 103, 101, 98, 95])
    out = simulate(trade(), path,
                   cfg(scale_out_at_r=1.0, scale_out_frac=0.5, min_notional=1.0,
                       trail_start_r=0.0, breakeven_at_r=0.0))
    assert out.banked is True
    naked = simulate(trade(), path,
                     cfg(scale_out_at_r=0.0, trail_start_r=0.0, breakeven_at_r=0.0))
    assert out.net_pnl > naked.net_pnl


def test_the_time_stop_closes_dead_money():
    flat = bars([100.1] * 400)
    out = simulate(trade(), flat,
                   cfg(time_stop_secs=10800, time_stop_min_r=0.5,
                       trail_start_r=0.0, breakeven_at_r=0.0, scale_out_at_r=0.0))
    assert out.exit_kind == "time_stop"


def test_a_banked_position_is_exempt_from_the_time_stop():
    path = bars([104.5] + [100.1] * 400)
    out = simulate(trade(), path,
                   cfg(time_stop_secs=10800, time_stop_min_r=0.5,
                       scale_out_at_r=1.0, scale_out_frac=0.5, min_notional=1.0,
                       trail_start_r=0.0, breakeven_at_r=0.0))
    assert out.banked is True and out.exit_kind != "time_stop"


def test_excursion_is_measured_from_the_candles():
    out = simulate(trade(), bars([98.0, 106.0, 99.0]),
                   cfg(scale_out_at_r=0.0, trail_start_r=0.0, breakeven_at_r=0.0))
    assert out.mfe_r > 1.4                  # ran to +6 on 4 of risk
    assert out.mae_r < -0.4                 # and -2 against first


def test_a_trade_with_no_candles_is_skipped_not_invented():
    report = run([trade()], lambda t: [], cfg(), "empty")
    assert report.n == 0


# -- the report's two caveats -------------------------------------------------

def outcome_report(label, pnls):
    from peri.replay import Outcome
    r = Report(label=label)
    for i, p in enumerate(pnls):
        r.outcomes.append(Outcome(i, "SOL", "long", "tp", 1.0, 10.0, p,
                                  p / 4.0, 1.0, -0.5))
    return r


def test_the_noise_bar_scales_with_the_spread_of_outcomes():
    tight = outcome_report("tight", [1.0, 1.1, 0.9, 1.0])
    wide = outcome_report("wide", [10.0, -12.0, 8.0, -6.0])
    assert wide.noise_sd > tight.noise_sd
    assert math.isinf(outcome_report("one", [1.0]).noise_sd)


def test_a_difference_inside_the_noise_is_not_reported_as_a_finding():
    base = outcome_report("baseline", [5.0, -6.0, 7.0, -4.0])
    barely = outcome_report("tweaked", [5.5, -6.0, 7.0, -4.0])
    text = format_report([base, barely])
    assert "NOT a finding" in text
    assert "exits only" in text.lower(), "the entries-are-given caveat is required"


def test_a_difference_beyond_the_noise_is_reported_as_one():
    base = outcome_report("baseline", [0.1, -0.1, 0.1, -0.1])
    better = outcome_report("tweaked", [40.0, 40.0, 40.0, 40.0])
    assert "A FINDING" in format_report([base, better])


# -- the cache ----------------------------------------------------------------

def test_the_cache_round_trips_and_reports_coverage(tmp_path):
    cache = CandleCache(str(tmp_path / "c.db"))
    cache.store("SOL", "1m", bars([100, 101, 102]))
    got = cache.load("SOL", "1m", 0, 10**12)
    assert [c["c"] for c in got] == [100, 101, 102]
    assert cache.covered("SOL", "1m", 60_000, 180_000)
    assert not cache.covered("SOL", "1m", 0, 10**12)


def test_storing_the_same_candle_twice_does_not_duplicate_it(tmp_path):
    cache = CandleCache(str(tmp_path / "c.db"))
    cache.store("SOL", "1m", bars([100, 101]))
    cache.store("SOL", "1m", bars([100, 101]))
    assert len(cache.load("SOL", "1m", 0, 10**12)) == 2


# -- fetching a historical window ---------------------------------------------

class RangePost:
    """A venue that returns at most 5000 bars per request, as HL does."""

    def __init__(self, span_ms=60_000, cap=5000):
        self.span, self.cap, self.requests = span_ms, cap, []

    def __call__(self, body):
        req = body["req"]
        start, end = int(req["startTime"]), int(req["endTime"])
        self.requests.append((start, end))
        out = []
        t = start
        while t <= end and len(out) < self.cap:
            out.append({"t": t, "o": 1, "h": 1, "l": 1, "c": 1, "v": 1})
            t += self.span
        return out


def test_a_long_window_is_chunked_rather_than_silently_truncated():
    """7 days of 1m is 10,080 bars against a 5,000-bar cap. A single request
    drops the tail — the part a replay cares about most."""
    from peri.market import Market
    post = RangePost()
    m = Market(post, dexes=())
    start = 1_700_000_000_000
    end = start + 7 * 24 * 3600 * 1000
    out = m.candles_range("SOL", "1m", start, end)

    assert len(post.requests) > 1, "must chunk"
    assert len(out) > 9000, len(out)
    times = [c["t"] for c in out]
    assert times == sorted(times) and len(set(times)) == len(times)
    assert times[0] == start and times[-1] <= end


def test_a_venue_that_stops_advancing_does_not_spin_forever():
    from peri.market import Market

    class Stuck:
        def __init__(self):
            self.calls = 0

        def __call__(self, body):
            self.calls += 1
            return [{"t": int(body["req"]["startTime"]) - 10_000_000,
                     "o": 1, "h": 1, "l": 1, "c": 1, "v": 1}]

    post = Stuck()
    out = Market(post, dexes=()).candles_range("SOL", "1m", 1_000_000, 9_000_000)
    assert post.calls == 1 and len(out) == 1


def test_an_empty_window_returns_nothing_rather_than_looping():
    from peri.market import Market
    out = Market(lambda body: [], dexes=()).candles_range("SOL", "1m", 0, 10**12)
    assert out == []
