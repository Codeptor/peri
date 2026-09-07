import json

import pytest

from peri.analyst import AnalystError, AnalystResult, ChatResult
from peri.config import (AnalystCfg, Config, NewsCfg, NotifyCfg, RiskCfg,
                         TelegramCfg, UniverseCfg)
from peri.engine import ContextError, Engine, ProposalError, utc_day
from peri.market import Ctx
from peri.models import ChatResponse, CloseAction, Decision, OpenAction
from peri.risk import Guard
from peri.router import DryRunAdapter
from peri.state import State

import asyncio
import time


def cfg(mode="dry"):
    return Config(
        mode=mode, hl_network="testnet", route_builder_fee=True,
        telegram=TelegramCfg(-100, ["caller1"], 0),
        analyst=AnalystCfg(900, 60, 2, 0.2, 4000, 0.75),
        universe=UniverseCfg(["BTC", "SOL"], ["xyz"], 1_000_000, 12),
        risk=RiskCfg(1.5, 20.0, 3, 6, 15.0, 2.0, 900, 3600, 14400, 10.0, 5.0, 1000.0),
        news=NewsCfg([], 12),
        notify=NotifyCfg(0),
    )


class FakeMarket:
    def __init__(self):
        self._ctxs = {
            "BTC": Ctx("BTC", 80000.0, -0.3, 11.0, 3e9, 3e9),
            "SOL": Ctx("SOL", 100.0, 3.0, 11.0, 1e8, 5e8),
            "ETH": Ctx("ETH", 200.0, 1.5, 8.0, 2e8, 6e8),
            "xyz:NVDA": Ctx("xyz:NVDA", 218.0, 2.0, 5.5, 1.3e8, 3e8),
        }

    def ctxs(self):
        return dict(self._ctxs)

    def known(self, name):
        return name in self._ctxs

    class _MI:
        sz_decimals = 2
        max_leverage = 20

    def info(self, name):
        return self._MI()

    def affordable(self, name, mark, max_notional):
        return 10 ** -self.info(name).sz_decimals * mark <= max_notional

    def candidates(self, allow, floor, top, must_include):
        out = [a for a in allow if a in self._ctxs]
        out += [m for m in must_include if m in self._ctxs and m not in out]
        return out

    def features(self, name, ctx):
        return {"mark": ctx.mark, "day_pct": ctx.day_pct,
                "funding_apr_pct": ctx.funding_apr_pct,
                "oi_usd": ctx.oi_usd, "vol_usd": ctx.vol_usd,
                "r_1h_pct": 0.1, "r_4h_pct": 0.2, "atr15m_pct": 0.4,
                "range24h_pos": 0.5,
                "hi_24h": ctx.mark * 1.05, "lo_24h": ctx.mark * 0.95}


class ScriptedAnalyst:
    model = "scripted"

    def __init__(self, decisions):
        self.decisions = list(decisions)
        self.bundles = []

    def decide(self, bundle):
        self.bundles.append(bundle)
        d = self.decisions.pop(0)
        if isinstance(d, Exception):
            raise d
        return AnalystResult(decision=Decision.model_validate(d), latency_ms=5,
                             raw=json.dumps(d))


class Quiet:
    def __init__(self):
        self.lines = []

    def send(self, text):
        self.lines.append(text)


class OrdersDownAdapter(DryRunAdapter):
    def open_orders_all(self):
        raise RuntimeError("venue orders unavailable")


class FillsDownAdapter(DryRunAdapter):
    def fills(self):
        raise RuntimeError("venue fills unavailable")


class StaticOrdersAdapter(DryRunAdapter):
    def __init__(self, state, bankroll, orders):
        super().__init__(state, bankroll)
        self._orders = orders

    def open_orders_all(self):
        return self._orders


OPEN_SOL = {"actions": [{"kind": "open", "market": "SOL", "side": "long",
                         "conviction": 0.8, "stop": 97.0, "take_profit": 107.0,
                         "leverage": 10, "margin_mode": "isolated",
                         "source": "own", "rationale": "momo",
                         "invalidation": "loses 97"}]}


def mk(tmp_path, decisions, mode="dry"):
    c = cfg(mode)
    state = State(str(tmp_path / "t.db"))
    market = FakeMarket()
    analyst = ScriptedAnalyst(decisions)
    guard = Guard(c.risk, state, c.analyst.conviction_min)
    adapter = DryRunAdapter(state, c.risk.paper_bankroll)
    notifier = Quiet()
    eng = Engine(c, state, market, analyst, guard, adapter, notifier,
                 asyncio.Event())
    # the suite never touches the network: Trench's feeds are stubbed off unless
    # a test deliberately supplies them
    eng.fetch_cohort_bias = lambda: None
    eng.fetch_economic_calendar = lambda: []
    eng.fetch_many_asset_bias = lambda coins: {}
    return eng, state, notifier, analyst


def install_chat(eng, proposal=None, answer="Grounded live answer."):
    captured = {}

    class ChatAnalyst:
        model = "qwen"

        def chat(self, bundle, history, message):
            captured.update(bundle=bundle, history=history, message=message)
            return ChatResult(
                response=ChatResponse.model_validate({
                    "answer": answer,
                    "proposal": proposal,
                }),
                latency_ms=7,
                raw="{}",
                tool_log=[],
            )

    eng.analyst = ChatAnalyst()
    return captured


def test_run_starts_immediately_and_keeps_start_to_start_cadence(tmp_path, monkeypatch):
    eng, _, _, _ = mk(tmp_path, [])
    events = []
    monotonic = iter((100.0, 100.0, 280.0, 1000.0))

    async def fake_to_thread(func, *args):
        if func.__name__ == "startup":
            events.append("startup")
            return
        events.append(("cycle", args[0]))
        if args[0] == "scheduled":
            raise asyncio.CancelledError

    async def fake_wait_for(awaitable, timeout):
        awaitable.close()
        events.append(("wait", timeout))
        raise asyncio.TimeoutError

    monkeypatch.setattr("peri.engine.asyncio.to_thread", fake_to_thread)
    monkeypatch.setattr("peri.engine.asyncio.wait_for", fake_wait_for)
    monkeypatch.setattr("peri.engine.monotonic", lambda: next(monotonic))

    with pytest.raises(asyncio.CancelledError):
        asyncio.run(eng.run())

    assert events == [
        "startup",
        ("cycle", "startup"),
        ("wait", pytest.approx(720.0)),
        ("cycle", "scheduled"),
    ]


def test_manual_wake_sets_one_labeled_cycle(tmp_path):
    eng, _, _, _ = mk(tmp_path, [])

    assert eng.request_wake() is False
    assert eng.wake.is_set()
    assert eng._wake_trigger == "manual dashboard"
    assert eng.request_wake("replacement") is True
    assert eng._wake_trigger == "manual dashboard"


def test_dashboard_snapshot_attributes_live_brackets_and_venue_closes(tmp_path):
    eng, state, _, _ = mk(tmp_path, [], mode="live")
    state.add_position("xyz:KIOXIA", "short", 333.76, 0.117, 39.0, 3.0,
                       328.8, 308.0, None, "external", "operator short", "breakout")
    state.record_decision(
        "caller message", "memory fade",
        json.dumps([{
            "kind": "adjust_stop", "market": "xyz:KIOXIA", "stop": 328.8,
            "take_profit": 308.0, "rationale": "trail",
        }]),
        "qwen", 10, "ok",
    )
    decision = state.recent_decisions(1)[0]
    placed_ms = int((decision["ts"] + 3) * 1000)
    closed_ms = placed_ms - 60_000
    state.mark_fill("seeded-close")

    class LiveDashboardAdapter(DryRunAdapter):
        def account_snapshot(self):
            return {
                "equity": 64.31, "held_collateral": 42.57,
                "available_margin": 21.74, "total_margin_used": 42.57,
                "positions": [{
                    "market": "xyz:KIOXIA", "side": "short", "size": 0.117,
                    "entry_px": 333.76, "leverage": 3.0, "margin": 14.5,
                    "position_value": 37.56, "upnl": 1.2,
                    "liquidation_px": 423.0, "roe": 0.08,
                }],
            }

        def open_orders_all(self):
            return [
                {"coin": "xyz:KIOXIA", "orderType": "Stop Market", "side": "B",
                 "sz": "0.117", "limitPx": "330.4", "triggerPx": "328.8",
                 "reduceOnly": True, "isTrigger": True, "oid": 101,
                 "timestamp": placed_ms},
                {"coin": "xyz:BTC", "orderType": "Limit", "side": "B", "sz": "0.01",
                 "limitPx": "78000", "triggerPx": "0", "reduceOnly": False,
                 "isTrigger": False, "oid": 202, "timestamp": placed_ms},
            ]

        def fills(self):
            return [
                {"tid": "seeded-close", "coin": "xyz:NVDA", "dir": "Close Long",
                 "sz": "0.26", "px": "232.5", "closedPnl": "2.0956", "fee": "0.024",
                 "time": closed_ms},
                {"tid": "open-fill", "coin": "SOL", "dir": "Open Long", "sz": "0.6",
                 "px": "105.41", "closedPnl": "0", "fee": "0.047", "time": placed_ms},
            ]

    eng.adapter = LiveDashboardAdapter(state, 1000.0)

    snapshot = eng.dashboard_snapshot()

    assert snapshot["account"]["available_margin"] == 21.74
    assert snapshot["positions"][0]["upnl"] == 1.2
    assert snapshot["positions"][0]["source"] == "external"
    assert snapshot["positions"][0]["rationale"] == "operator short"
    stop, entry = snapshot["orders"]
    assert stop["role"] == "stop_loss"
    assert stop["placed_by"] == "Qwen via Peri"
    assert stop["decision_id"] == decision["id"]
    assert stop["position_source"] == "external"
    assert stop["placed_ts"] == pytest.approx(placed_ms / 1000)
    assert entry["role"] == "entry"
    assert entry["placed_by"] == "External / manual"
    assert entry["decision_id"] is None
    assert snapshot["realized"] == {
        "total": pytest.approx(2.0716),
        "close_count": 1,
        "scope": "recent venue fills",
    }
    assert snapshot["closes"] == [{
        "id": "seeded-close", "market": "xyz:NVDA", "side": "long",
        "size": 0.26, "entry_px": pytest.approx(224.44), "close_px": 232.5,
        "gross_pnl": 2.0956,
        "fee": 0.024, "entry_fee": 0.0, "exit_fee": 0.024,
        "realized_pnl": pytest.approx(2.0716),
        "close_reason": "venue", "conviction": None,
        "closed_ts": pytest.approx(closed_ms / 1000),
    }]


def test_dashboard_empty_venue_positions_do_not_fall_back_to_stale_ledger(tmp_path):
    eng, state, _, _ = mk(tmp_path, [], mode="live")
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 3.0,
                       97.0, 107.0, 0.8, "own")

    class FlatVenueAdapter(DryRunAdapter):
        def account_snapshot(self):
            snapshot = super().account_snapshot({})
            snapshot["positions"] = []
            return snapshot

        def open_orders_all(self):
            return []

        def fills(self):
            return []

    eng.adapter = FlatVenueAdapter(state, 1000.0)

    assert eng.dashboard_snapshot()["positions"] == []


def test_dashboard_snapshot_coalesces_reads_and_survives_rate_limits(
    tmp_path, monkeypatch
):
    eng, state, _, _ = mk(tmp_path, [], mode="live")

    class FlakyDashboardAdapter(DryRunAdapter):
        def __init__(self):
            super().__init__(state, 1000.0)
            self.account_calls = 0

        def account_snapshot(self):
            self.account_calls += 1
            if self.account_calls == 2:
                raise RuntimeError("venue 429")
            snapshot = super().account_snapshot({})
            snapshot["equity"] = 1000.0 + self.account_calls
            return snapshot

        def open_orders_all(self):
            return []

        def fills(self):
            return []

    adapter = FlakyDashboardAdapter()
    eng.adapter = adapter
    ticks = iter((100.0, 101.0, 106.0, 107.0, 112.0))
    monkeypatch.setattr("peri.engine.monotonic", lambda: next(ticks))

    fresh = eng.dashboard_snapshot()
    cached = eng.dashboard_snapshot()
    stale = eng.dashboard_snapshot()
    backed_off = eng.dashboard_snapshot()
    recovered = eng.dashboard_snapshot()

    assert adapter.account_calls == 3
    assert fresh["stale"] is False and fresh["stale_reason"] is None
    assert cached["as_of_ts"] == fresh["as_of_ts"]
    assert stale["stale"] is True and "venue 429" in stale["stale_reason"]
    assert backed_off["as_of_ts"] == fresh["as_of_ts"]
    assert backed_off["stale"] is True
    assert recovered["stale"] is False
    assert recovered["account"]["equity"] == 1003.0


