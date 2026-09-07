"""Lighter market layer against canned transport. No network."""

import pytest

from peri.lighter_market import ALIASES, LighterMarket, resolve


BOOKS = {"order_books": [
    {"symbol": "ETH", "market_id": 0, "status": "active",
     "supported_price_decimals": 2, "supported_size_decimals": 4,
     "min_base_amount": "0.005", "min_quote_amount": "10.0"},
    {"symbol": "ANTHROPIC", "market_id": 5, "status": "active",
     "supported_price_decimals": 2, "supported_size_decimals": 2,
     "min_base_amount": "0.1", "min_quote_amount": "10.0"},
    {"symbol": "DEAD", "market_id": 9, "status": "delisted",
     "supported_price_decimals": 2, "supported_size_decimals": 2,
     "min_base_amount": "1", "min_quote_amount": "10.0"},
]}

DETAILS = {"order_book_details": [
    {"market_id": 0, "mark_price": "2485.69", "last_trade_price": 2485.59,
     "daily_price_change": -0.3, "daily_quote_token_volume": 232166363.0,
     "open_interest": 38912.0, "min_initial_margin_fraction": 200},
    {"market_id": 5, "mark_price": "44.10", "last_trade_price": 44.0,
     "daily_price_change": 2.5, "daily_quote_token_volume": 44000000.0,
     "open_interest": 100000.0, "min_initial_margin_fraction": 1000},
]}

CANDLES = {"c": [
    {"t": 1, "o": 2457.0, "h": 2459.0, "l": 2457.0, "c": 2458.0, "v": 10.0},
    {"t": 2, "o": 2458.0, "h": 2470.0, "l": 2456.0, "c": 2468.0, "v": 12.0},
    {"t": 3, "o": 2468.0, "h": 2490.0, "l": 2460.0, "c": 2485.0, "v": 14.0},
]}


class FakeMarket(LighterMarket):
    def __init__(self):
        super().__init__(host="https://fake.invalid")
        self.paths = []

    def _get(self, path: str):
        self.paths.append(path)
        if path == "orderBooks":
            return BOOKS
        if path == "orderBookDetails":
            return DETAILS
        assert path.startswith("candles?market_id=0"), path
        return CANDLES


def test_resolve_uses_aliases_then_suffix():
    assert resolve("io:ANTH") == "ANTHROPIC"
    assert resolve("xyz:NVDA") == "NVDA"
    assert resolve("ETH") == "ETH"


def test_ambiguous_names_stay_unmapped():
    # xyz:SKHX could be SKHYNIXUSD and xyz:SP500 could be SPX or US500, but
    # "could be" is not a mapping. Unmapped names raise like unknown markets.
    assert "xyz:SKHX" not in ALIASES
    assert "xyz:SP500" not in ALIASES
    m = FakeMarket()
    with pytest.raises(KeyError):
        m.info("xyz:SKHX")
    assert not m.known("xyz:SP500")


def test_meta_skips_inactive_and_reads_leverage_ceiling():
    m = FakeMarket()
    assert m.info("ETH").sz_decimals == 4
    # max leverage is 10000 / min_initial_margin_fraction: 50x here, 10x there
    assert m.info("ETH").max_leverage == 50.0
    assert m.info("ANTHROPIC").max_leverage == 10.0
    assert not m.known("DEAD")


def test_ctxs_values_and_zero_funding_means_unavailable():
    m = FakeMarket()
    ctxs = m.ctxs()
    eth = ctxs["ETH"]
    assert eth.mark == pytest.approx(2485.69)
    assert eth.day_pct == pytest.approx(-0.3)
    assert eth.vol_usd == pytest.approx(232166363.0)
    assert eth.oi_usd == pytest.approx(38912.0 * 2485.69)
    assert eth.funding_apr_pct == 0.0
    assert m.mark("ETH") == pytest.approx(2485.69)


def test_features_share_candle_math_with_hl():
    m = FakeMarket()
    f = m.features("ETH", m.ctxs()["ETH"])
    assert f["mark"] == pytest.approx(2485.69)
    assert "atr15m_pct" in f and f["atr15m_pct"] > 0
    assert f["hi_24h"] == pytest.approx(2490.0)
    assert f["lo_24h"] == pytest.approx(2456.0)
    assert 0.0 <= f["range24h_pos"] <= 1.0


def test_candidates_allowlist_movers_and_must_include():
    m = FakeMarket()
    out = m.candidates(["ETH", "MISSING"], volume_floor=1_000_000,
                       top_movers=1, must_include=["ANTHROPIC"])
    assert "ETH" in out and "MISSING" not in out
    assert "ANTHROPIC" in out
    assert len(out) == len(set(out))


def test_int_encoding_floors_size():
    m = FakeMarket()
    assert m.px_int("ETH", 2485.694) == 248569
    # 0.13759 at 4dp floors to 0.1375, never up
    assert m.sz_int("ETH", 0.13759) == 1375
    assert m.round_px("ETH", 2485.694) == pytest.approx(2485.69)


def test_affordable_flags_coarse_lots():
    m = FakeMarket()
    assert m.affordable("ETH", 2485.69, max_notional=1000.0)
    assert not m.affordable("ETH", 2485.69, max_notional=0.01)
