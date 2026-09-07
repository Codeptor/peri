"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  extent,
  GUIDE_DASH,
  monotonePath,
  padDomain,
  rollingMean,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type RollingPoint = { x: number; value: number }

export type RollingLineProps = {
  data: RollingPoint[]
  /** trailing window in points (default 10) */
  window?: number
  height?: number
  format?: (v: number) => string
  /** dashed level the mean is being judged against — break-even, a target */
  refLine?: number
  /** supply to label the x axis; omitted, the axis stays bare and the tooltip carries x */
  xFormat?: (v: number) => string
  ariaLabel?: string
  className?: string
}

const PAD_TOP = 12
const PAD_LEFT = 2
const PAD_RIGHT = 44

/**
 * Raw series faint, trailing mean prominent.
 *
 * The raw line stays on the chart because the mean alone hides its own
 * dispersion: a rolling win-rate of 55% built from a coin-flip series and one
 * built from a steady one draw the same mean and mean very different things.
 */
export function RollingLine({
  data,
  window = 10,
  height = 200,
  format = usd,
  refLine,
  xFormat,
  ariaLabel,
  className,
}: RollingLineProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()

  const padBottom = xFormat ? 18 : 4

  const series = React.useMemo(() => {
    const clean = data
      .filter((d) => Number.isFinite(d.x) && Number.isFinite(d.value))
      .sort((a, b) => a.x - b.x)
    if (clean.length === 0) return null
    return {
      clean,
      mean: rollingMean(
        clean.map((d) => d.value),
        window
      ),
    }
  }, [data, window])

  const geom = React.useMemo(() => {
    if (!series || width <= 0) return null
    const { clean, mean } = series
    const innerW = Math.max(1, width - PAD_LEFT - PAD_RIGHT)
    const innerH = Math.max(1, height - PAD_TOP - padBottom)

    const [rawLo, rawHi] = extent([...clean.map((d) => d.value), ...mean])
    const bounds = [rawLo, rawHi]
    if (refLine != null && Number.isFinite(refLine)) bounds.push(refLine)
    const [lo, hi] = padDomain(Math.min(...bounds), Math.max(...bounds))

    const [xLo, xHi] = extent(clean.map((d) => d.x))
    const sx = (v: number) =>
      xHi === xLo
        ? PAD_LEFT + innerW / 2
        : PAD_LEFT + ((v - xLo) / (xHi - xLo)) * innerW
    const sy = (v: number) => PAD_TOP + (1 - (v - lo) / (hi - lo)) * innerH

    const rawPts = clean.map((d): [number, number] => [sx(d.x), sy(d.value)])
    const meanPts = mean.map((v, i): [number, number] => [rawPts[i][0], sy(v)])

    return {
      innerW,
      innerH,
      rawPts,
      meanPts,
      rawPath: rawPts.map(([x, y]) => `${x} ${y}`).join(" L "),
      meanPath: monotonePath(meanPts),
      sy,
      lo: rawLo,
      hi: rawHi,
    }
  }, [series, width, height, padBottom, refLine])

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!geom) return
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left
    let best = 0
    let bestDist = Number.POSITIVE_INFINITY
    for (let i = 0; i < geom.rawPts.length; i++) {
      const dist = Math.abs(geom.rawPts[i][0] - x)
      if (dist < bestDist) {
        bestDist = dist
        best = i
      }
    }
    show(best, geom.meanPts[best][0], geom.meanPts[best][1])
  }

  const idx = tip?.data ?? null
  const point = idx != null && series ? series.clean[idx] : null

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={hide}
      onPointerMove={onMove}
      ref={containerRef}
      style={{ height }}
    >
      {!series ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={
            ariaLabel ??
            `Rolling ${window}-point mean over ${series.clean.length} samples`
          }
          className="block"
          height={height}
          role="img"
          width={width}
        >
          {refLine != null && Number.isFinite(refLine) ? (
            <g>
              <line
                opacity={0.55}
                stroke="var(--muted-foreground)"
                strokeDasharray={GUIDE_DASH}
                strokeWidth={1}
                x1={PAD_LEFT}
                x2={PAD_LEFT + geom.innerW}
                y1={geom.sy(refLine)}
                y2={geom.sy(refLine)}
              />
              <text
                className={AXIS_TICK_CLASS}
                fill={AXIS_TICK_FILL}
                fontSize={AXIS_TICK_SIZE}
                x={PAD_LEFT + geom.innerW + 7}
                y={geom.sy(refLine) + 3}
              >
                {format(refLine)}
              </text>
            </g>
          ) : null}

          <path
            d={`M ${geom.rawPath}`}
            fill="none"
            opacity={0.45}
            stroke="var(--chart-6)"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={1.25}
          />
          <path
            d={geom.meanPath}
            fill="none"
            stroke="var(--accent-orange)"
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={2.25}
          />

          <g
            className={AXIS_TICK_CLASS}
            fill={AXIS_TICK_FILL}
            fontSize={AXIS_TICK_SIZE}
          >
            <text x={PAD_LEFT + geom.innerW + 7} y={geom.sy(geom.hi) + 3}>
              {format(geom.hi)}
            </text>
            {Math.abs(geom.sy(geom.lo) - geom.sy(geom.hi)) > 12 ? (
              <text x={PAD_LEFT + geom.innerW + 7} y={geom.sy(geom.lo) + 3}>
                {format(geom.lo)}
              </text>
            ) : null}
            {xFormat ? (
              <>
                <text textAnchor="start" x={PAD_LEFT} y={height - 4}>
                  {xFormat(series.clean[0].x)}
                </text>
                <text
                  textAnchor="end"
                  x={PAD_LEFT + geom.innerW}
                  y={height - 4}
                >
                  {xFormat(series.clean[series.clean.length - 1].x)}
                </text>
              </>
            ) : null}
          </g>

          {idx != null ? (
            <g>
              <line
                stroke="var(--border)"
                strokeWidth={1}
                x1={geom.meanPts[idx][0]}
                x2={geom.meanPts[idx][0]}
                y1={PAD_TOP}
                y2={PAD_TOP + geom.innerH}
              />
              <circle
                cx={geom.rawPts[idx][0]}
                cy={geom.rawPts[idx][1]}
                fill="var(--chart-6)"
                r={2.5}
              />
              <circle
                cx={geom.meanPts[idx][0]}
                cy={geom.meanPts[idx][1]}
                fill="var(--accent-orange)"
                r={3.5}
                stroke="var(--card)"
                strokeWidth={2}
              />
            </g>
          ) : null}
        </svg>
      ) : null}

      {tip && point && series && idx != null ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          meta={xFormat ? xFormat(point.x) : undefined}
          rows={[
            { label: "Value", value: format(point.value) },
            {
              label: `Mean ${window}`,
              value: format(series.mean[idx]),
              color: "var(--accent-orange-ink)",
            },
          ]}
          x={tip.x}
          y={tip.y}
        />
      ) : null}
    </div>
  )
}
