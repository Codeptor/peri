"""Lighter executor against a fake venue. No network, no SDK."""

import asyncio

import pytest

from peri.lighter_adapter import LighterAdapter
from peri.risk import Approved
from peri.state import Position


ACCT = 744352


def raw_order(mid, idx, coi, *, otype="limit", price=2465.0, trig=0.0,
              size=0.5, ask=False, reduce=False, ts=1700000000):
    return {"order_index": idx, "client_order_index": coi, "market_index": mid,
            "initial_base_amount": str(size), "price": str(price),
            "is_ask": ask, "reduce_only": reduce, "type": otype,
            "trigger_price": str(trig), "status": "open", "timestamp": ts}


class FakeMarket:
    """ETH at 2dp/4dp on market 0, 50x ceiling — like the live venue."""
    def market_id(self, name):
        assert name == "ETH", name
        return 0

    def symbols_by_id(self):
        return {0: "ETH"}

    def info(self, name):
        from peri.market import MarketInfo
        return MarketInfo(name, 4, 50.0)

    def px_int(self, name, price):
        return int(round(round(price, 2) * 100))

    def sz_int(self, name, size):
        return int(size * 10000)

    def round_px(self, name, price):
        return round(price, 2)

    def mark(self, name):
        return 2481.0


class FakeBridge:
    def call(self, coro, timeout=60.0):
        return asyncio.run(coro)


class FakeOps:
    """A venue that stores what it is sent and serves it back on reads."""
    def __init__(self):
        self.calls = []
        self.leverage = []
        self.next_index = 1000
        self.orders = []
        self.trades_list = []
        self.positions = []
        self.fail_send_at = None

    def _oid(self):
        self.next_index += 1
        return self.next_index

    async def set_leverage(self, mid, cross, lev):
        self.leverage.append((mid, cross, lev))

    async def send_grouped(self, mid, legs):
        self.calls.append(("grouped", mid, [dict(leg) for leg in legs]))
        for leg in legs:
            self.orders.append(raw_order(
                mid, self._oid(), leg["coi"],
                otype={"limit": "limit", "stop": "stop-loss",
                       "tp": "take-profit"}[leg["order_type"]],
                price=leg["price"] / 100, trig=leg["trigger_price"] / 100,
                size=leg["base_amount"] / 10000,
                ask=leg["is_ask"], reduce=leg["reduce_only"]))

    async def send_order(self, mid, leg):
        self.calls.append(("order", mid, dict(leg)))
        self.order_attempts = getattr(self, "order_attempts", 0) + 1
        if self.fail_send_at == self.order_attempts:
            raise RuntimeError("venue refused")
        await self.send_grouped(mid, [leg])

    async def send_market(self, mid, coi, base, avg, ask, reduce):
        import time as _time
        self.calls.append(("market", mid, base, avg, ask, reduce))
        self.trades_list.append({
            "trade_id": 9000 + len(self.trades_list), "tx_hash": "0xabc",
            "market_id": mid, "size": str(base / 10000),
            "price": str(avg / 100), "is_maker_ask": False,
            "bid_account_id": 1 if ask else ACCT,
            "ask_account_id": ACCT if ask else 1,
            "taker_position_size_before": "0.0",
            "maker_position_size_before": "0.0",
            "bid_account_pnl": "0.0", "ask_account_pnl": "0.0",
            "taker_fee": 0, "maker_fee": 0,
            "timestamp": int(_time.time() * 1000)})

    async def modify(self, mid, idx, base, price, trig):
        self.calls.append(("modify", mid, idx, base, price, trig))
        for o in self.orders:
            if o["order_index"] == idx:
                o["price"] = str(price / 100)
                o["trigger_price"] = str(trig / 100)

    async def cancel(self, mid, idx):
        self.calls.append(("cancel", mid, idx))
        self.orders = [o for o in self.orders if o["order_index"] != idx]

    async def active(self):
        return [dict(o) for o in self.orders]

    async def account(self):
        return {"collateral": 37.03, "available": 14.23, "portfolio": 37.03,
                "positions": list(self.positions),
                "orders": [dict(o) for o in self.orders]}

    async def trades(self, limit):
        return [dict(t) for t in self.trades_list[:limit]]


def mk(ops=None):
    ops = ops or FakeOps()
    a = LighterAdapter(ops, FakeBridge(), ACCT, FakeMarket(), slippage=0.05)
    return a, ops


def approved(**kw):
    base = dict(market="ETH", side="long", notional=228.0, size_usd_risk=1.85,
                leverage=10.0, margin=22.8, margin_mode="isolated",
                stop_px=2445.0, tp_px=2505.0, entry_px=2465.0, size=0.0925)
    base.update(kw)
    return Approved(**base)


