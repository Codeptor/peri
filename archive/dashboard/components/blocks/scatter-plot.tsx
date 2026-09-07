"use client"

import * as React from "react"

import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  extent,
  GUIDE_DASH,
  padDomain,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type ScatterPoint = {
  x: number
  y: number
  /** dot radius in px (default 5) — encode a third variable here */
  r?: number
  color?: string
  /** tooltip headline; without it the tooltip shows coordinates only */
  label?: string
}

export type ScatterPlotProps = {
  points: ScatterPoint[]
  xLabel?: string
  yLabel?: string
  xFormat?: (v: number) => string
  yFormat?: (v: number) => string
  /** dashed reference lines in data space — thresholds, break-even, medians */
  guides?: { x?: number[]; y?: number[] }
  height?: number
  ariaLabel?: string
  className?: string
}

const PAD_TOP = 14
const PAD_RIGHT = 14

function plain(v: number): string {
  return Number.isInteger(v) ? String(v) : v.toFixed(2)
}

/**
 * Two numeric axes and one dot per observation — the form for "does x explain
 * y", which no time-bucketed chart can answer.
 */
export function ScatterPlot({
  points,
  xLabel,
  yLabel,
  xFormat = plain,
  yFormat = plain,
  guides,
  height = 260,
  ariaLabel,
  className,
}: ScatterPlotProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()

  const padLeft = yLabel ? 54 : 42
  const padBottom = xLabel ? 34 : 22

  const geom = React.useMemo(() => {
    const usable = points.filter(
      (p) => Number.isFinite(p.x) && Number.isFinite(p.y)
    )
    if (width <= 0 || usable.length === 0) return null
    const innerW = Math.max(1, width - padLeft - PAD_RIGHT)
    const innerH = Math.max(1, height - PAD_TOP - padBottom)

    const [rawXLo, rawXHi] = extent(usable.map((p) => p.x))
    const [rawYLo, rawYHi] = extent(usable.map((p) => p.y))
    // Guides are part of the story; a threshold outside the data's own range
    // still has to be visible, so the domain opens to include it.
    const xVals = [rawXLo, rawXHi, ...(guides?.x ?? [])].filter(Number.isFinite)
    const yVals = [rawYLo, rawYHi, ...(guides?.y ?? [])].filter(Number.isFinite)
    const [xLo, xHi] = padDomain(Math.min(...xVals), Math.max(...xVals))
    const [yLo, yHi] = padDomain(Math.min(...yVals), Math.max(...yVals))

    const sx = (v: number) => padLeft + ((v - xLo) / (xHi - xLo)) * innerW
    const sy = (v: number) => PAD_TOP + (1 - (v - yLo) / (yHi - yLo)) * innerH

    // Big dots first so a small one is never buried under a large neighbour.
    const dots = usable
      .map((p, i) => ({
        i,
        p,
        cx: sx(p.x),
        cy: sy(p.y),
        r: Math.max(2, p.r ?? 5),
      }))
      .sort((a, b) => b.r - a.r)

    return {
      innerW,
      innerH,
      dots,
      sx,
      sy,
      xTicks: [xLo, (xLo + xHi) / 2, xHi],
      yTicks: [yHi, (yLo + yHi) / 2, yLo],
    }
  }, [points, width, height, padLeft, padBottom, guides])

  const hovered = tip != null ? points[tip.data] : null

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={hide}
      ref={containerRef}
      style={{ height }}
    >
      {points.length === 0 ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={ariaLabel ?? `Scatter plot, ${geom.dots.length} points`}
          className="block"
          height={height}
          role="img"
          width={width}
        >
          <line
            stroke="var(--border)"
            strokeWidth={1}
            x1={padLeft}
            x2={padLeft + geom.innerW}
            y1={PAD_TOP + geom.innerH}
            y2={PAD_TOP + geom.innerH}
          />
          <line
            stroke="var(--border)"
            strokeWidth={1}
            x1={padLeft}
            x2={padLeft}
            y1={PAD_TOP}
            y2={PAD_TOP + geom.innerH}
          />

          {(guides?.x ?? []).map((v, i) => (
            <line
              key={`gx-${i}`}
              opacity={0.5}
              stroke="var(--muted-foreground)"
              strokeDasharray={GUIDE_DASH}
              strokeWidth={1}
              x1={geom.sx(v)}
              x2={geom.sx(v)}
              y1={PAD_TOP}
              y2={PAD_TOP + geom.innerH}
            />
          ))}
          {(guides?.y ?? []).map((v, i) => (
            <line
              key={`gy-${i}`}
              opacity={0.5}
              stroke="var(--muted-foreground)"
              strokeDasharray={GUIDE_DASH}
              strokeWidth={1}
              x1={padLeft}
              x2={padLeft + geom.innerW}
              y1={geom.sy(v)}
              y2={geom.sy(v)}
            />
          ))}

          <g
            className={AXIS_TICK_CLASS}
            fill={AXIS_TICK_FILL}
            fontSize={AXIS_TICK_SIZE}
          >
            {geom.yTicks.map((v, i) => (
              <text
                key={`ty-${i}`}
                textAnchor="end"
                x={padLeft - 6}
                y={geom.sy(v) + 3}
              >
                {yFormat(v)}
              </text>
            ))}
            {geom.xTicks.map((v, i) => (
              <text
                key={`tx-${i}`}
                textAnchor={i === 0 ? "start" : i === 2 ? "end" : "middle"}
                x={geom.sx(v)}
                y={PAD_TOP + geom.innerH + 13}
              >
                {xFormat(v)}
              </text>
            ))}
          </g>

          {xLabel ? (
            <text
              fill="var(--muted-foreground)"
              fontSize={11}
              textAnchor="middle"
              x={padLeft + geom.innerW / 2}
              y={height - 4}
            >
              {xLabel}
            </text>
          ) : null}
          {yLabel ? (
            <text
              fill="var(--muted-foreground)"
              fontSize={11}
              textAnchor="middle"
              transform={`rotate(-90 11 ${PAD_TOP + geom.innerH / 2})`}
              x={11}
              y={PAD_TOP + geom.innerH / 2}
            >
              {yLabel}
            </text>
          ) : null}

          {geom.dots.map((dot) => {
            const color = dot.p.color ?? "var(--accent-orange)"
            const active = tip?.data === dot.i
            return (
              <g key={dot.i}>
                <circle
                  cx={dot.cx}
                  cy={dot.cy}
                  fill={color}
                  fillOpacity={active ? 0.55 : 0.3}
                  r={dot.r}
                  stroke={color}
                  strokeWidth={active ? 2 : 1.5}
                />
                <circle
                  cx={dot.cx}
                  cy={dot.cy}
                  fill="transparent"
                  onPointerEnter={() => show(dot.i, dot.cx, dot.cy - dot.r)}
                  r={dot.r + 6}
                />
              </g>
            )
          })}
        </svg>
      ) : null}

      {tip && hovered ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          rows={[
            { label: xLabel ?? "x", value: xFormat(hovered.x) },
            { label: yLabel ?? "y", value: yFormat(hovered.y) },
          ]}
          title={hovered.label}
          x={tip.x}
          y={tip.y}
        />
      ) : null}
    </div>
  )
}
