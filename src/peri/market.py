"""Dex-aware read-only data layer over the Hyperliquid info API.

One Market instance serves both universes: native perps (crypto majors) and the
builder dex (equities + commodities, e.g. "xyz:NVDA"). Everything is keyed by the
full market name; the dex is inferred from the prefix. `post` is injected so
tests never touch the network.
"""

import math
import time
from dataclasses import dataclass
from typing import Callable, Optional

import httpx

from peri.fees import TAKER_FEE_RATE

INFO_URLS = {
    "mainnet": "https://api.hyperliquid.xyz/info",
    "testnet": "https://api.hyperliquid-testnet.xyz/info",
}


def http_post(network: str) -> Callable[[dict], object]:
    url = INFO_URLS[network]

    def post(body: dict) -> object:
        r = httpx.post(url, json=body, timeout=15)
        r.raise_for_status()
        return r.json()

    return post


@dataclass
class MarketInfo:
    name: str
    sz_decimals: int
    max_leverage: float


@dataclass
class Ctx:
    name: str
    mark: float
    day_pct: float
    funding_apr_pct: float   # hourly funding annualized, in %
    oi_usd: float
    vol_usd: float
    # Both come free in the same metaAndAssetCtxs payload and were being
    # discarded. The premium matters most on builder-dex names trading against
    # a shut underlying — a synthetic can drift a long way from its oracle
    # overnight, and the analyst was pricing entries off the mark alone.
    spread_bps: float = 0.0
    premium_pct: float = 0.0


