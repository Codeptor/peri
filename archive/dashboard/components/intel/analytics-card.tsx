"use client"

import * as React from "react"
import { ArrowDown01Icon, ChartAnalysisIcon } from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import type { Analytics, MarketRecord } from "@/lib/api"
import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { BarChart, type BarChartDatum } from "@/components/blocks/bar-chart"
import { DeltaChip } from "@/components/blocks/delta-chip"
import { SectionCard } from "@/components/blocks/section-card"
import {
  SegmentBar,
  type SegmentBarSegment,
} from "@/components/blocks/segment-bar"
import { Skeleton } from "@/components/ui/skeleton"
import { CounterfactualDetail } from "@/components/intel/counterfactual-list"
import { StateUnavailable } from "@/components/intel/feed-state"
import { pnlInk, signedUsd } from "@/components/intel/intel-utils"
import { MetaChip } from "@/components/intel/meta-chip"

/** Markets shown in the strip — ranked by how much pnl they moved, either way. */
const STRIP_MARKETS = 6

/** Matches `BarChart`'s right-edge label gutter so each figure sits under its own bar. */
const BAR_GUTTER = "pr-11"

function SubLabel({ children }: { children: React.ReactNode }) {
  return <div className="text-[11.5px] text-muted-foreground">{children}</div>
}

function MarketNetChip({ record }: { record: MarketRecord }) {
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-full bg-cell px-2.5 py-0.5 text-[11.5px] leading-5"
      title={`${record.trades} fills · ${usd(record.fees)} fees`}
    >
      <span className="font-mono font-medium">{record.market}</span>
      <span className={cn("font-mono tabular-nums", pnlInk(record.net_pnl))}>
        {signedUsd(record.net_pnl)}
      </span>
    </span>
  )
}

export type AnalyticsCardProps = {
  analytics: Analytics | null
  /** set when the endpoint failed — renders the neutral row */
  note: string | null
  /** false until the first poll settles — separates "loading" from "unavailable" */
  loaded: boolean
}

/** Sans label + chevron, right-aligned — the one disclosure control on this card. */
function DisclosureButton({
  expanded,
  onToggle,
}: {
  expanded: boolean
  onToggle: () => void
}) {
  return (
    <button
      aria-expanded={expanded}
      className="ml-auto inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-[11.5px] text-muted-foreground transition-colors hover:text-foreground"
      onClick={onToggle}
      type="button"
    >
      {expanded ? "Hide detail" : "Detail"}
      <HugeiconsIcon
        className={cn("transition-transform", expanded && "rotate-180")}
        icon={ArrowDown01Icon}
        size={13}
        strokeWidth={2}
      />
    </button>
  )
}

/**
 * Decision quality: does conviction pay, which markets carry the book, what the
 * reviewer's early closes actually cost, and how positions leave the book.
 *
 * Conviction vs outcome is a signed BarChart, not the table it used to be. The question
 * is comparative — "does the top bucket actually beat the bottom one" — and a table of
 * right-aligned dollars makes the reader do that comparison in their head, three signed
 * numbers at a time. Bars around zero answer it before it is asked; the counts and win
 * rates the table also carried survive underneath, one compact line per bucket, because
 * a net with no `n` behind it is not evidence.
 *
 * Exit mix renders as a SegmentBar at rest rather than a DotMatrix: at half-page
 * card width a 4-series × 14-column matrix competes with the tables around it, and
 * the resting question is the *mix*, not the daily shape — one stacked bar answers
 * it in a glance and carries its own legend. The by-day matrix is not lost, it is
 * demoted: it opens inside the counterfactual detail, where it has the room to read
 * and answers the follow-up question ("when did those vetoes happen?") in place.
 */
