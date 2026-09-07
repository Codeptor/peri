import pytest

from peri.hl_adapter import TRENCH_BUILDER, HyperliquidAdapter
from peri.risk import Approved
from peri.state import Position


class FakeExchange:
    def __init__(self):
        self.builders = []
        self.orders = []
        self.leverage_calls = []
        self.cancels = []
        self.dex_abstraction_calls = 0
        self.events = []
        self.next_oid = 100

    def update_leverage(self, lev, coin, is_cross=False):
        self.leverage_calls.append((lev, coin, is_cross))

    def market_open(self, coin, is_buy, sz, slippage=0.05, builder=None):
        self.events.append(("open", coin))
        self.builders.append(builder)
        self.orders.append(("market_open", coin, is_buy, sz))
        return {"response": {"data": {"statuses": [{"filled": {"avgPx": "100.7"}}]}}}

    def order(self, coin, is_buy, sz, limit_px, order_type, reduce_only=False, builder=None):
        self.next_oid += 1
        self.events.append(("order", order_type["trigger"]["tpsl"], self.next_oid))
        self.builders.append(builder)
        self.orders.append(("order", coin, is_buy, sz, limit_px, order_type, reduce_only))
        return {"response": {"data": {"statuses": [
            {"resting": {"oid": self.next_oid}},
        ]}}}

    def market_close(self, coin, sz=None, builder=None):
        self.events.append(("close", coin))
        self.builders.append(builder)
        self.orders.append(("market_close", coin, sz))
        return {"response": {"data": {"statuses": [{"filled": {"avgPx": "101.2"}}]}}}

    def cancel(self, coin, oid):
        self.events.append(("cancel", coin, oid))
        self.cancels.append((coin, oid))

    def bulk_orders(self, orders, builder=None, grouping="na"):
        self.next_oid += 1
        parent_oid = self.next_oid
        self.events.append(("bulk", grouping, len(orders), parent_oid))
        self.builders.append(builder)
        self.orders.append(("bulk_orders", orders, grouping))
        statuses = [{"resting": {"oid": parent_oid}}]
        for _ in orders[1:]:
            self.next_oid += 1
            statuses.append({"resting": {"oid": self.next_oid}})
        return {"response": {"data": {"statuses": statuses}}}

    def agent_enable_dex_abstraction(self):
        self.dex_abstraction_calls += 1
        return {"status": "ok"}


class FakeInfo:
    def __init__(self, open_orders=None, states=None, frontend_orders=None,
                 abstraction="disabled", fills=None):
        self._open_orders = open_orders or []
        self._states = states or {}
        self._frontend_orders = frontend_orders or {"": [], "xyz": []}
        self._abstraction = abstraction
        self._fills = fills if fills is not None else [
            {"tid": "t1", "coin": "SOL", "px": "99", "sz": "0.15",
             "dir": "Close Long", "closedPnl": "1.0", "fee": "0.02"},
        ]
        self.posts = []

    def open_orders(self, account):
        return self._open_orders

    def post(self, path, body):
        self.posts.append(body)
        if body["type"] == "clearinghouseState":
            return self._states.get(body.get("dex", ""),
                                    {"marginSummary": {"accountValue": "0"}})
        if body["type"] == "spotClearinghouseState":
            return self._states.get("spot", {"balances": []})
        if body["type"] == "userFills":
            return self._fills
        if body["type"] == "frontendOpenOrders":
            return self._frontend_orders[body.get("dex", "")]
        if body["type"] == "userAbstraction":
            return self._abstraction
        raise AssertionError(body)


class FakeMarketInfo:
    def __init__(self, szd=2, max_lev=20):
        self.sz_decimals = szd
        self.max_leverage = max_lev


class FakeMarket:
    def info(self, name):
        return FakeMarketInfo()


def mk(builder=TRENCH_BUILDER):
    ex, info = FakeExchange(), FakeInfo()
    a = HyperliquidAdapter(ex, info, "0xacct", FakeMarket(), builder=builder)
    return a, ex, info


def approved(**kw):
    # size is the guard's floored venue lot; the adapter no longer re-derives it
    base = dict(market="SOL", side="long", notional=15.0, size_usd_risk=0.3,
                leverage=10.0, margin=1.5, margin_mode="isolated",
                stop_px=99.0, tp_px=103.0, entry_px=100.0, size=0.15)
    base.update(kw)
    return Approved(**base)


