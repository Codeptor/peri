"use client"

import { Wallet01Icon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { relativeTime, usd } from "@/lib/format"
import { MARGIN_MAX, MAX_CONCURRENT } from "@/lib/risk"
import { isClose } from "@/lib/stats"
import { cn } from "@/lib/utils"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { StatTile } from "@/components/positions/stat-tile"
import {
  actionLabel,
  actionTone,
  fillNet,
  pnlTone,
} from "@/components/positions/util"

export type FlatBookProps = {
  trades: Trade[]
  loading: boolean
}

/** Nothing open: state the book is idle, then show where the last five went. */
export function FlatBook({ trades, loading }: FlatBookProps) {
  const closes = [...trades.filter(isClose)]
    .sort((a, b) => b.ts - a.ts)
    .slice(0, 5)

  return (
    <SectionCard icon={Wallet01Icon} title="Book is flat">
      <p className="max-w-prose text-[13px] text-muted-foreground">
        No open positions. The daemon keeps scanning — an entry lands here the
        moment a nominee clears the analyst gate.
      </p>

      <div className="mt-5 grid gap-3 sm:grid-cols-3">
        <StatTile
          label="Slots free"
          value={`${MAX_CONCURRENT} / ${MAX_CONCURRENT}`}
        />
        <StatTile
          label="Margin free"
          sub={
            <>
              <span className="font-mono">{`${MAX_CONCURRENT} × ${usd(MARGIN_MAX)}`}</span>{" "}
              cap
            </>
          }
          value={usd(MARGIN_MAX * MAX_CONCURRENT)}
        />
        <StatTile label="Open risk" sub="no stops working" value={usd(0)} />
      </div>

      <h3 className="mt-6 text-[13px] font-medium">Last 5 closes</h3>
      <div className="mt-3 flex flex-col gap-2">
        {closes.map((t) => (
          <div
            className="flex items-center gap-3 rounded-[10px] bg-surface-2 px-3.5 py-3"
            key={t.id}
          >
            <span className="truncate font-mono text-[13px] font-medium">
              {t.market}
            </span>
            <StatusPill tone={actionTone(t.action)}>
              {actionLabel(t.action)}
            </StatusPill>
            <span
              className={cn(
                "ml-auto shrink-0 font-mono text-[13px] font-medium",
                pnlTone(fillNet(t))
              )}
            >
              {usd(fillNet(t))}
            </span>
            <span className="w-16 shrink-0 text-right font-mono text-[11px] text-muted-foreground">
              {relativeTime(t.ts)}
            </span>
          </div>
        ))}
        {closes.length === 0 ? (
          <div className="rounded-[10px] bg-surface-2 px-3.5 py-4 text-center text-[13px] text-muted-foreground">
            {loading ? "Loading fills…" : "No closes recorded yet"}
          </div>
        ) : null}
      </div>
    </SectionCard>
  )
}
