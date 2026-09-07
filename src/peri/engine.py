"""The peri loop.

Every cycle (scheduled every cycle_secs, or woken immediately by a caller
message): reconcile venue state -> equity / day-roll / kill-switch -> assemble
the full context bundle -> ONE analyst decision -> risk-gate each action ->
execute -> record + notify. An analyst failure skips the cycle loudly; an
execution failure on one action never blocks the others."""

import asyncio
from concurrent.futures import ThreadPoolExecutor
import json
import math
import re
import secrets
import threading
import time
from datetime import datetime, timezone
from typing import Callable, Optional

from peri.analyst import Analyst, AnalystError
from peri.config import Config
from peri.hl_sizing import format_price
from peri.market import Market
from peri.models import (Action, AdjustStopAction, CloseAction, OpenAction,
                         RememberAction)
from peri.notifier import Notifier, fmt_close, fmt_open, fmt_refusal
from peri.risk import Approved, Guard, Refusal, isolated_liq_distance
from peri.router import FEE_RATE, Adapter, bracket_hit, realized_pnl
from peri.state import Position, State, dumps_actions
from pydantic import TypeAdapter

monotonic = time.monotonic
ACTION_ADAPTER = TypeAdapter(Action)
DASHBOARD_REFRESH_SECS = 5.0


ENTRY_FEE_GRACE_SECS = 900   # how long an opening fill waits for its ledger row
RECOVERY_WINDOW_SECS = 120   # before an unproven action is resolved from venue state
CLOSE_ECHO_SECS = 1800       # a venue fill this soon after a recorded close is its echo


def utc_day(ts: float) -> str:
    return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%d")


class ContextError(RuntimeError):
    pass


class ProposalError(RuntimeError):
    pass


def _running_loop():
    """The event loop running on THIS thread, or None. Used to decide whether a
    wake can touch the Event directly or must be marshalled onto the loop."""
    try:
        return asyncio.get_running_loop()
    except RuntimeError:
        return None


def reserved_order_markets(orders: list[dict]) -> frozenset[str]:
    markets = set()
    for order in orders:
        if not isinstance(order, dict):
            raise ContextError(f"malformed open order: {order!r}")
        coin = order.get("coin")
        if not isinstance(coin, str) or not coin:
            raise ContextError(f"open order missing market: {order!r}")
        if order.get("side") not in ("A", "B"):
            raise ContextError(f"open order side is malformed: {order!r}")
        reduce_only = order.get("reduceOnly")
        if not isinstance(reduce_only, bool):
            raise ContextError(f"open order reduceOnly is malformed: {order!r}")
        markets.add(coin)
    return frozenset(markets)


