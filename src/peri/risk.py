"""The risk guard. Code owns risk; the LLM owns direction.

Gate order is FROZEN (kestrel lineage), extended 2026-08-29 after a day that
lost 12.8% taking the same trade seven times (chase an extended move at the
edge of the range with a noise-width stop):

    kill -> operator pause -> day-loss halt -> max-concurrent -> daily-cap ->
    dup-market -> venue-order -> cooldown -> conviction -> stale-mirror ->
    equity-session blackout -> range-edge -> stop-sanity -> RR -> leverage ->
    sizing -> min-notional -> margin -> projected-net-TP

Sizing derives from stop distance, never the reverse:

    risk_usd  = equity * risk_pct / 100
    notional  = risk_usd / stop_distance_fraction
    leverage  = action.leverage (only exact 10x or 20x when supported)
    margin    = notional / leverage
"""

import math
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Optional, Union
from zoneinfo import ZoneInfo

from peri.config import RiskCfg
from peri.fees import HL_SCHEDULE, FeeSchedule
from peri.hl_sizing import format_price, notional_to_size
from peri.models import OpenAction
from peri.state import State


@dataclass
class Approved:
    market: str
    side: str
    notional: float
    size_usd_risk: float
    leverage: float
    margin: float
    margin_mode: str
    stop_px: float
    tp_px: float
    entry_px: float = 0.0    # the price every number above was derived from
    resting: bool = False    # True when entry_px is a resting limit, not the mark
    size: float = 0.0        # the EXACT venue lot; nothing downstream re-rounds
    # The take-profit split, resolved HERE so the adapters place it rather than
    # deriving their own. None = one full-size target, the original behaviour.
    scale_out: "Optional[ScaleOut]" = None


@dataclass
class Refusal:
    reason: str


@dataclass(frozen=True)
class ScaleOut:
    """A take-profit split into a banked tranche and a runner.

    tp1_px/tp1_size close `scale_out_frac` of the position at
    `scale_out_at_r`; runner_size rides on to the analyst's own target with the
    trail behind it. Sizes FLOOR to the venue lot and always sum to the
    approved size exactly — the runner takes the remainder, so rounding can
    never leave a sliver of the position unprotected."""
    tp1_px: float
    tp1_size: float
    runner_size: float
    at_r: float

    def blended_r(self, rr: float) -> float:
        """Reward in R once the target is reached, weighted by lot.

        Not used for sizing — for the honest TP projection. With part of the
        position banked at `at_r`, reward at target is no longer `rr` across
        the whole lot, and a floor that ignored that would be inflated by
        construction. Refuse rather than inflate."""
        total = self.tp1_size + self.runner_size
        if total <= 0:
            return rr
        w = self.tp1_size / total
        return w * self.at_r + (1.0 - w) * rr


def plan_scale_out(side: str, entry_px: float, stop_px: float, tp_px: float,
                   size: float, sz_decimals: int, *, at_r: float, frac: float,
                   min_notional: float) -> Optional[ScaleOut]:
    """Split one target into a banked tranche and a runner, or None.

    ONE definition, shared by the gate, the live adapter and the paper adapter,
    so dry mode can never model a different trade from the one live would place
    (the F1 lesson from 2026-09-07: the market path let the adapter re-derive a
    size and sent more risk than was approved).

    Returns None — meaning "ship the single full-size target unchanged" —
    whenever a split would be dishonest rather than useful: the feature is off,
    the analyst's own target is already nearer than the tranche, or either
    piece floors to nothing or to dust below the venue minimum."""
    if at_r <= 0 or not (0.0 < frac < 1.0) or size <= 0:
        return None
    risk = abs(entry_px - stop_px)
    if risk <= 0:
        return None
    tp1 = (entry_px + at_r * risk) if side == "long" else (entry_px - at_r * risk)
    # The runner's target has to be strictly beyond the tranche, or this is not
    # a scale-out — it is two orders at the same level.
    if side == "long" and not tp1 < tp_px:
        return None
    if side == "short" and not tp1 > tp_px:
        return None
    step = 10 ** -sz_decimals
    tp1_size = round(math.floor((size * frac) / step + 1e-9) * step, sz_decimals)
    runner = round(size - tp1_size, sz_decimals)
    if tp1_size <= 0 or runner <= 0:
        return None
    tp1 = format_price(tp1, sz_decimals)
    # A tranche too small to be worth a fill is churn: it pays a full builder
    # fee to bank pennies and leaves the runner under-sized.
    if tp1_size * tp1 < min_notional or runner * tp_px < min_notional:
        return None
    return ScaleOut(tp1_px=tp1, tp1_size=tp1_size, runner_size=runner, at_r=at_r)


