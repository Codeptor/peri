import json

import pytest

from peri.analyst import Analyst, AnalystError, SearchTools, build_prompt
from peri.config import AnalystCfg
from peri.models import AdjustStopAction, CloseAction, OpenAction

CFG = AnalystCfg(cycle_secs=900, timeout_secs=60, retries=2, temperature=0.2,
                 max_output_tokens=4000, conviction_min=0.75)

BUNDLE = {
    "trigger": "scheduled",
    "account": {"equity": 1000.0, "mode": "dry", "day_pnl": 12.5, "entries_today": 1,
                "daily_entry_cap": 6, "kill": False, "peri_open_positions": 1,
                "external_positions": 4, "max_concurrent": 3,
                "available_margin": 410.0, "total_margin_used": 590.0,
                "withdrawable_by_dex": {"native": 410.0, "xyz": 410.0}},
    "positions": [{"market": "xyz:NVDA", "side": "long", "size": 0.596,
                   "entry_px": 218.15, "mark": 219.0, "upnl": 0.5, "stop_px": 214.9,
                   "tp_px": 226.0, "notional": 130.0, "leverage": 20.0,
                   "margin_mode": "cross",
                   "margin": 43.33, "position_value": 130.52,
                   "liquidation_px": 171.5, "roe": 0.0115,
                   "opened_ts": 0.0, "source": "own",
                   "rationale": "beat and raise", "invalidation": "loses 215 shelf"}],
    "candidates": [{"name": "BTC", "max_leverage": 20,
                    "features": {"mark": 80000, "day_pct": -0.3,
                    "funding_apr_pct": 11.0, "oi_usd": 3e9, "vol_usd": 3e9,
                    "r_1h_pct": 0.1, "r_4h_pct": -0.5, "atr15m_pct": 0.2,
                    "range24h_pos": 0.4}}],
    "telegram": [{"msg_id": 9, "ts": 0.0, "sender": "h3rkk", "text": "SOL long ...",
                  "is_caller": 1}],
    "news": [{"source": "WSJ", "title": "Nvidia beats", "age": "2h"}],
    "closes": [{"market": "SOL", "side": "long", "entry_px": 100, "close_px": 102,
                "realized_pnl": 3.1, "close_reason": "tp", "conviction": 0.8}],
}


def mk(transport):
    return Analyst(CFG, "key", "https://api.example.com", "test-model",
                   0.75, 2.0, 20.0, transport=transport)


def test_prompt_contains_everything():
    p = build_prompt(BUNDLE, now=1000.0)
    for needle in ("equity=$1000.00", "xyz:NVDA long", "loses 215 shelf",
                   "CANDIDATES", "BTC | 80000", "[CALLER] id=9", "Nvidia beats",
                   "SOL long entry=100", "TRIGGER: scheduled",
                   "peri_positions=1/3", "external_positions=4",
                   "available_margin=$410.00", "liq=171.5", "roe=+1.15%",
                   "mode=cross", "maxLev=20x"):
        assert needle in p, f"missing: {needle}"


def test_prompt_marks_missing_technicals_unknown_instead_of_zero():
    b = dict(BUNDLE)
    b["candidates"] = [{"name": "BTC", "max_leverage": 20, "features": {
        "mark": 80000, "day_pct": -0.3, "funding_apr_pct": 11.0,
        "oi_usd": 3e9, "vol_usd": 3e9,
    }}]

    p = build_prompt(b, now=1000.0)

    row = next(line for line in p.splitlines() if line.startswith("- BTC |"))
    assert row.endswith("| ? | ? | ? | ?")


