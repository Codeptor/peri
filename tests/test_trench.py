"""Trench's cohort bias and economic calendar. No network: the transport is
injected, so these assert the parsing contract, not the vendor's uptime."""

import time

import pytest

from peri.trench import (fetch_asset_bias, fetch_cohort_bias,
                         fetch_economic_calendar, fetch_many_asset_bias)


def cohort(cid, long_usd, short_usd, traders=100, tops=()):
    return {"id": cid, "label": cid.replace("_", " ").title(), "range": "+$1M+",
            "totalTraders": traders, "longNotional": long_usd,
            "shortNotional": short_usd, "sentiment": "Neutral",
            "topMarkets": [{"ticker": t, "longNotional": lo, "shortNotional": sh}
                           for t, lo, sh in tops]}


def sentiment_body():
    return {"success": True, "computedAt": 1788151605663, "data": {
        "totalTraders": 121854,
        "pnlCohorts": [
            cohort("rekt", 10, 90, tops=[("HYPE", 10, 90)]),
            cohort("extremely_profitable", 80, 20, tops=[("HYPE", 80, 20)]),
            cohort("very_profitable", 70, 30, tops=[("HYPE", 70, 30)]),
            cohort("unprofitable", 40, 60, tops=[("HYPE", 40, 60)]),
            cohort("very_unprofitable", 30, 70, tops=[("HYPE", 30, 70)]),
            cohort("profitable", 60, 40, tops=[("HYPE", 60, 40)]),
        ]}}


def test_cohorts_come_back_ordered_from_winners_to_losers():
    body = sentiment_body()
    out = fetch_cohort_bias(lambda url, method="GET": body)
    assert [c["id"] for c in out["cohorts"]] == [
        "extremely_profitable", "very_profitable", "profitable",
        "unprofitable", "very_unprofitable", "rekt"]
    assert out["total_traders"] == 121854


def test_the_divergence_is_smart_money_minus_the_crowd():
    body = sentiment_body()
    out = fetch_cohort_bias(lambda url, method="GET": body)
    hype = out["by_asset"]["HYPE"]
    assert hype["smart_long_pct"] == pytest.approx(75.0)      # 80 and 70
    assert hype["crowd_long_pct"] == pytest.approx((40 + 30 + 10) / 3)
    assert hype["divergence"] == pytest.approx(75.0 - 80 / 3)


def test_a_rejected_or_empty_sentiment_body_raises():
    with pytest.raises(RuntimeError):
        fetch_cohort_bias(lambda url, method="GET": {"success": False})
    with pytest.raises(RuntimeError):
        fetch_cohort_bias(lambda url, method="GET": {"success": True, "data": {}})


def asset_body(shares):
    return {"success": True, "data": {
        "coin": "HYPE", "longShare": 0.4996, "sentiment": "Neutral",
        "longTraders": 13192, "shortTraders": 4741,
        "longNotional": 1e9, "shortNotional": 1e9,
        "cohortSplit": [{"id": cid, "label": cid, "longShare": share,
                         "longNotional": 1, "shortNotional": 1, "sentiment": "x"}
                        for cid, share in shares.items()]}}


def test_per_asset_bias_reads_the_cohort_split_not_the_placeholder():
    body = asset_body({"extremely_profitable": 0.67, "very_profitable": 0.80,
                       "profitable": 0.79, "unprofitable": 0.54,
                       "very_unprofitable": 0.32, "rekt": 0.11})
    out = fetch_asset_bias("HYPE", lambda url, method="GET": body)
    assert out["smart_long_pct"] == pytest.approx(73.5)
    assert out["crowd_long_pct"] == pytest.approx((54 + 32 + 11) / 3)
    assert out["divergence"] > 40
    # the payload's own longShare/sentiment are a flat placeholder and must not
    # be surfaced as if they were signal
    assert "long_share_pct" not in out and "sentiment" not in out
    assert out["long_traders"] == 13192


def test_a_market_with_no_split_yields_nothing_rather_than_a_guess():
    assert fetch_asset_bias("X", lambda url, method="GET": {"success": False}) is None
    assert fetch_asset_bias("X", lambda url, method="GET":
                            {"success": True, "data": {}}) is None


def test_one_dead_market_does_not_kill_the_batch():
    body = asset_body({"extremely_profitable": 0.6, "rekt": 0.2})

    def flaky(url, method="GET"):
        if "BAD" in url:
            raise RuntimeError("upstream 500")
        return body

    out = fetch_many_asset_bias(["GOOD", "BAD", "GOOD"], flaky)
    assert set(out) == {"GOOD"}


def test_the_calendar_filters_to_what_moves_this_book():
    rows = [
        {"title": "Non-Farm Employment Change", "country": "USD",
         "date": "2026-09-04T08:30:00-04:00", "impact": "High",
         "forecast": "58K", "previous": "-23K"},
        {"title": "Official Cash Rate", "country": "NZD",
         "date": "2026-09-01T22:00:00-04:00", "impact": "High",
         "forecast": "2.75%", "previous": "2.50%"},
        {"title": "Bank Holiday", "country": "GBP",
         "date": "2026-08-31T03:00:00-04:00", "impact": "Holiday",
         "forecast": "", "previous": ""},
        {"title": "ISM Services PMI", "country": "USD",
         "date": "2026-09-03T10:00:00-04:00", "impact": "Medium",
         "forecast": "54.1", "previous": "54.1"},
    ]
    out = fetch_economic_calendar(lambda url, method="GET": rows)
    assert [e["title"] for e in out] == [
        "USD ISM Services PMI (fc 54.1 vs prev 54.1)",
        "USD Non-Farm Employment Change (fc 58K vs prev -23K)",
    ]
    assert out[0]["ts"] < out[1]["ts"]              # sorted by time
    assert out[1]["impact"] == "high"               # normalised for the gate
    assert time.gmtime(out[1]["ts"]).tm_hour == 12  # 08:30 ET -> 12:30 UTC


def test_a_malformed_calendar_row_is_skipped_not_fatal():
    rows = [
        {"title": "", "country": "USD", "date": "2026-09-04T08:30:00-04:00",
         "impact": "High"},
        {"title": "Good", "country": "USD", "date": "not-a-date", "impact": "High"},
        {"title": "Real", "country": "USD", "date": "2026-09-04T08:30:00-04:00",
         "impact": "High", "forecast": "", "previous": ""},
    ]
    out = fetch_economic_calendar(lambda url, method="GET": rows)
    assert len(out) == 1 and out[0]["title"] == "USD Real"
    with pytest.raises(RuntimeError):
        fetch_economic_calendar(lambda url, method="GET": {"not": "a list"})