def pos(**kw):
    base = dict(id=1, market="ETH", side="long", entry_px=2465.0, size=0.0925,
                notional=228.0, leverage=10.0, margin_mode="isolated",
                stop_px=2445.0, tp_px=2505.0,
                conviction=0.8, source="own", rationale=None, invalidation=None,
                status="open", opened_ts=0.0)
    base.update(kw)
    return Position(**base)


def test_resting_entry_is_one_grouped_action_with_inheriting_legs():
    a, ops = mk()
    out = a.place_resting_entry(approved(), 0.0925)
    assert ops.leverage == [(0, False, 10)]
    assert [c[0] for c in ops.calls] == ["grouped"]
    _, _, legs = ops.calls[0]
    parent, stop, tp = legs
    assert parent["tif"] == "post" and not parent["reduce_only"]
    assert parent["base_amount"] == 925          # 0.0925 floored to the lot
    assert stop["base_amount"] == 0 and tp["base_amount"] == 0
    assert stop["reduce_only"] and tp["reduce_only"]
    assert stop["order_type"] == "stop" and tp["order_type"] == "tp"
    assert stop["is_ask"] and tp["is_ask"]       # selling closes a long
    assert out["size"] == 0.0925 and len(out["children"]) == 2


def test_resting_entry_refuses_zero_size_before_touching_the_venue():
    a, ops = mk()
    with pytest.raises(RuntimeError, match="rounds to 0"):
        a.place_resting_entry(approved(), 0.0)
    assert ops.calls == [] and ops.leverage == []


def test_resting_entry_unconfirmed_by_readback_raises(monkeypatch):
    import peri.lighter_adapter as la
    monkeypatch.setattr(la, "_READBACK_SECS", 0.05)
    monkeypatch.setattr(la, "_POLL_SECS", 0.01)
    a, ops = mk()

    async def no_orders():
        return []
    ops.active = no_orders
    with pytest.raises(RuntimeError, match="did not confirm"):
        a.place_resting_entry(approved(), 0.0925)


def test_open_entry_sends_exactly_the_approved_lot():
    a, ops = mk()
    fill = a.open_entry(approved(notional=228.5, size=0.0925), mark=2481.0)
    kind, _, base, avg, ask, reduce = ops.calls[0]
    assert (kind, base, ask, reduce) == ("market", 925, False, False)
    assert avg == 260505                         # 5% guard above the mark, as ticks
    assert fill["size"] == pytest.approx(0.0925)


def test_open_entry_without_a_lot_raises_instead_of_guessing():
    a, _ = mk()
    with pytest.raises(RuntimeError, match="no venue lot"):
        a.open_entry(approved(size=0.0), mark=2481.0)


def test_open_attaches_brackets_and_cleans_up_on_bracket_failure():
    a, ops = mk()
    ops.fail_send_at = 2   # stop ok, tp refused
    with pytest.raises(RuntimeError, match="venue refused"):
        a.open(approved(), mark=2481.0)
    kinds = [c[0] for c in ops.calls]
    assert kinds.count("market") == 2            # the open, then the cleanup close
    assert kinds.count("cancel") == 1            # the orphaned stop leg
    cleanup = ops.calls[-1]
    assert cleanup[4] is True and cleanup[5] is True   # sell, reduce-only


def test_bracket_orders_classify_by_venue_type():
    a, ops = mk()
    a.place_resting_entry(approved(), 0.0925)
    brackets = a.bracket_orders("ETH")
    assert len(brackets) == 2
    by_kind = {b["orderType"]: b for b in brackets}
    assert set(by_kind) == {"Stop Market", "Take Profit Market"}
    assert all(b["isTrigger"] and b["reduceOnly"] for b in by_kind.values())
    assert all(b["coin"] == "ETH" and b["side"] == "A"
               for b in by_kind.values())
    entries = [o for o in a.open_orders_all() if not o["isTrigger"]]
    assert len(entries) == 1 and entries[0]["orderType"] == "Limit"


def test_adjust_stop_modifies_in_place_keeping_the_index():
    a, ops = mk()
    a.place_resting_entry(approved(), 0.0925)
    before = {b["orderType"]: b["oid"] for b in a.bracket_orders("ETH")}
    out = a.adjust_stop(pos(), stop_px=2455.0, tp_px=2510.0)
    assert out["old_oids"] == sorted(before.values())
    assert [o["oid"] for o in out["new_orders"]] == sorted(before.values())
    mods = [c for c in ops.calls if c[0] == "modify"]
    assert len(mods) == 2
    assert mods[0][4] == 245500 and mods[1][4] == 251000