def test_decide_parses_valid_json():
    payload_seen = {}

    def transport(payload):
        payload_seen.update(payload)
        return json.dumps({"market_view": "quiet", "actions": []})

    res = mk(transport).decide(BUNDLE)
    assert res.decision.market_view == "quiet"
    assert payload_seen["model"] == "test-model"
    assert "response_format" not in payload_seen  # DashScope: JSON mode kills reasoning
    assert payload_seen["messages"][0]["role"] == "system"
    system = payload_seen["messages"][0]["content"]
    assert "opened outside Peri" in system
    assert "Nobody else is assumed to manage" in system
    assert "stop=None or tp=None" in system
    assert '"kind": "adjust_stop"' in system
    assert '"take_profit": 108.0' in system
    assert "never above the candidate's maxLev" in system
    assert "Leverage decides only how much margin" in system
    assert "cross" in system and "isolated" in system


def test_decide_strips_code_fences():
    def transport(payload):
        return "```json\n" + json.dumps({"actions": []}) + "\n```"

    assert mk(transport).decide(BUNDLE).decision.actions == []


def test_retries_then_raises():
    calls = {"n": 0}

    def transport(payload):
        calls["n"] += 1
        return "not json at all"

    with pytest.raises(AnalystError):
        mk(transport).decide(BUNDLE)
    assert calls["n"] == 3  # 1 + 2 retries


def test_recovers_on_second_attempt():
    calls = {"n": 0}

    def transport(payload):
        calls["n"] += 1
        if calls["n"] == 1:
            return "garbage"
        return json.dumps({"actions": [{"kind": "close", "market": "BTC",
                                        "rationale": "done"}]})

    res = mk(transport).decide(BUNDLE)
    assert res.decision.actions[0].market == "BTC"


def test_invalid_action_schema_is_retried_then_fatal():
    def transport(payload):
        return json.dumps({"actions": [{"kind": "open", "market": "BTC"}]})  # missing fields

    with pytest.raises(AnalystError):
        mk(transport).decide(BUNDLE)


def test_unconfigured_analyst_refuses_to_build():
    with pytest.raises(AnalystError):
        Analyst(CFG, "", "", "", 0.75, 2.0, 10.0)


def test_base_url_normalization():
    a = mk(lambda p: json.dumps({"actions": []}))
    assert a.url == "https://api.example.com/v1/chat/completions"
    b = Analyst(CFG, "k", "https://api.example.com/v1", "m", 0.75, 2.0, 10.0,
                transport=lambda p: "")
    assert b.url == "https://api.example.com/v1/chat/completions"


# -- tool loop ---------------------------------------------------------------

class FakeTools:
    def __init__(self):
        self.calls = []

    def available(self):
        return True

    def run(self, name, args):
        self.calls.append((name, args))
        return f"RESULT[{name}]: NVDA beat earnings, guides above consensus"


def test_tool_loop_search_then_decide():
    tools = FakeTools()
    payloads = []

    def transport(payload):
        payloads.append(payload)
        if len(payloads) == 1:
            return {"content": "", "reasoning_content": "need to verify catalyst",
                    "tool_calls": [{"id": "c1", "function": {
                        "name": "web_search",
                        "arguments": json.dumps({"query": "NVDA earnings"})}}]}
        return {"content": json.dumps({"market_view": "verified", "actions": []}),
                "reasoning_content": "catalyst confirmed"}

    a = Analyst(CFG, "key", "https://api.example.com", "m", 0.75, 2.0, 10.0,
                transport=transport, tools=tools)
    res = a.decide(BUNDLE)
    assert res.decision.market_view == "verified"
    assert tools.calls == [("web_search", {"query": "NVDA earnings"})]
    assert len(res.tool_log) == 1 and "beat earnings" in res.tool_log[0]["result"]
    assert "need to verify" in res.reasoning and "confirmed" in res.reasoning
    # 1st payload offers tools, final payload demands JSON
    assert "tools" in payloads[0] and "response_format" not in payloads[0]
    # tool result was fed back
    roles = [m["role"] for m in payloads[1]["messages"]]
    assert roles == ["system", "user", "assistant", "tool"]


