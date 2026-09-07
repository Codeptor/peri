# backtest sweep — conviction

**2026-08-06 00:00 .. 2026-08-08 23:59 UTC** · 4074 bars · 271 markets · 3 runs

Axes:

- `conviction` = 0.70, 0.75, 0.80

## What this is not

- mechanical replay — no analyst judgment, no news: conviction is fixed per run — this grid sweeps it over 0.70, 0.75, 0.80 and the screener's side_hint is taken as the side (spec Decision 2)
- no review loop: no veto closes, no stop moves, no analyst time-stop — brackets and the horizon are the only exits (spec Decision 5)
- exits: SL is checked before TP inside a candle (a candle touching both is scored as the stop) and fills AT the trigger; the 24h horizon fills at the candle close (spec Decision 5)
- entries fill at the next 1m open plus flat slip (2bp native / 5bp dex); 7.5bp taker fee per side (spec Decision 5)
- day_ntl_vlm is proxied by rolling 24h candle quote volume — the venue does not serve historical dayNtlVlm, and this proxy decides WHICH markets clear the universe filter (spec Decision 4)
- features are computed on 1m candle closes, not the live 1/5s mid tape; trade prints cross the spread, so vol1h runs slightly hotter than live (spec Decision 4)
- open_interest is 0.0 throughout — historical OI is not served (nothing in the screener, sizing or the gates reads it)
- the screener ticks every 1m (live: 45s) — the closest candle-aligned cadence (spec Decision 6)
- conviction axis: with no analyst every entry carries the same conviction, so risk.conviction_min moves with it (each run = 'the analyst always answered X'). What the axis measures is sizing — leverage 5+15·c·vol_ratio and margin bankroll·(0.01+0.04·c). Set risk.conviction_min explicitly to sweep the gate threshold instead.

## Ranked — by net, ties to the shallower drawdown

| # | combo | net | maxDD | maxDD% | entries | closes | win% | expectancy | payoff | tp | sl | time_stop | open | equity end | return% |
|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | `conviction=0.70` | -71.88 | 141.45 | 13.37% | 60 | 57 | 33.3% | -1.23 | 1.54 | 19 | 36 | 2 | 3 | 938.06 | -6.19% |
| 2 | `conviction=0.75` | -75.78 | 150.72 | 14.21% | 60 | 57 | 33.3% | -1.30 | 1.54 | 19 | 36 | 2 | 3 | 934.69 | -6.53% |
| 3 | `conviction=0.80` | -79.95 | 158.93 | 14.94% | 60 | 57 | 33.3% | -1.37 | 1.54 | 19 | 36 | 2 | 3 | 931.04 | -6.90% |

`net` is REALIZED only; a run holding positions at the end carries the rest in `unrealized`, and `equity end` is the sum of both.

| combo | unrealized | fees | kill-switch days | full report |
|---|---:|---:|---|---|
| `conviction=0.70` | +9.94 | 58.68 | none | [`conviction-0.70/report.md`](conviction-0.70/report.md) |
| `conviction=0.75` | +10.46 | 62.67 | none | [`conviction-0.75/report.md`](conviction-0.75/report.md) |
| `conviction=0.80` | +10.99 | 66.18 | none | [`conviction-0.80/report.md`](conviction-0.80/report.md) |

## Reproduce

```bash
kestreld backtest run --from 2026-08-06 --to 2026-08-08 --conviction 0.7 --set risk.conviction_min=0.7   # conviction=0.70
kestreld backtest run --from 2026-08-06 --to 2026-08-08 --conviction 0.75 --set risk.conviction_min=0.75   # conviction=0.75
kestreld backtest run --from 2026-08-06 --to 2026-08-08 --conviction 0.8 --set risk.conviction_min=0.8   # conviction=0.80
```
