"use client"

import * as React from "react"

import type { Position } from "@/lib/api"
import type { TrendPoint } from "@/components/blocks/trend-area"

/**
 * Base cadence. Positions are re-priced off every `mids` frame, which arrives
 * far faster than a 168px sparkline can say anything — so the buffer samples on
 * its own even clock instead of on the feed. An even grid also keeps the chart
 * honest: `TrendArea`'s x-axis is index-linear, so event-driven samples would
 * draw an irregular time axis as if it were regular.
 */
const SAMPLE_MS = 3_000

/**
 * Points held per position. On overflow the buffer halves its resolution and
 * doubles its stride, so the window keeps growing (12min → 24min → 48min …)
 * toward the real holding time without the memory or the path ever growing.
 */
const CAPACITY = 240

type Buffer = { points: TrendPoint[]; stride: number }

export type PnlSparks = ReadonlyMap<number, TrendPoint[]>

const EMPTY: PnlSparks = new Map()

/** Stable empty series, so a position with no samples yet does not churn memos. */
export const NO_SPARK: TrendPoint[] = []

/**
 * Unrealized-PnL history per open position, sampled from the live book.
 *
 * The series starts when this page mounts, not when the position opened —
 * nothing persists it — so callers should label the window it actually covers
 * rather than implying it spans the whole hold.
 */
export function usePnlSparks(positions: Position[]): PnlSparks {
  const latest = React.useRef(positions)
  React.useEffect(() => {
    latest.current = positions
  }, [positions])

  const buffers = React.useRef(new Map<number, Buffer>())
  const [sparks, setSparks] = React.useState<PnlSparks>(EMPTY)

  React.useEffect(() => {
    const id = setInterval(() => {
      const now = Date.now()
      const open = latest.current
      const live = new Set(open.map((p) => p.id))
      const bufs = buffers.current
      let changed = false

      for (const closed of [...bufs.keys()]) {
        if (!live.has(closed)) {
          bufs.delete(closed)
          changed = true
        }
      }

      for (const p of open) {
        const buf = bufs.get(p.id) ?? { points: [], stride: 1 }
        const last = buf.points[buf.points.length - 1]
        // Guard the cadence off the last *kept* sample: a widened stride skips
        // ticks rather than appending and immediately decimating them away.
        if (last && now - last.ts < SAMPLE_MS * buf.stride) continue
        const points = [...buf.points, { ts: now, value: p.unrealized_pnl }]
        const decimate = points.length > CAPACITY
        bufs.set(p.id, {
          points: decimate ? points.filter((_, i) => i % 2 === 0) : points,
          stride: decimate ? buf.stride * 2 : buf.stride,
        })
        changed = true
      }

      if (changed) {
        setSparks(new Map([...bufs].map(([pid, b]) => [pid, b.points])))
      }
    }, SAMPLE_MS)
    return () => clearInterval(id)
  }, [])

  return sparks
}
