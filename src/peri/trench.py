"""Trench's own market-bias and economic-calendar feeds.

Two public endpoints the Trench app uses, both reachable server-side without
auth (the sentiment one is a POST — a GET 404s, which is why it looks private):

  POST https://whale.trench.ag/api/v1/hyperliquid/market-sentiment?timeframe=all
  GET  https://app.trench.ag/api/economic-calendar

The sentiment feed buckets every Hyperliquid trader by realised PnL and reports
how each cohort is positioned, overall and in its top markets. That is the one
thing peri cannot derive from price: what the wallets that actually make money
are doing, versus the crowd. A divergence between the two is the signal —
agreement is just consensus.

Neither feed is load-bearing. A failure is reported and the cycle continues on
price and news alone.
"""

import json
import urllib.request
from typing import Callable, Optional

SENTIMENT_URL = ("https://whale.trench.ag/api/v1/hyperliquid/"
                 "market-sentiment?timeframe=all")
CALENDAR_URL = "https://app.trench.ag/api/economic-calendar"

# Cohorts ordered from the wallets that make the most money to the ones that
# lose it. "Smart" and "crowd" below are read off the ends of this list.
COHORT_ORDER = ("extremely_profitable", "very_profitable", "profitable",
                "unprofitable", "very_unprofitable", "rekt")
SMART = ("extremely_profitable", "very_profitable")
CROWD = ("unprofitable", "very_unprofitable", "rekt")


def _json_request(url: str, method: str = "GET", timeout: float = 20) -> object:
    data = b"{}" if method == "POST" else None
    req = urllib.request.Request(
        url, data=data, method=method,
        headers={"content-type": "application/json", "accept": "application/json",
                 "user-agent": "peri/1.0"})
    return json.loads(urllib.request.urlopen(req, timeout=timeout).read())


def _long_pct(long_notional: float, short_notional: float) -> Optional[float]:
    total = long_notional + short_notional
    return (long_notional / total * 100.0) if total > 0 else None


def fetch_cohort_bias(request: Callable[..., object] = _json_request) -> dict:
    """Cohort positioning, plus the per-asset split that matters.

    Returns {"cohorts": [...], "by_asset": {ticker: {...}}, "total_traders": n}.
    Raises on transport or shape failure — the caller decides whether a missing
    feed is fatal (it is not).
    """
    body = request(SENTIMENT_URL, "POST")
    if not isinstance(body, dict) or not body.get("success"):
        raise RuntimeError(f"trench sentiment rejected the request: {body!r}")
    data = body.get("data") or {}
    raw = data.get("pnlCohorts")
    if not isinstance(raw, list) or not raw:
        raise RuntimeError("trench sentiment returned no cohorts")

    cohorts, by_asset = [], {}
    for row in raw:
        cid = row.get("id")
        long_usd = float(row.get("longNotional") or 0)
        short_usd = float(row.get("shortNotional") or 0)
        cohorts.append({
            "id": cid,
            "label": row.get("label") or cid,
            "range": row.get("range") or "",
            "traders": int(row.get("totalTraders") or 0),
            "long_pct": _long_pct(long_usd, short_usd),
            "notional": long_usd + short_usd,
            "sentiment": row.get("sentiment") or "?",
        })
        for market in row.get("topMarkets") or []:
            ticker = market.get("ticker")
            if not ticker:
                continue
            m_long = float(market.get("longNotional") or 0)
            m_short = float(market.get("shortNotional") or 0)
            entry = by_asset.setdefault(ticker, {"cohorts": {}, "notional": 0.0})
            entry["cohorts"][cid] = _long_pct(m_long, m_short)
            entry["notional"] += m_long + m_short

    # The readable part: where the money-makers and the crowd disagree.
    for ticker, entry in by_asset.items():
        smart = [v for k, v in entry["cohorts"].items() if k in SMART and v is not None]
        crowd = [v for k, v in entry["cohorts"].items() if k in CROWD and v is not None]
        entry["smart_long_pct"] = sum(smart) / len(smart) if smart else None
        entry["crowd_long_pct"] = sum(crowd) / len(crowd) if crowd else None
        entry["divergence"] = (
            entry["smart_long_pct"] - entry["crowd_long_pct"]
            if entry["smart_long_pct"] is not None and entry["crowd_long_pct"] is not None
            else None)

    order = {cid: i for i, cid in enumerate(COHORT_ORDER)}
    cohorts.sort(key=lambda c: order.get(c["id"], 99))
    return {"cohorts": cohorts, "by_asset": by_asset,
            "total_traders": int(data.get("totalTraders") or 0),
            "computed_ts": (body.get("computedAt") or 0) / 1000.0}


