import type { Trade } from "./api";

export type DayPnl = { date: string; net: number; closes: number };

export function isClose(t: Trade): boolean {
  return t.action !== "open";
}

/** Net result of a fill: gross realized minus fee (opens are pure fee drag). */
export function netPnl(t: Trade): number {
  return (t.realized_pnl ?? 0) - t.fee;
}

/** A fill has no booked PnL until the position closes. */
export function realizedNetPnl(t: Trade): number | null {
  return t.realized_pnl == null ? null : netPnl(t)
}

function utcDay(ts: number): string {
  return new Date(ts).toISOString().slice(0, 10);
}

/** All trades bucketed by UTC day, Σ netPnl per day, ascending by date. */
export function dailyNetPnl(trades: Trade[]): DayPnl[] {
  const days = new Map<string, { net: number; closes: number }>();
  for (const t of trades) {
    const d = utcDay(t.ts);
    const cur = days.get(d) ?? { net: 0, closes: 0 };
    cur.net += netPnl(t);
    if (isClose(t)) cur.closes += 1;
    days.set(d, cur);
  }
  return [...days.entries()]
    .map(([date, v]) => ({ date, ...v }))
    .sort((a, b) => a.date.localeCompare(b.date));
}

export function exitMix(trades: Trade[]): {
  tp: number;
  sl: number;
  veto_close: number;
  other: number;
} {
  const mix = { tp: 0, sl: 0, veto_close: 0, other: 0 };
  for (const t of trades) {
    if (!isClose(t)) continue;
    if (t.action === "tp") mix.tp += 1;
    else if (t.action === "sl") mix.sl += 1;
    else if (t.action === "veto_close") mix.veto_close += 1;
    else mix.other += 1;
  }
  return mix;
}

/** Closes with positive gross realized / all closes. Null when no closes. */
export function winRate(trades: Trade[]): number | null {
  const closes = trades.filter(isClose);
  if (closes.length === 0) return null;
  const wins = closes.filter((t) => (t.realized_pnl ?? 0) > 0).length;
  return (wins / closes.length) * 100;
}

export function todayStats(
  trades: Trade[],
  now: number
): { realized: number; fees: number; closes: number; entries: number } {
  const day = utcDay(now);
  const today = trades.filter((t) => utcDay(t.ts) === day);
  return {
    realized: today.reduce((s, t) => s + (t.realized_pnl ?? 0), 0),
    fees: today.reduce((s, t) => s + t.fee, 0),
    closes: today.filter(isClose).length,
    entries: today.filter((t) => t.action === "open").length,
  };
}