def test_cycle_runtime_status_exposes_analyst_phase_then_returns_idle(tmp_path):
    eng, _, _, _ = mk(tmp_path, [])
    observed = []

    class InspectingAnalyst:
        model = "qwen"

        def decide(self, bundle):
            observed.append(eng.runtime_status())
            decision = Decision.model_validate({"actions": []})
            return AnalystResult(decision=decision, latency_ms=5, raw="{}")

    eng.analyst = InspectingAnalyst()

    eng.cycle("manual dashboard")

    assert observed[0]["phase"] == "analyst"
    assert observed[0]["trigger"] == "manual dashboard"
    assert observed[0]["started_ts"] is not None
    assert observed[0]["snapshot_ts"] is not None
    finished = eng.runtime_status()
    assert finished["phase"] == "idle"
    assert finished["last_trigger"] == "manual dashboard"
    assert finished["last_finished_ts"] is not None
    # per-phase wall clock so slow cycles can be attributed (context vs analyst)
    timings = finished["last_timings"]
    assert {"ctxs", "reconcile", "account", "orders", "bundle", "analyst", "total"} <= set(timings)
    assert all(isinstance(v, float) and v >= 0 for v in timings.values())


def test_open_action_executes_and_records(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [OPEN_SOL])
    eng.cycle("scheduled")
    pos = state.open_position_for("SOL")
    assert pos is not None and pos.side == "long"
    assert pos.margin_mode == "isolated"
    assert pos.stop_px == 97.0 and pos.tp_px == 107.0
    # risk $15 / 3% stop dist -> notional ~$500 at entry with slip
    assert pos.notional == pytest.approx(500.0, rel=0.01)
    day = utc_day(time.time())
    assert state.entries_today(day) == 1
    assert any("OPEN SOL" in line for line in notes.lines)
    # bundle carried candidates + account
    b = analyst.bundles[0]
    assert b["account"]["equity"] == pytest.approx(1000.0)
    assert {c["name"] for c in b["candidates"]} >= {"BTC", "SOL"}


def test_context_uses_adapter_available_margin_not_withdrawable_guess(tmp_path):
    eng, state, _, analyst = mk(tmp_path, [{"actions": []}])

    class AccountAdapter(DryRunAdapter):
        def account_snapshot(self, marks=None):
            snapshot = super().account_snapshot(marks)
            snapshot["available_margin"] = 777.0
            snapshot["held_collateral"] = 222.0
            snapshot["spot_usdc_total"] = 999.0
            snapshot["abstraction"] = "unifiedAccount"
            return snapshot

    eng.adapter = AccountAdapter(state, 1000.0)

    eng.cycle("scheduled")

    assert analyst.bundles[0]["account"]["available_margin"] == 777.0
    assert analyst.bundles[0]["account"]["held_collateral"] == 222.0
    assert analyst.bundles[0]["account"]["spot_usdc_total"] == 999.0
    assert analyst.bundles[0]["account"]["abstraction"] == "unifiedAccount"


