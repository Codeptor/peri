"""Re-score the trades peri actually took under different EXIT parameters.

The problem this exists for: 27 closed trades to 2026-09-07, net -$24.98, with
a per-close standard deviation around $10 on the lineage system's tape. The sd
of a 27-close run is therefore roughly $52, so the entire loss sits inside one
standard deviation of noise. Nothing in that record is a finding, and no amount
of staring at it will make one. Tuning an exit by watching three trades a day on
a $34 account is not measurement; it is waiting.

So: the ledger already stores entry price, initial stop, target, size, side and
the open/close timestamps for every position. Given 1m candles from the entry
forward, the exact exit machinery the engine runs can be replayed over the REAL
trades under different settings, with no LLM and no venue. That answers "would a
wider trail have kept more of the winners" with the trades that actually
happened.

What this is NOT: a strategy backtest. Entries are taken as given — the analyst's
judgement is replayed, never re-simulated — so this measures exits only. Every
report says so in its header.

The exit geometry is imported from `risk`, never reimplemented here: the whole
point is that the harness and the live engine cannot disagree about what the
strategy does.
"""

import math
import sqlite3
import time
from dataclasses import dataclass, field
from typing import Callable, Optional

from peri.config import RiskCfg
from peri.fees import entry_rate, round_trip_rate
from peri.risk import breakeven_px, plan_scale_out, trail_target

# The tape a replay reads. Kept in its own file so a cache rebuild can never
# put the trading ledger at risk.
_SCHEMA = """
CREATE TABLE IF NOT EXISTS candles (
    market TEXT NOT NULL, interval TEXT NOT NULL, t INTEGER NOT NULL,
    o REAL, h REAL, l REAL, c REAL, v REAL,
    PRIMARY KEY (market, interval, t));
"""


class CandleCache:
    """Cache-first 1m tape. Historical candles never change, so a range once
    fetched is fetched once."""

    def __init__(self, path: str = "replay_cache.db"):
        self.conn = sqlite3.connect(path, timeout=30)
        self.conn.row_factory = sqlite3.Row
        self.conn.executescript(_SCHEMA)
        self.conn.commit()

    def store(self, market: str, interval: str, candles: list[dict]) -> None:
        self.conn.executemany(
            "INSERT OR REPLACE INTO candles (market,interval,t,o,h,l,c,v)"
            " VALUES (?,?,?,?,?,?,?,?)",
            [(market, interval, int(c["t"]), float(c["o"]), float(c["h"]),
              float(c["l"]), float(c["c"]), float(c.get("v") or 0))
             for c in candles])
        self.conn.commit()

    def load(self, market: str, interval: str,
             start_ms: int, end_ms: int) -> list[dict]:
        return [dict(r) for r in self.conn.execute(
            "SELECT t,o,h,l,c,v FROM candles WHERE market=? AND interval=?"
            " AND t>=? AND t<=? ORDER BY t", (market, interval, start_ms, end_ms))]

    def covered(self, market: str, interval: str,
                start_ms: int, end_ms: int) -> bool:
        row = self.conn.execute(
            "SELECT MIN(t) lo, MAX(t) hi FROM candles WHERE market=? AND interval=?",
            (market, interval)).fetchone()
        if row is None or row["lo"] is None:
            return False
        return row["lo"] <= start_ms and row["hi"] >= end_ms


@dataclass
class Trade:
    """One recorded position, as the ledger stored it."""
    id: int
    market: str
    side: str
    entry_px: float
    size: float
    init_stop_px: float
    tp_px: float
    opened_ts: float
    closed_ts: Optional[float]
    realized_pnl: Optional[float]
    entry_style: Optional[str] = None
    entry_atr_pct: Optional[float] = None


@dataclass
class Outcome:
    trade_id: int
    market: str
    side: str
    exit_kind: str
    exit_px: float
    held_mins: float
    net_pnl: float
    r: float
    mfe_r: float
    mae_r: float
    banked: bool = False


