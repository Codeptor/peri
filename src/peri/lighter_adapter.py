"""Live Lighter executor. Implements the peri.router.Adapter protocol.

The engine never sees a Lighter-native shape: orders, positions and fills are
normalized to the same dicts the Hyperliquid adapter returns (coin/oid/side/
limitPx/triggerPx/reduceOnly/isTrigger/orderType/timestamp, and tid/hash/coin/
px/sz/dir/closedPnl/fee/time), so the gates, reconciliation and ledger work
unchanged. The venue differences live here:

- One signed action. A resting entry goes in as a grouped OTOCO whose stop and
  take-profit legs carry size zero and inherit whatever the parent actually
  fills — proven against testnet 2026-09-06, including the read-back. A partial
  fill is protected in proportion by the venue; peri never re-arms from the
  ledger the way it must on HL.
- In-place ratchet. modify_order moves a trigger keeping its order index, so
  adjust_stop never leaves the position momentarily naked (no place-before-
  cancel dance).
- Margin is sized from the MARKET's configured leverage, not from anything in
  the order. The adapter sets it before every open; without that the venue
  reserves at the market default and refuses the order for margin it never
  needed. Found live 2026-09-06 when the first 40x ETH order was refused twice.
- Fees are zero at the standard tier. fills() still reports a fee field, but it
  is dust; the guard prices Lighter trades with fees.ZERO, not the HL schedule.

Clients are injected as `ops` (async methods); unit tests use fakes and never
touch the network. `SdkOps` is the production implementation over lighter-sdk,
driven through a lighter_sync.Bridge because the engine is synchronous.
"""

import time
from typing import Optional

from peri.lighter_market import LighterMarket, resolve
from peri.risk import Approved
from peri.state import Position

ORDER_EXPIRY_HOURS = 24.0
_READBACK_SECS = 20.0
_FILL_WAIT_SECS = 20.0
_POLL_SECS = 1.5
# Venue fee units are 1e6 USDC ($0.00028 on the observed fills). Dust either
# way; the guard prices Lighter at exactly zero.
_FEE_SCALE = 1_000_000.0


def _coi_base() -> int:
    return int(time.time() * 1000) % 1_000_000_000


def _expiry_ms() -> int:
    return int((time.time() + ORDER_EXPIRY_HOURS * 3600) * 1000)


