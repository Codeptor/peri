"use client"

import * as React from "react"
import { ChartAverageIcon } from "@hugeicons/core-free-icons"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  RollingLine,
  type RollingPoint,
} from "@/components/blocks/rolling-line"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented } from "@/components/blocks/segmented"
import { Skeleton } from "@/components/ui/skeleton"
import { FeedEmpty, StateUnavailable } from "@/components/intel/feed-state"
import { pnlInk, utcStamp } from "@/components/intel/intel-utils"
import type { CloseSet } from "@/components/intel/r-multiple"

/** The window the task is measured over: ten closes is short enough to drift, long enough to mean something. */
const WINDOW = 10

type Metric = "win" | "expectancy"

const METRICS: { value: Metric; label: string }[] = [
  { value: "win", label: "Win rate" },
  { value: "expectancy", label: "Expectancy" },
]

const pct = (v: number) => `${Math.round(v * 100)}%`

export type RollingDisciplineProps = {
  /** null until the first poll settles */
  closeSet: CloseSet | null
  note: string | null
  loaded: boolean
}

/**
 * The same closes as the R timeline, read as a series instead of a cloud: is the desk
 * getting better or worse at this, and when did it turn?
 *
 * Two readings share one line because they answer the same question from opposite ends.
 * **Win rate** is how often it is right; **expectancy** is what an average close is
 * worth. They come apart exactly where the interesting failures live — a book that wins
 * four closes in five and still bleeds is a stop-discipline problem, and no single
 * number shows it. The raw series stays visible under the mean because a 60% window
 * built from a coin flip and one built from a steady hand draw the same curve.
 */
export function RollingDiscipline({
  closeSet,
  note,
  loaded,
}: RollingDisciplineProps) {
  const [metric, setMetric] = React.useState<Metric>("win")

  const series = React.useMemo(() => {
    if (closeSet == null) return null
    const closes = closeSet.closes
    const data: RollingPoint[] = closes.map((c) => ({
      x: c.closedTs,
      value: metric === "win" ? (c.net > 0 ? 1 : 0) : c.net,
    }))
    const tail = data.slice(-WINDOW)
    const current =
      tail.length === 0
        ? null
        : tail.reduce((sum, d) => sum + d.value, 0) / tail.length
    return {
      data,
      current,
      span: tail.length,
      total: closes.length,
      unpaired: closeSet.unpaired,
    }
  }, [closeSet, metric])

  const isWin = metric === "win"
  const format = isWin ? pct : usd

  return (
    <SectionCard
      action={
        <Segmented
          label="Rolling metric"
          onChange={setMetric}
          options={METRICS}
          size="sm"
          value={metric}
        />
      }
      icon={ChartAverageIcon}
      title="Rolling discipline"
    >
      {!loaded || series == null ? (
        <div className="flex flex-col gap-4">
          <Skeleton className="h-[220px] w-full rounded-md" />
          <Skeleton className="h-4 w-2/3 rounded-md" />
        </div>
      ) : note != null ? (
        <StateUnavailable note={note} />
      ) : series.total === 0 ? (
        <FeedEmpty
          hint={
            series.unpaired > 0
              ? `${series.unpaired} closes in the window opened before it, so none of them pair with an entry. The trailing mean starts at the next close.`
              : "The trailing mean starts as soon as the first position closes."
          }
          title="Nothing has closed yet"
        />
      ) : (
        <>
          <RollingLine
            ariaLabel={`Trailing ${WINDOW}-close ${isWin ? "win rate" : "expectancy"} over ${series.total} closes`}
            data={series.data}
            format={format}
            height={220}
            refLine={isWin ? 0.5 : 0}
            window={WINDOW}
            xFormat={utcStamp}
          />
          <div className="mt-3 border-t border-border pt-3 text-[13px] text-muted-foreground">
            {isWin ? "Win rate" : "Expectancy"}
            {" over the last "}
            <span className="font-mono tabular-nums">{series.span}</span>
            {" closes "}
            <span
              className={cn(
                "font-mono font-medium tabular-nums",
                isWin
                  ? series.current != null && series.current >= 0.5
                    ? "text-long-ink"
                    : "text-short-ink"
                  : pnlInk(series.current)
              )}
            >
              {series.current == null ? "—" : format(series.current)}
            </span>
            {" · "}
            <span className="font-mono tabular-nums">{series.total}</span>
            {" closes in the window"}
          </div>
          <div className="mt-1.5 text-[11.5px] leading-4 text-muted-foreground">
            {isWin
              ? "A win is net above zero after both fees — a scratch is not a win. The dashed line is the coin flip."
              : "Mean net per close after both fees. The dashed line is break-even; the faint series is each close on its own."}
          </div>
        </>
      )}
    </SectionCard>
  )
}