def test_chat_persists_grounded_turn_and_forces_explicit_ticker_into_context(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    captured = install_chat(eng, answer="NVDA has no open position.")

    result = eng.chat("What is the live status of $NVDA?")

    assert result["proposal"] is None
    assert result["message"]["content"] == "NVDA has no open position."
    assert result["message"]["context_ts"] is not None
    assert "xyz:NVDA" in {
        candidate["name"] for candidate in captured["bundle"]["candidates"]
    }
    assert captured["bundle"]["account"]["equity"] == pytest.approx(1000)
    assert captured["message"] == "What is the live status of $NVDA?"
    assert [m["role"] for m in state.chat_history()] == ["user", "assistant"]


def test_chat_stream_reports_fresh_context_model_delta_and_proposal(tmp_path):
    eng, _, _, _ = mk(tmp_path, [])
    action = {
        **OPEN_SOL["actions"][0],
        "leverage": 20,
        "margin_mode": "cross",
    }

    class StreamingChatAnalyst:
        model = "qwen"

        def chat(self, bundle, history, message, on_event=None):
            on_event("delta", {"delta": "Prepared live SOL entry."})
            return ChatResult(
                response=ChatResponse.model_validate({
                    "answer": "Prepared live SOL entry.",
                    "proposal": action,
                }),
                latency_ms=7,
                raw="{}",
                tool_log=[],
            )

    eng.analyst = StreamingChatAnalyst()
    events = []
    result = eng.chat(
        "Prepare SOL.",
        stream_event=lambda kind, data: events.append((kind, data)),
    )

    assert result["proposal"]["status"] == "pending"
    assert "".join(
        data["delta"] for kind, data in events if kind == "delta"
    ) == "Prepared live SOL entry."
    context = next(data for kind, data in events if kind == "context")
    assert context["as_of_ts"] > 0
    assert context["account_equity"] == pytest.approx(1000)
    assert context["open_positions"] == 0
    proposal = next(data for kind, data in events if kind == "proposal")
    assert proposal["id"] == result["proposal"]["id"]
    assert proposal["preview"]["leverage"] == 20


def test_chat_unknown_explicit_ticker_fails_loudly_without_calling_qwen(tmp_path):
    eng, _, _, _ = mk(tmp_path, [])
    captured = install_chat(eng)

    result = eng.chat("Assess $DOESNOTEXIST.")

    assert result["proposal"] is None
    assert "unknown market" in result["message"]["content"]
    assert captured == {}


def test_chat_open_requires_confirmation_then_executes_once_with_provenance(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    action = {
        **OPEN_SOL["actions"][0],
        "leverage": 20,
        "margin_mode": "cross",
    }
    install_chat(eng, action, answer="Prepared a 20x cross SOL long.")

    response = eng.chat("Prepare the SOL long, but do not execute yet.")
    proposal = response["proposal"]

    assert state.open_position_for("SOL") is None
    assert proposal["status"] == "pending"
    assert proposal["preview"]["notional"] == pytest.approx(500)
    assert proposal["preview"]["size"] == pytest.approx(5)
    assert proposal["preview"]["required_margin"] == pytest.approx(25)
    assert proposal["preview"]["margin_mode"] == "cross"
    assert proposal["preview"]["estimated_tp_net"] > 0
    assert proposal["preview"]["estimated_stop_loss"] > 0

    confirmed = eng.confirm_trade(proposal["id"])
    duplicate = eng.confirm_trade(proposal["id"])

    assert confirmed["status"] == "executed"
    assert duplicate == confirmed
    position = state.open_position_for("SOL")
    assert position is not None
    assert position.size == pytest.approx(5)
    assert position.leverage == 20
    assert position.margin_mode == "cross"
    assert state.entries_today(utc_day(time.time())) == 1
    decisions = state.recent_decisions()
    assert len(decisions) == 1
    assert decisions[0]["trigger"] == f"chat authorized {proposal['id']}"
    assert state.unfinished_action_executions() == []


def test_confirmation_expiry_and_mark_drift_refuse_without_trade(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    action = {
        **OPEN_SOL["actions"][0],
        "stop": 97.0,
        "take_profit": 109.0,
    }
    install_chat(eng, action)
    expired = eng.chat("Prepare SOL.")["proposal"]
    state.db.execute(
        "UPDATE trade_proposals SET expires_ts=0 WHERE id=?", (expired["id"],)
    )
    state.db.commit()
    assert eng.confirm_trade(expired["id"])["status"] == "expired"

    fresh = eng.chat("Prepare SOL again.")["proposal"]
    eng.market._ctxs["SOL"] = Ctx("SOL", 100.6, 3.0, 11.0, 1e8, 5e8)
    refused = eng.confirm_trade(fresh["id"])
    assert refused["status"] == "refused"
    assert "0.5%" in refused["result"]["reason"]
    assert state.open_position_for("SOL") is None


def test_full_close_confirmation_rejects_changed_position_fingerprint(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    position = state.add_position(
        "SOL", "long", 100, 5, 500, 10, 97, 107, 0.8, "own",
        margin_mode="isolated",
    )
    install_chat(
        eng,
        {"kind": "close", "market": "SOL", "rationale": "thesis invalidated"},
    )

    proposal = eng.chat("Close the full SOL position.")["proposal"]
    assert proposal["preview"]["full_size"] == 5
    state.update_size(position.id, 4.5)

    refused = eng.confirm_trade(proposal["id"])

    assert refused["status"] == "refused"
    assert "position changed" in refused["result"]["reason"]
    assert state.open_position_for("SOL").size == 4.5


def test_confirmed_full_close_and_both_bracket_replacement(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    state.add_position(
        "SOL", "long", 100, 5, 500, 10, 97, 107, 0.8, "own",
        margin_mode="isolated",
    )
    install_chat(
        eng,
        {
            "kind": "adjust_stop", "market": "SOL", "stop": 99,
            "take_profit": 110, "rationale": "protect the breakout",
        },
    )
    adjustment = eng.chat("Move both SOL brackets.")["proposal"]
    adjusted = eng.confirm_trade(adjustment["id"])
    assert adjusted["status"] == "executed"
    position = state.open_position_for("SOL")
    assert (position.stop_px, position.tp_px) == (99, 110)

    install_chat(
        eng,
        {"kind": "close", "market": "SOL", "rationale": "exit all"},
    )
    close = eng.chat("Now close the full SOL position.")["proposal"]
    closed = eng.confirm_trade(close["id"])
    assert closed["status"] == "executed"
    assert closed["result"]["full_size"] == 5
    assert state.open_position_for("SOL") is None


def test_invalid_bracket_proposal_creates_no_confirmation(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    state.add_position(
        "SOL", "long", 100, 5, 500, 10, 97, 107, 0.8, "own",
        margin_mode="isolated",
    )
    install_chat(
        eng,
        {
            "kind": "adjust_stop", "market": "SOL", "stop": 101,
            "take_profit": 110, "rationale": "invalid stop",
        },
    )

    result = eng.chat("Move the SOL brackets.")

    assert result["proposal"] is None
    assert "wrong side" in result["message"]["metadata"]["proposal_refusal"]
    count = state.db.execute("SELECT COUNT(*) c FROM trade_proposals").fetchone()["c"]
    assert count == 0


def test_unfinished_journal_blocks_new_entry_preview(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    state.create_action_execution(
        "ambiguous", origin="autonomous", kind="open", proposal_id=None,
        action={"kind": "open", "market": "BTC"}, pre_state={}, expected={},
    )
    eng._account_snapshot = {"available_margin": 1000.0}
    action = OpenAction.model_validate(OPEN_SOL["actions"][0])

    with pytest.raises(ProposalError, match="unfinished action ambiguous"):
        eng._preview_action(
            action, {"SOL": 100.0}, 1000.0, utc_day(time.time()), frozenset(),
            chat_authorized=True,
        )


def test_confirmed_chat_entry_can_exceed_autonomous_three_position_cap(tmp_path):
    eng, state, _, _ = mk(tmp_path, [])
    for market, entry in (("BTC", 80_000), ("SOL", 100), ("xyz:NVDA", 218)):
        state.add_position(
            market, "long", entry, 0.01, 10, 10, entry * 0.97, entry * 1.07,
            0.8, "own", margin_mode="isolated",
        )
    action = {
        "kind": "open", "market": "ETH", "side": "long", "conviction": 0.8,
        "stop": 194, "take_profit": 218, "leverage": 10,
        "margin_mode": "isolated", "source": "own",
        "rationale": "operator-authorized fourth position",
        "invalidation": "loses 194",
    }
    install_chat(eng, action)

    proposal = eng.chat("Prepare an ETH entry despite the autonomous cap.")["proposal"]
    assert proposal is not None
    assert proposal["status"] == "pending"

    confirmed = eng.confirm_trade(proposal["id"])

    assert confirmed["status"] == "executed"
    assert len(state.open_positions()) == 4


def test_ambiguous_live_entry_submission_is_journaled_and_not_retried(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [], mode="live")
    action = OpenAction.model_validate(OPEN_SOL["actions"][0])
    day = utc_day(time.time())
    state.open_day(day, 1000)
    decision_id = state.record_decision(
        "scheduled", "", action.model_dump_json(), "qwen", 1, "ok"
    )

    class AmbiguousEntryAdapter(DryRunAdapter):
        def __init__(self, ledger):
            super().__init__(ledger, 1000)
            self.attempts = 0

        def open_entry(self, approved, mark):
            self.attempts += 1
            raise TimeoutError("response lost")

    adapter = AmbiguousEntryAdapter(state)
    eng.adapter = adapter
    preview = {
        "kind": "open", "mode": "live", "market": "SOL", "side": "long",
        "reference_mark": 100.0, "size": 5.0, "notional": 500.0,
        "risk_usd": 15.0, "leverage": 10, "margin_mode": "isolated",
        "required_margin": 50.0, "available_margin": 1000.0,
        "stop_px": 97.0, "tp_px": 107.0,
    }

    result = eng._execute_open_preview(
        action, preview, day, origin="autonomous",
        proposal_id=None, decision_id=decision_id,
    )

    assert result["status"] == "needs_reconciliation"
    assert adapter.attempts == 1
    assert state.unfinished_action_executions()[0]["status"] == "needs_reconciliation"
    assert any("HIGH PRIORITY" in line for line in notes.lines)
    eng._account_snapshot = {"available_margin": 1000.0}
    with pytest.raises(ProposalError, match="unfinished action"):
        eng._preview_action(action, {"SOL": 100.0}, 1000.0, day, frozenset())
    assert adapter.attempts == 1


def test_fill_recovery_window_includes_exact_lower_and_upper_boundaries(tmp_path):
    eng, _, _, _ = mk(tmp_path, [], mode="live")
    journal = {
        "kind": "open",
        "action": {"market": "SOL", "side": "long"},
        "expected": {"size": 5.0},
        "fill_id": None,
        "submission_ts": 100.0,
        "response_ts": 110.0,
    }
    lower = {
        "tid": "lower", "coin": "SOL", "dir": "Open Long",
        "sz": "5", "px": "100", "time": 98_000,
    }
    upper = {
        "tid": "upper", "coin": "SOL", "dir": "Open Long",
        "sz": "5", "px": "100", "time": 130_000,
    }
    outside = {
        "tid": "outside", "coin": "SOL", "dir": "Open Long",
        "sz": "5", "px": "100", "time": 130_001,
    }

    assert eng._matching_journal_fills(journal, [lower]) == [lower]
    assert eng._matching_journal_fills(journal, [upper]) == [upper]
    assert eng._matching_journal_fills(journal, [outside]) == []


def test_entry_timeout_waits_for_full_recovery_window_before_failing(
    tmp_path, monkeypatch
):
    eng, state, _, _ = mk(tmp_path, [], mode="live")
    action = OpenAction.model_validate(OPEN_SOL["actions"][0])
    decision_id = state.record_decision(
        "scheduled", "", action.model_dump_json(), "qwen", 1, "ok"
    )
    state.create_action_execution(
        "late-fill", origin="autonomous", kind="open", proposal_id=None,
        decision_id=decision_id, action=action.model_dump(mode="json"),
        pre_state={"position": None}, expected={"size": 5.0}, now=100,
    )
    state.update_action_execution(
        "late-fill", stage="entry_submitted", status="needs_reconciliation",
        submission_ts=100, response_ts=105,
    )

    monkeypatch.setattr("peri.engine.time.time", lambda: 110)
    eng._reconcile_open_execution(
        state.action_execution("late-fill"), action, [], []
    )
    assert state.action_execution("late-fill")["status"] == "needs_reconciliation"

    monkeypatch.setattr("peri.engine.time.time", lambda: 131)
    eng._reconcile_open_execution(
        state.action_execution("late-fill"), action, [], []
    )
    assert state.action_execution("late-fill")["status"] == "failed"


class RecoveryAdapter(DryRunAdapter):
    def __init__(self, state, fills, orders):
        super().__init__(state, 1000)
        self._fills = fills
        self._orders = orders
        self.cancelled = []
        self.entry_attempts = 0

    def fills(self):
        return self._fills

    def open_orders_all(self):
        return self._orders

    def cancel_orders(self, market, order_ids):
        self.cancelled.extend((market, oid) for oid in order_ids)

    def open_entry(self, approved, mark):
        self.entry_attempts += 1
        raise AssertionError("recovery must never resubmit an entry")


def _trigger_order(oid, kind, px):
    return {
        "coin": "SOL",
        "orderType": "Stop Market" if kind == "sl" else "Take Profit Market",
        "side": "A",
        "sz": "5",
        "limitPx": str(px),
        "triggerPx": str(px),
        "reduceOnly": True,
        "isTrigger": True,
        "oid": oid,
    }


def test_exact_single_fill_recovery_finalizes_without_resubmitting_entry(tmp_path):
    eng, state, _, _ = mk(tmp_path, [], mode="live")
    action = OpenAction.model_validate(OPEN_SOL["actions"][0])
    position = state.add_position(
        "SOL", "long", 100.1, 5, 500, 10, None, None, None, "external",
        margin_mode="isolated",
    )
    decision_id = state.record_decision(
        "scheduled", "", action.model_dump_json(), "qwen", 1, "ok"
    )
    expected = {
        "kind": "open", "mode": "live", "market": "SOL", "side": "long",
        "reference_mark": 100, "size": 5.0, "notional": 500.0,
        "risk_usd": 15.0, "leverage": 10, "margin_mode": "isolated",
        "required_margin": 50.0, "available_margin": 1000.0,
        "stop_px": 97.0, "tp_px": 107.0,
    }
    state.open_day(utc_day(100), 1000)
    state.create_action_execution(
        "recover-open", origin="autonomous", kind="open", proposal_id=None,
        decision_id=decision_id, action=action.model_dump(mode="json"),
        pre_state={"position": None}, expected=expected, now=100,
    )
    state.update_action_execution(
        "recover-open", stage="entry_submitted", status="needs_reconciliation",
        submission_ts=100, response_ts=105,
    )
    fill = {
        "tid": "one", "coin": "SOL", "dir": "Open Long", "sz": "5",
        "px": "100.1", "time": 101_000,
    }
    adapter = RecoveryAdapter(
        state, [fill], [_trigger_order(11, "sl", 97), _trigger_order(12, "tp", 107)]
    )
    eng.adapter = adapter

    eng.reconcile_action_executions()

    journal = state.action_execution("recover-open")
    assert journal["status"] == "executed"
    assert journal["result"]["recovered"] is True
    recovered = state.open_position_for("SOL")
    assert recovered.id == position.id
    assert recovered.source == "own"
    assert recovered.stop_px == 97 and recovered.tp_px == 107
    assert adapter.entry_attempts == 0


def test_multiple_matching_entry_fills_enter_manual_review(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [], mode="live")
    action = OpenAction.model_validate(OPEN_SOL["actions"][0])
    decision_id = state.record_decision(
        "scheduled", "", action.model_dump_json(), "qwen", 1, "ok"
    )
    expected = {
        "size": 5.0, "stop_px": 97.0, "tp_px": 107.0,
        "notional": 500.0, "leverage": 10, "margin_mode": "isolated",
    }
    state.create_action_execution(
        "ambiguous-open", origin="autonomous", kind="open", proposal_id=None,
        decision_id=decision_id, action=action.model_dump(mode="json"),
        pre_state={"position": None}, expected=expected, now=100,
    )
    state.update_action_execution(
        "ambiguous-open", stage="entry_submitted", status="needs_reconciliation",
        submission_ts=100, response_ts=105,
    )
    fills = [
        {"tid": str(i), "coin": "SOL", "dir": "Open Long", "sz": "5",
         "px": "100", "time": 101_000}
        for i in range(2)
    ]
    adapter = RecoveryAdapter(state, fills, [])
    eng.adapter = adapter

    eng.reconcile_action_executions()

    assert state.action_execution("ambiguous-open")["status"] == "manual_review"
    assert adapter.entry_attempts == 0
    assert any("manual_review" in line for line in notes.lines)


def test_close_and_adjust_recovery_require_exact_venue_evidence(tmp_path):
    eng, state, _, _ = mk(tmp_path, [], mode="live")
    position = state.add_position(
        "SOL", "long", 100, 5, 500, 10, 97, 107, 0.8, "own",
        margin_mode="isolated",
    )
    fingerprint = eng._position_fingerprint(position)
    adjust = {
        "kind": "adjust_stop", "market": "SOL", "stop": 99,
        "take_profit": 110, "rationale": "trail",
    }
    adjust_decision = state.record_decision(
        "scheduled", "", json.dumps([adjust]), "qwen", 1, "ok"
    )
    state.create_action_execution(
        "recover-adjust", origin="autonomous", kind="adjust_stop", proposal_id=None,
        decision_id=adjust_decision, action=adjust,
        pre_state={
            "position": fingerprint,
            "old_orders": [_trigger_order(11, "sl", 97), _trigger_order(12, "tp", 107)],
        },
        expected={"new_stop_px": 99.0, "new_tp_px": 110.0}, now=100,
    )
    state.update_action_execution(
        "recover-adjust", stage="submitted", status="needs_reconciliation",
        submission_ts=100, response_ts=105,
    )
    orders = [
        _trigger_order(11, "sl", 97), _trigger_order(12, "tp", 107),
        _trigger_order(21, "sl", 99), _trigger_order(22, "tp", 110),
    ]
    adapter = RecoveryAdapter(state, [], orders)
    eng.adapter = adapter

    eng.reconcile_action_executions()

    assert state.action_execution("recover-adjust")["status"] == "executed"
    assert adapter.cancelled == [("SOL", 11), ("SOL", 12)]
    adjusted = state.open_position_for("SOL")
    assert (adjusted.stop_px, adjusted.tp_px) == (99, 110)

    close = {"kind": "close", "market": "SOL", "rationale": "exit"}
    close_decision = state.record_decision(
        "scheduled", "", json.dumps([close]), "qwen", 1, "ok"
    )
    close_fingerprint = eng._position_fingerprint(adjusted)
    state.create_action_execution(
        "recover-close", origin="autonomous", kind="close", proposal_id=None,
        decision_id=close_decision, action=close,
        pre_state={"position": close_fingerprint}, expected={}, now=200,
    )
    state.update_action_execution(
        "recover-close", stage="submitted", status="needs_reconciliation",
        submission_ts=200, response_ts=205,
    )
    state.mark_position_missing(adjusted.id)
    adapter._fills = [{
        "tid": "close-one", "coin": "SOL", "dir": "Close Long", "sz": "5",
        "px": "102", "closedPnl": "10", "fee": "1", "time": 201_000,
    }]

    eng.reconcile_action_executions()

    assert state.action_execution("recover-close")["status"] == "executed"
    assert state.recent_closes(1)[0]["realized_pnl"] == 9


def test_fresh_caller_symbol_forces_market_features_into_context(tmp_path):
    eng, state, _, analyst = mk(tmp_path, [{"actions": []}])
    state.add_tg_message(3193, time.time(), "caller1", "NVDA LONG", True)

    eng.cycle("caller message")

    assert "xyz:NVDA" in {c["name"] for c in analyst.bundles[0]["candidates"]}


def test_low_conviction_refused_and_recorded(tmp_path):
    bad = {"actions": [{**OPEN_SOL["actions"][0], "conviction": 0.5}]}
    eng, state, notes, _ = mk(tmp_path, [bad])
    eng.cycle("scheduled")
    assert state.open_position_for("SOL") is None
    r = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "conviction" in r["reason"]
    assert any("refused SOL" in line for line in notes.lines)


def test_dry_brackets_close_on_tp(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [OPEN_SOL, {"actions": []}])
    eng.cycle("scheduled")
    eng.market._ctxs["SOL"] = Ctx("SOL", 108.0, 8.0, 11.0, 1e8, 5e8)  # above TP
    eng.cycle("scheduled")
    assert state.open_position_for("SOL") is None
    closes = state.recent_closes()
    assert closes[0]["close_reason"] == "tp"
    assert closes[0]["realized_pnl"] > 0
    assert state.cooldown_until("SOL", time.time()) is not None


def test_dry_brackets_close_on_sl_with_long_cooldown(tmp_path):
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL, {"actions": []}])
    eng.cycle("scheduled")
    eng.market._ctxs["SOL"] = Ctx("SOL", 96.0, -4.0, 11.0, 1e8, 5e8)  # below SL
    eng.cycle("scheduled")
    closes = state.recent_closes()
    assert closes[0]["close_reason"] == "sl"
    # asymmetric cooldown: still cooling after the plain 1h window
    assert state.cooldown_until("SOL", time.time() + 3601) is not None


def test_analyst_error_skips_cycle_loudly(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [AnalystError("down")])
    eng.cycle("scheduled")
    row = state.db.execute("SELECT status FROM decisions").fetchone()
    assert row["status"] == "error"
    assert any("analyst DOWN" in line for line in notes.lines)


def test_open_orders_error_skips_analyst_loudly(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [{"actions": []}])
    eng.adapter = OrdersDownAdapter(state, eng.cfg.risk.paper_bankroll)

    eng.cycle("scheduled")

    assert analyst.bundles == []
    row = state.db.execute("SELECT status, reasoning FROM decisions").fetchone()
    assert row["status"] == "error"
    assert "open orders unavailable" in row["reasoning"]
    assert any("context DOWN" in line and "open orders unavailable" in line
               for line in notes.lines)


def test_live_fills_error_skips_analyst_loudly(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [{"actions": []}], mode="live")
    eng.adapter = FillsDownAdapter(state, eng.cfg.risk.paper_bankroll)

    eng.cycle("scheduled")

    assert analyst.bundles == []
    row = state.db.execute("SELECT status, reasoning FROM decisions").fetchone()
    assert row["status"] == "error"
    assert "venue fills unavailable" in row["reasoning"]
    assert any("context DOWN" in line and "venue fills unavailable" in line
               for line in notes.lines)


def test_pending_entry_order_blocks_duplicate_analyst_open(tmp_path):
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL])
    eng.adapter = StaticOrdersAdapter(
        state,
        eng.cfg.risk.paper_bankroll,
        [{"coin": "SOL", "side": "B", "reduceOnly": False}],
    )

    eng.cycle("scheduled")

    assert state.open_position_for("SOL") is None
    refusal = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "venue order already open" in refusal["reason"]


def test_protective_only_order_reserves_capacity_during_fill_race(tmp_path):
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL])
    state.add_position("BTC", "long", 80000.0, 0.01, 800.0, 3.0,
                       None, None, 0.8, "own")
    state.add_position("xyz:NVDA", "long", 218.0, 0.1, 21.8, 3.0,
                       None, None, 0.8, "own")
    eng.adapter = StaticOrdersAdapter(
        state,
        eng.cfg.risk.paper_bankroll,
        [{"coin": "xyz:MRNA", "side": "B", "reduceOnly": True,
          "isTrigger": True, "orderType": "Stop Market", "triggerPx": "148.6"}],
    )

    eng.cycle("scheduled")

    assert state.open_position_for("SOL") is None
    refusal = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "concurrent" in refusal["reason"]


def test_malformed_open_order_skips_analyst_loudly(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [{"actions": []}])
    eng.adapter = StaticOrdersAdapter(
        state,
        eng.cfg.risk.paper_bankroll,
        [{"coin": "SOL", "side": "X", "reduceOnly": False}],
    )

    eng.cycle("scheduled")

    assert analyst.bundles == []
    row = state.db.execute("SELECT status, reasoning FROM decisions").fetchone()
    assert row["status"] == "error"
    assert "open order side is malformed" in row["reasoning"]
    assert any("context DOWN" in line for line in notes.lines)


def test_live_brackets_sync_from_upstream_and_clear_stale_levels(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position(
        "xyz:KIOXIA", "short", 333.76, 0.117, 39.05, 2.0,
        None, None, None, "external",
    )
    state.add_position(
        "SOL", "long", 100.0, 1.0, 100.0, 2.0,
        95.0, 110.0, 0.8, "own",
    )
    orders = [
        {"coin": "xyz:KIOXIA", "side": "B", "reduceOnly": True,
         "isTrigger": True, "orderType": "Stop Market", "triggerPx": "342.0"},
        {"coin": "xyz:KIOXIA", "side": "B", "reduceOnly": True,
         "isTrigger": True, "orderType": "Take Profit Market", "triggerPx": "315.0"},
    ]

    eng.sync_live_brackets(orders)

    kioxia = state.open_position_for("xyz:KIOXIA")
    assert kioxia.stop_px == pytest.approx(342.0)
    assert kioxia.tp_px == pytest.approx(315.0)
    # SOL is OURS and the venue shows no protection for it: the engine no longer
    # just records NULL and moves on — it restores the levels it sized with
    sol = state.open_position_for("SOL")
    assert sol.stop_px == pytest.approx(95.0) and sol.tp_px == pytest.approx(110.0)


def test_missing_mark_for_open_position_skips_analyst_loudly(tmp_path):
    eng, state, notes, analyst = mk(tmp_path, [{"actions": []}])
    state.add_position(
        "xyz:GHOST", "long", 10.0, 1.0, 10.0, 1.0,
        None, None, 0.8, "external",
    )

    eng.cycle("scheduled")

    assert analyst.bundles == []
    row = state.db.execute("SELECT status, reasoning FROM decisions").fetchone()
    assert row["status"] == "error"
    assert "mark unavailable for open position xyz:GHOST" in row["reasoning"]
    assert any("context DOWN" in line for line in notes.lines)


def test_kill_switch_trips_and_blocks_entries(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}, OPEN_SOL])
    day = utc_day(time.time())
    state.open_day(day, 2000.0)  # dry equity 1000 => -50% from open
    eng.cycle("scheduled")
    assert state.kill_tripped(day)
    assert any("KILL SWITCH" in line for line in notes.lines)
    eng.cycle("scheduled")  # analyst tries to open — guard refuses
    assert state.open_position_for("SOL") is None
    r = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "kill" in r["reason"]


