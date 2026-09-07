# Exit-parameter replay

**Exits only — the analyst's entries are replayed as taken, never re-simulated. This measures what happened AFTER the entry and nothing about whether the entry was good.**

Source: real_peri.db · generated 2026-09-07 22:30Z

| run | n | net $ | win | avg R | 1sd of net | exit mix |
|---|---:|---:|---:|---:|---:|---|
| baseline (config.toml) | 13 | +34.69 | 69% | +0.50R | ±23.37 | initial_stop 4, tp 4, trail_stop 5 |
| old geometry (pre-09-07) | 13 | +14.29 | 69% | +0.41R | ±14.03 | initial_stop 4, open_at_end 1, time_stop 3, tp 3, trail_stop 2 |
| no scale-out | 13 | +50.66 | 69% | +0.77R | ±29.92 | initial_stop 4, tp 4, trail_stop 5 |
| no trail at all | 13 | +16.41 | 69% | +0.31R | ±15.34 | initial_stop 4, open_at_end 3, tp 4, trail_stop 2 |
| giveback 0.25R | 13 | +28.66 | 69% | +0.45R | ±20.20 | initial_stop 4, tp 4, trail_stop 5 |
| giveback 0.75R | 13 | +30.98 | 69% | +0.46R | ±21.74 | initial_stop 4, tp 4, trail_stop 5 |
| no time stop | 13 | +34.69 | 69% | +0.50R | ±23.37 | initial_stop 4, tp 4, trail_stop 5 |

## Is any of this a finding?

- **old geometry (pre-09-07)** vs baseline (config.toml): -20.40 against a 2sd bar of ±46.74 → NOT a finding — inside the noise.
- **no scale-out** vs baseline (config.toml): +15.97 against a 2sd bar of ±59.84 → NOT a finding — inside the noise.
- **no trail at all** vs baseline (config.toml): -18.28 against a 2sd bar of ±46.74 → NOT a finding — inside the noise.
- **giveback 0.25R** vs baseline (config.toml): -6.03 against a 2sd bar of ±46.74 → NOT a finding — inside the noise.
- **giveback 0.75R** vs baseline (config.toml): -3.71 against a 2sd bar of ±46.74 → NOT a finding — inside the noise.
- **no time stop** vs baseline (config.toml): +0.00 against a 2sd bar of ±46.74 → NOT a finding — inside the noise.

Nothing under ~2 standard deviations of net difference is a finding. The lineage system once moved net by $183 on a parameter probe that turned out to be noise.
