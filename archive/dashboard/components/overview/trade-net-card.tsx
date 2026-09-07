"use client"

import * as React from "react"
import { ChartHistogramIcon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { usd } from "@/lib/format"
import { isClose, netPnl } from "@/lib/stats"
import { Histogram } from "@/components/blocks/histogram"
import { SectionCard } from "@/components/blocks/section-card"
import { Skeleton } from "@/components/ui/skeleton"
import {
  CardNote,
  ChartLegend,
  LegendDot,
  LegendLine,
  MicroStat,
  pnlTone,
  StatGrid,
} from "@/components/overview/micro"

const CHART_H = 168

/**
 * Distribution of net result per close — the shape a win rate hides.
 *
 * A 40% win rate is a good desk or a dead one depending on whether the winners
 * are 2R and the losers are 1R, and no headline number says which. Every close
 * on record is binned (not a window): shape needs samples, and truncating to
 * the last 30 days could only ever remove them.
 */
export function TradeNetCard({
  trades,
  loading,
  className,
}: {
  trades: Trade[]
  loading: boolean
  className?: string
}) {
  const model = React.useMemo(() => {
    const nets = trades.filter(isClose).map(netPnl)
    if (nets.length === 0) return null

    let winSum = 0
    let winCount = 0
    let lossSum = 0
    let lossCount = 0
    let total = 0
    for (const v of nets) {
      total += v
      if (v > 0) {
        winSum += v
        winCount += 1
      } else if (v < 0) {
        lossSum += v
        lossCount += 1
      }
    }

    const avgWin = winCount > 0 ? winSum / winCount : null
    const avgLoss = lossCount > 0 ? lossSum / lossCount : null
    // Payoff in R: the average winner measured in average losers. Undefined
    // until both tails exist — one-sided samples have no ratio to report.
    const payoff =
      avgWin != null && avgLoss != null ? avgWin / Math.abs(avgLoss) : null

    return {
      nets,
      winCount,
      lossCount,
      avgWin,
      avgLoss,
      payoff,
      expectancy: total / nets.length,
    }
  }, [trades])

  return (
    <SectionCard
      action={
        loading || model == null ? null : (
          <CardNote>
            {model.nets.length} {model.nets.length === 1 ? "close" : "closes"}
          </CardNote>
        )
      }
      className={className}
      icon={ChartHistogramIcon}
      title="Net per close"
    >
      {loading ? (
        <Skeleton className="w-full rounded-md" style={{ height: CHART_H }} />
      ) : model == null ? (
        <p
          className="grid place-items-center text-[13px] text-muted-foreground"
          style={{ height: CHART_H }}
        >
          No closes recorded yet.
        </p>
      ) : (
        <>
          <Histogram
            ariaLabel={`Distribution of net result across ${model.nets.length} closes: ${model.winCount} winners averaging ${usd(model.avgWin)}, ${model.lossCount} losers averaging ${usd(model.avgLoss)}.`}
            format={usd}
            height={CHART_H}
            highlightZero
            values={model.nets}
          />
          <ChartLegend>
            <LegendDot color="var(--long)" label="Winners" />
            <LegendDot color="var(--short)" label="Losers" />
            <LegendLine
              color="var(--muted-foreground)"
              dashed
              label="Break even"
            />
          </ChartLegend>
          <StatGrid>
            <MicroStat label="Avg win" tone="long" value={usd(model.avgWin)} />
            <MicroStat
              label="Avg loss"
              tone="short"
              value={usd(model.avgLoss)}
            />
            <MicroStat
              label="Payoff"
              tone="muted"
              value={model.payoff == null ? "—" : `${model.payoff.toFixed(2)}R`}
            />
            <MicroStat
              label="Expectancy"
              tone={pnlTone(model.expectancy)}
              value={usd(model.expectancy)}
            />
          </StatGrid>
        </>
      )}
    </SectionCard>
  )
}
