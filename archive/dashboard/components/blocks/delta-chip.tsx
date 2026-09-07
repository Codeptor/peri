import { ArrowDown01Icon, ArrowUp01Icon } from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import { signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"
import { HUE_TINT, type Hue } from "@/components/blocks/icon-chip"

export type DeltaChipProps = {
  pct?: number | null
  text?: string
  className?: string
}

/** Tinted pill carrying a signed percentage: arrow glyph + mono value, green up / red down. */
export function DeltaChip({ pct, text, className }: DeltaChipProps) {
  const tone: Hue =
    pct == null || pct === 0 ? "neutral" : pct > 0 ? "long" : "short"
  const arrow =
    pct == null || pct === 0 ? null : pct > 0 ? ArrowUp01Icon : ArrowDown01Icon

  return (
    <span
      className={cn(
        "inline-flex items-center gap-0.5 rounded-full py-0.5 pr-2 font-mono text-[11px] font-medium",
        arrow ? "pl-1" : "pl-2",
        HUE_TINT[tone],
        className
      )}
    >
      {arrow ? (
        <HugeiconsIcon icon={arrow} size={12} strokeWidth={2.2} />
      ) : null}
      {text ?? signedPct(pct)}
    </span>
  )
}
