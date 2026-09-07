"""Dex-aware read-only data layer over the Hyperliquid info API.

One Market instance serves both universes: native perps (crypto majors) and the
builder dex (equities + commodities, e.g. "xyz:NVDA"). Everything is keyed by the
full market name; the dex is inferred from the prefix. `post` is injected so
tests never touch the network.
"""

import time
from dataclasses import dataclass
from typing import Callable

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


class Market:
    def __init__(self, post: Callable[[dict], object], dexes):
        self.post = post
        self.dexes = tuple(dexes)
        self._meta: dict[str, MarketInfo] = {}

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
                out[u["name"]] = Ctx(
                    name=u["name"],
                    mark=mark,
                    day_pct=(mark / prev - 1) * 100 if prev else 0.0,
                    funding_apr_pct=float(c.get("funding") or 0) * 100 * 24 * 365,
                    oi_usd=float(c.get("openInterest") or 0) * mark,
                    vol_usd=float(c.get("dayNtlVlm") or 0),
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
    def candles(self, name: str, interval: str, lookback_ms: int) -> list[dict]:
        now = int(time.time() * 1000)
        res = self.post({"type": "candleSnapshot",
                         "req": {"coin": name, "interval": interval,
                                 "startTime": now - lookback_ms, "endTime": now}})
        return res if isinstance(res, list) else []

    def features(self, name: str, ctx: Ctx) -> dict:
        cs = self.candles(name, "15m", 24 * 3600 * 1000)
        f = {"mark": ctx.mark, "day_pct": ctx.day_pct,
             "funding_apr_pct": ctx.funding_apr_pct,
             "oi_usd": ctx.oi_usd, "vol_usd": ctx.vol_usd}
        f.update(candle_features(cs))
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
        movers = sorted(
            (c for c in ctxs.values()
             if c.name.startswith(dex_prefixes) and c.vol_usd >= volume_floor),
            key=lambda c: abs(c.day_pct), reverse=True)
        out.extend(c.name for c in movers[:top_movers])
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
    return out


def close_fee_rate() -> float:
    """One taker side: HL 4.5bp + trench builder 3bp (verified 2026-08-29)."""
    return TAKER_FEE_RATE
