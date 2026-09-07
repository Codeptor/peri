"use client"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { TrendArea, type TrendPoint } from "@/components/blocks/trend-area"

const HEIGHT = 46

/**
 * Breakeven. `TrendArea` draws a reference line only when it falls inside the
 * window's domain, so this appears exactly when the position crossed flat —
 * and stays out of the way when the whole window was one-sided.
 */
const BREAKEVEN = [{ value: 0, label: "0", color: "var(--muted-foreground)" }]

/** Coarse span of a sampled window: `40s`, `12m`, `2h 05m`. */
function spanLabel(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000))
  if (secs < 60) return `${secs}s`
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m`
  return `${Math.floor(mins / 60)}h ${String(mins % 60).padStart(2, "0")}m`
}

export type PnlSparkProps = {
  /** ring buffer from `usePnlSparks` — ascending, evenly sampled */
  points: TrendPoint[]
  className?: string
}

/**
 * Unrealized PnL against holding time for the selected position.
 *
 * The figure beside it says where the position stands; this says how it got
 * there — whether a green number is a steady climb or the tail of a round trip.
 * The caption states the window the samples actually cover, because the buffer
 * starts at page load, not at the fill.
 */
export function PnlSpark({ points, className }: PnlSparkProps) {
  const ready = points.length >= 2
  const last = points[points.length - 1]
  const span = ready ? spanLabel(last.ts - points[0].ts) : null
  const color =
    !ready || last.value === 0
      ? "var(--chart-6)"
      : last.value > 0
        ? "var(--long)"
        : "var(--short)"

  return (
    <div className={cn("w-[168px] shrink-0", className)}>
      {ready ? (
        <TrendArea
          color={color}
          data={points}
          format={usd}
          height={HEIGHT}
          live
          refLines={BREAKEVEN}
          strokeWidth={1.75}
        />
      ) : (
        <div
          className="grid w-full place-items-center rounded-[8px] bg-surface-2 text-[10.5px] text-muted-foreground"
          style={{ height: HEIGHT }}
        >
          collecting…
        </div>
      )}
      <div className="mt-1 flex items-baseline gap-2 text-[10.5px] leading-4 text-muted-foreground">
        <span>uPnL</span>
        <span className="ml-auto shrink-0">
          {span == null ? (
            "since page load"
          ) : (
            <>
              last <span className="font-mono">{span}</span>
            </>
          )}
        </span>
      </div>
    </div>
  )
}
