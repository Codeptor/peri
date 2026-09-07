"""Live Hyperliquid executor — the proven engine (testnet-verified 2026-08-26),
now dex-aware. Trades native crypto ("BTC") and builder-dex markets ("xyz:NVDA")
with the Trench builder fee on every order. Clients are injected; unit tests use
fakes and never touch the network."""

import math
import time
from typing import Optional

from peri.hl_sizing import format_price, tpsl_limit_price
from peri.risk import Approved, ScaleOut
from peri.state import Position

# Trench's builder identity. Fee f is in tenths of a bp (30 = 0.03%).
TRENCH_BUILDER = {"b": "0x06919e1f2310aaff394481ee34925a1c59d47666", "f": 30}


class HyperliquidAdapter:
    def __init__(self, exchange, info, account_address: str, market,
                 builder: Optional[dict] = None, slippage: float = 0.05,
                 dexes=("xyz",)):
        self.ex = exchange
        self.info = info
        self.account = account_address
        self.market = market          # peri.market.Market — szDecimals / max leverage
        self.builder = builder
        self.slippage = slippage
        self.dexes = tuple(dexes)

    # -- account -----------------------------------------------------------
    def enable_dex_abstraction(self) -> None:
        """Unified collateral across native + builder dexes (agent-signed L1
        action). Idempotent server-side; unified accounts already have it (the
        action is disabled there — treated as success). Hard failures raise."""
        resp = self.ex.agent_enable_dex_abstraction()
        status = resp.get("status") if isinstance(resp, dict) else None
        text = str(resp).lower()
        # already-satisfied shapes seen live: "unified account is active",
        # "Abstraction transition not allowed" (unified accounts can't and
        # don't need to transition — collateral already routes)
        ok_anyway = "already" in text or "unified" in text or "transition not allowed" in text
        if status != "ok" and not ok_anyway:
            raise RuntimeError(f"enable_dex_abstraction failed: {resp!r}")

    @staticmethod
    def _account_number(value, field: str, default: Optional[float] = None) -> float:
        if value is None and default is not None:
            return default
        try:
            number = float(value)
        except (TypeError, ValueError) as e:
            raise RuntimeError(f"account {field} is invalid: {value!r}") from e
        if not math.isfinite(number) or number < 0:
            raise RuntimeError(f"account {field} is invalid: {number!r}")
        return number

    def account_snapshot(self, marks: Optional[dict[str, float]] = None) -> dict:
        """Return one normalized collateral snapshot across native and builder dexes."""
        abstraction = self.info.post(
            "/info", {"type": "userAbstraction", "user": self.account})
        sp = self.info.post("/info", {"type": "spotClearinghouseState", "user": self.account})
        balances = sp.get("balances") if isinstance(sp, dict) else None
        if not isinstance(balances, list):
            raise RuntimeError("spot balances response must be a list")
        spot_usdc = None
        spot_hold = 0.0
        for b in balances:
            if b.get("coin") == "USDC":
                spot_usdc = self._account_number(b.get("total"), "USDC total")
                spot_hold = self._account_number(b.get("hold"), "USDC hold", 0.0)

        account_value_by_dex = {}
        margin_used_by_dex = {}
        withdrawable_by_dex = {}
        positions = []
        for dex in ("", *self.dexes):
            body = {"type": "clearinghouseState", "user": self.account}
            if dex:
                body["dex"] = dex
            st = self.info.post("/info", body)
            summary = st.get("marginSummary") if isinstance(st, dict) else None
            if not isinstance(summary, dict):
                raise RuntimeError(f"{dex or 'native'} margin summary is invalid")
            label = dex or "native"
            account_value_by_dex[label] = self._account_number(
                summary.get("accountValue"), f"{label} accountValue")
            margin_used_by_dex[label] = self._account_number(
                summary.get("totalMarginUsed"), f"{label} totalMarginUsed", 0.0)
            withdrawable_by_dex[label] = self._account_number(
                st.get("withdrawable"), f"{label} withdrawable", 0.0)
            asset_positions = st.get("assetPositions", [])
            if not isinstance(asset_positions, list):
                raise RuntimeError(f"{label} assetPositions is invalid")
            for asset_position in asset_positions:
                position = (asset_position.get("position")
                            if isinstance(asset_position, dict) else None)
                if not isinstance(position, dict):
                    raise RuntimeError(f"{label} position is invalid")
                try:
                    signed_size = float(position.get("szi") or 0)
                    entry_px = float(position.get("entryPx") or 0)
                    leverage_data = position.get("leverage") or {}
                    leverage = float(leverage_data.get("value") or 0)
                    upnl = float(position.get("unrealizedPnl") or 0)
                    roe = float(position.get("returnOnEquity") or 0)
                except (AttributeError, TypeError, ValueError) as e:
                    raise RuntimeError(f"{label} position is invalid: {position!r}") from e
                if signed_size == 0:
                    continue
                values = (signed_size, entry_px, leverage, upnl, roe)
                if (not all(math.isfinite(value) for value in values)
                        or entry_px <= 0 or leverage <= 0):
                    raise RuntimeError(f"{label} position is invalid: {position!r}")
                liquidation = position.get("liquidationPx")
                liquidation_px = (None if liquidation in (None, "") else
                                  self._account_number(liquidation, "liquidationPx"))
                margin_mode = leverage_data.get("type")
                if margin_mode not in {"cross", "isolated"}:
                    margin_mode = "unknown"
                positions.append({
                    "market": position.get("coin"),
                    "side": "long" if signed_size > 0 else "short",
                    "size": abs(signed_size),
                    "entry_px": entry_px,
                    "leverage": leverage,
                    "margin_mode": margin_mode,
                    "margin": self._account_number(
                        position.get("marginUsed"), "marginUsed", 0.0),
                    "position_value": self._account_number(
                        position.get("positionValue"), "positionValue", 0.0),
                    "upnl": upnl,
                    "liquidation_px": liquidation_px,
                    "roe": roe,
                })

        if abstraction == "unifiedAccount":
            if spot_usdc is None:
                raise RuntimeError("unified USDC balance unavailable")
            # Free spot, plus the LIVE value of everything routed to the perp
            # dexes. Reading spot total alone would be equity only if that total
            # re-prices with open positions, which is exactly the thing that
            # cannot be observed while the book is flat — and the kill switch
            # and day-loss halt are worthless if they only see realised losses.
            # Each dex accountValue already carries its own unrealised PnL, so
            # this is mark-to-market by construction. It is identical to the old
            # reading whenever hold == the routed value, which is the normal
            # case; routed_value is what makes it true when it is not.
            routed_value = sum(account_value_by_dex.values())
            equity = max(0.0, spot_usdc - spot_hold) + routed_value
            available = max(0.0, spot_usdc - spot_hold)
            drift = equity - spot_usdc
            if abs(drift) > max(0.01, spot_usdc * 0.001):
                # Worth seeing: it means the spot hold and the routed collateral
                # have diverged, which is precisely the open-position case.
                print(f"[peri] equity is mark-to-market ${equity:.4f} vs spot total "
                      f"${spot_usdc:.4f} (routed ${routed_value:.4f} vs hold "
                      f"${spot_hold:.4f}, {drift:+.4f})", flush=True)
        elif abstraction == "portfolioMargin":
            raise RuntimeError("portfolio-margin equity is not supported")
        elif abstraction in ("default", "disabled"):
            equity = (spot_usdc or 0.0) + sum(account_value_by_dex.values())
            available = (spot_usdc or 0.0) + sum(withdrawable_by_dex.values())
        else:
            raise RuntimeError(f"unexpected account abstraction: {abstraction!r}")

        return {
            "abstraction": abstraction,
            "equity": equity,
            "spot_usdc_total": spot_usdc or 0.0,
            "held_collateral": spot_hold,
            "available_margin": available,
            "total_margin_used": sum(margin_used_by_dex.values()),
            "account_value_by_dex": account_value_by_dex,
            "margin_used_by_dex": margin_used_by_dex,
            "withdrawable_by_dex": withdrawable_by_dex,
            "positions": positions,
        }

    def equity(self, marks: dict[str, float]) -> float:
        """Return USDC equity without double-counting unified-account holds."""
        return self.account_snapshot()["equity"]

    # -- orders ------------------------------------------------------------
    @staticmethod
    def _fill_px(resp: dict) -> float:
        st = resp["response"]["data"]["statuses"][0]
        if "filled" not in st:
            raise RuntimeError(f"order not filled: {st!r}")
        return float(st["filled"]["avgPx"])

    def _trigger(self, coin: str, is_buy: bool, sz: float, px: float, tpsl: str,
                 szd: int):
        trigger_px = format_price(px, szd)
        limit_px = tpsl_limit_price(trigger_px, close_is_buy=is_buy, sz_decimals=szd)
        ot = {"trigger": {"triggerPx": trigger_px, "isMarket": True, "tpsl": tpsl}}
        return self.ex.order(coin, is_buy, sz, limit_px, ot, reduce_only=True,
                             builder=self.builder)

    def cancel_brackets(self, coin: str) -> None:
        self.cancel_orders(coin, [order["oid"] for order in self.bracket_orders(coin)])

    def bracket_orders(self, coin: str) -> list[dict]:
        orders = []
        for order in self.open_orders_all():
            if order.get("coin") != coin or not order.get("isTrigger"):
                continue
            if order.get("oid") is None:
                raise RuntimeError(f"trigger order missing oid: {order!r}")
            orders.append(order)
        return orders

    def cancel_orders(self, coin: str, order_ids: list[int]) -> None:
        for order_id in order_ids:
            self.ex.cancel(coin, order_id)

    @staticmethod
    def _resting_oid(response: dict) -> int:
        try:
            status = response["response"]["data"]["statuses"][0]
            resting = status["resting"]
            return int(resting["oid"])
        except (KeyError, IndexError, TypeError, ValueError) as exc:
            raise RuntimeError(f"trigger order was not confirmed resting: {response!r}") from exc

    def place_brackets(self, market: str, side: str, size: float,
                       stop_px: float, tp_px: float,
                       scale_out: Optional[ScaleOut] = None) -> list[dict]:
        """Stop at FULL size, take-profit either whole or split in two.

        The stop stays full-size on purpose: until the first tranche fills the
        whole position is still at risk, and a stop sized to the runner would
        leave the rest of it naked."""
        mi = self.market.info(market)
        close_is_buy = side == "short"
        legs = [("sl", stop_px, size)]
        if scale_out is not None:
            legs.append(("tp", scale_out.tp1_px, scale_out.tp1_size))
            legs.append(("tp", tp_px, scale_out.runner_size))
        else:
            legs.append(("tp", tp_px, size))
        placed = []
        try:
            for kind, price, leg_size in legs:
                response = self._trigger(
                    market, is_buy=close_is_buy, sz=leg_size, px=price,
                    tpsl=kind, szd=mi.sz_decimals,
                )
                placed.append({
                    "oid": self._resting_oid(response),
                    "kind": kind,
                    "trigger_px": format_price(price, mi.sz_decimals),
                    "size": leg_size,
                })
        except Exception:
            self.cancel_orders(market, [order["oid"] for order in placed])
            raise
        return placed

    def place_resting_entry(self, ap: Approved, size: float) -> dict:
        """Rest a maker limit entry with its stop and take-profit ATTACHED.

        One signed action with grouping="normalTpsl": the children arm at the
        venue the instant the parent fills, so a resting entry is never a
        naked position waiting for the next reconcile. Returns the parent oid.
        """
        coin, is_long = ap.market, ap.side == "long"
        mi = self.market.info(coin)
        szd = mi.sz_decimals
        if size <= 0:
            raise RuntimeError(f"size rounds to 0 for {coin} notional ${ap.notional:.2f}")
        self.ex.update_leverage(int(ap.leverage), coin, is_cross=ap.margin_mode == "cross")
        entry_px = format_price(ap.entry_px, szd)
        close_is_buy = not is_long
        orders = [{
            "coin": coin, "is_buy": is_long, "sz": size, "limit_px": entry_px,
            "order_type": {"limit": {"tif": "Gtc"}}, "reduce_only": False,
        }]
        child_legs = [("sl", ap.stop_px, size)]
        if ap.scale_out is not None:
            child_legs.append(("tp", ap.scale_out.tp1_px, ap.scale_out.tp1_size))
            child_legs.append(("tp", ap.tp_px, ap.scale_out.runner_size))
        else:
            child_legs.append(("tp", ap.tp_px, size))
        for kind, price, leg_size in child_legs:
            trigger_px = format_price(price, szd)
            orders.append({
                "coin": coin, "is_buy": close_is_buy, "sz": leg_size,
                "limit_px": tpsl_limit_price(trigger_px, close_is_buy=close_is_buy,
                                             sz_decimals=szd),
                "order_type": {"trigger": {"triggerPx": trigger_px, "isMarket": True,
                                           "tpsl": kind}},
                "reduce_only": True,
            })
        response = self.ex.bulk_orders(orders, self.builder, "normalTpsl")
        statuses = self._statuses(response)
        oid = self._any_oid(statuses[0])
        if oid is None:
            raise RuntimeError(f"resting entry was not accepted: {response!r}")
        return {"oid": oid, "entry_px": entry_px, "size": size,
                "children": [self._any_oid(st) for st in statuses[1:]]}

    @staticmethod
    def _statuses(response: dict) -> list[dict]:
        try:
            statuses = response["response"]["data"]["statuses"]
        except (KeyError, TypeError) as exc:
            raise RuntimeError(f"order response has no statuses: {response!r}") from exc
        if not isinstance(statuses, list) or not statuses:
            raise RuntimeError(f"order response has no statuses: {response!r}")
        for st in statuses:
            if isinstance(st, dict) and "error" in st:
                raise RuntimeError(f"order rejected: {st['error']}")
        return statuses

    @staticmethod
    def _any_oid(status) -> Optional[int]:
        if not isinstance(status, dict):
            return None
        for key in ("resting", "filled"):
            inner = status.get(key)
            if isinstance(inner, dict) and inner.get("oid") is not None:
                return int(inner["oid"])
        return None

    def open_entry(self, ap: Approved, mark: float) -> dict:
        coin, is_long = ap.market, ap.side == "long"
        self.ex.update_leverage(
            int(ap.leverage), coin, is_cross=ap.margin_mode == "cross"
        )
        # the guard already produced the exact venue lot; re-deriving it here was
        # a second rounding site that could disagree with what was approved, and
        # round() could land ABOVE the approved risk. There is no safe fallback:
        # an Approved without a lot is a bug upstream, so say so instead of guessing.
        size = ap.size
        if size <= 0:
            raise RuntimeError(
                f"{coin}: Approved carries no venue lot (size={ap.size!r}) — the guard "
                "sizes and floors every entry; refusing to re-derive it here")
        response = self.ex.market_open(
            coin, is_buy=is_long, sz=size, slippage=self.slippage, builder=self.builder
        )
        return {"entry_px": self._fill_px(response), "size": size}

    def open(self, ap: Approved, mark: float) -> dict:
        fill = self.open_entry(ap, mark)
        try:
            self.place_brackets(
                ap.market, ap.side, fill["size"], ap.stop_px, ap.tp_px,
                scale_out=ap.scale_out,
            )
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
        resp = self.ex.market_close(pos.market, builder=self.builder)
        return {"close_px": self._fill_px(resp)}

    def close(self, pos: Position, mark: float) -> dict:
        result = self.close_position_only(pos)
        self.cancel_brackets(pos.market)
        return result

    def adjust_stop(self, pos: Position, stop_px: float, tp_px: float,
                    scale_out: Optional[ScaleOut] = None) -> dict:
        """Place the complete replacement pair before removing old protection.

        `scale_out` must be passed whenever the position still carries an
        unfilled tranche, or replacing the brackets would quietly collapse the
        split back into one full-size target — and the trail arms at the same R
        the tranche banks at, so that race is the common case, not the corner."""
        old_orders = self.bracket_orders(pos.market)
        new_orders = self.place_brackets(
            pos.market, pos.side, pos.size, stop_px, tp_px, scale_out=scale_out
        )
        self.cancel_orders(pos.market, [order["oid"] for order in old_orders])
        return {
            "old_oids": [order["oid"] for order in old_orders],
            "new_orders": new_orders,
        }

    def open_orders_all(self) -> list[dict]:
        """Resting orders across native + builder dex (entries AND triggers)."""
        out = []
        for dex in ("", *self.dexes):
            body = {"type": "frontendOpenOrders", "user": self.account}
            if dex:
                body["dex"] = dex
            res = self.info.post("/info", body)
            if not isinstance(res, list):
                raise RuntimeError(f"{dex or 'native'} open orders response must be a list")
            out.extend(res)
        return out

    def fills(self) -> list[dict]:
        res = self.info.post("/info", {"type": "userFills", "user": self.account})
        if not isinstance(res, list):
            raise RuntimeError("fills response must be a list")
        return res