class Market:
    def __init__(self, post: Callable[[dict], object], dexes):
        self.post = post
        self.dexes = tuple(dexes)
        self._meta: dict[str, MarketInfo] = {}
        self._candle_cache: dict[tuple[str, str], tuple[float, list[dict]]] = {}

    # -- meta --------------------------------------------------------------
    def refresh_meta(self) -> None:
        merged: dict[str, MarketInfo] = {}
        for dex in ("", *self.dexes):
            body = {"type": "meta"} if not dex else {"type": "meta", "dex": dex}
            meta = self.post(body)
            for u in meta["universe"]:
                merged[u["name"]] = MarketInfo(
                    u["name"], int(u["szDecimals"]), float(u.get("maxLeverage", 20)))
        self._meta = merged

    def info(self, name: str) -> MarketInfo:
        if not self._meta:
            self.refresh_meta()
        if name not in self._meta:
            raise KeyError(f"unknown market: {name}")
        return self._meta[name]

    def known(self, name: str) -> bool:
        if not self._meta:
            self.refresh_meta()
        return name in self._meta

    # -- ctxs --------------------------------------------------------------
    def _ctxs_for(self, dex: str) -> dict[str, Ctx]:
        body = {"type": "metaAndAssetCtxs"}
        if dex:
            body["dex"] = dex
        meta, ctxs = self.post(body)
        out: dict[str, Ctx] = {}
        for u, c in zip(meta["universe"], ctxs):
            try:
                mark = float(c["markPx"])
                prev = float(c.get("prevDayPx") or 0)
                oracle = float(c.get("oraclePx") or 0)
                impacts = c.get("impactPxs") or []
                spread_bps = 0.0
                if isinstance(impacts, list) and len(impacts) >= 2:
                    try:
                        bid, ask = float(impacts[0]), float(impacts[1])
                        if bid > 0 and ask > 0:
                            spread_bps = (ask - bid) / ((ask + bid) / 2) * 10_000
                    except (TypeError, ValueError):
                        spread_bps = 0.0
                out[u["name"]] = Ctx(
                    name=u["name"],
                    mark=mark,
                    day_pct=(mark / prev - 1) * 100 if prev else 0.0,
                    funding_apr_pct=float(c.get("funding") or 0) * 100 * 24 * 365,
                    oi_usd=float(c.get("openInterest") or 0) * mark,
                    vol_usd=float(c.get("dayNtlVlm") or 0),
                    spread_bps=spread_bps,
                    premium_pct=((mark / oracle - 1) * 100) if oracle > 0 else 0.0,
                )
            except (KeyError, TypeError, ValueError):
                continue
        return out

    def ctxs(self) -> dict[str, Ctx]:
        out = self._ctxs_for("")
        for dex in self.dexes:
            out.update(self._ctxs_for(dex))
        return out

    def mark(self, name: str) -> float:
        c = self.ctxs().get(name)
        if c is None:
            raise KeyError(f"no ctx for market: {name}")
        return c.mark

    # -- candles / features ------------------------------------------------
    # How long a candle series is worth reusing. 15m is the trading timeframe
    # and is never cached; the higher ones move far more slowly than the cycle
    # does, and without this each candidate costs three info calls per cycle
    # instead of one.
    _CANDLE_TTL = {"1h": 300.0, "1d": 3600.0}

    def candles(self, name: str, interval: str, lookback_ms: int) -> list[dict]:
        ttl = self._CANDLE_TTL.get(interval, 0.0)
        key = (name, interval)
        if ttl:
            hit = self._candle_cache.get(key)
            if hit is not None and time.time() - hit[0] < ttl:
                return hit[1]
        now = int(time.time() * 1000)
        res = self.post({"type": "candleSnapshot",
                         "req": {"coin": name, "interval": interval,
                                 "startTime": now - lookback_ms, "endTime": now}})
        out = res if isinstance(res, list) else []
        if ttl and out:
            # Written from the feature worker pool. A racing duplicate fetch
            # costs one extra call and stores the same thing, so no lock.
            self._candle_cache[key] = (time.time(), out)
        return out

    def candles_range(self, name: str, interval: str,
                      start_ms: int, end_ms: int) -> list[dict]:
        """Candles over an EXPLICIT window, chunked to the venue's limit.

        `candles()` always reads backwards from now, which is right for a live
        decision and wrong for replaying a trade that closed last week.
        candleSnapshot returns at most 5000 bars per request, and 7 days of 1m
        is 10,080, so a naive single call silently truncates the tail — the part
        of the window a replay cares about most."""
        span = {"1m": 60, "5m": 300, "15m": 900, "1h": 3600,
                "4h": 14400, "1d": 86400}.get(interval, 60) * 1000
        out: list[dict] = []
        seen: set[int] = set()
        cursor = int(start_ms)
        end_ms = int(end_ms)
        while cursor < end_ms:
            stop = min(end_ms, cursor + 4500 * span)
            res = self.post({"type": "candleSnapshot",
                             "req": {"coin": name, "interval": interval,
                                     "startTime": cursor, "endTime": stop}})
            batch = res if isinstance(res, list) else []
            if not batch:
                break
            for c in batch:
                t = int(c["t"])
                if t not in seen:
                    seen.add(t)
                    out.append(c)
            latest = max(int(c["t"]) for c in batch)
            if latest + span <= cursor:
                break            # the venue is not advancing; stop rather than spin
            cursor = latest + span
        out.sort(key=lambda c: int(c["t"]))
        return out

    def features(self, name: str, ctx: Ctx) -> dict:
        """The whole market picture the analyst gets for one candidate.

        Until 2026-09-07 this was a single 15m/24h call. The analyst was being
        asked to pick a swing target at least 4% away — the RR floor against a
        2% stop forces that — through a 24-hour keyhole, with no daily levels,
        no prior-day high/low and no multi-day structure to anchor it to. Of 27
        live trades exactly one target was ever reached. Higher timeframes are
        two extra reads per candidate on a 300-900s cycle."""
        cs = self.candles(name, "15m", 24 * 3600 * 1000)
        f = {"mark": ctx.mark, "day_pct": ctx.day_pct,
             "funding_apr_pct": ctx.funding_apr_pct,
             "oi_usd": ctx.oi_usd, "vol_usd": ctx.vol_usd,
             "spread_bps": ctx.spread_bps, "premium_pct": ctx.premium_pct}
        f.update(candle_features(cs))
        h1 = self.candles(name, "1h", 14 * 24 * 3600 * 1000)
        d1 = self.candles(name, "1d", 60 * 24 * 3600 * 1000)
        f.update(htf_features(h1, d1, ctx.mark))
        return f

    # -- candidate screening ----------------------------------------------
    def affordable(self, name: str, mark: float, max_notional: float) -> bool:
        """Can one venue lot of this market be bought for the money we risk?

        A szDecimals=1 name at $3,400 has a $340 minimum lot: structurally
        untradeable on a small account, yet it still reached the analyst as a
        candidate and burned a cycle on 'size rounds to zero'."""
        try:
            step = 10 ** -self.info(name).sz_decimals
        except Exception:  # noqa: BLE001 — unknown meta is not a reason to hide it
            return True
        return step * mark <= max_notional

    def candidates(self, native_allow: list[str], volume_floor: float, top_movers: int,
                   must_include: list[str]) -> list[str]:
        ctxs = self.ctxs()
        out: list[str] = []
        for n in native_allow:
            if n in ctxs:
                out.append(n)
        dex_prefixes = tuple(f"{dex}:" for dex in self.dexes)
        liquid = [c for c in ctxs.values()
                  if c.name.startswith(dex_prefixes) and c.vol_usd >= volume_floor]
        # Ranking purely by |day%| fed the analyst the most EXTENDED names on the
        # dex — precisely the ones the range-edge gate then refuses (no long
        # above 0.80 of the range, no short below 0.20). The screener was
        # arguing with the guard, and 08-28's losing day was seven chases.
        # Half the slots still go to movers, because a move is where a catalyst
        # shows up; the rest go to the most liquid names regardless of how far
        # they have travelled, so there is always something tradeable in range.
        movers = sorted(liquid, key=lambda c: abs(c.day_pct), reverse=True)
        by_volume = sorted(liquid, key=lambda c: c.vol_usd, reverse=True)
        picked: list[str] = []
        for name in _interleave([c.name for c in movers[:top_movers]],
                                [c.name for c in by_volume]):
            if name not in picked:
                picked.append(name)
            if len(picked) >= top_movers:
                break
        out.extend(picked)
        for m in must_include:
            if m in ctxs and m not in out:
                out.append(m)
        seen: set[str] = set()
        return [m for m in out if not (m in seen or seen.add(m))]


