"use client"

import { cn } from "@/lib/utils"

export type SegmentedOption<T extends string> = {
  value: T
  label: string
}

export type SegmentedProps<T extends string> = {
  options: SegmentedOption<T>[]
  value: T
  onChange: (value: T) => void
  size?: "sm" | "md"
  /** accessible name for the group (rendered as aria-label, never visible) */
  label?: string
  className?: string
}

const ITEM_SIZE: Record<"sm" | "md", string> = {
  sm: "h-6 px-2.5 text-xs",
  md: "h-7 px-3 text-[13px]",
}

/**
 * The one segmented control (spec idiom 9 + amendment §3): rounded-full inset
 * track, sans labels in sentence case, active leg = card surface with a hairline
 * (light) / surface-2 (dark). Never orange-on-orange.
 */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  size = "md",
  label,
  className,
}: SegmentedProps<T>) {
  return (
    <div
      aria-label={label}
      className={cn(
        "inline-flex items-center gap-0.5 rounded-full border border-border bg-surface-2 p-0.5 dark:bg-background",
        className
      )}
      role="group"
    >
      {options.map((option) => {
        const active = option.value === value
        return (
          <button
            aria-pressed={active}
            className={cn(
              "inline-flex items-center rounded-full border font-medium whitespace-nowrap transition-colors",
              ITEM_SIZE[size],
              active
                ? "border-border bg-card text-foreground dark:border-transparent dark:bg-surface-2"
                : "border-transparent text-muted-foreground hover:text-foreground"
            )}
            key={option.value}
            onClick={() => onChange(option.value)}
            type="button"
          >
            {option.label}
          </button>
        )
      })}
    </div>
  )
}
