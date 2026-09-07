"use client"

import { Shield01Icon } from "@hugeicons/core-free-icons"

import type { EquityPoint, Health } from "@/lib/api"
import { usd } from "@/lib/format"
import { BANKROLL, KILL_SWITCH_PCT, killBudgetUsed } from "@/lib/risk"
import { HUE_VAR, type Hue } from "@/components/blocks/icon-chip"
import { RingStat } from "@/components/blocks/ring-stat"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { Skeleton } from "@/components/ui/skeleton"
import { RowStat } from "@/components/overview/micro"

const CAP = BANKROLL * (KILL_SWITCH_PCT / 100)
const HALT_AT = BANKROLL - CAP

/** Escalates long → warning → short as the drawdown budget is consumed. */
function escalate(pct: number): Hue {
  if (pct < 50) return "long"
  if (pct < 80) return "warning"
  return "short"
}

/** How much of the kill-switch drawdown allowance the desk has burned. */
export function KillBudgetCard({
  equity,
  health,
  loading,
  className,
}: {
  equity: EquityPoint[]
  health: Health | null
  loading: boolean
  className?: string
}) {
  const equityNow = equity[equity.length - 1]?.equity ?? health?.equity ?? null
  const used = equityNow == null ? 0 : killBudgetUsed(equityNow) * 100
  const drawdown = equityNow == null ? null : Math.max(0, BANKROLL - equityNow)
  const headroom = equityNow == null ? null : equityNow - HALT_AT
  const tone = escalate(used)
  const halted = health?.kill_switch === true

  return (
    <SectionCard
      action={
        <StatusPill tone={halted ? "short" : tone}>
          {halted
            ? "Halted"
            : tone === "long"
              ? "Safe"
              : tone === "warning"
                ? "Watch"
                : "Critical"}
        </StatusPill>
      }
      className={className}
      icon={Shield01Icon}
      title="Kill budget"
    >
      {loading ? (
        <div className="flex flex-col items-center gap-4">
          <Skeleton className="size-24 rounded-full" />
          <Skeleton className="h-16 w-full rounded-md" />
        </div>
      ) : (
        <>
          <RingStat
            color={HUE_VAR[tone]}
            label="Budget used"
            pct={used}
            size={96}
            sublabel={`${usd(drawdown)} / ${usd(CAP)}`}
          />
          <div className="mt-5 flex flex-col gap-2.5 border-t border-border pt-3.5">
            <RowStat label="Equity" value={usd(equityNow)} />
            <RowStat label="Bankroll" value={usd(BANKROLL)} tone="muted" />
            <RowStat label="Halt at" value={usd(HALT_AT)} tone="short" />
            <RowStat
              label="Headroom"
              tone={headroom != null && headroom > 0 ? "long" : "short"}
              value={usd(headroom)}
            />
          </div>
        </>
      )}
    </SectionCard>
  )
}
