"use client"

import * as React from "react"
import { Analytics01Icon } from "@hugeicons/core-free-icons"

import type { Decision } from "@/lib/api"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import { DotMatrix } from "@/components/blocks/dot-matrix"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented } from "@/components/blocks/segmented"
import {
  type DecisionDay,
  decisionDays,
  sparseDayLabels,
} from "@/components/intel/intel-utils"

const CELL = 8
const GAP = 3

/**
 * The matrix sizes itself to the busiest day in the window instead of standing at a
 * fixed twelve. Below the ceiling the grid is exactly as tall as the peak, so one cell
 * is one decision and the chart stops rounding — which was the only real argument for
 * replacing it with bars. The floor keeps a quiet week from collapsing into a stripe.
 */
const MIN_ROWS = 10
const MAX_ROWS = 14
/** Skeleton height: mid-range, so the first paint barely moves whatever the data turns out to be. */
const NOMINAL_ROWS = 12
const MATRIX_STYLE = { height: NOMINAL_ROWS * (CELL + GAP) - GAP + 16 }

type RangeKey = "7" | "14" | "30"

const RANGES: { value: RangeKey; label: string }[] = [
  { value: "7", label: "7d" },
  { value: "14", label: "14d" },
  { value: "30", label: "30d" },
]

type Series = {
  key: "executed" | "skipped" | "refused"
  label: string
  color: string
  dot: string
}

/** Non-PnL semantics: orange = what the desk acted on, neutral = passes, red = LLM refusals. */
const SERIES: Series[] = [
  {
    key: "executed",
    label: "Executed",
    color: "var(--accent-orange)",
    dot: "bg-accent-orange",
  },
  {
    key: "skipped",
    label: "Skipped",
    color: "var(--chart-6)",
    dot: "bg-muted-foreground",
  },
  { key: "refused", label: "Refused", color: "var(--short)", dot: "bg-short" },
]

function SeriesMatrix({
  series,
  days,
  max,
  rows,
  labels,
}: {
  series: Series
  days: DecisionDay[]
  max: number
  rows: number
  labels: string[]
}) {
  const total = days.reduce((sum, d) => sum + d[series.key], 0)
  const columns = days.map((d) => {
    const count = d[series.key]
    const share = d.total > 0 ? Math.round((count / d.total) * 100) : 0
    return {
      value: count,
      color: series.color,
      // ` · ` segments: meta line, headline figure, trailer (see DotMatrix).
      label: `${d.date} · ${count} ${series.label.toLowerCase()} · ${
        d.total === 0
          ? "nothing logged that day"
          : `${share}% of ${d.total} that day`
      }`,
    }
  })

  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        <span className={cn("size-1.5 shrink-0 rounded-full", series.dot)} />
        <span className="text-[13px] text-muted-foreground">
          {series.label}
        </span>
        <span className="ml-auto font-mono text-[15px] font-semibold">
          {total}
        </span>
      </div>
      <DotMatrix
        ariaLabel={`${series.label} decisions per UTC day, ${days.length} days, ${total} total`}
        cellSize={CELL}
        className="mt-3"
        columns={columns}
        gap={GAP}
        max={max}
        rows={rows}
        xLabels={labels}
      />
    </div>
  )
}

export type DecisionThroughputProps = {
  decisions: Decision[] | null
  /** null until the client clock is seeded — keeps the first render deterministic */
  now: number | null
}

/**
 * Decisions per UTC day, split into three matrices so the mix reads at a glance.
 *
 * Kept as a matrix rather than promoted to bars, deliberately. These are counts of
 * discrete events — the DotMatrix's declared domain — and the three series share one
 * `max`, so a tall executed column and a short refused one are directly comparable
 * across the row. Three `BarChart`s cannot reproduce that: each computes its own
 * y-domain, so a day with two refusals would draw the same height as a day with twenty
 * executions, and the one comparison this card exists to support would quietly become a
 * lie. The form's real cost was quantisation, and that is fixed above by sizing the grid
 * to the window's peak instead of a fixed twelve; the tooltips now carry the exact count
 * and its share of the day besides.
 */
export function DecisionThroughput({
  decisions,
  now,
}: DecisionThroughputProps) {
  const [range, setRange] = React.useState<RangeKey>("30")
  const rangeDays = Number(range)

  const ready = decisions != null && now != null

  const days = React.useMemo(
    () => (now == null ? [] : decisionDays(decisions ?? [], rangeDays, now)),
    [decisions, rangeDays, now]
  )
  const labels = React.useMemo(() => sparseDayLabels(days), [days])
  const peak = days.reduce(
    (hi, d) => Math.max(hi, d.executed, d.skipped, d.refused),
    0
  )
  // the matrices need a non-zero scale even on an empty window
  const max = Math.max(1, peak)
  const rows = Math.min(MAX_ROWS, Math.max(MIN_ROWS, peak))
  const total = days.reduce((sum, d) => sum + d.total, 0)

  return (
    <SectionCard
      action={
        <Segmented
          label="Throughput window"
          onChange={setRange}
          options={RANGES}
          size="sm"
          value={range}
        />
      }
      icon={Analytics01Icon}
      title="Decisions per day"
    >
      {ready ? (
        <>
          <div className="grid gap-6 sm:grid-cols-3">
            {SERIES.map((s) => (
              <SeriesMatrix
                days={days}
                key={s.key}
                labels={labels}
                max={max}
                rows={rows}
                series={s}
              />
            ))}
          </div>
          <div className="mt-4 text-[13px] text-muted-foreground">
            <span className="font-mono text-foreground">{total}</span>
            {" decisions over "}
            <span className="font-mono text-foreground">{rangeDays}</span>
            {" UTC days · peak "}
            <span className="font-mono text-foreground">{peak}</span>
            {" per day"}
            {peak > rows ? (
              <>
                {" · one cell ≈ "}
                <span className="font-mono text-foreground">
                  {(peak / rows).toFixed(1)}
                </span>
              </>
            ) : null}
          </div>
        </>
      ) : (
        <div className="grid gap-6 sm:grid-cols-3">
          {SERIES.map((s) => (
            <div className="flex flex-col gap-3" key={s.key}>
              <Skeleton className="h-4 w-24 rounded-md" />
              <Skeleton className="w-full rounded-md" style={MATRIX_STYLE} />
            </div>
          ))}
        </div>
      )}
    </SectionCard>
  )
}