def test_close_action(tmp_path):
    close = {"actions": [{"kind": "close", "market": "SOL", "rationale": "done"}]}
    eng, state, notes, _ = mk(tmp_path, [OPEN_SOL, close])
    eng.cycle("scheduled")
    eng.cycle("scheduled")
    assert state.open_position_for("SOL") is None
    assert state.recent_closes()[0]["close_reason"] == "analyst"


def test_adjust_stop_action(tmp_path):
    adj = {"actions": [{"kind": "adjust_stop", "market": "SOL", "stop": 99.0,
                        "take_profit": 109.0, "rationale": "lock in"}]}
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL, adj])
    eng.cycle("scheduled")
    eng.cycle("scheduled")
    position = state.open_position_for("SOL")
    assert position.stop_px == 99.0
    assert position.tp_px == 109.0


def test_adjust_stop_wrong_side_refused(tmp_path):
    adj = {"actions": [{"kind": "adjust_stop", "market": "SOL", "stop": 150.0,
                        "take_profit": 107.0, "rationale": "bad"}]}
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL, adj])
    eng.cycle("scheduled")
    eng.cycle("scheduled")
    assert state.open_position_for("SOL").stop_px == 97.0  # unchanged
    assert state.db.execute("SELECT COUNT(*) c FROM refusals").fetchone()["c"] == 1


def test_adjust_take_profit_wrong_side_refused(tmp_path):
    adj = {"actions": [{"kind": "adjust_stop", "market": "SOL", "stop": 99.0,
                        "take_profit": 95.0, "rationale": "bad target"}]}
    eng, state, _, _ = mk(tmp_path, [OPEN_SOL, adj])
    eng.cycle("scheduled")
    eng.cycle("scheduled")

    position = state.open_position_for("SOL")
    assert position.stop_px == 97.0 and position.tp_px == 107.0
    refusal = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "take-profit" in refusal["reason"]


def test_unknown_market_refused(tmp_path):
    ghost = {"actions": [{**OPEN_SOL["actions"][0], "market": "xyz:GHOST"}]}
    eng, state, _, _ = mk(tmp_path, [ghost])
    eng.cycle("scheduled")
    r = state.db.execute("SELECT reason FROM refusals").fetchone()
    assert "unknown market" in r["reason"]


class FakeLiveAdapter(DryRunAdapter):
    def __init__(self, state, bankroll, fills):
        super().__init__(state, bankroll)
        self._fills = fills
        self.cancelled_brackets = []

    def fills(self):
        return self._fills

    def cancel_brackets(self, market):
        self.cancelled_brackets.append(market)


