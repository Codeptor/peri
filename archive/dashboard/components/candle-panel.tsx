"use client"

import { useEffect, useRef, useState } from "react"
import {
  CandlestickSeries,
  ColorType,
  createChart,
  type IChartApi,
  type ISeriesApi,
  LineStyle,
  type UTCTimestamp,
} from "lightweight-charts"
import { useTheme } from "next-themes"

import {
  type Candle,
  type CandleInterval,
  fetchCandles,
  INTERVAL_MS,
  isFixtures,
} from "@/lib/candles"
import { cn } from "@/lib/utils"
import { useLiveFeed } from "@/lib/ws"

export type PriceLines = { entry?: number; sl?: number; tp?: number }

export type CandlePanelProps = {
  coin: string
  interval?: CandleInterval
  height?: number
  lines?: PriceLines
  className?: string
}

/**
 * lightweight-charts resolves a colour by setting `style.color` on a throwaway
 * div and regex-matching the *computed* value against legacy `rgb()`/`rgba()`
 * only. Our tokens are all `oklch()`, which the browser now keeps in its own
 * colour space all the way through — computed style hands back `oklch()`/`lab()`,
 * no match, and the parser throws from inside `createChart`.
 *
 * So the conversion has to leave CSS entirely: paint the colour onto a 1×1
 * sRGB canvas and read the pixel. Note that *serialising* `fillStyle` is not
 * enough — Chrome round-trips `oklch(…)` verbatim through the getter — only the
 * rasterised pixel is guaranteed to be legacy sRGB. Context is cached
 * module-wide and only ever touched from the mount effect, so SSR never sees it.
 */
let colorProbe: CanvasRenderingContext2D | null | undefined

/**
 * Paint `value` and read it back as `rgb()`/`rgba()`. `fillStyle` silently
 * *ignores* a value it cannot parse, leaving the previous one in place — hence
 * the seed, which is what gets painted when `value` is junk.
 */
function probeColor(
  ctx: CanvasRenderingContext2D,
  seed: string,
  value: string,
): string {
  ctx.fillStyle = seed
  ctx.fillStyle = value
  ctx.fillRect(0, 0, 1, 1)
  const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data
  if (a === 255) return `rgb(${r}, ${g}, ${b})`
  return `rgba(${r}, ${g}, ${b}, ${Math.round((a / 255) * 1000) / 1000})`
}

/** Legacy-form colour, or null when the browser cannot parse `value`. */
function normalizeColor(value: string): string | null {
  if (colorProbe === undefined) {
    const ctx = document
      .createElement("canvas")
      .getContext("2d", { willReadFrequently: true })
    // `copy` instead of the default `source-over`: each probe must *replace*
    // the pixel, not blend onto whatever the previous one left behind.
    if (ctx) ctx.globalCompositeOperation = "copy"
    colorProbe = ctx
  }
  if (!colorProbe) return null
  // Two different seeds: a real colour paints the same pixel twice, an
  // unparseable one leaves each seed to paint itself and they disagree.
  const first = probeColor(colorProbe, "#010203", value)
  const second = probeColor(colorProbe, "#040506", value)
  return first === second ? first : null
}

/**
 * Resolve a token to a colour lightweight-charts can parse. Every name below is
 * defined in `globals.css` for both themes, so `fallback` is a theme-neutral
 * grey that only surfaces if a token is renamed out from under us — never a
 * dark-only literal, which is what used to leak into the light theme.
 */
function cssColor(name: string, fallback = "#808080"): string {
  const raw = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim()
  if (!raw) return fallback
  return normalizeColor(raw) ?? fallback
}

function toChartBar(b: Candle) {
  return {
    time: (b.t / 1000) as UTCTimestamp,
    open: b.o,
    high: b.h,
    low: b.l,
    close: b.c,
  }
}

