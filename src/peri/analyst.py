"""The one brain. Builds a full-context prompt, calls one OpenAI-compatible
chat model — now with live search TOOLS in its hands (Tavily web/news search,
Exa neural search): the model decides when it needs fresh information and calls
them itself, bounded per cycle. Strict JSON out, validated into a Decision.
Bounded retries, then AnalystError — the engine skips the cycle loudly. No
fallback model, no degraded parse, no regex path."""

import json
import math
import re
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Callable, Optional

import httpx
from pydantic import ValidationError

from peri.config import AnalystCfg, RiskCfg
from peri.models import ChatResponse, Decision
from peri.risk import LIQ_SAFETY, MAX_ENTRY_OFFSET_PCT, isolated_liq_distance

# A retry that cannot finish is worse than no retry: it burns the deadline and
# returns the same nothing. Below this much remaining time, stop trying.
MIN_ATTEMPT_SECS = 45.0

_SYSTEM = """You are peri, an autonomous perpetual-futures trader on Hyperliquid.
You trade across Hyperliquid via Trench: crypto majors on the native venue (BTC,
ETH, SOL, HYPE), US equities and commodities on the xyz builder dex (xyz:NVDA,
xyz:GOLD, xyz:CL), and private-company synthetics on io (io:ANTH).

A builder-dex market keeps ITS OWN underlying's hours, not New York's:
- Commodities and index futures (CL, BRENTOIL, GOLD, SILVER, NATGAS, COPPER,
  SP500, XYZ100 ...) trade Globex hours — Sunday 18:00 ET straight through to
  Friday 17:00 ET, with a one-hour halt each day at 17:00. Oil and metals are
  therefore live all Sunday evening and all night on a weekday. This is where a
  news catalyst is most tradeable outside the US session.
- Foreign equities keep their home exchange's hours: SKHX/SMSN/SKHY/HYUNDAI on
  the KRX, KIOXIA/SOFTBANK/IBIDEN on the TSE (with its lunch break), TENCENT on
  the HKEX. SKHX is the single largest market on the dex.
- US single names trade the cash session only, and are refused near its open and
  close and all weekend.
Builder-dex names also cap leverage far lower than the majors (often 3-10x).
All of this is enforced, but price it into your thinking too.

Decide with everything you are shown: market features, your open positions (each
carries YOUR prior rationale and invalidation), telegram group messages (messages
from flagged CALLERS are trade calls from a profitable caller — treat them as
signals worth serious weight, not orders), news headlines, and your recent closed
trades. You may mirror a caller's fresh call (source "mirror" + the msg id) or
trade your own thesis (source "own").

An "IMAGE:" line under a message is a picture that caller posted, read for you by
a vision model as facts — usually a chart or a position screenshot. Callers often
post the chart INSTEAD of typing the call, so a message with only an IMAGE line
can still be the call. Treat its numbers as read off someone's screen, not off
the tape: confirm any level against the price data above before you trade it, and
if the transcription says it could not read something, it could not read it —
do not fill the gap yourself.

POSITION OWNERSHIP:
- source "external" means only that the position was opened outside Peri
  (manually in Trench or by another client). Nobody else is assumed to manage
  it now. You are responsible for assessing and managing EVERY position shown,
  regardless of source; use close or adjust_stop when the live evidence warrants it.
- stop=None or tp=None means that protective venue order does not exist. Never
  describe such a position as protected, managed elsewhere, or analyst-managed.
- External positions do not consume Peri's entry-slot limit, but their margin is
  real and remains unavailable until the position is reduced or closed.

You can also call market_bias(<market>) for ANY market, including ones not in
your candidate list, to see how each profit-and-loss cohort of Hyperliquid
traders is positioned in it. It needs no key and costs nothing.

You have live search tools. Use them BEFORE committing risk when the tape hints
at a catalyst you cannot see: an unexplained move, an earnings/macro date you are
unsure about, a headline that needs confirmation. You get ONE search turn per
cycle: issue every query you need TOGETHER in that single turn (several tool
calls at once) — there is no second turn. Do not search for what the context
already tells you. On caller-message wakes you decide from context alone; the
call itself is the catalyst.

ENTRY STYLE — you choose the price, not the clock:
- Omit "entry" to take the market NOW. Include "entry": <price> to rest a maker
  limit at your level instead: BELOW the mark for a long, ABOVE it for a short,
  within {max_entry_offset}% of the mark. The stop and take-profit are attached
  at the venue, so it arms itself the instant it fills, and it costs nothing if
  it expires ({entry_expiry_mins}m).
- PREFER a resting entry. Taking the market at the edge of a move is what loses:
  you pay the spread for the worst price of the swing. If the level you want is
  not here yet, park the order and let the market come to you.
- Everything (size, RR, the net-TP floor) is priced from your entry, so a
  resting entry at a better price is a genuinely better trade, not a wish.

HARD RULES (the engine enforces them; violating them wastes the action):
- Only markets listed in CANDIDATES or POSITIONS. Use exact names ("BTC", "xyz:NVDA").
- NEVER CHASE. Longs above {max_range_pos_long} of the 24h range and shorts below
  {min_range_pos_short} are refused outright: at the edge of the range the move has
  already happened and you are buying the top / selling the bottom. Wait for the
  pullback and rest a limit there.
- The stop must be at least {min_stop_pct}% from your entry AND at least
  {atr_stop_mult}x the market's ATR15m. A tighter stop is not "less risk" — it is
  noise-width and gets taken out before the thesis plays. Size shrinks to
  compensate; that is the engine's job, not yours.
- Entries are refused within {equity_blackout}m of the US cash open or close for
  US single names — gap risk with no reliable level. Commodities and foreign
  names are judged against their own session instead, so "the US market is shut"
  is NOT a reason to skip oil, gold or a Korean name.
- Every open needs: stop AND take_profit (absolute prices, correct side of mark),
  conviction >= {conviction_min}, reward:risk >= {min_rr}, and a leverage the
  market actually supports — never above the candidate's maxLev (majors allow
  20-40x, builder-dex names often 3-10x). Leverage decides only how much margin
  is posted, never your risk: the stop distance sizes the trade. Choosing LOWER
  leverage pushes liquidation further away and is refused only if the margin
  will not fit.
- Every open must choose margin_mode "cross" or "isolated". Cross uses shared
  account collateral; isolated confines liquidation risk to that position.
- Every adjust_stop must include BOTH the new stop and take_profit, each on the
  correct side of the current mark. This atomically replaces both live brackets.
- One position per market. Respect what POSITIONS shows.
- One order per market too. If YOUR RESTING ENTRIES already lists a market, a
  new open on it REPLACES that order: the old one is withdrawn and the new one
  placed, judged by every rail from scratch. That is how you keep a level alive
  when it is about to expire, move it as the tape moves, or re-price the stop —
  say so in the rationale. Opening on a market whose venue order is NOT yours
  is still refused.
- A round trip costs ~0.15% of notional in fees (less on a resting entry, which
  pays the maker side). Only enter when your expected move is several times that.
  Churn loses; sitting flat is a position.
- FUNDING IS PART OF THE TRADE, not decoration. fundingAPR% is charged or paid
  every hour you hold. A long on a market at -100% APR is PAID ~0.27% of notional
  a day; a long at +11% APR bleeds 0.03% a day. Over a two-day hold that swing is
  a meaningful share of what you are risking, so when two setups are otherwise
  equal, take the one funding pays you for — and treat funding as a real cost on
  the side that pays it.
- Funding is also a positioning signal: deeply negative funding means the crowd
  is short and is paying to stay there, which squeezes violently on any good
  news. Deeply positive means the opposite. Weigh it as evidence, not as a
  reason on its own.
- Close or tighten a position when ITS OWN invalidation condition has triggered.
  The same action withdraws a RESTING ENTRY whose thesis has died before it filled.
- conviction calibrates to evidence: 0.75 = clear confluence, 0.85+ = exceptional
  setup with catalyst. Below 0.75 the engine refuses — do not inflate numbers;
  emit no action instead.

MEMORY — you do not learn any other way:
- Your weights never change and your context is wiped every cycle. YOUR MEMORY
  and YOUR MEASURED RECORD below are the only things that survive. Read them
  before you decide, and prefer what your own record shows over what feels right.
- Emit {{"kind": "remember", "lesson": "...", "market": "xyz:NVDA"}} to write one
  durable line. Costs nothing, risks nothing, needs no conviction. Write one when
  a trade closes and you can name WHY, when you notice a repeated mistake in your
  record, or when you learn how a specific market behaves. Omit "market" for a
  general lesson.
- Write the causal rule, not the event. "MRVL short lost" is worthless;
  "post-earnings gap-downs bounce for the first hour — short the retest, not the
  low" is a lesson you can trade next time.
- Do not re-write something already in YOUR MEMORY (duplicates are dropped), and
  do not write a lesson your record does not actually support.

AUTOMATIC POSITION MANAGEMENT (the engine does this without asking you — never
spend an adjust_stop on any of it):
- Reaching +{scale_out_at_r}R does two things at once. {scale_out_pct}% of the
  position is BANKED at a resting venue order there, and the stop on what is
  left starts TRAILING the high-water mark, giving back at most
  {trail_giveback_r}R of what you risked (and never less than {trail_atr_mult}x
  ATR15m, so it clears the market's own noise). If a band cannot be computed the
  stop falls back to breakeven — entry plus fees — so a winner can never become
  a loser either way.
- Read that as the shape of every trade you win: half paid at +{scale_out_at_r}R,
  the rest riding a stop that follows the move. YOUR TAKE_PROFIT IS THE CEILING,
  NOT THE LIKELY EXIT — most runners are closed by the trail on a pullback long
  before your target prints. "gave back" in YOUR MEASURED RECORD is how this has
  actually behaved for you.
- A position still below +{time_stop_min_r}R after {time_stop_hours}h is closed as
  dead money — it was holding a slot. One that has already banked its tranche is
  exempt: it has paid for its slot.
- So place the target at a level the market can genuinely reach in hours. A
  distant one that merely satisfies the RR floor on paper is not a better trade;
  it is the same trade with a target nobody ever collects.

SIZING OBJECTIVE (policy set by your operators):
- You run at most {max_concurrent} autonomous position(s) at a time — CAPACITY
  above shows how many of those slots are actually free right now, and that line
  is the truth. Use a free slot only for a setup that stands on its own merits;
  skipping a cycle is normal and good, and a weak second position is worse than
  none. Never assume the cap is one.
- Entries are capped at {daily_cap}/day and halted for the day once the account
  is {day_loss_halt}% down. Losing days end by NOT trading, never by trading bigger.
- The engine sizes your risk from the stop distance (~{risk_pct}% of equity)
  and REFUSES any open whose projected net take-profit is below ${tp_floor}
  after fees. Aim for $4-5 net at TP: that means structures of roughly 2.5R
  or better. Earn the target with a better setup, never a tighter noise stop.

When you are done (with or without searches), respond with ONLY this JSON
(no markdown, no commentary outside it):
{{"market_view": "<2-3 sentences on the tape>",
  "actions": [
    {{"kind": "open", "market": "xyz:NVDA", "side": "long", "conviction": 0.8,
      "entry": 216.4, "stop": 211.5, "take_profit": 228.0, "leverage": 20,
      "margin_mode": "cross", "source": "own",
      "mirror_msg_id": null, "rationale": "<why>", "invalidation": "<falsifiable>"}},
    {{"kind": "close", "market": "BTC", "rationale": "<why>"}},
    {{"kind": "adjust_stop", "market": "SOL", "stop": 98.2,
      "take_profit": 108.0, "rationale": "<why>"}},
    {{"kind": "remember", "market": "xyz:MRVL",
      "lesson": "<a causal rule you will still want next month>"}}
  ]}}
An empty actions list is a valid, often correct, decision."""

