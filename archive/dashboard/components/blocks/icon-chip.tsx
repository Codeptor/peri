import type { IconSvgElement } from "@hugeicons/react"
import { HugeiconsIcon } from "@hugeicons/react"

import { cn } from "@/lib/utils"

/**
 * Semantic *data* hue set — chips, status pills, chart series. `accent` is the
 * orange data hue and the only attention colour among them; long/short are
 * PnL-only. Blue `--primary` is deliberately absent: it is the ACTION hue
 * (buttons, active nav, focus, selection, links) and never encodes a value.
 */
export type Hue = "accent" | "long" | "short" | "warning" | "neutral"

/**
 * 12% tint background + an *ink* foreground per hue. The ink token is the hue
 * darkened for light mode / lifted for dark, so text on the tint clears ~5:1 in
 * both themes — the raw hue would sit near 2.7:1 on the light ground.
 */
export const HUE_TINT: Record<Hue, string> = {
  accent: "bg-accent-orange/12 text-accent-orange-ink",
  long: "bg-long/12 text-long-ink",
  short: "bg-short/12 text-short-ink",
  warning: "bg-warning/14 text-warning-ink",
  neutral: "bg-cell text-muted-foreground",
}

/** Solid dot / fill colour, per hue. */
export const HUE_SOLID: Record<Hue, string> = {
  accent: "bg-accent-orange",
  long: "bg-long",
  short: "bg-short",
  warning: "bg-warning",
  neutral: "bg-muted-foreground",
}

/** Raw CSS custom property, for SVG `fill`/`stroke` attributes. */
export const HUE_VAR: Record<Hue, string> = {
  accent: "var(--accent-orange)",
  long: "var(--long)",
  short: "var(--short)",
  warning: "var(--warning)",
  neutral: "var(--chart-6)",
}

export type IconChipProps = {
  icon: IconSvgElement
  hue?: Hue
  className?: string
}

/** 28×28 rounded-square chip holding a 16px icon — leads every KPI and section header. */
export function IconChip({ icon, hue = "accent", className }: IconChipProps) {
  return (
    <span
      className={cn(
        "grid size-7 shrink-0 place-items-center rounded-[0.625rem]",
        HUE_TINT[hue],
        className
      )}
    >
      <HugeiconsIcon icon={icon} size={16} strokeWidth={1.8} />
    </span>
  )
}
