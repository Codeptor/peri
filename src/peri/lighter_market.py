"""Read-only data layer over Lighter, shaped like peri.market.Market.

The engine only ever touches a market object through a small surface —
refresh_meta/info/known/ctxs/mark/candles/features/affordable/candidates —
so this class implements exactly that surface against Lighter's REST API and
the engine never learns which venue it is reading. Candle math is shared:
candle_features is imported, not reimplemented.

Two deliberate v1 gaps, both documented rather than papered over:

- Funding. Lighter exposes no per-market CURRENT funding rate over the API
  (FundingApi returns reference-CEX rates, not what a Lighter position pays),
  so Ctx.funding_apr_pct reads 0.0 here. The analyst is told as much wherever
  this layer is wired; a zero here means "unavailable", not "free".
- Sessions. Lighter names carry no dex prefix, and the guard's session gate
  only constrains colon-prefixed builder-dex names, so Lighter equities trade
  without a home-hours check. Crypto is 24/7 and unaffected.
"""

import json
import os
import time
import urllib.request
from dataclasses import dataclass

from peri.market import Ctx, MarketInfo, candle_features

MAINNET_HOST = "https://mainnet.zklighter.elliot.ai"
TESTNET_HOST = "https://testnet.zklighter.elliot.ai"

# HL name -> Lighter symbol, ONLY where the mapping is certain. Suffix-stripping
# ("xyz:NVDA" -> "NVDA") covers everything else. The ISO currency codes XAU/XAG/
# XPT/XPD/XCU ARE gold/silver/platinum/palladium/copper by definition, and CL is
# the WTI futures ticker. Anything ambiguous (xyz:SKHX, xyz:SP500, xyz:JPY with
# its inverted convention) stays unmapped: info() raises KeyError, the same
# contract as an unknown HL market, and the analyst never sees it.
ALIASES = {
    "io:ANTH": "ANTHROPIC",
    "io:OAI": "OPENAI",
    "xyz:GOLD": "XAU",
    "xyz:SILVER": "XAG",
    "xyz:PLATINUM": "XPT",
    "xyz:PALLADIUM": "XPD",
    "xyz:COPPER": "XCU",
    "xyz:CL": "WTI",
    "xyz:EUR": "EURUSD",
    "xyz:GBP": "GBPUSD",
    "xyz:HYUNDAI": "HYUNDAIUSD",
}


def resolve(name: str) -> str:
    """A peri market name to its Lighter symbol. No guessing: unknown stays
    unknown and the caller gets KeyError from info()."""
    if name in ALIASES:
        return ALIASES[name]
    return name.split(":")[-1]


@dataclass
class _Meta:
    market_id: int
    price_decimals: int
    size_decimals: int
    max_leverage: float
    min_base: float
    min_quote: float


