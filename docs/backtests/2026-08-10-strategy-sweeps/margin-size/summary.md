# backtest sweep — margin-size

**2026-08-06 00:00 .. 2026-08-09 22:04 UTC** · 5373 bars · 271 markets · 16 runs

Axes:

- `margin` = 20, 40, 80, 160
- `risk.daily_cap` = 40, 20, 10, 5

## What this is not

- mechanical replay — no analyst judgment, no news: conviction is fixed at 0.75 for every entry and the screener's side_hint is taken as the side (spec Decision 2)
- no review loop: no veto closes, no stop moves, no analyst time-stop — brackets and the horizon are the only exits (spec Decision 5)
- exits: SL is checked before TP inside a candle (a candle touching both is scored as the stop) and fills AT the trigger; the 24h horizon fills at the candle close (spec Decision 5)
- entries fill at the next 1m open plus flat slip (2bp native / 5bp dex); 7.5bp taker fee per side (spec Decision 5)
- day_ntl_vlm is proxied by rolling 24h candle quote volume — the venue does not serve historical dayNtlVlm, and this proxy decides WHICH markets clear the universe filter (spec Decision 4)
- features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)
- open_interest is 0.0 throughout — historical OI is not served (nothing in the screener, sizing or the gates reads it)
- the screener ticks every 1m (live: 45s) — the closest candle-aligned cadence (spec Decision 6)
- margin axis: pins sizing.margin_min AND sizing.margin_max to the swept value (clamp(x,v,v) = v), so every position's margin is exactly v dollars regardless of conviction — pair it with a daily_cap/max_concurrent sweep (moved the other way) to compare fewer-larger against more-smaller positions at a similar aggregate notional. Name sizing.margin_min/margin_max yourself to leave that end of the clamp alone.

## Ranked — by net, ties to the shallower drawdown

| # | combo | net | maxDD | maxDD% | entries | closes | win% | expectancy | payoff | tp | sl | time_stop | open | equity end | return% |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | `margin=20 risk.daily_cap=20` | -35.65 | 51.49 | 5.13% | 80 | 77 | 35.1% | -0.45 | 1.55 | 25 | 48 | 4 | 3 | 971.45 | -2.86% |
| 2 | `margin=20 risk.daily_cap=5` | -47.98 | 51.73 | 5.15% | 20 | 20 | 20.0% | -2.40 | 1.20 | 3 | 15 | 2 | 0 | 952.02 | -4.80% |
| 3 | `margin=20 risk.daily_cap=40` | -56.39 | 59.84 | 5.96% | 134 | 129 | 35.7% | -0.43 | 1.52 | 44 | 81 | 4 | 5 | 947.38 | -5.26% |
| 4 | `margin=40 risk.daily_cap=20` | -71.30 | 102.98 | 10.22% | 80 | 77 | 35.1% | -0.90 | 1.55 | 25 | 48 | 4 | 3 | 942.90 | -5.71% |
| 5 | `margin=20 risk.daily_cap=10` | -74.08 | 82.08 | 8.18% | 40 | 38 | 23.7% | -1.93 | 1.33 | 7 | 28 | 3 | 2 | 928.52 | -7.15% |
| 6 | `margin=40 risk.daily_cap=5` | -95.96 | 103.46 | 10.27% | 20 | 20 | 20.0% | -4.80 | 1.20 | 3 | 15 | 2 | 0 | 904.04 | -9.60% |
| 7 | `margin=80 risk.daily_cap=20` | -105.21 | 168.58 | 16.61% | 75 | 72 | 36.1% | -1.41 | 1.54 | 24 | 44 | 4 | 3 | 923.18 | -7.68% |
| 8 | `margin=40 risk.daily_cap=40` | -112.78 | 119.67 | 11.88% | 134 | 129 | 35.7% | -0.85 | 1.52 | 44 | 81 | 4 | 5 | 894.76 | -10.52% |
| 9 | `margin=40 risk.daily_cap=10` | -148.17 | 164.15 | 16.29% | 40 | 38 | 23.7% | -3.87 | 1.33 | 7 | 28 | 3 | 2 | 857.05 | -14.30% |
| 10 | `margin=80 risk.daily_cap=5` | -191.93 | 206.93 | 20.39% | 20 | 20 | 20.0% | -9.60 | 1.20 | 3 | 15 | 2 | 0 | 808.07 | -19.19% |
| 11 | `margin=80 risk.daily_cap=40` | -244.17 | 266.56 | 26.07% | 98 | 95 | 32.6% | -2.53 | 1.60 | 30 | 62 | 3 | 3 | 767.18 | -23.28% |
| 12 | `margin=80 risk.daily_cap=10` | -296.33 | 328.30 | 32.35% | 40 | 38 | 23.7% | -7.74 | 1.33 | 7 | 28 | 3 | 2 | 714.10 | -28.59% |
| 13 | `margin=160 risk.daily_cap=10` | -380.58 | 461.87 | 44.84% | 27 | 25 | 24.0% | -15.03 | 1.36 | 5 | 18 | 2 | 2 | 640.28 | -35.97% |
| 14 | `margin=160 risk.daily_cap=20` | -380.58 | 461.87 | 44.84% | 27 | 25 | 24.0% | -15.03 | 1.36 | 5 | 18 | 2 | 2 | 640.28 | -35.97% |
| 15 | `margin=160 risk.daily_cap=40` | -380.58 | 461.87 | 44.84% | 27 | 25 | 24.0% | -15.03 | 1.36 | 5 | 18 | 2 | 2 | 640.28 | -35.97% |
| 16 | `margin=160 risk.daily_cap=5` | -383.85 | 413.86 | 40.18% | 20 | 20 | 20.0% | -19.19 | 1.20 | 3 | 15 | 2 | 0 | 616.15 | -38.39% |

