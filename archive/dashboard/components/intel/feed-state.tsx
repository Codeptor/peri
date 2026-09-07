import { StatusPill } from "@/components/blocks/status-pill"
import { Skeleton } from "@/components/ui/skeleton"

export type StateUnavailableProps = {
  /** why it is unavailable — e.g. `kestreld responded 503`. */
  note: string | null
}

/**
 * Neutral row for an endpoint that answered 503 (state wedged) or not at all.
 * A rail we cannot read is unknown, not broken — so it renders grey, never red,
 * and the card around it keeps its shape.
 */
export function StateUnavailable({ note }: StateUnavailableProps) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-md border border-border bg-surface-2 px-4 py-3">
      <StatusPill tone="neutral">State unavailable</StatusPill>
      <span className="text-[13px] text-muted-foreground">
        {note ?? "kestreld did not answer"}
      </span>
    </div>
  )
}

export type FeedEmptyProps = { title: string; hint: string }

/** Soft tile shown when a feed has nothing to say (or the filter emptied it). */
export function FeedEmpty({ title, hint }: FeedEmptyProps) {
  return (
    <div className="rounded-md border border-border bg-surface-2 px-5 py-10 text-center">
      <div className="text-sm font-medium">{title}</div>
      <p className="mx-auto mt-1.5 max-w-[34ch] text-[13px] leading-relaxed text-muted-foreground">
        {hint}
      </p>
    </div>
  )
}

export function FeedSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className="flex flex-col gap-3">
      {Array.from({ length: rows }, (_, i) => (
        <div
          className="rounded-md border border-border bg-surface-2 p-4"
          key={i}
        >
          <div className="flex items-center gap-2">
            <Skeleton className="h-3.5 w-16 rounded-md" />
            <Skeleton className="h-3.5 w-12 rounded-full" />
            <Skeleton className="ml-auto h-3 w-14 rounded-md" />
          </div>
          <Skeleton className="mt-4 h-1.5 w-full rounded-full" />
          <Skeleton className="mt-4 h-3 w-full rounded-md" />
          <Skeleton className="mt-2 h-3 w-3/4 rounded-md" />
        </div>
      ))}
    </div>
  )
}