def _stop_distance(side: str, mark: float, stop: float) -> Optional[float]:
    """Fractional distance mark->stop; None if the stop is on the wrong side."""
    if side == "long" and stop < mark:
        return (mark - stop) / mark
    if side == "short" and stop > mark:
        return (stop - mark) / mark
    return None


def _rr(side: str, mark: float, stop: float, tp: float) -> Optional[float]:
    risk = abs(mark - stop)
    reward = tp - mark if side == "long" else mark - tp
    if risk <= 0 or reward <= 0:
        return None
    return reward / risk


MAX_ENTRY_OFFSET_PCT = 5.0   # a resting entry further than this never fills
MIN_ENTRY_OFFSET_PCT = 0.1   # ...and closer than this is a market order wearing a hat
# A threshold met exactly must pass. 1.8/90.0 is 0.019999999999999997 in binary,
# so a stop placed at precisely the 2.00% floor was refused with the message
# "stop 2.00% from entry < 2.0% floor" — unsatisfiable, and unreadable. This is
# a representation tolerance, not slack in the rail: it is far below the
# precision of any price the venue accepts.
BOUNDARY_EPS = 1e-9
LIQ_SAFETY = 1.3             # liquidation must sit this many stop-distances away


def breakeven_px(side: str, entry_px: float, fee_rate: float) -> float:
    """Entry plus the round trip, so a winner cannot become a loser."""
    buffer = 1 + 2 * fee_rate
    return entry_px * buffer if side == "long" else entry_px / buffer


def trail_band(peak: float, stop_dist: Optional[float], atr_pct: Optional[float],
               *, giveback_r: float, atr_mult: float) -> float:
    """How far behind the high-water mark a trailing stop sits, in price.

    A fraction of the RISK TAKEN, floored by the market's own noise. Expressed
    in R because that is what the giveback means: the first version used a raw
    1x-ATR band against a stop that is never tighter than 4x ATR, so reaching
    +0.5R moved the stop from -4 ATR to +1 ATR in one step and cut winners at a
    quarter of an R while losers paid the full one.

    ONE definition, shared by the live engine and the replay harness — the two
    must never be able to disagree about what the strategy does."""
    band = 0.0
    if giveback_r > 0 and stop_dist:
        band = giveback_r * stop_dist
    if atr_pct and atr_pct > 0:
        band = max(band, peak * (atr_pct / 100.0) * atr_mult)
    return band


def trail_target(side: str, peak: float, stop_dist: Optional[float],
                 atr_pct: Optional[float], *, giveback_r: float,
                 atr_mult: float) -> Optional[float]:
    """The trailing stop price, or None when no honest band can be computed."""
    band = trail_band(peak, stop_dist, atr_pct,
                      giveback_r=giveback_r, atr_mult=atr_mult)
    if band <= 0:
        return None
    return peak - band if side == "long" else peak + band


def range_position(features: Optional[dict], px: float,
                   resting: bool) -> Optional[float]:
    """Where `px` sits in the last 24h range: 0.0 at the low, 1.0 at the high.

    ONE definition, because there were two. The gate judges a resting entry at
    its own level (2026-08-30: measuring at the mark refused 12 legitimate
    entries in 36h), but the ledger recorded the mark-derived figure — so
    `by_range_position` in the measured record was bucketing a different number
    from the one that actually gated the trade, and the analyst was learning
    from a statistic about a decision nobody made."""
    if resting:
        hi = (features or {}).get("hi_24h")
        lo = (features or {}).get("lo_24h")
        if (isinstance(hi, (int, float)) and isinstance(lo, (int, float))
                and hi > lo > 0):
            return min(1.0, max(0.0, (px - lo) / (hi - lo)))
    pos = (features or {}).get("range24h_pos")
    return pos if isinstance(pos, (int, float)) else None


