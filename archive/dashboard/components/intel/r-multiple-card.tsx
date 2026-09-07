"use client"

import * as React from "react"
import { ChartScatterIcon } from "@hugeicons/core-free-icons"

import { cn } from "@/lib/utils"
import {
  ScatterPlot,
  type ScatterPoint,
} from "@/components/blocks/scatter-plot"
import { SectionCard } from "@/components/blocks/section-card"
import { Skeleton } from "@/components/ui/skeleton"
import { HUE_SOLID, HUE_VAR, type Hue } from "@/components/blocks/icon-chip"
import { FeedEmpty, StateUnavailable } from "@/components/intel/feed-state"
import { pnlInk, signedUsd, utcStamp } from "@/components/intel/intel-utils"
import { MetaChip } from "@/components/intel/meta-chip"
import {
  type Close,
  type CloseSet,
  type ExitKind,
  formatR,
  KIND_LABEL,
} from "@/components/intel/r-multiple"

/** tp pays, sl costs, a veto is a risk decision, everything else is bookkeeping. */
const KIND_HUE: Record<ExitKind, Hue> = {
  tp: "long",
  sl: "short",
  veto_close: "warning",
  other: "neutral",
}

const ORDER: ExitKind[] = ["tp", "sl", "veto_close", "other"]

/** Break-even and a full stop — the only two levels an R multiple is read against. */
const GUIDES = { y: [0, -1] }

const DOT_MIN = 3.5
const DOT_SPAN = 5

function Legend({ counts }: { counts: Record<ExitKind, number> }) {
  const present = ORDER.filter((kind) => counts[kind] > 0)
  if (present.length === 0) return null
  return (
    <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5">
      {present.map((kind) => (
        <span className="inline-flex items-center gap-2" key={kind}>
          <span
            className={cn(
              "size-1.5 shrink-0 rounded-full",
              HUE_SOLID[KIND_HUE[kind]]
            )}
          />
          <span className="text-[11.5px] text-muted-foreground">
            {KIND_LABEL[kind]}
          </span>
          <span className="font-mono text-[11.5px] font-medium tabular-nums">
            {counts[kind]}
          </span>
        </span>
      ))}
    </div>
  )
}

export type RMultipleCardProps = {
  /** null until the first poll settles */
  closeSet: CloseSet | null
  /** set when the endpoints failed — renders the neutral row */
  note: string | null
  loaded: boolean
}

/**
 * Every close in the window as one dot: when it closed against what it made, measured
 * in units of the risk it took. The chart the exit-mix bar and the pnl total cannot
 * draw — those say *how* positions leave and *how much* they made, never whether the
 * money made was worth the money risked.
 *
 * A ScatterPlot rather than anything time-bucketed: R is a per-close property, and
 * bucketing closes into days would average away the exact spread the chart exists to
 * show. The two guides carry the whole reading — dots above `0` paid, dots at `−1` are
 * clean stop-outs, dots *below* `−1` are the ones that cost more than they were ever
 * risking, which is the only outright failure this desk can commit.
 *
 * Dot area scales with the position's risk in dollars, so a large loss taken on a small
 * stop is visibly not the same event as the same loss on a large one. Derivation and
 * its limits: `r-multiple.ts`.
 */
export function RMultipleCard({ closeSet, note, loaded }: RMultipleCardProps) {
  const view = React.useMemo(() => {
    if (closeSet == null) return null
    const plotted = closeSet.closes.filter(
      (c): c is Close & { r: number; risk: number } =>
        c.r != null && c.risk != null
    )
    const maxRisk = plotted.reduce((hi, c) => Math.max(hi, c.risk), 0)
    const points: ScatterPoint[] = plotted.map((c) => ({
      x: c.closedTs,
      y: c.r,
      // area ∝ risk, so the radius is its square root
      r: maxRisk > 0 ? DOT_MIN + DOT_SPAN * Math.sqrt(c.risk / maxRisk) : 5,
      color: HUE_VAR[KIND_HUE[c.kind]],
      label: `${c.market} · ${KIND_LABEL[c.kind]} · ${signedUsd(c.net)}`,
    }))
    const counts: Record<ExitKind, number> = {
      tp: 0,
      sl: 0,
      veto_close: 0,
      other: 0,
    }
    for (const c of plotted) counts[c.kind] += 1
    const meanR =
      plotted.length === 0
        ? null
        : plotted.reduce((sum, c) => sum + c.r, 0) / plotted.length
    return {
      points,
      counts,
      meanR,
      plotted: plotted.length,
      unresolved: closeSet.closes.length - plotted.length,
      unpaired: closeSet.unpaired,
    }
  }, [closeSet])

  return (
    <SectionCard
      action={
        view && view.plotted > 0 ? (
          <MetaChip label="plotted">{view.plotted}</MetaChip>
        ) : null
      }
      icon={ChartScatterIcon}
      title="R-multiple timeline"
    >
      {!loaded || view == null ? (
        <div className="flex flex-col gap-4">
          <Skeleton className="h-[280px] w-full rounded-md" />
          <Skeleton className="h-4 w-2/3 rounded-md" />
        </div>
      ) : note != null ? (
        <StateUnavailable note={note} />
      ) : view.plotted === 0 ? (
        <FeedEmpty
          hint={
            view.unresolved > 0
              ? `${view.unresolved} closes in the window, none with a recoverable stop distance. Vetoed closes resolve once their bracket replay lands.`
              : view.unpaired > 0
                ? `${view.unpaired} closes in the window opened before it, so the risk they carried cannot be measured from these fills.`
                : "Nothing has closed in the fetched window yet."
          }
          title="No close carries an R multiple"
        />
      ) : (
        <>
          <ScatterPlot
            ariaLabel={`R multiple of ${view.plotted} closes over time, against guides at zero and minus one R`}
            guides={GUIDES}
            height={280}
            points={view.points}
            xFormat={utcStamp}
            xLabel="Closed (UTC)"
            yFormat={formatR}
            yLabel="R multiple"
          />
          <Legend counts={view.counts} />
          <div className="mt-3 border-t border-border pt-3 text-[13px] text-muted-foreground">
            {"Mean "}
            <span
              className={cn(
                "font-mono font-medium tabular-nums",
                pnlInk(view.meanR)
              )}
            >
              {view.meanR == null ? "—" : formatR(view.meanR)}
            </span>
            {" per close · dot area scales with dollars risked"}
            {view.unresolved > 0 ? (
              <>
                {" · "}
                <span className="font-mono tabular-nums">
                  {view.unresolved}
                </span>
                {" close"}
                {view.unresolved === 1 ? "" : "s"}
                {" without a recoverable stop"}
              </>
            ) : null}
            {view.unpaired > 0 ? (
              <>
                {" · "}
                <span className="font-mono tabular-nums">{view.unpaired}</span>
                {" opened before the window"}
              </>
            ) : null}
          </div>
          <div className="mt-1.5 text-[11.5px] leading-4 text-muted-foreground">
            {
              "R = net ÷ (|entry − stop| × size). Stops come from the stop fill, from the "
            }
            {
              "take-profit fill halved (targets sit at 2R), or from a replayed bracket for "
            }
            {
              "vetoed closes. Net carries both fees, so a clean stop-out sits just under "
            }
            <span className="font-mono">−1R</span>.
          </div>
        </>
      )}
    </SectionCard>
  )
}