def candle_features(cs: list[dict]) -> dict:
    """Pure: 15m candles (asc) -> returns/ATR/range features. Empty input -> {}."""
    if not cs:
        return {}
    closes = [float(c["c"]) for c in cs]
    highs = [float(c["h"]) for c in cs]
    lows = [float(c["l"]) for c in cs]
    last = closes[-1]
    out: dict[str, float] = {}
    for label, bars in (("r_1h_pct", 4), ("r_4h_pct", 16)):
        if len(closes) > bars:
            out[label] = (last / closes[-1 - bars] - 1) * 100
    trs = []
    for i in range(1, len(cs)):
        trs.append(max(highs[i] - lows[i], abs(highs[i] - closes[i - 1]),
                       abs(lows[i] - closes[i - 1])))
    if trs:
        n = min(14, len(trs))
        out["atr15m_pct"] = sum(trs[-n:]) / n / last * 100
    hi, lo = max(highs), min(lows)
    if hi > lo:
        out["range24h_pos"] = (last - lo) / (hi - lo)  # 0 = at low, 1 = at high
    out["hi_24h"] = hi
    out["lo_24h"] = lo
    # Per-bar volume was fetched on every candle and thrown away. Whether the
    # last bar carries conviction or is drifting on nothing is the cheapest
    # confirmation there is.
    vols = []
    for c in cs:
        try:
            vols.append(float(c["v"]))
        except (KeyError, TypeError, ValueError):
            vols = []
            break
    if len(vols) >= 4:
        prior = vols[:-1]
        mean = sum(prior) / len(prior)
        if mean > 0:
            out["rvol"] = vols[-1] / mean
    return out


def _interleave(a: list, b: list) -> list:
    """a[0], b[0], a[1], b[1], ... — take from both without either starving."""
    out = []
    for i in range(max(len(a), len(b))):
        if i < len(a):
            out.append(a[i])
        if i < len(b):
            out.append(b[i])
    return out


