import pytest
from pydantic import ValidationError

from peri.models import AdjustStopAction, CloseAction, Decision, OpenAction


def test_decision_parses_full_open():
    d = Decision.model_validate({
        "market_view": "semis bid",
        "actions": [{"kind": "open", "market": "xyz:NVDA", "side": "long",
                     "conviction": 0.8, "stop": 214.9, "take_profit": 226.0,
                     "leverage": 20, "margin_mode": "cross", "source": "own",
                     "mirror_msg_id": None,
                     "rationale": "beat and raise", "invalidation": "loses 215 shelf"}]})
    a = d.actions[0]
    assert isinstance(a, OpenAction)
    assert a.market == "xyz:NVDA" and a.leverage == 20 and a.margin_mode == "cross"


def test_discriminates_kinds():
    d = Decision.model_validate({"actions": [
        {"kind": "close", "market": "BTC", "rationale": "thesis done"},
        {"kind": "adjust_stop", "market": "SOL", "stop": 98.2,
         "take_profit": 108.0, "rationale": "lock"}]})
    assert isinstance(d.actions[0], CloseAction)
    assert isinstance(d.actions[1], AdjustStopAction)


def test_adjust_stop_requires_take_profit():
    with pytest.raises(ValidationError):
        Decision.model_validate({"actions": [
            {"kind": "adjust_stop", "market": "SOL", "stop": 98.2,
             "rationale": "lock"},
        ]})


def test_open_requires_stop_and_tp():
    base = {"kind": "open", "market": "BTC", "side": "long", "conviction": 0.8,
            "leverage": 10, "margin_mode": "isolated",
            "rationale": "r", "invalidation": "i"}
    with pytest.raises(ValidationError):
        Decision.model_validate({"actions": [{**base, "take_profit": 80000}]})
    with pytest.raises(ValidationError):
        Decision.model_validate({"actions": [{**base, "stop": 70000}]})


def test_conviction_bounds():
    with pytest.raises(ValidationError):
        OpenAction(market="BTC", side="long", conviction=1.4, stop=1, take_profit=2,
                   leverage=10, margin_mode="isolated", rationale="r", invalidation="i")


@pytest.mark.parametrize("leverage", [0, 1, 51, 100])
def test_open_rejects_leverage_outside_the_venue_range(leverage):
    with pytest.raises(ValidationError):
        OpenAction(market="BTC", side="long", conviction=0.8, stop=1,
                   take_profit=2, leverage=leverage, margin_mode="isolated",
                   rationale="r", invalidation="i")


@pytest.mark.parametrize("leverage", [3, 5, 6, 10, 20])
def test_low_leverage_is_valid_because_builder_dexes_cap_low(leverage):
    """io:ANTH allows 6x and vntl 3x. A Literal[10, 20] made those markets
    untradeable by construction; the guard still enforces the config ceiling,
    the venue ceiling and the isolated-liquidation band."""
    action = OpenAction(market="io:ANTH", side="long", conviction=0.8, stop=1,
                        take_profit=2, leverage=leverage, margin_mode="isolated",
                        rationale="r", invalidation="i")
    assert action.leverage == leverage


def test_open_requires_valid_margin_mode():
    base = dict(market="BTC", side="long", conviction=0.8, stop=1,
                take_profit=2, leverage=10, rationale="r", invalidation="i")
    with pytest.raises(ValidationError):
        OpenAction(**base)
    with pytest.raises(ValidationError):
        OpenAction(**base, margin_mode="portfolio")


def test_unknown_kind_rejected():
    with pytest.raises(ValidationError):
        Decision.model_validate({"actions": [{"kind": "yolo", "market": "BTC"}]})


def test_empty_actions_valid():
    d = Decision.model_validate({"market_view": "chop, sitting flat"})
    assert d.actions == []


def test_rationale_required_nonempty():
    with pytest.raises(ValidationError):
        CloseAction(market="BTC", rationale="")