class LighterAdapter:
    def __init__(self, ops, bridge, account_index: int, market: LighterMarket,
                 slippage: float = 0.05):
        self.ops = ops
        self.bridge = bridge
        self.account_index = str(account_index)
        self.market = market
        self.slippage = slippage

    # -- small helpers ---------------------------------------------------
    def _call(self, coro):
        return self.bridge.call(coro)

    def _mid(self, peri_name: str) -> int:
        return self.market.market_id(resolve(peri_name))

    @staticmethod
    def _role(order_type) -> tuple[str, bool]:
        text = str(order_type or "").lower()
        if text.startswith("stop"):
            return "Stop Market", True
        if text.startswith("take"):
            return "Take Profit Market", True
        return "Limit", False

    def _norm_order(self, symbol: str, o: dict) -> dict:
        otype, is_trigger = self._role(o.get("type"))
        trig = o.get("trigger_price")
        return {
            "coin": symbol,
            "oid": int(o["order_index"]),
            "side": "A" if o.get("is_ask") else "B",
            "sz": float(o.get("initial_base_amount") or 0),
            "limitPx": float(o.get("price") or 0),
            "orderType": otype,
            "reduceOnly": bool(o.get("reduce_only")),
            "isTrigger": is_trigger,
            "triggerPx": float(trig) if trig else None,
            "timestamp": int(float(o.get("timestamp") or 0) * 1000),
        }

    def _norm_fill(self, sym_by_id: dict[int, str], t: dict) -> Optional[dict]:
        mine_bid = str(t.get("bid_account_id")) == self.account_index
        mine_ask = str(t.get("ask_account_id")) == self.account_index
        if not (mine_bid or mine_ask):
            return None
        buy = mine_bid  # a self-trade reads from the bid side; direction is the same
        maker_ask = bool(t.get("is_maker_ask"))
        i_am_taker = (buy and maker_ask) or (not buy and not maker_ask)
        before_raw = (t.get("taker_position_size_before")
                      if i_am_taker else t.get("maker_position_size_before"))
        try:
            before = float(before_raw or 0)
            size = float(t.get("size") or 0)
        except (TypeError, ValueError):
            return None
        if size <= 0:
            return None
        if before == 0:
            direction = "Open Long" if buy else "Open Short"
        elif (buy and before < 0) or (not buy and before > 0):
            direction = "Close Short" if before < 0 else "Close Long"
        else:
            direction = "Open Long" if buy else "Open Short"
        pnl_raw = t.get("bid_account_pnl") if buy else t.get("ask_account_pnl")
        fee_raw = t.get("taker_fee") if i_am_taker else t.get("maker_fee")
        try:
            pnl = float(pnl_raw or 0)
        except (TypeError, ValueError):
            pnl = 0.0
        try:
            fee = float(fee_raw or 0) / _FEE_SCALE
        except (TypeError, ValueError):
            fee = 0.0
        try:
            mid = int(t.get("market_id"))
        except (TypeError, ValueError):
            return None
        return {
            "tid": str(t.get("trade_id") or t.get("tx_hash") or ""),
            "hash": t.get("tx_hash") or "",
            "coin": sym_by_id.get(mid, f"lighter:{mid}"),
            "px": float(t.get("price") or 0),
            "sz": size,
            "dir": direction,
            "closedPnl": pnl,
            "fee": fee,
            "time": int(t.get("timestamp") or 0),
        }

    def _await_orders(self, market_id: int, cois: list[int]) -> dict[int, dict]:
        want = set(cois)
        deadline = time.time() + _READBACK_SECS
        while True:
            found = {}
            for o in self._call(self.ops.active()):
                try:
                    mid = int(o.get("market_index", -1))
                    coi = int(o.get("client_order_index", -1))
                except (TypeError, ValueError):
                    continue
                if mid == market_id and coi in want:
                    found[coi] = o
            if len(found) == len(want):
                return found
            if time.time() >= deadline:
                raise RuntimeError(
                    f"lighter did not confirm orders {sorted(want - set(found))} "
                    f"on market {market_id}")
            time.sleep(_POLL_SECS)

    def _await_fill(self, market_id: int, since_ms: int) -> dict:
        """Aggregate my trades on this market since t0 into one fill."""
        deadline = time.time() + _FILL_WAIT_SECS
        sym_by_id = self.market.symbols_by_id()
        while True:
            px_sz = 0.0
            total = 0.0
            for t in self._call(self.ops.trades(50)):
                try:
                    mid = int(t.get("market_id", -1))
                    ts = int(t.get("timestamp") or 0)
                except (TypeError, ValueError):
                    continue
                if mid != market_id or ts < since_ms:
                    continue
                norm = self._norm_fill(sym_by_id, t)
                if norm is None:
                    continue
                px_sz += norm["px"] * norm["sz"]
                total += norm["sz"]
            if total > 0:
                return {"entry_px": px_sz / total, "size": total}
            if time.time() >= deadline:
                raise RuntimeError(
                    f"lighter market order on {market_id} shows no fill "
                    f"after {_FILL_WAIT_SECS:.0f}s")
            time.sleep(_POLL_SECS)

    # -- account ---------------------------------------------------------
    def account_snapshot(self, marks: Optional[dict[str, float]] = None) -> dict:
        acct = self._call(self.ops.account())
        positions = []
        margin_used = 0.0
        for p in acct.get("positions", []):
            try:
                size = float(p["size"])
                entry_px = float(p["entry_px"])
                margin = float(p.get("margin") or 0)
            except (KeyError, TypeError, ValueError) as e:
                raise RuntimeError(f"lighter position is invalid: {p!r}") from e
            if size <= 0 or entry_px <= 0:
                raise RuntimeError(f"lighter position is invalid: {p!r}")
            symbol = p["symbol"]
            notional = entry_px * size
            leverage = (notional / margin if margin > 0
                        else self.market.info(symbol).max_leverage)
            margin_used += margin
            liq = p.get("liq_px")
            positions.append({
                "market": symbol,
                "side": p["side"],
                "size": size,
                "entry_px": entry_px,
                "leverage": leverage,
                "margin_mode": "isolated",
                "margin": margin,
                "position_value": notional,
                "upnl": float(p.get("upnl") or 0),
                "liquidation_px": float(liq) if liq else None,
                "roe": None,
            })
        equity = float(acct.get("portfolio", acct.get("collateral", 0)))
        available = float(acct.get("available", equity))
        return {
            "abstraction": "lighter",
            "equity": equity,
            "spot_usdc_total": float(acct.get("collateral", 0)),
            "held_collateral": max(0.0, equity - available),
            "available_margin": available,
            "total_margin_used": margin_used,
            "account_value_by_dex": {"lighter": equity},
            "margin_used_by_dex": {"lighter": margin_used},
            "withdrawable_by_dex": {"lighter": available},
            "positions": positions,
        }

    def equity(self, marks: dict[str, float]) -> float:
        return self.account_snapshot()["equity"]

    # -- orders ----------------------------------------------------------
    def open_orders_all(self) -> list[dict]:
        out = []
        for o in self._call(self.ops.active()):
            try:
                mid = int(o.get("market_index"))
            except (TypeError, ValueError):
                raise RuntimeError(f"lighter order is malformed: {o!r}")
            symbol = self.market.symbols_by_id().get(mid, f"lighter:{mid}")
            if "order_index" not in o:
                raise RuntimeError(f"lighter order is malformed: {o!r}")
            out.append(self._norm_order(symbol, o))
        return out

    def bracket_orders(self, market: str) -> list[dict]:
        symbol = resolve(market)
        return [o for o in self.open_orders_all()
                if o["coin"] == symbol and o["isTrigger"]]

    def cancel_orders(self, market: str, order_ids: list[int]) -> None:
        mid = self._mid(market)
        for oid in order_ids:
            self._call(self.ops.cancel(mid, int(oid)))

    def cancel_brackets(self, market: str) -> None:
        self.cancel_orders(market, [o["oid"] for o in self.bracket_orders(market)])

    def place_brackets(self, market: str, side: str, size: float,
                       stop_px: float, tp_px: float) -> list[dict]:
        symbol = resolve(market)
        mid = self._mid(market)
        size_int = self.market.sz_int(symbol, size)
        if size_int <= 0:
            raise RuntimeError(f"size rounds to 0 for {market}")
        is_ask = side == "long"   # the close side sells
        base = _coi_base()
        legs = [
            {"coi": base, "base_amount": size_int,
             "price": self.market.px_int(symbol, stop_px),
             "is_ask": is_ask, "order_type": "stop", "tif": "ioc",
             "reduce_only": True,
             "trigger_price": self.market.px_int(symbol, stop_px),
             "expiry": _expiry_ms()},
            {"coi": base + 1, "base_amount": size_int,
             "price": self.market.px_int(symbol, tp_px),
             "is_ask": is_ask, "order_type": "tp", "tif": "ioc",
             "reduce_only": True,
             "trigger_price": self.market.px_int(symbol, tp_px),
             "expiry": _expiry_ms()},
        ]
        placed: list[dict] = []
        try:
            for leg in legs:
                self._call(self.ops.send_order(mid, leg))
                placed.append(leg)
            found = self._await_orders(mid, [leg["coi"] for leg in legs])
        except Exception:
            for leg in placed:
                try:
                    oids = self._await_orders(mid, [leg["coi"]])
                    self._call(self.ops.cancel(mid, int(
                        oids[leg["coi"]]["order_index"])))
                except Exception:  # noqa: BLE001 — best-effort cleanup
                    pass
            raise
        out = []
        for leg, kind in zip(legs, ("sl", "tp")):
            o = found[leg["coi"]]
            out.append({"oid": int(o["order_index"]), "kind": kind,
                        "trigger_px": self.market.round_px(symbol, stop_px
                                                           if kind == "sl" else tp_px)})
        return out

    def place_resting_entry(self, ap: Approved, size: float) -> dict:
        """One grouped OTOCO: post-only maker parent, stop + take-profit legs at
        size zero that inherit the parent's actual fill. Returns the parent oid
        plus the leg oids read back off the venue."""
        symbol = resolve(ap.market)
        mid = self._mid(ap.market)
        if size <= 0:
            raise RuntimeError(
                f"size rounds to 0 for {ap.market} notional ${ap.notional:.2f}")
        size_int = self.market.sz_int(symbol, size)
        if size_int <= 0:
            raise RuntimeError(
                f"size rounds to 0 for {ap.market} notional ${ap.notional:.2f}")
        self._call(self.ops.set_leverage(
            mid, ap.margin_mode == "cross", int(ap.leverage)))
        long = ap.side == "long"
        base = _coi_base()
        expiry = _expiry_ms()
        legs = [
            {"coi": base, "base_amount": size_int,
             "price": self.market.px_int(symbol, ap.entry_px),
             "is_ask": not long, "order_type": "limit", "tif": "post",
             "reduce_only": False, "trigger_price": 0, "expiry": expiry},
            {"coi": base + 1, "base_amount": 0,
             "price": self.market.px_int(symbol, ap.stop_px),
             "is_ask": long, "order_type": "stop", "tif": "ioc",
             "reduce_only": True,
             "trigger_price": self.market.px_int(symbol, ap.stop_px),
             "expiry": expiry},
            {"coi": base + 2, "base_amount": 0,
             "price": self.market.px_int(symbol, ap.tp_px),
             "is_ask": long, "order_type": "tp", "tif": "ioc",
             "reduce_only": True,
             "trigger_price": self.market.px_int(symbol, ap.tp_px),
             "expiry": expiry},
        ]
        self._call(self.ops.send_grouped(mid, legs))
        found = self._await_orders(mid, [leg["coi"] for leg in legs])
        return {"oid": int(found[base]["order_index"]),
                "entry_px": self.market.round_px(symbol, ap.entry_px),
                "size": size,
                "children": [int(found[base + 1]["order_index"]),
                             int(found[base + 2]["order_index"])]}

    def open_entry(self, ap: Approved, mark: float) -> dict:
        # The guard produced the exact venue lot; re-deriving it here was a
        # second rounding site on HL that could exceed the approved risk. No
        # safe fallback: an Approved without a lot is a bug upstream.
        if ap.size <= 0:
            raise RuntimeError(
                f"{ap.market}: Approved carries no venue lot (size={ap.size!r}) — "
                "the guard sizes and floors every entry; refusing to re-derive it here")
        symbol = resolve(ap.market)
        mid = self._mid(ap.market)
        size_int = self.market.sz_int(symbol, ap.size)
        if size_int <= 0:
            raise RuntimeError(
                f"size rounds to 0 for {ap.market} notional ${ap.notional:.2f}")
        self._call(self.ops.set_leverage(
            mid, ap.margin_mode == "cross", int(ap.leverage)))
        is_ask = ap.side == "short"
        guard = (mark * (1 - self.slippage) if is_ask
                 else mark * (1 + self.slippage))
        since = int(time.time() * 1000)
        self._call(self.ops.send_market(
            mid, _coi_base(), size_int,
            self.market.px_int(symbol, guard), is_ask, False))
        fill = self._await_fill(mid, since)
        return {"entry_px": fill["entry_px"], "size": fill["size"]}

    def open(self, ap: Approved, mark: float) -> dict:
        fill = self.open_entry(ap, mark)
        try:
            self.place_brackets(ap.market, ap.side, fill["size"],
                                ap.stop_px, ap.tp_px)
        except Exception:
            transient = Position(
                0, ap.market, ap.side, fill["entry_px"], fill["size"], ap.notional,
                ap.leverage, ap.margin_mode, ap.stop_px, ap.tp_px, None, "cleanup",
                None, None, "open", time.time(),
            )
            self.close_position_only(transient)
            raise
        return fill

    def close_position_only(self, pos: Position) -> dict:
        symbol = resolve(pos.market)
        mid = self._mid(pos.market)
        size = pos.size
        for p in self._call(self.ops.account()).get("positions", []):
            if p.get("symbol") == symbol:
                try:
                    size = float(p.get("size") or 0) or size
                except (TypeError, ValueError):
                    pass
                break
        size_int = self.market.sz_int(symbol, size)
        if size_int <= 0:
            raise RuntimeError(f"nothing to close on {pos.market}")
        is_ask = pos.side == "long"   # selling closes a long
        mark = self.market.mark(symbol)
        guard = (mark * (1 - self.slippage) if is_ask
                 else mark * (1 + self.slippage))
        since = int(time.time() * 1000)
        self._call(self.ops.send_market(
            mid, _coi_base(), size_int,
            self.market.px_int(symbol, guard), is_ask, True))
        fill = self._await_fill(mid, since)
        return {"close_px": fill["entry_px"]}

    def close(self, pos: Position, mark: float) -> dict:
        result = self.close_position_only(pos)
        self.cancel_brackets(pos.market)
        return result

    def adjust_stop(self, pos: Position, stop_px: float, tp_px: float) -> dict:
        """Move each trigger in place, keeping its order index — the position is
        never momentarily naked. A missing leg is placed fresh."""
        symbol = resolve(pos.market)
        mid = self._mid(pos.market)
        legs = self.bracket_orders(pos.market)
        stop_o = next((o for o in legs if o["orderType"].startswith("Stop")), None)
        tp_o = next((o for o in legs
                     if o["orderType"].startswith("Take Profit")), None)
        old_oids = [o["oid"] for o in legs]
        new_orders = []
        for existing, kind, px in ((stop_o, "sl", stop_px), (tp_o, "tp", tp_px)):
            px_int = self.market.px_int(symbol, px)
            if existing is not None:
                base_int = self.market.sz_int(symbol, existing["sz"])
                self._call(self.ops.modify(
                    mid, existing["oid"], base_int, px_int, px_int))
                new_orders.append({"oid": existing["oid"], "kind": kind,
                                   "trigger_px": self.market.round_px(symbol, px)})
            else:
                is_ask = pos.side == "long"
                size_int = self.market.sz_int(symbol, pos.size)
                if size_int <= 0:
                    raise RuntimeError(f"size rounds to 0 for {pos.market}")
                base = _coi_base()
                self._call(self.ops.send_order(mid, {
                    "coi": base, "base_amount": size_int, "price": px_int,
                    "is_ask": is_ask,
                    "order_type": "stop" if kind == "sl" else "tp",
                    "tif": "ioc", "reduce_only": True,
                    "trigger_price": px_int, "expiry": _expiry_ms()}))
                found = self._await_orders(mid, [base])
                new_orders.append({"oid": int(found[base]["order_index"]),
                                   "kind": kind,
                                   "trigger_px": self.market.round_px(symbol, px)})
        return {"old_oids": old_oids, "new_orders": new_orders}

    def fills(self) -> list[dict]:
        sym_by_id = self.market.symbols_by_id()
        out = []
        for t in self._call(self.ops.trades(100)):
            norm = self._norm_fill(sym_by_id, t)
            if norm is not None:
                out.append(norm)
        return out