def _atr_pct(cs: list[dict], last: float, period: int = 14) -> Optional[float]:
    """Average true range over `period` bars, as a % of `last`."""
    if len(cs) < 2 or last <= 0:
        return None
    highs = [float(c["h"]) for c in cs]
    lows = [float(c["l"]) for c in cs]
    closes = [float(c["c"]) for c in cs]
    trs = [max(highs[i] - lows[i], abs(highs[i] - closes[i - 1]),
               abs(lows[i] - closes[i - 1])) for i in range(1, len(cs))]
    if not trs:
        return None
    n = min(period, len(trs))
    return sum(trs[-n:]) / n / last * 100


def _range_pos(cs: list[dict], last: float, bars: int) -> Optional[float]:
    """Where `last` sits in the high/low range of the final `bars` candles."""
    window = cs[-bars:]
    if not window:
        return None
    hi = max(float(c["h"]) for c in window)
    lo = min(float(c["l"]) for c in window)
    if hi <= lo:
        return None
    return min(1.0, max(0.0, (last - lo) / (hi - lo)))


def _trend_pct(cs: list[dict], last: float, bars: int) -> Optional[float]:
    """How far `last` sits above/below the mean close of the last `bars`, in %.

    A deliberately blunt trend read: the analyst does not need an indicator
    suite, it needs to know whether it is buying into strength or into a
    falling market, which nothing in the 15m/24h window could tell it."""
    window = [float(c["c"]) for c in cs[-bars:]]
    if len(window) < max(2, bars // 2) or last <= 0:
        return None
    mean = sum(window) / len(window)
    if mean <= 0:
        return None
    return (last / mean - 1) * 100


def htf_features(h1: list[dict], d1: list[dict], last: float) -> dict:
    """Structure above 15m: where this price sits in the multi-day picture.

    Every number here answers a question the 24h window could not: is the
    target I am about to set inside the range this market actually trades, is
    price extended against its own multi-day mean, and how far is the nearest
    level that has mattered recently."""
    out: dict[str, float] = {}
    if last <= 0:
        return out
    if h1:
        for key, value in (("atr1h_pct", _atr_pct(h1, last)),
                           ("trend_1h_pct", _trend_pct(h1, last, 24))):
            if value is not None:
                out[key] = value
    if d1:
        for key, value in (("atr1d_pct", _atr_pct(d1, last)),
                           ("range5d_pos", _range_pos(d1, last, 5)),
                           ("range20d_pos", _range_pos(d1, last, 20)),
                           ("trend_20d_pct", _trend_pct(d1, last, 20))):
            if value is not None:
                out[key] = value
        # The previous session's extremes: the levels a discretionary trader
        # reaches for first and the only ones the 24h window cannot express.
        if len(d1) >= 2:
            out["prev_day_hi"] = float(d1[-2]["h"])
            out["prev_day_lo"] = float(d1[-2]["l"])
        window = d1[-20:]
        if window:
            hi20 = max(float(c["h"]) for c in window)
            lo20 = min(float(c["l"]) for c in window)
            out["hi_20d"] = hi20
            out["lo_20d"] = lo20
            # Headroom to the nearest multi-day level, so a target can be
            # judged against structure instead of against the RR floor alone.
            out["to_hi_20d_pct"] = (hi20 / last - 1) * 100
            out["to_lo_20d_pct"] = (1 - lo20 / last) * 100
    return out


def expected_move_pct(atr15m_pct: Optional[float], hours: float) -> Optional[float]:
    """How far this market plausibly travels in `hours`, from its own ATR15m.

    Range scales with the square root of time, so 3h is ~sqrt(12) 15m bars.
    This exists to be rendered beside the target: the RR floor forces targets
    around 8-9x ATR15m while the time stop closes at 3h, and nothing in the
    prompt let the analyst notice those two numbers disagree."""
    if not atr15m_pct or atr15m_pct <= 0 or hours <= 0:
        return None
    return atr15m_pct * math.sqrt(hours * 4.0)


def close_fee_rate() -> float:
    """One taker side: HL 4.5bp + trench builder 3bp (verified 2026-08-29)."""
    return TAKER_FEE_RATE
