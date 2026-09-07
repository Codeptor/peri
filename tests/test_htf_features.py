"""Structure above 15m, and the levels a target can actually be anchored to.

Until 2026-09-07 `Market.features` made ONE call — 15m candles over 24h — and
that was the entire market picture the analyst received. It was being asked to
pick a swing target at least 4% away (the RR floor against a 2% minimum stop
forces that) with no daily levels, no prior-day high/low and no multi-day range
to place it in. Across 27 live trades exactly one target was ever reached.
"""

import math

from peri.market import (Ctx, Market, candle_features, expected_move_pct,
                         htf_features)


def bars(closes, *, spread=1.0, vol=100.0):
    return [{"t": i, "o": c, "c": c, "h": c + spread, "l": c - spread, "v": vol}
            for i, c in enumerate(closes)]


def test_daily_structure_places_the_price_in_its_multi_day_range():
    d1 = bars([100 + i for i in range(20)])          # steady climb to 119
    f = htf_features([], d1, last=119.0)
    assert f["range20d_pos"] > 0.9, "at the top of the 20d range"
    assert f["hi_20d"] == 120.0 and f["lo_20d"] == 99.0
    assert f["prev_day_hi"] == 119.0 and f["prev_day_lo"] == 117.0
    assert f["trend_20d_pct"] > 0


def test_headroom_to_the_nearest_multi_day_level_is_reported():
    """The number that says whether a 4%-away target is inside structure."""
    d1 = bars([100] * 19 + [102])
    f = htf_features([], d1, last=102.0)
    assert abs(f["to_hi_20d_pct"] - (103.0 / 102.0 - 1) * 100) < 1e-9
    assert abs(f["to_lo_20d_pct"] - (1 - 99.0 / 102.0) * 100) < 1e-9


def test_hourly_trend_says_whether_you_are_buying_strength_or_a_falling_market():
    rising = htf_features(bars([100 + i for i in range(24)]), [], last=123.0)
    falling = htf_features(bars([123 - i for i in range(24)]), [], last=100.0)
    assert rising["trend_1h_pct"] > 0
    assert falling["trend_1h_pct"] < 0


def test_atrs_are_reported_per_timeframe():
    f = htf_features(bars([100] * 30, spread=1.0), bars([100] * 30, spread=5.0),
                     last=100.0)
    assert 1.0 < f["atr1h_pct"] < 3.0
    assert f["atr1d_pct"] > f["atr1h_pct"], "daily range must exceed hourly"


def test_missing_candles_yield_nothing_rather_than_a_guess():
    assert htf_features([], [], last=100.0) == {}
    assert htf_features([], [], last=0.0) == {}


def test_relative_volume_comes_from_the_per_bar_volume_that_was_discarded():
    quiet = candle_features(bars([100] * 10, vol=100.0))
    assert abs(quiet["rvol"] - 1.0) < 1e-9
    cs = bars([100] * 9, vol=100.0) + bars([100], vol=400.0)
    assert candle_features(cs)["rvol"] > 3.0


def test_relative_volume_is_absent_rather_than_wrong_without_volume():
    cs = [{"t": i, "o": 100, "c": 100, "h": 101, "l": 99} for i in range(10)]
    assert "rvol" not in candle_features(cs)


def test_the_plausible_move_scales_with_the_square_root_of_time():
    """Range grows with sqrt(t); 3h is 12 fifteen-minute bars."""
    assert abs(expected_move_pct(0.5, 3) - 0.5 * math.sqrt(12)) < 1e-9
    assert expected_move_pct(0.5, 24) > expected_move_pct(0.5, 3)
    assert expected_move_pct(None, 3) is None
    assert expected_move_pct(0.0, 3) is None


def test_the_plausible_move_exposes_the_conflict_the_rails_create():
    """A 2% stop at RR 2.0 needs a 4% move. On a market whose ATR15m is 0.3%
    that is far beyond anything 3h plausibly delivers — the disagreement that
    produced one take-profit in 27 trades."""
    assert expected_move_pct(0.3, 3) < 1.2
    assert 4.0 > expected_move_pct(0.3, 24)