export function CandlePanel({
  coin,
  interval = "1m",
  height = 380,
  lines,
  className,
}: CandlePanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null)
  const seriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null)
  const lastBarRef = useRef<Candle | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [epoch, setEpoch] = useState(0)
  // lightweight-charts resolves every colour once, at creation — so the whole
  // chart is rebuilt when the resolved theme flips.
  const { resolvedTheme } = useTheme()

  const entry = lines?.entry
  const sl = lines?.sl
  const tp = lines?.tp

  useEffect(() => {
    const el = containerRef.current
    // `resolvedTheme` is undefined until next-themes reads the system query
    // (one tick after mount) — waiting for it costs a frame of spinner and
    // saves building the whole chart against unresolved colours.
    if (!el || !resolvedTheme) return
    let disposed = false
    setLoading(true)
    setError(null)

    const long = cssColor("--long")
    const short = cssColor("--short")
    const warning = cssColor("--warning")
    const border = cssColor("--border")
    const ring = cssColor("--ring")
    const popover = cssColor("--popover")

    // Everything lightweight-charts parses lives in here: a colour it chokes on
    // must surface as the panel's own error card, not take the route down.
    let chart: IChartApi
    let series: ISeriesApi<"Candlestick">
    // Holds a half-built chart so a throw after `createChart` can still dispose
    // it — otherwise every Retry click leaks a canvas and a resize listener.
    let unfinished: IChartApi | null = null
    try {
      chart = createChart(el, {
        layout: {
          background: { type: ColorType.Solid, color: "transparent" },
          textColor: cssColor("--muted-foreground"),
          fontSize: 10,
          attributionLogo: false,
        },
        grid: {
          vertLines: { color: border },
          horzLines: { color: border },
        },
        width: el.clientWidth,
        height,
        timeScale: {
          timeVisible: true,
          secondsVisible: false,
          borderColor: border,
        },
        rightPriceScale: { borderColor: border },
        crosshair: {
          vertLine: { color: ring, labelBackgroundColor: popover },
          horzLine: { color: ring, labelBackgroundColor: popover },
        },
      })
      unfinished = chart
      series = chart.addSeries(CandlestickSeries, {
        upColor: long,
        downColor: short,
        wickUpColor: long,
        wickDownColor: short,
        borderVisible: false,
      })

      const priceLines: Array<[number | undefined, string, string, LineStyle]> =
        [
          [entry, long, "Entry", LineStyle.Solid],
          [sl, short, "SL", LineStyle.Dashed],
          [tp, warning, "TP", LineStyle.Dashed],
        ]
      for (const [price, color, title, lineStyle] of priceLines) {
        if (price !== undefined) {
          series.createPriceLine({
            price,
            color,
            lineWidth: 1,
            lineStyle,
            axisLabelVisible: true,
            title,
          })
        }
      }
      unfinished = null
    } catch (e: unknown) {
      unfinished?.remove()
      // set-state-in-effect guards against cascading renders; there is no
      // cascade to guard here — `error` is not a dependency, so this fires once
      // per failed build. Deferring it into a callback would only let a stale
      // failure land after the next run has already cleared the error.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setError(`chart failed: ${e instanceof Error ? e.message : String(e)}`)
      setLoading(false)
      return
    }
    seriesRef.current = series

    fetchCandles(coin, interval)
      .then((bars) => {
        if (disposed) return
        lastBarRef.current = bars.at(-1) ?? null
        series.setData(bars.map(toChartBar))
        chart.timeScale().fitContent()
        setLoading(false)
      })
      .catch((e: unknown) => {
        if (!disposed) {
          setError(e instanceof Error ? e.message : String(e))
          setLoading(false)
        }
      })

    const ro = new ResizeObserver(() => {
      chart.applyOptions({ width: el.clientWidth })
    })
    ro.observe(el)

    return () => {
      disposed = true
      ro.disconnect()
      seriesRef.current = null
      lastBarRef.current = null
      chart.remove()
    }
  }, [coin, interval, height, entry, sl, tp, epoch, resolvedTheme])

  useLiveFeed({
    mids: (m) => {
      if (isFixtures()) return
      const px = m[coin]
      const series = seriesRef.current
      if (px === undefined || !series) return
      const ms = INTERVAL_MS[interval]
      const now = Date.now()
      const prev = lastBarRef.current
      const bar: Candle =
        !prev || now - prev.t >= ms
          ? { t: Math.floor(now / ms) * ms, o: px, h: px, l: px, c: px, v: 0 }
          : { ...prev, h: Math.max(prev.h, px), l: Math.min(prev.l, px), c: px }
      lastBarRef.current = bar
      series.update(toChartBar(bar))
    },
  })

  return (
    <div className={cn("relative", className)} style={{ height }}>
      <div className="h-full w-full" ref={containerRef} />
      {loading && (
        <div className="absolute inset-0 grid place-items-center">
          <span className="animate-pulse text-[13px] text-muted-foreground">
            Loading {coin} candles…
          </span>
        </div>
      )}
      {error && !loading && (
        <div className="absolute inset-0 grid place-items-center">
          <div className="flex flex-col items-center gap-2">
            <span className="text-[13px] text-muted-foreground">
              Candles unavailable — {error}
            </span>
            <button
              className="rounded-lg border border-border px-2.5 py-1 text-[13px] font-medium text-foreground transition-colors hover:bg-muted"
              onClick={() => setEpoch((e) => e + 1)}
              type="button"
            >
              Retry
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
