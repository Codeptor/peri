"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  GRID_STROKE,
  monotonePath,
  roundedBarPath,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type BarChartDatum = {
  x: string | number
  value: number
  /** overrides the signed default (long above the baseline, short below) */
  color?: string
}

export type BarChartProps = {
  data: BarChartDatum[]
  /** overlay the running total as a smooth accent line on its own scale */
  cumulative?: boolean
  baseline?: number
  height?: number
  format?: (v: number) => string
  xLabels?: "sparse" | "all" | "none"
  /** accessible summary of the chart; the tooltip carries the per-bar detail */
  ariaLabel?: string
  className?: string
}

const PAD_TOP = 12
const PAD_LEFT = 2
const PAD_RIGHT = 44

/**
 * Signed vertical bars around a baseline, optionally with the running total
 * drawn over them.
 *
 * Bars answer "how did each bucket do"; the cumulative line answers "where did
 * that leave us" — a matrix of filled cells can say neither, which is why the
 * 30-day PnL view moved here. The line carries its own y-domain: a month of
 * small daily nets against one large running total would otherwise flatten the
 * bars into noise.
 */
export function BarChart({
  data,
  cumulative = false,
  baseline = 0,
  height = 180,
  format = usd,
  xLabels = "sparse",
  ariaLabel,
  className,
}: BarChartProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()

  const padBottom = xLabels === "none" ? 4 : 18

  const geom = React.useMemo(() => {
    if (width <= 0 || data.length === 0) return null
    const innerW = Math.max(1, width - PAD_LEFT - PAD_RIGHT)
    const innerH = Math.max(1, height - PAD_TOP - padBottom)

    let lo = baseline
    let hi = baseline
    for (const d of data) {
      if (!Number.isFinite(d.value)) continue
      if (d.value < lo) lo = d.value
      if (d.value > hi) hi = d.value
    }
    // Pad only away from the baseline: bars must meet the axis, not hover above it.
    const span = hi - lo || Math.abs(baseline) || 1
    let domLo = lo < baseline ? lo - span * 0.08 : lo
    let domHi = hi > baseline ? hi + span * 0.08 : hi
    if (domHi <= domLo) {
      domLo = baseline - 1
      domHi = baseline + 1
    }
    const yOf = (v: number) =>
      PAD_TOP + (1 - (v - domLo) / (domHi - domLo)) * innerH

    const slot = innerW / data.length
    const barW = Math.max(1, slot - Math.min(6, Math.max(1, slot * 0.22)))
    const radius = Math.min(3, barW / 2)
    const yBase = yOf(baseline)

    const bars = data.map((d, i) => {
      const value = Number.isFinite(d.value) ? d.value : baseline
      const cx = PAD_LEFT + slot * i + slot / 2
      const up = value >= baseline
      const raw = Math.abs(yOf(value) - yBase)
      // A tiny non-zero bucket still has to be visible; a zero one draws nothing.
      const h = value === baseline ? 0 : Math.max(2, raw)
      const y = up ? yBase - h : yBase
      return {
        cx,
        slotX: PAD_LEFT + slot * i,
        y,
        h,
        value,
        color: d.color ?? (value >= baseline ? "var(--long)" : "var(--short)"),
        path:
          h > 0
            ? roundedBarPath(
                cx - barW / 2,
                y,
                barW,
                h,
                radius,
                up ? "top" : "bottom"
              )
            : "",
      }
    })

    let cumSeries: {
      totals: number[]
      path: string
      last: [number, number]
    } | null = null
    if (cumulative) {
      const totals: number[] = []
      let running = 0
      for (const d of data) {
        running += Number.isFinite(d.value) ? d.value : 0
        totals.push(running)
      }
      let cLo = 0
      let cHi = 0
      for (const t of totals) {
        if (t < cLo) cLo = t
        if (t > cHi) cHi = t
      }
      if (cHi <= cLo) {
        cLo -= 1
        cHi += 1
      }
      const cumPad = (cHi - cLo) * 0.08
      const lo2 = cLo - cumPad
      const hi2 = cHi + cumPad
      const pts = totals.map((t, i): [number, number] => [
        bars[i].cx,
        PAD_TOP + (1 - (t - lo2) / (hi2 - lo2)) * innerH,
      ])
      cumSeries = {
        totals,
        path: monotonePath(pts),
        last: pts[pts.length - 1],
      }
    }

    return { innerW, innerH, slot, bars, yBase, yOf, lo, hi, cum: cumSeries }
  }, [data, width, height, padBottom, baseline, cumulative])

  const tickIndices = React.useMemo(() => {
    if (xLabels === "none" || data.length === 0) return []
    if (xLabels === "all") return data.map((_, i) => i)
    const mid = Math.floor((data.length - 1) / 2)
    return Array.from(new Set([0, mid, data.length - 1]))
  }, [xLabels, data])

  const active = tip?.data ?? null
  const datum = active != null ? data[active] : null

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={hide}
      ref={containerRef}
      style={{ height }}
    >
      {data.length === 0 ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={ariaLabel ?? `Bar chart, ${data.length} buckets`}
          className="block"
          height={height}
          role="img"
          width={width}
        >
          {active != null ? (
            <rect
              fill="var(--cell)"
              height={geom.innerH}
              width={geom.slot}
              x={geom.bars[active].slotX}
              y={PAD_TOP}
            />
          ) : null}

          <line
            stroke={GRID_STROKE}
            strokeWidth={1}
            x1={PAD_LEFT}
            x2={PAD_LEFT + geom.innerW}
            y1={geom.yBase}
            y2={geom.yBase}
          />

          {geom.bars.map((bar, i) =>
            bar.path ? (
              <path
                d={bar.path}
                fill={bar.color}
                key={i}
                opacity={active == null || active === i ? 1 : 0.55}
              />
            ) : null
          )}

          {geom.cum ? (
            <g>
              <path
                d={geom.cum.path}
                fill="none"
                stroke="var(--accent-orange)"
                strokeLinecap="round"
                strokeLinejoin="round"
                strokeWidth={1.75}
              />
              <circle
                cx={geom.cum.last[0]}
                cy={geom.cum.last[1]}
                fill="var(--accent-orange)"
                r={2.75}
              />
            </g>
          ) : null}

          <g
            className={AXIS_TICK_CLASS}
            fill={AXIS_TICK_FILL}
            fontSize={AXIS_TICK_SIZE}
          >
            {geom.hi > baseline ? (
              <text x={PAD_LEFT + geom.innerW + 7} y={geom.yOf(geom.hi) + 3}>
                {format(geom.hi)}
              </text>
            ) : null}
            {geom.lo < baseline &&
            Math.abs(geom.yOf(geom.lo) - geom.yOf(geom.hi)) > 12 ? (
              <text x={PAD_LEFT + geom.innerW + 7} y={geom.yOf(geom.lo) + 3}>
                {format(geom.lo)}
              </text>
            ) : null}
            {tickIndices.map((i) => (
              <text
                key={i}
                textAnchor={
                  i === 0 ? "start" : i === data.length - 1 ? "end" : "middle"
                }
                x={
                  i === 0
                    ? PAD_LEFT
                    : i === data.length - 1
                      ? PAD_LEFT + geom.innerW
                      : geom.bars[i].cx
                }
                y={height - 4}
              >
                {data[i].x}
              </text>
            ))}
          </g>

          {geom.bars.map((bar, i) => (
            <rect
              fill="transparent"
              height={geom.innerH}
              key={i}
              onPointerEnter={() =>
                show(i, bar.cx, bar.h > 0 ? bar.y : geom.yBase)
              }
              width={geom.slot}
              x={bar.slotX}
              y={PAD_TOP}
            />
          ))}
        </svg>
      ) : null}

      {tip && datum && geom ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          rows={
            geom.cum
              ? [
                  {
                    label: "Cumulative",
                    value: format(geom.cum.totals[tip.data]),
                    color: "var(--accent-orange-ink)",
                  },
                ]
              : undefined
          }
          title={String(datum.x)}
          value={format(datum.value)}
          valueColor={
            datum.value > baseline
              ? "var(--long-ink)"
              : datum.value < baseline
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