def test_tool_round_cap_forces_decision():
    tools = FakeTools()
    payloads = []

    def transport(payload):
        payloads.append(payload)
        if "tools" in payload:  # keeps trying to search while allowed
            return {"tool_calls": [{"id": "x", "function": {
                "name": "web_search", "arguments": "{}"}}], "content": ""}
        return {"content": json.dumps({"actions": []})}

    a = Analyst(CFG, "key", "https://api.example.com", "m", 0.75, 2.0, 10.0,
                transport=transport, tools=tools)
    res = a.decide(BUNDLE)
    assert res.decision.actions == []
    # CFG.max_tool_rounds tool-offering payloads, then one forced-final
    assert sum(1 for p in payloads if "tools" in p) == CFG.max_tool_rounds
    assert "tools" not in payloads[-1]  # final round demands the decision


def test_market_bias_is_offered_even_with_no_search_keys():
    """market_bias needs no API key, so the tool surface is never empty. A
    search tool that IS unconfigured says so to the model rather than vanishing."""
    payloads = []

    def transport(payload):
        payloads.append(payload)
        return json.dumps({"actions": []})

    analyst = mk(transport)              # default SearchTools = no keys at all
    res = analyst.decide(BUNDLE)
    offered = [t["function"]["name"] for t in payloads[0].get("tools", [])]
    assert "market_bias" in offered
    assert res.decision.actions == []
    assert "not configured" in analyst.tools.run("web_search", {"query": "x"})


def test_prompt_and_reasoning_captured():
    def transport(payload):
        return {"content": json.dumps({"actions": []}), "reasoning_content": "thought"}

    res = mk(transport).decide(BUNDLE)
    assert res.reasoning == "thought"
    assert "CANDIDATES" in res.prompt


def test_prompt_operator_notes_and_orders():
    b = dict(BUNDLE)
    b["notes"] = [{"ts": 0.0, "text": "manage the KIOXIA short; carry pays you"}]
    b["orders"] = [{"coin": "SOL", "orderType": "Limit", "side": "A", "sz": "1.12",
                    "limitPx": "104.75", "triggerCondition": None}]
    p = build_prompt(b, now=1000.0)
    assert "OPERATOR NOTES" in p and "carry pays you" in p
    assert "OPEN ORDERS" in p and "SOL Limit sell sz=1.12" in p
    # absent sections stay absent
    p2 = build_prompt(BUNDLE, now=1000.0)
    assert "OPERATOR NOTES" not in p2 and "OPEN ORDERS" not in p2


# -- live-context conversation ----------------------------------------------

def test_chat_uses_full_fresh_context_history_decisions_and_current_request():
    payloads = []

    def transport(payload):
        payloads.append(payload)
        return json.dumps({
            "answer": "NVDA is still protected by its live brackets.",
            "proposal": None,
        })

    bundle = {
        **BUNDLE,
        "context_ts": 1_777_777_777.0,
        "orders": [{
            "coin": "xyz:NVDA", "orderType": "Stop Market", "side": "A",
            "sz": "0.596", "limitPx": "214.8", "triggerPx": "214.9",
            "reduceOnly": True, "oid": 77,
        }],
        "decisions": [{
            "id": 9, "ts": 1_777_777_700.0, "trigger": "scheduled",
            "market_view": "Semis remain bid", "actions_json": "[]", "status": "ok",
        }],
    }
    result = mk(transport).chat(
        bundle,
        [{"role": "user", "content": "Earlier we discussed NVDA."}],
        "Can you assess the currently opened NVDA position?",
    )

    assert result.response.answer.startswith("NVDA")
    assert result.response.proposal is None
    messages = payloads[0]["messages"]
    rendered = "\n".join(str(m.get("content", "")) for m in messages)
    for needle in (
        "context_ts=1777777777", "equity=$1000.00", "available_margin=$410.00",
        "xyz:NVDA long", "mode=cross", "liq=171.5", "oid=77",
        "Semis remain bid", "Earlier we discussed NVDA",
        "Can you assess the currently opened NVDA position?",
    ):
        assert needle in rendered
    system = messages[0]["content"]
    assert "does not execute" in system
    assert "full position" in system
    assert "BOTH" in system


