# backtest stats — stats-2026-08-06_2026-08-09

**2026-08-06 00:00 .. 2026-08-09 21:49 UTC** · 5373 bars of 5630 requested minutes · 271 markets

## What this is not

- distribution report only — no gates, no sizing decisions, no fills, no pnl: this is what the cached price/funding tape looks like, not a strategy replay
- Hyperliquid retains only ~3.6 days of 1m candles — a window older than that is unobtainable, ever. Requested 5630 minute(s) across 271 market(s); 5373 of them actually had cached tape (the rest predates the venue's retention or the cache has not been backfilled that far back).
- day_ntl_vlm is a proxy — rolling 24h candle quote volume, not the venue's own dayNtlVlm (spec Decision 4) — and it is what avg_day_ntl_vlm below ranks markets by
- features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)
- score is screener::screen run with an open floor and top_k so every vlm-eligible, feature-warmed market-minute is pooled — no gate, cooldown or exclusion is applied, so this is signal DENSITY, not what the strategy would actually enter
- min_score is read from the config file (--config, default kestreld.toml) — this subcommand takes no --set overrides, so it always reports the config as-is

## vol1h% percentiles

| series | n | p50 | p75 | p90 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|
| BTC (regime gate input) | 5364 | 0.0179 | 0.0277 | 0.0407 | 0.0509 | 0.0695 | 0.0794 |
| whole universe, pooled | 1359506 | 0.0575 | 0.0905 | 0.1466 | 0.2122 | 0.4667 | 4.3956 |

## Per-market vol1h%, top 15 by volume proxy

| market | avg day_ntl_vlm | n | p50 | p75 | p90 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| BTC | 775064259 | 5364 | 0.0179 | 0.0277 | 0.0407 | 0.0509 | 0.0695 | 0.0794 |
| ETH | 509593095 | 5123 | 0.0229 | 0.0363 | 0.0518 | 0.0684 | 0.0884 | 0.0934 |
| xyz:SPCX | 319842167 | 5182 | 0.0710 | 0.1246 | 0.2539 | 0.3951 | 0.5086 | 0.5386 |
| xyz:SKHX | 287564433 | 5312 | 0.0538 | 0.1158 | 0.1648 | 0.2316 | 0.2902 | 0.3245 |
| xyz:SNDK | 216302522 | 5326 | 0.0428 | 0.1259 | 0.2134 | 0.2739 | 0.4791 | 0.5721 |
| xyz:SP500 | 194055761 | 5240 | 0.0118 | 0.0163 | 0.0218 | 0.0261 | 0.0344 | 0.0414 |
| xyz:MU | 175354637 | 5329 | 0.0325 | 0.1035 | 0.1429 | 0.1954 | 0.3381 | 0.3839 |
| HYPE | 165854098 | 5223 | 0.0529 | 0.0666 | 0.0917 | 0.1034 | 0.1395 | 0.1649 |
| xyz:CL | 162798516 | 5295 | 0.0490 | 0.0738 | 0.0880 | 0.1019 | 0.1519 | 0.2050 |
| xyz:XYZ100 | 159017437 | 5125 | 0.0148 | 0.0239 | 0.0367 | 0.0515 | 0.0702 | 0.0851 |
| xyz:BRENTOIL | 96145978 | 5247 | 0.0478 | 0.0714 | 0.0829 | 0.0942 | 0.1435 | 0.1620 |
| SOL | 89148556 | 5113 | 0.0346 | 0.0450 | 0.0550 | 0.0635 | 0.0811 | 0.0885 |
| xyz:GOLD | 75139785 | 5118 | 0.0109 | 0.0273 | 0.0391 | 0.0497 | 0.1362 | 0.1423 |
| xyz:INTC | 71660093 | 3833 | 0.0529 | 0.0946 | 0.1358 | 0.2473 | 0.3473 | 0.4192 |
| xyz:SKHY | 67424843 | 5128 | 0.0420 | 0.1038 | 0.1702 | 0.2322 | 0.3205 | 0.4053 |

## funding_z distribution

market-minutes n = 1360861

| \|z\| threshold | share of market-minutes |
|---|---:|
| > 1.0 | 19.51% |
| > 2.0 | 7.44% |
| > 2.5 (screener's side-flip threshold) | 5.12% |

## screener score distribution

pooled market-minutes n = 429348, current `min_score` = 1.8

share of market-minutes scoring >= min_score: **45.26%**

| n | p50 | p75 | p90 | p95 | p99 | max |
|---:|---:|---:|---:|---:|---:|---:|
| 429348 | 1.645 | 2.812 | 4.712 | 6.703 | 14.442 | 32.221 |

## turnover / fee reference

`sizing::size_position` at this window's measured median vol1h (0.0575%):

| conviction | leverage | margin | notional | round-trip fee $ | round-trip fee bp |
|---:|---:|---:|---:|---:|---:|
| 0.75 | 20.0 | 40.00 | 800.00 | 1.20 | 15.00 |
| 1 | 20.0 | 50.00 | 1000.00 | 1.50 | 15.00 |

round-trip fee in bp is independent of notional (fee is linear in it), so it is the same at every conviction — confirms it here rather than asserting it from outside.