ASSET_URL = ("https://whale.trench.ag/api/v1/hyperliquid/"
             "market-sentiment/asset?coin={coin}&timeframe=all")


def fetch_asset_bias(coin: str,
                     request: Callable[..., object] = _json_request) -> Optional[dict]:
    """How each PnL cohort is positioned in ONE market — including xyz names.

    This is the per-asset half: the overall feed only carries each cohort's top
    three markets, which is BTC/ETH/HYPE and nothing else.
    """
    import urllib.parse
    body = request(ASSET_URL.format(coin=urllib.parse.quote(coin)), "POST")
    if not isinstance(body, dict) or not body.get("success"):
        return None
    data = body.get("data") or {}
    split = data.get("cohortSplit")
    if not isinstance(split, list) or not split:
        return None
    smart, crowd = [], []
    for row in split:
        share = row.get("longShare")
        if not isinstance(share, (int, float)):
            continue
        if row.get("id") in SMART:
            smart.append(share * 100.0)
        elif row.get("id") in CROWD:
            crowd.append(share * 100.0)
    smart_pct = sum(smart) / len(smart) if smart else None
    crowd_pct = sum(crowd) / len(crowd) if crowd else None
    # NB the payload's own longShare/sentiment come back as a flat 50%/"Neutral"
    # for every market — placeholders, not signal. The cohort split is the data.
    return {
        "coin": data.get("coin") or coin,
        "long_traders": data.get("longTraders"),
        "short_traders": data.get("shortTraders"),
        "notional": float(data.get("longNotional") or 0) + float(data.get("shortNotional") or 0),
        "smart_long_pct": smart_pct,
        "crowd_long_pct": crowd_pct,
        "divergence": (smart_pct - crowd_pct)
                      if smart_pct is not None and crowd_pct is not None else None,
        "cohorts": [{"id": r.get("id"), "label": r.get("label"),
                     "long_pct": (r.get("longShare") or 0) * 100.0,
                     "sentiment": r.get("sentiment")} for r in split],
    }


def fetch_many_asset_bias(coins, request: Callable[..., object] = _json_request,
                          workers: int = 8) -> dict[str, dict]:
    """One request per market, concurrently. A market that fails is simply
    absent — the analyst is told what it has, never a guess."""
    from concurrent.futures import ThreadPoolExecutor
    coins = list(dict.fromkeys(coins))
    if not coins:
        return {}

    def one(coin: str):
        try:
            return coin, fetch_asset_bias(coin, request)
        except Exception:  # noqa: BLE001 — one dead market is not a dead cycle
            return coin, None

    with ThreadPoolExecutor(max_workers=min(workers, len(coins))) as pool:
        return {c: b for c, b in pool.map(one, coins) if b}


def fetch_economic_calendar(request: Callable[..., object] = _json_request,
                            countries: tuple[str, ...] = ("USD",),
                            impacts: tuple[str, ...] = ("High", "Medium")) -> list[dict]:
    """Dated macro events, filtered to what actually moves this book.

    The raw feed carries every country; an NZD rate decision is noise for a
    crypto and US-equity book and would crowd out the print that matters.
    """
    rows = request(CALENDAR_URL, "GET")
    if not isinstance(rows, list):
        raise RuntimeError(f"trench calendar returned {type(rows).__name__}, not a list")
    out = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        if row.get("impact") not in impacts or row.get("country") not in countries:
            continue
        stamp = row.get("date")
        title = row.get("title")
        if not (isinstance(stamp, str) and isinstance(title, str) and title.strip()):
            continue
        try:
            from datetime import datetime
            ts = datetime.fromisoformat(stamp).timestamp()
        except ValueError:
            continue
        detail = ""
        forecast, previous = row.get("forecast"), row.get("previous")
        if forecast or previous:
            detail = f" (fc {forecast or '-'} vs prev {previous or '-'})"
        out.append({"ts": ts,
                    "title": f"{row['country']} {title.strip()}{detail}",
                    "impact": str(row["impact"]).lower(),
                    "scope": row.get("country")})
    out.sort(key=lambda e: e["ts"])
    return out
