# backtest sweep — min-score

**2026-08-06 00:00 .. 2026-08-09 22:07 UTC** · 5373 bars · 271 markets · 3 runs

Axes:

- `screener.min_score` = 1.8, 2.5, 3.5

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
| 1 | `screener.min_score=1.8` | -71.30 | 102.98 | 10.22% | 80 | 77 | 35.1% | -0.90 | 1.55 | 25 | 48 | 4 | 3 | 942.90 | -5.71% |
| 2 | `screener.min_score=2.5` | -71.30 | 102.98 | 10.22% | 80 | 77 | 35.1% | -0.90 | 1.55 | 25 | 48 | 4 | 3 | 942.90 | -5.71% |
| 3 | `screener.min_score=3.5` | -141.50 | 173.78 | 17.24% | 80 | 77 | 31.2% | -1.81 | 1.52 | 22 | 51 | 4 | 3 | 872.70 | -12.73% |

`net` is REALIZED only; a run holding positions at the end carries the rest in `unrealized`, and `equity end` is the sum of both.


## Fee anatomy — what the turnover cost

`gross` is before fees (`net + fees`); `turnover` is the notional those fees were charged on (fee is a flat 7.50bp per side, so turnover = fees ÷ 0.00075); `fees ÷ |net|` is what share of a LOSING run's loss the fee bill is, and is blank for a run that made money.

| combo | gross | fees | turnover | fees ÷ \|net\| | unrealized | kill-switch days | full report |
|---|---:|---:|---:|---:|---:|---|---|
| `screener.min_score=1.8` | +15.80 | 87.10 | 116138 | 122.2% | +14.19 | none | [`screener.min_score-1.8/report.md`](screener.min_score-1.8/report.md) |
| `screener.min_score=2.5` | +15.80 | 87.10 | 116138 | 122.2% | +14.19 | none | [`screener.min_score-2.5/report.md`](screener.min_score-2.5/report.md) |
| `screener.min_score=3.5` | -54.81 | 86.68 | 115579 | 61.3% | +14.19 | none | [`screener.min_score-3.5/report.md`](screener.min_score-3.5/report.md) |

## Reproduce

```bash
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set screener.min_score=1.8   # screener.min_score=1.8
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set screener.min_score=2.5   # screener.min_score=2.5
kestreld backtest run --from 2026-08-06 --to 2026-08-09 --conviction 0.75 --set screener.min_score=3.5   # screener.min_score=3.5
```