def test_open_places_entry_and_both_brackets_with_builder():
    a, ex, _ = mk()
    fill = a.open(approved(), mark=100.0)
    assert fill == {"entry_px": 100.7, "size": 0.15}
    kinds = [o[0] for o in ex.orders]
    assert kinds == ["market_open", "order", "order"]
    trigger_kinds = [o[5]["trigger"]["tpsl"] for o in ex.orders[1:]]
    assert sorted(trigger_kinds) == ["sl", "tp"]
    assert all(o[6] is True for o in ex.orders[1:])           # reduce-only
    assert all(b == TRENCH_BUILDER for b in ex.builders)      # builder on EVERY order
    assert ex.leverage_calls == [(10, "SOL", False)]


def test_open_passes_cross_margin_mode_to_exchange():
    a, ex, _ = mk()
    a.open(approved(leverage=20, margin_mode="cross"), mark=100.0)
    assert ex.leverage_calls == [(20, "SOL", True)]


def test_open_without_builder_when_disabled():
    a, ex, _ = mk(builder=None)
    a.open(approved(), mark=100.0)
    assert all(b is None for b in ex.builders)


def test_open_unfilled_raises():
    a, ex, _ = mk()
    ex.market_open = lambda *a_, **k: {"response": {"data": {"statuses": [
        {"error": "insufficient margin"}]}}}
    with pytest.raises(RuntimeError):
        a.open(approved(), mark=100.0)


def test_open_zero_size_raises():
    """An Approved with no venue lot is an upstream bug, not something to guess
    around: the adapter used to re-derive size with round(), which could land
    ABOVE the risk the guard approved."""
    a, _, _ = mk()
    with pytest.raises(RuntimeError, match="no venue lot"):
        a.open(approved(notional=0.0001, size=0.0), mark=100.0)


def test_open_sends_exactly_the_lot_the_guard_approved():
    """The guard FLOORS to the venue step. Re-deriving from notional/mark with
    round() rounds half-UP, so a lot the guard floored to 0.15 could be sent as
    0.2 — more risk than was ever approved."""
    a, ex, _ = mk()
    # notional/mark = 0.179..., which round(1dp) would take UP to 0.2
    a.open(approved(notional=17.9, size=0.1), mark=100.0)
    assert ex.orders[0][0] == "market_open"
    assert ex.orders[0][3] == 0.1


def pos(**kw):
    base = dict(id=1, market="SOL", side="long", entry_px=100.7, size=0.15,
                notional=15.0, leverage=3.0, margin_mode="unknown",
                stop_px=99.0, tp_px=103.0,
                conviction=0.8, source="own", rationale=None, invalidation=None,
                status="open", opened_ts=0.0)
    base.update(kw)
    return Position(**base)


def test_close_cancels_triggers_then_closes():
    ex, info = FakeExchange(), FakeInfo(frontend_orders={"": [
        {"coin": "SOL", "isTrigger": True, "oid": 11},
        {"coin": "SOL", "isTrigger": True, "oid": 12},
        {"coin": "BTC", "isTrigger": True, "oid": 13}], "xyz": []})
    a = HyperliquidAdapter(ex, info, "0xacct", FakeMarket(), builder=TRENCH_BUILDER)
    out = a.close(pos(), mark=101.0)
    assert out == {"close_px": 101.2}
    assert ex.cancels == [("SOL", 11), ("SOL", 12)]           # BTC untouched
    assert ex.orders[-1][0] == "market_close"
    assert ex.events[:3] == [
        ("close", "SOL"), ("cancel", "SOL", 11), ("cancel", "SOL", 12),
    ]


def test_failed_close_leaves_existing_brackets_untouched():
    ex = FakeExchange()
    info = FakeInfo(frontend_orders={"": [
        {"coin": "SOL", "isTrigger": True, "oid": 11},
        {"coin": "SOL", "isTrigger": True, "oid": 12},
    ], "xyz": []})
    adapter = HyperliquidAdapter(ex, info, "0xacct", FakeMarket())
    ex.market_close = lambda *args, **kwargs: (_ for _ in ()).throw(RuntimeError("timeout"))

    with pytest.raises(RuntimeError, match="timeout"):
        adapter.close(pos(), mark=101.0)

    assert ex.cancels == []