def test_live_reconcile_closes_on_bracket_fill(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position("SOL", "long", 100.0, 5.0, 500.0, 5.0, 97.0, 107.0,
                       0.8, "own")
    adapter = FakeLiveAdapter(state, 1000.0, [
        {"tid": "f1", "coin": "SOL", "px": "107.02", "sz": "5.0",
         "dir": "Close Long", "closedPnl": "35.1", "fee": "0.56"}])
    eng.adapter = adapter
    eng.reconcile_live()
    assert state.open_position_for("SOL") is None
    assert adapter.cancelled_brackets == ["SOL"]
    c = state.recent_closes()[0]
    assert c["close_reason"] == "tp"
    assert c["realized_pnl"] == pytest.approx(35.1 - 0.56)
    # same fill replayed -> no double close
    eng.reconcile_live()
    assert state.db.execute(
        "SELECT COUNT(*) c FROM positions WHERE status='closed'").fetchone()["c"] == 1


def test_live_bundle_includes_seen_venue_close_history(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    fill = {
        "tid": "seeded-before-fresh-start", "coin": "xyz:NVDA", "px": "225.83",
        "sz": "0.596", "dir": "Close Long", "closedPnl": "4.57728",
        "fee": "0.052491", "time": 1_787_821_288_299,
    }
    state.mark_fill(fill["tid"])
    eng.adapter = FakeLiveAdapter(state, 1000.0, [fill])

    eng.reconcile_live()
    bundle = eng.build_bundle(
        "scheduled", eng.market.ctxs(), {}, 1000.0,
        utc_day(time.time()), 0.0, [],
    )

    assert bundle["closes"][0]["id"] == fill["tid"]
    assert bundle["closes"][0]["realized_pnl"] == pytest.approx(4.524789)
    assert bundle["account"]["realized_pnl_recent"] == pytest.approx(4.524789)
    assert bundle["account"]["realized_scope"] == "recent venue fills"


def test_live_reconcile_retries_fill_when_orphan_cleanup_fails(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position("SOL", "long", 100.0, 5.0, 500.0, 5.0, 97.0, 107.0,
                       0.8, "own")

    class FailingCleanupAdapter(FakeLiveAdapter):
        def cancel_brackets(self, market):
            raise RuntimeError("venue unavailable")

    eng.adapter = FailingCleanupAdapter(state, 1000.0, [
        {"tid": "retry-close", "coin": "SOL", "px": "107.0", "sz": "5.0",
         "dir": "Close Long", "closedPnl": "35.0", "fee": "0.5"},
    ])

    with pytest.raises(RuntimeError, match="venue unavailable"):
        eng.reconcile_live()

    assert state.open_position_for("SOL") is not None
    assert not state.fill_seen("retry-close")


def test_live_reconcile_partial_close_shrinks(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position("SOL", "long", 100.0, 5.0, 500.0, 5.0, 97.0, 107.0, 0.8, "own")
    eng.adapter = FakeLiveAdapter(state, 1000.0, [
        {"tid": "p1", "coin": "SOL", "px": "104.0", "sz": "2.0",
         "dir": "Close Long", "closedPnl": "8.0", "fee": "0.2"}])
    eng.reconcile_live()
    pos = state.open_position_for("SOL")
    assert pos is not None and pos.size == pytest.approx(3.0)


def test_live_reconcile_validates_all_fills_before_marking_any_seen(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position("SOL", "long", 100.0, 5.0, 500.0, 5.0, 97.0, 107.0, 0.8, "own")
    eng.adapter = FakeLiveAdapter(state, 1000.0, [
        {"tid": "valid-open", "coin": "SOL", "px": "100", "sz": "1",
         "dir": "Open Long", "closedPnl": "0", "fee": "0.1"},
        {"tid": "bad-close", "coin": "SOL", "px": "bad", "sz": "2",
         "dir": "Close Long", "closedPnl": "8", "fee": "0.2"},
    ])

    with pytest.raises(ContextError, match="fill is malformed"):
        eng.reconcile_live()

    assert not state.fill_seen("valid-open")
    assert not state.fill_seen("bad-close")


class FakeLiveInfoAdapter(FakeLiveAdapter):
    """Live-boot fake: carries .info/.account for adoption + fills history."""

    class _Info:
        def __init__(self, states):
            self._states = states

        def post(self, path, body):
            return self._states.get(body.get("dex", ""), {"assetPositions": []})

    def __init__(self, state, bankroll, fills, venue_states):
        super().__init__(state, bankroll, fills)
        self.info = self._Info(venue_states)
        self.account = "0xacct"


def test_live_reconcile_syncs_existing_position_from_venue(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position(
        "SOL", "long", 100.0, 2.0, 200.0, 5.0,
        97.0, 107.0, 0.8, "own", rationale="original rationale",
    )
    venue = {"": {"assetPositions": [{"position": {
        "coin": "SOL", "szi": "3.0", "entryPx": "105.0",
        "leverage": {"type": "cross", "value": 4},
        "positionValue": "315.6", "marginUsed": "78.9",
        "unrealizedPnl": "1.8", "returnOnEquity": "0.0228",
        "liquidationPx": "82.5",
    }}], "withdrawable": "44.5"}}
    eng.adapter = FakeLiveInfoAdapter(state, 1000.0, [], venue)

    eng.reconcile({})

    pos = state.open_position_for("SOL")
    assert pos is not None
    assert pos.side == "long"
    assert pos.size == pytest.approx(3.0)
    assert pos.entry_px == pytest.approx(105.0)
    assert pos.notional == pytest.approx(315.0)
    assert pos.leverage == pytest.approx(4.0)
    assert pos.margin_mode == "cross"
    assert pos.rationale == "original rationale"
    assert any("synced venue position: SOL" in line for line in notes.lines)

    bundle = eng.build_bundle(
        "scheduled", eng.market.ctxs(), {"SOL": 100.0}, 1000.0,
        utc_day(time.time()), 0.0, [],
    )
    live_pos = next(p for p in bundle["positions"] if p["market"] == "SOL")
    assert live_pos["position_value"] == pytest.approx(315.6)
    assert live_pos["margin"] == pytest.approx(78.9)
    assert live_pos["upnl"] == pytest.approx(1.8)
    assert live_pos["roe"] == pytest.approx(0.0228)
    assert live_pos["liquidation_px"] == pytest.approx(82.5)
    assert live_pos["margin_mode"] == "cross"
    assert bundle["account"]["available_margin"] == pytest.approx(44.5)
    assert bundle["account"]["total_margin_used"] == pytest.approx(78.9)


def test_live_reconcile_marks_ledger_position_missing_at_venue(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}], mode="live")
    pos = state.add_position(
        "SOL", "long", 100.0, 2.0, 200.0, 5.0,
        97.0, 107.0, 0.8, "own",
    )
    venue = {
        "": {"assetPositions": []},
        "xyz": {"assetPositions": []},
    }
    eng.adapter = FakeLiveInfoAdapter(state, 1000.0, [], venue)

    # one absent read is not proof: the close fill is usually moments behind it,
    # and writing the row off terminally discards that trade's realized PnL
    eng.reconcile({})
    assert state.open_position_for("SOL") is not None
    assert any("waiting one more read" in line for line in notes.lines)

    eng.reconcile({})

    assert state.open_position_for("SOL") is None
    row = state.db.execute(
        "SELECT status, close_reason FROM positions WHERE id=?", (pos.id,),
    ).fetchone()
    assert dict(row) == {"status": "missing", "close_reason": "venue_missing"}
    assert any("position missing at venue: SOL" in line for line in notes.lines)


def test_a_position_that_reappears_resets_the_absence_count(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    state.add_position("SOL", "long", 100.0, 2.0, 200.0, 5.0, 97.0, 107.0, 0.8, "own")
    present = {"": {"assetPositions": [{"position": {
        "coin": "SOL", "szi": "2.0", "entryPx": "100.0", "leverage": {"value": 5}}}]},
        "xyz": {"assetPositions": []}}
    absent = {"": {"assetPositions": []}, "xyz": {"assetPositions": []}}
    eng.adapter = FakeLiveInfoAdapter(state, 1000.0, [], absent)
    eng.reconcile({})                       # absent once
    eng.adapter.info._states = present
    eng.reconcile({})                       # back — a stale read, not a close
    eng.adapter.info._states = absent
    eng.reconcile({})                       # absent once again, not twice
    assert state.open_position_for("SOL") is not None


def test_live_reconcile_rejects_malformed_position_snapshot(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    eng.adapter = FakeLiveInfoAdapter(
        state, 1000.0, [], {"": {"error": "invalid JSON"}},
    )

    with pytest.raises(ContextError, match="native positions response"):
        eng.adopt_external()


def test_live_reconcile_rejects_malformed_risk_detail(tmp_path):
    eng, state, _, _ = mk(tmp_path, [{"actions": []}], mode="live")
    venue = {"": {"assetPositions": [{"position": {
        "coin": "SOL", "szi": "1", "entryPx": "100",
        "leverage": {"value": 5}, "liquidationPx": "bad",
    }}]}}
    eng.adapter = FakeLiveInfoAdapter(state, 1000.0, [], venue)

    with pytest.raises(ContextError, match="liquidationPx"):
        eng.adopt_external()


def test_startup_seeds_history_before_adopting(tmp_path):
    """A historical Close fill must never close a freshly adopted position."""
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}], mode="live")
    old_fill = {"tid": "hist1", "coin": "SOL", "px": "101.0", "sz": "5.0",
                "dir": "Close Long", "closedPnl": "5.0", "fee": "0.1"}
    venue = {"": {"assetPositions": [{"position": {
        "coin": "SOL", "szi": "5.0", "entryPx": "100.0",
        "leverage": {"value": 5}}}]}}
    eng.adapter = FakeLiveInfoAdapter(state, 1000.0, [old_fill], venue)
    eng.startup()
    pos = state.open_position_for("SOL")
    assert pos is not None and pos.source == "external"
    assert state.fill_seen("hist1")
    eng.reconcile_live()  # historical fill already seen -> position survives
    assert state.open_position_for("SOL") is not None


def test_restart_reconciles_fill_that_closed_existing_position(tmp_path):
    eng, state, notes, _ = mk(tmp_path, [{"actions": []}], mode="live")
    old_fill = {"tid": "hist1", "coin": "SOL", "px": "101.0", "sz": "5.0",
                "dir": "Close Long", "closedPnl": "5.0", "fee": "0.1"}
    open_venue = {"": {"assetPositions": [{"position": {
        "coin": "SOL", "szi": "5.0", "entryPx": "100.0",
        "leverage": {"value": 5}}}]}}
    adapter = FakeLiveInfoAdapter(state, 1000.0, [old_fill], open_venue)
    eng.adapter = adapter
    eng.startup()

    offline_close = {"tid": "close2", "coin": "SOL", "px": "107.0", "sz": "5.0",
                     "dir": "Close Long", "closedPnl": "35.0", "fee": "0.5"}
    adapter._fills = [old_fill, offline_close]
    adapter.info._states = {
        "": {"assetPositions": []},
        "xyz": {"assetPositions": []},
    }

    eng.startup()

    assert state.open_position_for("SOL") is None
    close = state.recent_closes()[0]
    assert close["close_reason"] == "external"
    assert close["close_px"] == pytest.approx(107.0)
    assert close["realized_pnl"] == pytest.approx(34.5)
    assert state.fill_seen("close2")
    assert not any("position missing at venue: SOL" in line for line in notes.lines)


def test_redundant_close_after_bracket_fill_is_cancelled_not_stuck(tmp_path):
    """Live race: SL fired, then the analyst's close was submitted against a
    flat position. The close must finish as terminal 'cancelled' (unblocking
    new entries), not fail-close forever in manual_review."""
    eng, state, notes, _ = mk(tmp_path, [], mode="live")
    position = state.add_position(
        "SOL", "long", 106.17, 0.78, 82.8, 10, 106.0, 111.4, 0.8, "own",
        margin_mode="isolated",
    )
    fingerprint = eng._position_fingerprint(position)
    close = {"kind": "close", "market": "SOL", "rationale": "caller fades my long"}
    decision_id = state.record_decision(
        "scheduled", "", json.dumps([close]), "qwen", 1, "ok"
    )
    state.create_action_execution(
        "moot-close", origin="autonomous", kind="close", proposal_id=None,
        decision_id=decision_id, action=close,
        pre_state={"position": fingerprint}, expected={}, now=200,
    )
    state.update_action_execution(
        "moot-close", stage="submitted", status="needs_reconciliation",
        submission_ts=200, response_ts=205,
    )
    # the bracket had already closed it (ledger reconciled from venue)
    state.close_position(position.id, "sl", 106.0, -0.195)
    sl_fill = {"tid": "sl-fill", "coin": "SOL", "dir": "Close Long", "sz": "0.78",
               "px": "106.0", "closedPnl": "-0.13", "fee": "0.06", "time": 123_000}
    eng.adapter = RecoveryAdapter(state, [sl_fill], [])   # 77s outside the window

    eng.reconcile_action_executions()

    journal = state.action_execution("moot-close")
    assert journal["status"] == "cancelled"
    assert state.unfinished_action_executions() == []
    assert not any("HIGH PRIORITY" in line for line in notes.lines)
    assert any("superseded" in line for line in notes.lines)


# -- 2026-08-29: resting entries, position management, pause ---------------
def rails_cfg(mode="dry"):
    c = cfg(mode)
    r = c.risk
    r.daily_entry_cap = 3
    r.day_loss_halt_pct = 6.0
    r.min_stop_pct = 2.0
    r.atr_stop_mult = 4.0
    r.max_range_pos_long = 0.80
    r.min_range_pos_short = 0.20
    r.breakeven_at_r = 1.0
    r.time_stop_secs = 10800
    r.time_stop_min_r = 0.5
    r.entry_expiry_secs = 7200
    c.analyst.cycle_secs_quiet = 900
    c.analyst.cycle_secs_active = 300
    c.analyst.heat_atr_pct = 0.9
    return c


def mk_rails(tmp_path, decisions, mode="dry"):
    eng, state, notes, analyst = mk(tmp_path, decisions, mode)
    eng.cfg = rails_cfg(mode)
    eng.guard.cfg = eng.cfg.risk
    return eng, state, notes, analyst


REST_SOL = {"actions": [{"kind": "open", "market": "SOL", "side": "long",
                         "conviction": 0.8, "entry": 97.5, "stop": 94.0,
                         "take_profit": 108.0, "leverage": 10,
                         "margin_mode": "isolated", "source": "own",
                         "rationale": "buy the pullback, do not chase",
                         "invalidation": "loses 94"}]}


def test_resting_entry_parks_an_order_and_opens_nothing(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [REST_SOL])
    eng.cycle("scheduled")
    assert state.open_positions() == []
    resting = state.resting_entries()
    assert len(resting) == 1
    entry = resting[0]
    assert entry["market"] == "SOL" and entry["side"] == "long"
    assert entry["entry_px"] == 97.5 and entry["stop_px"] == 94.0
    # sized from the resting price, not the 100.0 mark
    assert entry["notional"] == pytest.approx(entry["size"] * 97.5, rel=1e-3)
    assert any("RESTING long SOL @ 97.5" in line for line in notes.lines)


def test_resting_entry_becomes_a_position_when_the_market_comes_to_it(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [REST_SOL, {"actions": []}])
    eng.cycle("scheduled")
    assert state.open_positions() == []
    eng.market._ctxs["SOL"] = Ctx("SOL", 97.4, 3.0, 11.0, 1e8, 5e8)  # pullback arrives
    eng.cycle("scheduled")
    positions = state.open_positions()
    assert len(positions) == 1
    assert positions[0].market == "SOL" and positions[0].entry_px == 97.5
    assert positions[0].stop_px == 94.0 and positions[0].source == "own"
    assert state.resting_entries() == []
    assert state.entries_today(utc_day(time.time())) == 1
    assert any("FILLED" in line for line in notes.lines)


def test_unfilled_resting_entry_expires_costing_nothing(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [REST_SOL, {"actions": []}])
    eng.cycle("scheduled")
    entry_id = state.resting_entries()[0]["id"]
    state.db.execute("UPDATE pending_entries SET expires_ts=? WHERE id=?",
                     (time.time() - 1, entry_id))
    state.db.commit()
    eng.cycle("scheduled")
    assert state.resting_entries() == []
    assert state.open_positions() == []
    assert state.recent_pending_entries(1)[0]["outcome"] == "expired"
    assert any("expired" in line and "no exposure taken" in line for line in notes.lines)


def test_pause_halts_entries_and_cancels_what_is_resting(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [REST_SOL, REST_SOL])
    eng.cycle("scheduled")
    assert len(state.resting_entries()) == 1

    result = eng.set_paused(True)
    assert result["paused"] is True and result["cancelled_entries"] == ["SOL"]
    assert state.resting_entries() == []
    assert state.paused() is True

    eng.cycle("scheduled")          # the analyst tries again while paused
    assert state.resting_entries() == []
    assert state.open_positions() == []
    assert any("paused by operator" in line for line in notes.lines)

    eng.set_paused(False)
    assert state.paused() is False


def test_pause_survives_a_restart(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    eng.set_paused(True)
    reopened = State(str(tmp_path / "t.db"))
    assert reopened.paused() is True


def test_breakeven_stop_moves_once_the_trade_is_one_r_in_front(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [])
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                             0.8, "own", rationale="r", invalidation="i",
                             margin_mode="isolated")
    eng.manage_positions({"SOL": 103.0})            # +0.75R — too early
    assert state.open_position_for("SOL").stop_px == 96.0
    eng.manage_positions({"SOL": 104.5})            # +1.125R
    moved = state.open_position_for("SOL")
    assert moved.stop_px > 100.0                    # entry plus the round trip in fees
    assert moved.stop_px == pytest.approx(100.0 * (1 + 2 * 0.00075), rel=1e-4)
    assert state.initial_stop(pos.id) == 96.0       # R still measured from the real risk
    assert any("breakeven" in line for line in notes.lines)


def test_breakeven_never_moves_a_stop_backwards(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    state.add_position("SOL", "short", 100.0, 1.0, 100.0, 10.0, 104.0, 88.0,
                       0.8, "own", rationale="r", invalidation="i",
                       margin_mode="isolated")
    state.update_brackets(1, 97.0, 88.0)            # already trailed below breakeven
    eng.manage_positions({"SOL": 95.0})
    assert state.open_position_for("SOL").stop_px == 97.0


def test_time_stop_closes_a_position_that_went_nowhere(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", rationale="r", invalidation="i",
                       margin_mode="isolated")
    state.db.execute("UPDATE positions SET opened_ts=? WHERE id=1",
                     (time.time() - 4 * 3600,))
    state.db.commit()
    eng.manage_positions({"SOL": 100.4})            # +0.1R after four hours
    assert state.open_positions() == []
    closed = state.recent_closes(1)[0]
    assert closed["close_reason"] == "analyst"
    assert any("time stop" in line for line in notes.lines)


def test_time_stop_leaves_a_working_position_alone(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", rationale="r", invalidation="i",
                       margin_mode="isolated")
    state.db.execute("UPDATE positions SET opened_ts=? WHERE id=1",
                     (time.time() - 4 * 3600,))
    state.db.commit()
    eng.manage_positions({"SOL": 103.0})            # +0.75R — working, keep it
    assert len(state.open_positions()) == 1


def test_adaptive_cadence_speeds_up_on_a_hot_tape(tmp_path):
    eng, _, _, _ = mk_rails(tmp_path, [])
    eng._universe_heat = 0.4
    assert eng.next_cycle_secs() == 900
    eng._universe_heat = 1.4
    assert eng.next_cycle_secs() == 300


def test_chasing_the_range_edge_is_refused_end_to_end(tmp_path):
    chase = {"actions": [{"kind": "open", "market": "SOL", "side": "long",
                          "conviction": 0.85, "stop": 97.0, "take_profit": 112.0,
                          "leverage": 10, "margin_mode": "isolated", "source": "own",
                          "rationale": "it is ripping", "invalidation": "no"}]}
    eng, state, _, _ = mk_rails(tmp_path, [chase])

    def hot(name, ctx):
        f = FakeMarket.features(eng.market, name, ctx)
        f["range24h_pos"] = 0.97
        return f

    eng.market.features = hot
    eng.cycle("scheduled")
    assert state.open_positions() == []
    assert state.resting_entries() == []
    refusal = state.recent_refusals(1)[0]
    assert "chasing the high" in refusal["reason"]


def test_a_failing_ledger_write_reports_the_real_error_not_a_typeerror(tmp_path):
    """Regression: the error path spread the result dict (which carries
    execution_id) over _mark_execution_problem's positional arg, so a genuine
    finalization failure surfaced as a TypeError with the cause lost."""
    eng, state, notes, _ = mk_rails(tmp_path, [])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", rationale="r", invalidation="i",
                       margin_mode="isolated")

    def boom(*a, **kw):
        raise RuntimeError("ledger is on fire")

    state.finalize_close_action = boom
    eng.cfg.mode = "live"          # the stakes case: the venue moved, the ledger did not
    decision_id = state.record_decision("test", "", "[]", "m", 0, "ok")
    eng.exec_close(CloseAction(market="SOL", rationale="close it"),
                   {"SOL": 100.4}, 1000.0, utc_day(time.time()), frozenset(),
                   decision_id=decision_id)
    journal = state.unfinished_action_executions()
    assert len(journal) == 1
    assert journal[0]["status"] == "needs_reconciliation"
    assert "ledger is on fire" in journal[0]["result"]["reason"]
    assert any("ledger is on fire" in line for line in notes.lines), notes.lines


# -- memory: the only thing that survives a cycle --------------------------
REMEMBER = {"actions": [{"kind": "remember", "market": "xyz:NVDA",
                         "lesson": "Post-earnings gap-downs bounce for the first "
                                   "hour; short the retest, never the low."}]}


def test_the_analyst_can_write_a_lesson_and_reads_it_back_next_cycle(tmp_path):
    eng, state, notes, analyst = mk_rails(tmp_path, [REMEMBER, {"actions": []}])
    eng.cycle("scheduled")
    lessons = state.lessons()
    assert len(lessons) == 1
    assert lessons[0]["market"] == "xyz:NVDA" and lessons[0]["source"] == "analyst"
    assert any("learned [xyz:NVDA]" in line for line in notes.lines)

    eng.cycle("scheduled")
    bundle = analyst.bundles[-1]
    assert bundle["lessons"][0]["text"].startswith("Post-earnings gap-downs")


def test_a_repeated_lesson_is_not_new_knowledge(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [REMEMBER, REMEMBER])
    eng.cycle("scheduled")
    eng.cycle("scheduled")
    assert len(state.lessons()) == 1
    assert sum("learned" in line for line in notes.lines) == 1


def test_a_lesson_tagged_to_an_unknown_market_is_refused(tmp_path):
    bad = {"actions": [{"kind": "remember", "market": "xyz:FAKE",
                        "lesson": "something about a market that does not exist"}]}
    eng, state, _, _ = mk_rails(tmp_path, [bad])
    eng.cycle("scheduled")
    assert state.lessons() == []
    assert "unknown market" in state.recent_refusals(1)[0]["reason"]


def test_remembering_needs_no_conviction_gate_or_venue_call(tmp_path):
    """A lesson is free: it must work while paused, at the daily cap, and with
    the kill switch tripped — the moments the bot most needs to learn."""
    eng, state, _, _ = mk_rails(tmp_path, [REMEMBER])
    state.set_paused(True)
    state.trip_kill(utc_day(time.time()))
    eng.cycle("scheduled")
    assert len(state.lessons()) == 1


def test_entry_context_is_captured_so_losses_attribute_to_a_pattern(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [OPEN_SOL, REST_SOL])
    eng.cycle("scheduled")
    row = dict(state.db.execute(
        "SELECT entry_style, entry_range_pos, entry_atr_pct, entry_trigger"
        " FROM positions WHERE market='SOL'").fetchone())
    assert row["entry_style"] == "market"
    assert row["entry_range_pos"] == 0.5          # what FakeMarket reported
    assert row["entry_atr_pct"] == 0.4
    assert row["entry_trigger"] == "scheduled"

    # ...and a resting entry carries the snapshot from when it was DECIDED
    state.close_position(1, "tp", 107.0, 3.0)
    eng.cycle("caller message")
    pending = state.resting_entries()[0]
    assert json.loads(pending["entry_context_json"])["style"] == "resting"
    assert json.loads(pending["entry_context_json"])["trigger"] == "caller message"


def test_the_bundle_carries_the_measured_record(tmp_path):
    eng, state, _, analyst = mk_rails(tmp_path, [{"actions": []}])
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 98.0, 110.0,
                             0.8, "own", entry_context={"style": "market",
                                                        "range_pos": 0.9,
                                                        "atr_pct": 0.4,
                                                        "trigger": "scheduled"})
    state.close_position(pos.id, "sl", 98.0, -2.1)
    eng.cycle("scheduled")
    perf = analyst.bundles[-1]["performance"]
    assert perf["overall"]["n"] == 1 and perf["overall"]["wins"] == 0
    assert perf["by_range_position"]["top of range (>=0.80)"]["pnl"] == pytest.approx(-2.1)


# -- fee attribution: HL reports closedPnl GROSS ---------------------------
def venue_fill(tid, coin, direction, px, sz, fee, closed_pnl=0.0, ts_ms=1_787_900_000_000):
    return {"tid": tid, "coin": coin, "dir": direction, "px": str(px), "sz": str(sz),
            "fee": str(fee), "closedPnl": str(closed_pnl), "time": ts_ms}


def test_venue_history_charges_both_sides_of_the_round_trip(tmp_path):
    """The dashboard showed +$2.31 on a book that had actually made +$1.28: it
    subtracted the closing fee and silently kept the opening one."""
    eng, _, _, _ = mk_rails(tmp_path, [], mode="live")
    fills = [
        venue_fill("o1", "BTC", "Open Short", 77720, 0.00515, 0.3002),
        venue_fill("c1", "BTC", "Close Short", 77572, 0.00515, 0.2996, 0.7622),
    ]
    closes, total = eng._venue_close_history(fills)
    assert len(closes) == 1
    row = closes[0]
    assert row["gross_pnl"] == pytest.approx(0.7622)
    assert row["exit_fee"] == pytest.approx(0.2996)
    assert row["entry_fee"] == pytest.approx(0.3002)
    assert row["realized_pnl"] == pytest.approx(0.7622 - 0.2996 - 0.3002)
    assert total == pytest.approx(0.1624)      # not 0.4626


def test_venue_history_totals_match_the_balance_change(tmp_path):
    """Aggregate must equal sum(closedPnl) - sum(EVERY fee), which is what the
    USDC balance actually moved by."""
    eng, _, _, _ = mk_rails(tmp_path, [], mode="live")
    fills = [
        venue_fill("o1", "SOL", "Open Long", 105.41, 0.6, 0.0664),
        venue_fill("c1", "SOL", "Close Long", 109.13, 0.6, 0.0688, 2.2517),
        venue_fill("o2", "BTC", "Open Long", 80464, 0.0011, 0.0929),
        venue_fill("c2", "BTC", "Close Long", 79870, 0.0011, 0.0922, -0.6534),
    ]
    _, total = eng._venue_close_history(fills)
    gross = 2.2517 - 0.6534
    every_fee = 0.0664 + 0.0688 + 0.0929 + 0.0922
    assert total == pytest.approx(gross - every_fee)


def test_an_open_position_does_not_realize_its_entry_fee_yet(tmp_path):
    eng, _, _, _ = mk_rails(tmp_path, [], mode="live")
    fills = [venue_fill("o1", "BTC", "Open Short", 77720, 0.00515, 0.3002)]
    closes, total = eng._venue_close_history(fills)
    assert closes == [] and total == 0.0


class FillsAdapter(DryRunAdapter):
    def __init__(self, state, bankroll, fills):
        super().__init__(state, bankroll)
        self._fills = fills

    def fills(self):
        return list(self._fills)


def live_engine(tmp_path, fills):
    eng, state, notes, analyst = mk_rails(tmp_path, [], mode="live")
    eng.adapter = FillsAdapter(state, 1000.0, fills)
    return eng, state, notes


def test_reconcile_records_a_close_net_of_the_entry_fee(tmp_path):
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [
        venue_fill("o1", "SOL", "Open Long", 100.0, 1.0, 0.105, ts_ms=now_ms - 60_000),
    ])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 110.0,
                       0.8, "own", rationale="r", invalidation="i",
                       margin_mode="isolated")
    eng.reconcile_live()
    assert state.entry_fee(1) == pytest.approx(0.105)

    eng.adapter._fills.append(
        venue_fill("c1", "SOL", "Close Long", 110.0, 1.0, 0.1155, 10.0, ts_ms=now_ms))
    eng.reconcile_live()
    closed = state.recent_closes(1)[0]
    assert closed["realized_pnl"] == pytest.approx(10.0 - 0.1155 - 0.105)
    assert any("+9.78" in line for line in notes.lines), notes.lines