# -- the ctx fields that were being discarded ---------------------------------

def ctx_market(payload):
    return Market(lambda body: payload, dexes=())


def test_spread_and_premium_are_read_from_the_payload():
    meta = {"universe": [{"name": "SOL", "szDecimals": 2}]}
    ctxs = [{"markPx": "101.0", "prevDayPx": "100.0", "funding": "0.00001",
             "openInterest": "10", "dayNtlVlm": "5000000",
             "oraclePx": "100.0", "impactPxs": ["100.95", "101.05"]}]
    c = ctx_market([meta, ctxs]).ctxs()["SOL"]
    assert abs(c.premium_pct - 1.0) < 1e-9          # mark 1% over oracle
    assert 9.0 < c.spread_bps < 11.0                # ~10bp impact spread


def test_a_missing_oracle_or_impact_is_zero_not_an_exception():
    meta = {"universe": [{"name": "SOL", "szDecimals": 2}]}
    ctxs = [{"markPx": "101.0", "prevDayPx": "100.0", "funding": "0",
             "openInterest": "10", "dayNtlVlm": "1"}]
    c = ctx_market([meta, ctxs]).ctxs()["SOL"]
    assert c.premium_pct == 0.0 and c.spread_bps == 0.0


# -- the screener no longer argues with the range gate ------------------------

def test_candidates_are_not_only_the_most_extended_names():
    """Ranking purely by |day%| fed the analyst exactly the names the range-edge
    gate refuses. Liquid in-range names must survive the screen too."""
    ctxs = {}
    for i in range(6):                              # big movers, thin
        ctxs[f"xyz:M{i}"] = Ctx(f"xyz:M{i}", 100.0, 20.0 - i, 0.0, 0.0, 3e6)
    for i in range(6):                              # quiet, deeply liquid
        ctxs[f"xyz:L{i}"] = Ctx(f"xyz:L{i}", 100.0, 0.1, 0.0, 0.0, 5e8 - i)

    m = Market(lambda body: None, dexes=("xyz",))
    m.ctxs = lambda: ctxs
    picked = m.candidates([], volume_floor=2e6, top_movers=6, must_include=[])

    assert len(picked) == 6
    assert any(n.startswith("xyz:L") for n in picked), picked
    assert any(n.startswith("xyz:M") for n in picked), picked
    assert len(set(picked)) == len(picked), "no duplicates"


# -- cost control: the higher timeframes must not triple the call rate --------

class CountingPost:
    def __init__(self):
        self.calls = []

    def __call__(self, body):
        self.calls.append(body["req"]["interval"])
        return [{"t": 0, "o": 100, "c": 100, "h": 101, "l": 99, "v": 10}]


def test_slow_timeframes_are_cached_and_the_trading_one_is_not():
    """15m is the timeframe decisions are made on and is always refetched.
    Without caching the rest, each candidate costs three info calls a cycle."""
    post = CountingPost()
    m = Market(post, dexes=())
    for _ in range(3):
        m.candles("BTC", "15m", 1000)
        m.candles("BTC", "1h", 1000)
        m.candles("BTC", "1d", 1000)

    assert post.calls.count("15m") == 3, "the trading timeframe must stay live"
    assert post.calls.count("1h") == 1
    assert post.calls.count("1d") == 1


def test_the_cache_is_per_market():
    post = CountingPost()
    m = Market(post, dexes=())
    m.candles("BTC", "1d", 1000)
    m.candles("ETH", "1d", 1000)
    assert post.calls.count("1d") == 2


def test_an_empty_response_is_not_cached():
    """A market whose candles failed must be retried, not remembered as empty
    for an hour — that is how a market silently loses its gates."""
    calls = []

    def post(body):
        calls.append(body["req"]["interval"])
        return []

    m = Market(post, dexes=())
    m.candles("BTC", "1d", 1000)
    m.candles("BTC", "1d", 1000)
    assert len(calls) == 2
