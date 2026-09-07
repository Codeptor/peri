"use client"

import { useId } from "react"

import { cn } from "@/lib/utils"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type SegmentBarSegment = {
  label: string
  count: number
  color: string
}

export type SegmentBarProps = {
  segments: SegmentBarSegment[]
  className?: string
}

const BAR_H = 10

/** One horizontal stacked bar with rounded ends + legend rows (dot, label, count, pct). */
export function SegmentBar({ segments, className }: SegmentBarProps) {
  const clipId = `segbar-${useId().replace(/[^a-zA-Z0-9]/g, "")}`
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()
  const total = segments.reduce((sum, s) => sum + Math.max(0, s.count), 0)

  // Painted widest-first from x=0 so adjacent segments can never seam.
  const cumulative: number[] = []
  let running = 0
  for (const s of segments) {
    running += Math.max(0, s.count)
    cumulative.push(total > 0 ? (running / total) * 100 : 0)
  }
  const startPct = (i: number) => (i === 0 ? 0 : cumulative[i - 1])

  const hovered = tip != null ? segments[tip.data] : null

  return (
    <div className={cn("flex flex-col gap-4", className)}>
      <div className="relative" onPointerLeave={hide} ref={containerRef}>
        <svg
          aria-label={segments.map((s) => `${s.label} ${s.count}`).join(", ")}
          className="block w-full"
          height={BAR_H}
          role="img"
          width="100%"
        >
          <defs>
            <clipPath id={clipId}>
              <rect height={BAR_H} rx={BAR_H / 2} width="100%" />
            </clipPath>
          </defs>
          <g clipPath={`url(#${clipId})`}>
            <rect fill="var(--cell)" height={BAR_H} width="100%" />
            {segments
              .map((s, i) => ({ ...s, end: cumulative[i], i }))
              .reverse()
              .map((s) =>
                s.end > 0 ? (
                  <rect
                    fill={s.color}
                    height={BAR_H}
                    key={s.i}
                    opacity={tip == null || tip.data === s.i ? 1 : 0.55}
                    width={`${s.end}%`}
                    x={0}
                  />
                ) : null
              )}
            {segments.map((s, i) =>
              Math.max(0, s.count) > 0 ? (
                <rect
                  fill="transparent"
                  height={BAR_H}
                  key={s.label}
                  onPointerEnter={() =>
                    show(i, ((startPct(i) + cumulative[i]) / 200) * width, 0)
                  }
                  width={`${cumulative[i] - startPct(i)}%`}
                  x={`${startPct(i)}%`}
                />
              ) : null
            )}
          </g>
        </svg>

        {tip && hovered ? (
          <TooltipCard
            boundsWidth={width}
            placement="above"
            rows={[
              { label: "Count", value: hovered.count },
              {
                label: "Share",
                value:
                  total > 0
                    ? `${((Math.max(0, hovered.count) / total) * 100).toFixed(0)}%`
                    : "—",
              },
            ]}
            title={hovered.label}
            x={tip.x}
            y={tip.y}
          />
        ) : null}
      </div>

      <div className="flex flex-col gap-2">
        {segments.map((s) => (
          <div className="flex items-center gap-2.5" key={s.label}>
            <span
              className="size-2 shrink-0 rounded-full"
              style={{ backgroundColor: s.color }}
            />
            <span className="truncate text-[13px] text-muted-foreground">
              {s.label}
            </span>
            <span className="ml-auto shrink-0 font-mono text-[13px] font-medium">
              {s.count}
            </span>
            <span className="w-11 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
              {total > 0
                ? `${((Math.max(0, s.count) / total) * 100).toFixed(0)}%`
                : "—"}
            </span>
          </div>
        ))}
      </div>
    </div>
  )
}
