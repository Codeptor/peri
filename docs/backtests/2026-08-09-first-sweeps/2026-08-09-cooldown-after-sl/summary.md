# backtest sweep — cooldown-after-sl

**2026-08-06 00:00 .. 2026-08-08 23:59 UTC** · 4074 bars · 271 markets · 2 runs

Axes:

- `risk.cooldown_after_sl_min` = 30, 120

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
| 1 | `risk.cooldown_after_sl_min=120` | -75.78 | 150.72 | 14.21% | 60 | 57 | 33.3% | -1.30 | 1.54 | 19 | 36 | 2 | 3 | 934.69 | -6.53% |
| 2 | `risk.cooldown_after_sl_min=30` | -91.31 | 143.94 | 13.86% | 60 | 57 | 31.6% | -1.57 | 1.59 | 18 | 37 | 2 | 3 | 919.16 | -8.08% |

`net` is REALIZED only; a run holding positions at the end carries the rest in `unrealized`, and `equity end` is the sum of both.

| combo | unrealized | fees | kill-switch days | full report |
|---|---:|---:|---|---|
| `risk.cooldown_after_sl_min=120` | +10.46 | 62.67 | none | [`risk.cooldown_after_sl_min-120/report.md`](risk.cooldown_after_sl_min-120/report.md) |
| `risk.cooldown_after_sl_min=30` | +10.46 | 64.06 | none | [`risk.cooldown_after_sl_min-30/report.md`](risk.cooldown_after_sl_min-30/report.md) |

## Reproduce

```bash
kestreld backtest run --from 2026-08-06 --to 2026-08-08 --conviction 0.75 --set risk.cooldown_after_sl_min=120   # risk.cooldown_after_sl_min=120
kestreld backtest run --from 2026-08-06 --to 2026-08-08 --conviction 0.75 --set risk.cooldown_after_sl_min=30   # risk.cooldown_after_sl_min=30
```