`net` is REALIZED only; a run holding positions at the end carries the rest in `unrealized`, and `equity end` is the sum of both.


## Fee anatomy — what the turnover cost

`gross` is before fees (`net + fees`); `turnover` is the notional those fees were charged on (fee is a flat 7.50bp per side, so turnover = fees ÷ 0.00075); `fees ÷ |net|` is what share of a LOSING run's loss the fee bill is, and is blank for a run that made money.

| combo | gross | fees | turnover | fees ÷ \|net\| | unrealized | kill-switch days | full report |
|---|---:|---:|---:|---:|---:|---|---|
| `margin=20 risk.daily_cap=20` | +7.90 | 43.55 | 58069 | 122.2% | +7.10 | none | [`margin-20__risk.daily_cap-20/report.md`](margin-20__risk.daily_cap-20/report.md) |
| `margin=20 risk.daily_cap=5` | -36.91 | 11.07 | 14763 | 23.1% | +0.00 | none | [`margin-20__risk.daily_cap-5/report.md`](margin-20__risk.daily_cap-5/report.md) |
| `margin=20 risk.daily_cap=40` | +14.69 | 71.08 | 94774 | 126.1% | +3.77 | none | [`margin-20__risk.daily_cap-40/report.md`](margin-20__risk.daily_cap-40/report.md) |
| `margin=40 risk.daily_cap=20` | +15.80 | 87.10 | 116138 | 122.2% | +14.19 | none | [`margin-40__risk.daily_cap-20/report.md`](margin-40__risk.daily_cap-20/report.md) |
| `margin=20 risk.daily_cap=10` | -52.16 | 21.92 | 29225 | 29.6% | +2.61 | none | [`margin-20__risk.daily_cap-10/report.md`](margin-20__risk.daily_cap-10/report.md) |
| `margin=40 risk.daily_cap=5` | -73.82 | 22.14 | 29526 | 23.1% | +0.00 | none | [`margin-40__risk.daily_cap-5/report.md`](margin-40__risk.daily_cap-5/report.md) |
| `margin=80 risk.daily_cap=20` | +58.37 | 163.58 | 218103 | 155.5% | +28.39 | 2026-08-06 | [`margin-80__risk.daily_cap-20/report.md`](margin-80__risk.daily_cap-20/report.md) |
| `margin=40 risk.daily_cap=40` | +29.38 | 142.16 | 189548 | 126.1% | +7.54 | none | [`margin-40__risk.daily_cap-40/report.md`](margin-40__risk.daily_cap-40/report.md) |
| `margin=40 risk.daily_cap=10` | -104.33 | 43.84 | 58449 | 29.6% | +5.21 | none | [`margin-40__risk.daily_cap-10/report.md`](margin-40__risk.daily_cap-10/report.md) |
| `margin=80 risk.daily_cap=5` | -147.64 | 44.29 | 59052 | 23.1% | -0.00 | none | [`margin-80__risk.daily_cap-5/report.md`](margin-80__risk.daily_cap-5/report.md) |
| `margin=80 risk.daily_cap=40` | -31.38 | 212.78 | 283712 | 87.1% | +11.35 | 2026-08-06, 2026-08-08, 2026-08-09 | [`margin-80__risk.daily_cap-40/report.md`](margin-80__risk.daily_cap-40/report.md) |
| `margin=80 risk.daily_cap=10` | -208.66 | 87.67 | 116898 | 29.6% | +10.43 | 2026-08-08 | [`margin-80__risk.daily_cap-10/report.md`](margin-80__risk.daily_cap-10/report.md) |
| `margin=160 risk.daily_cap=10` | -263.27 | 117.31 | 156407 | 30.8% | +20.86 | 2026-08-06, 2026-08-07, 2026-08-08, 2026-08-09 | [`margin-160__risk.daily_cap-10/report.md`](margin-160__risk.daily_cap-10/report.md) |
| `margin=160 risk.daily_cap=20` | -263.27 | 117.31 | 156407 | 30.8% | +20.86 | 2026-08-06, 2026-08-07, 2026-08-08, 2026-08-09 | [`margin-160__risk.daily_cap-20/report.md`](margin-160__risk.daily_cap-20/report.md) |
| `margin=160 risk.daily_cap=40` | -263.27 | 117.31 | 156407 | 30.8% | +20.86 | 2026-08-06, 2026-08-07, 2026-08-08, 2026-08-09 | [`margin-160__risk.daily_cap-40/report.md`](margin-160__risk.daily_cap-40/report.md) |
| `margin=160 risk.daily_cap=5` | -295.27 | 88.58 | 118103 | 23.1% | +0.00 | 2026-08-06, 2026-08-07, 2026-08-08, 2026-08-09 | [`margin-160__risk.daily_cap-5/report.md`](margin-160__risk.daily_cap-5/report.md) |