export function AnalyticsCard({ analytics, note, loaded }: AnalyticsCardProps) {
  const [expanded, setExpanded] = React.useState(false)

  const topMarkets = React.useMemo(
    () =>
      [...(analytics?.per_market ?? [])]
        .sort((a, b) => Math.abs(b.net_pnl) - Math.abs(a.net_pnl))
        .slice(0, STRIP_MARKETS),
    [analytics]
  )

  // Signed bars around zero: the buckets' only interesting property is which side of
  // break-even each one lands on and by how much, which a column of right-aligned
  // figures made the reader compute for themselves.
  const convictionBars = React.useMemo<BarChartDatum[]>(
    () =>
      (analytics?.conviction_buckets ?? []).map((b) => ({
        x: b.bucket,
        value: b.net_pnl,
      })),
    [analytics]
  )

  const { segments, exits } = React.useMemo(() => {
    const days = analytics?.exit_mix_daily ?? []
    const tp = days.reduce((sum, d) => sum + d.tp, 0)
    const sl = days.reduce((sum, d) => sum + d.sl, 0)
    const veto = days.reduce((sum, d) => sum + d.veto_close, 0)
    const other = days.reduce((sum, d) => sum + d.other, 0)
    const next: SegmentBarSegment[] = [
      { label: "Take profit", count: tp, color: "var(--long)" },
      { label: "Stop loss", count: sl, color: "var(--short)" },
      { label: "Veto close", count: veto, color: "var(--warning)" },
      { label: "Other", count: other, color: "var(--chart-6)" },
    ]
    return { segments: next, exits: tp + sl + veto + other }
  }, [analytics])

  const cf = analytics?.counterfactuals ?? null
  // actual − bracket: positive means closing early BEAT the position's own bracket,
  // which is the direction the chip should paint green.
  const edge = cf == null ? 0 : cf.net_actual - cf.net_bracket

  return (
    <SectionCard
      action={analytics ? <MetaChip label="closes">{exits}</MetaChip> : null}
      icon={ChartAnalysisIcon}
      title="Analytics"
    >
      {!loaded ? (
        <div className="flex flex-col gap-4">
          <Skeleton className="h-36 w-full rounded-md" />
          <Skeleton className="h-4 w-3/4 rounded-md" />
          <Skeleton className="h-2.5 w-full rounded-full" />
          <Skeleton className="h-20 w-full rounded-md" />
        </div>
      ) : analytics == null ? (
        <StateUnavailable note={note} />
      ) : (
        <>
          <SubLabel>Conviction vs outcome</SubLabel>
          <BarChart
            ariaLabel="Net pnl by conviction bucket"
            className="mt-2"
            data={convictionBars}
            format={signedUsd}
            height={150}
            xLabels="all"
          />
          <div className={cn("mt-1 grid grid-cols-3 gap-2 pl-0.5", BAR_GUTTER)}>
            {analytics.conviction_buckets.map((b) => (
              <div
                className="text-center text-[11.5px] leading-4"
                key={b.bucket}
              >
                <span className="font-mono tabular-nums">{b.closes}</span>
                <span className="text-muted-foreground"> closes · </span>
                <span
                  className={cn(
                    "font-mono tabular-nums",
                    b.closes === 0 && "text-muted-foreground"
                  )}
                >
                  {b.closes === 0
                    ? "—"
                    : `${((b.wins / b.closes) * 100).toFixed(0)}%`}
                </span>
                <span className="text-muted-foreground"> win</span>
              </div>
            ))}
          </div>

          <div className="mt-4 border-t border-border pt-3.5">
            <div className="flex items-center gap-2">
              <SubLabel>Veto counterfactual</SubLabel>
              {cf == null ? null : (
                <DisclosureButton
                  expanded={expanded}
                  onToggle={() => setExpanded((prev) => !prev)}
                />
              )}
            </div>
            {cf != null && cf.computed > 0 ? (
              <>
                <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1.5 text-[13px] text-muted-foreground">
                  <span className="font-mono text-foreground tabular-nums">
                    {signedUsd(cf.net_actual)}
                  </span>
                  <span>actual vs</span>
                  <span className="font-mono text-foreground tabular-nums">
                    {signedUsd(cf.net_bracket)}
                  </span>
                  <span>bracket</span>
                  <DeltaChip pct={edge} text={signedUsd(edge)} />
                  <span className="ml-auto font-mono tabular-nums">
                    {cf.computed} computed · {cf.pending} pending
                  </span>
                </div>
                <div className="mt-1.5 text-[11.5px] leading-4 text-muted-foreground">
                  Replayed against 1m candles. A positive edge means closing
                  early beat the position&apos;s own bracket.
                </div>
              </>
            ) : (
              <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1.5 text-[13px] text-muted-foreground">
                <span>No veto close has resolved yet</span>
                <span className="ml-auto font-mono tabular-nums">
                  {cf?.computed ?? 0} computed · {cf?.pending ?? 0} pending
                </span>
              </div>
            )}
            {expanded && cf != null ? (
              <CounterfactualDetail
                counterfactuals={cf}
                exitMix={analytics.exit_mix_daily}
              />
            ) : null}
          </div>

          <div className="mt-3.5 border-t border-border pt-3.5">
            <SubLabel>Per market · top {STRIP_MARKETS} by net</SubLabel>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {topMarkets.length === 0 ? (
                <span className="text-[13px] text-muted-foreground">
                  Nothing traded yet
                </span>
              ) : (
                topMarkets.map((m) => (
                  <MarketNetChip key={m.market} record={m} />
                ))
              )}
            </div>
          </div>

          <div className="mt-3.5 border-t border-border pt-3.5">
            <SubLabel>Exit mix · last 14 days</SubLabel>
            <SegmentBar className="mt-3" segments={segments} />
          </div>
        </>
      )}
    </SectionCard>
  )
}
