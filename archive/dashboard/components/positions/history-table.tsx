"use client"

import * as React from "react"
import { ArchiveIcon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { fmtPrice, relativeTime, usd } from "@/lib/format"
import { isClose } from "@/lib/stats"
import { cn } from "@/lib/utils"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented, type SegmentedOption } from "@/components/blocks/segmented"
import { StatusPill } from "@/components/blocks/status-pill"
import { AnalystChip } from "@/components/analyst/analyst-chip"
import { Button } from "@/components/ui/button"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  actionLabel,
  actionTone,
  fillNet,
  pnlTone,
  utcStamp,
} from "@/components/positions/util"

const PAGE = 20

type Filter = "closes" | "all"

const FILTERS: SegmentedOption<Filter>[] = [
  { value: "closes", label: "Closes" },
  { value: "all", label: "All fills" },
]

const HEAD = "h-10 px-3 text-xs font-medium"
const CELL = "h-11 px-3 py-0 text-[13px]"
const NUM = `${CELL} text-right font-mono`

export type HistoryTableProps = {
  trades: Trade[]
  loading: boolean
}

/** Fill history — closes by default, every fill on demand (idiom 10). */
export function HistoryTable({ trades, loading }: HistoryTableProps) {
  const [filter, setFilter] = React.useState<Filter>("closes")
  const [expanded, setExpanded] = React.useState(false)

  const rows = React.useMemo(() => {
    const kept = filter === "closes" ? trades.filter(isClose) : trades
    return [...kept].sort((a, b) => b.ts - a.ts)
  }, [trades, filter])

  const visible = expanded ? rows : rows.slice(0, PAGE)

  return (
    <SectionCard
      action={
        <Segmented
          label="History filter"
          onChange={(value) => {
            setFilter(value)
            setExpanded(false)
          }}
          options={FILTERS}
          size="sm"
          value={filter}
        />
      }
      icon={ArchiveIcon}
      title="History"
    >
      <Table>
        <TableHeader>
          <TableRow className="border-b border-border hover:bg-transparent">
            <TableHead className={HEAD}>Time</TableHead>
            <TableHead className={HEAD}>Market</TableHead>
            <TableHead className={HEAD}>Action</TableHead>
            <TableHead className={HEAD}>Analyst</TableHead>
            <TableHead className={cn(HEAD, "text-right")}>Price</TableHead>
            <TableHead className={cn(HEAD, "text-right")}>Size</TableHead>
            <TableHead className={cn(HEAD, "text-right")}>Fee</TableHead>
            <TableHead className={cn(HEAD, "text-right")}>Net</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {visible.map((t) => (
            <TableRow className="border-b-0 hover:bg-surface-2/60" key={t.id}>
              <TableCell className={CELL}>
                <div className="font-mono text-xs leading-4">
                  {utcStamp(t.ts)}
                </div>
                <div className="font-mono text-[11px] leading-4 text-muted-foreground">
                  {relativeTime(t.ts)}
                </div>
              </TableCell>
              <TableCell className={cn(CELL, "font-mono")}>
                {t.market}
              </TableCell>
              <TableCell className={CELL}>
                <StatusPill tone={actionTone(t.action)}>
                  {actionLabel(t.action)}
                </StatusPill>
              </TableCell>
              <TableCell className={CELL}>
                <AnalystChip analyst={t.analyst} />
              </TableCell>
              <TableCell className={NUM}>{fmtPrice(t.px)}</TableCell>
              <TableCell className={NUM}>{t.size}</TableCell>
              <TableCell className={cn(NUM, "text-muted-foreground")}>
                {usd(t.fee)}
              </TableCell>
              <TableCell className={cn(NUM, pnlTone(fillNet(t)))}>
                {usd(fillNet(t))}
              </TableCell>
            </TableRow>
          ))}
          {visible.length === 0 ? (
            <TableRow className="border-b-0 hover:bg-transparent">
              <TableCell
                className="h-16 px-3 text-center text-[13px] text-muted-foreground"
                colSpan={8}
              >
                {loading ? "Loading fills…" : "No fills recorded yet"}
              </TableCell>
            </TableRow>
          ) : null}
        </TableBody>
      </Table>

      {rows.length > PAGE ? (
        <div className="mt-4 flex justify-center">
          <Button
            onClick={() => setExpanded((v) => !v)}
            size="sm"
            variant="outline"
          >
            {expanded ? "Show less" : `Show all ${rows.length}`}
          </Button>
        </div>
      ) : null}
    </SectionCard>
  )
}