class LighterMarket:
    def __init__(self, host: str = MAINNET_HOST, timeout: int = 25):
        self.host = host.rstrip("/")
        self.timeout = timeout
        self._meta: dict[str, _Meta] = {}
        self._last = 0.0

    # -- transport -------------------------------------------------------
    def _get(self, path: str):
        gap = float(os.environ.get("LIGHTER_MIN_GAP", "0.30"))
        wait = gap - (time.monotonic() - self._last)
        if wait > 0:
            time.sleep(wait)
        self._last = time.monotonic()
        with urllib.request.urlopen(f"{self.host}/api/v1/{path}",
                                    timeout=self.timeout) as r:
            return json.loads(r.read())

    # -- meta ------------------------------------------------------------
    def refresh_meta(self) -> None:
        books = self._get("orderBooks")["order_books"]
        details = {d["market_id"]: d
                   for d in self._get("orderBookDetails")["order_book_details"]}
        meta: dict[str, _Meta] = {}
        for b in books:
            if b.get("status") != "active":
                continue
            d = details.get(b["market_id"], {})
            try:
                min_margin = float(d.get("min_initial_margin_fraction") or 0)
                meta[b["symbol"]] = _Meta(
                    market_id=int(b["market_id"]),
                    price_decimals=int(b["supported_price_decimals"]),
                    size_decimals=int(b["supported_size_decimals"]),
                    max_leverage=(10000.0 / min_margin if min_margin > 0 else 1.0),
                    min_base=float(b.get("min_base_amount") or 0),
                    min_quote=float(b.get("min_quote_amount") or 0),
                )
            except (TypeError, ValueError):
                continue
        self._meta = meta

    def market_id(self, symbol: str) -> int:
        if not self._meta:
            self.refresh_meta()
        return self._meta[symbol].market_id

    def symbols_by_id(self) -> dict[int, str]:
        if not self._meta:
            self.refresh_meta()
        return {m.market_id: s for s, m in self._meta.items()}

    def _dec(self, symbol: str) -> _Meta:
        if not self._meta:
            self.refresh_meta()
        if symbol not in self._meta:
            raise KeyError(f"unknown market: {symbol}")
        return self._meta[symbol]

    def px_int(self, name: str, price: float) -> int:
        """Price as the venue wants it: a scaled integer at this market's tick."""
        m = self._dec(resolve(name))
        return int(round(round(price, m.price_decimals) * 10 ** m.price_decimals))

    def sz_int(self, name: str, size: float) -> int:
        """Size FLOORED to the venue lot. Never round up into more risk."""
        m = self._dec(resolve(name))
        return int(size * 10 ** m.size_decimals)

    def round_px(self, name: str, price: float) -> float:
        return round(price, self._dec(resolve(name)).price_decimals)

    def info(self, name: str) -> MarketInfo:
        if not self._meta:
            self.refresh_meta()
        symbol = resolve(name)
        if symbol not in self._meta:
            raise KeyError(f"unknown market: {name}")
        m = self._meta[symbol]
        return MarketInfo(name, m.size_decimals, m.max_leverage)

    def known(self, name: str) -> bool:
        if not self._meta:
            self.refresh_meta()
        return resolve(name) in self._meta

    # -- ctxs ------------------------------------------------------------
    def ctxs(self) -> dict[str, Ctx]:
        details = {d["market_id"]: d
                   for d in self._get("orderBookDetails")["order_book_details"]}
        if not self._meta:
            self.refresh_meta()
        by_id = {m.market_id: s for s, m in self._meta.items()}
        out: dict[str, Ctx] = {}
        for mid, d in details.items():
            symbol = by_id.get(mid)
            if symbol is None:
                continue
            try:
                mark = float(d.get("mark_price") or d.get("last_trade_price") or 0)
                if mark <= 0:
                    continue
                out[symbol] = Ctx(
                    name=symbol,
                    mark=mark,
                    day_pct=float(d.get("daily_price_change") or 0),
                    funding_apr_pct=0.0,   # unavailable over the API; see module doc
                    oi_usd=float(d.get("open_interest") or 0) * mark,
                    vol_usd=float(d.get("daily_quote_token_volume") or 0),
                )
            except (TypeError, ValueError):
                continue
        return out

    def mark(self, name: str) -> float:
        c = self.ctxs().get(resolve(name))
        if c is None:
            raise KeyError(f"no ctx for market: {name}")
        return c.mark

    # -- candles / features ----------------------------------------------
    def candles(self, name: str, interval: str, lookback_ms: int) -> list[dict]:
        mid = self.market_id(resolve(name))
        now = int(time.time())
        res = self._get(
            f"candles?market_id={mid}&resolution={interval}"
            f"&start_timestamp={now - lookback_ms // 1000}"
            f"&end_timestamp={now}&count_back=200")
        cs = res.get("c") if isinstance(res, dict) else None
        return cs if isinstance(cs, list) else []

    def features(self, name: str, ctx: Ctx) -> dict:
        cs = self.candles(name, "15m", 24 * 3600 * 1000)
        f = {"mark": ctx.mark, "day_pct": ctx.day_pct,
             "funding_apr_pct": ctx.funding_apr_pct,
             "oi_usd": ctx.oi_usd, "vol_usd": ctx.vol_usd}
        f.update(candle_features(cs))
        return f

    # -- candidate screening ----------------------------------------------
    def affordable(self, name: str, mark: float, max_notional: float) -> bool:
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
            if resolve(n) in ctxs:
                out.append(resolve(n))
        movers = sorted(
            (c for c in ctxs.values() if c.vol_usd >= volume_floor),
            key=lambda c: abs(c.day_pct), reverse=True)
        out.extend(c.name for c in movers[:top_movers])
        for m in must_include:
            symbol = resolve(m)
            if symbol in ctxs and symbol not in out:
                out.append(symbol)
        seen: set[str] = set()
        return [m for m in out if not (m in seen or seen.add(m))]
