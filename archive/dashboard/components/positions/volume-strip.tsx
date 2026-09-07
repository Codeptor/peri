"use client"

import * as React from "react"

import { type Candle, type CandleInterval, fetchCandles } from "@/lib/candles"
import { compact, fmtPrice, signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  AXIS_TICK_CLASS,
  AXIS_TICK_FILL,
  AXIS_TICK_SIZE,
  GRID_STROKE,
  roundedBarPath,
} from "@/components/blocks/chart-geometry"
import { TooltipCard, useChartTooltip } from "@/components/blocks/chart-tooltip"
import { utcStamp } from "@/components/positions/util"

/**
 * The lookback `CandlePanel` asks for (`fetchCandles`' own default). Pinned
 * explicitly here so the strip and the candles above it can never drift onto
 * different windows through a change to that default.
 */
export const VOLUME_LOOKBACK_H = 12

/**
 * Width reserved on the right for `CandlePanel`'s price scale so a bar lands
 * under the candle it belongs to. lightweight-charts sizes that scale from its
 * own label text and exposes no way to read it back, so this is a calibrated
 * constant, not a measurement — alignment is close, not exact, and it drifts
 * once the user pans or zooms the candles (see the component doc).
 */
const PRICE_SCALE_W = 58

const PAD_TOP = 3

const NO_BARS: Candle[] = []

/** Base-asset volume: compact above 1000, otherwise enough digits to differ. */
function vol(n: number): string {
  if (!Number.isFinite(n)) return "—"
  if (n >= 1000) return compact(n)
  if (n >= 10) return n.toFixed(1)
  if (n >= 1) return n.toFixed(2)
  return n.toFixed(3)
}

export type VolumeStripProps = {
  coin: string
  interval: CandleInterval
  height?: number
  /** width held clear on the right for the candle panel's price scale */
  gutter?: number
  className?: string
}

/**
 * Per-bar traded volume, drawn to sit directly under `CandlePanel`.
 *
 * `CandlePanel` is shared and takes no volume series, so the strip re-runs the
 * same `fetchCandles(coin, interval, 12h)` query and draws the `v` field the
 * panel throws away. Consequences, stated rather than hidden:
 *
 * - It fetches once per `(coin, interval)` and never polls — exactly like the
 *   panel, which only ever extends its own last bar from the mid stream. Both
 *   therefore show the same historical window from the same moment.
 * - The x mapping is reconstructed (`gutter` above, bars centred in equal
 *   slots), not read from the chart. It lines up at the panel's initial
 *   `fitContent()` view and stops lining up if the candles are panned or
 *   zoomed.
 * - The forming bar carries no volume: the panel synthesises it from mids,
 *   which have no size, so the strip simply has one fewer bar than the panel.
 *
 * Bars are coloured by their own candle's direction, matching the candles above.
 */
