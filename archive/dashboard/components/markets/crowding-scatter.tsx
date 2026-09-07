"use client"

import * as React from "react"

import { compact, signedPct } from "@/lib/format"
import { Skeleton } from "@/components/ui/skeleton"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  GRID_STROKE,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

import {
  EmptyNote,
  MarketName,
  META_LABEL,
  QUADRANT_LABEL,
  type ScreenerRow,
  signedFixed,
  SUB_TITLE,
} from "./shared"

export type CrowdingScatterProps = {
  rows: ScreenerRow[]
  loading: boolean
  selected: string | null
  onSelect: (market: string) => void
  height?: number
}

/** |funding z| at or above this reads as a crowded book. */
const CROWDED_Z = 1.5

const PAD = { top: 18, right: 18, bottom: 26, left: 44 }

type Point = {
  market: string
  fz: number
  r1h: number
  vol: number
  nominated: boolean
  x: number
  y: number
  r: number
}

/**
 * funding_z × r1h, dot area by 24h volume. Orange = crowded, neutral = calm.
 *
 * Kept bespoke rather than folded into the shared `ScatterPlot`: the axes here
 * cross *at zero* because the quadrants are the reading (a crowded short book
 * that is already ripping is a different animal from a crowded long book that
 * is bleeding), and the marks carry a nominated ring plus a persistent ticker
 * label the primitive has no contract for. What it does share is the tooltip —
 * one hover card across the whole desk, in place of the native `<title>` that
 * used to fire a second later on top of it.
 */
