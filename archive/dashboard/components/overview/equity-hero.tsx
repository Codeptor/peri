"use client"

import * as React from "react"
import { ChartLineData01Icon } from "@hugeicons/core-free-icons"

import type { EquityPoint, Trade } from "@/lib/api"
import { usd } from "@/lib/format"
import { KILL_SWITCH_PCT } from "@/lib/risk"
import { DeltaChip } from "@/components/blocks/delta-chip"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented } from "@/components/blocks/segmented"
import {
  TrendArea,
  type TrendBand,
  type TrendMarker,
  type TrendPoint,
  type TrendRefLine,
} from "@/components/blocks/trend-area"
import { Skeleton } from "@/components/ui/skeleton"
import { actionLabel, fillMark, fillNet } from "@/components/overview/fills"
import { MicroStat, StatStrip } from "@/components/overview/micro"
import {
  RANGE_MS,
  RANGE_OPTIONS,
  type RangeKey,
} from "@/components/overview/range"

const CHART_H = 232

function utcDayStart(ts: number): number {
  const d = new Date(ts)
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate())
}

/** Legend swatch: filled dot for a resolved fill, ring for one still open. */
function LegendMark({
  color,
  hollow = false,
  label,
}: {
  color: string
  hollow?: boolean
  label: React.ReactNode
}) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className="size-2 shrink-0 rounded-full"
        style={
          hollow
            ? { boxShadow: `inset 0 0 0 1.5px ${color}` }
            : { backgroundColor: color }
        }
      />
      <span className="text-xs text-muted-foreground">{label}</span>
    </span>
  )
}

export type EquityHeroProps = {
  equity: EquityPoint[]
  /** every fetched fill — the ones inside the window become curve markers */
  trades: Trade[]
  loading: boolean
  live: boolean
  className?: string
}

/**
 * Hero equity curve — orange gradient area, range-filtered, live end dot, plus
 * three overlays that turn the line into a risk read: fill markers (what the desk
 * did and what it paid), drawdown shading (every stretch under the running peak),
 * and the kill floor the daemon halts at.
 */
