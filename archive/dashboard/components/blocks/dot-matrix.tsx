"use client"

import * as React from "react"

import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type DotMatrixColumn = {
  value: number
  color?: string
  /**
   * Tooltip copy, written as ` · `-separated segments: the first becomes the
   * muted meta line, the second the headline figure, the rest a trailer.
   * `2026-08-01 · $12.30 · 3 closes` reads as date / figure / context.
   */
  label?: string
}

export type DotMatrixProps = {
  columns: DotMatrixColumn[]
  max?: number
  rows?: number
  cellSize?: number
  gap?: number
  xLabels?: string[]
  ariaLabel?: string
  className?: string
}

/** `a · b · c` → meta `a`, value `b`, trailer `c`. */
function splitLabel(label: string | undefined) {
  if (!label) return null
  const parts = label.split(" · ")
  return {
    meta: parts[0],
    value: parts[1],
    trailer: parts.slice(2).join(" · ") || undefined,
  }
}

/**
 * The signature v3 chart: a grid of small rounded cells. One column per time
 * bucket, filled from the bottom; filled-cell count encodes the value.
 *
 * It stays the form for presence and counts — "did anything happen, how often"
 * — where the quantised cells are a feature. Magnitudes that need reading off
 * an axis belong in `BarChart`.
 */
export function DotMatrix({
  columns,
  max,
  rows = 12,
  cellSize = 8,
  gap = 3,
  xLabels,
  ariaLabel,
  className,
}: DotMatrixProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()
  const scrollRef = React.useRef<HTMLDivElement | null>(null)

  const pitch = cellSize + gap
  const gridW = Math.max(columns.length * pitch - gap, 0)
  const gridH = rows * pitch - gap
  const labelH = xLabels ? 16 : 0
  const scale =
    max ?? columns.reduce((hi, c) => Math.max(hi, Math.abs(c.value)), 0)
  const radius = Math.max(1, Math.round(cellSize / 4))

  const fillOf = (value: number) => {
    const magnitude = Math.abs(value)
    if (scale <= 0 || magnitude === 0) return 0
    return Math.min(rows, Math.max(1, Math.round((magnitude / scale) * rows)))
  }

  const hovered = tip != null ? columns[tip.data] : null
  const parts = splitLabel(hovered?.label)

  return (
    <div className={cn("relative w-full", className)} ref={containerRef}>
      <div
        className="w-full overflow-x-auto"
        onPointerLeave={hide}
        ref={scrollRef}
      >
        <svg
          aria-label={ariaLabel ?? `Matrix chart, ${columns.length} buckets`}
          className="block shrink-0"
          height={gridH + labelH}
          role="img"
          viewBox={`0 0 ${gridW} ${gridH + labelH}`}
          width={gridW}
        >
          {tip != null ? (
            <rect
              fill="var(--cell)"
              height={gridH + 2}
              rx={radius}
              width={pitch}
              x={tip.data * pitch - gap / 2}
              y={-1}
            />
          ) : null}

          {columns.map((col, i) => {
            const filled = fillOf(col.value)
            const x = i * pitch
            return (
              <g key={i}>
                {Array.from({ length: rows }, (_, r) => (
                  <rect
                    fill={
                      r >= rows - filled
                        ? (col.color ?? "var(--accent-orange)")
                        : "var(--cell)"
                    }
                    height={cellSize}
                    key={r}
                    rx={radius}
                    width={cellSize}
                    x={x}
                    y={r * pitch}
                  />
                ))}
              </g>
            )
          })}

          {xLabels
            ? xLabels.map((label, i) =>
                label ? (
                  <text
                    className={AXIS_TICK_CLASS}
                    fill={AXIS_TICK_FILL}
                    fontSize={AXIS_TICK_SIZE}
                    key={i}
                    textAnchor={
                      i === 0
                        ? "start"
                        : i === xLabels.length - 1
                          ? "end"
                          : "middle"
                    }
                    x={
                      i === 0
                        ? 0
                        : i === xLabels.length - 1
                          ? i * pitch + cellSize
                          : i * pitch + cellSize / 2
                    }
                    y={gridH + 12}
                  >
                    {label}
                  </text>
                ) : null
              )
            : null}

          {columns.map((col, i) => (
            <rect
              fill="transparent"
              height={gridH}
              key={i}
              onPointerEnter={() =>
                show(
                  i,
                  // the matrix can scroll inside its own box; the tooltip lives
                  // in the outer frame, so the anchor is un-scrolled here
                  i * pitch +
                    cellSize / 2 -
                    (scrollRef.current?.scrollLeft ?? 0),
                  Math.max(0, (rows - fillOf(col.value)) * pitch)
                )
              }
              width={pitch}
              x={i * pitch - gap / 2}
              y={0}
            />
          ))}
        </svg>
      </div>

      {tip && parts ? (
        <TooltipCard
          boundsWidth={width}
          meta={parts.meta}
          value={parts.value}
          x={tip.x}
          y={tip.y}
        >
          {parts.trailer ? (
            <div className="text-[11px] text-muted-foreground">
              {parts.trailer}
            </div>
          ) : null}
        </TooltipCard>
      ) : null}
    </div>
  )
}
