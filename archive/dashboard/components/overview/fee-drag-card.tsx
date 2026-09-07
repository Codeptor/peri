"use client"

import * as React from "react"
import { ChartLineData02Icon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  GRID_STROKE,
  monotonePath,
  padDomain,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"
import { SectionCard } from "@/components/blocks/section-card"
import { Skeleton } from "@/components/ui/skeleton"
import {
  CardNote,
  ChartLegend,
  LegendLine,
  MicroStat,
  pnlTone,
  StatGrid,
} from "@/components/overview/micro"

const CHART_H = 168
const PAD_TOP = 14
const PAD_RIGHT = 50
const PAD_BOTTOM = 18
const PAD_LEFT = 3
/** Minimum vertical gap between the two right-edge labels before they are pushed apart. */
const LABEL_GAP = 11

type CumPoint = { ts: number; gross: number; fees: number }

function stamp(ts: number): string {
  return new Date(ts).toISOString().slice(5, 16).replace("T", " ")
}

function dayStamp(ts: number): string {
  return new Date(ts).toISOString().slice(5, 10)
}

/**
 * Two cumulative dollar series on ONE y-domain: gross realized, and the fees
 * paid to earn it. A shared scale is the entire point — the vertical gap
 * between the curves at any instant *is* the net, so the comparison has to be
 * readable as distance on the chart rather than inferred from two scales.
 */
function FeeDragChart({
  points,
  height = CHART_H,
  className,
}: {
  points: CumPoint[]
  height?: number
  className?: string
}) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()
  const gradientId = `fee-drag-${React.useId().replace(/[^a-zA-Z0-9]/g, "")}`

  const geom = React.useMemo(() => {
    if (width <= 0 || points.length === 0) return null
    const innerW = Math.max(1, width - PAD_LEFT - PAD_RIGHT)
    const innerH = Math.max(1, height - PAD_TOP - PAD_BOTTOM)

    // Zero is always in the domain: fees climb from it and gross is measured
    // against it, so a window that never crosses zero must still show where it is.
    let lo = 0
    let hi = 0
    for (const p of points) {
      if (p.gross < lo) lo = p.gross
      if (p.gross > hi) hi = p.gross
      if (p.fees > hi) hi = p.fees
    }
    const [domLo, domHi] = padDomain(lo, hi)

    const xOf = (i: number) =>
      points.length === 1
        ? PAD_LEFT + innerW / 2
        : PAD_LEFT + (i * innerW) / (points.length - 1)
    const yOf = (v: number) =>
      PAD_TOP + (1 - (v - domLo) / (domHi - domLo)) * innerH

    const grossPts = points.map((p, i): [number, number] => [
      xOf(i),
      yOf(p.gross),
    ])
    const feePts = points.map((p, i): [number, number] => [xOf(i), yOf(p.fees)])
    const zeroY = yOf(0)
    const feeLine = monotonePath(feePts)
    const lastX = feePts[feePts.length - 1][0]

    // Labels are pushed apart rather than allowed to overlap: early in a
    // window the two curves sit on top of each other, which is exactly when
    // knowing which number belongs to which series matters most.
    let grossLabelY = grossPts[grossPts.length - 1][1]
    let feeLabelY = feePts[feePts.length - 1][1]
    const spread = feeLabelY - grossLabelY
    if (Math.abs(spread) < LABEL_GAP) {
      const push = (LABEL_GAP - Math.abs(spread)) / 2
      const dir = spread >= 0 ? 1 : -1
      grossLabelY -= push * dir
      feeLabelY += push * dir
    }

    return {
      innerW,
      innerH,
      grossPts,
      feePts,
      grossLine: monotonePath(grossPts),
      feeLine,
      feeArea: `${feeLine} L ${lastX} ${zeroY} L ${feePts[0][0]} ${zeroY} Z`,
      zeroY,
      grossLabelY,
      feeLabelY,
    }
  }, [points, width, height])

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!geom) return
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left
    let best = 0
    let bestDist = Number.POSITIVE_INFINITY
    for (let i = 0; i < geom.grossPts.length; i++) {
      const dist = Math.abs(geom.grossPts[i][0] - x)
      if (dist < bestDist) {
        bestDist = dist
        best = i
      }
    }
    show(
      best,
      geom.grossPts[best][0],
      Math.min(geom.grossPts[best][1], geom.feePts[best][1])
    )
  }

  const active = tip?.data ?? null
  const point = active != null ? points[active] : null
  const last = points[points.length - 1]

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={hide}
      onPointerMove={onMove}
      ref={containerRef}
      style={{ height }}
    >
      {points.length === 0 ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={`Cumulative gross realized ${usd(last.gross)} against cumulative fees ${usd(last.fees)} across ${points.length} fills.`}
          className="block"
          height={height}
          role="img"
          width={width}
        >
          <defs>
            <linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0%" stopColor="var(--short)" stopOpacity={0.24} />
              <stop offset="100%" stopColor="var(--short)" stopOpacity={0.02} />
            </linearGradient>
          </defs>

          {/* Fees are a stack that only ever grows, so they read as filled area;
              gross wanders and stays a line. */}
          <path d={geom.feeArea} fill={`url(#${gradientId})`} />

          <line
            stroke={GRID_STROKE}
            strokeWidth={1}
            x1={PAD_LEFT}
            x2={PAD_LEFT + geom.innerW}
            y1={geom.zeroY}
            y2={geom.zeroY}
          />

          <path
            d={geom.feeLine}
            fill="none"
            stroke="var(--short)"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={1.75}
          />
          <path
            d={geom.grossLine}
            fill="none"
            stroke="var(--accent-orange)"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2.25}
          />

          {active != null ? (
            <g>
              <line
                stroke="var(--border)"
                strokeWidth={1}
                x1={geom.grossPts[active][0]}
                x2={geom.grossPts[active][0]}
                y1={PAD_TOP}
                y2={PAD_TOP + geom.innerH}
              />
              <circle
                cx={geom.feePts[active][0]}
                cy={geom.feePts[active][1]}
                fill="var(--short)"
                r={3}
                stroke="var(--card)"
                strokeWidth={1.75}
              />
              <circle
                cx={geom.grossPts[active][0]}
                cy={geom.grossPts[active][1]}
                fill="var(--accent-orange)"
                r={3.5}
                stroke="var(--card)"
                strokeWidth={2}
              />
            </g>
          ) : null}

          <g className={AXIS_TICK_CLASS} fontSize={AXIS_TICK_SIZE}>
            {/* Anchored to the frame's right edge, not to the plot's: a
                four-figure total left-anchored after the plot runs off the
                canvas, and a label that grows leftward can never clip. */}
            <text
              fill="var(--accent-orange-ink)"
              textAnchor="end"
              x={width - 2}
              y={geom.grossLabelY + 3}
            >
              {usd(last.gross)}
            </text>
            <text
              fill="var(--short-ink)"
              textAnchor="end"
              x={width - 2}
              y={geom.feeLabelY + 3}
            >
              {usd(last.fees)}
            </text>
            <g fill={AXIS_TICK_FILL}>
              <text textAnchor="start" x={PAD_LEFT} y={height - 4}>
                {dayStamp(points[0].ts)}
              </text>
              {points.length > 2 ? (
                <text
                  textAnchor="middle"
                  x={PAD_LEFT + geom.innerW / 2}
                  y={height - 4}
                >
                  {dayStamp(points[Math.floor(points.length / 2)].ts)}
                </text>
              ) : null}
              <text textAnchor="end" x={PAD_LEFT + geom.innerW} y={height - 4}>
                {dayStamp(last.ts)}
              </text>
            </g>
          </g>
        </svg>
      ) : null}

      {tip && point ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          meta={stamp(point.ts)}
          rows={[
            {
              label: "Gross realized",
              value: usd(point.gross),
              color: "var(--accent-orange-ink)",
            },
            {
              label: "Fees",
              value: `-${usd(point.fees)}`,
              color: "var(--short-ink)",
            },
          ]}
          title="Cumulative net"
          value={usd(point.gross - point.fees)}
          valueColor={
            point.gross - point.fees > 0
              ? "var(--long-ink)"
              : point.gross - point.fees < 0
                ? "var(--short-ink)"
                : undefined
          }
          x={tip.x}
          y={tip.y}
        />
      ) : null}
    </div>
  )
}

