"use client"

import * as React from "react"
import { ChartColumnIcon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { usd } from "@/lib/format"
import { dailyNetPnl } from "@/lib/stats"
import { BarChart, type BarChartDatum } from "@/components/blocks/bar-chart"
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

const DAYS = 30
const DAY_MS = 86_400_000
const CHART_H = 168

function utcDate(dayIndex: number): string {
  return new Date(dayIndex * DAY_MS).toISOString().slice(0, 10)
}

/**
 * Net PnL per UTC day for the last 30 days, as signed bars with the running
 * total over them.
 *
 * This was the signature dot matrix. A matrix quantises a value into filled
 * cells against a shared max, so a $38 day and a $4 day differed by a couple of
 * squares and the month's trajectory — the number the desk is actually judged
 * on — could not be drawn at all. Bars carry magnitude on an axis; the
 * cumulative line carries where those days left the book.
 */
export function DailyPnlCard({
  trades,
  asOf,
  loading,
  className,
}: {
  trades: Trade[]
  asOf: number | null
  loading: boolean
  className?: string
}) {
  const model = React.useMemo(() => {
    if (asOf == null) return null
    const byDay = new Map(dailyNetPnl(trades).map((d) => [d.date, d]))
    const endDay = Math.floor(asOf / DAY_MS)
    const bars: BarChartDatum[] = []
    let net = 0
    let best = 0
    let worst = 0
    let green = 0
    let active = 0

    // Every one of the 30 days is emitted, traded or not: a gap in the bar row
    // is itself information, and dropping quiet days would compress the x-axis
    // into a lie about how often the desk actually fires.
    for (let i = DAYS - 1; i >= 0; i--) {
      const date = utcDate(endDay - i)
      const day = byDay.get(date)
      const value = day?.net ?? 0
      net += value
      if (value > best) best = value
      if (value < worst) worst = value
      if (value > 0) green += 1
      if (day) active += 1
      bars.push({ x: date.slice(5), value })
    }

    return { bars, net, best, worst, green, active }
  }, [trades, asOf])

  return (
    <SectionCard
      action={<CardNote>30d</CardNote>}
      className={className}
      icon={ChartColumnIcon}
      title="Daily net PnL"
    >
      {loading ? (
        <Skeleton className="w-full rounded-md" style={{ height: CHART_H }} />
      ) : model == null ? (
        <p
          className="grid place-items-center text-[13px] text-muted-foreground"
          style={{ height: CHART_H }}
        >
          No desk activity recorded yet.
        </p>
      ) : (
        <>
          <BarChart
            ariaLabel={`Net PnL for each of the last ${DAYS} UTC days, with the running total. ${usd(model.net)} over the window, ${model.green} green days of ${model.active} traded.`}
            cumulative
            data={model.bars}
            format={usd}
            height={CHART_H}
          />
          <ChartLegend>
            <LegendDot color="var(--long)" label="Up day" />
            <LegendDot color="var(--short)" label="Down day" />
            <LegendLine color="var(--accent-orange)" label="Cumulative" />
          </ChartLegend>
          <StatGrid>
            <MicroStat
              label="Net 30d"
              tone={pnlTone(model.net)}
              value={usd(model.net)}
            />
            <MicroStat
              label="Green days"
              tone="muted"
              value={`${model.green}/${model.active}`}
            />
            <MicroStat label="Best day" tone="long" value={usd(model.best)} />
            <MicroStat
              label="Worst day"
              tone="short"
              value={usd(model.worst)}
            />
          </StatGrid>
        </>
      )}
    </SectionCard>
  )
}
