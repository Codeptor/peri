import type * as React from "react"

import { cn } from "@/lib/utils"

export type MetaChipProps = {
  /** sans, sentence case — names the metric */
  label?: string
  /** the value itself: numeral, duration or id → mono */
  children: React.ReactNode
  className?: string
}

/** Micro chip for model / latency / horizon metadata: sans label + mono value. */
export function MetaChip({ label, children, className }: MetaChipProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full bg-cell px-2.5 py-0.5 text-[11.5px] leading-5",
        className
      )}
    >
      {label ? <span className="text-muted-foreground">{label}</span> : null}
      <span className="font-mono text-foreground">{children}</span>
    </span>
  )
}

export type MarketChipProps = {
  market: string
  active?: boolean
  onSelect: (market: string) => void
  className?: string
}

/** Clickable market tag — drives the page-wide wire filter. Symbol is an id → mono. */
export function MarketChip({
  market,
  active = false,
  onSelect,
  className,
}: MarketChipProps) {
  return (
    <button
      aria-pressed={active}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 font-mono text-[11.5px] leading-5 font-medium transition-colors",
        active
          ? "bg-primary/14 text-primary-ink"
          : "bg-cell text-muted-foreground hover:bg-primary/12 hover:text-primary-ink",
        className
      )}
      onClick={() => onSelect(market)}
      type="button"
    >
      <span
        className={cn(
          "size-1.5 shrink-0 rounded-full",
          active ? "bg-primary" : "bg-muted-foreground"
        )}
      />
      {market}
    </button>
  )
}