## Reproduce

```bash
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=20 --set sizing.margin_max=20 --set risk.daily_cap=20   # margin=20 risk.daily_cap=20
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=20 --set sizing.margin_max=20 --set risk.daily_cap=5   # margin=20 risk.daily_cap=5
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=20 --set sizing.margin_max=20 --set risk.daily_cap=40   # margin=20 risk.daily_cap=40
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=40 --set sizing.margin_max=40 --set risk.daily_cap=20   # margin=40 risk.daily_cap=20
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=20 --set sizing.margin_max=20 --set risk.daily_cap=10   # margin=20 risk.daily_cap=10
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=40 --set sizing.margin_max=40 --set risk.daily_cap=5   # margin=40 risk.daily_cap=5
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=80 --set sizing.margin_max=80 --set risk.daily_cap=20   # margin=80 risk.daily_cap=20
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=40 --set sizing.margin_max=40 --set risk.daily_cap=40   # margin=40 risk.daily_cap=40
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=40 --set sizing.margin_max=40 --set risk.daily_cap=10   # margin=40 risk.daily_cap=10
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=80 --set sizing.margin_max=80 --set risk.daily_cap=5   # margin=80 risk.daily_cap=5
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=80 --set sizing.margin_max=80 --set risk.daily_cap=40   # margin=80 risk.daily_cap=40
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=80 --set sizing.margin_max=80 --set risk.daily_cap=10   # margin=80 risk.daily_cap=10
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=160 --set sizing.margin_max=160 --set risk.daily_cap=10   # margin=160 risk.daily_cap=10
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=160 --set sizing.margin_max=160 --set risk.daily_cap=20   # margin=160 risk.daily_cap=20
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=160 --set sizing.margin_max=160 --set risk.daily_cap=40   # margin=160 risk.daily_cap=40
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set sizing.margin_min=160 --set sizing.margin_max=160 --set risk.daily_cap=5   # margin=160 risk.daily_cap=5
```
