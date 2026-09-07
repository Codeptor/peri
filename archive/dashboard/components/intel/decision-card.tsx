"use client"

import * as React from "react"
import { ArrowDown01Icon, SourceCodeIcon } from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import type { Decision } from "@/lib/api"
import { relativeTime } from "@/lib/format"
import { cn } from "@/lib/utils"
import type { Hue } from "@/components/blocks/icon-chip"
import { StatusPill } from "@/components/blocks/status-pill"
import { formatLatency, parseReason } from "@/components/intel/intel-utils"
import { MetaChip } from "@/components/intel/meta-chip"

/** open = the desk acted (orange); veto/move-stop = risk gate (amber); everything else stays neutral. */
function actionTone(action: string): Hue {
  if (action === "open") return "accent"
  if (action === "veto_close" || action.startsWith("move_stop"))
    return "warning"
  return "neutral"
}

/** `move_stop_loss` → `Move stop loss` — sentence case, no snake left over. */
function actionLabel(action: string): string {
  const words = action.replace(/_/g, " ")
  return words.charAt(0).toUpperCase() + words.slice(1)
}

export type DecisionCardProps = {
  decision: Decision
  active: boolean
  onSelectMarket: (market: string) => void
}

/** One analyst call: market + action, conviction meter, thesis, model/latency chips, raw JSON. */
export function DecisionCard({
  decision,
  active,
  onSelectMarket,
}: DecisionCardProps) {
  const [open, setOpen] = React.useState(false)
  const meta = parseReason(decision.reason)
  const side =
    decision.side === "long" || decision.side === "short" ? decision.side : null
  const conviction = Math.min(1, Math.max(0, decision.conviction))

  return (
    <article className="rounded-md border border-border bg-surface-2 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <button
          aria-pressed={active}
          className={cn(
            "font-mono text-[13px] font-semibold tracking-tight transition-colors",
            active ? "text-primary-ink" : "hover:text-primary-ink"
          )}
          onClick={() => onSelectMarket(decision.market)}
          type="button"
        >
          {decision.market}
        </button>
        <StatusPill tone={actionTone(decision.action)}>
          {actionLabel(decision.action)}
        </StatusPill>
        {side ? (
          <StatusPill tone={side}>
            {side === "long" ? "Long" : "Short"}
          </StatusPill>
        ) : null}
        <span className="ml-auto shrink-0 font-mono text-[11px] text-muted-foreground">
          {relativeTime(decision.ts)}
        </span>
      </div>

      <div className="mt-3.5 flex items-center gap-3">
        <span className="shrink-0 text-[13px] text-muted-foreground">
          Conviction
        </span>
        <span
          aria-label={`conviction ${conviction.toFixed(2)}`}
          aria-valuemax={1}
          aria-valuemin={0}
          aria-valuenow={conviction}
          className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-cell"
          role="progressbar"
        >
          <span
            className="block h-full rounded-full bg-accent-orange"
            style={{ width: `${conviction * 100}%` }}
          />
        </span>
        <span className="shrink-0 font-mono text-[13px] font-semibold">
          {conviction.toFixed(2)}
        </span>
      </div>

      <p className="mt-3 text-[13px] leading-relaxed">
        {decision.thesis || "—"}
      </p>

      <div className="mt-3.5 flex flex-wrap items-center gap-2">
        {meta.model ? <MetaChip label="Model">{meta.model}</MetaChip> : null}
        {meta.latencyMs != null ? (
          <MetaChip label="Latency">{formatLatency(meta.latencyMs)}</MetaChip>
        ) : null}
        {decision.horizon_hours != null ? (
          <MetaChip label="Horizon">{`${decision.horizon_hours}h`}</MetaChip>
        ) : null}
        {meta.searched.length > 0 ? (
          <MetaChip label="Searches">{meta.searched.length}</MetaChip>
        ) : null}
        {meta.review ? <StatusPill tone="neutral">Review</StatusPill> : null}
        {decision.executed ? (
          <StatusPill tone="accent">Executed</StatusPill>
        ) : null}
        {decision.vetoed ? (
          <StatusPill tone="warning">Vetoed</StatusPill>
        ) : null}
        {meta.refused ? (
          <StatusPill tone="short">
            {meta.refundable ? `Refused · ${meta.refundable}` : "Refused"}
          </StatusPill>
        ) : null}

        <button
          aria-expanded={open}
          className="ml-auto inline-flex shrink-0 items-center gap-1.5 rounded-full bg-cell px-2.5 py-0.5 text-[11.5px] leading-5 text-muted-foreground transition-colors hover:text-foreground"
          onClick={() => setOpen((v) => !v)}
          type="button"
        >
          <HugeiconsIcon icon={SourceCodeIcon} size={12} strokeWidth={1.8} />
          Raw
          <HugeiconsIcon
            className={cn("transition-transform", open && "rotate-180")}
            icon={ArrowDown01Icon}
            size={12}
            strokeWidth={1.8}
          />
        </button>
      </div>

      {open ? (
        <pre className="mt-3 overflow-x-auto rounded-md border border-border bg-card p-3 font-mono text-[11px] leading-5 text-muted-foreground">
          {JSON.stringify(decision, null, 2)}
        </pre>
      ) : null}
    </article>
  )
}
