"use client"

import { useEffect, useState } from "react"
import {
  RiAlertLine,
  RiCheckLine,
  RiShieldCheckLine,
  RiTimeLine,
} from "@remixicon/react"

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@workspace/ui/components/alert-dialog"
import { Badge } from "@workspace/ui/components/badge"
import { Button } from "@workspace/ui/components/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@workspace/ui/components/card"
import { Spinner } from "@workspace/ui/components/spinner"
import { cn } from "@workspace/ui/lib/utils"
import { api, type ProposalStatus, type TradeProposal } from "@/lib/api"
import {
  effectiveProposalStatus,
  proposalExecutionMode,
  proposalSecondsLeft,
  proposalTitle,
} from "@/lib/chat"
import { px, signedUsd, usd } from "@/lib/format"

function numeric(preview: Record<string, unknown>, key: string): number | null {
  const value = preview[key]
  return typeof value === "number" && Number.isFinite(value) ? value : null
}

function statusVariant(status: ProposalStatus) {
  if (status === "executed") return "default" as const
  if (status === "pending" || status === "authorizing")
    return "secondary" as const
  if (status === "needs_reconciliation" || status === "manual_review") {
    return "destructive" as const
  }
  return "outline" as const
}

function PreviewMetric({
  label,
  value,
}: {
  label: string
  value: React.ReactNode
}) {
  return (
    <div className="border-l border-border pl-2">
      <div className="text-[10px] tracking-wider text-muted-foreground uppercase">
        {label}
      </div>
      <div className="mt-0.5 font-mono text-xs font-medium tabular-nums">
        {value}
      </div>
    </div>
  )
}

function PreviewGrid({ proposal }: { proposal: TradeProposal }) {
  const { action, preview } = proposal
  if (action.kind === "open") {
    return (
      <div className="grid grid-cols-2 gap-x-3 gap-y-3 sm:grid-cols-4">
        <PreviewMetric
          label="Reference"
          value={px(numeric(preview, "reference_mark"))}
        />
        <PreviewMetric label="Size" value={px(numeric(preview, "size"))} />
        <PreviewMetric
          label="Notional"
          value={usd(numeric(preview, "notional"))}
        />
        <PreviewMetric
          label="Margin"
          value={usd(numeric(preview, "required_margin"))}
        />
        <PreviewMetric label="Stop" value={px(action.stop)} />
        <PreviewMetric label="Take profit" value={px(action.take_profit)} />
        <PreviewMetric
          label="TP net est."
          value={signedUsd(numeric(preview, "estimated_tp_net"))}
        />
        <PreviewMetric
          label="Stop loss est."
          value={
            numeric(preview, "estimated_stop_loss") == null
              ? "—"
              : `-${usd(numeric(preview, "estimated_stop_loss"))}`
          }
        />
      </div>
    )
  }
  if (action.kind === "close") {
    return (
      <div className="grid grid-cols-2 gap-x-3 gap-y-3 sm:grid-cols-4">
        <PreviewMetric
          label="Reference"
          value={px(numeric(preview, "reference_mark"))}
        />
        <PreviewMetric
          label="Full size"
          value={px(numeric(preview, "full_size"))}
        />
        <PreviewMetric
          label="Current stop"
          value={px(numeric(preview, "current_stop_px"))}
        />
        <PreviewMetric
          label="Current TP"
          value={px(numeric(preview, "current_tp_px"))}
        />
      </div>
    )
  }
  return (
    <div className="grid grid-cols-2 gap-x-3 gap-y-3 sm:grid-cols-4">
      <PreviewMetric
        label="Reference"
        value={px(numeric(preview, "reference_mark"))}
      />
      <PreviewMetric
        label="Full size"
        value={px(numeric(preview, "full_size"))}
      />
      <PreviewMetric label="New stop" value={px(action.stop)} />
      <PreviewMetric label="New TP" value={px(action.take_profit)} />
    </div>
  )
}

