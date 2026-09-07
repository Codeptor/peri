import { cn } from "@/lib/utils"

export type MeterProps = {
  /** sans label on the left, e.g. `Stop loss` — a colored dot carries the hue */
  label: string
  /** the level price, mono, beside the label */
  level: string
  /** 0..1 travelled from entry toward the level */
  progress: number
  color: string
  /** right-hand mono readout, e.g. distance to the level */
  readout?: string
  className?: string
}

/**
 * Soft rounded track with a colored fill — how far the mark has walked to SL/TP.
 * The hue lives in the dot and the fill only; the label stays `muted-foreground`
 * so it clears contrast on both the light card and the dark charcoal one.
 */
export function Meter({
  label,
  level,
  progress,
  color,
  readout,
  className,
}: MeterProps) {
  const pct = Math.min(100, Math.max(0, progress * 100))

  return (
    <div className={cn("min-w-0", className)}>
      <div className="flex items-baseline gap-2">
        <span className="flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground">
          <span
            className="size-1.5 shrink-0 rounded-full"
            style={{ backgroundColor: color }}
          />
          {label}
        </span>
        <span className="truncate font-mono text-[11.5px] text-foreground">
          {level}
        </span>
        {readout ? (
          <span className="ml-auto shrink-0 font-mono text-[11.5px] text-muted-foreground">
            {readout}
          </span>
        ) : null}
      </div>
      <div className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-surface-2">
        <div
          className="h-full rounded-full transition-[width] duration-500 ease-out"
          style={{ backgroundColor: color, width: `${pct}%` }}
        />
      </div>
    </div>
  )
}