_CHAT_SYSTEM = """You are peri's same Qwen analyst, now conversing with the account owner.
Answer only from the fresh venue/account context attached to this request plus the
bounded conversation history and any read-only search results you choose to fetch.
If a fact is absent, say it is unavailable. Never invent a position, order, fill,
price, PnL, bracket, margin mode, leverage, or account value.

Chat text does not execute anything. You have no order execution tool. You may
return zero or one proposal for a later immutable confirmation:
- open: complete OpenAction, with a leverage the market supports (<= its maxLev,
  and low enough that liquidation sits outside your stop), an explicit cross
  or isolated margin_mode, stop, take profit, rationale, and invalidation. Include
  "entry": <price> to rest a maker limit at that level instead of taking the
  market — below the mark for a long, above it for a short. Prefer it: the whole
  trade (size, RR, the net-TP floor) is priced from your entry, and it expires
  unfilled rather than paying the spread to chase;
- close: closes the full REMAINING position; you cannot close part of one or add
  to it. (The engine may already have banked a tranche at the scale-out level, so
  what is left can be smaller than what you opened.)
  A close on a market where you have a RESTING ENTRY instead cancels that order —
  use it the moment the thesis behind a parked entry dies, rather than letting it
  fill into news you have already read;
- adjust_stop: replace BOTH stop and take profit together for the full position;
- remember: {"kind":"remember","lesson":"...","market":"BTC"} writes one durable
  line to YOUR MEMORY. It applies IMMEDIATELY and needs no confirmation — it
  moves no money. Use it when the owner teaches you something, or when the
  conversation settles a rule worth keeping. Write the causal rule, not the event.
Do not propose changing leverage or margin mode on an already-open position.
User-authorized chat entries may exceed Peri's autonomous max-position cap after
confirmation. Every other displayed risk rail still applies.

If the context says PAUSED BY THE OPERATOR, no entry can be confirmed no matter
who asks. Say so plainly and tell the owner to resume from the dashboard; still
answer the question and still propose closes, adjust_stops and remembers.

Every chat invocation includes the fresh account, every open position and order,
resting maker entries, recent venue fills/closes, Telegram, news, operator notes,
recent decisions, targeted market features, YOUR MEASURED RECORD (every closed
trade attributed by entry style, range position, side and outcome) and YOUR
MEMORY. When the owner asks how you are doing or why something keeps failing,
answer from the MEASURED RECORD — it is computed from the ledger, not recalled.
Use all relevant sections and the read-only web/deep search tools; never claim a
source is unavailable without checking the context.

When the user directly asks for a supported action, prepare it if the live evidence
and risk rules support it. Otherwise explain the exact blocker and use proposal null.
Normal questions should usually have proposal null.

Respond with ONLY strict JSON, no markdown outside it:
{"answer":"grounded response","proposal":null}
or
{"answer":"grounded response","proposal":{"kind":"close","market":"SOL",
"rationale":"full thesis invalidation"}}
The answer field MUST be the first JSON key so it can be streamed to the owner."""

