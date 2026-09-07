"""Re-score the trades peri actually took under different EXIT parameters.

    uv run python replay_exits.py --db /path/to/peri.db --out docs/replays/

Reads closed positions from a ledger, fetches 1m candles from the entry forward
(cached in replay_cache.db, a separate file — a cache rebuild must never put the
trading ledger at risk), and replays the engine's own exit machinery over them
under the current config and a set of comparison variants.

Exits only: the analyst's entries are replayed as taken, never re-simulated.
Read the noise bar at the bottom of the report before believing any difference.
"""

import argparse
import sys
import time

from peri.app import http_post
from peri.config import load_config
from peri.market import Market
from peri.replay import CandleCache, format_report, load_trades, run

# The questions this was built to answer, all on the exit side. The baseline
# MUST be first: every verdict is measured against it.
VARIANTS = [
    ("baseline (config.toml)", {}),
    ("old geometry (pre-09-07)", dict(trail_start_r=0.5, trail_atr_mult=1.0,
                                      trail_giveback_r=0.0, scale_out_at_r=0.0)),
    ("no scale-out", dict(scale_out_at_r=0.0)),
    ("no trail at all", dict(trail_start_r=0.0)),
    ("giveback 0.25R", dict(trail_giveback_r=0.25)),
    ("giveback 0.75R", dict(trail_giveback_r=0.75)),
    ("no time stop", dict(time_stop_secs=0)),
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="peri.db", help="ledger to replay")
    ap.add_argument("--cache", default="replay_cache.db")
    ap.add_argument("--config", default="config.toml")
    ap.add_argument("--out", default="", help="write the markdown report here")
    ap.add_argument("--max-hold-hours", type=float, default=168.0)
    a = ap.parse_args()

    cfg = load_config(a.config, "/nonexistent.env")
    trades = load_trades(a.db)
    if not trades:
        print(f"{a.db}: no replayable closed positions.\n"
              "A trade needs an initial stop and a target to have an exit to "
              "simulate; adopted external positions have neither.", file=sys.stderr)
        return 1
    print(f"{len(trades)} replayable trades from {a.db}")

    cache = CandleCache(a.cache)
    market = Market(http_post(cfg.hl_network), cfg.universe.dexes)
    horizon_ms = int(a.max_hold_hours * 3600 * 1000)

    def candles_for(trade):
        start = int(trade.opened_ts * 1000)
        end = start + horizon_ms
        if not cache.covered(trade.market, "1m", start, end):
            try:
                # an EXPLICIT window: these trades are historical, and
                # Market.candles always reads backwards from now
                fetched = market.candles_range(trade.market, "1m", start, end)
            except Exception as exc:  # noqa: BLE001 — one market never kills the run
                print(f"  {trade.market}: candles unavailable ({exc!r})",
                      file=sys.stderr)
                fetched = []
            if fetched:
                cache.store(trade.market, "1m", fetched)
        return cache.load(trade.market, "1m", start, end)

    reports = []
    for label, over in VARIANTS:
        risk = load_config(a.config, "/nonexistent.env").risk
        for key, value in over.items():
            setattr(risk, key, value)
        reports.append(run(trades, candles_for, risk, label,
                           max_hold_secs=a.max_hold_hours * 3600))
        print(f"  {label}: net {reports[-1].net:+.2f} over {reports[-1].n}")

    text = format_report(reports, source=a.db)
    print("\n" + text)
    if a.out:
        path = a.out.rstrip("/") + f"/{time.strftime('%Y-%m-%d')}-exit-replay.md"
        with open(path, "w") as fh:
            fh.write(text + "\n")
        print(f"\nwritten to {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