def isolated_liq_distance(leverage: float, market_max_lev: float) -> float:
    """Fractional adverse move that liquidates an ISOLATED position.

    HL's maintenance margin is half the asset's max leverage, so the buffer is
    the posted margin minus that: 1/L - 1/(2*Lmax). Verified against a live
    position (BTC 10x, max 40: predicted 8.75%, venue said 8.57%) — the estimate
    runs slightly wide, which is why LIQ_SAFETY exists."""
    if leverage <= 0 or market_max_lev <= 0:
        return 0.0
    return max(0.0, 1.0 / leverage - 1.0 / (2.0 * market_max_lev))

_NY = ZoneInfo("America/New_York")

# NYSE/Nasdaq full closures, as literal dates: the alternative is a dependency
# that has to be right about the past as well as the future. Extend each
# December. A date NOT listed is treated as a normal session, so staleness costs
# a blackout that fails to fire, never a trade wrongly blocked.
US_MARKET_HOLIDAYS = frozenset({
    "2026-01-01", "2026-01-19", "2026-02-16", "2026-04-03", "2026-05-25",
    "2026-06-19", "2026-07-03", "2026-09-07", "2026-11-26", "2026-12-25",
    "2027-01-01", "2027-01-18", "2027-02-15", "2027-03-26", "2027-05-31",
    "2027-06-18", "2027-07-05", "2027-09-06", "2027-11-25", "2027-12-24",
})
# Early closes at 13:00 ET (July 3rd eve, the day after Thanksgiving, Dec 24).
US_HALF_DAYS = frozenset({
    "2026-07-02", "2026-11-27", "2026-12-24", "2027-11-26",
})

# Builder-dex names whose underlying is a CME/ICE future, not a cash equity.
# These do NOT keep stock-market hours: Globex runs Sunday 18:00 ET straight
# through to Friday 17:00 ET with a one-hour maintenance halt each day at 17:00.
# Treating them as "closed at the weekend" wrongly blocked live oil and metals
# every Sunday evening (verified 2026-08-30 23:09 ET: xyz:CL was doing $18M a
# 90-minute window and moving tick by tick).
CME_HOURS_MARKETS = frozenset({
    "GOLD", "SILVER", "COPPER", "NATGAS", "URANIUM", "ALUMINIUM", "PLATINUM",
    "PALLADIUM", "BRENTOIL", "CL", "WTI", "CORN", "WHEAT", "SOYBEAN",
    "SP500", "XYZ100", "VIX", "US500", "USTECH", "SMALL2000", "USBOND",
})


# Builder-dex names whose underlying is a FOREIGN cash equity. Their session is
# the home exchange's, not New York's: SK Hynix is the biggest market on the xyz
# dex ($298M/24h) and trades while the US sleeps. Only symbols whose listing is
# unambiguous are mapped — the rest stay on the conservative US rule, because a
# guess about which exchange a synthetic tracks is worse than a blocked trade.
_KRX = ("Asia/Seoul", ((9 * 60, 15 * 60 + 30),))
_TSE = ("Asia/Tokyo", ((9 * 60, 11 * 60 + 30), (12 * 60 + 30, 15 * 60 + 30)))
_HKEX = ("Asia/Hong_Kong", ((9 * 60 + 30, 12 * 60), (13 * 60, 16 * 60)))
_SSE = ("Asia/Shanghai", ((9 * 60 + 30, 11 * 60 + 30), (13 * 60, 15 * 60)))
FOREIGN_EQUITY_SESSIONS = {
    # Korea
    "SKHX": _KRX, "SKHY": _KRX, "SMSN": _KRX, "HYUNDAI": _KRX,
    # Japan
    "SOFTBANK": _TSE, "KIOXIA": _TSE, "IBIDEN": _TSE,
    # Hong Kong — Z.ai (02513.HK, listed 2026-01-08) and MiniMax (2026-01-09)
    "TENCENT": _HKEX, "ZHIPU": _HKEX, "MINIMAX": _HKEX,
    # Shanghai STAR / main board — Unitree (688836, listed 2026-08-19),
    # CXMT (listed 2026-07-27), GigaDevice (603986)
    "UNITREE": _SSE, "CXMT": _SSE, "GIGADEV": _SSE,
}