@pytest.mark.parametrize(
    ("proposal", "expected_type"),
    [
        ({
            "kind": "open", "market": "BTC", "side": "long", "conviction": 0.8,
            "stop": 78000, "take_profit": 84000, "leverage": 20,
            "margin_mode": "cross", "source": "own", "mirror_msg_id": None,
            "rationale": "breakout", "invalidation": "loses 78k",
        }, OpenAction),
        ({"kind": "close", "market": "xyz:NVDA", "rationale": "thesis failed"},
         CloseAction),
        ({
            "kind": "adjust_stop", "market": "xyz:NVDA", "stop": 217,
            "take_profit": 228, "rationale": "lock profit",
        }, AdjustStopAction),
    ],
)
def test_chat_accepts_one_complete_discriminated_proposal(proposal, expected_type):
    def transport(payload):
        return json.dumps({"answer": "Prepared for confirmation.", "proposal": proposal})

    response = mk(transport).chat(BUNDLE, [], "prepare it").response
    assert isinstance(response.proposal, expected_type)


@pytest.mark.parametrize(
    "proposal",
    [
        {
            # 5x is legitimate now (builder dexes cap at 3-6x); 99x is not
            "kind": "open", "market": "BTC", "side": "long", "conviction": 0.8,
            "stop": 78000, "take_profit": 84000, "leverage": 99,
            "margin_mode": "isolated", "rationale": "x", "invalidation": "y",
        },
        {
            "kind": "close", "market": "xyz:NVDA", "size": 0.1,
            "rationale": "partial close",
        },
        {
            "kind": "adjust_stop", "market": "xyz:NVDA", "stop": 217,
            "rationale": "missing TP",
        },
    ],
)
def test_chat_rejects_incomplete_or_unsupported_proposal(proposal):
    def transport(payload):
        return json.dumps({"answer": "bad", "proposal": proposal})

    with pytest.raises(AnalystError):
        mk(transport).chat(BUNDLE, [], "prepare it")


def test_chat_bounds_history_to_40_messages_and_20000_characters():
    payloads = []

    def transport(payload):
        payloads.append(payload)
        return json.dumps({"answer": "fresh answer", "proposal": None})

    history = [
        {"role": "user" if i % 2 == 0 else "assistant", "content": f"H{i:02d}:" + "x" * 995}
        for i in range(50)
    ]
    mk(transport).chat(BUNDLE, history, "current")
    sent = payloads[0]["messages"]
    prior = [m for m in sent if str(m.get("content", "")).startswith("H")]
    assert len(prior) <= 40
    assert sum(len(m["content"]) for m in prior) <= 20_000
    assert any(m["content"].startswith("H49:") for m in prior)
    assert not any(m["content"].startswith("H00:") for m in prior)
    assert sent[-1] == {"role": "user", "content": "current"}


def test_chat_malformed_output_retries_then_fails_without_proposal():
    calls = 0

    def transport(payload):
        nonlocal calls
        calls += 1
        return "not json"

    with pytest.raises(AnalystError):
        mk(transport).chat(BUNDLE, [], "question")
    assert calls == CFG.retries + 1


def test_chat_default_transport_streams_decoded_answer_not_json(monkeypatch):
    analyst = Analyst(
        CFG, "key", "https://api.example.com", "test-model",
        0.75, 2.0, 20.0, tools=SearchTools(),
    )
    raw = json.dumps({
        "answer": "NVDA is live.\nBoth brackets are protected.",
        "proposal": None,
    })
    chunks = [raw[:5], raw[5:17], raw[17:28], raw[28:43], raw[43:]]
    payloads = []

    def stream(payload):
        payloads.append(payload)
        return iter({"content": chunk} for chunk in chunks)

    monkeypatch.setattr(analyst, "_http_stream", stream)
    events = []
    result = analyst.chat(
        BUNDLE, [], "status", on_event=lambda kind, data: events.append((kind, data))
    )

    assert result.response.answer == "NVDA is live.\nBoth brackets are protected."
    assert "".join(
        data["delta"] for kind, data in events if kind == "delta"
    ) == result.response.answer
    assert not any("answer" in data.get("delta", "") for kind, data in events
                   if kind == "delta")
    assert payloads[0]["messages"][-1] == {"role": "user", "content": "status"}