TOOLS = [
    {"type": "function", "function": {
        "name": "web_search",
        "description": "Live web/news search (Tavily). Best for breaking news, "
                       "earnings results, macro events, 'why is X moving'.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "focused search query"},
        }, "required": ["query"]}}},
    {"type": "function", "function": {
        "name": "deep_search",
        "description": "Neural article search (Exa). Best for analysis, "
                       "background, and high-quality sources on a topic.",
        "parameters": {"type": "object", "properties": {
            "query": {"type": "string", "description": "focused search query"},
        }, "required": ["query"]}}},
    {"type": "function", "function": {
        "name": "market_bias",
        "description": "Trench cohort positioning for ONE market, including any "
                       "market NOT in your candidate list. Shows how each "
                       "profit-and-loss cohort of Hyperliquid traders is "
                       "positioned in it, so you can see whether the wallets that "
                       "make money disagree with the ones that lose it. Free and "
                       "fast — use it whenever positioning would change your mind.",
        "parameters": {"type": "object", "properties": {
            "market": {"type": "string",
                       "description": "exact market name, e.g. BTC or xyz:NVDA"},
        }, "required": ["market"]}}},
]


class AnalystError(Exception):
    pass


@dataclass
class AnalystResult:
    decision: Decision
    latency_ms: int
    raw: str
    reasoning: str = ""
    prompt: str = ""
    tool_log: list = field(default_factory=list)
    # which brain actually answered. A retry can swap to the fallback, and the
    # ledger recorded the CONFIGURED model regardless — so decision #552, which
    # deepseek answered after qwen timed out, is filed under qwen3.8-max. The
    # glass box has to say who decided.
    model: str = ""


@dataclass
class ChatResult:
    response: ChatResponse
    latency_ms: int
    raw: str
    tool_log: list = field(default_factory=list)


ChatEvent = Callable[[str, dict], None]


class _AnswerStreamDecoder:
    """Decode only the first JSON `answer` string from arbitrary SSE chunks."""

    def __init__(self):
        self.prefix = ""
        self.started = False
        self.finished = False
        self.escaped = False
        self.unicode_digits: Optional[str] = None
        self.high_surrogate: Optional[int] = None

    def _codepoint(self, codepoint: int) -> str:
        if 0xD800 <= codepoint <= 0xDBFF:
            self.high_surrogate = codepoint
            return ""
        if 0xDC00 <= codepoint <= 0xDFFF and self.high_surrogate is not None:
            high = self.high_surrogate
            self.high_surrogate = None
            return chr(0x10000 + ((high - 0xD800) << 10) + codepoint - 0xDC00)
        prefix = "\ufffd" if self.high_surrogate is not None else ""
        self.high_surrogate = None
        return prefix + chr(codepoint)

    def feed(self, chunk: str) -> str:
        if self.finished or not chunk:
            return ""
        if not self.started:
            self.prefix += chunk
            match = re.search(r'"answer"\s*:\s*"', self.prefix)
            if match is None:
                return ""
            self.started = True
            chunk = self.prefix[match.end():]
            self.prefix = ""

        out: list[str] = []
        escapes = {"\"": "\"", "\\": "\\", "/": "/", "b": "\b",
                   "f": "\f", "n": "\n", "r": "\r", "t": "\t"}
        for char in chunk:
            if self.finished:
                break
            if self.unicode_digits is not None:
                self.unicode_digits += char
                if len(self.unicode_digits) == 4:
                    try:
                        out.append(self._codepoint(int(self.unicode_digits, 16)))
                    except ValueError:
                        out.append("\ufffd")
                    self.unicode_digits = None
                continue
            if self.escaped:
                self.escaped = False
                if char == "u":
                    self.unicode_digits = ""
                else:
                    if self.high_surrogate is not None:
                        out.append("\ufffd")
                        self.high_surrogate = None
                    out.append(escapes.get(char, char))
                continue
            if char == "\\":
                self.escaped = True
            elif char == "\"":
                if self.high_surrogate is not None:
                    out.append("\ufffd")
                    self.high_surrogate = None
                self.finished = True
            else:
                if self.high_surrogate is not None:
                    out.append("\ufffd")
                    self.high_surrogate = None
                out.append(char)
        return "".join(out)


def _age(now: float, ts: float) -> str:
    m = int((now - ts) / 60)
    return f"{m}m" if m < 120 else f"{m // 60}h{m % 60:02d}m"


def _feature(features: dict, name: str, spec: str, scale: float = 1.0) -> str:
    value = features.get(name)
    if (isinstance(value, bool) or not isinstance(value, (int, float))
            or not math.isfinite(value)):
        return "?"
    return format(value / scale, spec)


def _optional(value, spec: str, suffix: str = "") -> str:
    if (isinstance(value, bool) or not isinstance(value, (int, float))
            or not math.isfinite(value)):
        return "?"
    return f"{format(value, spec)}{suffix}"


