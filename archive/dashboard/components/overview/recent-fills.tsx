"use client"

import * as React from "react"
import { Invoice01Icon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { fmtPrice, usd } from "@/lib/format"
import { realizedNetPnl } from "@/lib/stats"
import { cn } from "@/lib/utils"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { AnalystChip } from "@/components/analyst/analyst-chip"
import { Skeleton } from "@/components/ui/skeleton"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { ACTION_TONE, actionLabel } from "@/components/overview/fills"
import { CardNote } from "@/components/overview/micro"

/**
 * The header rides the scroll port, so it needs its own opaque ground and a
 * hairline: under `border-collapse: collapse` a sticky cell drops the row's
 * bottom border, and an inset shadow paints one that survives.
 */
const HEAD =
  "sticky top-0 z-10 h-9 bg-card px-3 shadow-[inset_0_-1px_0_var(--border)]"
const NUM = "px-3 text-right font-mono text-[13px]"

/**
 * The card holds every fetched fill, so the list is capped by height instead of
 * by count. The scroll port has to be the table's own container — a wrapper
 * around it would leave the sticky header anchored to the inner, unscrolled box.
 */
const SCROLLER =
  "[&_[data-slot=table-container]]:max-h-[26rem] [&_[data-slot=table-container]]:overflow-y-auto"

function utcDay(ts: number): string {
  return new Date(ts).toISOString().slice(5, 10)
}

function utcTime(ts: number): string {
  return new Date(ts).toISOString().slice(11, 19)
}

/** Every fetched fill — airy rows, tinted action pills, mono numerals. */
export function RecentFills({
  trades,
  loading,
  className,
}: {
  trades: Trade[]
  loading: boolean
  className?: string
}) {
  const rows = React.useMemo(
    () => [...trades].sort((a, b) => b.ts - a.ts),
    [trades]
  )

  return (
    <SectionCard
      action={
        loading ? null : (
          <CardNote>
            {rows.length} {rows.length === 1 ? "fill" : "fills"}
          </CardNote>
        )
      }
      className={className}
      icon={Invoice01Icon}
      title="Recent fills"
    >
      {loading ? (
        <div className="flex flex-col gap-2.5">
          {Array.from({ length: 5 }, (_, i) => (
            <Skeleton className="h-9 w-full rounded-md" key={i} />
          ))}
        </div>
      ) : rows.length === 0 ? (
        <p className="py-6 text-center text-[13px] text-muted-foreground">
          No fills recorded yet.
        </p>
      ) : (
        <div className={SCROLLER}>
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead className={HEAD}>Time</TableHead>
                <TableHead className={HEAD}>Market</TableHead>
                <TableHead className={HEAD}>Action</TableHead>
                <TableHead className={HEAD}>Analyst</TableHead>
                <TableHead className={cn(HEAD, "text-right")}>Price</TableHead>
                <TableHead className={cn(HEAD, "text-right")}>Net</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((t) => {
                const net = realizedNetPnl(t)
                return (
                  <TableRow
                    className="h-11 border-0 hover:bg-foreground/4"
                    key={t.id}
                  >
                    <TableCell className="px-3 font-mono text-[13px]">
                      <span className="text-muted-foreground">
                        {utcDay(t.ts)}
                      </span>{" "}
                      {utcTime(t.ts)}
                    </TableCell>
                    <TableCell className="px-3 text-[13px] font-medium">
                      {t.market}
                    </TableCell>
                    <TableCell className="px-3">
                      <StatusPill tone={ACTION_TONE[t.action] ?? "neutral"}>
                        {actionLabel(t.action)}
                      </StatusPill>
                    </TableCell>
                    <TableCell className="px-3">
                      <AnalystChip analyst={t.analyst} />
                    </TableCell>
                    <TableCell className={NUM}>{fmtPrice(t.px)}</TableCell>
                    <TableCell
                      className={cn(
                        NUM,
                        "font-medium",
                        net != null && net > 0
                          ? "text-long-ink"
                          : net != null && net < 0
                            ? "text-short-ink"
                            : undefined
                      )}
                    >
                      {usd(net)}
                    </TableCell>
                  </TableRow>
                )
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </SectionCard>
  )
}
