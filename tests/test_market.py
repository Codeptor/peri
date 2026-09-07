import pytest

from peri.market import Market, candle_features


def fake_post_factory():
    """Two-dex fake HL info endpoint."""
    native_meta = {"universe": [{"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                                {"name": "SOL", "szDecimals": 2, "maxLeverage": 20}]}
    xyz_meta = {"universe": [{"name": "xyz:NVDA", "szDecimals": 2, "maxLeverage": 20},
                             {"name": "xyz:GOLD", "szDecimals": 1, "maxLeverage": 20},
                             {"name": "xyz:THIN", "szDecimals": 0, "maxLeverage": 10}]}
    native_ctxs = [{"markPx": "80000", "prevDayPx": "80400", "funding": "0.0000125",
                    "openInterest": "100", "dayNtlVlm": "3e9"},
                   {"markPx": "100", "prevDayPx": "97", "funding": "0.0000125",
                    "openInterest": "1000", "dayNtlVlm": "5e8"}]
    xyz_ctxs = [{"markPx": "218", "prevDayPx": "213.7", "funding": "0.0000063",
                 "openInterest": "600000", "dayNtlVlm": "3e8"},
                {"markPx": "4610", "prevDayPx": "4650", "funding": "0.0000063",
                 "openInterest": "80000", "dayNtlVlm": "6e7"},
                {"markPx": "10", "prevDayPx": "9", "funding": "0", "openInterest": "10",
                 "dayNtlVlm": "50000"}]

    def post(body):
        t = body["type"]
        dex = body.get("dex", "")
        if t == "meta":
            return xyz_meta if dex else native_meta
        if t == "metaAndAssetCtxs":
            return [xyz_meta, xyz_ctxs] if dex else [native_meta, native_ctxs]
        if t == "candleSnapshot":
            return [{"t": i, "o": 100, "h": 102, "l": 99, "c": 100 + i * 0.1, "v": 10}
                    for i in range(20)]
        raise AssertionError(f"unexpected body {body}")

    return post


def test_meta_merged_across_dexes():
    m = Market(fake_post_factory(), ["xyz"])
    assert m.info("BTC").max_leverage == 40
    assert m.info("xyz:NVDA").sz_decimals == 2
    assert m.known("xyz:GOLD") and not m.known("xyz:FAKE")
    with pytest.raises(KeyError):
        m.info("xyz:FAKE")


def test_ctxs_merged_and_computed():
    m = Market(fake_post_factory(), ["xyz"])
    ctxs = m.ctxs()
    assert ctxs["BTC"].day_pct == pytest.approx(-0.4975, abs=1e-3)
    assert ctxs["xyz:NVDA"].mark == 218.0
    assert ctxs["xyz:NVDA"].oi_usd == pytest.approx(600000 * 218)
    assert m.mark("SOL") == 100.0


def test_candidates_screening():
    m = Market(fake_post_factory(), ["xyz"])
    # floor 1e6 excludes xyz:THIN; movers sorted by |day%|
    names = m.candidates(["BTC", "SOL"], 1_000_000, 12, must_include=[])
    assert names[0] == "BTC" and "SOL" in names
    assert "xyz:NVDA" in names and "xyz:GOLD" in names
    assert "xyz:THIN" not in names


def test_candidates_must_include_and_dedupe():
    m = Market(fake_post_factory(), ["xyz"])
    names = m.candidates(["BTC"], 1e12, 0, must_include=["xyz:GOLD", "BTC"])
    assert names.count("BTC") == 1
    assert "xyz:GOLD" in names


def test_candle_features():
    cs = [{"t": i, "o": 100, "h": 101 + i * 0.1, "l": 99, "c": 100 + i * 0.1, "v": 1}
          for i in range(20)]
    f = candle_features(cs)
    assert "r_1h_pct" in f and "r_4h_pct" in f and f["atr15m_pct"] > 0
    assert 0 <= f["range24h_pos"] <= 1
    assert candle_features([]) == {}
