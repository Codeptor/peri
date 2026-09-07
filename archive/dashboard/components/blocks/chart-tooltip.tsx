"use client"

import * as React from "react"

import { cn } from "@/lib/utils"

/** One `label … value` line inside a tooltip. */
export type ChartTipRow = {
  label: string
  value: React.ReactNode
  /** CSS colour for the value — pass an *ink* token, the raw hue is too faint as text */
  color?: string
}

/** Whatever the chart needs to describe the hovered mark, plus its anchor. */
export type ChartTip<T> = {
  data: T
  /** anchor point in container-local pixels */
  x: number
  y: number
}

/** Shallow one level deep — enough to tell "same mark" from "new mark". */
function sameMark(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true
  if (
    typeof a !== "object" ||
    typeof b !== "object" ||
    a === null ||
    b === null
  ) {
    return false
  }
  const ka = Object.keys(a)
  const kb = Object.keys(b)
  if (ka.length !== kb.length) return false
  return ka.every((k) =>
    Object.is(
      (a as Record<string, unknown>)[k],
      (b as Record<string, unknown>)[k]
    )
  )
}

export type UseChartTooltip<T> = {
  /** attach to the chart's `relative` wrapper — it is both the measured box and the tooltip's frame */
  containerRef: React.RefObject<HTMLDivElement | null>
  width: number
  height: number
  tip: ChartTip<T> | null
  show: (data: T, x: number, y: number) => void
  hide: () => void
}

/**
 * Container measurement + hover state for every hand-rolled chart.
 *
 * One hook so sizing and tooltip behaviour are identical across primitives:
 * charts render nothing until `width > 0` (SSR emits just the sized box, the
 * first client paint has real geometry), and every tooltip anchors, clamps and
 * dismisses the same way.
 */
export function useChartTooltip<T>(): UseChartTooltip<T> {
  const containerRef = React.useRef<HTMLDivElement | null>(null)
  const [size, setSize] = React.useState({ width: 0, height: 0 })
  const [tip, setTip] = React.useState<ChartTip<T> | null>(null)

  React.useEffect(() => {
    const el = containerRef.current
    if (!el) return
    const measure = () => {
      const width = el.clientWidth
      const height = el.clientHeight
      // Bail on an unchanged box: ResizeObserver fires on layout passes that did
      // not actually resize us, and a fresh object every time would re-render.
      setSize((prev) =>
        prev.width === width && prev.height === height
          ? prev
          : { width, height }
      )
    }
    measure()
    const ro = new ResizeObserver(measure)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // Bail on an unchanged mark: the pointer-move charts call this on every event,
  // and a fresh object each time would re-render the whole chart at pointer rate.
  const show = React.useCallback((data: T, x: number, y: number) => {
    setTip((prev) =>
      prev && prev.x === x && prev.y === y && sameMark(prev.data, data)
        ? prev
        : { data, x, y }
    )
  }, [])

  const hide = React.useCallback(() => setTip(null), [])

  return {
    containerRef,
    width: size.width,
    height: size.height,
    tip,
    show,
    hide,
  }
}

export type TooltipCardProps = {
  /** anchor in container-local pixels — the card centres on `x` and sits above `y` */
  x: number
  y: number
  /** measured container box; the card is clamped inside it */
  boundsWidth: number
  boundsHeight?: number
  /**
   * `auto` keeps the card inside the chart box, flipping below the anchor when
   * sitting above it would clip. `above` pins it above and lets it overflow —
   * the only sane option for a chart only a few pixels tall.
   */
  placement?: "auto" | "above"
  /** small muted line above the title — timestamps, bucket ranges */
  meta?: React.ReactNode
  title?: React.ReactNode
  /** the headline figure */
  value?: React.ReactNode
  /** CSS colour for `value` — pass an *ink* token */
  valueColor?: string
  rows?: ChartTipRow[]
  children?: React.ReactNode
  className?: string
}

/** Layout effect on the client, plain effect on the server (never runs there). */
const useIsoLayoutEffect =
  typeof window === "undefined" ? React.useEffect : React.useLayoutEffect

const EDGE = 4
const ANCHOR_GAP = 10

/**
 * The one chart tooltip: popover surface, 12px, rounded-lg, theme-correct in
 * both palettes. It measures itself and clamps to the chart box, so a mark at
 * the far right edge does not push the card off the card.
 */
export function TooltipCard({
  x,
  y,
  boundsWidth,
  boundsHeight,
  placement = "auto",
  meta,
  title,
  value,
  valueColor,
  rows,
  children,
  className,
}: TooltipCardProps) {
  const ref = React.useRef<HTMLDivElement | null>(null)
  const [box, setBox] = React.useState({ w: 0, h: 0 })

  useIsoLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const w = el.offsetWidth
    const h = el.offsetHeight
    setBox((prev) => (prev.w === w && prev.h === h ? prev : { w, h }))
  })

  const measured = box.w > 0
  const left = measured
    ? Math.min(
        Math.max(x - box.w / 2, EDGE),
        Math.max(EDGE, boundsWidth - box.w - EDGE)
      )
    : x
  // Above the mark by default; flipped below when that would clip the top edge,
  // then pulled back inside if the flip would run past the bottom.
  let top = y - box.h - ANCHOR_GAP
  if (placement === "auto") {
    if (top < EDGE) top = y + ANCHOR_GAP
    if (boundsHeight != null && measured && top + box.h > boundsHeight - EDGE) {
      top = Math.max(EDGE, boundsHeight - box.h - EDGE)
    }
  }

  return (
    <div
      className={cn(
        "pointer-events-none absolute z-20 rounded-lg border border-border bg-popover px-2.5 py-1.5 text-xs whitespace-nowrap shadow-[var(--shadow-card)]",
        className
      )}
      ref={ref}
      // Hidden until measured so the card never paints one frame at the raw anchor.
      style={{ left, top, opacity: measured ? 1 : 0 }}
    >
      {meta != null ? (
        <div className="font-mono text-[10px] text-muted-foreground">
          {meta}
        </div>
      ) : null}
      {title != null ? (
        <div className="text-xs leading-4 font-medium text-foreground">
          {title}
        </div>
      ) : null}
      {value != null ? (
        <div
          className="font-mono text-[13px] leading-5 font-semibold"
          style={valueColor ? { color: valueColor } : undefined}
        >
          {value}
        </div>
      ) : null}
      {rows && rows.length > 0 ? (
        <div className="mt-0.5 flex flex-col gap-0.5">
          {rows.map((row) => (
            <div className="flex items-baseline gap-4" key={row.label}>
              <span className="text-[11px] text-muted-foreground">
                {row.label}
              </span>
              <span
                className="ml-auto font-mono text-[11.5px] text-foreground"
                style={row.color ? { color: row.color } : undefined}
              >
                {row.value}
              </span>
            </div>
          ))}
        </div>
      ) : null}
      {children}
    </div>
  )
}