export function VolumeStrip({
  coin,
  interval,
  height = 46,
  gutter = PRICE_SCALE_W,
  className,
}: VolumeStripProps) {
  const { containerRef, width, tip, show, hide } = useChartTooltip<number>()
  const key = `${coin}|${interval}`
  // Request identity travels with the payload instead of an effect clearing
  // state up front, so a coin switch shows nothing rather than the last coin's
  // volume under the new coin's candles.
  const [loaded, setLoaded] = React.useState<{
    key: string
    bars: Candle[]
  } | null>(null)
  const [failure, setFailure] = React.useState<{
    key: string
    message: string
  } | null>(null)
  // The strip owns its retry: `CandlePanel`'s Retry button bumps an epoch
  // private to that component, so a recovered panel would otherwise sit above a
  // permanently blank strip.
  const [attempt, setAttempt] = React.useState(0)

  React.useEffect(() => {
    let cancelled = false
    fetchCandles(coin, interval, VOLUME_LOOKBACK_H)
      .then((bars) => {
        if (!cancelled) setLoaded({ key, bars })
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setFailure({
            key,
            message: e instanceof Error ? e.message : String(e),
          })
        }
      })
    return () => {
      cancelled = true
    }
  }, [coin, interval, key, attempt])

  const bars = loaded?.key === key ? loaded.bars : NO_BARS
  const failed = failure?.key === key && bars.length === 0

  const geom = React.useMemo(() => {
    if (width <= 0 || bars.length === 0) return null
    const innerW = Math.max(1, width - gutter)
    const innerH = Math.max(1, height - PAD_TOP)
    let max = 0
    for (const b of bars) {
      if (Number.isFinite(b.v) && b.v > max) max = b.v
    }
    const slot = innerW / bars.length
    const barW = Math.max(1, slot * 0.74)
    const radius = Math.min(1.5, barW / 2)
    const base = PAD_TOP + innerH
    const centers: number[] = new Array(bars.length)
    const tops: number[] = new Array(bars.length)
    // One merged path per direction: a 12h 1m window is 720 bars, and 720
    // nodes plus 720 hit rects is a lot of DOM for a 46px strip.
    let up = ""
    let down = ""
    bars.forEach((b, i) => {
      const cx = slot * (i + 0.5)
      const v = Number.isFinite(b.v) ? Math.max(0, b.v) : 0
      const h = max > 0 && v > 0 ? Math.max(1, (v / max) * innerH) : 0
      const y = base - h
      centers[i] = cx
      tops[i] = h > 0 ? y : base
      if (h > 0) {
        const d = roundedBarPath(cx - barW / 2, y, barW, h, radius, "top")
        if (b.c >= b.o) up += d
        else down += d
      }
    })
    return { innerW, innerH, slot, base, max, up, down, centers, tops }
  }, [bars, width, height, gutter])

  const onMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!geom) return
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left
    const i = Math.min(bars.length - 1, Math.max(0, Math.floor(x / geom.slot)))
    show(i, geom.centers[i], geom.tops[i])
  }

  const hovered = tip && geom ? (bars[tip.data] ?? null) : null
  const change =
    hovered && hovered.o !== 0 ? ((hovered.c - hovered.o) / hovered.o) * 100 : 0

  return (
    <div className={cn("min-w-0", className)}>
      <div className="flex items-baseline gap-2 pb-1">
        <span className="text-[11px] leading-4 text-muted-foreground">
          Volume
        </span>
        {failed ? (
          // The reason is written out rather than hidden behind a native
          // `title` tooltip — the design system has no `<title>`-only hovers.
          <span className="ml-auto flex min-w-0 items-baseline gap-2">
            <span className="truncate font-mono text-[10px] leading-4 text-muted-foreground">
              {failure?.message}
            </span>
            <button
              className="shrink-0 text-[10.5px] leading-4 text-muted-foreground underline-offset-2 transition-colors hover:text-foreground hover:underline"
              onClick={() => setAttempt((a) => a + 1)}
              type="button"
            >
              Retry
            </button>
          </span>
        ) : (
          <span className="ml-auto shrink-0 font-mono text-[10px] leading-4 text-muted-foreground">
            {`${interval} · ${VOLUME_LOOKBACK_H}h`}
          </span>
        )}
      </div>
      <div
        className="relative w-full"
        onPointerLeave={hide}
        onPointerMove={onMove}
        ref={containerRef}
        style={{ height }}
      >
        {geom ? (
          <svg
            aria-label={`Traded volume per ${interval} bar for ${coin}, ${bars.length} bars, peak ${vol(geom.max)}`}
            className="block"
            height={height}
            role="img"
            width={width}
          >
            {tip ? (
              <rect
                fill="var(--cell)"
                height={geom.innerH}
                width={Math.max(1.5, geom.slot)}
                x={geom.centers[tip.data] - Math.max(1.5, geom.slot) / 2}
                y={PAD_TOP}
              />
            ) : null}
            {geom.down ? (
              <path d={geom.down} fill="var(--short)" opacity={0.65} />
            ) : null}
            {geom.up ? (
              <path d={geom.up} fill="var(--long)" opacity={0.65} />
            ) : null}
            <line
              stroke={GRID_STROKE}
              strokeWidth={1}
              x1={0}
              x2={geom.innerW}
              y1={geom.base}
              y2={geom.base}
            />
            <text
              className={AXIS_TICK_CLASS}
              fill={AXIS_TICK_FILL}
              fontSize={AXIS_TICK_SIZE}
              x={geom.innerW + 7}
              y={PAD_TOP + 8}
            >
              {vol(geom.max)}
            </text>
          </svg>
        ) : (
          <div className="grid h-full place-items-center font-mono text-[11px] text-muted-foreground">
            —
          </div>
        )}
        {tip && hovered && geom ? (
          <TooltipCard
            boundsWidth={width}
            meta={utcStamp(hovered.t)}
            placement="above"
            rows={[
              { label: "Close", value: fmtPrice(hovered.c) },
              {
                label: "Bar",
                value: signedPct(change),
                color:
                  change > 0
                    ? "var(--long-ink)"
                    : change < 0
                      ? "var(--short-ink)"
                      : undefined,
              },
            ]}
            title="Volume"
            value={vol(hovered.v)}
            x={geom.centers[tip.data]}
            y={geom.tops[tip.data]}
          />
        ) : null}
      </div>
    </div>
  )
}