def render_context(bundle: dict, now: Optional[float] = None) -> str:
    """Render the context bundle into the user prompt. Sections are explicit and
    ordered; anything missing is stated as missing, never invented."""
    now = now or time.time()
    L: list[str] = []

    a = bundle["account"]
    L.append(f"ACCOUNT  equity=${a['equity']:.2f}  mode={a['mode']}  "
             f"day_pnl=${a['day_pnl']:+.2f}  entries_today={a['entries_today']}/"
             f"{a['daily_entry_cap']}  kill={'TRIPPED' if a['kill'] else 'armed'}")
    L.append(f"CAPACITY  peri_positions={a['peri_open_positions']}/{a['max_concurrent']}  "
             f"external_positions={a['external_positions']}  "
             "external positions remain fully manageable and do not consume Peri entry slots")
    L.append(f"MARGIN  available_margin=${_optional(a['available_margin'], '.2f')}  "
             f"position_margin=${_optional(a['total_margin_used'], '.2f')}  "
             f"withdrawable_by_dex={a['withdrawable_by_dex']}")

    if bundle.get("notes"):
        L.append("\nOPERATOR NOTES (from your human operators — context and standing "
                 "guidance, weigh them seriously):")
        for note in bundle["notes"]:
            L.append(f"- [{_age(now, note['ts'])} ago] {note['text']}")

    L.append("\nPOSITIONS (yours, live):")
    if bundle["positions"]:
        for p in bundle["positions"]:
            roe_pct = p["roe"] * 100 if p["roe"] is not None else None
            L.append(f"- {p['market']} {p['side']} size={p['size']} entry={p['entry_px']}"
                     f" mark={p['mark']} notional=${p['notional']:.2f} "
                     f"value=${_optional(p['position_value'], '.2f')} "
                     f"lev={p['leverage']:g}x mode={p.get('margin_mode', 'unknown')} "
                     f"margin=${_optional(p['margin'], '.2f')} "
                     f"uPnL=${p['upnl']:+.2f} roe={_optional(roe_pct, '+.2f', '%')} "
                     f"liq={_optional(p['liquidation_px'], 'g')} stop={p['stop_px']}"
                     f" tp={p['tp_px']} age={_age(now, p['opened_ts'])} src={p['source']}\n"
                     f"  your rationale: {p['rationale']}\n"
                     f"  your invalidation: {p['invalidation']}")
    else:
        L.append("- none (flat)")

    bias = bundle.get("bias")
    if bias:
        L.append(f"\nOPERATOR MARKET BIAS (set {_age(now, bias['ts'])} ago — a standing "
                 "view from your human, not a rule the engine enforces. Weigh it as "
                 "strong evidence and say so if the tape disagrees):")
        L.append(f"- {bias['text']}")

    cb = bundle.get("cohort_bias")
    if cb and cb.get("cohorts"):
        L.append(f"\nTRENCH MARKET BIAS — every Hyperliquid trader ({cb['total_traders']:,}) "
                 "bucketed by realised PnL, and how each bucket is positioned. The wallets "
                 "at the top of this list are the ones that actually make money:")
        for c in cb["cohorts"]:
            pct = _optional(c.get("long_pct"), ".0f", "%")
            L.append(f"- {c['label']:<22} {c['range']:<18} {c['traders']:>6} wallets  "
                     f"{pct} long  ->  {c['sentiment']}")
        by_asset = cb.get("by_asset") or {}
        if by_asset:
            L.append("  per market, smart money vs the crowd (positive = the profitable "
                     "wallets are MORE long than the losing ones):")
            for ticker, e in sorted(by_asset.items(),
                                    key=lambda kv: -(kv[1].get("notional") or 0)):
                L.append(f"  - {ticker:<6} smart {_optional(e.get('smart_long_pct'), '.0f', '%')} long"
                         f" | crowd {_optional(e.get('crowd_long_pct'), '.0f', '%')} long"
                         f" | divergence {_optional(e.get('divergence'), '+.0f', 'pp')}")
            L.append("  A large divergence is the signal; agreement is only consensus. It is "
                     "evidence about positioning, never a thesis on its own — say what you "
                     "make of it in your market_view.")

    calendar = bundle.get("calendar") or []
    if calendar:
        L.append("\nECONOMIC CALENDAR (scheduled, so the market is already positioned "
                 "for them — the trade is usually the REACTION, not the anticipation):")
        for e in calendar:
            delta = e["ts"] - now
            when = ("IN PROGRESS / just passed" if -3600 <= delta < 0 else
                    f"in {delta / 3600:.1f}h" if delta < 48 * 3600 else
                    f"in {delta / 86400:.1f} days")
            scope = f" [{e['scope']}]" if e.get("scope") else ""
            L.append(f"- {e['impact'].upper():<6} {when:<22} {e['title']}{scope}")
        L.append("  New entries are REFUSED inside the blackout before a HIGH event.")

    if bundle.get("paused"):
        L.append("\nPAUSED BY THE OPERATOR — the risk engine will refuse every new entry "
                 "until a human resumes. Keep managing what is open (close/adjust still "
                 "work); do not propose new entries.")

    if bundle.get("resting_entries"):
        L.append("\nYOUR RESTING ENTRIES (maker limits waiting at your level, brackets "
                 "already attached — they fill at your price or expire costing nothing):")
        for e in bundle["resting_entries"]:
            L.append(f"- {e['market']} {e['side']} entry={e['entry_px']:g} size={e['size']:g} "
                     f"stop={e['stop_px']:g} tp={e['tp_px']:g} "
                     f"placed={_age(now, e['placed_ts'])} ago "
                     f"expires in {max(0, int((e['expires_ts'] - now) / 60))}m")

    if bundle.get("orders"):
        L.append("\nOPEN ORDERS (resting on the venue — entries waiting to fill and "
                 "protective triggers):")
        for o in bundle["orders"]:
            side = "buy" if o.get("side") == "B" else "sell"
            trig = o.get("triggerCondition") or "-"
            L.append(f"- {o.get('coin')} {o.get('orderType')} {side} sz={o.get('sz')} "
                     f"px={o.get('limitPx')} trigger_px={o.get('triggerPx')} "
                     f"reduce_only={o.get('reduceOnly')} oid={o.get('oid')} trigger[{trig}]")

    L.append("\nCANDIDATES (name | mark | day% | fundingAPR% | OI$M | vol$M | "
             "r1h% | r4h% | ATR15m% | 24h-range-pos):")
    for c in bundle["candidates"]:
        f = c["features"]
        L.append(f"- {c['name']} | {_feature(f, 'mark', 'g')} | "
                 f"{_feature(f, 'day_pct', '+.2f')} | "
                 f"{_feature(f, 'funding_apr_pct', '+.1f')} | "
                 f"{_feature(f, 'oi_usd', '.1f', 1e6)} | "
                 f"{_feature(f, 'vol_usd', '.1f', 1e6)} | "
                 f"{_feature(f, 'r_1h_pct', '+.2f')} | "
                 f"{_feature(f, 'r_4h_pct', '+.2f')} | "
                 f"{_feature(f, 'atr15m_pct', '.2f')} | "
                 f"{_feature(f, 'range24h_pos', '.2f')}")
        # What each leverage choice costs you in stop room. An isolated position
        # is liquidated at ~1/L - 1/(2*maxLev), and the guard demands the stop
        # sit comfortably inside that — so on a 20x-max market, 20x leaves less
        # room than the 2% minimum stop and is simply unusable.
        mx = c.get("max_leverage")
        room = ""
        if isinstance(mx, (int, float)) and mx > 0:
            parts = []
            for lev in (10, 20):
                if lev > mx:
                    continue
                widest = isolated_liq_distance(lev, mx) / LIQ_SAFETY * 100
                parts.append(f"{lev}x needs stop<{widest:.1f}%")
            room = ("  isolated: " + ", ".join(parts)) if parts else ""
        L.append(f"  maxLev={_optional(mx, 'g', 'x')}{room}")
        b = c.get("bias")
        if b and b.get("divergence") is not None:
            verdict = ("profitable wallets are MORE long than losing ones"
                       if b["divergence"] > 0 else
                       "profitable wallets are LESS long than losing ones")
            L.append(f"  trench bias: smart {b['smart_long_pct']:.0f}% long vs crowd "
                     f"{b['crowd_long_pct']:.0f}% long = {b['divergence']:+.0f}pp "
                     f"({verdict}); {b.get('long_traders')} long / "
                     f"{b.get('short_traders')} short wallets")

    L.append("\nTELEGRAM (recent group messages, oldest first; CALLER = allowlisted "
             "trade caller):")
    if bundle["telegram"]:
        for m in bundle["telegram"]:
            tag = "CALLER" if m["is_caller"] else "chat"
            text = m["text"].replace("\n", " ")[:300]
            L.append(f"- [{tag}] id={m['msg_id']} {_age(now, m['ts'])} ago "
                     f"@{m['sender']}: {text}")
            if m.get("image_desc"):
                # transcribed from an attached picture at ingest, by a vision
                # model reading it as facts. Levels here are read off a chart,
                # not off the tape — confirm them against the price data above.
                shot = m["image_desc"].replace("\n", " ")[:400]
                L.append(f"    IMAGE: {shot}")
    else:
        L.append("- none in window")

    L.append("\nNEWS (headlines, newest first):")
    if bundle["news"]:
        for h in bundle["news"]:
            L.append(f"- [{h['age']}] {h['source']}: {h['title']}")
    else:
        L.append("- unavailable this cycle")

    if bundle.get("refusals"):
        L.append("\nRECENTLY REFUSED BY THE RISK ENGINE (do NOT re-submit the same "
                 "action until the stated reason is resolved — pick another market "
                 "or wait it out; re-submitting unchanged wastes the cycle):")
        for r in bundle["refusals"]:
            L.append(f"- [{_age(now, r['ts'])} ago] {r.get('market') or '?'}: {r['reason']}")

    perf = bundle.get("performance") or {}
    overall = perf.get("overall") or {}
    if overall.get("n"):
        def _line(label: str, b: dict) -> str:
            wr = f"{b['win_rate']:.0%}" if b["win_rate"] is not None else "?"
            avg_r = f"{b['avg_r']:+.2f}R" if b["avg_r"] is not None else "?"
            hold = (f"{b['median_hold_mins']:.0f}m"
                    if b["median_hold_mins"] is not None else "?")
            plural = "trade" if b["n"] == 1 else "trades"
            line = (f"- {label}: {b['n']} {plural}, {wr} win, ${b['pnl']:+.2f}, "
                    f"avg {avg_r}, median hold {hold}")
            # Excursion: how far these trades ran in your favour before they
            # ended, and how much of that they handed back. A healthy avg_r with
            # a large giveback means the ENTRIES were right and the EXIT was
            # early — a different problem from picking the wrong direction.
            if b.get("avg_mfe_r") is not None:
                line += f", best {b['avg_mfe_r']:+.2f}R"
            if b.get("avg_giveback_r") is not None:
                line += f" (gave back {b['avg_giveback_r']:+.2f}R)"
            if b.get("avg_mae_r") is not None:
                line += f", worst {b['avg_mae_r']:+.2f}R"
            return line

        L.append("\nYOUR MEASURED RECORD (every closed trade, computed from the "
                 "ledger — this is what your decisions have actually produced, "
                 "not an impression):")
        L.append(_line("ALL", overall))
        for section, title in (("by_entry_style", "entry style"),
                               ("by_range_position", "where in the 24h range you entered"),
                               ("by_side", "side"),
                               ("by_close_reason", "which bracket filled"),
                               ("by_exit_kind", "what actually ended it"),
                               ("by_conviction", "the conviction you assigned"),
                               ("by_trigger", "what woke the cycle"),
                               ("by_volatility", "the market's volatility at entry")):
            buckets = perf.get(section) or {}
            if len(buckets) > 1 or (buckets and section == "by_range_position"):
                L.append(f"  {title}:")
                for label, b in buckets.items():
                    L.append("  " + _line(label, b))
        worst = perf.get("worst_markets") or {}
        if worst:
            L.append("  markets that have cost you most: " + ", ".join(
                f"{m} ${b['pnl']:+.2f} ({b['n']})" for m, b in worst.items()))
        # best_markets was computed every cycle and never rendered, so the prompt
        # could tell the model what had cost it money but never what had worked.
        # Avoidance is only half a policy: it needs to know where to lean IN.
        best = perf.get("best_markets") or {}
        if best:
            L.append("  markets that have paid you most: " + ", ".join(
                f"{m} ${b['pnl']:+.2f} ({b['n']})" for m, b in best.items()))

    if bundle.get("lessons"):
        L.append("\nYOUR MEMORY (lessons you wrote after earlier trades — you carry "
                 "nothing else between cycles, so treat these as hard-won and act on "
                 "them; PIN = your operator wrote it):")
        for lesson in bundle["lessons"]:
            tag = f"[{lesson['market']}] " if lesson.get("market") else ""
            pin = "PIN " if lesson.get("pinned") else ""
            L.append(f"- {pin}{tag}{lesson['text']} ({_age(now, lesson['ts'])} ago)")

    L.append("\nYOUR RECENT CLOSES (learn from them):")
    if bundle["closes"]:
        for c in bundle["closes"]:
            L.append(f"- {c['market']} {c['side']} entry={c['entry_px']} "
                     f"close={c['close_px']} pnl=${_optional(c['realized_pnl'], '+.2f')} "
                     f"reason={c['close_reason']} conv={c['conviction']}")
    else:
        L.append("- none yet")

    L.append("\nRECENT VENUE FILLS (newest available; opens and closes):")
    if bundle.get("fills"):
        for fill in bundle["fills"][:50]:
            L.append(
                f"- id={fill.get('tid') or fill.get('hash')} "
                f"{fill.get('coin')} {fill.get('dir')} sz={fill.get('sz')} "
                f"px={fill.get('px')} closedPnl={fill.get('closedPnl')} "
                f"fee={fill.get('fee')} time={fill.get('time')}"
            )
    else:
        L.append("- none available for this snapshot")

    if bundle.get("decisions"):
        L.append("\nRECENT DECISIONS (newest first; provenance, not live state):")
        for decision in bundle["decisions"][:8]:
            view = str(decision.get("market_view") or "—").replace("\n", " ")[:300]
            L.append(
                f"- id={decision.get('id')} trigger={decision.get('trigger')} "
                f"status={decision.get('status')} view={view}"
            )

    return "\n".join(L)


