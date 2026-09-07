"use client";
import {
  CandlestickSeries,
  ColorType,
  createChart,
  type ISeriesApi,
  LineStyle,
  type UTCTimestamp,
} from "lightweight-charts";
import { useEffect, useRef, useState } from "react";
import { ShimmeringText } from "@/components/shimmering-text";
import {
  type Candle,
  type CandleInterval,
  fetchCandles,
  INTERVAL_MS,
  isFixtures,
} from "@/lib/candles";
import { cn } from "@/lib/utils";
import { useLiveFeed } from "@/lib/ws";

export type PriceLines = { entry?: number; sl?: number; tp?: number };

export type CandlePanelProps = {
  coin: string;
  interval?: CandleInterval;
  height?: number;
  lines?: PriceLines;
  className?: string;
};

function cssColor(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  const v = getComputedStyle(document.documentElement)
    .getPropertyValue(name)
    .trim();
  return v || fallback;
}

function toChartBar(b: Candle) {
  return {
    time: (b.t / 1000) as UTCTimestamp,
    open: b.o,
    high: b.h,
    low: b.l,
    close: b.c,
  };
}

export function CandlePanel({
  coin,
  interval = "1m",
  height = 380,
  lines,
  className,
}: CandlePanelProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const seriesRef = useRef<ISeriesApi<"Candlestick"> | null>(null);
  const lastBarRef = useRef<Candle | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [epoch, setEpoch] = useState(0);

  const entry = lines?.entry;
  const sl = lines?.sl;
  const tp = lines?.tp;

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    let disposed = false;
    setLoading(true);
    setError(null);

    const long = cssColor("--long", "#22c55e");
    const short = cssColor("--short", "#ef4444");
    const warning = cssColor("--warning", "#f59e0b");
    const border = cssColor("--border", "rgba(255,255,255,0.08)");
    const ring = cssColor("--ring", "#777");

    const chart = createChart(el, {
      layout: {
        background: { type: ColorType.Solid, color: "transparent" },
        textColor: cssColor("--muted-foreground", "#888"),
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
        vertLine: { color: ring, labelBackgroundColor: cssColor("--surface-raised", "#222") },
        horzLine: { color: ring, labelBackgroundColor: cssColor("--surface-raised", "#222") },
      },
    });
    const series = chart.addSeries(CandlestickSeries, {
      upColor: long,
      downColor: short,
      wickUpColor: long,
      wickDownColor: short,
      borderVisible: false,
    });
    seriesRef.current = series;

    const priceLines: Array<
      [number | undefined, string, string, LineStyle]
    > = [
      [entry, long, "ENTRY", LineStyle.Solid],
      [sl, short, "SL", LineStyle.Dashed],
      [tp, warning, "TP", LineStyle.Dashed],
    ];
    for (const [price, color, title, lineStyle] of priceLines) {
      if (price !== undefined) {
        series.createPriceLine({
          price,
          color,
          lineWidth: 1,
          lineStyle,
          axisLabelVisible: true,
          title,
        });
      }
    }

    fetchCandles(coin, interval)
      .then((bars) => {
        if (disposed) return;
        lastBarRef.current = bars.at(-1) ?? null;
        series.setData(bars.map(toChartBar));
        chart.timeScale().fitContent();
        setLoading(false);
      })
      .catch((e: unknown) => {
        if (!disposed) {
          setError(e instanceof Error ? e.message : String(e));
          setLoading(false);
        }
      });

    const ro = new ResizeObserver(() => {
      chart.applyOptions({ width: el.clientWidth });
    });
    ro.observe(el);

    return () => {
      disposed = true;
      ro.disconnect();
      seriesRef.current = null;
      lastBarRef.current = null;
      chart.remove();
    };
  }, [coin, interval, height, entry, sl, tp, epoch]);

  useLiveFeed({
    mids: (m) => {
      if (isFixtures()) return;
      const px = m[coin];
      const series = seriesRef.current;
      if (px === undefined || !series) return;
      const ms = INTERVAL_MS[interval];
      const now = Date.now();
      const prev = lastBarRef.current;
      const bar: Candle =
        !prev || now - prev.t >= ms
          ? { t: Math.floor(now / ms) * ms, o: px, h: px, l: px, c: px, v: 0 }
          : { ...prev, h: Math.max(prev.h, px), l: Math.min(prev.l, px), c: px };
      lastBarRef.current = bar;
      series.update(toChartBar(bar));
    },
  });

  return (
    <div className={cn("relative", className)} style={{ height }}>
      <div className="h-full w-full" ref={containerRef} />
      {loading && (
        <div className="absolute inset-0 grid place-items-center">
          <ShimmeringText
            className="font-mono text-[11px] tracking-[0.2em] text-muted-foreground"
            text={`LOADING ${coin.toUpperCase()}`}
          />
        </div>
      )}
      {error && !loading && (
        <div className="absolute inset-0 grid place-items-center">
          <div className="flex flex-col items-center gap-2">
            <span className="font-mono text-[11px] text-muted-foreground">
              candles unavailable — {error}
            </span>
            <button
              className="rounded-md border border-border px-2 py-1 font-mono text-[10px] text-foreground transition-colors hover:bg-muted"
              onClick={() => setEpoch((e) => e + 1)}
              type="button"
            >
              RETRY
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