# Synthetics on a company that is NOT YET LISTED have no exchange behind them:
# nothing opens or closes, so they trade like crypto and must not be weekend-
# blocked as if they were US stocks.
#
# Check before adding: SPCX was in this set and should not have been — SpaceX
# IPO'd on Nasdaq on 2026-06-12 at $135, so it keeps US cash hours like any
# other listed stock. Anthropic filed confidentially in June 2026 and is
# targeting an OCTOBER 2026 Nasdaq listing: move ANTH out of here the day it
# lists. OpenAI is now leaning toward 2027.
NO_SESSION_MARKETS = frozenset({
    "ANTH", "ANTHROPIC", "OPENAI", "H100",
})


def foreign_session_open(ticker: str, now: float) -> Optional[str]:
    """None when the home exchange is trading; otherwise why it is not.
    KRX/TSE/HKEX observe no DST, but ZoneInfo handles that for us."""
    spec = FOREIGN_EQUITY_SESSIONS.get(ticker)
    if spec is None:
        return None
    zone, windows = spec
    local = datetime.fromtimestamp(now, tz=timezone.utc).astimezone(ZoneInfo(zone))
    if local.weekday() >= 5:
        return f"{zone.split('/')[-1]} is closed for the weekend"
    minute = local.hour * 60 + local.minute
    if any(start <= minute < end for start, end in windows):
        return None
    return (f"{zone.split('/')[-1]} cash market is closed "
            f"(local time {local.strftime('%H:%M')})")


def cme_session_open(now: float) -> Optional[str]:
    """None when Globex is trading; otherwise why it is not.

    Sunday 18:00 ET -> Friday 17:00 ET, with a 60-minute halt each day at 17:00.
    """
    ny = datetime.fromtimestamp(now, tz=timezone.utc).astimezone(_NY)
    weekday, minute = ny.weekday(), ny.hour * 60 + ny.minute
    if weekday == 5:
        return "Globex is shut all Saturday"
    if weekday == 6:
        if minute < 18 * 60:
            return "Globex reopens at 18:00 ET on Sunday"
        return None
    if weekday == 4 and minute >= 17 * 60:
        return "Globex closed for the week at 17:00 ET Friday"
    if 17 * 60 <= minute < 18 * 60:
        return "inside the daily 17:00-18:00 ET Globex halt"
    return None


def us_session_minutes(now: float) -> Optional[tuple[float, float]]:
    """Minutes since the US equity open and until the close, or None when the
    cash market is shut.

    DST-aware, and aware that a half-day closes at 13:00 — without that the
    closing blackout never fires and builder-dex entries stay open for three
    hours after the underlying has stopped trading."""
    ny = datetime.fromtimestamp(now, tz=timezone.utc).astimezone(_NY)
    if ny.weekday() >= 5:
        return None
    day = ny.strftime("%Y-%m-%d")
    if day in US_MARKET_HOLIDAYS:
        return None
    close_minute = (13 * 60) if day in US_HALF_DAYS else (16 * 60)
    minute = ny.hour * 60 + ny.minute + ny.second / 60.0
    return minute - (9 * 60 + 30), close_minute - minute