def test_close_cancels_builder_dex_triggers_from_frontend_orders():
    info = FakeInfo(frontend_orders={
        "": [{"coin": "SOL", "isTrigger": True, "oid": 11}],
        "xyz": [
            {"coin": "xyz:KIOXIA", "isTrigger": True, "oid": 21},
            {"coin": "xyz:KIOXIA", "isTrigger": True, "oid": 22},
        ],
    })
    ex = FakeExchange()
    adapter = HyperliquidAdapter(ex, info, "0xacct", FakeMarket(), dexes=("xyz",))

    adapter.close(pos(market="xyz:KIOXIA"), mark=25.0)

    assert ex.cancels == [("xyz:KIOXIA", 21), ("xyz:KIOXIA", 22)]


def test_adjust_stop_replaces_both_brackets():
    ex, info = FakeExchange(), FakeInfo(frontend_orders={"": [
        {"coin": "SOL", "isTrigger": True, "oid": 11}], "xyz": []})
    a = HyperliquidAdapter(ex, info, "0xacct", FakeMarket(), builder=TRENCH_BUILDER)
    a.adjust_stop(pos(), 100.0, 110.0)
    assert ex.cancels == [("SOL", 11)]
    tpsls = [o[5]["trigger"]["tpsl"] for o in ex.orders]
    assert sorted(tpsls) == ["sl", "tp"]
    triggers = {
        o[5]["trigger"]["tpsl"]: o[5]["trigger"]["triggerPx"] for o in ex.orders
    }
    assert triggers == {"sl": 100.0, "tp": 110.0}
    assert [event[0] for event in ex.events] == ["order", "order", "cancel"]


def test_adjust_failure_cancels_only_new_trigger_and_preserves_old_pair():
    ex = FakeExchange()
    original_order = ex.order
    calls = 0

    def fail_second(*args, **kwargs):
        nonlocal calls
        calls += 1
        if calls == 2:
            raise RuntimeError("second trigger rejected")
        return original_order(*args, **kwargs)

    ex.order = fail_second
    info = FakeInfo(frontend_orders={"": [
        {"coin": "SOL", "isTrigger": True, "oid": 11},
        {"coin": "SOL", "isTrigger": True, "oid": 12},
    ], "xyz": []})
    adapter = HyperliquidAdapter(ex, info, "0xacct", FakeMarket())

    with pytest.raises(RuntimeError, match="second trigger"):
        adapter.adjust_stop(pos(), 100.0, 110.0)

    assert ex.cancels == [("SOL", 101)]


def test_open_orders_all_rejects_malformed_response():
    info = FakeInfo(frontend_orders={"": [], "xyz": {"error": "invalid JSON"}})
    adapter = HyperliquidAdapter(FakeExchange(), info, "0xacct", FakeMarket(),
                                 builder=TRENCH_BUILDER)

    with pytest.raises(RuntimeError, match="xyz open orders response must be a list"):
        adapter.open_orders_all()


def test_fills_rejects_malformed_response():
    adapter = HyperliquidAdapter(
        FakeExchange(), FakeInfo(fills={"error": "invalid JSON"}),
        "0xacct", FakeMarket(),
    )

    with pytest.raises(RuntimeError, match="fills response must be a list"):
        adapter.fills()


def test_equity_sums_standard_account_balances():
    ex = FakeExchange()
    info = FakeInfo(states={"": {"marginSummary": {"accountValue": "50.5"}},
                            "xyz": {"marginSummary": {"accountValue": "9.5"}},
                            "spot": {"balances": [{"coin": "USDC", "total": "62.45"},
                                                  {"coin": "HYPE", "total": "3"}]}})
    a = HyperliquidAdapter(ex, info, "0xacct", FakeMarket(), dexes=("xyz",))
    assert a.equity({}) == pytest.approx(122.45)  # USDC counted, HYPE not


def test_equity_uses_spot_total_for_unified_account():
    info = FakeInfo(
        abstraction="unifiedAccount",
        states={"": {"marginSummary": {"accountValue": "23.69864"}},
                "xyz": {"marginSummary": {"accountValue": "26.153398"}},
                "spot": {"balances": [
                    {"coin": "USDC", "total": "62.36275001", "hold": "49.852038"},
                ]}},
    )
    adapter = HyperliquidAdapter(FakeExchange(), info, "0xacct", FakeMarket(), dexes=("xyz",))

    assert adapter.equity({}) == pytest.approx(62.36275001)


