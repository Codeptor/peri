"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { Skeleton } from "@/components/ui/skeleton"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  GRID_STROKE,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

import type { RankedMarket } from "./pnl-data"
import { EmptyNote, MarketName } from "./shared"

export type MarketPnlBarsProps = {
  rows: RankedMarket[]
  loading: boolean
  /** markets with fills that the ±ranking left out, reported under the chart */
  omitted: number
  selected: string | null
  onSelect: (market: string) => void
}

const ROW_H = 26
const BAR_H = 11
const PAD_Y = 6
/** market label column — mono 11px, ~11 characters before the ellipsis */
const GUTTER_LEFT = 78
/**
 * Signed usd, right-aligned. Sized off the widest string `usd` emits before it
 * compacts to `M` — `-$99,999.99`, 11 mono characters at 10px ≈ 68px — plus the
 * 8px the column is inset by, so the numeral never lands on the longest bar.
 */
const GUTTER_VALUE = 78
/** round-trip count, so a lone lucky fill never reads like an edge */
const GUTTER_TRIPS = 34
const MIN_PLOT = 48
/** 10 characters of Geist Mono at 11px ≈ 66px — clear of the 78px gutter. */
const NAME_MAX = 10

/** Horizontal bar rounded only at its far end, so it meets the zero line square. */
function hBarPath(
  x: number,
  y: number,
  w: number,
  h: number,
  radius: number,
  round: "right" | "left"
): string {
  const r = Math.max(0, Math.min(radius, w, h / 2))
  if (r === 0) return `M ${x} ${y} h ${w} v ${h} h ${-w} Z`
  if (round === "right") {
    return `M ${x} ${y} L ${x + w - r} ${y} Q ${x + w} ${y} ${x + w} ${y + r} L ${x + w} ${y + h - r} Q ${x + w} ${y + h} ${x + w - r} ${y + h} L ${x} ${y + h} Z`
  }
  return `M ${x + w} ${y} L ${x + r} ${y} Q ${x} ${y} ${x} ${y + r} L ${x} ${y + h - r} Q ${x} ${y + h} ${x + r} ${y + h} L ${x + w} ${y + h} Z`
}

/** `xyz:TSLA` → muted venue prefix + ticker, the `MarketName` idiom in SVG. */
function NameText({ market, x, y }: { market: string; x: number; y: number }) {
  const clipped =
    market.length > NAME_MAX ? `${market.slice(0, NAME_MAX - 1)}…` : market
  const split = clipped.indexOf(":")
  return (
    <text
      className="font-mono font-medium"
      fill="var(--foreground)"
      fontSize={11}
      x={x}
      y={y}
    >
      {split === -1 ? (
        clipped
      ) : (
        <>
          <tspan fill="var(--muted-foreground)">
            {clipped.slice(0, split + 1)}
          </tspan>
          {clipped.slice(split + 1)}
        </>
      )}
    </text>
  )
}

/**
 * Realized net per market, ranked, as diverging horizontal bars.
 *
 * Horizontal because the categories are names, not time: rank maps to reading
 * order top-down, and each ticker gets a full-length horizontal label at any
 * viewport width. The shared `BarChart` is vertical — sixteen tickers under a
 * half-width card would collide into `sparse` ticks, and a ranked category chart
 * that cannot label its categories has lost the thing it was drawn to show.
 */