/**
 * What the desk earned before costs, against what the venue took.
 *
 * Cumulated over every fill on record rather than a trailing window: a
 * cumulative fee curve restarted at zero 30 days ago understates the fees
 * actually paid, and "fees paid" is the one number here that must not be
 * shaved by the frame it is drawn in.
 */
export function FeeDragCard({
  trades,
  loading,
  className,
}: {
  trades: Trade[]
  loading: boolean
  className?: string
}) {
  const model = React.useMemo(() => {
    if (trades.length === 0) return null
    const sorted = [...trades].sort((a, b) => a.ts - b.ts)
    const points: CumPoint[] = []
    let gross = 0
    let fees = 0
    for (const t of sorted) {
      gross += t.realized_pnl ?? 0
      fees += t.fee
      points.push({ ts: t.ts, gross, fees })
    }
    return {
      points,
      gross,
      fees,
      net: gross - fees,
      // Drag as a share of gross is only meaningful against a positive gross —
      // on a losing book the ratio is arithmetic without a reading.
      drag: gross > 0 ? (fees / gross) * 100 : null,
    }
  }, [trades])

  return (
    <SectionCard
      action={
        loading || model == null ? null : (
          <CardNote>
            {model.points.length} {model.points.length === 1 ? "fill" : "fills"}
          </CardNote>
        )
      }
      className={className}
      icon={ChartLineData02Icon}
      title="Fee drag"
    >
      {loading ? (
        <Skeleton className="w-full rounded-md" style={{ height: CHART_H }} />
      ) : model == null ? (
        <p
          className="grid place-items-center text-[13px] text-muted-foreground"
          style={{ height: CHART_H }}
        >
          No fills recorded yet.
        </p>
      ) : (
        <>
          <FeeDragChart points={model.points} />
          <ChartLegend>
            <LegendLine color="var(--accent-orange)" label="Gross realized" />
            <LegendLine color="var(--short)" label="Fees paid" />
          </ChartLegend>
          <StatGrid>
            <MicroStat
              label="Gross realized"
              tone={pnlTone(model.gross)}
              value={usd(model.gross)}
            />
            <MicroStat label="Fees paid" tone="short" value={usd(model.fees)} />
            <MicroStat
              label="Net after fees"
              tone={pnlTone(model.net)}
              value={usd(model.net)}
            />
            <MicroStat
              label="Fees / gross"
              tone="muted"
              value={model.drag == null ? "—" : `${model.drag.toFixed(1)}%`}
            />
          </StatGrid>
        </>
      )}
    </SectionCard>
  )
}