@dataclass
class Report:
    label: str
    outcomes: list[Outcome] = field(default_factory=list)

    @property
    def n(self) -> int:
        return len(self.outcomes)

    @property
    def net(self) -> float:
        return sum(o.net_pnl for o in self.outcomes)

    @property
    def win_rate(self) -> Optional[float]:
        return (sum(1 for o in self.outcomes if o.net_pnl > 0) / self.n
                if self.n else None)

    @property
    def avg_r(self) -> Optional[float]:
        return (sum(o.r for o in self.outcomes) / self.n) if self.n else None

    @property
    def exit_mix(self) -> dict[str, int]:
        mix: dict[str, int] = {}
        for o in self.outcomes:
            mix[o.exit_kind] = mix.get(o.exit_kind, 0) + 1
        return dict(sorted(mix.items()))

    @property
    def noise_sd(self) -> float:
        """One standard deviation of this run's NET, from its own per-close
        spread. The kestrel scar: a `morning_entry_budget` probe once moved net
        by $183 non-monotonically, and the conclusion drawn from it was noise.
        Nothing below ~2 of these is a finding."""
        if self.n < 2:
            return float("inf")
        mean = self.net / self.n
        var = sum((o.net_pnl - mean) ** 2 for o in self.outcomes) / (self.n - 1)
        return math.sqrt(var) * math.sqrt(self.n)


def load_trades(db_path: str, limit: Optional[int] = None) -> list[Trade]:
    """Every closed position the ledger can replay.

    A trade with no initial stop has no risk unit and no exit to simulate —
    adopted external positions are skipped rather than guessed at."""
    conn = sqlite3.connect(db_path, timeout=30)
    conn.row_factory = sqlite3.Row
    sql = ("SELECT id,market,side,entry_px,size,init_stop_px,tp_px,opened_ts,"
           "closed_ts,realized_pnl,entry_style,entry_atr_pct FROM positions"
           " WHERE status='closed' AND init_stop_px IS NOT NULL"
           " AND tp_px IS NOT NULL AND entry_px>0 AND size>0"
           " ORDER BY opened_ts")
    if limit:
        sql += f" LIMIT {int(limit)}"
    out = []
    for r in conn.execute(sql):
        if abs(float(r["entry_px"]) - float(r["init_stop_px"])) <= 0:
            continue
        out.append(Trade(
            id=r["id"], market=r["market"], side=r["side"],
            entry_px=float(r["entry_px"]), size=float(r["size"]),
            init_stop_px=float(r["init_stop_px"]), tp_px=float(r["tp_px"]),
            opened_ts=float(r["opened_ts"]),
            closed_ts=float(r["closed_ts"]) if r["closed_ts"] else None,
            realized_pnl=r["realized_pnl"], entry_style=r["entry_style"],
            entry_atr_pct=r["entry_atr_pct"]))
    conn.close()
    return out


def _favourable(side: str, px: float, entry: float) -> float:
    return (px - entry) if side == "long" else (entry - px)


