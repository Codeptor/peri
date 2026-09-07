import type { CSSProperties, ReactNode } from "react"

import type { Features, MarketRow, Nominee } from "@/lib/api"
import { signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"

/** One screener line: the snapshot row plus what the page derives on top. */
export type ScreenerRow = {
  market: string
  data: MarketRow
  /** live mark — websocket mids win over the polled snapshot */
  mark: number
  /** % move vs the previous UTC day close, null when unknown */
  chg24h: number | null
  nominee: Nominee | null
}

/**
 * Heat cap. The spec allows 20%, but 20% of a mid-chroma hue over the near-white
 * light card floods a six-column feature block — at 16% the loud cells still
 * separate from the calm ones and the light ground stays readable. Same alpha in
 * dark, where the hue is lifted and reads at least as strongly.
 */
const TINT_MAX_PCT = 16

export type TintHue = "long" | "short" | "accent-orange"

/**
 * Soft cell wash whose weight tracks |value| against the column's own extreme:
 * nothing at the calmest cell, 16% of the hue at the loudest.
 *
 * `base` is what the hue is mixed into. Table cells leave it `transparent` so
 * the mix stays translucent and the row-hover wash reads through. Tiles that
 * already carry a ground pass that ground's token (`var(--surface-2)`) instead
 * — mixing into `transparent` there would punch a hole and make a hot tile look
 * *lighter* than a calm one on the light theme.
 */
export function tintStyle(
  ratio: number,
  hue: TintHue,
  base = "transparent"
): CSSProperties | undefined {
  const safe = Number.isFinite(ratio) ? ratio : 0
  const alpha = Math.min(1, Math.max(0, safe)) * TINT_MAX_PCT
  if (alpha < 1) return undefined
  return {
    backgroundColor: `color-mix(in oklch, var(--${hue}) ${alpha.toFixed(0)}%, ${base})`,
  }
}

/** `+2.90` / `-1.80` / `—` — mono-friendly signed fixed. */
export function signedFixed(v: number | null | undefined, digits = 2): string {
  if (v == null || Number.isNaN(v)) return "—"
  return `${v > 0 ? "+" : ""}${v.toFixed(digits)}`
}

/** Funding rate is a tiny per-hour fraction — basis points read far better. */
export function fundingBp(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return "—"
  return signedFixed(v * 10_000, 2)
}

export type FeatureKey = keyof Features

export type FeatureMeta = {
  key: FeatureKey
  /** sans, normal case — it is a column label, not a chart tick (amendment §2) */
  head: string
  /** returns carry PnL direction → long/short; the rest are magnitude → orange */
  signed: boolean
  format: (v: number) => string
  /** raw tint weight; the table normalises it against the column max */
  weight: (v: number) => number
}

export const FEATURES: FeatureMeta[] = [
  {
    key: "r5m",
    head: "5m ret",
    signed: true,
    format: signedPct,
    weight: Math.abs,
  },
  {
    key: "r1h",
    head: "1h ret",
    signed: true,
    format: signedPct,
    weight: Math.abs,
  },
  {
    key: "r24h",
    head: "24h ret",
    signed: true,
    format: signedPct,
    weight: Math.abs,
  },
  {
    key: "vol1h",
    head: "Vol 1h",
    signed: false,
    format: (v) => v.toFixed(2),
    weight: Math.abs,
  },
  {
    key: "funding_z",
    head: "Funding z",
    signed: false,
    format: (v) => signedFixed(v),
    weight: Math.abs,
  },
  {
    key: "range_pos",
    head: "Range",
    signed: false,
    format: (v) => `${Math.round(v * 100)}%`,
    weight: (v) => Math.abs(v - 0.5) * 2,
  },
]

/** Loudest weight per feature column across the whole universe. */
export function featureMaxima(rows: ScreenerRow[]): Map<FeatureKey, number> {
  const out = new Map<FeatureKey, number>()
  for (const meta of FEATURES) {
    let max = 0
    for (const row of rows) {
      const features = row.data.features
      if (!features) continue
      max = Math.max(max, meta.weight(features[meta.key]))
    }
    out.set(meta.key, max)
  }
  return out
}

/** 0..1 intensity of one cell against its column — drives the tint alpha. */
export function featureRatio(
  meta: FeatureMeta,
  value: number,
  maxima: Map<FeatureKey, number>
): number {
  const max = maxima.get(meta.key) ?? 0
  return max > 0 ? meta.weight(value) / max : 0
}

export function featureHue(meta: FeatureMeta, v: number): TintHue {
  if (!meta.signed) return "accent-orange"
  return v >= 0 ? "long" : "short"
}

/**
 * Signed numerals use the *ink* hue, not the raw one: on the near-white light
 * card `--long` sits at ~2.9:1 while `--long-ink` clears 5:1, and in dark the
 * ink is the lifted variant — so the same class reads in both themes, tinted
 * cell or not.
 */
export function signedTextClass(v: number | null | undefined): string {
  if (v == null || v === 0) return ""
  return v > 0 ? "text-long-ink" : "text-short-ink"
}

/** Meta line / section-action text — sans, normal case (amendment §2). */
export const META_LABEL = "text-xs text-muted-foreground"

/** Label sitting above a stat-tile figure — sans, normal case. */
export const TILE_LABEL = "text-[11.5px] leading-4 text-muted-foreground"

/** Sub-heading inside a card, one step under SectionCard's 15px title. */
export const SUB_TITLE = "text-[13px] font-medium text-foreground"

/** Chart axis ticks — one of the few places mono survives (amendment §2). */
export const AXIS_TICK = "font-mono text-[10px]"

/** In-chart quadrant caption — sans, normal case: it is prose, not a tick (amendment §2). */
export const QUADRANT_LABEL = "text-[10px]"

/**
 * The card-level "nothing to draw" state: a dashed well holding one sentence
 * that says *why* it is empty, never a bare dash.
 */
export function EmptyNote({
  children,
  className,
}: {
  children: ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        "rounded-md border border-dashed border-border px-5 py-8 text-center",
        className
      )}
    >
      <p className="mx-auto max-w-[48ch] text-[13px] leading-5 text-muted-foreground">
        {children}
      </p>
    </div>
  )
}

export function MarketName({
  market,
  className,
}: {
  market: string
  className?: string
}) {
  const split = market.indexOf(":")
  const prefix = split === -1 ? null : market.slice(0, split + 1)
  const ticker = split === -1 ? market : market.slice(split + 1)
  return (
    <span className={cn("font-mono font-medium", className)}>
      {prefix ? <span className="text-muted-foreground">{prefix}</span> : null}
      {ticker}
    </span>
  )
}

/** `long` → `Long` — side hints arrive lowercase from the API. */
export function titleCase(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1)
}