def test_account_snapshot_calculates_unified_free_collateral():
    info = FakeInfo(
        abstraction="unifiedAccount",
        states={
            "": {
                "marginSummary": {
                    "accountValue": "23.69864",
                    "totalMarginUsed": "20.0",
                },
                "withdrawable": "0.0",
                "assetPositions": [],
            },
            "xyz": {
                "marginSummary": {
                    "accountValue": "26.153398",
                    "totalMarginUsed": "26.153398",
                },
                "withdrawable": "0.0",
                "assetPositions": [{"position": {
                    "coin": "xyz:NVDA", "szi": "0.26", "entryPx": "224.44",
                    "leverage": {"type": "isolated", "value": 4},
                    "marginUsed": "14.729643", "positionValue": "58.5104",
                    "unrealizedPnl": "0.0988", "liquidationPx": "171.2",
                    "returnOnEquity": "0.0067",
                }}],
            },
            "spot": {"balances": [
                {"coin": "USDC", "total": "62.36275001", "hold": "49.852038"},
            ]},
        },
    )
    adapter = HyperliquidAdapter(FakeExchange(), info, "0xacct", FakeMarket(), dexes=("xyz",))

    snapshot = adapter.account_snapshot()

    assert snapshot["equity"] == pytest.approx(62.36275001)
    assert snapshot["held_collateral"] == pytest.approx(49.852038)
    assert snapshot["total_margin_used"] == pytest.approx(46.153398)
    assert snapshot["available_margin"] == pytest.approx(12.51071201)
    assert snapshot["withdrawable_by_dex"] == {"native": 0.0, "xyz": 0.0}
    assert snapshot["positions"] == [{
        "market": "xyz:NVDA", "side": "long", "size": 0.26,
        "entry_px": 224.44, "leverage": 4.0, "margin_mode": "isolated",
        "margin": 14.729643,
        "position_value": 58.5104, "upnl": 0.0988, "liquidation_px": 171.2,
        "roe": 0.0067,
    }]


def test_equity_rejects_missing_unified_usdc_balance():
    info = FakeInfo(
        abstraction="unifiedAccount",
        states={"spot": {"balances": [{"coin": "HYPE", "total": "3"}]}},
    )
    adapter = HyperliquidAdapter(FakeExchange(), info, "0xacct", FakeMarket())

    with pytest.raises(RuntimeError, match="unified USDC balance unavailable"):
        adapter.equity({})


def test_enable_dex_abstraction_tolerates_unified_account():
    a, ex, _ = mk()
    ex.agent_enable_dex_abstraction = lambda: {
        "status": "err", "response": "Action disabled when unified account is active"}
    a.enable_dex_abstraction()  # must not raise
    ex.agent_enable_dex_abstraction = lambda: {
        "status": "err", "response": "Abstraction transition not allowed"}
    a.enable_dex_abstraction()  # must not raise
    ex.agent_enable_dex_abstraction = lambda: {"status": "err", "response": "boom"}
    with pytest.raises(RuntimeError):
        a.enable_dex_abstraction()


def test_fills_and_dex_abstraction():
    a, ex, info = mk()
    fills = a.fills()
    assert fills[0]["coin"] == "SOL"
    a.enable_dex_abstraction()
    assert ex.dex_abstraction_calls == 1


# -- resting maker entries (2026-08-29) -----------------------------------
def test_resting_entry_is_one_action_with_both_brackets_attached():
    a, ex, _ = mk()
    out = a.place_resting_entry(approved(entry_px=97.5, resting=True), size=0.15)

    assert ex.leverage_calls == [(10, "SOL", False)]
    kind, orders, grouping = ex.orders[0]
    assert kind == "bulk_orders"
    # ONE signed action: the children arm at the venue the moment the parent
    # fills, so a resting entry is never an unprotected position
    assert grouping == "normalTpsl"
    assert len(orders) == 3
    entry, sl, tp = orders
    assert entry["is_buy"] is True and entry["reduce_only"] is False
    assert entry["limit_px"] == 97.5 and entry["order_type"] == {"limit": {"tif": "Gtc"}}
    assert sl["is_buy"] is False and sl["reduce_only"] is True
    assert sl["order_type"]["trigger"]["triggerPx"] == 99.0
    assert sl["order_type"]["trigger"]["tpsl"] == "sl"
    assert tp["order_type"]["trigger"]["tpsl"] == "tp"
    assert tp["order_type"]["trigger"]["triggerPx"] == 103.0
    assert all(o["sz"] == 0.15 for o in orders)
    assert ex.builders == [TRENCH_BUILDER]     # the builder fee rides the whole group
    assert out["oid"] == 101 and out["children"] == [102, 103]