def test_an_entry_fee_waits_for_its_ledger_row_instead_of_being_lost(tmp_path):
    """A resting entry fills before reconcile_pending_entries creates the row.
    The opening fill must not be marked seen and dropped."""
    now_ms = int(time.time() * 1000)
    eng, state, _ = live_engine(tmp_path, [
        venue_fill("o1", "SOL", "Open Long", 100.0, 1.0, 0.105, ts_ms=now_ms),
    ])
    eng.reconcile_live()
    assert state.fill_seen("o1") is False        # deferred, not discarded

    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 110.0,
                       0.8, "own", margin_mode="isolated")
    eng.reconcile_live()
    assert state.entry_fee(1) == pytest.approx(0.105)
    assert state.fill_seen("o1") is True


def test_an_orphan_entry_fill_is_eventually_dropped_not_retried_forever(tmp_path):
    old_ms = int((time.time() - 2 * 3600) * 1000)
    eng, state, _ = live_engine(tmp_path, [
        venue_fill("o1", "SOL", "Open Long", 100.0, 1.0, 0.105, ts_ms=old_ms),
    ])
    eng.reconcile_live()
    assert state.fill_seen("o1") is True


def test_a_partial_close_only_realizes_its_share_of_the_entry_fee(tmp_path):
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [
        venue_fill("o1", "SOL", "Open Long", 100.0, 2.0, 0.21, ts_ms=now_ms - 60_000),
    ])
    state.add_position("SOL", "long", 100.0, 2.0, 200.0, 10.0, 96.0, 110.0,
                       0.8, "own", margin_mode="isolated")
    eng.reconcile_live()
    eng.adapter._fills.append(
        venue_fill("c1", "SOL", "Close Long", 110.0, 1.0, 0.11, 10.0, ts_ms=now_ms))
    eng.reconcile_live()
    assert state.open_position_for("SOL").size == pytest.approx(1.0)
    assert state.entry_fee(1) == pytest.approx(0.105)   # half stays with the rest
    assert any("net of both sides" in line for line in notes.lines)


# -- chat sees and uses everything the cycle does --------------------------
def test_chat_context_carries_memory_record_resting_and_pause(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [REST_SOL])
    eng.cycle("scheduled")                       # park a resting entry
    pos = state.add_position("BTC", "long", 100.0, 1.0, 100.0, 10.0, 98.0, 110.0,
                             0.8, "own", entry_context={"style": "market",
                                                        "range_pos": 0.9,
                                                        "atr_pct": 0.4,
                                                        "trigger": "scheduled"})
    state.close_position(pos.id, "sl", 98.0, -2.1)
    state.add_lesson("Do not chase the range edge.", source="operator", pinned=True)
    state.set_paused(True)

    captured = install_chat(eng, answer="Here is the state.")
    eng.chat("what does my record say?")
    bundle = captured["bundle"]
    assert bundle["paused"] is True
    assert bundle["performance"]["overall"]["n"] == 1
    assert bundle["lessons"][0]["text"] == "Do not chase the range edge."
    assert len(bundle["resting_entries"]) == 1

    from peri.analyst import build_chat_context
    rendered = build_chat_context(bundle)
    assert "YOUR MEASURED RECORD" in rendered
    assert "YOUR MEMORY" in rendered
    assert "YOUR RESTING ENTRIES" in rendered
    assert "PAUSED BY THE OPERATOR" in rendered