def build_prompt(bundle: dict, now: Optional[float] = None) -> str:
    context = render_context(bundle, now=now)
    return (
        f"{context}\n\nTRIGGER: {bundle['trigger']}\n"
        "Decide now. Search first if a catalyst needs verifying. JSON only."
    )


def build_chat_context(bundle: dict, now: Optional[float] = None) -> str:
    context_ts = _optional(bundle.get("context_ts"), ".0f")
    return (
        f"LIVE VENUE CONTEXT context_ts={context_ts}\n"
        f"{render_context(bundle, now=now)}\n"
        f"SOURCE TRIGGER: {bundle.get('trigger', 'chat')}"
    )


def _bounded_history(history: list[dict]) -> list[dict]:
    remaining = 20_000
    selected = []
    for message in reversed(history[-40:]):
        role = message.get("role")
        if role not in {"user", "assistant"}:
            continue
        content = str(message.get("content") or "")[:4000]
        if not content:
            continue
        if len(content) > remaining:
            content = content[:remaining]
        if not content:
            break
        selected.append({"role": role, "content": content})
        remaining -= len(content)
        if remaining == 0:
            break
    return list(reversed(selected))


class SearchTools:
    """Tavily + Exa search plus Trench cohort positioning. A failed call returns
    an error string the model can see — visibility, not fallback."""

    @staticmethod
    def _market_bias(market: str) -> str:
        if not market:
            return "error: empty market"
        try:
            from peri.trench import fetch_asset_bias
            bias = fetch_asset_bias(market)
        except Exception as exc:  # noqa: BLE001 — the model should see the failure
            return f"error: market_bias failed for {market}: {exc!r}"
        if bias is None:
            return (f"no cohort positioning for {market} — check the exact market "
                    "name (BTC, xyz:NVDA, io:ANTH)")
        lines = [f"{market}: {bias.get('long_traders')} wallets long / "
                 f"{bias.get('short_traders')} short"]
        for c in bias.get("cohorts") or []:
            lines.append(f"  {c.get('label')}: {c.get('long_pct', 0):.0f}% long "
                         f"({c.get('sentiment')})")
        if bias.get("divergence") is not None:
            lines.append(f"  smart {bias['smart_long_pct']:.0f}% long vs crowd "
                         f"{bias['crowd_long_pct']:.0f}% long = "
                         f"{bias['divergence']:+.0f}pp")
        return "\n".join(lines)

    def __init__(self, tavily_key: str = "", exa_key: str = ""):
        self.tavily_key = tavily_key
        self.exa_key = exa_key

    def available(self) -> bool:
        # market_bias needs no key, so there is always at least one tool
        return True

    def run(self, name: str, args: dict) -> str:
        if name == "market_bias":
            return self._market_bias(str(args.get("market", "")).strip())
        query = str(args.get("query", ""))[:300]
        if not query:
            return "error: empty query"
        try:
            if name == "web_search":
                if not self.tavily_key:
                    return "error: web_search not configured"
                r = httpx.post("https://api.tavily.com/search",
                               json={"api_key": self.tavily_key, "query": query,
                                     "max_results": 5, "topic": "news"},
                               timeout=20)
                r.raise_for_status()
                out = []
                for it in r.json().get("results", [])[:5]:
                    out.append(f"- {it.get('title', '')} ({it.get('url', '')}) "
                               f"{str(it.get('content', ''))[:300]}")
                return "\n".join(out) or "no results"
            if name == "deep_search":
                if not self.exa_key:
                    return "error: deep_search not configured"
                r = httpx.post("https://api.exa.ai/search",
                               headers={"x-api-key": self.exa_key},
                               json={"query": query, "numResults": 5,
                                     "contents": {"text": {"maxCharacters": 400}}},
                               timeout=20)
                r.raise_for_status()
                out = []
                for it in r.json().get("results", [])[:5]:
                    out.append(f"- {it.get('title', '')} ({it.get('url', '')}) "
                               f"{str(it.get('text', ''))[:300]}")
                return "\n".join(out) or "no results"
            return f"error: unknown tool {name}"
        except httpx.HTTPError as e:
            return f"error: search failed: {e!r}"