def test_chat_stream_accumulates_tool_call_fragments_and_reports_progress(monkeypatch):
    tools = FakeTools()
    analyst = Analyst(
        CFG, "key", "https://api.example.com", "test-model",
        0.75, 2.0, 20.0, tools=tools,
    )
    response = json.dumps({"answer": "Catalyst verified.", "proposal": None})
    rounds = iter([
        [
            {"tool_calls": [{"index": 0, "id": "call-1", "type": "function",
                             "function": {"name": "web_", "arguments": "{\"query\":"}}]},
            {"tool_calls": [{"index": 0,
                             "function": {"name": "search", "arguments": "\"NVDA\"}"}}]},
        ],
        [{"content": response[:20]}, {"content": response[20:]}],
    ])
    monkeypatch.setattr(analyst, "_http_stream", lambda payload: iter(next(rounds)))
    events = []

    result = analyst.chat(
        BUNDLE, [], "why is NVDA moving?",
        on_event=lambda kind, data: events.append((kind, data)),
    )

    assert result.response.answer == "Catalyst verified."
    assert tools.calls == [("web_search", {"query": "NVDA"})]
    tool_events = [data for kind, data in events if kind == "tool"]
    assert [event["phase"] for event in tool_events] == ["start", "result"]
    assert tool_events[0]["tool"] == "web_search"
    assert "beat earnings" in tool_events[1]["result"]


def test_prompt_refusal_feedback_section():
    b = dict(BUNDLE)
    b["refusals"] = [{"ts": 400.0, "market": "xyz:NVDA",
                      "reason": "xyz:NVDA in cooldown for 6676s"}]
    p = build_prompt(b, now=1000.0)
    assert "RECENTLY REFUSED BY THE RISK ENGINE" in p
    assert "xyz:NVDA: xyz:NVDA in cooldown" in p
    p2 = build_prompt(BUNDLE, now=1000.0)
    assert "RECENTLY REFUSED" not in p2  # absent when no refusals


def test_caller_wake_skips_search_tools():
    tools = FakeTools()
    payloads = []

    def transport(payload):
        payloads.append(payload)
        return {"content": json.dumps({"market_view": "fast", "actions": []})}

    a = Analyst(CFG, "key", "https://api.example.com", "m", 0.75, 2.0, 10.0,
                transport=transport, tools=tools)
    b = dict(BUNDLE)
    b["trigger"] = "caller message"
    res = a.decide(b)
    assert res.decision.market_view == "fast"
    assert len(payloads) == 1 and "tools" not in payloads[0]   # single pass, no tools
    assert tools.calls == []
    # scheduled cycles still get the tools
    payloads.clear()
    a.decide(dict(BUNDLE, trigger="scheduled"))
    assert "tools" in payloads[0]


def test_tool_calls_in_one_round_run_concurrently_in_order():
    import threading
    import time as _t

    class SlowTools(FakeTools):
        def run(self, name, args):
            _t.sleep(0.25)  # two sequential calls would take >=0.5s
            return f"{name}:{args['query']}:{threading.current_thread().name}"

    tools = SlowTools()

    seen = {"rounds": 0}

    def transport(payload):
        if "tools" in payload and seen["rounds"] == 0:   # one search round only
            seen["rounds"] += 1
            return {"content": "", "tool_calls": [
                {"id": "a", "function": {"name": "web_search",
                                         "arguments": json.dumps({"query": "q1"})}},
                {"id": "b", "function": {"name": "deep_search",
                                         "arguments": json.dumps({"query": "q2"})}}]}
        return {"content": json.dumps({"actions": []})}

    a = Analyst(CFG, "key", "https://api.example.com", "m", 0.75, 2.0, 10.0,
                transport=transport, tools=tools)
    t0 = _t.monotonic()
    res = a.decide(dict(BUNDLE, trigger="scheduled"))
    elapsed = _t.monotonic() - t0
    assert elapsed < 0.45, f"tool calls ran sequentially ({elapsed:.2f}s)"
    assert [t["tool"] for t in res.tool_log] == ["web_search", "deep_search"]  # order kept
    assert res.tool_log[0]["result"].startswith("web_search:q1")