def test_a_lesson_from_chat_applies_immediately_without_confirmation(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    install_chat(eng, proposal={"kind": "remember", "market": "BTC",
                                "lesson": "BTC weekends chop; do not size up into them."},
                 answer="Noted.")
    out = eng.chat("remember that btc chops on weekends")
    assert out["proposal"] is None               # nothing to confirm — no money moves
    lessons = state.lessons()
    assert len(lessons) == 1 and lessons[0]["market"] == "BTC"
    assert "Remembered:" in out["message"]["content"]
    assert out["message"]["metadata"]["lesson"]["stored"] is True


def test_a_duplicate_lesson_from_chat_says_so(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    state.add_lesson("BTC weekends chop; do not size up into them.")
    install_chat(eng, proposal={"kind": "remember",
                                "lesson": "btc weekends CHOP; do not size up into them."},
                 answer="Noted.")
    out = eng.chat("remember it again")
    assert len(state.lessons()) == 1
    assert "Already remembered:" in out["message"]["content"]


def test_chat_can_propose_a_resting_entry(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    install_chat(eng, proposal={"kind": "open", "market": "SOL", "side": "long",
                                "conviction": 0.8, "entry": 97.5, "stop": 94.0,
                                "take_profit": 108.0, "leverage": 10,
                                "margin_mode": "isolated", "source": "own",
                                "rationale": "buy the pullback",
                                "invalidation": "loses 94"},
                 answer="Resting entry prepared.")
    out = eng.chat("short sol on a bounce")
    assert out["proposal"] is not None
    preview = out["proposal"]["preview"]
    assert preview["resting"] is True and preview["entry_px"] == 97.5
    assert preview["notional"] == pytest.approx(preview["size"] * 97.5, rel=1e-3)


def test_pause_blocks_a_chat_authorized_entry_too(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    state.set_paused(True)
    install_chat(eng, proposal={"kind": "open", "market": "SOL", "side": "long",
                                "conviction": 0.9, "stop": 94.0,
                                "take_profit": 112.0, "leverage": 10,
                                "margin_mode": "isolated", "source": "own",
                                "rationale": "owner asked", "invalidation": "94"},
                 answer="Trying.")
    out = eng.chat("open sol for me")
    assert out["proposal"] is None
    assert "paused by operator" in out["message"]["content"]


def test_a_paper_resting_entry_survives_until_it_fills_or_expires(tmp_path):
    """Regression: the paper adapter had no order book, so reconciliation could
    not see the entry it had just placed and settled it as vanished on the very
    next cycle — resting entries were unusable in dry mode."""
    eng, state, _, _ = mk_rails(tmp_path, [REST_SOL, {"actions": []},
                                           {"actions": []}, {"actions": []}])
    eng.cycle("scheduled")
    entry = state.resting_entries()[0]
    assert eng.adapter.open_orders_all()[0]["oid"] == entry["oid"]

    eng.cycle("scheduled")          # mark still 100.0, nowhere near 97.5
    eng.cycle("scheduled")
    assert len(state.resting_entries()) == 1
    assert state.open_positions() == []

    eng.market._ctxs["SOL"] = Ctx("SOL", 97.4, 3.0, 11.0, 1e8, 5e8)
    eng.cycle("scheduled")
    assert state.resting_entries() == []
    assert len(state.open_positions()) == 1
    assert eng.adapter.open_orders_all() == []      # the book is clean again


def test_pausing_clears_the_paper_book_too(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [REST_SOL])
    eng.cycle("scheduled")
    assert len(eng.adapter.open_orders_all()) == 1
    eng.set_paused(True)
    assert eng.adapter.open_orders_all() == []
    assert state.resting_entries() == []


# -- 2026-08-29 audit: protection, locking, recovery ----------------------
class BracketSpyAdapter(DryRunAdapter):
    def __init__(self, state, bankroll):
        super().__init__(state, bankroll)
        self.bracket_cancels = []

    def cancel_brackets(self, market):
        self.bracket_cancels.append(market)


def test_pause_never_strips_a_live_position_of_its_brackets(tmp_path):
    """cancel_brackets is market-wide. A resting entry that partially filled
    leaves a REAL position on that market; pausing must not disarm it."""
    eng, state, _, _ = mk_rails(tmp_path, [REST_SOL], mode="live")
    eng.adapter = BracketSpyAdapter(state, 1000.0)
    eng.state.add_pending_entry(
        market="SOL", side="long", entry_px=97.5, size=1.0, notional=97.5,
        leverage=10.0, margin_mode="isolated", stop_px=94.0, tp_px=108.0,
        conviction=0.8, rationale="r", invalidation="i", oid=1,
        decision_id=None, expires_ts=time.time() + 3600)
    state.add_position("SOL", "long", 97.5, 0.4, 39.0, 10.0, 94.0, 108.0,
                       0.8, "own", margin_mode="isolated")   # the partial fill

    eng.set_paused(True)
    assert eng.adapter.bracket_cancels == []        # the position keeps its stop
    assert state.open_position_for("SOL").stop_px == 94.0
    assert state.resting_entries() == []            # the remainder is still cancelled


def test_orphan_brackets_are_cleared_when_nothing_is_open(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [], mode="live")
    eng.adapter = BracketSpyAdapter(state, 1000.0)
    state.add_pending_entry(
        market="SOL", side="long", entry_px=97.5, size=1.0, notional=97.5,
        leverage=10.0, margin_mode="isolated", stop_px=94.0, tp_px=108.0,
        conviction=0.8, rationale="r", invalidation="i", oid=1,
        decision_id=None, expires_ts=time.time() + 3600)
    eng.set_paused(True)
    assert eng.adapter.bracket_cancels == ["SOL"]


def test_split_take_profit_tranches_do_not_kill_the_cycle(tmp_path):
    """manage_trade.py places one stop and several TP tranches on purpose; the
    engine used to raise ContextError and skip every cycle until they cleared."""
    eng, state, _, _ = mk_rails(tmp_path, [], mode="live")
    state.add_position("BTC", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 110.0,
                       0.8, "own", margin_mode="isolated")
    orders = [
        {"coin": "BTC", "side": "A", "reduceOnly": True, "isTrigger": True,
         "orderType": "Stop Market", "triggerPx": "96.0", "oid": 1},
        {"coin": "BTC", "side": "A", "reduceOnly": True, "isTrigger": True,
         "orderType": "Take Profit Market", "triggerPx": "110.0", "oid": 2},
        {"coin": "BTC", "side": "A", "reduceOnly": True, "isTrigger": True,
         "orderType": "Take Profit Market", "triggerPx": "125.0", "oid": 3},
    ]
    eng.sync_live_brackets(orders)                  # must not raise
    pos = state.open_position_for("BTC")
    assert pos.stop_px == 96.0
    assert pos.tp_px == 110.0                       # the NEAREST tranche, not 125


def test_a_close_that_never_reached_the_venue_unblocks_entries(tmp_path):
    """One flaky HTTP call used to leave a manual_review row that refused every
    future entry forever, with no way out but sqlite."""
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    pos = state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 110.0,
                             0.8, "own", margin_mode="isolated")
    action = CloseAction(market="SOL", rationale="close it")
    execution_id = "stuck-close"
    state.create_action_execution(
        execution_id, origin="autonomous", kind="close", proposal_id=None,
        action=action.model_dump(mode="json"),
        pre_state={"position": eng._position_fingerprint(pos)},
        expected={}, decision_id=None, now=time.time() - 600)
    state.update_action_execution(execution_id, stage="submitted",
                                  status="needs_reconciliation",
                                  submission_ts=time.time() - 600,
                                  response_ts=time.time() - 600)
    assert len(state.unfinished_action_executions()) == 1

    eng._reconcile_close_execution(state.unfinished_action_executions()[0], action, [])
    assert state.unfinished_action_executions() == []
    assert state.open_position_for("SOL") is not None      # untouched
    assert any("did not execute" in line for line in notes.lines)


def test_an_operator_can_resolve_a_stuck_execution(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    state.create_action_execution(
        "wedged", origin="autonomous", kind="close", proposal_id=None,
        action={"kind": "close", "market": "SOL", "rationale": "x"},
        pre_state={"position": None}, expected={}, decision_id=None)
    state.update_action_execution("wedged", stage="x", status="manual_review")
    assert len(state.unfinished_action_executions()) == 1
    eng.resolve_execution("wedged", reason="checked the venue by hand")
    assert state.unfinished_action_executions() == []
    with pytest.raises(ProposalError):
        eng.resolve_execution("does-not-exist")


def test_a_live_resting_fill_consumes_a_daily_entry_slot(tmp_path):
    """adopt_external runs first every live cycle, so the fill arrives as an
    already-adopted row — the branch that used to skip count_entry."""
    eng, state, _, _ = mk_rails(tmp_path, [], mode="live")
    day = utc_day(time.time())
    state.open_day(day, 1000.0)
    entry_id = state.add_pending_entry(
        market="SOL", side="long", entry_px=97.5, size=1.0, notional=97.5,
        leverage=10.0, margin_mode="isolated", stop_px=94.0, tp_px=108.0,
        conviction=0.8, rationale="pullback", invalidation="loses 94", oid=1,
        decision_id=None, expires_ts=time.time() + 3600,
        entry_context={"style": "resting", "range_pos": 0.31, "atr_pct": 0.4,
                       "trigger": "price move"})
    state.add_position("SOL", "long", 97.5, 1.0, 97.5, 10.0, None, None,
                       None, "external", margin_mode="isolated")
    before = state.entries_today(day)

    eng._account_snapshot = {"positions": []}
    eng.reconcile_pending_entries({"SOL": 97.5}, [])

    assert state.entries_today(day) == before + 1
    claimed = state.open_position_for("SOL")
    assert claimed.source == "own" and claimed.stop_px == 94.0
    row = dict(state.db.execute(
        "SELECT entry_style, entry_range_pos, entry_trigger FROM positions"
        " WHERE id=?", (claimed.id,)).fetchone())
    assert row["entry_style"] == "resting"          # attribution survives the claim
    assert row["entry_range_pos"] == 0.31
    assert row["entry_trigger"] == "price move"
    assert state.recent_pending_entries(1)[0]["outcome"] == "filled"
    assert entry_id == state.recent_pending_entries(1)[0]["id"]


def test_a_round_trip_between_reconciles_keeps_its_pnl_and_cooldown(tmp_path):
    """The stop fired minutes after a resting entry filled, both inside one
    cycle. The close used to be discarded: no PnL, no record, and no stop
    cooldown — so the analyst could re-enter the market that just stopped it."""
    now_ms = int(time.time() * 1000)
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    state.open_day(utc_day(time.time()), 1000.0)
    state.add_pending_entry(
        market="SOL", side="long", entry_px=100.0, size=1.0, notional=100.0,
        leverage=10.0, margin_mode="isolated", stop_px=96.0, tp_px=112.0,
        conviction=0.8, rationale="pullback", invalidation="loses 96", oid=1,
        decision_id=None, expires_ts=time.time() + 3600,
        entry_context={"style": "resting", "range_pos": 0.4, "atr_pct": 0.5,
                       "trigger": "price move"})
    eng.adapter = FillsAdapter(state, 1000.0, [
        venue_fill("o1", "SOL", "Open Long", 100.0, 1.0, 0.105, ts_ms=now_ms - 120_000),
        venue_fill("c1", "SOL", "Close Long", 96.0, 1.0, 0.101, -4.0, ts_ms=now_ms),
    ])
    eng.reconcile_live()

    closed = state.recent_closes(1)[0]
    assert closed["market"] == "SOL"
    assert closed["entry_px"] == pytest.approx(100.0)
    assert closed["close_reason"] == "sl"
    assert closed["realized_pnl"] == pytest.approx(-4.101)
    # the cooldown is the point: a losing stop must lock the market out
    assert state.cooldown_until("SOL", time.time()) is not None
    assert state.entries_today(utc_day(time.time())) == 1
    assert state.resting_entries() == []
    assert any("reconstructed" in line for line in notes.lines)


def test_a_position_that_lost_its_venue_brackets_is_re_armed_and_reported(tmp_path):
    """'Code owns risk' quietly stopped being true when a stop vanished: the
    engine recorded NULL and every automatic rule then declined to act."""
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    placed = []

    class Rearm(DryRunAdapter):
        def place_brackets(self, market, side, size, stop_px, tp_px):
            placed.append((market, side, size, stop_px, tp_px))
            return [{"oid": 1, "kind": "sl"}, {"oid": 2, "kind": "tp"}]

    eng.adapter = Rearm(state, 1000.0)
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", margin_mode="isolated")
    eng.sync_live_brackets([])                      # the venue shows nothing

    assert placed == [("SOL", "long", 1.0, 96.0, 112.0)]
    restored = state.open_position_for("SOL")
    assert restored.stop_px == 96.0 and restored.tp_px == 112.0
    assert any("re-armed SOL" in line for line in notes.lines)


def test_an_unprotectable_position_is_shouted_about_not_ignored(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, None, None,
                       None, "own", margin_mode="isolated")
    eng.sync_live_brackets([])
    assert any("UNPROTECTED: SOL" in line for line in notes.lines)


def test_re_arming_failure_is_reported_rather_than_swallowed(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")

    class Broken(DryRunAdapter):
        def place_brackets(self, *a, **kw):
            raise RuntimeError("venue rejected")

    eng.adapter = Broken(state, 1000.0)
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", margin_mode="isolated")
    eng.sync_live_brackets([])
    assert any("re-arming FAILED" in line for line in notes.lines)


def test_an_external_position_is_never_silently_re_armed(tmp_path):
    eng, state, notes, _ = mk_rails(tmp_path, [], mode="live")
    placed = []

    class Rearm(DryRunAdapter):
        def place_brackets(self, market, side, size, stop_px, tp_px):
            placed.append(market)
            return []

    eng.adapter = Rearm(state, 1000.0)
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       None, "external", margin_mode="isolated")
    eng.sync_live_brackets([])
    assert placed == []


def test_a_slipped_stop_is_still_a_stop_and_earns_the_long_cooldown(tmp_path):
    """Classifying by 1% price proximity downgraded any stop that slipped
    further to 'external', which halved the cooldown on exactly the violent
    moves it exists for."""
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", margin_mode="isolated")
    eng.adapter._fills = [
        venue_fill("c1", "SOL", "Close Long", 92.0, 1.0, 0.096, -8.0, ts_ms=now_ms),
    ]
    eng.reconcile_live()
    assert state.recent_closes(1)[0]["close_reason"] == "sl"
    remaining = state.cooldown_until("SOL", time.time()) - time.time()
    assert remaining > 3600            # the 4h stop cooldown, not the 1h default


def test_a_take_profit_tranche_reaches_the_ledger(tmp_path):
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [])
    state.add_position("BTC", "short", 77720.0, 0.00515, 400.0, 10.0, 78950.0,
                       75700.0, 0.8, "own", margin_mode="isolated")
    eng.adapter._fills = [
        venue_fill("c1", "BTC", "Close Short", 75700.0, 0.00258, 0.2, 5.21, ts_ms=now_ms),
    ]
    eng.reconcile_live()
    still_open = state.open_position_for("BTC")
    assert still_open.size == pytest.approx(0.00257)
    tranche = state.recent_closes(1)[0]
    assert tranche["market"] == "BTC" and tranche["close_reason"] == "tp"
    assert tranche["realized_pnl"] == pytest.approx(5.01)
    assert state.realized_total() == pytest.approx(5.01)
    assert state.performance_digest()["overall"]["n"] == 1


def test_markets_whose_minimum_lot_is_unaffordable_are_not_offered(tmp_path):
    """A coarse-lot market at a high price is structurally untradeable on a
    small account; showing it burns a cycle on 'size rounds to zero'."""
    eng, state, _, analyst = mk_rails(tmp_path, [{"actions": []}])
    eng.market._ctxs["xyz:WHALE"] = Ctx("xyz:WHALE", 3400.0, 1.0, 5.0, 1e8, 5e8)

    class CoarseLot:
        sz_decimals = 0
        max_leverage = 10

    real_info = eng.market.info
    eng.market.info = lambda n: CoarseLot() if n == "xyz:WHALE" else real_info(n)
    eng.market.candidates = lambda a, f, t, must: ["BTC", "SOL", "xyz:WHALE"] + [
        m for m in must if m not in ("BTC", "SOL", "xyz:WHALE")]

    eng.cycle("scheduled")
    offered = {c["name"] for c in analyst.bundles[-1]["candidates"]}
    assert "xyz:WHALE" not in offered          # one lot = $3,400 on a $1k book
    assert {"BTC", "SOL"} <= offered


def test_a_market_we_hold_is_always_offered_however_coarse(tmp_path):
    eng, state, _, analyst = mk_rails(tmp_path, [{"actions": []}])
    eng.market._ctxs["xyz:WHALE"] = Ctx("xyz:WHALE", 3400.0, 1.0, 5.0, 1e8, 5e8)

    class CoarseLot:
        sz_decimals = 0
        max_leverage = 10

    real_info = eng.market.info
    eng.market.info = lambda n: CoarseLot() if n == "xyz:WHALE" else real_info(n)
    eng.market.candidates = lambda a, f, t, must: ["BTC"] + list(must)
    state.add_position("xyz:WHALE", "long", 3400.0, 1.0, 3400.0, 10.0, 3300.0,
                       3600.0, 0.8, "own", margin_mode="isolated")

    eng.cycle("scheduled")
    offered = {c["name"] for c in analyst.bundles[-1]["candidates"]}
    assert "xyz:WHALE" in offered              # you can never be blind to what you hold


def test_an_analyst_close_is_not_booked_twice_when_its_fill_arrives(tmp_path):
    """exec_close records from the adapter response and cannot know the fill's
    tid, so the venue fill always shows up afterwards with no open position to
    match. The orphan reconstruction must recognise its own echo — otherwise
    every analyst close becomes a second, phantom trade in the measured record."""
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [])
    pos = state.add_position("BTC", "long", 78380.0, 0.00181, 141.9, 10.0,
                             76750.0, 82450.0, 0.76, "own", margin_mode="isolated",
                             entry_context={"style": "resting", "range_pos": 0.62,
                                            "atr_pct": 0.3, "trigger": "scheduled"})
    state.close_position(pos.id, "analyst", 78664.0, 0.3009)   # the engine's own close
    assert state.realized_total() == pytest.approx(0.3009)

    eng.adapter._fills = [venue_fill("c1", "BTC", "Close Long", 78664.0, 0.00181,
                                     0.1068, 0.5140, ts_ms=now_ms)]
    eng.reconcile_live()

    assert len(state.recent_closes(10)) == 1            # still ONE trade
    assert state.realized_total() == pytest.approx(0.3009)
    assert state.fill_seen("c1") is True                # the echo was consumed
    assert not any("reconstructed" in line for line in notes.lines)


def test_a_genuinely_unaccounted_close_is_still_reconstructed(tmp_path):
    """The guard must not swallow a real orphan: a different market, or a close
    with no matching ledger row, still has to be recovered."""
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [])
    state.open_day(utc_day(time.time()), 1000.0)
    pos = state.add_position("BTC", "long", 78380.0, 0.00181, 141.9, 10.0,
                             76750.0, 82450.0, 0.76, "own", margin_mode="isolated")
    state.close_position(pos.id, "analyst", 78664.0, 0.3009)
    eng.adapter._fills = [venue_fill("c2", "SOL", "Close Long", 104.0, 1.0,
                                     0.078, 4.0, ts_ms=now_ms)]
    eng.reconcile_live()
    assert len(state.recent_closes(10)) == 2
    assert state.recent_closes(1)[0]["market"] == "SOL"
    assert any("reconstructed" in line for line in notes.lines)


def test_the_same_market_reopened_later_is_not_mistaken_for_an_echo(tmp_path):
    """A second, genuinely new round trip on the same market at a very different
    price must still be reconstructed."""
    now_ms = int(time.time() * 1000)
    eng, state, notes = live_engine(tmp_path, [])
    state.open_day(utc_day(time.time()), 1000.0)
    pos = state.add_position("BTC", "long", 78380.0, 0.00181, 141.9, 10.0,
                             76750.0, 82450.0, 0.76, "own", margin_mode="isolated")
    state.close_position(pos.id, "analyst", 78664.0, 0.3009)
    eng.adapter._fills = [venue_fill("c3", "BTC", "Close Long", 81000.0, 0.00181,
                                     0.11, 4.7, ts_ms=now_ms)]
    eng.reconcile_live()
    assert len(state.recent_closes(10)) == 2


def test_a_close_cancels_a_resting_entry_whose_thesis_has_died(tmp_path):
    """2026-08-30 20:19Z: the analyst read the IRGC strike headlines, tried to
    pull its resting BTC long with a close action, and was told 'no open
    position'. The order it wanted gone filled 42 minutes later."""
    eng, state, notes, _ = mk_rails(tmp_path, [REST_SOL, {"actions": []}])
    eng.cycle("scheduled")
    assert len(state.resting_entries()) == 1
    assert len(eng.adapter.open_orders_all()) == 1

    decision_id = state.record_decision("test", "", "[]", "m", 0, "ok")
    eng.exec_close(
        CloseAction(market="SOL",
                    rationale="fresh risk-off headline kills the dip-buy thesis"),
        {"SOL": 100.0}, 1000.0, utc_day(time.time()), frozenset(),
        decision_id=decision_id)

    assert state.resting_entries() == []
    assert eng.adapter.open_orders_all() == []
    assert state.recent_pending_entries(1)[0]["outcome"] == "cancelled"
    assert any("cancelled resting long SOL" in line for line in notes.lines)
    assert state.open_positions() == []            # nothing was ever opened


def test_closing_a_market_with_neither_position_nor_entry_still_refuses(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    decision_id = state.record_decision("test", "", "[]", "m", 0, "ok")
    eng.exec_close(CloseAction(market="SOL", rationale="nothing here"),
                   {"SOL": 100.0}, 1000.0, utc_day(time.time()), frozenset(),
                   decision_id=decision_id)
    reason = state.recent_refusals(1)[0]["reason"]
    assert "no open position or resting entry" in reason


def test_a_real_position_still_closes_normally_when_an_entry_also_rests(tmp_path):
    """A position takes precedence: the close must not be downgraded to a cancel."""
    eng, state, notes, _ = mk_rails(tmp_path, [])
    state.add_position("SOL", "long", 100.0, 1.0, 100.0, 10.0, 96.0, 112.0,
                       0.8, "own", margin_mode="isolated")
    state.add_pending_entry(
        market="SOL", side="long", entry_px=97.5, size=1.0, notional=97.5,
        leverage=10.0, margin_mode="isolated", stop_px=94.0, tp_px=108.0,
        conviction=0.8, rationale="r", invalidation="i", oid=1,
        decision_id=None, expires_ts=time.time() + 3600)
    decision_id = state.record_decision("test", "", "[]", "m", 0, "ok")
    eng.exec_close(CloseAction(market="SOL", rationale="thesis done"),
                   {"SOL": 104.0}, 1000.0, utc_day(time.time()), frozenset(),
                   decision_id=decision_id)
    assert state.open_positions() == []
    assert len(state.resting_entries()) == 1       # the order is untouched


def test_the_preview_says_where_the_venue_would_liquidate(tmp_path):
    """The analyst picks leverage and margin mode while seeing nothing about
    where either puts liquidation — it got refused rather than informed."""
    eng, state, _, _ = mk_rails(tmp_path, [])
    action = OpenAction(market="SOL", side="long", conviction=0.8, stop=97.0,
                        take_profit=112.0, leverage=10, margin_mode="isolated",
                        rationale="r", invalidation="i")
    eng._features_cache["SOL"] = {"range24h_pos": 0.5, "atr15m_pct": 0.4,
                                  "hi_24h": 110.0, "lo_24h": 95.0}
    eng._account_snapshot = {"available_margin": 1000.0, "equity": 1000.0}
    _, preview = eng._preview_action(action, {"SOL": 100.0}, 1000.0,
                                     utc_day(time.time()), frozenset())
    # FakeMarket reports maxLev 20, so 10x isolated liquidates ~7.5% below entry
    assert preview["liquidation_px"] == pytest.approx(100.0 * (1 - 0.075), rel=1e-3)
    assert preview["stop_inside_liquidation_by"] == pytest.approx(0.075 - 0.03, abs=1e-6)


def test_a_cross_margin_preview_reports_no_isolated_liquidation(tmp_path):
    eng, state, _, _ = mk_rails(tmp_path, [])
    action = OpenAction(market="SOL", side="long", conviction=0.8, stop=97.0,
                        take_profit=112.0, leverage=10, margin_mode="cross",
                        rationale="r", invalidation="i")
    eng._features_cache["SOL"] = {"range24h_pos": 0.5, "atr15m_pct": 0.4,
                                  "hi_24h": 110.0, "lo_24h": 95.0}
    eng._account_snapshot = {"available_margin": 1000.0, "equity": 1000.0}
    _, preview = eng._preview_action(action, {"SOL": 100.0}, 1000.0,
                                     utc_day(time.time()), frozenset())
    assert preview["liquidation_px"] is None


def test_the_guard_returns_the_exact_venue_lot(tmp_path):
    """One rounding site: what the guard approves is what is sent. A coarse lot
    that would risk more than approved is refused, not silently taken."""
    eng, state, _, _ = mk_rails(tmp_path, [])
    action = OpenAction(market="SOL", side="long", conviction=0.8, stop=97.0,
                        take_profit=112.0, leverage=10, margin_mode="isolated",
                        rationale="r", invalidation="i")
    eng._features_cache["SOL"] = {"range24h_pos": 0.5, "atr15m_pct": 0.4,
                                  "hi_24h": 110.0, "lo_24h": 95.0}
    eng._account_snapshot = {"available_margin": 1000.0, "equity": 1000.0}
    _, preview = eng._preview_action(action, {"SOL": 100.0}, 1000.0,
                                     utc_day(time.time()), frozenset())
    step = 10 ** -eng.market.info("SOL").sz_decimals
    assert abs(preview["size"] / step - round(preview["size"] / step)) < 1e-9
    assert preview["notional"] == pytest.approx(preview["size"] * 100.0)
    # the realized risk can only ever be at or below what was approved
    assert abs(100.0 - 97.0) * preview["size"] <= preview["risk_usd"] * 1.05
