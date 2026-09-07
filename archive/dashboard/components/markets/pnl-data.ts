import type { MarketRecord, Trade } from "@/lib/api"
import type { Hue } from "@/components/blocks/icon-chip"

/** One row of the realized-by-market ranking. */
export type RankedMarket = {
  market: string
  /** Σ realized − Σ fees, all time — kestreld's own `market_stats` definition */
  net: number
  /** closed round trips; opening fills are not counted */
  trips: number
  fees: number
}

/**
 * The biggest winners and the biggest losers, `perSide` of each, ordered best →
 * worst so the ranking reads top-down.
 *
 * Flat markets are dropped rather than drawn: a zero-length bar carries no
 * comparison and only costs a row. The caller reports how many were left out.
 */
export function rankMarkets(
  records: MarketRecord[],
  perSide = 8
): RankedMarket[] {
  const rows = records
    .filter((r) => Number.isFinite(r.net_pnl) && r.net_pnl !== 0)
    .map((r) => ({
      market: r.market,
      net: r.net_pnl,
      trips: r.trades,
      fees: r.fees,
    }))
  const winners = rows
    .filter((r) => r.net > 0)
    .sort((a, b) => b.net - a.net)
    .slice(0, perSide)
  const losers = rows
    .filter((r) => r.net < 0)
    .sort((a, b) => a.net - b.net)
    .slice(0, perSide)
  return [...winners, ...losers].sort((a, b) => b.net - a.net)
}

/** How a position finished. `other` is every close kestreld books that is not the first three (today: `time_stop`). */
export type ExitKind = "tp" | "sl" | "veto_close" | "other"

export const EXIT_KINDS: ExitKind[] = ["tp", "sl", "veto_close", "other"]

export const EXIT_LABEL: Record<ExitKind, string> = {
  tp: "Take profit",
  sl: "Stop loss",
  veto_close: "Veto close",
  other: "Time stop",
}

/**
 * tp/sl carry the PnL direction, so they take the PnL hues; a veto is a caution,
 * not a loss; everything else stays neutral. Same mapping as the overview's exit
 * mix, so one colour means one thing across the desk.
 */
export const EXIT_HUE: Record<ExitKind, Hue> = {
  tp: "long",
  sl: "short",
  veto_close: "warning",
  other: "neutral",
}

function exitKind(action: string): ExitKind {
  if (action === "tp" || action === "sl" || action === "veto_close") {
    return action
  }
  return "other"
}

/** One closed position: how long it was held against what it paid. */
export type HoldPoint = {
  market: string
  /** first opening fill → last closing fill, in hours */
  hours: number
  /** Σ realized − Σ fees over every fill of the position, opening fee included */
  net: number
  kind: ExitKind
  closedTs: number
}

export type HoldSample = {
  points: HoldPoint[]
  /** closes whose opening fill sits outside the fetched window — unpairable */
  orphans: number
  /** positions opened inside the window and still running — no hold time yet */
  running: number
}

const HOUR_MS = 3_600_000

/**
 * Pair fills into closed positions by `position_id` and measure each one.
 *
 * The API has no round-trip endpoint — `/api/trades` is a flat fill log — so
 * hold time only exists if it is derived here. Two honest limits come with that,
 * both reported back so the card can say them out loud:
 *
 * 1. The window is the last N fills. A position whose *opening* fill fell off
 *    the end of that window cannot be measured, so long holds are the first to
 *    disappear as the log grows — the sample skews short at the boundary.
 * 2. Hold time is fill-to-fill, not signal-to-exit: the analyst's decision and
 *    the router's latency sit outside it.
 *
 * Partial exits are folded into their position — the *last* close ends the hold
 * and names the exit kind, and net sums every fill the position paid for.
 */
export function holdTimeSample(trades: Trade[]): HoldSample {
  const byPosition = new Map<number, Trade[]>()
  for (const trade of trades) {
    const group = byPosition.get(trade.position_id)
    if (group) group.push(trade)
    else byPosition.set(trade.position_id, [trade])
  }

  const points: HoldPoint[] = []
  let orphans = 0
  let running = 0

  for (const group of byPosition.values()) {
    const closes = group.filter((t) => t.action !== "open")
    if (closes.length === 0) {
      running += 1
      continue
    }
    // Scale-ins share a position id; the first fill is when the risk went on.
    const openedTs = group
      .filter((t) => t.action === "open")
      .reduce((lo, t) => Math.min(lo, t.ts), Number.POSITIVE_INFINITY)
    if (!Number.isFinite(openedTs)) {
      orphans += closes.length
      continue
    }
    const last = closes.reduce((a, b) => (b.ts >= a.ts ? b : a))
    points.push({
      market: last.market,
      hours: Math.max(0, (last.ts - openedTs) / HOUR_MS),
      net: group.reduce((sum, t) => sum + (t.realized_pnl ?? 0) - t.fee, 0),
      kind: exitKind(last.action),
      closedTs: last.ts,
    })
  }

  points.sort((a, b) => a.closedTs - b.closedTs)
  return { points, orphans, running }
}

/** `0.4h` / `18h` / `2.1d` — one hold-time wording across axis, tooltip and meta. */
export function holdLabel(v: number | null): string {
  if (v == null || !Number.isFinite(v)) return "—"
  if (v >= 48) return `${(v / 24).toFixed(1)}d`
  return `${v.toFixed(v < 10 ? 1 : 0)}h`
}

/** Median hold of the sample — the typical position, not the one that ran away. */
export function medianHold(points: HoldPoint[]): number | null {
  if (points.length === 0) return null
  const sorted = points.map((p) => p.hours).sort((a, b) => a - b)
  const mid = Math.floor(sorted.length / 2)
  return sorted.length % 2 === 1
    ? sorted[mid]
    : (sorted[mid - 1] + sorted[mid]) / 2
}

/** Population per exit kind, in the fixed legend order. */
export function exitKindCounts(points: HoldPoint[]): Map<ExitKind, number> {
  const counts = new Map<ExitKind, number>(EXIT_KINDS.map((k) => [k, 0]))
  for (const point of points) {
    counts.set(point.kind, (counts.get(point.kind) ?? 0) + 1)
  }
  return counts
}