def test_the_prompt_tells_the_analyst_to_price_funding():
    """fundingAPR% has always been in the candidate table with no instruction
    for using it; at -100% APR it is worth a quarter of a trade's risk budget
    over a two-day hold."""
    from peri.analyst import Analyst
    from peri.config import AnalystCfg, RiskCfg
    cfg = AnalystCfg(900, 60, 2, 0.2, 4000, 0.75)
    rails = RiskCfg(5.0, 20.0, 1, 3, 15.0, 2.0, 900, 3600, 14400, 10.0, 5.0, 1000.0)
    a = Analyst(cfg, "k", "https://x/v1", "m", 0.75, 2.0, 20.0, rails=rails)
    assert "FUNDING IS PART OF THE TRADE" in a.system
    assert "positioning signal" in a.system
    assert "0.15%" in a.system          # the corrected fee, not the old 0.21%


def test_market_bias_is_a_tool_the_analyst_can_call_for_any_market():
    """The bundle pre-fetches bias for what we hold plus the big movers; the
    tool lets the analyst ask about anything else."""
    from peri.analyst import TOOLS, SearchTools
    names = [t["function"]["name"] for t in TOOLS]
    assert "market_bias" in names
    spec = next(t for t in TOOLS if t["function"]["name"] == "market_bias")
    assert spec["function"]["parameters"]["required"] == ["market"]

    tools = SearchTools()                       # no API keys at all
    assert tools.available() is True            # market_bias needs none

    calls = {}

    def fake(coin, request=None):
        calls["coin"] = coin
        return {"coin": coin, "long_traders": 10, "short_traders": 3,
                "smart_long_pct": 74.0, "crowd_long_pct": 32.0, "divergence": 42.0,
                "cohorts": [{"label": "Rekt", "long_pct": 11.0,
                             "sentiment": "Extremely Bearish"}]}

    import peri.trench as trench
    original = trench.fetch_asset_bias
    trench.fetch_asset_bias = fake
    try:
        out = tools.run("market_bias", {"market": "io:ANTH"})
    finally:
        trench.fetch_asset_bias = original
    assert calls["coin"] == "io:ANTH"
    assert "Rekt: 11% long" in out and "+42pp" in out


def test_market_bias_reports_a_bad_market_instead_of_inventing_one():
    from peri.analyst import SearchTools
    tools = SearchTools()
    assert "empty market" in tools.run("market_bias", {"market": "  "})

    import peri.trench as trench
    original = trench.fetch_asset_bias
    trench.fetch_asset_bias = lambda coin, request=None: None
    try:
        out = tools.run("market_bias", {"market": "NOSUCH"})
    finally:
        trench.fetch_asset_bias = original
    assert "no cohort positioning for NOSUCH" in out


def test_the_position_cap_in_the_prompt_comes_from_config_not_a_hardcoded_one():
    """analyst.py:169 hardcoded 'at most ONE autonomous position' while the
    CAPACITY line rendered peri_positions=0/3. The analyst obeyed the sentence,
    not the data, and spent 2026-08-31 declining every second setup with 'I am
    at my one-position cap' — including HYPE at +35pp smart-money divergence."""
    from peri.analyst import Analyst
    from peri.config import RiskCfg

    for cap in (1, 3, 5):
        rails = RiskCfg(5.0, 20.0, cap, 3, 15.0, 2.0, 900, 3600, 14400, 10.0, 5.0, 1000.0)
        a = Analyst(CFG, "k", "https://x/v1", "m", 0.75, 2.0, 20.0, rails=rails)
        assert f"at most {cap} autonomous position" in a.system
    assert "at most ONE autonomous position" not in a.system