export function CrowdingScatter({
  rows,
  loading,
  selected,
  onSelect,
  height = 300,
}: CrowdingScatterProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<string>()

  const plotted = React.useMemo(
    () =>
      rows.flatMap((row) => {
        const features = row.data.features
        return features ? [{ row, features }] : []
      }),
    [rows]
  )

  const geom = React.useMemo(() => {
    if (width <= 0 || plotted.length === 0) return null
    const innerW = Math.max(1, width - PAD.left - PAD.right)
    const innerH = Math.max(1, height - PAD.top - PAD.bottom)

    let xAbs = CROWDED_Z
    let yAbs = 0.5
    let volMax = 0
    for (const { row, features } of plotted) {
      xAbs = Math.max(xAbs, Math.abs(features.funding_z))
      yAbs = Math.max(yAbs, Math.abs(features.r1h))
      volMax = Math.max(volMax, row.data.day_ntl_vlm)
    }
    xAbs *= 1.2
    yAbs *= 1.25

    const scaleX = (v: number) => PAD.left + ((v + xAbs) / (2 * xAbs)) * innerW
    const scaleY = (v: number) =>
      PAD.top + (1 - (v + yAbs) / (2 * yAbs)) * innerH

    const points: Point[] = plotted.map(({ row, features }) => {
      const scale = volMax > 0 ? Math.sqrt(row.data.day_ntl_vlm / volMax) : 0
      return {
        market: row.market,
        fz: features.funding_z,
        r1h: features.r1h,
        vol: row.data.day_ntl_vlm,
        nominated: row.nominee != null,
        x: scaleX(features.funding_z),
        y: scaleY(features.r1h),
        r: 5 + 9 * scale,
      }
    })

    // Big dots paint first so a small one is never buried under a large neighbour.
    points.sort((a, b) => b.r - a.r)

    return {
      points,
      innerW,
      innerH,
      xAbs,
      yAbs,
      zeroX: scaleX(0),
      zeroY: scaleY(0),
    }
  }, [plotted, width, height])

  const crowded = React.useMemo(
    () =>
      [...plotted]
        .sort(
          (a, b) =>
            Math.abs(b.features.funding_z) - Math.abs(a.features.funding_z)
        )
        .slice(0, 3),
    [plotted]
  )

  const hovered = geom?.points.find((p) => p.market === tip?.data) ?? null

  return (
    <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_190px]">
      <div
        className="relative"
        onPointerLeave={hide}
        ref={containerRef}
        style={{ height }}
      >
        {loading && plotted.length === 0 ? (
          <Skeleton className="h-full w-full rounded-md" />
        ) : plotted.length === 0 ? (
          <div className="grid h-full place-items-center">
            <EmptyNote className="w-full">
              No feature data in this snapshot — the crowding map needs funding
              and return features, which kestreld only publishes once a market
              has filled its ring buffers.
            </EmptyNote>
          </div>
        ) : geom ? (
          <svg
            aria-label="Crowding map: funding z-score against one-hour return, dot size by 24 hour volume"
            className="block"
            height={height}
            role="img"
            width={width}
          >
            <line
              stroke={GRID_STROKE}
              strokeWidth={1}
              x1={geom.zeroX}
              x2={geom.zeroX}
              y1={PAD.top}
              y2={PAD.top + geom.innerH}
            />
            <line
              stroke={GRID_STROKE}
              strokeWidth={1}
              x1={PAD.left}
              x2={PAD.left + geom.innerW}
              y1={geom.zeroY}
              y2={geom.zeroY}
            />

            <g
              className={QUADRANT_LABEL}
              fill="var(--muted-foreground)"
              opacity={0.7}
            >
              <text x={PAD.left + 4} y={PAD.top + 10}>
                Short squeeze
              </text>
              <text
                textAnchor="end"
                x={PAD.left + geom.innerW - 4}
                y={PAD.top + geom.innerH - 4}
              >
                Long flush
              </text>
            </g>

            <g
              className={AXIS_TICK_CLASS}
              fill={AXIS_TICK_FILL}
              fontSize={AXIS_TICK_SIZE}
            >
              <text textAnchor="end" x={PAD.left - 8} y={PAD.top + 4}>
                {`${signedFixed(geom.yAbs, 1)}%`}
              </text>
              <text textAnchor="end" x={PAD.left - 8} y={geom.zeroY + 3}>
                0.0%
              </text>
              <text
                textAnchor="end"
                x={PAD.left - 8}
                y={PAD.top + geom.innerH + 3}
              >
                {`${signedFixed(-geom.yAbs, 1)}%`}
              </text>
              <text textAnchor="start" x={PAD.left} y={height - 4}>
                {signedFixed(-geom.xAbs, 1)}
              </text>
              <text textAnchor="middle" x={geom.zeroX} y={height - 4}>
                0.0
              </text>
              <text textAnchor="end" x={PAD.left + geom.innerW} y={height - 4}>
                {signedFixed(geom.xAbs, 1)}
              </text>
            </g>

            {geom.points.map((point) => {
              const isCrowded = Math.abs(point.fz) >= CROWDED_Z
              const color = isCrowded
                ? "var(--accent-orange)"
                : "var(--chart-6)"
              const active =
                selected === point.market || tip?.data === point.market
              return (
                <g
                  aria-label={`${point.market}: funding z ${signedFixed(point.fz)}, 1h ${signedPct(point.r1h)}, 24h volume ${compact(point.vol)}`}
                  className="cursor-pointer"
                  key={point.market}
                  onBlur={hide}
                  onClick={() => onSelect(point.market)}
                  onFocus={() => show(point.market, point.x, point.y - point.r)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault()
                      onSelect(point.market)
                    }
                  }}
                  onPointerEnter={() =>
                    show(point.market, point.x, point.y - point.r)
                  }
                  role="button"
                  tabIndex={0}
                >
                  {point.nominated ? (
                    <circle
                      cx={point.x}
                      cy={point.y}
                      fill="none"
                      opacity={0.75}
                      r={point.r + 3.5}
                      stroke="var(--accent-orange)"
                      strokeWidth={1}
                    />
                  ) : null}
                  {/*
                    Fill is a translucent wash of the same token as the stroke, so
                    the dot sits on the near-white light card and the warm dark
                    card identically — the solid stroke carries it in both.
                  */}
                  <circle
                    cx={point.x}
                    cy={point.y}
                    fill={color}
                    fillOpacity={active ? 0.55 : 0.3}
                    r={point.r}
                    stroke={color}
                    strokeWidth={active ? 2 : 1.5}
                  />
                  <text
                    className={AXIS_TICK_CLASS}
                    fill={
                      active ? "var(--foreground)" : "var(--muted-foreground)"
                    }
                    fontSize={AXIS_TICK_SIZE}
                    x={point.x + point.r + 5}
                    y={point.y + 3}
                  >
                    {point.market}
                  </text>
                  {/* Hit area sized to the mark, so a 5px dot is still hoverable. */}
                  <circle
                    cx={point.x}
                    cy={point.y}
                    fill="transparent"
                    r={point.r + 6}
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
              { label: "Funding z", value: signedFixed(hovered.fz) },
              {
                label: "1h return",
                value: signedPct(hovered.r1h),
                color:
                  hovered.r1h > 0
                    ? "var(--long-ink)"
                    : hovered.r1h < 0
                      ? "var(--short-ink)"
                      : undefined,
              },
              { label: "24h volume", value: compact(hovered.vol) },
            ]}
            title={<MarketName className="text-xs" market={hovered.market} />}
            x={tip.x}
            y={tip.y}
          />
        ) : null}
      </div>

      <div className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          <span className={SUB_TITLE}>Legend</span>
          <div className="flex items-center gap-2.5 text-[13px] text-muted-foreground">
            <span className="size-2 shrink-0 rounded-full bg-accent-orange" />
            Crowded <span className="font-mono">|z| ≥ {CROWDED_Z}</span>
          </div>
          <div className="flex items-center gap-2.5 text-[13px] text-muted-foreground">
            <span className="size-2 shrink-0 rounded-full bg-chart-6" />
            Calm book
          </div>
          <div className="flex items-center gap-2.5 text-[13px] text-muted-foreground">
            <span className="size-2 shrink-0 rounded-full border border-accent-orange" />
            Nominated
          </div>
          <span className={META_LABEL}>Dot size = 24h volume</span>
        </div>

        {crowded.length > 0 ? (
          <div className="flex flex-col gap-2 border-t border-border pt-4">
            <span className={SUB_TITLE}>Most crowded</span>
            {crowded.map(({ row, features }) => (
              <button
                className="flex items-center gap-2 text-left transition-colors hover:text-primary-ink"
                key={row.market}
                onClick={() => onSelect(row.market)}
                type="button"
              >
                <MarketName
                  className="truncate text-[13px]"
                  market={row.market}
                />
                <span className="ml-auto shrink-0 font-mono text-[13px] text-muted-foreground">
                  {signedFixed(features.funding_z)}
                </span>
              </button>
            ))}
          </div>
        ) : null}
      </div>
    </div>
  )
}