export function EquityHero({
  equity,
  trades,
  loading,
  live,
  className,
}: EquityHeroProps) {
  const [range, setRange] = React.useState<RangeKey>("24h")

  // Windows anchor to the newest sample, not wall-clock: a paused or replayed
  // desk still renders its last hour instead of an empty chart.
  const points = React.useMemo<TrendPoint[]>(() => {
    if (equity.length === 0) return []
    const span = RANGE_MS[range]
    if (span == null) return equity.map((p) => ({ ts: p.ts, value: p.equity }))
    const from = equity[equity.length - 1].ts - span
    return equity
      .filter((p) => p.ts >= from)
      .map((p) => ({ ts: p.ts, value: p.equity }))
  }, [equity, range])

  const bounds = React.useMemo(() => {
    if (points.length === 0) return null
    let lo = Number.POSITIVE_INFINITY
    let hi = Number.NEGATIVE_INFINITY
    for (const p of points) {
      if (p.value < lo) lo = p.value
      if (p.value > hi) hi = p.value
    }
    const open = points[0].value
    const last = points[points.length - 1].value
    return {
      open,
      last,
      lo,
      hi,
      changePct: open === 0 ? null : ((last - open) / open) * 100,
    }
  }, [points])

  // Fills inside the window only. A fill from before it is not clamped onto the
  // left edge — it would read as an event that happened in view when it did not.
  const markers = React.useMemo<TrendMarker[]>(() => {
    if (points.length === 0 || trades.length === 0) return []
    const from = points[0].ts
    const to = points[points.length - 1].ts
    return trades
      .filter((t) => t.ts >= from && t.ts <= to)
      .sort((a, b) => a.ts - b.ts)
      .map((t) => {
        const mark = fillMark(t.action)
        return {
          ts: t.ts,
          color: mark.color,
          hollow: mark.hollow,
          title: `${t.market} · ${actionLabel(t.action)}`,
          detail: fillNet(t),
        }
      })
  }, [points, trades])

  // Every stretch where equity sits under its own running peak, from the peak
  // itself to the sample that reclaims it (or to the right edge, still underwater).
  const { bands, maxDrawdownPct } = React.useMemo(() => {
    if (points.length < 2)
      return { bands: [] as TrendBand[], maxDrawdownPct: 0 }
    const out: TrendBand[] = []
    let deepest = 0
    let peak = points[0].value
    let peakTs = points[0].ts
    let start: number | null = null
    let trough = Number.POSITIVE_INFINITY

    const close = (to: number) => {
      if (start == null) return
      const depth = peak > 0 ? ((peak - trough) / peak) * 100 : 0
      if (depth > deepest) deepest = depth
      out.push({
        from: start,
        to,
        color: "var(--short)",
        label: `Drawdown ${depth.toFixed(2)}% · ${usd(peak)} → ${usd(trough)}`,
      })
      start = null
      trough = Number.POSITIVE_INFINITY
    }

    for (const p of points) {
      if (p.value >= peak) {
        close(p.ts)
        peak = p.value
        peakTs = p.ts
      } else {
        if (start == null) start = peakTs
        if (p.value < trough) trough = p.value
      }
    }
    close(points[points.length - 1].ts)
    return { bands: out, maxDrawdownPct: deepest }
  }, [points])

  // kestreld stamps `day_open` when the UTC day rolls, so the first sample of the
  // anchor day is the same number the daemon measures its kill floor against.
  const killFloor = React.useMemo(() => {
    if (equity.length === 0) return null
    const dayStart = utcDayStart(equity[equity.length - 1].ts)
    const dayOpen = equity.find((p) => p.ts >= dayStart)?.equity
    if (dayOpen == null || dayOpen <= 0) return null
    return dayOpen * (1 - KILL_SWITCH_PCT / 100)
  }, [equity])

  // TrendArea drops a reference line that falls outside the y-domain, so on a
  // healthy day the line simply is not drawn — the legend still states the level.
  const refLines = React.useMemo<TrendRefLine[]>(
    () =>
      killFloor == null
        ? []
        : [
            {
              value: killFloor,
              // the ink token, not the raw hue: this colour paints the label too
              label: `Kill floor ${usd(killFloor)}`,
              color: "var(--short-ink)",
            },
          ],
    [killFloor]
  )

  return (
    <SectionCard
      action={
        <>
          <DeltaChip pct={bounds?.changePct ?? null} />
          <Segmented
            label="Equity range"
            onChange={setRange}
            options={RANGE_OPTIONS}
            size="sm"
            value={range}
          />
        </>
      }
      className={className}
      icon={ChartLineData01Icon}
      title="Equity curve"
    >
      {loading ? (
        <Skeleton className="w-full rounded-md" style={{ height: CHART_H }} />
      ) : (
        <TrendArea
          axis
          bands={bands}
          color="var(--accent-orange)"
          data={points}
          format={usd}
          height={CHART_H}
          live={live && points.length > 0}
          markers={markers}
          refLines={refLines}
        />
      )}

      <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5">
        <LegendMark color="var(--accent-orange)" hollow label="Entry" />
        <LegendMark color="var(--long)" label="Take profit" />
        <LegendMark color="var(--short)" label="Stop loss" />
        <LegendMark color="var(--warning)" label="Veto close" />
        <span className="inline-flex items-center gap-1.5">
          <span className="h-2 w-3.5 shrink-0 rounded-[2px] bg-short/20" />
          <span className="text-xs text-muted-foreground">Drawdown</span>
        </span>
        {killFloor == null ? null : (
          <span className="inline-flex items-center gap-1.5">
            <span className="h-0 w-3.5 shrink-0 border-t border-dashed border-short" />
            <span className="text-xs text-muted-foreground">
              Kill floor <span className="font-mono">{usd(killFloor)}</span>
            </span>
          </span>
        )}
        <span className="ml-auto text-xs text-muted-foreground">
          <span className="font-mono">{markers.length}</span> fills in window
        </span>
      </div>

      <StatStrip>
        <MicroStat label="Open" value={usd(bounds?.open)} />
        <MicroStat label="High" value={usd(bounds?.hi)} />
        <MicroStat label="Low" value={usd(bounds?.lo)} />
        <MicroStat label="Last" value={usd(bounds?.last)} />
        <MicroStat
          label="Max drawdown"
          tone={maxDrawdownPct > 0 ? "short" : "muted"}
          value={`${maxDrawdownPct.toFixed(2)}%`}
        />
        <MicroStat
          className="ml-auto text-right"
          label="Samples"
          tone="muted"
          value={String(points.length)}
        />
      </StatStrip>
    </SectionCard>
  )
}
