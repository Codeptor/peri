import { cn } from "@/lib/utils"

type Tone = "default" | "long" | "short" | "muted"

/**
 * PnL *text* uses the ink tokens, not the raw hue: on the light card `--long`
 * lands near 2.8:1, while `--long-ink` clears 4.9:1 and stays legible in dark.
 * Raw hues remain correct for chart marks (matrix cells, bars, dots, rings).
 */
const TONE_CLASS: Record<Tone, string> = {
  default: "text-foreground",
  long: "text-long-ink",
  short: "text-short-ink",
  muted: "text-muted-foreground",
}

/** Sign-aware tone for a PnL figure. */
export function pnlTone(n: number | null | undefined): Tone {
  if (n == null || n === 0) return "muted"
  return n > 0 ? "long" : "short"
}

/** Sans label over a mono value — the stat unit under charts and in card footers. */
export function MicroStat({
  label,
  value,
  tone = "default",
  className,
}: {
  label: string
  value: string
  tone?: Tone
  className?: string
}) {
  return (
    <div className={cn("min-w-0", className)}>
      <div className="text-xs leading-tight text-muted-foreground">{label}</div>
      <div
        className={cn(
          "mt-1 truncate font-mono text-[13px] font-medium",
          TONE_CLASS[tone]
        )}
      >
        {value}
      </div>
    </div>
  )
}

/** Hairline-topped strip that carries a row of MicroStats under a chart. */
export function StatStrip({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        "mt-4 flex flex-wrap items-start gap-x-6 gap-y-3 border-t border-border pt-3.5",
        className
      )}
    >
      {children}
    </div>
  )
}

/**
 * Fixed two-column stat block. `StatStrip` wraps on width, so three cards side
 * by side end up different heights depending on how their labels happen to
 * break; a grid of four stats is always exactly two rows in every column.
 */
export function StatGrid({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        "mt-4 grid grid-cols-2 gap-x-5 gap-y-3 border-t border-border pt-3.5",
        className
      )}
    >
      {children}
    </div>
  )
}

/** Legend strip that sits directly under a chart. */
export function ChartLegend({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <div
      className={cn(
        "mt-3 flex flex-wrap items-center gap-x-4 gap-y-1.5",
        className
      )}
    >
      {children}
    </div>
  )
}

/** Label left, mono value right — vertical stat lists inside narrow cards. */
export function RowStat({
  label,
  value,
  tone = "default",
}: {
  label: string
  value: string
  tone?: Tone
}) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <span className="text-[13px] text-muted-foreground">{label}</span>
      <span
        className={cn("font-mono text-[13px] font-medium", TONE_CLASS[tone])}
      >
        {value}
      </span>
    </div>
  )
}

/** Colour dot + sans label — chart legends in card headers. */
export function LegendDot({ color, label }: { color: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className="size-1.5 shrink-0 rounded-full"
        style={{ backgroundColor: color }}
      />
      <span className="text-xs text-muted-foreground">{label}</span>
    </span>
  )
}

/**
 * Rule swatch + sans label — the legend mark for a *curve*, where a dot would
 * claim the series is a set of discrete points. `dashed` reads as a threshold.
 */
export function LegendLine({
  color,
  label,
  dashed = false,
}: {
  color: string
  label: string
  dashed?: boolean
}) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className="h-0 w-3.5 shrink-0"
        style={{
          borderTopWidth: 1.5,
          borderTopStyle: dashed ? "dashed" : "solid",
          borderTopColor: color,
        }}
      />
      <span className="text-xs text-muted-foreground">{label}</span>
    </span>
  )
}

/** Sans count/annotation that sits in a SectionCard action slot. */
export function CardNote({ children }: { children: React.ReactNode }) {
  return (
    <span className="text-xs whitespace-nowrap text-muted-foreground">
      {children}
    </span>
  )
}
