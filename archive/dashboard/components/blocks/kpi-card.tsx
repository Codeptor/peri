"use client"

import NumberFlow, { type Format } from "@number-flow/react"
import type { IconSvgElement } from "@hugeicons/react"

import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import { DeltaChip } from "@/components/blocks/delta-chip"
import { IconChip } from "@/components/blocks/icon-chip"

export type KpiCardProps = {
  label: string
  value: number | null
  format?: Format
  deltaPct?: number | null
  deltaLabel?: string
  /** muted sentence beside the delta chip on row 3 */
  context?: string
  icon: IconSvgElement
  hue?: "accent" | "long" | "short" | "neutral"
  loading?: boolean
  className?: string
}

/**
 * Three stacked rows (amendment §3): chip + label · mono figure · delta + context.
 * The label owns its row alone so it can wrap instead of truncating at 5-up.
 */
export function KpiCard({
  label,
  value,
  format,
  deltaPct,
  deltaLabel,
  context,
  icon,
  hue = "accent",
  loading = false,
  className,
}: KpiCardProps) {
  const showDelta = deltaPct != null || deltaLabel != null

  return (
    <div
      className={cn(
        "flex min-h-[120px] flex-col gap-3 rounded-lg border border-border bg-card p-5 shadow-[var(--shadow-card)]",
        className
      )}
    >
      <div className="flex items-center gap-2.5">
        <IconChip hue={hue} icon={icon} />
        <span className="text-[13px] leading-tight text-muted-foreground">
          {label}
        </span>
      </div>
      <div className="mt-auto font-mono text-[27px] leading-9 font-semibold tracking-tight">
        {loading ? (
          <Skeleton className="h-8 w-28 rounded-md" />
        ) : value == null ? (
          <span className="text-muted-foreground">—</span>
        ) : (
          <NumberFlow format={format} value={value} />
        )}
      </div>
      {showDelta || context ? (
        <div className="flex min-w-0 items-center gap-2">
          {showDelta ? <DeltaChip pct={deltaPct} text={deltaLabel} /> : null}
          {context ? (
            <span className="truncate text-xs text-muted-foreground">
              {context}
            </span>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}