export function ProposalCard({
  proposal,
  onUpdate,
}: {
  proposal: TradeProposal
  onUpdate: (proposal: TradeProposal) => void
}) {
  const [nowMs, setNowMs] = useState<number | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [confirming, setConfirming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const timer = setInterval(() => setNowMs(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])

  const status =
    nowMs == null ? proposal.status : effectiveProposalStatus(proposal, nowMs)
  const seconds =
    nowMs == null
      ? Math.max(0, Math.ceil(proposal.expires_ts - proposal.created_ts))
      : proposalSecondsLeft(proposal, nowMs)
  const pending = status === "pending" && seconds > 0
  const executionMode = proposalExecutionMode(proposal)
  const liveMode = executionMode === "live"
  const reason =
    proposal.result && typeof proposal.result.reason === "string"
      ? proposal.result.reason
      : null

  async function confirm() {
    setConfirming(true)
    setError(null)
    try {
      const updated = await api.confirmProposal(proposal.id)
      onUpdate(updated)
      setDialogOpen(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setConfirming(false)
    }
  }

  return (
    <Card className="mt-3 gap-3 border-l-2 border-l-amber-500 bg-amber-500/[0.035] py-3">
      <CardHeader className="flex-row items-start justify-between gap-3 px-3">
        <div className="min-w-0 space-y-1">
          <CardTitle className="flex flex-wrap items-center gap-2 font-sans text-xs font-semibold">
            <RiShieldCheckLine className="size-4 text-amber-500" />
            Confirmation required
            <Badge
              variant={statusVariant(status)}
              className="font-mono text-[10px] uppercase"
            >
              {status.replaceAll("_", " ")}
            </Badge>
          </CardTitle>
          <div className="font-heading text-base font-semibold tracking-tight">
            {proposalTitle(proposal)}
          </div>
        </div>
        {pending && (
          <div
            className={cn(
              "flex shrink-0 items-center gap-1 font-mono text-[11px] tabular-nums",
              seconds <= 20 ? "text-red-500" : "text-muted-foreground"
            )}
          >
            <RiTimeLine className="size-3.5" />
            {seconds}s
          </div>
        )}
      </CardHeader>
      <CardContent className="space-y-3 px-3">
        <PreviewGrid proposal={proposal} />
        <p className="text-xs leading-relaxed text-muted-foreground">
          {proposal.action.rationale}
        </p>
        {proposal.action.kind === "open" && (
          <p className="border-l border-border pl-2 text-[11px] leading-relaxed text-muted-foreground">
            Invalidation: {proposal.action.invalidation}
          </p>
        )}
        {(reason || error) && (
          <div className="flex items-start gap-2 border border-destructive/30 bg-destructive/5 p-2 text-[11px] text-destructive">
            <RiAlertLine className="mt-0.5 size-3.5 shrink-0" />
            {error ?? reason}
          </div>
        )}
        {status === "executed" ? (
          <div className="flex items-center gap-2 text-xs font-medium text-emerald-500">
            <RiCheckLine className="size-4" /> Executed and journaled
          </div>
        ) : (
          <AlertDialog open={dialogOpen} onOpenChange={setDialogOpen}>
            <AlertDialogTrigger
              render={<Button disabled={!pending || confirming} size="sm" />}
            >
              <RiShieldCheckLine />
              {status === "expired"
                ? "Confirmation expired"
                : "Review & confirm"}
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogMedia className="bg-amber-500/10 text-amber-500">
                  <RiAlertLine />
                </AlertDialogMedia>
                <AlertDialogTitle>
                  Execute {proposalTitle(proposal)}?
                </AlertDialogTitle>
                <AlertDialogDescription>
                  This is the only step that can place or modify a{" "}
                  {executionMode}
                  {liveMode ? " Trench" : " simulated"} order. Peri will fetch
                  venue state again and refuse if the mark, position, margin, or
                  risk rails changed. The browser cannot alter the stored
                  action.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel disabled={confirming}>
                  Cancel
                </AlertDialogCancel>
                <AlertDialogAction
                  disabled={confirming || !pending}
                  onClick={() => void confirm()}
                  variant={
                    proposal.action.kind === "close" ? "destructive" : "default"
                  }
                >
                  {confirming ? <Spinner /> : <RiShieldCheckLine />}
                  {confirming
                    ? "Revalidating…"
                    : `Confirm ${executionMode} action`}
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
        <div className="font-mono text-[10px] text-muted-foreground">
          immutable id {proposal.id.slice(0, 12)} · context {executionMode}
        </div>
      </CardContent>
    </Card>
  )
}