def test_adjust_stop_places_a_missing_leg_fresh():
    a, ops = mk()
    a.place_resting_entry(approved(), 0.0925)
    ops.orders = [o for o in ops.orders if o["type"] != "take-profit"]
    out = a.adjust_stop(pos(), stop_px=2455.0, tp_px=2510.0)
    assert len(out["old_oids"]) == 1
    assert sorted(o["kind"] for o in out["new_orders"]) == ["sl", "tp"]
    assert len(a.bracket_orders("ETH")) == 2


def test_close_sells_reduce_only_then_clears_brackets():
    a, ops = mk()
    a.place_resting_entry(approved(), 0.0925)
    out = a.close(pos(), mark=2481.0)
    assert out["close_px"] == pytest.approx(2481.0 * 0.95, rel=1e-9)
    kinds = [c[0] for c in ops.calls if c[0] in ("market", "cancel")]
    assert kinds[0] == "market"
    assert a.bracket_orders("ETH") == []


def test_close_uses_the_venue_size_not_the_ledger_row():
    a, ops = mk()
    ops.positions = [{"symbol": "ETH", "size": 0.05, "side": "long",
                      "entry_px": 2465.0, "upnl": 1.0, "margin": 12.0,
                      "liq_px": 2200.0}]
    a.close(pos(size=0.0925), mark=2481.0)
    market_call = next(c for c in ops.calls if c[0] == "market")
    assert market_call[2] == 500                # 0.05, not the stale 0.0925


def test_fills_map_open_close_dirs_and_my_pnl():
    a, ops = mk()
    ops.trades_list = [
        {"trade_id": 1, "tx_hash": "0x1", "market_id": 0,
         "size": "0.5", "price": "2465.0", "is_maker_ask": True,
         "bid_account_id": ACCT, "ask_account_id": 7,
         "taker_position_size_before": "0.0", "maker_position_size_before": "3.0",
         "bid_account_pnl": "0.0", "ask_account_pnl": "0.0",
         "taker_fee": 0, "maker_fee": 0, "timestamp": 1788800000001},
        {"trade_id": 2, "tx_hash": "0x2", "market_id": 0,
         "size": "9.0", "price": "12.91758", "is_maker_ask": True,
         "bid_account_id": ACCT, "ask_account_id": 7684,
         "taker_position_size_before": "-9.0", "maker_position_size_before": "0.0",
         "bid_account_pnl": "0.74178", "ask_account_pnl": "0.0",
         "taker_fee": 280, "maker_fee": 0, "timestamp": 1788800000002},
        {"trade_id": 3, "tx_hash": "0x3", "market_id": 0,
         "size": "1.0", "price": "2500.0", "is_maker_ask": False,
         "bid_account_id": 9, "ask_account_id": 10,      # not mine
         "taker_position_size_before": "0.0", "maker_position_size_before": "0.0",
         "bid_account_pnl": "5.0", "ask_account_pnl": "0.0",
         "taker_fee": 0, "maker_fee": 0, "timestamp": 1788800000003},
    ]
    fills = a.fills()
    assert len(fills) == 2
    opened, closed = fills
    assert opened["dir"] == "Open Long" and opened["coin"] == "ETH"
    assert opened["sz"] == pytest.approx(0.5)
    assert closed["dir"] == "Close Short"
    assert closed["closedPnl"] == pytest.approx(0.74178)
    assert closed["fee"] == pytest.approx(280 / 1_000_000.0)
    assert closed["tid"] == "2" and closed["time"] == 1788800000002


def test_snapshot_shape_matches_what_the_engine_reads():
    a, ops = mk()
    ops.positions = [{"symbol": "ETH", "size": 0.5, "side": "long",
                      "entry_px": 2465.0, "upnl": 8.0, "margin": 123.25,
                      "liq_px": 2200.0}]
    snap = a.account_snapshot()
    assert snap["abstraction"] == "lighter"
    assert snap["equity"] == pytest.approx(37.03)
    assert snap["available_margin"] == pytest.approx(14.23)
    assert snap["account_value_by_dex"] == {"lighter": pytest.approx(37.03)}
    (p,) = snap["positions"]
    assert (p["market"], p["side"], p["size"]) == ("ETH", "long", 0.5)
    assert p["leverage"] == pytest.approx(2465.0 * 0.5 / 123.25)
    assert p["margin_mode"] == "isolated"
    assert p["liquidation_px"] == pytest.approx(2200.0)
    assert a.equity({}) == pytest.approx(37.03)


def test_cancel_orders_targets_the_market_index():
    a, ops = mk()
    a.place_resting_entry(approved(), 0.0925)
    oid = a.open_orders_all()[0]["oid"]
    a.cancel_orders("ETH", [oid])
    assert ("cancel", 0, oid) in ops.calls
    assert oid not in [o["oid"] for o in a.open_orders_all()]