def test_resting_short_entry_sells_into_the_level_and_buys_to_close():
    a, ex, _ = mk()
    a.place_resting_entry(
        approved(side="short", entry_px=103.0, stop_px=105.0, tp_px=97.0,
                 resting=True), size=0.2)
    _, orders, _ = ex.orders[0]
    assert orders[0]["is_buy"] is False        # sell the bounce
    assert all(o["is_buy"] is True for o in orders[1:])   # both exits buy back


def test_a_rejected_resting_entry_raises_instead_of_reporting_success():
    a, ex, _ = mk()

    def rejecting(orders, builder=None, grouping="na"):
        return {"response": {"data": {"statuses": [{"error": "Insufficient margin"}]}}}

    ex.bulk_orders = rejecting
    with pytest.raises(RuntimeError, match="Insufficient margin"):
        a.place_resting_entry(approved(entry_px=97.5, resting=True), size=0.15)


def test_zero_size_resting_entry_refuses_before_touching_the_venue():
    a, ex, _ = mk()
    with pytest.raises(RuntimeError, match="rounds to 0"):
        a.place_resting_entry(approved(entry_px=97.5, resting=True), size=0.0)
    assert ex.orders == []


# -- scale-out: the target splits, the stop never does ------------------------

def scale_out(**kw):
    from peri.risk import ScaleOut
    base = dict(tp1_px=101.0, tp1_size=0.07, runner_size=0.08, at_r=1.0)
    base.update(kw)
    return ScaleOut(**base)


def test_a_market_open_arms_a_full_size_stop_and_two_target_tranches():
    a, ex, _ = mk()
    a.open(approved(scale_out=scale_out()), mark=100.0)

    kinds = [o[0] for o in ex.orders]
    assert kinds == ["market_open", "order", "order", "order"]
    sl, tp1, tp2 = ex.orders[1:]
    assert sl[5]["trigger"]["tpsl"] == "sl"
    # the stop stays FULL size: until a tranche fills the whole position is
    # still at risk, and a stop sized to the runner leaves the rest naked
    assert sl[3] == 0.15
    assert tp1[5]["trigger"]["tpsl"] == "tp" and tp1[5]["trigger"]["triggerPx"] == 101.0
    assert tp2[5]["trigger"]["tpsl"] == "tp" and tp2[5]["trigger"]["triggerPx"] == 103.0
    assert abs((tp1[3] + tp2[3]) - 0.15) < 1e-9, "tranches must sum to the approved lot"
    assert all(o[6] is True for o in ex.orders[1:])          # every leg reduce-only
    assert all(b == TRENCH_BUILDER for b in ex.builders)


def test_a_resting_entry_attaches_both_tranches_in_the_same_signed_action():
    a, ex, _ = mk()
    out = a.place_resting_entry(
        approved(entry_px=97.5, resting=True, scale_out=scale_out()), size=0.15)

    _kind, orders, grouping = ex.orders[0]
    assert grouping == "normalTpsl"
    assert len(orders) == 4                     # entry + stop + two targets
    entry, sl, tp1, tp2 = orders
    assert entry["reduce_only"] is False and entry["sz"] == 0.15
    assert sl["order_type"]["trigger"]["tpsl"] == "sl" and sl["sz"] == 0.15
    assert tp1["order_type"]["trigger"]["triggerPx"] == 101.0 and tp1["sz"] == 0.07
    assert tp2["order_type"]["trigger"]["triggerPx"] == 103.0 and tp2["sz"] == 0.08
    assert out["children"] == [102, 103, 104]


def test_without_a_plan_the_single_full_size_target_is_unchanged():
    a, ex, _ = mk()
    a.open(approved(), mark=100.0)
    assert [o[0] for o in ex.orders] == ["market_open", "order", "order"]
    assert all(o[3] == 0.15 for o in ex.orders[1:])


def test_replacing_brackets_preserves_an_unfilled_tranche():
    """The trail arms at the same R the tranche banks at, so a replacement that
    dropped the split would collapse it in the common case, not a corner one."""
    a, ex, _ = mk()
    pos = Position(1, "SOL", "long", 100.0, 0.15, 15.0, 10.0, "isolated",
                   99.0, 103.0, 0.8, "own", None, None, "open", 0.0)
    a.adjust_stop(pos, 100.5, 103.0, scale_out=scale_out())

    triggers = [o for o in ex.orders if o[0] == "order"]
    assert len(triggers) == 3
    assert [t[5]["trigger"]["tpsl"] for t in triggers] == ["sl", "tp", "tp"]
    assert triggers[0][3] == 0.15                            # stop still full size
