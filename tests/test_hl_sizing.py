from peri.hl_sizing import format_price, notional_to_size, sz_decimals, tpsl_limit_price


def test_notional_to_size():
    assert notional_to_size(15.0, 150.0, 2) == 0.1     # 15/150
    assert notional_to_size(15.0, 3.0, 1) == 5.0


def test_sz_decimals():
    meta = {"universe": [{"name": "SOL", "szDecimals": 2}, {"name": "BTC", "szDecimals": 5}]}
    assert sz_decimals(meta, "SOL") == 2
    assert sz_decimals(meta, "BTC") == 5


def test_format_price_tick_rules():
    assert format_price(4169.0, 2) == 4169          # integers always pass
    assert format_price(150.2985, 2) == 150.3       # >5 sig figs → 5
    assert format_price(62345.5, 5) == 62346        # BTC-style 6 sig figs → 5
    assert format_price(147.75, 2) == 147.75        # already legal → unchanged
    assert format_price(0.0011223344, 0) == 0.001122  # decimals capped at 6 - szDecimals


def test_tpsl_limit_price_guards():
    assert tpsl_limit_price(100.0, close_is_buy=False, sz_decimals=2) == 92.0    # sell floor
    assert tpsl_limit_price(100.0, close_is_buy=True, sz_decimals=2) == 108.0    # buy ceiling