class Analyst:
    def __init__(self, cfg: AnalystCfg, api_key: str, base_url: str, model: str,
                 conviction_min: float, min_rr: float, max_leverage: float,
                 transport: Optional[Callable[[dict], object]] = None,
                 tools: Optional[SearchTools] = None,
                 risk_pct: float = 1.5, tp_floor: float = 0.0,
                 rails: Optional[RiskCfg] = None,
                 fallback_model: str = ""):
        if not (api_key and base_url and model):
            raise AnalystError("analyst not configured: need ANALYST_API_KEY, "
                               "ANALYST_BASE_URL, ANALYST_MODEL in .env")
        self.cfg = cfg
        self.model = model
        # A measurably faster brain for retries. Blank disables the swap.
        self.fallback_model = fallback_model
        rails = rails or RiskCfg(risk_pct=risk_pct, max_leverage=max_leverage,
                                 max_concurrent=1, daily_entry_cap=6,
                                 kill_switch_pct=15.0, min_rr=min_rr,
                                 stale_call_secs=900, cooldown_secs=3600,
                                 stop_cooldown_secs=14400, min_notional=10.0,
                                 slippage_pct=5.0, paper_bankroll=1000.0,
                                 tp_net_floor_usd=tp_floor)
        self.system = _SYSTEM.format(
            max_concurrent=rails.max_concurrent,
            conviction_min=conviction_min, min_rr=min_rr,
            max_lev=int(max_leverage), risk_pct=risk_pct, tp_floor=tp_floor,
            max_entry_offset=f"{MAX_ENTRY_OFFSET_PCT:g}",
            entry_expiry_mins=int(rails.entry_expiry_secs / 60),
            max_range_pos_long=f"{rails.max_range_pos_long:.2f}",
            min_range_pos_short=f"{rails.min_range_pos_short:.2f}",
            min_stop_pct=f"{rails.min_stop_pct:g}",
            atr_stop_mult=f"{rails.atr_stop_mult:g}",
            equity_blackout=max(rails.equity_open_blackout_mins,
                                rails.equity_close_blackout_mins),
            breakeven_at_r=f"{rails.breakeven_at_r:g}",
            trail_start_r=f"{rails.trail_start_r:g}",
            trail_giveback_r=f"{rails.trail_giveback_r:g}",
            trail_atr_mult=f"{rails.trail_atr_mult:g}",
            scale_out_at_r=f"{rails.scale_out_at_r:g}",
            scale_out_pct=f"{rails.scale_out_frac * 100:g}",
            time_stop_min_r=f"{rails.time_stop_min_r:g}",
            time_stop_hours=f"{rails.time_stop_secs / 3600:g}",
            daily_cap=rails.daily_entry_cap,
            day_loss_halt=f"{rails.day_loss_halt_pct:g}")
        self.chat_system = (
            _CHAT_SYSTEM
            + f"\nConfigured entry rails: conviction >= {conviction_min:g}, "
              f"reward:risk >= {min_rr:g}, ceiling {max_leverage:g}x, "
              f"stop >= {rails.min_stop_pct:g}% and >= {rails.atr_stop_mult:g}x ATR15m, "
              f"no long above {rails.max_range_pos_long:.2f} or short below "
              f"{rails.min_range_pos_short:.2f} of the 24h range, no builder-dex entry "
              f"within {max(rails.equity_open_blackout_mins, rails.equity_close_blackout_mins)}m "
              f"of the US open/close, {rails.daily_entry_cap} entries/day, entries halted "
              f"at -{rails.day_loss_halt_pct:g}% on the day, projected net TP >= "
              f"${rails.tp_net_floor_usd:g}. A resting entry must sit within "
              f"{MAX_ENTRY_OFFSET_PCT:g}% of the mark and expires in "
              f"{int(rails.entry_expiry_secs / 60)}m; re-opening that market "
              f"before then replaces the order rather than adding to it."
        )
        base = base_url.rstrip("/")
        path = "/chat/completions" if base.endswith("/v1") else "/v1/chat/completions"
        self.url = base + path
        self.headers = {"Authorization": f"Bearer {api_key}"}
        self._default_transport = transport is None
        self.transport = transport or self._http
        self.tools = tools or SearchTools()

    def _http(self, payload: dict) -> dict:
        payload = dict(payload)
        timeout = payload.pop("_timeout", None) or self.cfg.timeout_secs
        r = httpx.post(self.url, json=payload, headers=self.headers,
                       timeout=timeout)
        r.raise_for_status()
        return r.json()["choices"][0]["message"]

    def _http_stream(self, payload: dict):
        stream_payload = {**payload, "stream": True}
        with httpx.stream(
            "POST", self.url, json=stream_payload, headers=self.headers,
            timeout=self.cfg.timeout_secs,
        ) as response:
            response.raise_for_status()
            for line in response.iter_lines():
                if not line.startswith("data:"):
                    continue
                data = line[5:].strip()
                if not data or data == "[DONE]":
                    continue
                event = json.loads(data)
                choices = event.get("choices") or []
                if choices:
                    yield choices[0].get("delta") or {}

    def _stream_message(self, payload: dict,
                        on_event: Optional[ChatEvent]) -> dict:
        decoder = _AnswerStreamDecoder()
        content: list[str] = []
        calls_by_index: dict[int, dict] = {}
        for delta in self._http_stream(payload):
            piece = delta.get("content") or ""
            if piece:
                content.append(piece)
                answer_delta = decoder.feed(piece)
                if answer_delta and on_event is not None:
                    on_event("delta", {"delta": answer_delta})
            for fragment in delta.get("tool_calls") or []:
                index = int(fragment.get("index", 0))
                call = calls_by_index.setdefault(index, {
                    "id": "", "type": "function",
                    "function": {"name": "", "arguments": ""},
                })
                if fragment.get("id"):
                    call["id"] = fragment["id"]
                if fragment.get("type"):
                    call["type"] = fragment["type"]
                function = fragment.get("function") or {}
                call["function"]["name"] += function.get("name") or ""
                call["function"]["arguments"] += function.get("arguments") or ""
        return {
            "content": "".join(content),
            "tool_calls": [calls_by_index[i] for i in sorted(calls_by_index)],
        }

    @staticmethod
    def _message(raw) -> dict:
        """Normalize transport output: full message dict, or bare content str."""
        if isinstance(raw, dict):
            return raw
        return {"content": raw}

    def _parse(self, content: str) -> Decision:
        if not content:
            raise ValueError("empty content from model")
        content = content.strip()
        if content.startswith("```"):
            content = content.strip("`")
            content = content[4:] if content.startswith("json") else content
        return Decision.model_validate(json.loads(content))

    def _parse_chat(self, content: str) -> ChatResponse:
        if not content:
            raise ValueError("empty content from model")
        content = content.strip()
        if content.startswith("```"):
            content = content.strip("`")
            content = content[4:] if content.startswith("json") else content
        return ChatResponse.model_validate(json.loads(content))

    def chat(self, bundle: dict, history: list[dict], message: str,
             on_event: Optional[ChatEvent] = None) -> ChatResult:
        context = build_chat_context(bundle)
        last: Exception | None = None
        for _ in range(self.cfg.retries + 1):
            t0 = time.monotonic()
            streamed_answer = False

            def forward(kind: str, data: dict) -> None:
                nonlocal streamed_answer
                if kind == "delta":
                    streamed_answer = True
                if on_event is not None:
                    on_event(kind, data)

            try:
                return self._chat_once(
                    context, history, message, t0,
                    forward if on_event is not None else None,
                )
            except (httpx.HTTPError, json.JSONDecodeError, ValidationError,
                    KeyError, IndexError, TypeError, ValueError) as e:
                last = e
                if streamed_answer:
                    break
        raise AnalystError(
            f"analyst chat failed after {self.cfg.retries + 1} attempts: {last!r}"
        )

    def _chat_once(self, context: str, history: list[dict], message: str,
                   t0: float, on_event: Optional[ChatEvent] = None) -> ChatResult:
        messages = [
            {"role": "system", "content": self.chat_system},
            {"role": "user", "content": context},
            *_bounded_history(history),
            {"role": "user", "content": message},
        ]
        tool_log: list = []
        rounds = self.cfg.max_tool_rounds if self.tools.available() else 0

        for round_no in range(rounds + 1):
            final = round_no == rounds
            payload = {
                "model": self.model,
                "messages": messages,
                "temperature": self.cfg.temperature,
                "max_tokens": self.cfg.max_output_tokens,
            }
            if not final:
                payload["tools"] = TOOLS
            if self._default_transport:
                response_message = self._stream_message(payload, on_event)
            else:
                response_message = self._message(self.transport(payload))
            calls = response_message.get("tool_calls") or []
            if calls and not final:
                messages.append({
                    "role": "assistant",
                    "content": response_message.get("content") or "",
                    "tool_calls": calls,
                })
                parsed_calls = []
                for call in calls[:4]:
                    function = call.get("function", {})
                    name = function.get("name", "")
                    try:
                        args = json.loads(function.get("arguments") or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    parsed_calls.append((call, name, args))
                    if on_event is not None:
                        on_event("tool", {
                            "phase": "start", "tool": name, "args": args,
                        })
                for call, name, args, result in self._run_tool_calls(calls):
                    tool_log.append({
                        "tool": name,
                        "args": args,
                        "result": result[:2000],
                    })
                    if on_event is not None:
                        on_event("tool", {
                            "phase": "result", "tool": name, "args": args,
                            "result": result[:2000],
                        })
                    messages.append({
                        "role": "tool",
                        "tool_call_id": call.get("id", ""),
                        "content": result[:4000],
                    })
                continue

            raw = str(response_message.get("content") or "")
            parsed = self._parse_chat(raw)
            return ChatResult(
                response=parsed,
                latency_ms=int((time.monotonic() - t0) * 1000),
                raw=raw,
                tool_log=tool_log,
            )
        raise ValueError("tool loop ended without a chat response")

    def _run_tool_calls(self, calls: list[dict]) -> list[tuple[dict, str, dict, str]]:
        """Execute up to 4 tool calls of one round concurrently, preserving
        call order. Returns [(call, name, args, result)]."""
        parsed = []
        for c in calls[:4]:
            fn = c.get("function", {})
            name = fn.get("name", "")
            try:
                args = json.loads(fn.get("arguments") or "{}")
            except json.JSONDecodeError:
                args = {}
            parsed.append((c, name, args))
        if not parsed:
            return []
        with ThreadPoolExecutor(max_workers=len(parsed)) as pool:
            results = list(pool.map(lambda t: self.tools.run(t[1], t[2]), parsed))
        return [(c, name, args, res) for (c, name, args), res in zip(parsed, results)]

    def decide(self, bundle: dict) -> AnalystResult:
        prompt = build_prompt(bundle)
        # caller-wake fast path: the call IS the catalyst — decide from context,
        # skip search rounds (each round is another full reasoning pass)
        use_tools = self.tools.available() and not (
            self.cfg.caller_wake_skip_tools and bundle.get("trigger") == "caller message")
        started = time.monotonic()
        deadline = started + self.cfg.total_deadline_secs
        model = self.model
        last: Exception | None = None

        for attempt in range(self.cfg.retries + 1):
            remaining = deadline - time.monotonic()
            if attempt and remaining < MIN_ATTEMPT_SECS:
                raise AnalystError(
                    f"analyst gave up after {time.monotonic() - started:.0f}s "
                    f"({attempt} attempt(s)) — no time left inside the "
                    f"{self.cfg.total_deadline_secs}s deadline: {last!r}")
            t0 = time.monotonic()
            try:
                return self._decide_once(prompt, t0, use_tools=use_tools,
                                         model=model,
                                         timeout=min(self.cfg.timeout_secs, remaining))
            except (httpx.HTTPError, json.JSONDecodeError, ValidationError,
                    KeyError, IndexError, TypeError, ValueError) as e:
                last = e
                # A retry must be a genuinely cheaper question, and MEASUREMENT
                # says which lever works. Shrinking max_tokens does NOT: on
                # 2026-08-31 the same prompt at max_tokens=3000 produced 8,176
                # completion tokens (the cap is not applied to reasoning on this
                # endpoint) and took 225.8s versus 178.6s at 9000 — slower, not
                # faster. Every model reasons 6.5-8.4k tokens on this prompt; the
                # PROMPT is what demands the thinking.
                # What does work is a faster model: deepseek-v4-flash answered
                # the identical prompt in 80.7s against qwen3.8-max's 178.6s. So
                # the retry changes the brain, not the budget, and drops the
                # search rounds, each of which is another full reasoning pass.
                if self.fallback_model and model != self.fallback_model:
                    model = self.fallback_model
                use_tools = False
                print(f"[peri] analyst attempt {attempt + 1} failed ({type(e).__name__}); "
                      f"retrying on {model}, no tools", flush=True)
        raise AnalystError(f"analyst failed after {self.cfg.retries + 1} attempts: {last!r}")

    def _decide_once(self, prompt: str, t0: float,
                     use_tools: bool = True,
                     model: Optional[str] = None,
                     timeout: Optional[float] = None) -> AnalystResult:
        messages = [{"role": "system", "content": self.system},
                    {"role": "user", "content": prompt}]
        tool_log: list = []
        reasoning_parts: list[str] = []
        rounds = self.cfg.max_tool_rounds if (use_tools and self.tools.available()) else 0

        for round_no in range(rounds + 1):
            final = round_no == rounds
            payload = {
                "model": model or self.model,
                "messages": messages,
                "temperature": self.cfg.temperature,
                "max_tokens": self.cfg.max_output_tokens,
            }
            if timeout is not None:
                payload["_timeout"] = timeout   # stripped by the transport
            # NOTE: no response_format=json_object — DashScope suppresses
            # reasoning_content under JSON mode; strict pydantic validation +
            # retries are the real contract, and the glass-box needs the chain.
            if not final:
                payload["tools"] = TOOLS
            msg = self._message(self.transport(payload))
            if msg.get("reasoning_content"):
                reasoning_parts.append(str(msg["reasoning_content"]))

            calls = msg.get("tool_calls") or []
            if calls and not final:
                messages.append({"role": "assistant", "content": msg.get("content") or "",
                                 "tool_calls": calls})
                for c, name, args, result in self._run_tool_calls(calls):
                    tool_log.append({"tool": name, "args": args,
                                     "result": result[:2000]})
                    messages.append({"role": "tool", "tool_call_id": c.get("id", ""),
                                     "content": result[:4000]})
                continue

            decision = self._parse(msg.get("content") or "")
            return AnalystResult(decision=decision,
                                 latency_ms=int((time.monotonic() - t0) * 1000),
                                 raw=str(msg.get("content") or ""),
                                 reasoning="\n---\n".join(reasoning_parts),
                                 prompt=prompt, tool_log=tool_log,
                                 model=model or self.model)
        raise ValueError("tool loop ended without a decision")