class SdkOps:
    """Production ops over lighter-sdk. Every method is a coroutine for the
    bridge; lighter imports stay inside this class so the adapter module (and
    its offline tests) never needs the SDK or its compiled signer."""

    AUTH_SKEW_SECS = 60.0

    def __init__(self, host: str, chain_id: int, account_index: int,
                 api_key_index: int, api_private_key: str):
        self.host = host.rstrip("/")
        self.chain_id = chain_id
        self.account_index = account_index
        self.api_key_index = api_key_index
        self.api_private_key = api_private_key
        self._client = None
        self._auth: Optional[str] = None
        self._auth_ts = 0.0

    async def _cli(self):
        if self._client is None:
            from lighter import signer_client as sc
            self._client = sc.SignerClient(
                url=self.host, account_index=self.account_index,
                api_private_keys={self.api_key_index: self.api_private_key},
                chain_id=self.chain_id)
            err = self._client.check_client()
            if err is not None:
                raise RuntimeError(f"lighter API key is not usable: {err}")
        return self._client

    async def _auth_token(self) -> str:
        client = await self._cli()
        if (self._auth is None
                or time.time() - self._auth_ts > 9 * 60 - self.AUTH_SKEW_SECS):
            auth, err = client.create_auth_token_with_expiry(
                client.DEFAULT_10_MIN_AUTH_EXPIRY)
            if err:
                raise RuntimeError(f"lighter auth token failed: {err}")
            self._auth, self._auth_ts = auth, time.time()
        return self._auth

    def _api(self):
        import lighter
        return lighter.ApiClient(lighter.Configuration(host=self.host))

    @staticmethod
    def _raise(what: str, err) -> None:
        if err:
            raise RuntimeError(f"lighter {what} failed: {err}")

    async def set_leverage(self, market_index: int, cross: bool, leverage: int) -> None:
        from lighter import signer_client as sc
        client = await self._cli()
        mode = sc.SignerClient.CROSS_MARGIN_MODE if cross else sc.SignerClient.ISOLATED_MARGIN_MODE
        _, _, err = await client.update_leverage(
            market_index=market_index, margin_mode=mode, leverage=leverage,
            api_key_index=self.api_key_index)
        self._raise("set leverage", err)

    def _req(self, client, mid: int, leg: dict):
        from lighter import signer_client as sc
        ot = {"limit": client.ORDER_TYPE_LIMIT, "stop": client.ORDER_TYPE_STOP_LOSS,
              "tp": client.ORDER_TYPE_TAKE_PROFIT}[leg["order_type"]]
        tif = {"post": client.ORDER_TIME_IN_FORCE_POST_ONLY,
               "ioc": client.ORDER_TIME_IN_FORCE_IMMEDIATE_OR_CANCEL,
               "gtt": client.ORDER_TIME_IN_FORCE_GOOD_TILL_TIME}[leg["tif"]]
        return sc.CreateOrderTxReq(
            MarketIndex=mid, ClientOrderIndex=leg["coi"],
            BaseAmount=leg["base_amount"], Price=leg["price"],
            IsAsk=1 if leg["is_ask"] else 0, Type=ot, TimeInForce=tif,
            ReduceOnly=1 if leg["reduce_only"] else 0,
            TriggerPrice=leg.get("trigger_price") or client.NIL_TRIGGER_PRICE,
            OrderExpiry=leg.get("expiry") or client.DEFAULT_28_DAY_ORDER_EXPIRY)

    async def send_grouped(self, market_index: int, legs: list[dict]) -> None:
        client = await self._cli()
        _, _, err = await client.create_grouped_orders(
            grouping_type=client.GROUPING_TYPE_ONE_TRIGGERS_A_ONE_CANCELS_THE_OTHER,
            orders=[self._req(client, market_index, leg) for leg in legs],
            api_key_index=self.api_key_index)
        self._raise("grouped order", err)

    async def send_order(self, market_index: int, leg: dict) -> None:
        client = await self._cli()
        _, _, err = await client.create_order(
            market_index=market_index, client_order_index=leg["coi"],
            base_amount=leg["base_amount"], price=leg["price"],
            is_ask=leg["is_ask"],
            order_type={"limit": client.ORDER_TYPE_LIMIT,
                        "stop": client.ORDER_TYPE_STOP_LOSS,
                        "tp": client.ORDER_TYPE_TAKE_PROFIT}[leg["order_type"]],
            time_in_force={"post": client.ORDER_TIME_IN_FORCE_POST_ONLY,
                           "ioc": client.ORDER_TIME_IN_FORCE_IMMEDIATE_OR_CANCEL,
                           "gtt": client.ORDER_TIME_IN_FORCE_GOOD_TILL_TIME}[leg["tif"]],
            reduce_only=leg["reduce_only"],
            trigger_price=leg.get("trigger_price") or client.NIL_TRIGGER_PRICE,
            order_expiry=leg.get("expiry") or client.DEFAULT_28_DAY_ORDER_EXPIRY,
            api_key_index=self.api_key_index)
        self._raise("order", err)

    async def send_market(self, market_index: int, coi: int, base_amount: int,
                          avg_price: int, is_ask: bool, reduce_only: bool) -> None:
        client = await self._cli()
        _, _, err = await client.create_market_order(
            market_index=market_index, client_order_index=coi,
            base_amount=base_amount, avg_execution_price=avg_price,
            is_ask=is_ask, reduce_only=reduce_only,
            api_key_index=self.api_key_index)
        self._raise("market order", err)

    async def modify(self, market_index: int, order_index: int, base_amount: int,
                     price: int, trigger_price: int) -> None:
        client = await self._cli()
        _, _, err = await client.modify_order(
            market_index=market_index, order_index=order_index,
            base_amount=base_amount, price=price, trigger_price=trigger_price,
            api_key_index=self.api_key_index)
        self._raise("modify order", err)

    async def cancel(self, market_index: int, order_index: int) -> None:
        client = await self._cli()
        _, _, err = await client.cancel_order(
            market_index=market_index, order_index=order_index,
            api_key_index=self.api_key_index)
        self._raise("cancel order", err)

    async def active(self) -> list[dict]:
        import lighter
        api = self._api()
        try:
            auth = await self._auth_token()
            r = await lighter.OrderApi(api).account_active_orders(
                authorization=auth, account_index=self.account_index,
                market_id=None)
            return [o.to_dict() for o in (r.orders or [])]
        finally:
            await api.close()

    async def account(self) -> dict:
        import lighter
        api = self._api()
        try:
            acct = (await lighter.AccountApi(api).account(
                by="index", value=str(self.account_index))).accounts[0]
            auth = await self._auth_token()
            r = await lighter.OrderApi(api).account_active_orders(
                authorization=auth, account_index=self.account_index,
                market_id=None)
            positions = []
            for p in acct.positions or []:
                try:
                    signed = float(p.position)
                except (TypeError, ValueError):
                    continue
                if signed == 0:
                    continue
                positions.append({
                    "symbol": p.symbol, "size": abs(signed),
                    "side": "long" if float(p.sign) > 0 else "short",
                    "entry_px": float(p.avg_entry_price),
                    "upnl": float(p.unrealized_pnl),
                    "margin": float(getattr(p, "allocated_margin", 0) or 0),
                    "liq_px": float(getattr(p, "liquidation_price", 0) or 0),
                })
            return {
                "collateral": float(acct.collateral),
                "portfolio": float(getattr(acct, "total_asset_value",
                                           acct.collateral)),
                "available": float(getattr(acct, "available_balance",
                                           acct.collateral)),
                "positions": positions,
                "orders": [o.to_dict() for o in (r.orders or [])],
            }
        finally:
            await api.close()

    async def trades(self, limit: int) -> list[dict]:
        import lighter
        api = self._api()
        try:
            auth = await self._auth_token()
            r = await lighter.OrderApi(api).trades(
                sort_by="timestamp", limit=max(1, min(100, limit)),
                authorization=auth, account_index=self.account_index)
            return [t.to_dict() for t in (r.trades or [])]
        finally:
            await api.close()
