"use client"

import * as React from "react"

import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  monotonePath,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"

export type TrendPoint = { ts: number; value: number }

/** A dot pinned to the curve at an instant — a fill, an event, an annotation. */
export type TrendMarker = {
  ts: number
  /** CSS colour for the dot (a design token, never a raw hex) */
  color: string
  /** hollow = the thing it marks has not resolved yet (an open position) */
  hollow?: boolean
  /** first tooltip line — e.g. `SOL · Take profit` */
  title: string
  /** second tooltip line — e.g. `+$42.30` */
  detail?: string
}

/** Vertical shading over a time span — drawdown windows, halts, blackout periods. */
export type TrendBand = {
  from: number
  to: number
  color?: string
  label?: string
}

/**
 * Horizontal dashed threshold. Rendered only when it falls inside the y-domain —
 * a line pinned to an edge would claim a level the window never reached.
 * `color` paints both the dash and its label, so pass an *ink* token: the raw hue
 * sits near 2.7:1 as text on the light card.
 */
export type TrendRefLine = {
  value: number
  label: string
  color?: string
}

export type TrendAreaProps = {
  data: TrendPoint[]
  color?: string
  height?: number
  live?: boolean
  /** render the minimal mono axis labels (off for sparklines) */
  axis?: boolean
  /**
   * Pin the window's min, max and last values to their points. Defaults to
   * `axis`, and replaces the right-edge hi/lo pair when on — the same two
   * numbers, said where they happened.
   */
  annotations?: boolean
  /** value formatter used by the hover tooltip */
  format?: (v: number) => string
  /** 2.25 is the hero weight (amendment §3); sparklines pass something thinner */
  strokeWidth?: number
  /** optional overlays — every call site that omits them renders exactly as before */
  markers?: TrendMarker[]
  bands?: TrendBand[]
  refLines?: TrendRefLine[]
  className?: string
}

/** Shared empty defaults: a literal `[]` per render would churn every overlay memo. */
const NO_MARKERS: TrendMarker[] = []
const NO_BANDS: TrendBand[] = []
const NO_REF_LINES: TrendRefLine[] = []

type TrendTip = { kind: "point" | "marker"; index: number }

/**
 * Fractional index of `ts` inside an ascending series, or null when it falls outside.
 * The x-axis is index-linear, not time-linear, so an overlay pinned to a wall-clock
 * instant has to be resolved through the samples that bracket it.
 */
function locate(data: TrendPoint[], ts: number): number | null {
  const n = data.length
  if (n === 0) return null
  if (n === 1) return ts === data[0].ts ? 0 : null
  if (ts < data[0].ts || ts > data[n - 1].ts) return null
  let lo = 0
  let hi = n - 1
  while (hi - lo > 1) {
    const mid = (lo + hi) >> 1
    if (data[mid].ts <= ts) lo = mid
    else hi = mid
  }
  const span = data[hi].ts - data[lo].ts
  return span === 0 ? lo : lo + (ts - data[lo].ts) / span
}

/** Pixel position at a fractional index — linear between the two neighbouring samples. */
function at(pts: [number, number][], f: number): [number, number] {
  const i = Math.min(pts.length - 1, Math.max(0, Math.floor(f)))
  const j = Math.min(pts.length - 1, i + 1)
  const t = f - i
  return [
    pts[i][0] + (pts[j][0] - pts[i][0]) * t,
    pts[i][1] + (pts[j][1] - pts[i][1]) * t,
  ]
}

function hhmm(ts: number): string {
  return new Date(ts).toISOString().slice(11, 16)
}

function stamp(ts: number): string {
  return new Date(ts).toISOString().slice(5, 16).replace("T", " ")
}