class Guard:
    def __init__(self, cfg: RiskCfg, state: State, conviction_min: float,
                 fees: FeeSchedule = HL_SCHEDULE):
        self.cfg = cfg
        self.state = state
        self.conviction_min = conviction_min
        self.fees = fees

    def gate_open(self, a: OpenAction, equity: float, mark: float,
                  market_max_lev: float, day: str,
                  *, available_margin: float,
                  reserved_order_markets: frozenset[str],
                  enforce_max_concurrent: bool = True,
                  now: Optional[float] = None,
                  features: Optional[dict] = None,
                  day_pnl_pct: Optional[float] = None,
                  sz_decimals: Optional[int] = None) -> Union[Approved, Refusal]:
        now = now or time.time()
        positions = self.state.open_positions()
        open_markets = {p.market for p in positions}
        peri_open_markets = {p.market for p in positions if p.source != "external"}
        pending_order_markets = reserved_order_markets - open_markets
        peri_occupied_markets = peri_open_markets | pending_order_markets

        if self.state.kill_tripped(day):
            return Refusal("kill switch tripped — entries halted for the day")
        if self.state.paused():
            return Refusal("paused by operator — new entries halted from the dashboard")
        if (self.cfg.day_loss_halt_pct > 0 and day_pnl_pct is not None
                and day_pnl_pct <= -self.cfg.day_loss_halt_pct):
            return Refusal(
                f"day down {day_pnl_pct:.1f}% — new entries halted at the "
                f"-{self.cfg.day_loss_halt_pct:.0f}% soft limit (stop digging)")
        if (
            enforce_max_concurrent
            and len(peri_occupied_markets) >= self.cfg.max_concurrent
        ):
            return Refusal(
                f"max concurrent Peri positions ({self.cfg.max_concurrent}) reached")
        # Count orders ALREADY ON THE BOOK, not just filled entries. A resting
        # entry was only counted when it filled, so three orders parked against
        # a cap sitting at 2/3 all cleared this gate and could all fill — five
        # entries on a three-entry day. A fill moves one from resting to
        # counted; an expiry gives the slot back, so the total stays honest.
        taken = self.state.entries_today(day) + self.state.resting_entries_today(day)
        if taken >= self.cfg.daily_entry_cap:
            return Refusal(f"daily entry cap ({self.cfg.daily_entry_cap}) reached"
                           f" — {taken} entered or resting today")
        if a.market in open_markets:
            return Refusal(f"position already open on {a.market}")
        if a.market in reserved_order_markets:
            return Refusal(f"venue order already open on {a.market}")
        until = self.state.cooldown_until(a.market, now)
        if until:
            return Refusal(f"{a.market} in cooldown for {int(until - now)}s")
        if a.conviction < self.conviction_min:
            return Refusal(f"conviction {a.conviction:.2f} < floor {self.conviction_min:.2f}")
        if a.source == "mirror":
            msg = self.state.tg_message(a.mirror_msg_id) if a.mirror_msg_id else None
            if msg is None:
                return Refusal("mirror entry without a resolvable telegram message id")
            if now - msg["ts"] > self.cfg.stale_call_secs:
                return Refusal(f"mirrored call is stale ({int(now - msg['ts'])}s "
                               f"> {self.cfg.stale_call_secs}s)")

        # -- entry price: the mark, or a resting limit at a better level -----
        entry_px, resting = mark, False
        if a.entry is not None:
            entry_px, resting = a.entry, True
            if a.side == "long" and entry_px >= mark:
                return Refusal(
                    f"resting long entry {entry_px} is at/above mark {mark} — a limit "
                    "that crosses is just a market order; rest BELOW the mark")
            if a.side == "short" and entry_px <= mark:
                return Refusal(
                    f"resting short entry {entry_px} is at/below mark {mark} — a limit "
                    "that crosses is just a market order; rest ABOVE the mark")
            offset = abs(entry_px - mark) / mark * 100
            if offset > MAX_ENTRY_OFFSET_PCT:
                return Refusal(
                    f"resting entry {offset:.1f}% from mark exceeds "
                    f"{MAX_ENTRY_OFFSET_PCT:.0f}% — it would never fill")
            if offset < MIN_ENTRY_OFFSET_PCT:
                return Refusal(
                    f"resting entry {offset:.3f}% from mark is inside the tick/"
                    f"rounding band — it crosses and fills as a taker at the mark, "
                    f"which is the chase you were avoiding. Rest at least "
                    f"{MIN_ENTRY_OFFSET_PCT:g}% away")

        # -- calendar blackout: do not enter into a scheduled shock -----------
        if self.cfg.event_blackout_mins > 0:
            event = self.state.next_high_impact(now)
            if event is not None:
                mins = (event["ts"] - now) / 60.0
                if 0 <= mins <= self.cfg.event_blackout_mins:
                    return Refusal(
                        f"{event['title']} lands in {mins:.0f}m — inside the "
                        f"{self.cfg.event_blackout_mins}m blackout before a high-impact "
                        "release. A position opened into a scheduled shock is a coin "
                        "flip, not a thesis; wait for the print and trade the reaction")

        # -- session blackout: the US open and close are where slippage lives -
        if ":" in a.market and a.market.split(":")[-1].upper() in NO_SESSION_MARKETS:
            pass          # a private-company synthetic never closes
        elif ":" in a.market and a.market.split(":")[-1].upper() in CME_HOURS_MARKETS:
            shut = cme_session_open(now)
            if shut is not None:
                return Refusal(
                    f"{a.market}: {shut} — the underlying future is not trading, so "
                    "this price is perp flow against a stale reference")
        elif ":" in a.market and a.market.split(":")[-1].upper() in FOREIGN_EQUITY_SESSIONS:
            shut = foreign_session_open(a.market.split(":")[-1].upper(), now)
            if shut is not None:
                return Refusal(
                    f"{a.market}: {shut} — the underlying is not trading, so this "
                    "price is perp flow against a stale reference")
        elif ":" in a.market:
            session = us_session_minutes(now)
            if session is None:
                # On a weekend NOTHING on the builder dex has a live underlying:
                # equities are shut and CME commodities close Friday 17:00 ET
                # until Sunday 18:00. The perp still prints, but on its own flow
                # against a stale reference — it either sits pinned or gets
                # pushed, and Monday's open re-prices it straight through any
                # stop. equity_rth_only stays the knob for WEEKDAY off-hours,
                # where futures genuinely trade.
                weekday = datetime.fromtimestamp(
                    now, tz=timezone.utc).astimezone(_NY).weekday()
                if weekday >= 5:
                    return Refusal(
                        f"{a.market}: weekend — the underlying is not trading, so "
                        "this price is perp flow against a stale reference")
                if self.cfg.equity_rth_only:
                    return Refusal(f"{a.market}: US cash market closed (holiday)")
            else:
                since_open, until_close = session
                if self.cfg.equity_rth_only and not (0 <= since_open and until_close > 0):
                    return Refusal(
                        f"{a.market}: outside regular US hours "
                        f"({since_open:+.0f}m from the open)")
                blackout = self.cfg.equity_open_blackout_mins
                if blackout > 0 and -blackout <= since_open < blackout:
                    return Refusal(
                        f"{a.market}: {since_open:+.0f}m from the US open — inside the "
                        f"{blackout}m opening blackout (gap risk, no reliable level)")
                blackout = self.cfg.equity_close_blackout_mins
                if blackout > 0 and 0 < until_close <= blackout:
                    return Refusal(
                        f"{a.market}: {until_close:.0f}m to the US close — inside the "
                        f"{blackout}m closing blackout (no time to work, overnight gap)")

        # -- range edge: never chase a move that has already happened ---------
        # These gates FAIL CLOSED. A missing feature used to mean "skip", which
        # switched the 08-29 rails off for exactly the market whose candles were
        # down — refuse instead, and say why.
        # Judged where the ENTRY sits, not where the mark sits — see
        # range_position(), which the ledger now shares so the measured record
        # buckets the same number this gate ruled on.
        range_pos = range_position(features, entry_px, resting)
        gate_needs_range = (self.cfg.max_range_pos_long < 1.0
                            or self.cfg.min_range_pos_short > 0.0)
        if gate_needs_range and not isinstance(range_pos, (int, float)):
            return Refusal(
                f"24h range position unavailable for {a.market} — cannot verify "
                "the entry is not chasing; refusing rather than trading blind")
        if isinstance(range_pos, (int, float)):
            where = "your entry" if resting else "the mark"
            if a.side == "long" and range_pos > self.cfg.max_range_pos_long:
                return Refusal(
                    f"long with {where} at {range_pos:.2f} of the 24h range > "
                    f"{self.cfg.max_range_pos_long:.2f} — that is chasing the high; "
                    "rest a limit lower in the range")
            if a.side == "short" and range_pos < self.cfg.min_range_pos_short:
                return Refusal(
                    f"short with {where} at {range_pos:.2f} of the 24h range < "
                    f"{self.cfg.min_range_pos_short:.2f} — that is chasing the low; "
                    "rest a limit higher in the range")

        dist = _stop_distance(a.side, entry_px, a.stop)
        if dist is None:
            return Refusal(f"stop {a.stop} on wrong side of entry {entry_px} for {a.side}")
        if dist < 0.001:
            return Refusal(f"stop distance {dist:.4%} < 0.1% — noise-width stop")
        if (self.cfg.min_stop_pct > 0
                and dist * 100 < self.cfg.min_stop_pct - BOUNDARY_EPS):
            return Refusal(
                f"stop {dist * 100:.2f}% from entry < {self.cfg.min_stop_pct:.1f}% floor "
                "— noise takes that out before the thesis plays; widen the stop and "
                "let the smaller size carry the risk")
        atr = (features or {}).get("atr15m_pct")
        if self.cfg.atr_stop_mult > 0 and not isinstance(atr, (int, float)):
            return Refusal(
                f"ATR15m unavailable for {a.market} — cannot verify the stop "
                "clears this market's noise band; refusing rather than guessing")
        if (self.cfg.atr_stop_mult > 0 and isinstance(atr, (int, float)) and atr > 0
                and dist * 100 < self.cfg.atr_stop_mult * atr - BOUNDARY_EPS):
            return Refusal(
                f"stop {dist * 100:.2f}% < {self.cfg.atr_stop_mult:g}x ATR15m "
                f"({atr:.2f}%) — inside the noise band of this market")
        rr = _rr(a.side, entry_px, a.stop, a.take_profit)
        if rr is None:
            return Refusal(
                f"take_profit {a.take_profit} on wrong side of entry {entry_px} for {a.side}")
        if rr < self.cfg.min_rr - BOUNDARY_EPS:
            return Refusal(f"RR {rr:.2f} < floor {self.cfg.min_rr:.1f}")

        risk_usd = equity * self.cfg.risk_pct / 100.0
        notional = risk_usd / dist
        if a.leverage > self.cfg.max_leverage:
            return Refusal(
                f"requested leverage {a.leverage:g}x exceeds configured maximum "
                f"{self.cfg.max_leverage:g}x"
            )
        if a.leverage > market_max_lev:
            return Refusal(
                f"requested leverage {a.leverage:g}x exceeds venue maximum "
                f"{market_max_lev:g}x"
            )
        # A stop the venue never reaches is not a stop. At 20x isolated on a
        # 20x-max market liquidation is ~2.5% away, and the 2% minimum-stop rail
        # pushes stops right into it: 2026-08-28's palladium trade carried a
        # 2.75% stop at 20x and would have liquidated before it triggered.
        if a.margin_mode == "isolated":
            liq_dist = isolated_liq_distance(a.leverage, market_max_lev)
            if dist * LIQ_SAFETY >= liq_dist:
                safe_lev = 1.0 / (dist * LIQ_SAFETY + 1.0 / (2.0 * market_max_lev))
                return Refusal(
                    f"stop {dist:.2%} is inside the ~{liq_dist:.2%} isolated "
                    f"liquidation band at {a.leverage:g}x — the position would be "
                    f"liquidated before the stop ever fired. Use "
                    f"{'10x' if safe_lev >= 10 else 'cross margin'} for this stop, "
                    "or a tighter one that still clears the noise floor")
        leverage = a.leverage
        margin = notional / leverage
        # Round to the venue lot HERE, then judge the numbers that will actually
        # be sent. The guard used to size in dollars and let the engine re-round,
        # so min-notional, margin and the TP floor were checked against a size
        # the venue would never see.
        size = notional / entry_px
        if sz_decimals is not None:
            size = notional_to_size(notional, entry_px, sz_decimals)
            if size <= 0:
                return Refusal(
                    f"one venue lot of {a.market} is "
                    f"${10 ** -sz_decimals * entry_px:.2f} — more than the "
                    f"${notional:.2f} this risk budget can buy")
            notional = entry_px * size
            margin = notional / leverage
        if notional < self.cfg.min_notional:
            return Refusal(f"notional ${notional:.2f} < venue min ${self.cfg.min_notional:.0f}"
                           " — risk too small to trade honestly")
        if margin > available_margin:
            return Refusal(
                f"margin ${margin:.2f} exceeds available ${available_margin:.2f}"
            )
        if margin > equity * 0.9:
            return Refusal(f"margin ${margin:.2f} would exceed 90% of equity ${equity:.2f}")
        actual_risk = abs(entry_px - a.stop) * size
        if actual_risk > risk_usd * 1.05:
            return Refusal(
                f"the venue lot risks ${actual_risk:.2f}, above the approved "
                f"${risk_usd:.2f} — this market's lot is too coarse for this size")

        # projected-net-TP floor (2026-08-28 policy: aim $4-5, refuse below the
        # configured floor). The rates used to be inlined here as literals,
        # because risk.py cannot import router without a cycle — so the gate
        # deciding whether a trade clears its costs could silently disagree with
        # the ledger booking them. peri.fees imports nothing and settles it.
        scale_out = plan_scale_out(
            a.side, entry_px, a.stop, a.take_profit, size, sz_decimals or 0,
            at_r=self.cfg.scale_out_at_r, frac=self.cfg.scale_out_frac,
            min_notional=self.cfg.min_notional)

        if self.cfg.tp_net_floor_usd > 0:
            # self.fees, not the module constants: a zero-fee venue must price
            # its trades at zero rather than inherit HL's schedule.
            round_trip_fees = notional * self.fees.round_trip_rate(resting)
            # against actual_risk, not the pre-floor budget: the lot was FLOORED
            # to the venue step a few lines up, so `risk_usd` is what we wanted
            # to stake and `actual_risk` is what this lot actually stakes. Using
            # the budget overstated the projection by the whole flooring gap —
            # on a coarse-lot market that is the difference between clearing the
            # floor on paper and clearing it in the account.
            # And with a tranche banked at scale_out_at_r, reward at target is
            # the lot-weighted blend, not rr across the whole position:
            # projecting the un-blended figure would inflate this floor by
            # construction. Both corrections make the gate stricter.
            reward_r = scale_out.blended_r(rr) if scale_out else rr
            projected_net_tp = reward_r * actual_risk - round_trip_fees
            if projected_net_tp < self.cfg.tp_net_floor_usd:
                return Refusal(
                    f"projected net TP ${projected_net_tp:.2f} < floor "
                    f"${self.cfg.tp_net_floor_usd:.2f} — pick a stronger setup "
                    "(more RR), never a tighter noise stop")

        return Approved(market=a.market, side=a.side, notional=notional,
                        size_usd_risk=risk_usd, leverage=leverage, margin=margin,
                        margin_mode=a.margin_mode,
                        stop_px=a.stop, tp_px=a.take_profit, scale_out=scale_out,
                        entry_px=entry_px, resting=resting, size=size)

    def cooldown_after_close(self, market: str, reason: str,
                             now: Optional[float] = None,
                             pnl: Optional[float] = None) -> None:
        """Asymmetric: LOSING stop-outs earn the long cooldown (thesis was
        wrong — cool off). A profitable trailing-stop exit is a win and gets
        the normal cooldown; unknown pnl stays conservative (long)."""
        now = now or time.time()
        losing_stop = reason == "sl" and (pnl is None or pnl < 0)
        secs = self.cfg.stop_cooldown_secs if losing_stop else self.cfg.cooldown_secs
        self.state.set_cooldown(market, now + secs, reason)
