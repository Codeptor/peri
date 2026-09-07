import type * as React from "react"

import { cn } from "@/lib/utils"
import { HUE_SOLID, HUE_TINT, type Hue } from "@/components/blocks/icon-chip"

export type StatusPillProps = {
  tone: Hue
  children: React.ReactNode
  className?: string
}

/** Tinted pill with a leading dot — table status cells, side markers, mode flags. */
export function StatusPill({ tone, children, className }: StatusPillProps) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11.5px] leading-5 font-medium",
        HUE_TINT[tone],
        className
      )}
    >
      <span className={cn("size-1.5 shrink-0 rounded-full", HUE_SOLID[tone])} />
      {children}
    </span>
  )
}