class Engine:
    def __init__(self, cfg: Config, state: State, market: Market, analyst: Analyst,
                 guard: Guard, adapter: Adapter, notifier: Notifier,
                 wake: asyncio.Event):
        self.cfg = cfg
        self.state = state
        self.market = market
        self.analyst = analyst
        self.guard = guard
        self.adapter = adapter
        self.notify = notifier
        self.wake = wake
        self._wake_trigger = "caller message"
        self._wake_lock = threading.Lock()
        self._day_halt_announced: dict[str, bool] = {}
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._venue_position_details: dict[str, dict] = {}
        self._venue_withdrawable_by_dex: dict[str, float] = {}
        self._account_snapshot: dict = {}
        self._recent_venue_history: tuple[list[dict], float] = ([], 0.0)
        self._recent_venue_fills: list[dict] = []
        self._adapter_lock = threading.RLock()
        self._trade_lock = threading.RLock()
        # Held ONLY across an action: the venue round trip and the ledger write
        # that records it. _trade_lock spans a whole cycle, analyst call
        # included, so it is useless for deciding whether it is safe to exit —
        # waiting on it timed out at 75s and earned a SIGKILL. This one is
        # seconds, and it is exactly the window where a kill orphans an order.
        self._execution_lock = threading.RLock()
        self._runtime_lock = threading.Lock()
        self._dashboard_snapshot_lock = threading.Lock()
        self._dashboard_cache: dict | None = None
        self._dashboard_last_attempt = -math.inf
        self._dashboard_stale_reason: str | None = None
        self._runtime = {
            "phase": "idle", "trigger": None, "started_ts": None,
            "snapshot_ts": None, "last_trigger": None,
            "last_started_ts": None, "last_finished_ts": None,
        }
        self._features_cache: dict[str, dict] = {}
        self._bias_snapshot: dict = {"cohorts": [], "assets": {}, "fetched_ts": None}
        self._degraded: set[str] = set()
        # injectable so the suite never touches the network
        from peri.trench import (fetch_cohort_bias, fetch_economic_calendar,
                                 fetch_many_asset_bias)
        self.fetch_cohort_bias = fetch_cohort_bias
        self.fetch_economic_calendar = fetch_economic_calendar
        self.fetch_many_asset_bias = fetch_many_asset_bias
        self._current_trigger: str = "startup"
        self._universe_heat: float = 0.0
        self._day_pnl_pct: float = 0.0

    def _runtime_update(self, **changes) -> None:
        with self._runtime_lock:
            self._runtime.update(changes)

    def runtime_status(self) -> dict:
        with self._runtime_lock:
            return dict(self._runtime)

    def realized_status(self) -> dict:
        if self.cfg.mode == "live":
            closes, total = self._recent_venue_history
            return {
                "total": total,
                "close_count": len(closes),
                "scope": "recent venue fills",
            }
        closes = self.state.recent_closes(200)
        return {
            "total": self.state.realized_total(),
            "close_count": len(closes),
            "scope": "Peri ledger",
        }

    @staticmethod
    def _order_role(order: dict) -> str:
        order_type = str(order.get("orderType") or "").lower()
        if order_type.startswith("stop"):
            return "stop_loss"
        if order_type.startswith("take profit"):
            return "take_profit"
        return "entry"

    @staticmethod
    def _order_number(value):
        if isinstance(value, bool):
            return None
        try:
            number = float(value)
        except (TypeError, ValueError):
            return None
        return number if math.isfinite(number) else None

    def _order_decision(self, order: dict, role: str, placed_ts: float | None):
        if placed_ts is None or role == "entry":
            return None
        trigger_px = self._order_number(order.get("triggerPx"))
        if trigger_px is None:
            return None
        action_field = "stop" if role == "stop_loss" else "take_profit"
        for decision in self.state.recent_decisions(200):
            decision_ts = float(decision["ts"])
            if placed_ts < decision_ts - 1 or placed_ts - decision_ts > 300:
                continue
            try:
                actions = json.loads(decision.get("actions_json") or "[]")
            except json.JSONDecodeError:
                continue
            for action in actions:
                if (action.get("kind") not in ("open", "adjust_stop")
                        or action.get("market") != order.get("coin")):
                    continue
                action_px = self._order_number(action.get(action_field))
                if action_px is not None and math.isclose(
                        action_px, trigger_px, rel_tol=1e-9, abs_tol=1e-8):
                    return decision
        return None

    def _venue_close_history(self, fills: list[dict], *, require_timestamps: bool = True
                             ) -> tuple[list[dict], float]:
        # HL reports closedPnl GROSS and charges the entry fee on the opening
        # fill, so a close priced at (closedPnl - its own fee) overstates every
        # trade by the entry side. Attribute the window's opening fees per unit
        # of size: exact in aggregate, and exact per row whenever a position is
        # opened and closed in one clip (which is how peri trades).
        open_fee, open_size = {}, {}
        for fill in fills:
            if not isinstance(fill, dict):
                continue
            direction = fill.get("dir")
            if isinstance(direction, str) and direction.startswith("Close"):
                continue
            coin = fill.get("coin")
            fee = self._order_number(fill.get("fee"))
            size = self._order_number(fill.get("sz"))
            if not isinstance(coin, str) or not coin or fee is None or not size:
                continue
            open_fee[coin] = open_fee.get(coin, 0.0) + fee
            open_size[coin] = open_size.get(coin, 0.0) + size
        entry_fee_per_unit = {
            coin: open_fee[coin] / open_size[coin]
            for coin in open_fee if open_size.get(coin)
        }
        closes = []
        for fill in fills:
            if not isinstance(fill, dict):
                raise ContextError(f"fill is malformed: {fill!r}")
            direction = fill.get("dir")
            if not isinstance(direction, str) or not direction.startswith("Close"):
                continue
            if direction.endswith("Long"):
                side = "long"
            elif direction.endswith("Short"):
                side = "short"
            else:
                raise ContextError(f"close direction is malformed: {fill!r}")
            tid = str(fill.get("tid") or fill.get("hash") or "")
            market = fill.get("coin")
            size = self._order_number(fill.get("sz"))
            close_px = self._order_number(fill.get("px"))
            gross_pnl = self._order_number(fill.get("closedPnl"))
            fee = self._order_number(fill.get("fee"))
            timestamp_ms = self._order_number(fill.get("time"))
            if timestamp_ms is None and not require_timestamps:
                continue
            if (not tid or not isinstance(market, str) or not market
                    or size is None or size <= 0 or close_px is None or close_px <= 0
                    or gross_pnl is None or fee is None
                    or timestamp_ms is None or timestamp_ms <= 0):
                raise ContextError(f"close fill is malformed: {fill!r}")
            entry_px = (close_px - gross_pnl / size if side == "long"
                        else close_px + gross_pnl / size)
            if not math.isfinite(entry_px) or entry_px <= 0:
                raise ContextError(f"close fill implies an invalid entry: {fill!r}")
            entry_fee = size * entry_fee_per_unit.get(market, 0.0)
            closes.append({
                "id": tid,
                "market": market,
                "side": side,
                "size": size,
                "entry_px": entry_px,
                "close_px": close_px,
                "gross_pnl": gross_pnl,
                "fee": fee + entry_fee,
                "entry_fee": entry_fee,
                "exit_fee": fee,
                "realized_pnl": gross_pnl - fee - entry_fee,
                "close_reason": "venue",
                "conviction": None,
                "closed_ts": timestamp_ms / 1000,
            })
        closes.sort(key=lambda close: close["closed_ts"], reverse=True)
        return closes, sum(close["realized_pnl"] for close in closes)

    def dashboard_snapshot(self) -> dict:
        """Coalesce venue reads and preserve the last good dashboard snapshot.

        This cache is dashboard-only. Analyst and confirmation context always use
        ``context_snapshot`` and therefore still fail closed on any upstream gap.
        """
        with self._dashboard_snapshot_lock:
            now = monotonic()
            if (
                self._dashboard_cache is not None
                and now - self._dashboard_last_attempt < DASHBOARD_REFRESH_SECS
            ):
                return self._dashboard_snapshot_view()
            self._dashboard_last_attempt = now
            try:
                snapshot = self._fresh_dashboard_snapshot()
            except Exception as exc:
                if self._dashboard_cache is None:
                    raise
                self._dashboard_stale_reason = (
                    f"{type(exc).__name__}: {exc}"
                )[:500]
                return self._dashboard_snapshot_view()
            self._dashboard_cache = snapshot
            self._dashboard_stale_reason = None
            return self._dashboard_snapshot_view()

    def _dashboard_snapshot_view(self) -> dict:
        if self._dashboard_cache is None:
            raise RuntimeError("dashboard snapshot cache is empty")
        return {
            **self._dashboard_cache,
            "stale": self._dashboard_stale_reason is not None,
            "stale_reason": self._dashboard_stale_reason,
            "runtime": self.runtime_status(),
        }

    def _fresh_dashboard_snapshot(self) -> dict:
        """Read current venue/account state and attach explicit order provenance."""
        with self._adapter_lock:
            account = dict(self.adapter.account_snapshot())
            orders = self.adapter.open_orders_all()
            fills = self.adapter.fills() if self.cfg.mode == "live" else []
        venue_positions = account.pop("positions", [])
        positions = self.state.open_positions()
        by_market = {position.market: position for position in positions}
        live_positions = []
        for venue_position in venue_positions:
            position = by_market.get(venue_position.get("market"))
            live_positions.append({
                **venue_position,
                "id": position.id if position else None,
                "source": position.source if position else "external",
                "stop_px": position.stop_px if position else None,
                "tp_px": position.tp_px if position else None,
                "conviction": position.conviction if position else None,
                "rationale": position.rationale if position else "untracked venue position",
                "invalidation": position.invalidation if position else None,
                "opened_ts": position.opened_ts if position else None,
            })
        normalized = []
        for order in orders:
            role = self._order_role(order)
            timestamp_ms = self._order_number(order.get("timestamp"))
            placed_ts = timestamp_ms / 1000 if timestamp_ms is not None else None
            decision = self._order_decision(order, role, placed_ts)
            position = by_market.get(order.get("coin"))
            normalized.append({
                "oid": order.get("oid"),
                "market": order.get("coin"),
                "role": role,
                "side": "buy" if order.get("side") == "B" else "sell",
                "size": self._order_number(order.get("sz")),
                "limit_px": self._order_number(order.get("limitPx")),
                "trigger_px": self._order_number(order.get("triggerPx")),
                "reduce_only": order.get("reduceOnly") is True,
                "placed_ts": placed_ts,
                "position_source": position.source if position else None,
                "placed_by": "Qwen via Peri" if decision else "External / manual",
                "route": "Trench" if decision else "Venue",
                "decision_id": decision["id"] if decision else None,
                "attribution": "exact decision match" if decision else "unattributed",
            })
        if self.cfg.mode == "live":
            closes, realized_total = self._venue_close_history(fills)
            self._recent_venue_history = (closes, realized_total)
            realized_scope = "recent venue fills"
        else:
            closes = self.state.recent_closes(200)
            realized_total = self.state.realized_total()
            realized_scope = "Peri ledger"
        return {
            "as_of_ts": time.time(),
            "account": account,
            "positions": live_positions,
            "orders": normalized,
            "realized": {
                "total": realized_total,
                "close_count": len(closes),
                "scope": realized_scope,
            },
            "closes": closes,
            "runtime": self.runtime_status(),
        }

    def bias_snapshot(self) -> dict:
        """The last good Trench read, for the dashboard. Never re-fetches: the
        cycle owns the network, a UI poll must not."""
        return dict(self._bias_snapshot)

    def request_wake(self, trigger: str = "manual dashboard") -> bool:
        """Queue one immediate cycle; return whether a wake was already pending.

        Callable from ANY thread. The price watcher and the telegram control
        both poll inside `asyncio.to_thread`, so they reach this from a worker
        thread — and `asyncio.Event.set()` is not thread-safe: it resolves the
        waiter's future via `loop.call_soon` without waking the loop's selector,
        so the wake was only noticed at the loop's next scheduled wakeup. That
        silently defeated the entire point of price-triggered wakes, which exist
        to decide at the START of a move.
        """
        with self._wake_lock:
            already_pending = self.wake.is_set()
            if not already_pending:
                self._wake_trigger = trigger
        loop = self._loop
        if loop is not None and loop is not _running_loop():
            loop.call_soon_threadsafe(self.wake.set)
        else:
            self.wake.set()
        return already_pending

    # -- async shell -------------------------------------------------------
    async def run(self) -> None:
        self._loop = asyncio.get_running_loop()
        self.notify.send(f"peri up · mode={self.cfg.mode} · net={self.cfg.hl_network} "
                         f"· cycle={self.cfg.analyst.cycle_secs}s")
        await asyncio.to_thread(self.startup)
        first_cycle = True
        next_scheduled = monotonic()
        while True:
            if first_cycle:
                trigger = "startup"
                first_cycle = False
            else:
                trigger = "scheduled"
                timeout = max(0.0, next_scheduled - monotonic())
                try:
                    await asyncio.wait_for(self.wake.wait(), timeout=timeout)
                    with self._wake_lock:
                        trigger = self._wake_trigger
                        self._wake_trigger = "caller message"
                        self.wake.clear()
                except asyncio.TimeoutError:
                    pass
            try:
                await asyncio.to_thread(self.cycle, trigger)
            except Exception as e:  # noqa: BLE001 — one bad cycle never kills the daemon
                self.notify.send(f"cycle error ({trigger}): {e!r}")
            # Schedule the NEXT cycle from the moment this one finished, whatever
            # woke it. Computing it before the body meant a cycle slower than the
            # cadence (routine at cycle_secs_active=300 against a 180-225s
            # analyst) left next_scheduled already in the past, so timeout was 0
            # and the daemon ran LLM decisions back to back with no idle gap.
            # Deferring on wake-triggered cycles too stops a wake at t+299s from
            # being followed by the scheduled cycle one second later.
            next_scheduled = monotonic() + self.next_cycle_secs()

    def next_cycle_secs(self) -> int:
        """Adaptive cadence: a quiet tape does not need a decision every 15
        minutes, and a moving one cannot wait that long. Heat is the median
        candidate ATR15m% from the last bundle."""
        a = self.cfg.analyst
        if a.quiet_secs() == a.active_secs():
            return a.cycle_secs
        return a.active_secs() if self._universe_heat >= a.heat_atr_pct else a.quiet_secs()

    def resolve_execution(self, execution_id: str, reason: str = "operator") -> dict:
        """Force one stuck action journal terminal.

        A single non-terminal row refuses EVERY new entry, so an operator who
        has checked the venue needs a way to clear it without editing sqlite."""
        rows = [row for row in self.state.unfinished_action_executions()
                if row["id"] == execution_id]
        if not rows:
            raise ProposalError(f"no unfinished execution {execution_id}")
        result = self._mark_execution_problem(
            execution_id, rows[0].get("proposal_id"), "cancelled",
            f"resolved by operator: {reason}")
        self.notify.send(f"execution {execution_id} resolved by operator — "
                         "entries unblocked")
        return result

    def request_close(self, market: str, reason: str) -> dict:
        """Close a position (or cancel a resting entry) on operator command.

        Takes the trade lock and refreshes context first, exactly like a
        confirmed chat trade — an operator closing from a phone must not race a
        cycle. It can only ever REDUCE exposure; there is no operator open."""
        action = CloseAction(market=market, rationale=reason)
        with self._trade_lock:
            bundle, marks, equity, day, reserved = self.context_snapshot(
                "operator close", target_query="")
            decision_id = self.state.record_decision(
                "operator close", f"operator closed {market}: {reason}",
                dumps_actions([action]), "operator", 0, "ok",
                reasoning="requested from telegram/dashboard, not by the analyst")
            self.exec_close(action, marks, equity, day, reserved,
                            origin="operator", decision_id=decision_id)
        return {"market": market, "reason": reason}

    def cancel_resting_entries(self, why: str) -> list[str]:
        """Withdraw every unfilled maker entry. Returns the markets cleared.

        A resting order can sit for entry_expiry_secs (2h) and fill LONG after
        the conditions that justified it stopped holding — after the kill switch
        tripped, or after the day-loss halt engaged. The fill path adopts it
        unconditionally, so the halt that is supposed to stop the bleeding did
        not reach the orders already on the book. Operator pause has always
        cleared them; the automatic halts must do the same.
        """
        cleared = []
        for entry in self.state.resting_entries():
            oid = self._order_number(entry.get("oid"))
            try:
                if oid is not None:
                    with self._adapter_lock:
                        self.adapter.cancel_orders(entry["market"], [int(oid)])
                        self._clear_orphan_brackets(entry["market"])
                self.state.settle_pending_entry(entry["id"], "cancelled")
                cleared.append(entry["market"])
            except Exception as exc:  # noqa: BLE001 — report, never silently leave it
                self.notify.send(
                    f"{why}: could not cancel resting entry on "
                    f"{entry['market']}: {exc!r}")
        return cleared

    def set_paused(self, paused: bool, who: str = "dashboard") -> dict:
        """Operator pause. Halts NEW entries (gate 2) and cancels anything
        resting; open positions keep their venue brackets and the analyst keeps
        managing them."""
        was = self.state.paused()
        self.state.set_paused(paused, who)
        cancelled = []
        if paused and not was:
            cancelled = self.cancel_resting_entries("pause")
        if paused != was:
            self.notify.send(
                f"{'PAUSED' if paused else 'RESUMED'} by {who}"
                + (f" · cancelled resting entries: {', '.join(cancelled)}" if cancelled else "")
                + (" · open positions keep their brackets" if paused else ""))
        return {"paused": paused, "changed": paused != was,
                "cancelled_entries": cancelled, **self.state.pause_state()}

    # -- startup -----------------------------------------------------------
    def startup(self) -> None:
        """Live boot: seed fill history exactly once, then reconcile every restart.

        The durable initialization marker distinguishes the first attachment to
        an account (all fills are historical) from later boots, where an unseen
        fill may have closed a ledger-open position while the daemon was down.
        """
        if self.cfg.mode != "live":
            return
        with self._adapter_lock:
            if self.state.fill_history_initialized():
                self.reconcile_live()
            else:
                seeded = 0
                for f in self.adapter.fills():
                    tid = str(f.get("tid") or f.get("hash") or "")
                    if tid and not self.state.fill_seen(tid):
                        self.state.mark_fill(tid)
                        seeded += 1
                self.state.mark_fill_history_initialized()
                if seeded:
                    self.notify.send(
                        f"seeded {seeded} historical fills (future boots reconcile new fills)"
                    )
            adopted = self.adopt_external()
        for market_name in adopted:
            self.notify.send(f"adopted external position: {market_name}")

    def adopt_external(self) -> list[str]:
        snapshots: dict[str, dict] = {}
        details: dict[str, dict] = {}
        withdrawable_by_dex: dict[str, float] = {}

        def optional_number(payload: dict, field: str, *, nonnegative: bool = False):
            value = payload.get(field)
            if value is None or value == "":
                return None
            try:
                number = float(value)
            except (TypeError, ValueError) as e:
                raise ContextError(f"position {field} is malformed: {payload!r}") from e
            if not math.isfinite(number) or (nonnegative and number < 0):
                raise ContextError(f"position {field} is malformed: {payload!r}")
            return number

        for dex in ("", *self.cfg.universe.dexes):
            body = {"type": "clearinghouseState", "user": self.adapter.account}
            if dex:
                body["dex"] = dex
            st = self.adapter.info.post("/info", body)
            asset_positions = st.get("assetPositions") if isinstance(st, dict) else None
            if not isinstance(asset_positions, list):
                raise ContextError(
                    f"{dex or 'native'} positions response is malformed")
            withdrawable = optional_number(st, "withdrawable", nonnegative=True)
            if withdrawable is not None:
                withdrawable_by_dex[dex or "native"] = withdrawable
            for ap in asset_positions:
                p = ap.get("position") if isinstance(ap, dict) else None
                if not isinstance(p, dict):
                    raise ContextError(f"{dex or 'native'} position is malformed: {ap!r}")
                try:
                    szi = float(p.get("szi", 0) or 0)
                except (TypeError, ValueError) as e:
                    raise ContextError(
                        f"{dex or 'native'} position size is malformed: {p!r}") from e
                if not math.isfinite(szi):
                    raise ContextError(
                        f"{dex or 'native'} position size is malformed: {p!r}")
                if szi == 0:
                    continue
                name = p.get("coin")
                try:
                    entry = float(p.get("entryPx") or 0)
                    leverage_data = p.get("leverage") or {}
                    lev = float(leverage_data.get("value") or 0)
                except (AttributeError, TypeError, ValueError) as e:
                    raise ContextError(
                        f"{dex or 'native'} position values are malformed: {p!r}") from e
                if (not isinstance(name, str) or not name
                        or not math.isfinite(entry) or entry <= 0
                        or not math.isfinite(lev) or lev <= 0):
                    raise ContextError(
                        f"{dex or 'native'} position values are malformed: {p!r}")
                if name in snapshots:
                    raise ContextError(f"duplicate venue position: {name}")
                side = "long" if szi > 0 else "short"
                size = abs(szi)
                notional = size * entry
                margin_mode = leverage_data.get("type")
                if margin_mode not in {"cross", "isolated"}:
                    margin_mode = "unknown"
                snapshots[name] = {
                    "side": side, "entry": entry, "size": size,
                    "notional": notional, "leverage": lev,
                    "margin_mode": margin_mode,
                }
                details[name] = {
                    "position_value": optional_number(p, "positionValue", nonnegative=True),
                    "margin_used": optional_number(p, "marginUsed", nonnegative=True),
                    "unrealized_pnl": optional_number(p, "unrealizedPnl"),
                    "roe": optional_number(p, "returnOnEquity"),
                    "liquidation_px": optional_number(p, "liquidationPx", nonnegative=True),
                }

        adopted = []
        for name, snapshot in snapshots.items():
            side = snapshot["side"]
            entry = snapshot["entry"]
            size = snapshot["size"]
            notional = snapshot["notional"]
            lev = snapshot["leverage"]
            margin_mode = snapshot["margin_mode"]
            existing = self.state.open_position_for(name)
            if existing:
                changed = (
                    existing.side != side
                    or not math.isclose(existing.entry_px, entry)
                    or not math.isclose(existing.size, size)
                    or not math.isclose(existing.notional, notional)
                    or not math.isclose(existing.leverage, lev)
                    or existing.margin_mode != margin_mode
                )
                if changed:
                    self.state.sync_position_snapshot(
                        existing.id, side, entry, size, notional, lev, margin_mode)
                    self.notify.send(f"synced venue position: {name}")
                continue
            self.state.add_position(
                market=name, side=side,
                entry_px=entry, size=size, notional=notional,
                leverage=lev, stop_px=None, tp_px=None, conviction=None,
                source="external", rationale="adopted from venue",
                invalidation="unknown — set by analyst",
                margin_mode=margin_mode)
            adopted.append(name)
        venue_markets = set(snapshots)
        for existing in self.state.open_positions():
            if existing.market in venue_markets:
                self.state.clear_position_absent(existing.id)
                continue
            # 'missing' is terminal and hides the row from the close fill that is
            # usually moments behind it, discarding the realized PnL. Require two
            # consecutive absences before believing it.
            absences = self.state.note_position_absent(existing.id)
            if absences < 2:
                self.notify.send(
                    f"{existing.market} absent from the venue snapshot — waiting "
                    "one more read before writing it off")
                continue
            self.state.mark_position_missing(existing.id)
            self.state.clear_position_absent(existing.id)
            self.notify.send(f"position missing at venue: {existing.market}")
        self._venue_position_details = details
        self._venue_withdrawable_by_dex = withdrawable_by_dex
        return adopted

    # -- the cycle ---------------------------------------------------------
    def cycle(self, trigger: str) -> None:
        started_ts = time.time()
        self._runtime_update(
            phase="context", trigger=trigger, started_ts=started_ts,
            snapshot_ts=None, last_trigger=trigger, last_started_ts=started_ts,
        )
        timings: dict[str, float] = {}
        try:
            with self._trade_lock:
                self._cycle(trigger, timings)
        finally:
            timings["total"] = round(time.time() - started_ts, 1)
            self._runtime_update(
                phase="idle", trigger=None, started_ts=None, snapshot_ts=None,
                last_finished_ts=time.time(), last_timings=timings,
            )
            print(f"[peri] cycle {trigger} · "
                  + " · ".join(f"{k} {v:.1f}s" for k, v in timings.items()), flush=True)

    def _cycle(self, trigger: str, timings: dict[str, float]) -> None:
        self._current_trigger = trigger
        try:
            bundle, marks, equity, day, reserved_markets = self.context_snapshot(
                trigger, timings=timings)
        except Exception as e:  # noqa: BLE001 — no partial upstream context reaches analyst
            reason = str(e) if isinstance(e, ContextError) else (
                f"upstream context unavailable: {type(e).__name__}: {e}")
            self.state.record_decision(trigger, "", "[]", self.analyst.model, 0, "error",
                                       reasoning=reason)
            self.notify.send(f"context DOWN — cycle skipped: {reason}")
            return

        self._runtime_update(phase="analyst", snapshot_ts=time.time())
        t0 = time.time()
        try:
            res = self.analyst.decide(bundle)
        except AnalystError as e:
            timings["analyst"] = round(time.time() - t0, 1)
            self.state.record_decision(trigger, "", "[]", self.analyst.model, 0, "error",
                                       reasoning=str(e))
            self.notify.send(f"analyst DOWN — cycle skipped: {e}")
            return
        timings["analyst"] = round(time.time() - t0, 1)

        d = res.decision
        decision_id = self.state.record_decision(
            trigger, d.market_view, dumps_actions(d.actions),
            res.model or self.analyst.model, res.latency_ms, "ok",
            reasoning=res.reasoning, prompt=res.prompt,
            tool_log=json.dumps(res.tool_log),
        )
        for t in res.tool_log:
            self.notify.send(f"searched [{t['tool']}]: {t['args'].get('query', '')}")
        if d.market_view:
            self.notify.send(f"view: {d.market_view}")
        if not d.actions:
            return

        self._runtime_update(phase="executing")
        t0 = time.time()
        decided_marks = marks
        if any(isinstance(a, OpenAction) for a in d.actions):
            # The analyst has been thinking for minutes. Nothing below may be
            # gated against the marks it started from.
            try:
                marks, equity, reserved_markets = self.refresh_execution_context()
            except Exception as e:  # noqa: BLE001 — fail closed, never act on stale prices
                reason = f"execution context unavailable: {type(e).__name__}: {e}"
                for action in d.actions:
                    self.state.record_refusal(getattr(action, "market", None),
                                              action.model_dump_json(), reason)
                self.notify.send(f"execution context DOWN — actions skipped: {reason}")
                timings["execute"] = round(time.time() - t0, 1)
                return
        for action in d.actions:
            try:
                if isinstance(action, OpenAction) and not self._entry_still_valid(
                        action, decided_marks, marks):
                    continue
                self.execute(
                    action, marks, equity, day, reserved_markets,
                    origin="autonomous", decision_id=decision_id,
                )
            except Exception as e:  # noqa: BLE001 — isolate action failures
                self.state.record_refusal(getattr(action, "market", None),
                                          action.model_dump_json(),
                                          f"execution error: {e!r}")
                self.notify.send(f"execution error on {getattr(action, 'market', '?')}: {e!r}")
            else:
                # Each action changes the book the NEXT one is judged against.
                # Reusing one pre-decision snapshot let two opens in the same
                # decision both see an empty reserve and the same available
                # margin, overrunning max_concurrent and double-committing margin.
                if isinstance(action, OpenAction):
                    reserved_markets = reserved_markets | {action.market}
                    try:
                        equity = float(self._validated_snapshot(marks)["equity"])
                    except Exception as e:  # noqa: BLE001 — keep the last good equity
                        self.notify.send(f"post-action account re-read failed: {e!r}")
        timings["execute"] = round(time.time() - t0, 1)

    @staticmethod
    def resolve_market_mentions(message: str, market_names) -> tuple[str, ...]:
        names = list(market_names)
        exact = {name.upper(): name for name in names}
        by_ticker: dict[str, list[str]] = {}
        for name in names:
            by_ticker.setdefault(name.split(":", 1)[-1].upper(), []).append(name)

        resolved = []
        for raw in re.findall(r"\$?[A-Za-z][A-Za-z0-9.:-]*", message):
            explicit = raw.startswith("$") or ":" in raw
            token = raw.removeprefix("$").upper()
            # BE, ME, NOT, NEAR, MOVE, S, W, GOLD, PEOPLE and TRUMP are all live
            # HL tickers. Matching bare lowercase words pulled junk markets into
            # the analyst's candidate list from ordinary English ("it's not
            # moving near my level" -> S, NOT, NEAR), so a bare word must be
            # written the way a trader writes a ticker: $SOL, xyz:NVDA, or CAPS.
            if not explicit and raw != token:
                continue
            match = exact.get(token)
            if match is not None:
                if match not in resolved:
                    resolved.append(match)
                continue
            ticker_matches = by_ticker.get(token, [])
            if len(ticker_matches) == 1:
                if ticker_matches[0] not in resolved:
                    resolved.append(ticker_matches[0])
            elif len(ticker_matches) > 1:
                raise ContextError(
                    f"ambiguous market {raw!r}: {', '.join(sorted(ticker_matches))}"
                )
            elif explicit:
                raise ContextError(f"unknown market {raw.removeprefix('$')!r}")
        return tuple(resolved)

    def _entry_still_valid(self, action: OpenAction, decided: dict,
                           fresh: dict) -> bool:
        """Did the market run away while the analyst was thinking?

        Re-gating against fresh marks already handles a RESTING entry: its level
        is an explicit price, and the guard refuses a limit that has ended up on
        the wrong side of the mark. A MARKET order is different — its entire
        thesis was priced at the mark the analyst saw, and taking it minutes
        later at a materially different price is precisely the chase the
        range-edge rail exists to prevent. Refuse it and say why, so the
        refusal reaches the next prompt instead of silently becoming a fill.
        """
        tolerance = self.cfg.risk.max_mark_drift_pct / 100.0
        if action.entry is not None or tolerance <= 0:
            return True
        before, now = decided.get(action.market), fresh.get(action.market)
        if not isinstance(before, (int, float)) or before <= 0:
            return True
        if not isinstance(now, (int, float)) or now <= 0:
            self.refuse(action, f"no live mark for {action.market} at execution")
            return False
        drift = self._relative_change(now, before)
        if drift <= tolerance:
            return True
        self.refuse(
            action,
            f"{action.market} moved {drift * 100:.2f}% ({before:g} -> {now:g}) while "
            f"the decision was being made, past the {self.cfg.risk.max_mark_drift_pct:g}% "
            "limit — a market order priced at the old mark is a chase by the time it "
            "lands. Rest a limit at the level you actually want instead")
        self.notify.send(
            f"stale entry refused: {action.market} {action.side} moved "
            f"{drift * 100:.2f}% during the decision")
        return False

    def _validated_snapshot(self, marks: dict) -> dict:
        """Fetch, validate, and only then publish. Nothing downstream may
        ever read a snapshot that failed its own checks."""
        with self._adapter_lock:
            snap = self.adapter.account_snapshot(marks)
        if not isinstance(snap, dict):
            raise ContextError(f"account snapshot is invalid: {snap!r}")
        value = snap.get("equity")
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or value < 0):
            raise ContextError(f"account equity is invalid: {value!r}")
        for field in ("available_margin", "held_collateral", "total_margin_used",
                      "spot_usdc_total"):
            value = snap.get(field)
            if (isinstance(value, bool) or not isinstance(value, (int, float))
                    or not math.isfinite(value) or value < 0):
                raise ContextError(f"account {field} is invalid: {value!r}")
        if not isinstance(snap.get("abstraction"), str):
            raise ContextError(
                f"account abstraction is invalid: {snap.get('abstraction')!r}")
        self._account_snapshot = snap
        return snap

    def refresh_execution_context(self) -> tuple[dict, float, frozenset[str]]:
        """Re-read the venue between DECIDING and ACTING.

        The analyst call takes 180-225s against a 420s deadline, and the cycle is
        very often triggered BECAUSE a market just moved 1%. Executing against
        the marks the bundle was built from meant every price-dependent rail —
        range-edge, the resting-entry side check, stop-distance sizing, the
        margin fit, the isolated liquidation band — judged a price minutes old.
        A resting long placed under a stale mark can already sit ABOVE the live
        one, filling as the taker chase the whole design exists to avoid.

        The chat path has always revalidated like this before acting on a
        proposal (`confirm_trade`); the autonomous path never did. This is the
        cheap half of `context_snapshot` — marks, account, orders — with no
        candles, news or Trench calls, so it costs a few HTTP requests.
        """
        ctxs = self.market.ctxs()
        marks = {n: c.mark for n, c in ctxs.items()}
        snap = self._validated_snapshot(marks)
        with self._adapter_lock:
            orders = self.adapter.open_orders_all()
        return marks, float(snap["equity"]), reserved_order_markets(orders)

    def context_snapshot(self, trigger: str, *,
                         target_query: str = "",
                         timings: dict[str, float] | None = None) -> tuple[
                             dict, dict, float, str, frozenset[str]
                         ]:
        """Fetch and validate the one upstream snapshot used by analyst and guard.
        `timings` (when given) receives wall seconds per upstream phase."""
        now = time.time()
        day = utc_day(now)
        t0 = time.time()

        def _phase(name: str) -> None:
            nonlocal t0
            if timings is not None:
                timings[name] = round(time.time() - t0, 1)
            t0 = time.time()

        ctxs = self.market.ctxs()
        marks = {n: c.mark for n, c in ctxs.items()}
        forced_markets = self.resolve_market_mentions(target_query, ctxs)
        _phase("ctxs")

        self.reconcile(marks)
        _phase("reconcile")

        def _snapshot() -> dict:
            return self._validated_snapshot(marks)

        account_snapshot = _snapshot()
        # manage_positions sizes nothing, but it CAN close (the time stop), and
        # it used to run BEFORE this refresh — reading an equity one cycle old,
        # or 0.0 on the first cycle after a restart. Give it the fresh snapshot,
        # then re-read if it changed the book, so nothing downstream sees a
        # position the engine has just closed.
        if self.manage_positions(marks):
            account_snapshot = _snapshot()
        equity = account_snapshot["equity"]
        _phase("account")
        if self.state.day_row(day) is None:
            self.state.open_day(day, equity)
            self.notify.send(f"day roll {day} · open equity ${equity:.2f}")
        day_open = self.state.day_row(day)["open_equity"]
        day_pnl = equity - day_open
        self._day_pnl_pct = (day_pnl / day_open * 100.0) if day_open > 0 else 0.0

        if (not self.state.kill_tripped(day)
                and day_open > 0
                and equity <= day_open * (1 - self.cfg.risk.kill_switch_pct / 100)):
            self.state.trip_kill(day)
            # Orders already on the book are entries too: left resting they fill
            # straight through the halt that just tripped.
            withdrawn = self.cancel_resting_entries("kill switch")
            self.notify.send(f"KILL SWITCH: equity ${equity:.2f} is "
                             f"{self.cfg.risk.kill_switch_pct:.0f}% below day open "
                             f"${day_open:.2f} — entries halted until 00:00 UTC"
                             + (f" · withdrew resting: {', '.join(withdrawn)}"
                                if withdrawn else ""))
        halt = self.cfg.risk.day_loss_halt_pct
        if (halt > 0 and self._day_pnl_pct <= -halt
                and not self._day_halt_announced.get(day)):
            self._day_halt_announced = {day: True}
            withdrawn = self.cancel_resting_entries("day-loss halt")
            self.notify.send(
                f"DAY-LOSS HALT: down {self._day_pnl_pct:.1f}% on the day, past the "
                f"-{halt:.0f}% soft limit — no new entries"
                + (f" · withdrew resting: {', '.join(withdrawn)}" if withdrawn else ""))

        try:
            with self._adapter_lock:
                orders = self.adapter.open_orders_all()
        except Exception as e:  # noqa: BLE001 — external context failure must fail closed
            raise ContextError(f"open orders unavailable: {e}") from e
        if self.reconcile_pending_entries(marks, orders):
            try:
                with self._adapter_lock:
                    orders = self.adapter.open_orders_all()
            except Exception as e:  # noqa: BLE001 — external context failure must fail closed
                raise ContextError(f"open orders unavailable after entry settle: {e}") from e
        reserved_markets = reserved_order_markets(orders)
        if self.cfg.mode == "live":
            self.sync_live_brackets(orders)
        _phase("orders")
        bundle = self.build_bundle(
            trigger, ctxs, marks, equity, day, day_pnl, orders,
            forced_markets=forced_markets,
        )
        _phase("bundle")
        bundle["context_ts"] = time.time()
        return bundle, marks, equity, day, reserved_markets

    # -- bundle ------------------------------------------------------------
    def build_bundle(self, trigger: str, ctxs, marks: dict, equity: float,
                     day: str, day_pnl: float, orders: list[dict],
                     forced_markets: tuple[str, ...] = ()) -> dict:
        open_positions = self.state.open_positions()
        now_ts = time.time()
        telegram = self.state.recent_tg(20)
        caller_tokens = set()
        for message in telegram:
            if (message["is_caller"]
                    and now_ts - message["ts"] <= self.cfg.risk.stale_call_secs):
                caller_tokens.update(re.findall(r"[A-Z0-9.]+", message["text"].upper()))
        caller_markets = []
        for name in ctxs:
            ticker = name.split(":", 1)[-1].upper()
            if ticker in caller_tokens:
                caller_markets.append(name)

        positions = []
        for p in open_positions:
            mark = marks.get(p.market)
            if mark is None or not math.isfinite(mark) or mark <= 0:
                raise ContextError(
                    f"mark unavailable for open position {p.market}: {mark!r}")
            d = mark - p.entry_px if p.side == "long" else p.entry_px - mark
            venue = self._venue_position_details.get(p.market, {})
            upnl = venue.get("unrealized_pnl")
            if upnl is None:
                upnl = d * p.size
            margin = venue.get("margin_used")
            position_value = venue.get("position_value")
            if self.cfg.mode != "live":
                margin = p.notional / p.leverage
                position_value = mark * p.size
            positions.append({"market": p.market, "side": p.side, "size": p.size,
                              "entry_px": p.entry_px, "mark": mark, "upnl": upnl,
                              "notional": p.notional, "leverage": p.leverage,
                              "margin_mode": p.margin_mode,
                              "margin": margin, "position_value": position_value,
                              "liquidation_px": venue.get("liquidation_px"),
                              "roe": venue.get("roe"),
                              "stop_px": p.stop_px, "tp_px": p.tp_px,
                              "opened_ts": p.opened_ts, "source": p.source,
                              "rationale": p.rationale or "—",
                              "invalidation": p.invalidation or "—"})

        must_include = list(dict.fromkeys(
            [p["market"] for p in positions] + caller_markets + list(forced_markets)))
        names = self.market.candidates(
            self.cfg.universe.native_allow, self.cfg.universe.dex_volume_floor,
            self.cfg.universe.top_movers, must_include)
        # Hide markets whose smallest venue lot costs more than the largest
        # notional this equity can carry: offering them wastes a cycle on a
        # "size rounds to zero" refusal. Anything held or explicitly named
        # stays, so the analyst can always manage what it already owns.
        max_notional = max(
            equity * self.cfg.risk.risk_pct / 100.0 / 0.005,  # a 0.5% stop
            self.cfg.risk.min_notional)
        keep = set(must_include)
        unaffordable = [
            n for n in names
            if n not in keep and ctxs.get(n) is not None
            and not self.market.affordable(n, ctxs[n].mark, max_notional)
        ]
        if unaffordable:
            names = [n for n in names if n not in set(unaffordable)]
        present = [(n, ctxs[n]) for n in names if ctxs.get(n) is not None]

        def _features(item):
            n, c = item
            try:
                return self.market.features(n, c)
            except Exception as exc:  # noqa: BLE001 — one dead candle feed ≠ no cycle
                # Degraded features used to be returned here AND cached, which
                # silently switched off the range-edge and ATR gates for that
                # market — fail-open on exactly the gates written to stop the
                # 2026-08-28 losses. The name is now marked unusable instead.
                return {"mark": c.mark, "day_pct": c.day_pct,
                        "funding_apr_pct": c.funding_apr_pct,
                        "oi_usd": c.oi_usd, "vol_usd": c.vol_usd,
                        "unavailable": f"{type(exc).__name__}: {exc}"}

        # one candle fetch per candidate — concurrent, order preserved
        if present:
            with ThreadPoolExecutor(max_workers=min(8, len(present))) as pool:
                feature_list = list(pool.map(_features, present))
        else:
            feature_list = []
        candidates = [{
            "name": n,
            "features": feats,
            "max_leverage": self.market.info(n).max_leverage,
        } for (n, _c), feats in zip(present, feature_list)]
        # Per-asset cohort positioning, bounded: everything we hold or have
        # resting, plus the biggest movers. One request each, concurrently.
        asset_bias: dict = {}
        try:
            held = [p["market"] for p in positions] + [
                e["market"] for e in self.state.resting_entries()]
            movers = sorted(candidates,
                            key=lambda c: -abs(c["features"].get("day_pct") or 0))
            wanted = list(dict.fromkeys(held + [c["name"] for c in movers]))[:12]
            asset_bias = self.fetch_many_asset_bias(wanted)
            for candidate in candidates:
                bias_row = asset_bias.get(candidate["name"])
                if bias_row:
                    candidate["bias"] = bias_row
        except Exception as exc:  # noqa: BLE001
            self._trench_note("per-asset bias", exc)
        # the gates price range position and ATR from exactly what the analyst
        # saw — but a degraded row must never be cached as if it were complete
        self._features_cache = {c["name"]: c["features"] for c in candidates
                                if not c["features"].get("unavailable")}
        atrs = sorted(f["atr15m_pct"] for f in self._features_cache.values()
                      if isinstance(f.get("atr15m_pct"), (int, float)))
        self._universe_heat = atrs[len(atrs) // 2] if atrs else 0.0

        # Trench's own feeds: what the wallets that make money are positioned
        # in, and the dated macro prints. Neither is load-bearing — a failure is
        # reported and the cycle continues on price and news alone.
        cohort_bias = None
        try:
            cohort_bias = self.fetch_cohort_bias()
        except Exception as exc:  # noqa: BLE001
            self._trench_note("cohort bias", exc)
        # keep the last good read so the dashboard can show it without
        # re-hitting Trench on every poll
        self._bias_snapshot = {
            "cohorts": (cohort_bias or {}).get("cohorts", []),
            "total_traders": (cohort_bias or {}).get("total_traders"),
            "assets": {k: v for k, v in sorted(
                asset_bias.items(),
                key=lambda kv: -abs(kv[1].get("divergence") or 0))},
            "fetched_ts": time.time(),
        }
        try:
            added = 0
            for event in self.fetch_economic_calendar():
                if self.state.add_calendar_event(
                        event["ts"], event["title"], impact=event["impact"],
                        scope=event["scope"]) is not None:
                    added += 1
            if added:
                self.notify.send(f"calendar: {added} new macro event(s) from trench")
        except Exception as exc:  # noqa: BLE001
            self._trench_note("economic calendar", exc)

        from peri.news import _age_str, fetch_headlines
        news = fetch_headlines(self.cfg.news.rss, self.cfg.news.max_headlines)
        for item in self.state.recent_news(self.cfg.news.max_headlines):
            news.append({"source": f"tg:{item['source']}", "title": item["text"][:220],
                         "ts": item["ts"], "age": _age_str(now_ts, item["ts"])})
        news.sort(key=lambda h: h.get("ts") or 0, reverse=True)
        news = news[:self.cfg.news.max_headlines]

        margins = [p["margin"] for p in positions]
        total_margin_used = None
        if all(margin is not None for margin in margins):
            total_margin_used = sum(margins)
        snapshot_margin = self._account_snapshot.get("total_margin_used")
        if (not isinstance(snapshot_margin, bool)
                and isinstance(snapshot_margin, (int, float))
                and math.isfinite(snapshot_margin) and snapshot_margin >= 0):
            total_margin_used = snapshot_margin
        available_margin = self._account_snapshot.get("available_margin")
        if (isinstance(available_margin, bool)
                or not isinstance(available_margin, (int, float))
                or not math.isfinite(available_margin) or available_margin < 0):
            available_margin = (max(self._venue_withdrawable_by_dex.values())
                                if self._venue_withdrawable_by_dex else None)
            if (available_margin is None and self.cfg.mode != "live"
                    and total_margin_used is not None):
                available_margin = max(0.0, equity - total_margin_used)
        withdrawable_by_dex = self._account_snapshot.get("withdrawable_by_dex")
        if not isinstance(withdrawable_by_dex, dict):
            withdrawable_by_dex = dict(self._venue_withdrawable_by_dex)
        venue_closes, venue_realized = self._recent_venue_history
        realized_pnl_recent = (venue_realized if self.cfg.mode == "live"
                               else self.state.realized_total())
        realized_scope = ("recent venue fills" if self.cfg.mode == "live"
                          else "Peri ledger")

        return {
            "trigger": trigger,
            "account": {"equity": equity, "mode": self.cfg.mode, "day_pnl": day_pnl,
                        "entries_today": self.state.entries_today(day),
                        "daily_entry_cap": self.cfg.risk.daily_entry_cap,
                        "peri_open_positions": sum(
                            p.source != "external" for p in open_positions),
                        "external_positions": sum(
                            p.source == "external" for p in open_positions),
                        "max_concurrent": self.cfg.risk.max_concurrent,
                        "available_margin": available_margin,
                        "held_collateral": self._account_snapshot.get("held_collateral"),
                        "spot_usdc_total": self._account_snapshot.get("spot_usdc_total"),
                        "abstraction": self._account_snapshot.get("abstraction"),
                        "total_margin_used": total_margin_used,
                        "withdrawable_by_dex": withdrawable_by_dex,
                        "realized_pnl_recent": realized_pnl_recent,
                        "realized_scope": realized_scope,
                        "kill": self.state.kill_tripped(day)},
            "paused": self.state.paused(),
            "bias": self.state.bias(),
            "cohort_bias": cohort_bias,
            "calendar": self.state.upcoming_events(now=now_ts),
            "lessons": self.state.lessons(
                [c["name"] for c in candidates] + [p["market"] for p in positions], 20),
            "performance": self.state.performance_digest(),
            "notes": self.state.recent_notes(5),
            "orders": orders,
            "resting_entries": self.state.resting_entries(),
            "positions": positions,
            "candidates": candidates,
            "telegram": telegram,
            "news": news,
            "closes": (venue_closes[:8] if self.cfg.mode == "live"
                       else self.state.recent_closes(8)),
            "refusals": self.state.recent_refusals(4, max_age_secs=2 * 3600),
            "fills": self._recent_venue_fills[:50] if self.cfg.mode == "live" else [],
        }

    def _entry_context(self, market: str, resting: bool) -> dict:
        """What the setup looked like at the moment of the decision. Closed
        trades are attributed against this — without it the ledger can say a
        trade lost but never that a KIND of trade loses."""
        features = self._features_cache.get(market) or {}
        return {
            "style": "resting" if resting else "market",
            "range_pos": features.get("range24h_pos"),
            "atr_pct": features.get("atr15m_pct"),
            "trigger": self._current_trigger,
        }

    def _trench_note(self, what: str, exc: Exception) -> None:
        """One line per outage, not one per cycle."""
        key = f"trench:{what}"
        first = key not in self._degraded
        self._degraded.add(key)
        if first:
            self.notify.send(f"trench {what} unavailable: {exc!r} — deciding without it")

    def _features_for(self, market: str) -> dict:
        """Candle features for one market. The cycle fills the cache; anything
        else (chat, a manual proposal) fetches fresh. A failure raises rather
        than returning None — gates that need features must never be skipped."""
        cached = self._features_cache.get(market)
        if cached is not None and not cached.get("unavailable"):
            return cached
        ctx = self.market.ctxs().get(market)
        if ctx is None:
            raise ProposalError(f"no market context for {market}")
        try:
            features = self.market.features(market, ctx)
        except Exception as exc:  # noqa: BLE001 — ungated is worse than refused
            raise ProposalError(
                f"candle features unavailable for {market}: {exc!r}") from exc
        self._features_cache[market] = features
        return features

    @staticmethod
    def _position_fingerprint(position: Position) -> dict:
        return {
            "id": position.id,
            "market": position.market,
            "side": position.side,
            "entry_px": position.entry_px,
            "size": position.size,
            "notional": position.notional,
            "leverage": position.leverage,
            "margin_mode": position.margin_mode,
            "stop_px": position.stop_px,
            "tp_px": position.tp_px,
        }

    @staticmethod
    def _validate_bracket_sides(position: Position, mark: float,
                                stop_px: float, tp_px: float) -> None:
        stop_ok = (
            (position.side == "long" and stop_px < mark)
            or (position.side == "short" and stop_px > mark)
        )
        if not stop_ok:
            raise ProposalError(
                f"new stop {stop_px} on wrong side of mark {mark}"
            )
        tp_ok = (
            (position.side == "long" and tp_px > mark)
            or (position.side == "short" and tp_px < mark)
        )
        if not tp_ok:
            raise ProposalError(
                f"new take-profit {tp_px} on wrong side of mark {mark}"
            )

    def _preview_action(self, action, marks: dict, equity: float, day: str,
                        reserved_markets: frozenset[str], *,
                        chat_authorized: bool = False) -> tuple[object, dict]:
        now = time.time()
        mode = self.cfg.mode
        if isinstance(action, OpenAction):
            pending = self.state.unfinished_action_executions()
            if pending:
                raise ProposalError(
                    f"new entries blocked by unfinished action {pending[0]['id']} "
                    f"({pending[0]['status']})"
                )
            if not self.market.known(action.market) or action.market not in marks:
                raise ProposalError(f"unknown market {action.market}")
            # An open on a market where WE already have an unfilled maker entry
            # is a replacement, not a second position. Until this existed the
            # analyst's only way to say "that level is still right, keep it
            # alive" was an open the gate then refused — 07:57Z on 2026-08-31,
            # 16 minutes before its own xyz:CL order expired. The slot the gate
            # sees occupied is the very order being replaced, so free it; the
            # replacement is still judged by every other rail.
            replacing = self.state.resting_entry_for(action.market)
            if replacing is not None:
                reserved_markets = reserved_markets - {action.market}
            market_info = self.market.info(action.market)
            # format the ENTRY too: the gate must judge the price the venue will
            # receive, or tick rounding can push a "resting" limit across the
            # mark and fill it as a taker at exactly the chase we refused
            canonical = action.model_copy(update={
                "stop": format_price(action.stop, market_info.sz_decimals),
                "take_profit": format_price(
                    action.take_profit, market_info.sz_decimals
                ),
                **({"entry": format_price(action.entry, market_info.sz_decimals)}
                   if action.entry is not None else {}),
            })
            mark = marks[action.market]
            if not math.isfinite(mark) or mark <= 0:
                raise ProposalError(f"no live mark for {action.market}: {mark!r}")
            verdict = self.guard.gate_open(
                canonical, equity, mark, market_info.max_leverage, day,
                available_margin=float(self._account_snapshot["available_margin"]),
                reserved_order_markets=reserved_markets,
                enforce_max_concurrent=not chat_authorized,
                now=now,
                features=self._features_for(action.market),
                day_pnl_pct=self._day_pnl_pct,
                sz_decimals=market_info.sz_decimals,
            )
            if isinstance(verdict, Refusal):
                raise ProposalError(verdict.reason)
            # The guard already rounded to the venue lot and re-judged every
            # invariant against it, so nothing here re-rounds: what it approved
            # is exactly what is sent.
            entry_px = verdict.entry_px
            size = verdict.size
            notional = verdict.notional
            replacing_id = replacing["id"] if replacing else None
            replacing_oid = self._order_number(replacing.get("oid")) if replacing else None
            margin = notional / verdict.leverage
            available = float(self._account_snapshot["available_margin"])
            if margin > available:
                raise ProposalError(
                    f"margin ${margin:.2f} exceeds available ${available:.2f}"
                )
            if size <= 0:
                raise ProposalError("derived entry size rounds to zero")
            # where the venue would liquidate this, so the number is visible in
            # the confirmation dialog and in the journal afterwards
            liq_px = liq_headroom = None
            if canonical.margin_mode == "isolated":
                liq_dist = isolated_liq_distance(
                    verdict.leverage, market_info.max_leverage)
                if liq_dist > 0:
                    liq_px = format_price(
                        entry_px * (1 - liq_dist) if canonical.side == "long"
                        else entry_px * (1 + liq_dist), market_info.sz_decimals)
                    stop_dist = abs(entry_px - canonical.stop) / entry_px
                    liq_headroom = liq_dist - stop_dist
            if canonical.side == "long":
                tp_gross = (canonical.take_profit - entry_px) * size
                stop_gross = (entry_px - canonical.stop) * size
            else:
                tp_gross = (entry_px - canonical.take_profit) * size
                stop_gross = (canonical.stop - entry_px) * size
            tp_fees = (entry_px + canonical.take_profit) * size * FEE_RATE
            stop_fees = (entry_px + canonical.stop) * size * FEE_RATE
            preview = {
                "kind": "open",
                "mode": mode,
                "market": canonical.market,
                "side": canonical.side,
                "reference_mark": mark,
                "entry_px": entry_px,
                "resting": verdict.resting,
                "size": size,
                "notional": notional,
                "risk_usd": verdict.size_usd_risk,
                "leverage": verdict.leverage,
                "margin_mode": verdict.margin_mode,
                "required_margin": margin,
                "available_margin": available,
                "stop_px": canonical.stop,
                "tp_px": canonical.take_profit,
                "liquidation_px": liq_px,
                "stop_inside_liquidation_by": liq_headroom,
                "reward_risk": tp_gross / stop_gross,
                "estimated_tp_net": tp_gross - tp_fees,
                "estimated_stop_loss": stop_gross + stop_fees,
                "estimated_tp_fees": tp_fees,
                "estimated_stop_fees": stop_fees,
                "rationale": canonical.rationale,
                "invalidation": canonical.invalidation,
                "replacing_entry_id": replacing_id,
                "replacing_entry_oid": replacing_oid,
            }
            return canonical, preview

        position = self.state.open_position_for(action.market)
        if position is None and isinstance(action, CloseAction):
            # A close on a market with a RESTING entry is a cancel. On
            # 2026-08-30 the analyst read fresh escalation headlines, tried to
            # pull its resting BTC long with exactly this action, and was told
            # "no open position" — the order it wanted gone filled 42 minutes
            # later. Withdrawing an order you no longer believe in must not
            # require a position to exist first.
            pending = self.state.resting_entry_for(action.market)
            if pending is not None:
                return action, {
                    "kind": "cancel_entry",
                    "mode": mode,
                    "market": action.market,
                    "side": pending["side"],
                    "entry_px": pending["entry_px"],
                    "size": pending["size"],
                    "stop_px": pending["stop_px"],
                    "tp_px": pending["tp_px"],
                    "pending_entry_id": pending["id"],
                    "oid": pending["oid"],
                    "rationale": action.rationale,
                }
        if position is None:
            raise ProposalError(
                f"no open position or resting entry on {action.market}")
        mark = marks.get(action.market)
        if mark is None or not math.isfinite(mark) or mark <= 0:
            raise ProposalError(f"no live mark for {action.market}")
        fingerprint = self._position_fingerprint(position)

        if isinstance(action, CloseAction):
            gross = (
                (mark - position.entry_px) * position.size
                if position.side == "long"
                else (position.entry_px - mark) * position.size
            )
            fees = (position.entry_px + mark) * position.size * FEE_RATE
            return action, {
                "kind": "close",
                "mode": mode,
                "market": position.market,
                "side": position.side,
                "reference_mark": mark,
                "full_size": position.size,
                "entry_px": position.entry_px,
                "estimated_gross_pnl": gross,
                "estimated_fees": fees,
                "estimated_net_pnl": gross - fees,
                "current_stop_px": position.stop_px,
                "current_tp_px": position.tp_px,
                "position_fingerprint": fingerprint,
                "rationale": action.rationale,
            }

        if isinstance(action, AdjustStopAction):
            market_info = self.market.info(action.market)
            canonical = action.model_copy(update={
                "stop": format_price(action.stop, market_info.sz_decimals),
                "take_profit": format_price(
                    action.take_profit, market_info.sz_decimals
                ),
            })
            self._validate_bracket_sides(
                position, mark, canonical.stop, canonical.take_profit
            )
            return canonical, {
                "kind": "adjust_stop",
                "mode": mode,
                "market": position.market,
                "side": position.side,
                "reference_mark": mark,
                "full_size": position.size,
                "current_stop_px": position.stop_px,
                "current_tp_px": position.tp_px,
                "new_stop_px": canonical.stop,
                "new_tp_px": canonical.take_profit,
                "position_fingerprint": fingerprint,
                "rationale": canonical.rationale,
            }
        raise ProposalError(f"unsupported action: {type(action).__name__}")

    def chat(self, message: str,
             stream_event: Optional[Callable[[str, dict], None]] = None) -> dict:
        user_row = self.state.add_chat_message("user", message)
        history = self.state.chat_history(limit=40, before_id=user_row["id"])
        if stream_event is not None:
            stream_event("status", {"phase": "refreshing_context"})
        try:
            # context_snapshot reconciles, manages positions and settles resting
            # entries — all ledger writes. Without the lock a dashboard chat
            # racing a cycle double-counts an entry fee and can create two
            # ledger rows for one venue position.
            with self._trade_lock:
                bundle, marks, equity, day, reserved = self.context_snapshot(
                    "analyst chat", target_query=message
                )
        except Exception as exc:  # noqa: BLE001 — explicit upstream failure is the answer
            reason = str(exc) if isinstance(exc, ContextError) else (
                f"upstream context unavailable: {type(exc).__name__}: {exc}"
            )
            if stream_event is not None:
                stream_event("error", {"phase": "context", "error": reason})
            assistant = self.state.add_chat_message(
                "assistant", f"Live upstream context unavailable: {reason}",
                metadata={"error": reason},
            )
            return {"message": assistant, "proposal": None}

        bundle["decisions"] = self.state.recent_decisions(8)
        if stream_event is not None:
            stream_event("context", {
                "as_of_ts": bundle["context_ts"],
                "account_equity": bundle["account"]["equity"],
                "available_margin": bundle["account"]["available_margin"],
                "open_positions": len(bundle["positions"]),
                "open_orders": len(bundle["orders"]),
                "venue_fills": len(bundle.get("fills", [])),
                "telegram_messages": len(bundle["telegram"]),
                "news_items": len(bundle["news"]),
                "candidates": [c["name"] for c in bundle["candidates"]],
            })
            stream_event("status", {"phase": "analyst"})
        try:
            if stream_event is None:
                result = self.analyst.chat(bundle, history, message)
            else:
                result = self.analyst.chat(
                    bundle, history, message, on_event=stream_event
                )
        except AnalystError as exc:
            if stream_event is not None:
                stream_event("error", {"phase": "analyst", "error": str(exc)})
            assistant = self.state.add_chat_message(
                "assistant", f"Qwen analyst unavailable: {exc}",
                context_ts=bundle["context_ts"], metadata={"error": str(exc)},
            )
            return {"message": assistant, "proposal": None}

        proposal = None
        metadata = {
            "latency_ms": result.latency_ms,
            "tools": result.tool_log,
        }
        answer = result.response.answer
        if isinstance(result.response.proposal, RememberAction):
            # a lesson moves no money: apply it now rather than asking the owner
            # to confirm a note-to-self
            lesson = result.response.proposal
            before = {row["id"] for row in self.state.lessons(limit=200)}
            self.exec_remember(lesson)
            after = {row["id"] for row in self.state.lessons(limit=200)}
            stored = bool(after - before)
            metadata["lesson"] = {"text": lesson.lesson, "market": lesson.market,
                                  "stored": stored}
            answer = f"{answer}\n\n{'Remembered' if stored else 'Already remembered'}: {lesson.lesson}"
            if stream_event is not None:
                stream_event("lesson", metadata["lesson"])
        elif result.response.proposal is not None:
            try:
                canonical, preview = self._preview_action(
                    result.response.proposal, marks, equity, day, reserved,
                    chat_authorized=True,
                )
            except ProposalError as exc:
                metadata["proposal_refusal"] = str(exc)
                answer = f"{answer}\n\nNo confirmation created: {exc}"
            else:
                proposal_id = secrets.token_urlsafe(18)
                created_ts = time.time()
                proposal = self.state.create_trade_proposal(
                    proposal_id,
                    action=canonical.model_dump(mode="json"),
                    preview=preview,
                    context_ts=bundle["context_ts"],
                    expires_ts=created_ts + 120,
                    now=created_ts,
                )
                if stream_event is not None:
                    stream_event("proposal", proposal)

        assistant = self.state.add_chat_message(
            "assistant", answer, context_ts=bundle["context_ts"],
            proposal_id=proposal["id"] if proposal else None,
            metadata=metadata,
        )
        if stream_event is not None:
            stream_event("status", {"phase": "complete"})
        return {"message": assistant, "proposal": proposal}

    def _finish_claimed_proposal(self, proposal_id: str, status: str,
                                 reason: str) -> dict:
        return self.state.finish_trade_proposal(
            proposal_id, status, {"status": status, "reason": reason}
        )

    @staticmethod
    def _relative_change(current: float, reference: float) -> float:
        if reference == 0:
            return math.inf if current != 0 else 0.0
        return abs(current - reference) / abs(reference)

    def confirm_trade(self, proposal_id: str) -> dict:
        with self._trade_lock:
            proposal = self.state.claim_trade_proposal(proposal_id)
            if proposal is None:
                existing = self.state.trade_proposal(proposal_id)
                if existing is None:
                    raise ProposalError(f"proposal {proposal_id} not found")
                return existing

            if time.time() > proposal["expires_ts"]:
                return self._finish_claimed_proposal(
                    proposal_id, "expired", "confirmation window expired"
                )
            try:
                action = ACTION_ADAPTER.validate_python(proposal["action"])
            except Exception as exc:
                return self._finish_claimed_proposal(
                    proposal_id, "failed",
                    f"stored proposal action is invalid: {exc}",
                )

            try:
                _, marks, equity, day, reserved = self.context_snapshot(
                    "chat confirmation", target_query=action.market
                )
                canonical, fresh = self._preview_action(
                    action, marks, equity, day, reserved,
                    chat_authorized=True,
                )
            except Exception as exc:  # noqa: BLE001 — fail closed on any fresh-state gap
                return self._finish_claimed_proposal(
                    proposal_id, "refused",
                    f"fresh revalidation failed: {exc}",
                )

            stored = proposal["preview"]
            reference_mark = float(stored["reference_mark"])
            current_mark = float(fresh["reference_mark"])
            if self._relative_change(current_mark, reference_mark) > 0.005:
                return self._finish_claimed_proposal(
                    proposal_id, "refused",
                    f"reference mark moved more than 0.5% "
                    f"({reference_mark:g} -> {current_mark:g})",
                )

            if isinstance(canonical, OpenAction):
                if (
                    fresh["leverage"] != stored["leverage"]
                    or fresh["margin_mode"] != stored["margin_mode"]
                    or fresh["size"] != stored["size"]
                    or self._relative_change(
                        fresh["notional"], stored["notional"]
                    ) > 0.01
                    or self._relative_change(
                        fresh["required_margin"], stored["required_margin"]
                    ) > 0.01
                ):
                    return self._finish_claimed_proposal(
                        proposal_id, "refused",
                        "entry size, margin, leverage, or margin mode changed",
                    )
                return_value = self._execute_open_preview(
                    canonical, fresh, day, origin="chat",
                    proposal_id=proposal_id, decision_id=None,
                )
            elif isinstance(canonical, CloseAction):
                if (
                    fresh["position_fingerprint"]
                    != stored["position_fingerprint"]
                ):
                    return self._finish_claimed_proposal(
                        proposal_id, "refused",
                        "open position changed since the confirmation card",
                    )
                return_value = self._execute_close_preview(
                    canonical, fresh, origin="chat",
                    proposal_id=proposal_id, decision_id=None,
                )
            else:
                if (
                    fresh["position_fingerprint"]
                    != stored["position_fingerprint"]
                    or fresh["new_stop_px"] != stored["new_stop_px"]
                    or fresh["new_tp_px"] != stored["new_tp_px"]
                ):
                    return self._finish_claimed_proposal(
                        proposal_id, "refused",
                        "position or replacement bracket levels changed",
                    )
                return_value = self._execute_adjust_preview(
                    canonical, fresh, origin="chat",
                    proposal_id=proposal_id, decision_id=None,
                )

            updated = self.state.trade_proposal(proposal_id)
            if updated is None:
                raise RuntimeError(
                    f"proposal {proposal_id} disappeared after execution"
                )
            if updated["result"] is None:
                updated["result"] = return_value
            return updated

    @staticmethod
    def _safer_trigger(kind: str, existing: float, candidate: float,
                       close_side) -> float:
        """Of two triggers of the same kind, the one that protects sooner.

        A reduce-only BUY closes a short (stop above, TP below); a reduce-only
        SELL closes a long (stop below, TP above)."""
        closes_short = close_side == "B"
        if kind == "stop":
            return min(existing, candidate) if closes_short else max(existing, candidate)
        return max(existing, candidate) if closes_short else min(existing, candidate)

    def sync_live_brackets(self, orders: list[dict]) -> None:
        """Make venue protective triggers authoritative for live positions."""
        levels: dict[str, dict[str, float]] = {}
        for order in orders:
            is_trigger = order.get("isTrigger")
            if not isinstance(is_trigger, bool):
                raise ContextError(f"open order isTrigger is malformed: {order!r}")
            if not order["reduceOnly"] or not is_trigger:
                continue
            order_type = order.get("orderType")
            if isinstance(order_type, str) and order_type.startswith("Stop"):
                kind = "stop"
            elif isinstance(order_type, str) and order_type.startswith("Take Profit"):
                kind = "tp"
            else:
                raise ContextError(f"protective order type is malformed: {order!r}")
            try:
                px = float(order["triggerPx"])
            except (KeyError, TypeError, ValueError) as e:
                raise ContextError(f"protective trigger price is malformed: {order!r}") from e
            if not math.isfinite(px) or px <= 0:
                raise ContextError(f"protective trigger price is malformed: {order!r}")
            market_levels = levels.setdefault(order["coin"], {})
            if kind in market_levels:
                # manage_trade.py deliberately places split take-profit tranches,
                # so a multi-trigger book is a legitimate venue state. Keep the
                # protective extreme instead of failing the cycle and going blind.
                px = self._safer_trigger(kind, market_levels[kind], px,
                                         order.get("side"))
            market_levels[kind] = px

        for position in self.state.open_positions():
            market_levels = levels.get(position.market, {})
            stop_px = market_levels.get("stop")
            tp_px = market_levels.get("tp")
            if position.stop_px != stop_px or position.tp_px != tp_px:
                self.state.update_brackets(position.id, stop_px, tp_px)
            self._guard_protection(position, stop_px, tp_px)

    def _guard_protection(self, position: Position, stop_px, tp_px) -> None:
        """A live position with no venue protection is the one state this system
        must never sit quietly in.

        Recording NULL used to be the whole response: breakeven then declined to
        act (it needs a tp), the time stop waited three hours, and 'code owns
        risk' silently stopped being true. Re-place from the levels the position
        was sized with, and say so loudly either way."""
        if position.source == "external" or (stop_px is not None and tp_px is not None):
            return
        intended_stop = self.state.initial_stop(position.id) or position.stop_px or stop_px
        intended_tp = position.tp_px or tp_px
        missing = ("stop" if stop_px is None else "") + (
            " and take-profit" if tp_px is None else "")
        if intended_stop is None or intended_tp is None:
            self.notify.send(
                f"UNPROTECTED: {position.market} {position.side} has no venue "
                f"{missing.strip()} and no recorded level to restore it from — "
                "close it or set brackets by hand")
            return
        try:
            with self._adapter_lock:
                self.adapter.place_brackets(
                    position.market, position.side, position.size,
                    intended_stop, intended_tp)
            self.state.update_brackets(position.id, intended_stop, intended_tp)
        except Exception as exc:  # noqa: BLE001 — exposure stays; shout about it
            self.notify.send(
                f"UNPROTECTED: {position.market} {position.side} lost its "
                f"{missing.strip()} and re-arming FAILED: {exc!r}")
            return
        self.notify.send(
            f"re-armed {position.market} {position.side}: venue {missing.strip()} "
            f"was gone, restored stop {intended_stop:g} / tp {intended_tp:g}")

    # -- execution ---------------------------------------------------------
    def execute(self, action, marks: dict, equity: float, day: str,
                reserved_markets: frozenset[str], *,
                origin: str = "autonomous",
                proposal_id: str | None = None,
                decision_id: int | None = None) -> None:
        with self._execution_lock:
            self._execute_locked(action, marks, equity, day, reserved_markets,
                                 origin=origin, proposal_id=proposal_id,
                                 decision_id=decision_id)

    def _execute_locked(self, action, marks: dict, equity: float, day: str,
                        reserved_markets: frozenset[str], *,
                        origin: str = "autonomous",
                        proposal_id: str | None = None,
                        decision_id: int | None = None) -> None:
        if isinstance(action, OpenAction):
            self.exec_open(
                action, marks, equity, day, reserved_markets,
                origin=origin, proposal_id=proposal_id, decision_id=decision_id,
            )
        elif isinstance(action, CloseAction):
            self.exec_close(
                action, marks, equity, day, reserved_markets,
                origin=origin, proposal_id=proposal_id, decision_id=decision_id,
            )
        elif isinstance(action, AdjustStopAction):
            self.exec_adjust(
                action, marks, equity, day, reserved_markets,
                origin=origin, proposal_id=proposal_id, decision_id=decision_id,
            )
        elif isinstance(action, RememberAction):
            self.exec_remember(action, decision_id=decision_id)

    def _chat_decision(self, action, proposal_id: str) -> dict:
        return {
            "trigger": f"chat authorized {proposal_id}",
            "market_view": "user-authorized analyst chat action",
            "actions_json": dumps_actions([action]),
            "model": self.analyst.model,
            "latency_ms": 0,
            "status": "ok",
            "reasoning": "",
            "prompt": "",
            "tool_log": "[]",
        }

    def _decision_args(self, action, proposal_id: str | None,
                       decision_id: int | None) -> dict:
        if decision_id is not None:
            return {"decision_id": decision_id}
        if proposal_id is None:
            raise RuntimeError("journaled action requires decision provenance")
        return {"decision": self._chat_decision(action, proposal_id)}

    def _mark_execution_problem(self, execution_id: str,
                                proposal_id: str | None, status: str,
                                reason: str, **details) -> dict:
        result = {"status": status, "reason": reason, **details}
        self.state.update_action_execution(
            execution_id, stage=status, status=status, response_ts=time.time(),
            result=result,
        )
        if proposal_id is not None:
            self.state.finish_trade_proposal(proposal_id, status, result)
        if status in {"needs_reconciliation", "manual_review"}:
            self.notify.send(
                f"HIGH PRIORITY: action {execution_id} {status}: {reason}"
            )
        return result

    def _execute_resting_entry(self, action: OpenAction, preview: dict, *,
                               origin: str, proposal_id: str | None,
                               decision_id: int | None) -> dict:
        """Park a maker limit at the analyst's level with SL+TP attached.

        Nothing is opened here — the venue holds one order that either fills at
        our price (brackets arm atomically) or expires untouched. This is the
        cure for chasing: the level waits for the market instead of the engine
        paying the spread to catch a move that already happened.
        """
        execution_id = secrets.token_urlsafe(18)
        self.state.create_action_execution(
            execution_id, origin=origin, kind="open", proposal_id=proposal_id,
            action=action.model_dump(mode="json"),
            pre_state={"position": None}, expected=preview, decision_id=decision_id,
        )
        approved = Approved(
            market=action.market, side=action.side, notional=preview["notional"],
            size_usd_risk=preview["risk_usd"], leverage=preview["leverage"],
            margin=preview["required_margin"], margin_mode=preview["margin_mode"],
            stop_px=preview["stop_px"], tp_px=preview["tp_px"],
            entry_px=preview["entry_px"], resting=True, size=preview["size"],
        )
        self.state.update_action_execution(
            execution_id, stage="entry_submitted", status="executing",
            submission_ts=time.time(),
        )
        try:
            with self._adapter_lock:
                placed = self.adapter.place_resting_entry(approved, preview["size"])
        except Exception as exc:  # noqa: BLE001 — submission outcome may be ambiguous
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"resting entry submission failed or timed out: {exc!r}")

        expires_ts = time.time() + self.cfg.risk.entry_expiry_secs
        try:
            entry_id = self.state.add_pending_entry(
                market=action.market, side=action.side, entry_px=preview["entry_px"],
                size=preview["size"], notional=preview["notional"],
                leverage=preview["leverage"], margin_mode=preview["margin_mode"],
                stop_px=preview["stop_px"], tp_px=preview["tp_px"],
                conviction=action.conviction, rationale=action.rationale,
                invalidation=action.invalidation, oid=placed.get("oid"),
                decision_id=decision_id, expires_ts=expires_ts,
                entry_context=self._entry_context(action.market, resting=True),
            )
        except Exception as exc:  # noqa: BLE001 — an untracked live order is worse
            # The order is ALREADY at the venue. Without its ledger row nothing
            # can expire it, cancel it on pause, or attribute the fill, so the
            # only safe outcome is to take it back off the book.
            oid = self._order_number(placed.get("oid"))
            try:
                with self._adapter_lock:
                    if oid is not None:
                        self.adapter.cancel_orders(action.market, [int(oid)])
                    self._clear_orphan_brackets(action.market)
            except Exception as cleanup_exc:  # noqa: BLE001 — exposure may remain
                return self._mark_execution_problem(
                    execution_id, proposal_id, "needs_reconciliation",
                    f"resting entry placed but the ledger write failed ({exc!r}); "
                    f"cancelling it ALSO failed ({cleanup_exc!r}) — there is a live "
                    f"order on {action.market} that nothing is managing",
                    oid=placed.get("oid"))
            return self._mark_execution_problem(
                execution_id, proposal_id, "failed",
                f"ledger write failed after the order was placed; the order was "
                f"cancelled and no exposure remains: {exc!r}")
        result = {
            "status": "resting",
            "execution_id": execution_id,
            "pending_entry_id": entry_id,
            "market": action.market,
            "side": action.side,
            "entry_px": preview["entry_px"],
            "size": preview["size"],
            "stop_px": preview["stop_px"],
            "tp_px": preview["tp_px"],
            "oid": placed.get("oid"),
            "expires_ts": expires_ts,
        }
        self.state.update_action_execution(
            execution_id, stage="resting", status="executed",
            response_ts=time.time(), result=result)
        if proposal_id:
            self._finish_claimed_proposal(proposal_id, "executed", "resting entry placed")
        self.notify.send(
            f"RESTING {action.side} {action.market} @ {preview['entry_px']:g} "
            f"(mark {preview['reference_mark']:g}) size {preview['size']:g} "
            f"SL {preview['stop_px']:g} TP {preview['tp_px']:g} "
            f"· expires in {self.cfg.risk.entry_expiry_secs // 60}m")
        return result

    def _execute_open_preview(self, action: OpenAction, preview: dict, day: str, *,
                              origin: str, proposal_id: str | None,
                              decision_id: int | None) -> dict:
        self._withdraw_replaced_entry(preview)
        if preview.get("resting"):
            return self._execute_resting_entry(
                action, preview, origin=origin, proposal_id=proposal_id,
                decision_id=decision_id)
        execution_id = secrets.token_urlsafe(18)
        self.state.create_action_execution(
            execution_id, origin=origin, kind="open", proposal_id=proposal_id,
            action=action.model_dump(mode="json"),
            pre_state={"position": None},
            expected=preview,
            decision_id=decision_id,
        )
        approved = Approved(
            market=action.market,
            side=action.side,
            notional=preview["notional"],
            size_usd_risk=preview["risk_usd"],
            leverage=preview["leverage"],
            margin=preview["required_margin"],
            margin_mode=preview["margin_mode"],
            stop_px=preview["stop_px"],
            tp_px=preview["tp_px"],
            # The guard already floored this to the venue lot and re-judged every
            # invariant against it. Omitting it here sent Approved.size=0.0 to the
            # adapter, whose fallback re-derived the lot with round() — rounding UP
            # past the approved risk budget on the path that takes most trades.
            entry_px=preview["entry_px"],
            resting=False,
            size=preview["size"],
        )
        submitted_ts = time.time()
        self.state.update_action_execution(
            execution_id, stage="entry_submitted", status="executing",
            submission_ts=submitted_ts,
        )
        try:
            with self._adapter_lock:
                fill = self.adapter.open_entry(approved, preview["reference_mark"])
        except Exception as exc:  # noqa: BLE001 — submission outcome may be ambiguous
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"entry submission failed or timed out: {exc!r}",
            )

        fill_id = fill.get("fill_id")
        self.state.update_action_execution(
            execution_id, stage="entry_filled", status="executing",
            response_ts=time.time(), fill_id=str(fill_id) if fill_id else None,
            fill_px=float(fill["entry_px"]),
            result={"entry_px": fill["entry_px"], "size": fill["size"]},
        )
        self.state.update_action_execution(
            execution_id, stage="bracketing", status="executing"
        )
        try:
            with self._adapter_lock:
                brackets = self.adapter.place_brackets(
                    action.market, action.side, fill["size"],
                    preview["stop_px"], preview["tp_px"],
                )
        except Exception as bracket_exc:  # noqa: BLE001 — cleanup is mandatory
            transient = Position(
                0, action.market, action.side, float(fill["entry_px"]),
                float(fill["size"]), preview["notional"], preview["leverage"],
                preview["margin_mode"], preview["stop_px"], preview["tp_px"],
                action.conviction, action.source, action.rationale,
                action.invalidation, "open", submitted_ts,
            )
            try:
                with self._adapter_lock:
                    cleanup = self.adapter.close_position_only(transient)
            except Exception as cleanup_exc:  # noqa: BLE001 — exposure may remain
                return self._mark_execution_problem(
                    execution_id, proposal_id, "needs_reconciliation",
                    f"bracket placement failed ({bracket_exc!r}); "
                    f"cleanup close ambiguous ({cleanup_exc!r})",
                    entry_px=fill["entry_px"], size=fill["size"],
                )
            return self._mark_execution_problem(
                execution_id, proposal_id, "failed",
                f"bracket placement failed; entry was closed: {bracket_exc!r}",
                entry_px=fill["entry_px"], size=fill["size"], cleanup=cleanup,
            )

        result = {
            "status": "executed",
            "execution_id": execution_id,
            "market": action.market,
            "side": action.side,
            "entry_px": float(fill["entry_px"]),
            "size": float(fill["size"]),
            "notional": preview["notional"],
            "leverage": preview["leverage"],
            "margin_mode": preview["margin_mode"],
            "stop_px": preview["stop_px"],
            "tp_px": preview["tp_px"],
            "brackets": brackets,
        }
        try:
            self.state.finalize_open_action(
                execution_id, proposal_id,
                position={
                    "market": action.market,
                    "side": action.side,
                    "entry_px": float(fill["entry_px"]),
                    "size": float(fill["size"]),
                    "notional": preview["notional"],
                    "leverage": preview["leverage"],
                    "margin_mode": preview["margin_mode"],
                    "stop_px": preview["stop_px"],
                    "tp_px": preview["tp_px"],
                    "conviction": action.conviction,
                    "source": action.source,
                    "rationale": action.rationale,
                    "invalidation": action.invalidation,
                    "entry_context": self._entry_context(action.market, resting=False),
                },
                result=result,
                day=day,
                **self._decision_args(action, proposal_id, decision_id),
            )
        except Exception as exc:
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"venue action succeeded but ledger finalization failed: {exc!r}",
                **{key: value for key, value in result.items()
                   if key not in ("status", "execution_id")},
            )
        self.notify.send(fmt_open(self.cfg.mode, approved, float(fill["entry_px"])))
        return result

    def _execute_cancel_entry(self, action: CloseAction, preview: dict) -> dict:
        """Withdraw a resting entry the analyst no longer believes in."""
        oid = self._order_number(preview.get("oid"))
        try:
            with self._adapter_lock:
                if oid is not None:
                    self.adapter.cancel_orders(action.market, [int(oid)])
                self._clear_orphan_brackets(action.market)
        except Exception as exc:  # noqa: BLE001 — the order may still be working
            self.notify.send(
                f"could not cancel the resting {preview['side']} on {action.market}: "
                f"{exc!r} — the order may still be live")
            return {"status": "failed", "market": action.market, "reason": repr(exc)}
        self.state.settle_pending_entry(preview["pending_entry_id"], "cancelled")
        self.notify.send(
            f"cancelled resting {preview['side']} {action.market} @ "
            f"{preview['entry_px']:g} — {action.rationale[:90]}")
        return {"status": "cancelled", "market": action.market,
                "entry_px": preview["entry_px"], "rationale": action.rationale}

    def _execute_close_preview(self, action: CloseAction, preview: dict, *,
                               origin: str, proposal_id: str | None,
                               decision_id: int | None) -> dict:
        if preview.get("kind") == "cancel_entry":
            return self._execute_cancel_entry(action, preview)
        position = self.state.open_position_for(action.market)
        if position is None:
            raise ProposalError(f"no open position on {action.market}")
        execution_id = secrets.token_urlsafe(18)
        self.state.create_action_execution(
            execution_id, origin=origin, kind="close", proposal_id=proposal_id,
            action=action.model_dump(mode="json"),
            pre_state={"position": self._position_fingerprint(position)},
            expected=preview,
            decision_id=decision_id,
        )
        self.state.update_action_execution(
            execution_id, stage="submitted", status="executing",
            submission_ts=time.time(),
        )
        try:
            with self._adapter_lock:
                output = self.adapter.close(position, preview["reference_mark"])
        except Exception as exc:  # noqa: BLE001 — never resubmit an ambiguous close
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"full close failed or timed out: {exc!r}",
            )
        close_px = float(output["close_px"])
        pnl = realized_pnl(
            position.side, position.entry_px, close_px, position.size
        )
        result = {
            "status": "executed",
            "execution_id": execution_id,
            "market": position.market,
            "full_size": position.size,
            "close_px": close_px,
            "realized_pnl": pnl,
        }
        try:
            self.state.finalize_close_action(
                execution_id, proposal_id,
                position_id=position.id,
                close_reason="analyst",
                close_px=close_px,
                realized_pnl=pnl,
                result=result,
                **self._decision_args(action, proposal_id, decision_id),
            )
        except Exception as exc:
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"close filled but ledger finalization failed: {exc!r}",
                **{key: value for key, value in result.items()
                   if key not in ("status", "execution_id")},
            )
        self.guard.cooldown_after_close(position.market, "analyst", pnl=pnl)
        self.notify.send(fmt_close(
            position.market, f"analyst: {action.rationale[:80]}", close_px, pnl
        ))
        return result

    def _execute_adjust_preview(self, action: AdjustStopAction, preview: dict, *,
                                origin: str, proposal_id: str | None,
                                decision_id: int | None) -> dict:
        position = self.state.open_position_for(action.market)
        if position is None:
            raise ProposalError(f"no open position on {action.market}")
        try:
            with self._adapter_lock:
                old_orders = self.adapter.bracket_orders(action.market)
        except Exception as exc:
            result = {
                "status": "failed",
                "reason": f"could not snapshot existing brackets: {exc!r}",
            }
            if proposal_id is not None:
                self.state.finish_trade_proposal(proposal_id, "failed", result)
            return result
        execution_id = secrets.token_urlsafe(18)
        self.state.create_action_execution(
            execution_id, origin=origin, kind="adjust_stop",
            proposal_id=proposal_id,
            action=action.model_dump(mode="json"),
            pre_state={
                "position": self._position_fingerprint(position),
                "old_orders": old_orders,
            },
            expected=preview,
            decision_id=decision_id,
        )
        self.state.update_action_execution(
            execution_id, stage="submitted", status="executing",
            submission_ts=time.time(),
        )
        try:
            with self._adapter_lock:
                venue_result = self.adapter.adjust_stop(
                    position, preview["new_stop_px"], preview["new_tp_px"]
                )
        except Exception as exc:  # noqa: BLE001 — pair state may need inspection
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"bracket replacement failed or timed out: {exc!r}",
            )
        result = {
            "status": "executed",
            "execution_id": execution_id,
            "market": position.market,
            "full_size": position.size,
            "stop_px": preview["new_stop_px"],
            "tp_px": preview["new_tp_px"],
            "venue": venue_result,
        }
        try:
            self.state.finalize_adjust_action(
                execution_id, proposal_id,
                position_id=position.id,
                stop_px=preview["new_stop_px"],
                tp_px=preview["new_tp_px"],
                result=result,
                **self._decision_args(action, proposal_id, decision_id),
            )
        except Exception as exc:
            status = "needs_reconciliation" if self.cfg.mode == "live" else "failed"
            return self._mark_execution_problem(
                execution_id, proposal_id, status,
                f"brackets changed but ledger finalization failed: {exc!r}",
                **{key: value for key, value in result.items()
                   if key not in ("status", "execution_id")},
            )
        self.notify.send(
            f"brackets {position.market} -> SL {preview['new_stop_px']} · "
            f"TP {preview['new_tp_px']} ({action.rationale[:80]})"
        )
        return result

    def _withdraw_replaced_entry(self, preview: dict) -> None:
        """Take the old maker order off the book before its replacement goes on.

        Cancel first, place second: the other order risks two live entries on
        one market, which is the doubling-up the venue-order gate exists to
        stop. If the placement then fails there is simply no order — no
        exposure, and the analyst can propose again."""
        entry_id = preview.get("replacing_entry_id")
        if not entry_id:
            return
        market = preview["market"]
        oid = preview.get("replacing_entry_oid")
        if self.cfg.mode == "live" and oid is not None:
            with self._adapter_lock:
                self.adapter.cancel_orders(market, [int(oid)])
                # the old brackets belong to the order just cancelled
                self._clear_orphan_brackets(market)
        self.state.settle_pending_entry(int(entry_id), "cancelled")
        self.notify.send(f"replacing the resting entry on {market} — old order withdrawn")

    def exec_open(self, a: OpenAction, marks: dict, equity: float, day: str,
                  reserved_markets: frozenset[str], *, origin: str = "autonomous",
                  proposal_id: str | None = None,
                  decision_id: int | None = None) -> None:
        try:
            canonical, preview = self._preview_action(
                a, marks, equity, day, reserved_markets
            )
        except ProposalError as exc:
            self.refuse(a, str(exc))
            if proposal_id is not None:
                self.state.finish_trade_proposal(
                    proposal_id, "refused", {"status": "refused", "reason": str(exc)}
                )
            return
        self._execute_open_preview(
            canonical, preview, day, origin=origin,
            proposal_id=proposal_id, decision_id=decision_id,
        )

    def exec_close(self, a: CloseAction, marks: dict, equity: float, day: str,
                   reserved_markets: frozenset[str], *,
                   origin: str = "autonomous",
                   proposal_id: str | None = None,
                   decision_id: int | None = None) -> None:
        try:
            canonical, preview = self._preview_action(
                a, marks, equity, day, reserved_markets
            )
        except ProposalError as exc:
            self.refuse(a, str(exc))
            if proposal_id is not None:
                self.state.finish_trade_proposal(
                    proposal_id, "refused", {"status": "refused", "reason": str(exc)}
                )
            return
        self._execute_close_preview(
            canonical, preview, origin=origin,
            proposal_id=proposal_id, decision_id=decision_id,
        )

    def exec_adjust(self, a: AdjustStopAction, marks: dict, equity: float,
                    day: str, reserved_markets: frozenset[str], *,
                    origin: str = "autonomous",
                    proposal_id: str | None = None,
                    decision_id: int | None = None) -> None:
        try:
            canonical, preview = self._preview_action(
                a, marks, equity, day, reserved_markets
            )
        except ProposalError as exc:
            self.refuse(a, str(exc))
            if proposal_id is not None:
                self.state.finish_trade_proposal(
                    proposal_id, "refused", {"status": "refused", "reason": str(exc)}
                )
            return
        self._execute_adjust_preview(
            canonical, preview, origin=origin,
            proposal_id=proposal_id, decision_id=decision_id,
        )

    def exec_remember(self, a: RememberAction, *, decision_id: int | None = None) -> None:
        """Write one lesson to durable memory. No venue call, no risk, no gate —
        the only cost of being wrong is a wasted line, and the cost of not
        writing it is repeating Friday."""
        if a.market and not self.market.known(a.market):
            self.refuse(a, f"lesson tagged to unknown market {a.market}")
            return
        lesson_id = self.state.add_lesson(
            a.lesson, market=a.market, source="analyst", decision_id=decision_id)
        if lesson_id is None:
            return          # already remembered; repeating it is not new knowledge
        self.notify.send(f"learned{f' [{a.market}]' if a.market else ''}: {a.lesson}")

    def refuse(self, action, reason: str) -> None:
        self.state.record_refusal(getattr(action, "market", None),
                                  action.model_dump_json(), reason)
        self.notify.send(fmt_refusal(getattr(action, "market", "?"), reason))

    @staticmethod
    def _fill_ts(fill: dict, reference_ts: float | None = None) -> float | None:
        raw = fill.get("time", fill.get("timestamp"))
        try:
            value = float(raw)
        except (TypeError, ValueError):
            return None
        if (
            value > 10_000_000_000
            or (
                reference_ts is not None
                and abs(value / 1000 - reference_ts) < abs(value - reference_ts)
            )
        ):
            value /= 1000
        return value if math.isfinite(value) else None

    def _matching_journal_fills(self, journal: dict, fills: list[dict]) -> list[dict]:
        action = journal["action"]
        expected = journal["expected"]
        kind = journal["kind"]
        if kind == "open":
            direction = f"Open {str(action['side']).title()}"
            size = float(expected["size"])
        elif kind == "close":
            fingerprint = journal["pre_state"]["position"]
            direction = f"Close {str(fingerprint['side']).title()}"
            size = float(fingerprint["size"])
        else:
            return []

        market = action["market"]
        size_step = 10 ** (-self.market.info(market).sz_decimals)
        fill_id = journal.get("fill_id")
        submitted = journal.get("submission_ts")
        if submitted is None:
            return []
        lower = float(submitted) - 2
        upper = float(submitted) + 30

        matches = []
        for fill in fills:
            if not isinstance(fill, dict):
                continue
            tid = str(fill.get("tid") or fill.get("hash") or "")
            if fill_id and tid != str(fill_id):
                continue
            if fill.get("coin") != market or fill.get("dir") != direction:
                continue
            try:
                fill_size = float(fill["sz"])
            except (KeyError, TypeError, ValueError):
                continue
            if not math.isfinite(fill_size) or abs(fill_size - size) > size_step:
                continue
            if not fill_id:
                fill_ts = self._fill_ts(fill, float(submitted))
                if fill_ts is None or not (lower <= fill_ts <= upper):
                    continue
            matches.append(fill)
        return matches

    @staticmethod
    def _requested_brackets(orders: list[dict], market: str,
                            stop_px: float, tp_px: float) -> list[dict]:
        matched = []
        kinds = set()
        for order in orders:
            if (
                order.get("coin") != market
                or order.get("reduceOnly") is not True
                or order.get("isTrigger") is not True
            ):
                continue
            order_type = str(order.get("orderType") or "")
            if order_type.startswith("Stop"):
                kind, expected = "sl", stop_px
            elif order_type.startswith("Take Profit"):
                kind, expected = "tp", tp_px
            else:
                continue
            try:
                trigger_px = float(order["triggerPx"])
            except (KeyError, TypeError, ValueError):
                continue
            if math.isclose(trigger_px, expected, rel_tol=0, abs_tol=1e-9):
                kinds.add(kind)
                matched.append(order)
        return matched if kinds == {"sl", "tp"} and len(matched) == 2 else []

    @staticmethod
    def _same_position_identity(position: Position, fingerprint: dict) -> bool:
        return (
            position.id == fingerprint["id"]
            and position.market == fingerprint["market"]
            and position.side == fingerprint["side"]
            and math.isclose(position.entry_px, fingerprint["entry_px"])
            and math.isclose(position.size, fingerprint["size"])
            and math.isclose(position.notional, fingerprint["notional"])
            and math.isclose(position.leverage, fingerprint["leverage"])
            and position.margin_mode == fingerprint["margin_mode"]
        )

    def _recovery_decision_args(self, journal: dict, action) -> dict:
        if journal.get("decision_id") is not None:
            return {"decision_id": int(journal["decision_id"])}
        proposal_id = journal.get("proposal_id")
        if proposal_id is None:
            raise RuntimeError("unfinished autonomous action lacks decision provenance")
        return {"decision": self._chat_decision(action, proposal_id)}

    def _matching_resting_order(self, journal: dict, action: OpenAction,
                                orders: list[dict]) -> Optional[dict]:
        """The venue order this execution placed, if it is still resting.

        The oid is never in the journal: it comes back from place_resting_entry
        and is written to the ledger in the same breath, so a process killed
        between those two lines leaves an order whose id was never recorded
        anywhere. Match it the only way left — market, side, price and size,
        all of which the journal DID record before submitting."""
        expected = journal.get("expected") or {}
        entry_px = self._order_number(expected.get("entry_px"))
        size = self._order_number(expected.get("size"))
        if not expected.get("resting") or entry_px is None or size is None:
            return None
        info = self.market.info(action.market)
        size_step = 10 ** (-info.sz_decimals)
        want_side = "B" if action.side == "long" else "A"
        for order in orders:
            if not isinstance(order, dict) or order.get("coin") != action.market:
                continue
            if order.get("reduceOnly") or order.get("side") != want_side:
                continue
            px = self._order_number(order.get("limitPx"))
            sz = self._order_number(order.get("sz"))
            if px is None or sz is None:
                continue
            if abs(px - entry_px) > max(entry_px * 1e-6, 1e-9):
                continue
            if abs(sz - size) > size_step:
                continue
            return order
        return None

    def _adopt_orphan_resting_entry(self, journal: dict, action: OpenAction,
                                    order: dict) -> None:
        """Re-attach a live venue order to the ledger that never recorded it."""
        expected = journal["expected"]
        oid = self._order_number(order.get("oid"))
        entry_id = self.state.add_pending_entry(
            market=action.market, side=action.side,
            entry_px=float(expected["entry_px"]), size=float(expected["size"]),
            notional=float(expected["notional"]), leverage=float(expected["leverage"]),
            margin_mode=str(expected["margin_mode"]),
            stop_px=float(expected["stop_px"]), tp_px=float(expected["tp_px"]),
            conviction=action.conviction, rationale=action.rationale,
            invalidation=action.invalidation,
            oid=int(oid) if oid is not None else None,
            decision_id=journal.get("decision_id"),
            expires_ts=float(journal["submission_ts"]) + self.cfg.risk.entry_expiry_secs,
            entry_context=self._entry_context(action.market, resting=True),
        )
        self.state.update_action_execution(
            journal["id"], stage="resting", status="executed",
            response_ts=time.time(),
            result={"status": "resting", "market": action.market, "side": action.side,
                    "entry_px": expected["entry_px"], "size": expected["size"],
                    "stop_px": expected["stop_px"], "tp_px": expected["tp_px"],
                    "oid": oid, "pending_entry_id": entry_id,
                    "recovered": "adopted a live venue order the ledger never recorded"},
        )
        self.notify.send(
            f"recovered a resting {action.side} entry on {action.market} @ "
            f"{expected['entry_px']:g} (oid {oid}) — it was live at the venue with no "
            "ledger row; it is managed again and will expire on schedule")

    def _reconcile_open_execution(self, journal: dict, action: OpenAction,
                                  fills: list[dict], orders: list[dict]) -> None:
        matches = self._matching_journal_fills(journal, fills)
        if len(matches) > 1:
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "manual_review",
                "multiple matching entry fills found during recovery",
            )
            return
        if not matches:
            window_closed = time.time() >= float(journal["submission_ts"]) + 30
            if not window_closed:
                return
            # A two-clip fill matches nothing, so 'failed' can be declared while
            # a real position exists — adopted as 'external', which
            # manage_positions skips, leaving it unbracketed AND unmanaged.
            venue = self.state.open_position_for(action.market)
            if venue is not None and venue.side == action.side:
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "needs_reconciliation",
                    f"no single matching fill, but {action.market} IS open at the "
                    "venue — the entry landed in pieces; protection must be "
                    "verified by hand before entries resume",
                )
                return
            # A resting entry has NOT filled — that is its whole point. Declaring
            # it failed while the order sits live at the venue orphans it: nothing
            # expires it, pause cannot cancel it, and if it fills the position is
            # adopted as external, which manage_positions skips. Adopt it instead.
            resting = self._matching_resting_order(journal, action, orders)
            if resting is not None:
                if self.state.resting_entry_for(action.market) is None:
                    self._adopt_orphan_resting_entry(journal, action, resting)
                else:
                    self.state.update_action_execution(
                        journal["id"], stage="resting", status="executed",
                        response_ts=time.time(),
                        result={"status": "resting", "market": action.market,
                                "recovered": "ledger already tracks this entry"})
                return
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "failed",
                "no matching entry fill found in the bounded recovery window",
            )
            return

        fill = matches[0]
        position = self.state.open_position_for(action.market)
        expected = journal["expected"]
        size_step = 10 ** (-self.market.info(action.market).sz_decimals)
        if (
            position is None
            or position.side != action.side
            or abs(position.size - float(expected["size"])) > size_step
        ):
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "manual_review",
                "matching fill found but venue position is absent or different",
            )
            return

        requested = self._requested_brackets(
            orders, action.market, expected["stop_px"], expected["tp_px"]
        )
        if not requested:
            old_trigger_ids = [
                int(order["oid"]) for order in orders
                if order.get("coin") == action.market
                and order.get("isTrigger") is True
                and order.get("oid") is not None
            ]
            try:
                new_orders = self.adapter.place_brackets(
                    action.market, action.side, position.size,
                    expected["stop_px"], expected["tp_px"],
                )
                self.adapter.cancel_orders(action.market, old_trigger_ids)
            except Exception as exc:  # noqa: BLE001 — cleanup must remove exposure
                try:
                    cleanup = self.adapter.close_position_only(position)
                except Exception as cleanup_exc:  # noqa: BLE001
                    self._mark_execution_problem(
                        journal["id"], journal.get("proposal_id"),
                        "needs_reconciliation",
                        f"recovery bracketing failed ({exc!r}); cleanup close "
                        f"ambiguous ({cleanup_exc!r})",
                    )
                    return
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "failed",
                    f"recovery bracketing failed; exposure closed: {exc!r}",
                    cleanup=cleanup,
                )
                return
        else:
            new_orders = requested

        try:
            fill_px = float(fill["px"])
        except (KeyError, TypeError, ValueError):
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "manual_review",
                "matching entry fill has no valid price",
            )
            return
        result = {
            "status": "executed",
            "execution_id": journal["id"],
            "recovered": True,
            "market": action.market,
            "side": action.side,
            "entry_px": fill_px,
            "size": position.size,
            "notional": expected["notional"],
            "leverage": expected["leverage"],
            "margin_mode": position.margin_mode,
            "stop_px": expected["stop_px"],
            "tp_px": expected["tp_px"],
            "brackets": new_orders,
        }
        self.state.finalize_recovered_open_action(
            journal["id"], journal.get("proposal_id"),
            position_id=position.id,
            position={
                "market": action.market,
                "side": action.side,
                "entry_px": fill_px,
                "size": position.size,
                "notional": expected["notional"],
                "leverage": expected["leverage"],
                "margin_mode": position.margin_mode,
                "stop_px": expected["stop_px"],
                "tp_px": expected["tp_px"],
                "conviction": action.conviction,
                "source": action.source,
                "rationale": action.rationale,
                "invalidation": action.invalidation,
            },
            result=result,
            day=utc_day(journal["created_ts"]),
            **self._recovery_decision_args(journal, action),
        )

    def _reconcile_close_execution(self, journal: dict, action: CloseAction,
                                   fills: list[dict]) -> None:
        matches = self._matching_journal_fills(journal, fills)
        if not matches and self.state.open_position_for(action.market) is None:
            # The fingerprinted position is already gone from the (venue-
            # reconciled) ledger and this close produced no fill of its own:
            # a bracket or external fill closed it first (live-observed: SL
            # fired 77s before the analyst's close). Nothing to attribute,
            # nothing to mutate twice -> the close is moot. Terminal
            # 'cancelled' instead of a permanent manual_review that fail-closes
            # every future entry.
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "cancelled",
                "close superseded: position already closed by bracket/external "
                "fill before this close executed",
            )
            self.notify.send(
                f"close {action.market} superseded by bracket/external fill "
                f"(execution {journal['id']} cancelled)"
            )
            return
        if len(matches) != 1:
            if matches:
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "manual_review",
                    "close recovery requires exactly one matching fill",
                )
                return
            # No fill at all. If the venue still shows the SAME position after
            # the settling window, the close provably never executed — that is
            # a clean terminal 'cancelled', not a permanent manual_review that
            # fail-closes every future entry on one flaky HTTP call.
            current = self.state.open_position_for(action.market)
            fingerprint = journal["pre_state"]["position"]
            settled = time.time() >= float(
                journal.get("submission_ts") or journal["created_ts"]
            ) + RECOVERY_WINDOW_SECS
            if (settled and current is not None
                    and self._same_position_identity(current, fingerprint)):
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "cancelled",
                    "close never reached the venue: no fill and the position is "
                    "unchanged after the recovery window — safe to retry",
                )
                self.notify.send(
                    f"close {action.market} did not execute (position unchanged) "
                    f"— execution {journal['id']} cancelled, entries unblocked")
                return
            if settled and journal.get("response_ts"):
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "manual_review",
                    "close recovery requires exactly one matching fill",
                )
            return
        current = self.state.open_position_for(action.market)
        fingerprint = journal["pre_state"]["position"]
        if current is not None:
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "manual_review",
                "close fill matched but the fingerprinted position still exists",
            )
            return
        fill = matches[0]
        try:
            close_px = float(fill["px"])
            realized = float(fill.get("closedPnl") or 0) - float(fill.get("fee") or 0)
        except (KeyError, TypeError, ValueError):
            self._mark_execution_problem(
                journal["id"], journal.get("proposal_id"), "manual_review",
                "matching close fill has malformed price or PnL",
            )
            return
        result = {
            "status": "executed",
            "execution_id": journal["id"],
            "recovered": True,
            "market": action.market,
            "full_size": fingerprint["size"],
            "close_px": close_px,
            "realized_pnl": realized,
        }
        self.state.finalize_recovered_close_action(
            journal["id"], journal.get("proposal_id"),
            position_id=fingerprint["id"],
            close_reason="analyst",
            close_px=close_px,
            realized_pnl=realized,
            result=result,
            **self._recovery_decision_args(journal, action),
        )

    def _reconcile_adjust_execution(self, journal: dict,
                                    action: AdjustStopAction,
                                    orders: list[dict]) -> None:
        position = self.state.open_position_for(action.market)
        fingerprint = journal["pre_state"]["position"]
        expected = journal["expected"]
        requested = self._requested_brackets(
            orders, action.market, expected["new_stop_px"], expected["new_tp_px"]
        )
        if (
            position is None
            or not self._same_position_identity(position, fingerprint)
            or not requested
        ):
            # The old pair still intact and the new pair absent proves the
            # replacement never landed: the position keeps the protection it
            # had, so this is terminal 'cancelled', not a permanent block.
            old_pair = self._requested_brackets(
                orders, action.market, fingerprint.get("stop_px"),
                fingerprint.get("tp_px"),
            ) if position is not None else []
            settled = time.time() >= float(
                journal.get("submission_ts") or journal["created_ts"]
            ) + RECOVERY_WINDOW_SECS
            if (settled and not requested and len(old_pair) == 2
                    and position is not None
                    and self._same_position_identity(position, fingerprint)):
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "cancelled",
                    "bracket replacement never landed; the original stop and "
                    "take-profit are still live — safe to retry",
                )
                self.notify.send(
                    f"adjust {action.market} did not execute (original brackets "
                    f"intact) — execution {journal['id']} cancelled, entries unblocked")
                return
            if settled:
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "manual_review",
                    "could not prove the exact position and requested final bracket pair",
                )
            return
        requested_ids = {int(order["oid"]) for order in requested if order.get("oid") is not None}
        old_ids = {
            int(order["oid"])
            for order in journal["pre_state"].get("old_orders", [])
            if order.get("oid") is not None
        }
        self.adapter.cancel_orders(action.market, sorted(old_ids - requested_ids))
        result = {
            "status": "executed",
            "execution_id": journal["id"],
            "recovered": True,
            "market": action.market,
            "full_size": position.size,
            "stop_px": expected["new_stop_px"],
            "tp_px": expected["new_tp_px"],
            "venue": {"new_orders": requested},
        }
        self.state.finalize_adjust_action(
            journal["id"], journal.get("proposal_id"),
            position_id=position.id,
            stop_px=expected["new_stop_px"],
            tp_px=expected["new_tp_px"],
            result=result,
            **self._recovery_decision_args(journal, action),
        )

    def reconcile_action_executions(self) -> None:
        journals = self.state.unfinished_action_executions()
        if not journals:
            return
        if self.cfg.mode != "live":
            for journal in journals:
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "failed",
                    "paper action interrupted before durable completion",
                )
            return
        fills = self.adapter.fills()
        orders = self.adapter.open_orders_all()
        for journal in journals:
            if journal.get("submission_ts") is None:
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "failed",
                    "action was journaled but never submitted",
                )
                continue
            try:
                action = ACTION_ADAPTER.validate_python(journal["action"])
                if journal["kind"] == "open":
                    self._reconcile_open_execution(journal, action, fills, orders)
                elif journal["kind"] == "close":
                    self._reconcile_close_execution(journal, action, fills)
                elif journal["kind"] == "adjust_stop":
                    self._reconcile_adjust_execution(journal, action, orders)
                else:
                    raise ValueError(f"unknown action kind {journal['kind']!r}")
            except Exception as exc:  # noqa: BLE001 — recovery never mutates twice
                self._mark_execution_problem(
                    journal["id"], journal.get("proposal_id"), "manual_review",
                    f"action recovery failed: {exc!r}",
                )

    # -- reconciliation ----------------------------------------------------
    # -- position management (code-owned, no LLM in the loop) --------------
    def _position_r(self, pos: Position, mark: float) -> Optional[float]:
        """Profit in units of the ORIGINAL risk. None when the initial stop is
        unknown (adopted external positions)."""
        init_stop = self.state.initial_stop(pos.id)
        if init_stop is None or init_stop <= 0 or pos.entry_px <= 0:
            return None
        risk = abs(pos.entry_px - init_stop)
        if risk <= 0:
            return None
        move = (mark - pos.entry_px) if pos.side == "long" else (pos.entry_px - mark)
        return move / risk

    def manage_positions(self, marks: dict) -> bool:
        """Two code-owned rules, applied before the analyst ever sees the book.
        Returns True when it changed the book, so the caller can re-read state.

        breakeven — once a trade is `breakeven_at_r` R in front, its stop moves
        to entry plus the round-trip fee, so a winner can no longer become a
        loser. time stop — a position that has not reached `time_stop_min_r`
        after `time_stop_secs` is dead money holding the one concurrency slot;
        close it and free the slot.
        """
        cfg = self.cfg.risk
        if cfg.breakeven_at_r <= 0 and cfg.time_stop_secs <= 0:
            return False
        changed = False
        now = time.time()
        for pos in self.state.open_positions():
            if pos.source == "external":
                continue
            mark = marks.get(pos.market)
            if mark is None or not math.isfinite(mark) or mark <= 0:
                continue
            r = self._position_r(pos, mark)
            if r is None:
                continue
            if (cfg.time_stop_secs > 0 and now - pos.opened_ts >= cfg.time_stop_secs
                    and r < cfg.time_stop_min_r):
                age_m = int((now - pos.opened_ts) / 60)
                action = CloseAction(
                    market=pos.market,
                    rationale=f"time stop: {age_m}m old and only {r:+.2f}R — the "
                              "thesis did not play, the slot is worth more than the hope")
                try:
                    decision_id = self.state.record_decision(
                        "time stop",
                        f"{pos.market} {pos.side} sat {age_m}m at {r:+.2f}R without "
                        f"reaching +{cfg.time_stop_min_r:g}R — closing to free the slot",
                        dumps_actions([action]), "engine", 0, "ok",
                        reasoning=f"code-owned rule: time_stop_secs={cfg.time_stop_secs}, "
                                  f"time_stop_min_r={cfg.time_stop_min_r:g}")
                    self.exec_close(
                        action, marks, self._account_snapshot_equity(), utc_day(now),
                        frozenset(), origin="time_stop", decision_id=decision_id)
                    changed = True
                except Exception as exc:  # noqa: BLE001 — one stuck position never kills the cycle
                    self.notify.send(f"time stop failed on {pos.market}: {exc!r}")
                continue
            mi = self.market.info(pos.market)
            target = None
            how = ""

            # -- trailing: follow the high-water mark ------------------------
            # 2026-08-31: the xyz:CL long peaked at +8.5% on margin 14 minutes
            # after filling and gave 80% of it back inside the hour, while its
            # take-profit sat 4.56% away — roughly 9x ATR15m. The RR>=2 floor
            # against a 2% minimum stop forces targets that far out, so on
            # anything but a runaway move the exit has to come from the stop
            # following the price, not from the target being reached.
            if cfg.trail_start_r > 0 and r >= cfg.trail_start_r:
                peak = self.state.update_peak(pos.id, mark)
                atr_pct = (self._features_cache.get(pos.market) or {}).get("atr15m_pct")
                if isinstance(atr_pct, (int, float)) and atr_pct > 0:
                    # the trail must clear this market's own noise, or it is a
                    # coin-flip exit dressed up as risk management
                    band = peak * (atr_pct / 100.0) * cfg.trail_atr_mult
                    trailed = peak - band if pos.side == "long" else peak + band
                    target = format_price(trailed, mi.sz_decimals)
                    how = f"trailing {cfg.trail_atr_mult:g}xATR behind {peak:g}"

            # -- breakeven: a winner must not become a loser ------------------
            if target is None:
                if cfg.breakeven_at_r <= 0 or r < cfg.breakeven_at_r:
                    continue
                buffer = 1 + 2 * FEE_RATE
                target = (pos.entry_px * buffer if pos.side == "long"
                          else pos.entry_px / buffer)
                target = format_price(target, mi.sz_decimals)
                how = "breakeven (entry plus the round trip in fees)"
            better = (pos.stop_px is None
                      or (pos.side == "long" and target > pos.stop_px)
                      or (pos.side == "short" and target < pos.stop_px))
            if not better:
                continue
            wrong_side = ((pos.side == "long" and target >= mark)
                          or (pos.side == "short" and target <= mark))
            if wrong_side or pos.tp_px is None:
                continue
            try:
                with self._adapter_lock:
                    self.adapter.adjust_stop(pos, target, pos.tp_px)
                self.state.update_brackets(pos.id, target, pos.tp_px)
            except Exception as exc:  # noqa: BLE001 — protection stays as it was
                self.notify.send(f"stop move failed on {pos.market} ({how}): {exc!r}")
                continue
            changed = True
            locked = ((target - pos.entry_px) if pos.side == "long"
                      else (pos.entry_px - target)) * pos.size
            # `better` deliberately admits a position whose venue stop vanished
            # (stop_px is None). Formatting it with :g raised TypeError here —
            # OUTSIDE the try above — so the stop moved, then the exception
            # propagated out of context_snapshot and the whole cycle was logged
            # as "context DOWN" and skipped: no analyst, no time stop, nothing.
            was = f"{pos.stop_px:g}" if pos.stop_px is not None else "none"
            self.notify.send(
                f"{how}: {pos.market} {pos.side} at {r:+.2f}R — stop "
                f"{was} -> {target:g}, ${locked:+.2f} locked in")
        return changed

    def _account_snapshot_equity(self) -> float:
        snapshot = self._account_snapshot or {}
        equity = snapshot.get("equity")
        return float(equity) if isinstance(equity, (int, float)) else 0.0

    # -- resting entries ---------------------------------------------------
    def reconcile_pending_entries(self, marks: dict, orders: list[dict]) -> bool:
        """Settle every resting entry against authoritative venue state.

        Returns True when the venue order book was changed (caller re-reads it).
        A filled maker entry already carries its brackets (grouping normalTpsl),
        so the only work here is bringing the ledger into line.
        """
        resting = self.state.resting_entries()
        if not resting:
            return False
        now = time.time()
        live_oids = {self._order_number(o.get("oid")) for o in orders}
        venue_positions = {p["market"]: p for p in (self._account_snapshot or {}).get(
            "positions", []) if isinstance(p, dict) and p.get("market")}
        changed = False
        for entry in resting:
            market, side = entry["market"], entry["side"]
            ledger = self.state.open_position_for(market)
            venue = venue_positions.get(market)
            filled_dry = (self.cfg.mode != "live" and self._dry_entry_filled(entry, marks))
            if ledger is not None and ledger.side == side:
                if ledger.source == "external":
                    # adopt_external runs first every live cycle, so this is the
                    # branch a filled resting entry actually takes: it must both
                    # claim the attribution and consume a daily entry slot
                    self.state.claim_position(
                        ledger.id, conviction=entry["conviction"],
                        rationale=entry["rationale"], invalidation=entry["invalidation"],
                        stop_px=entry["stop_px"], tp_px=entry["tp_px"],
                        entry_context=json.loads(entry.get("entry_context_json") or "{}"))
                    self.state.count_entry(utc_day(now))
                self.state.settle_pending_entry(entry["id"], "filled")
                changed |= self._cancel_entry_remainder(entry, live_oids)
                self.notify.send(
                    f"resting entry FILLED {side} {market} @ {ledger.entry_px:g}")
                continue
            if venue is not None and venue.get("side") == side:
                position = self.state.add_position(
                    market, side, float(venue["entry_px"]), float(venue["size"]),
                    float(venue["entry_px"]) * float(venue["size"]),
                    float(venue.get("leverage") or entry["leverage"]),
                    entry["stop_px"], entry["tp_px"], entry["conviction"], "own",
                    rationale=entry["rationale"], invalidation=entry["invalidation"],
                    margin_mode=str(venue.get("margin_mode") or entry["margin_mode"]),
                    entry_context=json.loads(entry.get("entry_context_json") or "{}"))
                self.state.count_entry(utc_day(now))
                self.state.settle_pending_entry(entry["id"], "filled")
                changed |= self._cancel_entry_remainder(entry, live_oids)
                self.notify.send(
                    f"resting entry FILLED {side} {market} @ {position.entry_px:g} "
                    f"size {position.size:g} (brackets armed at the venue)")
                continue
            if filled_dry:
                px = float(entry["entry_px"])
                self.state.add_position(
                    market, side, px, float(entry["size"]), float(entry["notional"]),
                    float(entry["leverage"]), entry["stop_px"], entry["tp_px"],
                    entry["conviction"], "own", rationale=entry["rationale"],
                    invalidation=entry["invalidation"],
                    margin_mode=str(entry["margin_mode"]),
                    entry_context=json.loads(entry.get("entry_context_json") or "{}"))
                self.state.count_entry(utc_day(now))
                self.state.settle_pending_entry(entry["id"], "filled")
                # the venue removes a filled order itself; the paper book does not
                changed |= self._cancel_entry_remainder(entry, live_oids)
                self.notify.send(f"resting entry FILLED (paper) {side} {market} @ {px:g}")
                continue
            oid = self._order_number(entry.get("oid"))
            expired = now >= float(entry["expires_ts"])
            still_open = oid is not None and oid in live_oids
            if expired or not still_open:
                if still_open:
                    try:
                        with self._adapter_lock:
                            self.adapter.cancel_orders(market, [int(oid)])
                        changed = True
                    except Exception as exc:  # noqa: BLE001 — settle anyway, report loudly
                        self.notify.send(
                            f"could not cancel expired entry on {market}: {exc!r}")
                try:
                    with self._adapter_lock:
                        changed |= self._clear_orphan_brackets(market)
                except Exception as exc:  # noqa: BLE001
                    self.notify.send(
                        f"could not clear orphan brackets on {market}: {exc!r}")
                outcome = "expired" if expired else "vanished"
                self.state.settle_pending_entry(entry["id"], outcome)
                self.notify.send(
                    f"resting entry {outcome} {side} {market} @ {entry['entry_px']:g} "
                    f"— never filled, no exposure taken")
        return changed

    def _clear_orphan_brackets(self, market: str) -> bool:
        """Cancel leftover triggers ONLY when nothing is open on that market.

        cancel_brackets is market-wide. Calling it while a position exists — a
        resting entry that partially filled, say — strips that live position's
        stop and take-profit, the exact opposite of what an operator pressing
        Pause is asking for."""
        if self.cfg.mode != "live":
            return False
        if self.state.open_position_for(market) is not None:
            return False
        self.adapter.cancel_brackets(market)
        return True

    def _cancel_entry_remainder(self, entry: dict, live_oids: set) -> bool:
        """A partial fill leaves the rest of the maker order resting; the risk
        was sized once, so the remainder is cancelled rather than allowed to
        grow the position."""
        oid = self._order_number(entry.get("oid"))
        if oid is None or oid not in live_oids:
            return False
        try:
            with self._adapter_lock:
                self.adapter.cancel_orders(entry["market"], [int(oid)])
            return True
        except Exception as exc:  # noqa: BLE001
            self.notify.send(
                f"could not cancel the unfilled remainder on {entry['market']}: {exc!r}")
            return False

    @staticmethod
    def _dry_entry_filled(entry: dict, marks: dict) -> bool:
        mark = marks.get(entry["market"])
        if mark is None:
            return False
        return (mark <= float(entry["entry_px"]) if entry["side"] == "long"
                else mark >= float(entry["entry_px"]))

    def reconcile(self, marks: dict) -> None:
        with self._adapter_lock:
            if self.cfg.mode == "live":
                self.reconcile_live()
                # adopt positions opened outside the engine SINCE boot (manual /
                # UI trades) so the brain always sees the whole account, not just
                # what existed at startup
                for market_name in self.adopt_external():
                    self.notify.send(f"adopted external position: {market_name}")
                self.reconcile_action_executions()
            else:
                self.reconcile_dry(marks)
                self.reconcile_action_executions()

    def _close_already_recorded(self, coin: str, c: dict) -> bool:
        """Is this close fill the tail of a close the ledger already holds?

        exec_close records from the adapter response and cannot know the fill's
        tid, so the venue fill always arrives afterwards, unseen and with no
        open position to match. Without this guard the orphan reconstruction
        turns every analyst-initiated close into a second, phantom trade."""
        for row in self.state.recent_closes(12):
            if row["market"] != coin or row["close_px"] in (None, 0):
                continue
            if time.time() - (row["closed_ts"] or 0) > CLOSE_ECHO_SECS:
                continue
            if abs(row["close_px"] - c["px"]) / c["px"] > 0.002:
                continue
            if row["size"] and abs(row["size"] - c["sz"]) / c["sz"] > 0.01:
                continue
            return True
        return False

    @staticmethod
    def _close_reason(pos: Position, c: dict) -> str:
        """Which bracket ended this, judged by SIDE rather than by price
        proximity.

        A 1% proximity test mislabels any stop that slipped further than that as
        'external' — and the triggered market close is allowed to fill up to 8%
        through the trigger. That downgrade cost the 4h stop cooldown on exactly
        the violent moves it exists for."""
        px = c["px"]
        if pos.side == "long":
            if pos.stop_px and px <= pos.stop_px * (1 + 1e-4):
                return "sl"
            if pos.tp_px and px >= pos.tp_px * (1 - 1e-4):
                return "tp"
        else:
            if pos.stop_px and px >= pos.stop_px * (1 - 1e-4):
                return "sl"
            if pos.tp_px and px <= pos.tp_px * (1 + 1e-4):
                return "tp"
        return "external"

    def _synthesize_orphan_close(self, coin: str, c: dict):
        """Rebuild the position a close fill implies, when the round trip
        happened entirely between two reconciles.

        HL gives closedPnl gross, so the entry price is recoverable exactly:
        long  entry = close_px - pnl/size ; short entry = close_px + pnl/size.
        The side comes from the pending entry we placed, or from the venue
        direction recorded on the fill."""
        if self._close_already_recorded(coin, c):
            # The engine closed this itself moments ago and recorded it from the
            # adapter response; the venue fill is the SAME round trip arriving
            # late. Reconstructing it would book the trade twice.
            return None
        side = c.get("side")
        entry = self.state.resting_entry_for(coin)
        if side is None and entry is not None:
            side = entry["side"]
        if side not in ("long", "short") or c["sz"] <= 0 or c["px"] <= 0:
            return None
        entry_px = (c["px"] - c["pnl"] / c["sz"] if side == "long"
                    else c["px"] + c["pnl"] / c["sz"])
        if not math.isfinite(entry_px) or entry_px <= 0:
            return None
        position = self.state.add_position(
            coin, side, entry_px, c["sz"], entry_px * c["sz"],
            float(entry["leverage"]) if entry else 1.0,
            entry["stop_px"] if entry else None,
            entry["tp_px"] if entry else None,
            entry["conviction"] if entry else None,
            "own" if entry else "external",
            rationale=(entry["rationale"] if entry else
                       "reconstructed from a venue close fill with no open ledger row"),
            invalidation=entry["invalidation"] if entry else None,
            margin_mode=str(entry["margin_mode"]) if entry else "unknown",
            entry_context=(json.loads(entry.get("entry_context_json") or "{}")
                           if entry else None),
        )
        if entry is not None:
            self.state.settle_pending_entry(entry["id"], "filled")
            self.state.count_entry(utc_day(time.time()))
        self.notify.send(
            f"reconstructed {side} {coin}: opened and closed between reconciles "
            f"@ {entry_px:g} -> {c['px']:g}")
        return position

    def reconcile_dry(self, marks: dict) -> None:
        """Paper brackets: the engine IS the venue. Trigger fills at bracket px."""
        for pos in self.state.open_positions():
            mark = marks.get(pos.market)
            if mark is None:
                continue
            hit = bracket_hit(pos.side, mark, pos.stop_px, pos.tp_px)
            if hit is None:
                continue
            px = pos.stop_px if hit == "sl" else pos.tp_px
            pnl = realized_pnl(pos.side, pos.entry_px, px, pos.size)
            self.state.close_position(pos.id, hit, px, pnl)
            self.guard.cooldown_after_close(pos.market, hit, pnl=pnl)
            self.notify.send(fmt_close(pos.market, hit, px, pnl))

    def reconcile_live(self) -> None:
        """Detect bracket/external closes via userFills; record realized PnL
        (closedPnl minus fees), apply cooldowns."""
        fills = self.adapter.fills()
        self._recent_venue_fills = [
            {
                key: fill.get(key)
                for key in (
                    "tid", "hash", "coin", "dir", "px", "sz",
                    "closedPnl", "fee", "time",
                )
                if fill.get(key) is not None
            }
            for fill in fills[:50]
            if isinstance(fill, dict)
        ]
        self._recent_venue_history = self._venue_close_history(
            fills, require_timestamps=False)
        validated = []
        for fill in fills:
            if not isinstance(fill, dict):
                raise ContextError(f"fill is malformed: {fill!r}")
            tid = str(fill.get("tid") or fill.get("hash") or "")
            direction = fill.get("dir")
            if not tid or not isinstance(direction, str) or not direction:
                raise ContextError(f"fill is malformed: {fill!r}")
            if self.state.fill_seen(tid):
                continue
            close = None
            opening = None
            if direction.startswith("Close"):
                coin = fill.get("coin")
                try:
                    size = float(fill["sz"])
                    pnl = float(fill.get("closedPnl") or 0)
                    fee = float(fill.get("fee") or 0)
                    px = float(fill["px"])
                except (KeyError, TypeError, ValueError) as e:
                    raise ContextError(f"fill is malformed: {fill!r}") from e
                if (not isinstance(coin, str) or not coin
                        or not all(math.isfinite(v) for v in (size, pnl, fee, px))
                        or size <= 0 or px <= 0):
                    raise ContextError(f"fill is malformed: {fill!r}")
                close = {"coin": coin, "sz": size, "pnl": pnl, "fee": fee, "px": px,
                         "side": ("long" if direction.endswith("Long")
                                  else "short" if direction.endswith("Short") else None)}
            else:
                coin = fill.get("coin")
                fee = self._order_number(fill.get("fee"))
                ts_ms = self._order_number(fill.get("time"))
                if isinstance(coin, str) and coin and fee is not None:
                    opening = {"coin": coin, "fee": fee, "ts": (ts_ms or 0) / 1000}
            validated.append((tid, close, opening))

        closes: dict[str, dict] = {}
        non_close_tids = []
        unclaimed_open_fees: dict[str, list[tuple[str, float]]] = {}
        for tid, fill, opening in validated:
            if fill is None:
                # what it cost to get IN belongs to the position, not to nothing:
                # HL charges it on the opening fill and reports closedPnl gross
                if opening is not None:
                    pos = self.state.open_position_for(opening["coin"])
                    if pos is not None:
                        # fee and dedupe commit together: a crash between them
                        # used to re-add the same fee on the next pass
                        self.state.attribute_entry_fee(pos.id, opening["fee"], tid)
                        continue
                    # A round trip that opened AND closed between two reconciles
                    # has both fills in THIS batch. Its opening fee has nowhere
                    # to go yet, so hold it: the close loop below reconstructs
                    # the position and claims it. Without this the reconstructed
                    # trade was booked gross of what it cost to get in, which
                    # inflated the measured record the analyst learns from.
                    unclaimed_open_fees.setdefault(opening["coin"], []).append(
                        (tid, opening["fee"]))
                    if (time.time() - opening["ts"] < ENTRY_FEE_GRACE_SECS
                          or self.state.resting_entry_for(opening["coin"]) is not None):
                        # the ledger row does not exist YET: a resting entry
                        # fills before reconcile_pending_entries creates it, and
                        # after downtime that entry can be hours old. Leave the
                        # fill unseen and claim the fee once the row appears.
                        continue
                non_close_tids.append(tid)
                continue
            coin = fill["coin"]
            c = closes.setdefault(
                coin, {"sz": 0.0, "pnl": 0.0, "fee": 0.0, "px": 0.0, "tids": [],
                       "side": fill.get("side")}
            )
            c["sz"] += fill["sz"]
            c["pnl"] += fill["pnl"]
            c["fee"] += fill["fee"]
            c["px"] = fill["px"]
            c["tids"].append(tid)
        for tid in non_close_tids:
            self.state.mark_fill(tid)
        for coin, c in closes.items():
            pos = self.state.open_position_for(coin)
            if pos is None:
                # A round trip that opened AND closed between two reconciles has
                # no ledger row to attribute to. Discarding it lost the realized
                # PnL, kept it out of the measured record, and — the money part —
                # skipped cooldown_after_close, so the analyst could immediately
                # re-enter the market that had just stopped it out. Reconstruct
                # the trade from the fill instead.
                pos = self._synthesize_orphan_close(coin, c)
                if pos is None:
                    for tid in c["tids"]:
                        self.state.mark_fill(tid)
                    continue
                for open_tid, open_fee in unclaimed_open_fees.pop(coin, []):
                    # what it cost to get in. HL reports closedPnl GROSS, so
                    # without this the round trip reads better than it was.
                    self.state.attribute_entry_fee(pos.id, open_fee, open_tid)
            entry_fee = self.state.entry_fee(pos.id)
            if c["sz"] < pos.size * 0.999:
                share = min(1.0, c["sz"] / pos.size) if pos.size > 0 else 0.0
                partial_entry_fee = entry_fee * share
                pnl = c["pnl"] - c["fee"] - partial_entry_fee
                reason = self._close_reason(pos, c)
                # split take-profits close in tranches; recording only the final
                # one hid every earlier tranche's profit from realized_total()
                # and from the measured record the analyst reads
                self.state.record_partial_close(pos, c["sz"], c["px"], pnl, reason)
                self.state.update_size(pos.id, pos.size - c["sz"])
                self.state.add_entry_fee(pos.id, -partial_entry_fee)
                for tid in c["tids"]:
                    self.state.mark_fill(tid)
                self.notify.send(
                    f"partial close {coin} ({reason}): -{c['sz']} "
                    f"(pnl ${pnl:+.2f} net of both sides)")
                continue
            reason = self._close_reason(pos, c)
            pnl = c["pnl"] - c["fee"] - entry_fee
            self.adapter.cancel_brackets(coin)
            self.state.close_position(pos.id, reason, c["px"], pnl)
            self.guard.cooldown_after_close(coin, reason, pnl=pnl)
            for tid in c["tids"]:
                self.state.mark_fill(tid)
            self.notify.send(fmt_close(coin, reason, c["px"], pnl))


def summarize_decision(actions) -> str:
    return json.dumps([a.model_dump() for a in actions])[:400]