def simulate(trade: Trade, candles: list[dict], cfg: RiskCfg,
             *, sz_decimals: int = 4, max_hold_secs: float = 7 * 24 * 3600) -> Outcome:
    """Walk 1m candles and apply the engine's own exit rules to one trade.

    Conventions, inherited from the kestrel harness so results stay comparable:
    within a candle the STOP is checked before the target (the conservative
    reading — a bar that spans both is booked as the loss), and management runs
    once per bar rather than continuously.
    """
    side, entry = trade.side, trade.entry_px
    risk = abs(entry - trade.init_stop_px)
    stop = trade.init_stop_px
    peak = entry
    size = trade.size
    mfe = mae = 0.0
    banked_pnl = 0.0
    banked = False
    resting = (trade.entry_style == "resting")

    plan = plan_scale_out(
        side, entry, trade.init_stop_px, trade.tp_px, size, sz_decimals,
        at_r=cfg.scale_out_at_r, frac=cfg.scale_out_frac,
        min_notional=cfg.min_notional)

    def book(exit_px: float, kind: str, ts: float) -> Outcome:
        gross = _favourable(side, exit_px, entry) * size
        fees = (entry * size * entry_rate(resting)
                + exit_px * size * round_trip_rate(resting) / 2)
        net = banked_pnl + gross - fees
        return Outcome(
            trade_id=trade.id, market=trade.market, side=side, exit_kind=kind,
            exit_px=exit_px, held_mins=(ts - trade.opened_ts) / 60.0,
            net_pnl=net, r=net / (risk * trade.size) if risk else 0.0,
            mfe_r=mfe, mae_r=mae, banked=banked)

    last_px = entry
    ts = trade.opened_ts
    for candle in candles:
        ts = float(candle["t"]) / 1000.0
        if ts - trade.opened_ts > max_hold_secs:
            break
        hi, lo, close = float(candle["h"]), float(candle["l"]), float(candle["c"])
        last_px = close
        best = hi if side == "long" else lo
        worst = lo if side == "long" else hi
        if risk:
            mfe = max(mfe, _favourable(side, best, entry) / risk)
            mae = min(mae, _favourable(side, worst, entry) / risk)

        # 1. stop first: a bar spanning both brackets is booked as the loss
        if (side == "long" and lo <= stop) or (side == "short" and hi >= stop):
            kind = ("initial_stop" if stop == trade.init_stop_px
                    else "trail_stop" if _favourable(side, stop, entry) > 0
                    else "breakeven_stop")
            return book(stop, kind, ts)

        # 2. the banked tranche
        if (plan is not None and not banked
                and ((side == "long" and hi >= plan.tp1_px)
                     or (side == "short" and lo <= plan.tp1_px))):
            gross = _favourable(side, plan.tp1_px, entry) * plan.tp1_size
            banked_pnl += gross - (
                entry * plan.tp1_size * entry_rate(resting)
                + plan.tp1_px * plan.tp1_size * round_trip_rate(resting) / 2)
            size = plan.runner_size
            banked = True

        # 3. the target
        if (side == "long" and hi >= trade.tp_px) or (side == "short" and lo <= trade.tp_px):
            return book(trade.tp_px, "tp", ts)

        # 4. management, once a bar
        peak = max(peak, best) if side == "long" else min(peak, best)
        r_now = _favourable(side, close, entry) / risk if risk else 0.0
        target = None
        if cfg.trail_start_r > 0 and r_now >= cfg.trail_start_r:
            target = trail_target(side, peak, risk, trade.entry_atr_pct,
                                  giveback_r=cfg.trail_giveback_r,
                                  atr_mult=cfg.trail_atr_mult)
        if target is None and cfg.breakeven_at_r > 0 and r_now >= cfg.breakeven_at_r:
            target = breakeven_px(side, entry, round_trip_rate(resting) / 2)
        if target is not None:
            better = target > stop if side == "long" else target < stop
            wrong_side = ((side == "long" and target >= close)
                          or (side == "short" and target <= close))
            if better and not wrong_side:
                stop = target

        # 5. the time stop, exempt once a tranche has banked
        if (cfg.time_stop_secs > 0 and not banked
                and ts - trade.opened_ts >= cfg.time_stop_secs
                and r_now < cfg.time_stop_min_r):
            return book(close, "time_stop", ts)

    return book(last_px, "open_at_end", ts)


def run(trades: list[Trade], candles_for: Callable[[Trade], list[dict]],
        cfg: RiskCfg, label: str, **kw) -> Report:
    report = Report(label=label)
    for trade in trades:
        candles = candles_for(trade)
        if not candles:
            continue
        report.outcomes.append(simulate(trade, candles, cfg, **kw))
    return report


def format_report(reports: list[Report], *, source: str = "") -> str:
    """Markdown, with the caveats that make the numbers readable.

    Both headers are load-bearing. The first stops a reader treating this as a
    strategy result when the entries were given. The second stops a $12
    difference across 27 trades being called an improvement."""
    lines = [
        "# Exit-parameter replay",
        "",
        "**Exits only — the analyst's entries are replayed as taken, never "
        "re-simulated. This measures what happened AFTER the entry and nothing "
        "about whether the entry was good.**",
        "",
        f"Source: {source or 'ledger'} · generated "
        f"{time.strftime('%Y-%m-%d %H:%MZ', time.gmtime())}",
        "",
        "| run | n | net $ | win | avg R | 1sd of net | exit mix |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    for r in reports:
        wr = f"{r.win_rate:.0%}" if r.win_rate is not None else "?"
        ar = f"{r.avg_r:+.2f}R" if r.avg_r is not None else "?"
        mix = ", ".join(f"{k} {v}" for k, v in r.exit_mix.items()) or "—"
        lines.append(f"| {r.label} | {r.n} | {r.net:+.2f} | {wr} | {ar} | "
                     f"±{r.noise_sd:.2f} | {mix} |")
    if reports:
        base = reports[0]
        lines += ["", "## Is any of this a finding?", ""]
        for r in reports[1:]:
            delta = r.net - base.net
            bar = 2 * max(base.noise_sd, r.noise_sd)
            verdict = ("A FINDING" if abs(delta) >= bar else
                       "NOT a finding — inside the noise")
            lines.append(f"- **{r.label}** vs {base.label}: {delta:+.2f} against a "
                         f"2sd bar of ±{bar:.2f} → {verdict}.")
        lines += ["", "Nothing under ~2 standard deviations of net difference is a "
                      "finding. The lineage system once moved net by $183 on a "
                      "parameter probe that turned out to be noise."]
    return "\n".join(lines)
