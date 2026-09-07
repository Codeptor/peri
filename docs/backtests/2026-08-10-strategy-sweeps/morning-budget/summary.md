# backtest sweep — morning-budget

**2026-08-06 00:00 .. 2026-08-09 22:08 UTC** · 5373 bars · 271 markets · 5 runs

Axes:

- `risk.morning_entry_budget` = 20, 12, 6, 3, 0

## What this is not

- mechanical replay — no analyst judgment, no news: conviction is fixed at 0.75 for every entry and the screener's side_hint is taken as the side (spec Decision 2)
- no review loop: no veto closes, no stop moves, no analyst time-stop — brackets and the horizon are the only exits (spec Decision 5)
- exits: SL is checked before TP inside a candle (a candle touching both is scored as the stop) and fills AT the trigger; the 24h horizon fills at the candle close (spec Decision 5)
- entries fill at the next 1m open plus flat slip (2bp native / 5bp dex); 7.5bp taker fee per side (spec Decision 5)
- day_ntl_vlm is proxied by rolling 24h candle quote volume — the venue does not serve historical dayNtlVlm, and this proxy decides WHICH markets clear the universe filter (spec Decision 4)
- features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)
- open_interest is 0.0 throughout — historical OI is not served (nothing in the screener, sizing or the gates reads it)
- the screener ticks every 1m (live: 45s) — the closest candle-aligned cadence (spec Decision 6)

## Ranked — by net, ties to the shallower drawdown

| # | combo | net | maxDD | maxDD% | entries | closes | win% | expectancy | payoff | tp | sl | time_stop | open | equity end | return% |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | `risk.morning_entry_budget=0` | -59.23 | 111.22 | 10.70% | 80 | 78 | 35.9% | -0.74 | 1.55 | 27 | 49 | 2 | 2 | 947.29 | -5.27% |
| 2 | `risk.morning_entry_budget=12` | -71.30 | 102.98 | 10.22% | 80 | 77 | 35.1% | -0.90 | 1.55 | 25 | 48 | 4 | 3 | 942.90 | -5.71% |
| 3 | `risk.morning_entry_budget=20` | -92.40 | 109.79 | 10.83% | 80 | 78 | 33.3% | -1.17 | 1.58 | 24 | 51 | 3 | 2 | 912.82 | -8.72% |
| 4 | `risk.morning_entry_budget=3` | -109.84 | 127.39 | 12.64% | 80 | 78 | 33.3% | -1.39 | 1.52 | 26 | 50 | 2 | 2 | 887.03 | -11.30% |
| 5 | `risk.morning_entry_budget=6` | -242.58 | 245.11 | 24.33% | 77 | 76 | 25.0% | -3.18 | 1.53 | 18 | 55 | 3 | 1 | 763.18 | -23.68% |

`net` is REALIZED only; a run holding positions at the end carries the rest in `unrealized`, and `equity end` is the sum of both.


## Fee anatomy — what the turnover cost

`gross` is before fees (`net + fees`); `turnover` is the notional those fees were charged on (fee is a flat 7.50bp per side, so turnover = fees ÷ 0.00075); `fees ÷ |net|` is what share of a LOSING run's loss the fee bill is, and is blank for a run that made money.

| combo | gross | fees | turnover | fees ÷ \|net\| | unrealized | kill-switch days | full report |
|---|---:|---:|---:|---:|---:|---|---|
| `risk.morning_entry_budget=0` | +27.74 | 86.97 | 115964 | 146.8% | +6.52 | none | [`risk.morning_entry_budget-0/report.md`](risk.morning_entry_budget-0/report.md) |
| `risk.morning_entry_budget=12` | +15.80 | 87.10 | 116138 | 122.2% | +14.19 | none | [`risk.morning_entry_budget-12/report.md`](risk.morning_entry_budget-12/report.md) |
| `risk.morning_entry_budget=20` | -7.43 | 84.97 | 113292 | 92.0% | +5.21 | none | [`risk.morning_entry_budget-20/report.md`](risk.morning_entry_budget-20/report.md) |
| `risk.morning_entry_budget=3` | -23.32 | 86.52 | 115361 | 78.8% | -3.13 | none | [`risk.morning_entry_budget-3/report.md`](risk.morning_entry_budget-3/report.md) |
| `risk.morning_entry_budget=6` | -157.24 | 85.34 | 113787 | 35.2% | +5.76 | 2026-08-06 | [`risk.morning_entry_budget-6/report.md`](risk.morning_entry_budget-6/report.md) |

## Reproduce

```bash
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set risk.morning_entry_budget=0   # risk.morning_entry_budget=0
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set risk.morning_entry_budget=12   # risk.morning_entry_budget=12
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set risk.morning_entry_budget=20   # risk.morning_entry_budget=20
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set risk.morning_entry_budget=3   # risk.morning_entry_budget=3
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set risk.morning_entry_budget=6   # risk.morning_entry_budget=6
```