/** Soft gradient area chart: smooth curve, fill fading to transparent, dark hover card. */
export function TrendArea({
  data,
  color = "var(--accent-orange)",
  height = 160,
  live = false,
  axis = false,
  annotations,
  format = usd,
  strokeWidth = 2.25,
  markers = NO_MARKERS,
  bands = NO_BANDS,
  refLines = NO_REF_LINES,
  className,
}: TrendAreaProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<TrendTip>()
  // A marker owns the pointer while it is under it: pointermove bubbles up from
  // the marker's hit circle and would otherwise overwrite the more specific tip.
  const overMarker = React.useRef<number | null>(null)
  const gradientId = `trend-${React.useId().replace(/[^a-zA-Z0-9]/g, "")}`

  const annotate = annotations ?? axis
  const padTop = annotate ? 20 : 10
  const padRight = annotate ? 8 : axis ? 44 : 3
  const padBottom = annotate ? (axis ? 26 : 18) : axis ? 16 : 3
  const padLeft = 3

  const geom = React.useMemo(() => {
    if (width <= 0 || data.length === 0) return null
    const innerW = Math.max(1, width - padLeft - padRight)
    const innerH = Math.max(1, height - padTop - padBottom)
    let lo = Number.POSITIVE_INFINITY
    let hi = Number.NEGATIVE_INFINITY
    let loIdx = 0
    let hiIdx = 0
    data.forEach((p, i) => {
      if (p.value < lo) {
        lo = p.value
        loIdx = i
      }
      if (p.value > hi) {
        hi = p.value
        hiIdx = i
      }
    })
    const flat = hi === lo
    const pts = data.map((p, i): [number, number] => [
      data.length === 1
        ? padLeft + innerW / 2
        : padLeft + (i * innerW) / (data.length - 1),
      flat
        ? padTop + innerH / 2
        : padTop + (1 - (p.value - lo) / (hi - lo)) * innerH,
    ])
    const baseline = padTop + innerH
    const line = monotonePath(pts)
    const last = pts[pts.length - 1]
    return {
      pts,
      line,
      area: `${line} L ${last[0]} ${baseline} L ${pts[0][0]} ${baseline} Z`,
      lo,
      hi,
      loIdx,
      hiIdx,
      flat,
      innerH,
      innerW,
      baseline,
      last,
    }
  }, [data, width, height, padTop, padRight, padBottom])

  // Markers outside the rendered window are dropped, never clamped — a fill from
  // before the range must not pile up on the left edge and read as a fill inside it.
  const markerGeom = React.useMemo(() => {
    if (!geom || markers.length === 0) return []
    return markers.flatMap((marker) => {
      const f = locate(data, marker.ts)
      if (f == null) return []
      const [x, y] = at(geom.pts, f)
      return [{ marker, x, y }]
    })
  }, [geom, markers, data])

  // Bands ARE clamped: a drawdown that opened before the window is still underwater
  // inside it, and shading only the visible part tells the truth about this window.
  const bandGeom = React.useMemo(() => {
    if (!geom || bands.length === 0 || data.length === 0) return []
    const first = data[0].ts
    const last = data[data.length - 1].ts
    return bands.flatMap((band) => {
      const from = Math.max(band.from, first)
      const to = Math.min(band.to, last)
      if (to <= from) return []
      const a = locate(data, from)
      const b = locate(data, to)
      if (a == null || b == null) return []
      const x = at(geom.pts, a)[0]
      return [{ band, x, width: Math.max(1, at(geom.pts, b)[0] - x) }]
    })
  }, [geom, bands, data])

  const refGeom = React.useMemo(() => {
    if (!geom || refLines.length === 0 || geom.hi === geom.lo) return []
    return refLines.flatMap((line) => {
      if (!Number.isFinite(line.value)) return []
      if (line.value < geom.lo || line.value > geom.hi) return []
      const y =
        padTop +
        (1 - (line.value - geom.lo) / (geom.hi - geom.lo)) * geom.innerH
      return [{ line, y }]
    })
  }, [geom, refLines, padTop])

  /**
   * Extreme labels, collision-avoided: the last value always wins, an extreme
   * that *is* the last point is dropped rather than doubled, and one that would
   * land on top of the last label is dropped too.
   */
  const extremes = React.useMemo(() => {
    if (!geom || !annotate || data.length === 0) return null
    const lastIdx = data.length - 1
    const [lastX, lastY] = geom.last
    const lastLabel = {
      x: lastX,
      y: lastY - 9 < padTop + 2 ? lastY + 15 : lastY - 9,
      text: format(data[lastIdx].value),
    }
    const place = (idx: number, above: boolean) => {
      if (geom.flat || idx === lastIdx) return null
      const [x, y] = geom.pts[idx]
      const ly = above ? y - 7 : y + 12
      if (Math.abs(x - lastLabel.x) < 62 && Math.abs(ly - lastLabel.y) < 14) {
        return null
      }
      const nearLeft = x - padLeft < 26
      const nearRight = padLeft + geom.innerW - x < 40
      return {
        x: nearLeft ? padLeft : nearRight ? padLeft + geom.innerW : x,
        y: ly,
        anchor: nearLeft ? "start" : nearRight ? "end" : "middle",
        text: format(data[idx].value),
      } as const
    }
    return {
      last: lastLabel,
      max: place(geom.hiIdx, true),
      min: place(geom.loIdx, false),
    }
  }, [geom, annotate, data, format, padTop])

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!geom || overMarker.current != null) return
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left
    let best = 0
    let bestDist = Number.POSITIVE_INFINITY
    for (let i = 0; i < geom.pts.length; i++) {
      const dist = Math.abs(geom.pts[i][0] - x)
      if (dist < bestDist) {
        bestDist = dist
        best = i
      }
    }
    show({ kind: "point", index: best }, geom.pts[best][0], geom.pts[best][1])
  }

  const onLeave = () => {
    overMarker.current = null
    hide()
  }

  const hoverIndex = tip?.data.kind === "point" && geom ? tip.data.index : null
  const hoverMarker = tip?.data.kind === "marker" ? tip.data.index : null
  const pinned = hoverMarker != null ? (markerGeom[hoverMarker] ?? null) : null
  const hovered = hoverIndex != null && geom ? geom.pts[hoverIndex] : null
  // A band describes the sample under the pointer, so it rides the real hover
  // card rather than a native `<title>` firing a second later on top of it.
  const hoveredBand = hovered
    ? (bandGeom.find((b) => hovered[0] >= b.x && hovered[0] <= b.x + b.width)
        ?.band.label ?? null)
    : null

  return (
    <div
      className={cn("relative w-full", className)}
      onPointerLeave={onLeave}
      onPointerMove={onMove}
      ref={containerRef}
      style={{ height }}
    >
      {data.length === 0 ? (
        <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
          —
        </div>
      ) : geom ? (
        <svg
          aria-label={`Trend area, ${data.length} samples from ${stamp(data[0].ts)} to ${stamp(data[data.length - 1].ts)}`}
          className="block"
          height={height}
          role="img"
          width={width}
        >
          <defs>
            <linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0%" stopColor={color} stopOpacity={0.28} />
              <stop offset="100%" stopColor={color} stopOpacity={0} />
            </linearGradient>
          </defs>
          {bandGeom.map((b, i) => (
            <rect
              fill={b.band.color ?? "var(--short)"}
              height={geom.baseline - padTop}
              key={i}
              opacity={0.09}
              width={b.width}
              x={b.x}
              y={padTop}
            />
          ))}
          <path d={geom.area} fill={`url(#${gradientId})`} />
          <path
            d={geom.line}
            fill="none"
            stroke={color}
            strokeLinecap="round"
            strokeLinejoin="round"
            strokeWidth={strokeWidth}
          />
          {refGeom.map((r, i) => (
            <g key={i}>
              <line
                stroke={r.line.color ?? "var(--short-ink)"}
                strokeDasharray="4 4"
                strokeWidth={1.25}
                x1={padLeft}
                x2={width - padRight}
                y1={r.y}
                y2={r.y}
              />
              <text
                className={AXIS_TICK_CLASS}
                fill={r.line.color ?? "var(--short-ink)"}
                fontSize={AXIS_TICK_SIZE}
                textAnchor="start"
                x={padLeft + 2}
                y={Math.max(padTop + 8, r.y - 4)}
              >
                {r.line.label}
              </text>
            </g>
          ))}
          {axis ? (
            <g
              className={AXIS_TICK_CLASS}
              fill={AXIS_TICK_FILL}
              fontSize={AXIS_TICK_SIZE}
            >
              {annotate ? null : (
                <>
                  <text x={width - padRight + 7} y={padTop + 4}>
                    {format(geom.hi)}
                  </text>
                  <text x={width - padRight + 7} y={geom.baseline}>
                    {format(geom.lo)}
                  </text>
                </>
              )}
              <text textAnchor="start" x={padLeft} y={height - 3}>
                {hhmm(data[0].ts)}
              </text>
              {data.length > 2 ? (
                <text
                  textAnchor="middle"
                  x={padLeft + geom.innerW / 2}
                  y={height - 3}
                >
                  {hhmm(data[Math.floor(data.length / 2)].ts)}
                </text>
              ) : null}
              <text textAnchor="end" x={width - padRight} y={height - 3}>
                {hhmm(data[data.length - 1].ts)}
              </text>
            </g>
          ) : null}
          {extremes ? (
            <g className={AXIS_TICK_CLASS} fontSize={AXIS_TICK_SIZE}>
              {extremes.max ? (
                <text
                  fill={AXIS_TICK_FILL}
                  textAnchor={extremes.max.anchor}
                  x={extremes.max.x}
                  y={extremes.max.y}
                >
                  {extremes.max.text}
                </text>
              ) : null}
              {extremes.min ? (
                <text
                  fill={AXIS_TICK_FILL}
                  textAnchor={extremes.min.anchor}
                  x={extremes.min.x}
                  y={extremes.min.y}
                >
                  {extremes.min.text}
                </text>
              ) : null}
              <text
                fill="var(--foreground)"
                fontWeight={600}
                textAnchor="end"
                x={extremes.last.x}
                y={extremes.last.y}
              >
                {extremes.last.text}
              </text>
            </g>
          ) : null}
          {live ? (
            <g>
              <circle
                className="animate-ping"
                cx={geom.last[0]}
                cy={geom.last[1]}
                fill={color}
                opacity={0.25}
                r={6}
                style={{ transformBox: "fill-box", transformOrigin: "center" }}
              />
              <circle
                cx={geom.last[0]}
                cy={geom.last[1]}
                fill={color}
                r={2.75}
              />
            </g>
          ) : null}
          {hovered ? (
            <g>
              <line
                stroke="var(--border)"
                strokeWidth={1}
                x1={hovered[0]}
                x2={hovered[0]}
                y1={padTop}
                y2={geom.baseline}
              />
              <circle
                cx={hovered[0]}
                cy={hovered[1]}
                fill={color}
                r={3.5}
                stroke="var(--background)"
                strokeWidth={2}
              />
            </g>
          ) : null}
          {markerGeom.map((m, i) => (
            <g key={i}>
              <circle
                cx={m.x}
                cy={m.y}
                fill={m.marker.hollow ? "var(--background)" : m.marker.color}
                r={hoverMarker === i ? 4.25 : 3.25}
                stroke={m.marker.color}
                strokeWidth={m.marker.hollow ? 1.75 : 1.25}
              />
              <circle
                cx={m.x}
                cy={m.y}
                fill="transparent"
                onPointerEnter={() => {
                  overMarker.current = i
                  show({ kind: "marker", index: i }, m.x, m.y)
                }}
                onPointerLeave={() => {
                  overMarker.current = null
                }}
                r={8}
              />
            </g>
          ))}
        </svg>
      ) : null}
      {tip && pinned ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          meta={stamp(pinned.marker.ts)}
          title={pinned.marker.title}
          value={pinned.marker.detail}
          x={pinned.x}
          y={pinned.y}
        />
      ) : tip && hoverIndex != null && hovered ? (
        <TooltipCard
          boundsHeight={height}
          boundsWidth={width}
          meta={stamp(data[hoverIndex].ts)}
          title={hoveredBand}
          value={format(data[hoverIndex].value)}
          x={hovered[0]}
          y={hovered[1]}
        />
      ) : null}
    </div>
  )
}