export function MarketPnlBars({
  rows,
  loading,
  omitted,
  selected,
  onSelect,
}: MarketPnlBarsProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()
  const height = PAD_Y * 2 + Math.max(1, rows.length) * ROW_H

  const geom = React.useMemo(() => {
    if (width <= 0 || rows.length === 0) return null
    const plotW = Math.max(
      MIN_PLOT,
      width - GUTTER_LEFT - GUTTER_VALUE - GUTTER_TRIPS
    )
    // Pad away from zero only: bars have to meet the axis, not float off it.
    let lo = 0
    let hi = 0
    for (const row of rows) {
      if (row.net < lo) lo = row.net
      if (row.net > hi) hi = row.net
    }
    lo *= 1.04
    hi *= 1.04
    const span = hi - lo || 1
    const xOf = (v: number) => GUTTER_LEFT + ((v - lo) / span) * plotW
    const zeroX = xOf(0)

    const bars = rows.map((row, i) => {
      const top = PAD_Y + i * ROW_H
      const tipX = xOf(row.net)
      const up = row.net >= 0
      const w = Math.max(2, Math.abs(tipX - zeroX))
      return {
        row,
        top,
        mid: top + ROW_H / 2,
        tipX,
        up,
        path: hBarPath(
          up ? zeroX : zeroX - w,
          top + (ROW_H - BAR_H) / 2,
          w,
          BAR_H,
          3,
          up ? "right" : "left"
        ),
      }
    })

    return {
      plotW,
      zeroX,
      bars,
      valueX: width - GUTTER_TRIPS - 8,
      tripsX: width - 2,
    }
  }, [rows, width])

  if (loading && rows.length === 0) {
    return <Skeleton className="h-[210px] w-full rounded-md" />
  }

  if (rows.length === 0) {
    return (
      <EmptyNote>
        No market has booked a round trip yet — kestreld reports no realized PnL
        to rank.
      </EmptyNote>
    )
  }

  const hovered = tip != null ? rows[tip.data] : null

  return (
    <div className="flex flex-col gap-3">
      <div
        className="relative w-full"
        onPointerLeave={hide}
        ref={containerRef}
        style={{ height }}
      >
        {geom ? (
          <svg
            aria-label={`Realized net by market, ${rows.length} markets ranked`}
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
              y1={PAD_Y}
              y2={height - PAD_Y}
            />

            {geom.bars.map((bar, i) => {
              const active = tip?.data === i
              const isSelected = selected === bar.row.market
              const hue = bar.up ? "var(--long)" : "var(--short)"
              return (
                <g
                  aria-label={`${bar.row.market}: ${usd(bar.row.net)} net over ${bar.row.trips} round trips`}
                  className="cursor-pointer"
                  key={bar.row.market}
                  onBlur={hide}
                  onClick={() => onSelect(bar.row.market)}
                  onFocus={() => show(i, bar.tipX, bar.top)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault()
                      onSelect(bar.row.market)
                    }
                  }}
                  onPointerEnter={() => show(i, bar.tipX, bar.top)}
                  role="button"
                  tabIndex={0}
                >
                  {isSelected || active ? (
                    <rect
                      fill={isSelected ? "var(--primary)" : "var(--cell)"}
                      fillOpacity={isSelected ? 0.08 : 1}
                      height={ROW_H - 2}
                      rx={5}
                      stroke={isSelected ? "var(--primary)" : "none"}
                      strokeOpacity={0.35}
                      width={width}
                      x={0}
                      y={bar.top + 1}
                    />
                  ) : null}

                  <NameText market={bar.row.market} x={2} y={bar.mid + 4} />

                  <path
                    d={bar.path}
                    fill={hue}
                    opacity={tip == null || active ? 1 : 0.55}
                  />

                  <g
                    className={AXIS_TICK_CLASS}
                    fill={AXIS_TICK_FILL}
                    fontSize={AXIS_TICK_SIZE}
                  >
                    <text
                      fill={bar.up ? "var(--long-ink)" : "var(--short-ink)"}
                      textAnchor="end"
                      x={geom.valueX}
                      y={bar.mid + 3.5}
                    >
                      {usd(bar.row.net)}
                    </text>
                    <text textAnchor="end" x={geom.tripsX} y={bar.mid + 3.5}>
                      {bar.row.trips}
                    </text>
                  </g>

                  <rect
                    fill="transparent"
                    height={ROW_H}
                    width={width}
                    x={0}
                    y={bar.top}
                  />
                </g>
              )
            })}
          </svg>
        ) : null}

        {tip && hovered && geom ? (
          <TooltipCard
            boundsHeight={height}
            boundsWidth={width}
            rows={[
              { label: "Round trips", value: hovered.trips },
              { label: "Fees paid", value: usd(hovered.fees) },
              { label: "Before fees", value: usd(hovered.net + hovered.fees) },
            ]}
            title={<MarketName className="text-xs" market={hovered.market} />}
            value={usd(hovered.net)}
            valueColor={
              hovered.net >= 0 ? "var(--long-ink)" : "var(--short-ink)"
            }
            x={tip.x}
            y={tip.y}
          />
        ) : null}
      </div>

      <p className="text-xs text-muted-foreground">
        Realized, all time, after every fee the position paid. The right-hand
        numeral is its closed round trips
        {omitted > 0
          ? ` · ${omitted} flatter market${omitted === 1 ? "" : "s"} not shown`
          : null}
        .
      </p>
    </div>
  )
}
