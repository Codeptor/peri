import type * as React from "react"

import { cn } from "@/lib/utils"

export type StatTileProps = {
  label: string
  value: React.ReactNode
  sub?: React.ReactNode
  valueClassName?: string
  className?: string
}

/**
 * Inner tile: sans label over a mono figure (amendment §2 — mono is numerals
 * only). `surface-2` is the inner-tile step in both themes: a shade darker than
 * the white card in light, a shade lighter than the charcoal card in dark.
 */
export function StatTile({
  label,
  value,
  sub,
  valueClassName,
  className,
}: StatTileProps) {
  return (
    <div className={cn("rounded-[10px] bg-surface-2 px-3.5 py-3", className)}>
      <div className="truncate text-xs text-muted-foreground">{label}</div>
      <div
        className={cn(
          "mt-1.5 truncate font-mono text-[15px] leading-5 font-semibold",
          valueClassName
        )}
      >
        {value}
      </div>
      {sub ? (
        <div className="mt-1 truncate text-[11.5px] text-muted-foreground">
          {sub}
        </div>
      ) : null}
    </div>
  )
}
