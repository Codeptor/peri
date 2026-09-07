"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  binCount,
  extent,
  GUIDE_DASH,
  roundedBarPath,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type HistogramProps = {
  values: number[]
  /** defaults to Freedman–Diaconis, capped 8..20 */
  bins?: number
  height?: number
  format?: (v: number) => string
  /** colour bins by sign and mark the zero line (default true) */
  highlightZero?: boolean
  ariaLabel?: string
  className?: string
}

const PAD_TOP = 12
const PAD_LEFT = 2
const PAD_RIGHT = 30
const PAD_BOTTOM = 18

/**
 * Distribution of a sample, with zero marked.
 *
 * When the sample straddles zero the bin grid is anchored *at* zero, so no bin
 * ever mixes winners with losers — a straddling bin would hide exactly the
 * asymmetry a PnL distribution is being read for.
 */
export function Histogram({
  values,
  bins,
  height = 180,
  format = usd,
  highlightZero = true,
  ariaLabel,
  className,
}: HistogramProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()

  const model = React.useMemo(() => {
    const finite = values.filter((v) => Number.isFinite(v))
    if (finite.length === 0) return null

    const k = Math.max(1, Math.floor(bins ?? binCount(finite)))
    let [lo, hi] = extent(finite)
    if (hi === lo) {
      const pad = Math.abs(lo) > 0 ? Math.abs(lo) * 0.5 : 0.5
      lo -= pad
      hi += pad
    }
    const step = (hi - lo) / k
    let start = lo
    let count = k
    if (lo < 0 && hi > 0) {
      start = Math.floor(lo / step) * step
      const end = Math.ceil(hi / step) * step
      count = Math.max(1, Math.round((end - start) / step))
    }

    const counts = new Array<number>(count).fill(0)
    for (const v of finite) {
      const idx = Math.max(
        0,
        Math.min(count - 1, Math.floor((v - start) / step))
      )
      counts[idx] += 1
    }
    const peak = counts.reduce((m, c) => Math.max(m, c), 0)
    return {
      counts,
      step,
      start,
      end: start + step * count,
      peak,
      total: finite.length,
    }
  }, [values, bins])

  const geom = React.useMemo(() => {
    if (!model || width <= 0) return null
    const innerW = Math.max(1, width - PAD_LEFT - PAD_RIGHT)
    const innerH = Math.max(1, height - PAD_TOP - PAD_BOTTOM)
    const slot = innerW / model.counts.length
    const barW = Math.max(1, slot - Math.min(4, Math.max(1, slot * 0.16)))
    const yBase = PAD_TOP + innerH
    const scale = model.peak > 0 ? innerH / model.peak : 0

    const bars = model.counts.map((c, i) => {
      const edgeLo = model.start + model.step * i
      const edgeHi = edgeLo + model.step
      const h = c > 0 ? Math.max(2, c * scale) : 0
      const cx = PAD_LEFT + slot * i + slot / 2
      const color = !highlightZero
        ? "var(--accent-orange)"
        : edgeHi <= 0
          ? "var(--short)"
          : edgeLo >= 0
            ? "var(--long)"
            : "var(--chart-6)"
      return {
        count: c,
        edgeLo,
        edgeHi,
        cx,
        slotX: PAD_LEFT + slot * i,
        y: yBase - h,
        h,
        color,
        path:
          h > 0
            ? roundedBarPath(
                cx - barW / 2,
                yBase - h,
                barW,
                h,
                Math.min(3, barW / 2),
                "top"
              )
            : "",
      }
    })

    const spansZero = model.start < 0 && model.end > 0
    const zeroX = spansZero
      ? PAD_LEFT + ((0 - model.start) / (model.end - model.start)) * innerW
      : null

    return { innerW, innerH, slot, bars, yBase, zeroX }
  }, [model, width, height, highlightZero])

  const active = tip?.data ?? null
  const bar = active != null && geom ? geom.bars[active] : null

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={hide}
      ref={containerRef}
      style={{ height }}
    >
      {!model ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={
            ariaLabel ??
            `Histogram, ${geom.bars.length} bins over ${model.total} samples`
          }
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

          {geom.bars.map((b, i) =>
            b.path ? (
              <path
                d={b.path}
                fill={b.color}
                key={i}
                opacity={active == null || active === i ? 1 : 0.55}
              />
            ) : null
          )}

          <line
            stroke="var(--border)"
            strokeWidth={1}
            x1={PAD_LEFT}
            x2={PAD_LEFT + geom.innerW}
            y1={geom.yBase}
            y2={geom.yBase}
          />

          {geom.zeroX != null ? (
            <line
              opacity={0.55}
              stroke="var(--muted-foreground)"
              strokeDasharray={GUIDE_DASH}
              strokeWidth={1}
              x1={geom.zeroX}
              x2={geom.zeroX}
              y1={PAD_TOP}
              y2={geom.yBase}
            />
          ) : null}

          <g
            className={AXIS_TICK_CLASS}
            fill={AXIS_TICK_FILL}
            fontSize={AXIS_TICK_SIZE}
          >
            <text x={PAD_LEFT + geom.innerW + 6} y={PAD_TOP + 4}>
              {model.peak}
            </text>
            <text textAnchor="start" x={PAD_LEFT} y={height - 4}>
              {format(model.start)}
            </text>
            {geom.zeroX != null &&
            geom.zeroX > PAD_LEFT + 28 &&
            geom.zeroX < PAD_LEFT + geom.innerW - 28 ? (
              <text textAnchor="middle" x={geom.zeroX} y={height - 4}>
                0
              </text>
            ) : null}
            <text textAnchor="end" x={PAD_LEFT + geom.innerW} y={height - 4}>
              {format(model.end)}
            </text>
          </g>

          {geom.bars.map((b, i) => (
            <rect
              fill="transparent"
              height={geom.innerH}
              key={i}
              onPointerEnter={() =>
                show(i, b.cx, b.h > 0 ? b.y : geom.yBase - 2)
              }
              width={geom.slot}
              x={b.slotX}
              y={PAD_TOP}
            />
          ))}
        </svg>
      ) : null}

      {tip && bar && model ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          meta={`${format(bar.edgeLo)} – ${format(bar.edgeHi)}`}
          rows={[
            { label: "Count", value: bar.count },
            {
              label: "Share",
              value: `${((bar.count / model.total) * 100).toFixed(0)}%`,
            },
          ]}
          x={tip.x}
          y={tip.y}
        />
      ) : null}
    </div>
  )
}
