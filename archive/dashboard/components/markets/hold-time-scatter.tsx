"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import { HUE_SOLID, HUE_VAR } from "@/components/blocks/icon-chip"
import {
  ScatterPlot,
  type ScatterPoint,
} from "@/components/blocks/scatter-plot"

import {
  EXIT_HUE,
  EXIT_KINDS,
  EXIT_LABEL,
  exitKindCounts,
  holdLabel,
  type HoldSample,
} from "./pnl-data"
import { EmptyNote } from "./shared"

export type HoldTimeScatterProps = {
  sample: HoldSample
  /** size of the fill window the sample was derived from — the caveat needs it */
  fillWindow: number
  loading: boolean
  height?: number
}

/**
 * Every closed position as one dot: how long it was held against what it paid,
 * coloured by how it ended.
 *
 * The question it answers — "do the winners come from patience or from getting
 * out fast?" — needs two continuous axes per position. No bucketed form can say
 * it: a matrix or bar chart would have to collapse the holds into ranges first,
 * and that is exactly the shape being interrogated.
 */
export function HoldTimeScatter({
  sample,
  fillWindow,
  loading,
  height = 300,
}: HoldTimeScatterProps) {
  const points = React.useMemo<ScatterPoint[]>(
    () =>
      sample.points.map((p) => ({
        x: p.hours,
        y: p.net,
        r: 5,
        color: HUE_VAR[EXIT_HUE[p.kind]],
        label: `${p.market} · ${EXIT_LABEL[p.kind]}`,
      })),
    [sample.points]
  )

  const counts = React.useMemo(
    () => exitKindCounts(sample.points),
    [sample.points]
  )

  if (loading && sample.points.length === 0) {
    return <Skeleton className="w-full rounded-md" style={{ height }} />
  }

  const caveats: string[] = []
  if (sample.orphans > 0) {
    caveats.push(
      `${sample.orphans} close${sample.orphans === 1 ? " has" : "s have"} no opening fill in it`
    )
  }
  if (sample.running > 0) {
    caveats.push(
      `${sample.running} position${sample.running === 1 ? " is" : "s are"} still open`
    )
  }

  if (sample.points.length === 0) {
    return (
      <EmptyNote>
        No round trip closed inside the last{" "}
        <span className="font-mono">{fillWindow}</span> fills — hold time only
        exists once one fill log entry opens a position and another closes it.
        {caveats.length > 0 ? ` (${caveats.join(", ")}.)` : null}
      </EmptyNote>
    )
  }

  return (
    <div className="flex flex-col gap-3">
      <ScatterPlot
        ariaLabel={`Hold time against net result, ${sample.points.length} closed positions`}
        guides={{ y: [0] }}
        height={height}
        points={points}
        xFormat={holdLabel}
        xLabel="Hold time"
        yFormat={usd}
        yLabel="Net"
      />

      <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
        {EXIT_KINDS.filter((kind) => (counts.get(kind) ?? 0) > 0).map(
          (kind) => (
            <span
              className="flex items-center gap-2 text-[13px] text-muted-foreground"
              key={kind}
            >
              <span
                className={cn(
                  "size-2 shrink-0 rounded-full",
                  HUE_SOLID[EXIT_HUE[kind]]
                )}
              />
              {EXIT_LABEL[kind]}
              <span className="font-mono text-foreground">
                {counts.get(kind) ?? 0}
              </span>
            </span>
          )
        )}
      </div>

      <p className="text-xs leading-5 text-muted-foreground">
        Derived here, not served: the API logs fills, so each dot is a position
        rebuilt from its own fills — first open to last close, net of every fee
        it paid. Only the last <span className="font-mono">{fillWindow}</span>{" "}
        fills are read, so a position whose opening fill has scrolled out cannot
        be measured and the sample skews short at that edge
        {caveats.length > 0 ? ` (${caveats.join(", ")})` : null}. Hold time is
        fill-to-fill — the analyst&apos;s decision and the router sit outside
        it.
      </p>
    </div>
  )
}
