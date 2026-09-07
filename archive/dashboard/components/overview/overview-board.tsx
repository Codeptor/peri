"use client"

import {
  AlertCircleIcon,
  DashboardSquare01Icon,
  RefreshIcon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import { relativeTime } from "@/lib/format"
import { cn } from "@/lib/utils"
import { PageHeader } from "@/components/blocks/page-header"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { DailyPnlCard } from "@/components/overview/daily-pnl-card"
import { EquityHero } from "@/components/overview/equity-hero"
import { ExitMixCard } from "@/components/overview/exit-mix-card"
import { FeeDragCard } from "@/components/overview/fee-drag-card"
import { KillBudgetCard } from "@/components/overview/kill-budget-card"
import { KpiRow } from "@/components/overview/kpi-row"
import { RecentFills } from "@/components/overview/recent-fills"
import { TradeNetCard } from "@/components/overview/trade-net-card"
import { STALE_MS, useOverview } from "@/components/overview/use-overview"

function utcStamp(ts: number): string {
  const iso = new Date(ts).toISOString()
  return `${iso.slice(0, 10)} ${iso.slice(11, 19)}Z`
}

/** The whole `/` desk: KPI row, equity hero, analytics row, recent fills. */
export function OverviewBoard() {
  const {
    equity,
    positions,
    trades,
    health,
    loading,
    failed,
    refreshing,
    connected,
    asOf,
    now,
    refresh,
  } = useOverview()

  const stale = asOf != null && now - asOf > STALE_MS

  return (
    <>
      <PageHeader
        actions={
          <Button disabled={refreshing} onClick={refresh} size="sm">
            <HugeiconsIcon
              className={cn(refreshing && "animate-spin")}
              icon={RefreshIcon}
              size={14}
              strokeWidth={1.8}
            />
            Refresh
          </Button>
        }
        icon={DashboardSquare01Icon}
        meta={
          loading ? (
            <Skeleton className="h-4 w-64 rounded-md" />
          ) : (
            <>
              <span>Paper desk · UTC</span>
              {asOf == null ? null : (
                <span>
                  As of <span className="font-mono">{utcStamp(asOf)}</span>
                </span>
              )}
              <StatusPill tone={stale ? "warning" : "long"}>
                {asOf == null
                  ? "No data"
                  : stale
                    ? `Stale · ${relativeTime(asOf)}`
                    : "Fresh"}
              </StatusPill>
              <span>
                <span className="font-mono">{positions.length}</span> open ·{" "}
                <span className="font-mono">{trades.length}</span> fills
              </span>
            </>
          )
        }
        title="Overview"
      />

      {failed ? (
        <SectionCard icon={AlertCircleIcon} title="Desk unreachable">
          <p className="text-[13px] text-muted-foreground">
            kestreld did not answer on{" "}
            <span className="font-mono">127.0.0.1:7411</span>. Start the daemon,
            or run the dashboard with{" "}
            <span className="font-mono">NEXT_PUBLIC_FIXTURES=1</span>.
          </p>
        </SectionCard>
      ) : (
        <div className="flex flex-col gap-5">
          <KpiRow
            asOf={asOf}
            equity={equity}
            health={health}
            loading={loading}
            positions={positions}
            trades={trades}
          />

          <EquityHero
            equity={equity}
            live={connected && !stale}
            loading={loading}
            trades={trades}
          />

          {/* Three reads of the same book: what each day paid, how a single
              close is shaped, and what the venue took along the way. Equal
              thirds — none of the three is the lead. */}
          <div className="grid gap-5 lg:grid-cols-3">
            <DailyPnlCard asOf={asOf} loading={loading} trades={trades} />
            <TradeNetCard loading={loading} trades={trades} />
            <FeeDragCard loading={loading} trades={trades} />
          </div>

          <div className="grid gap-5 lg:grid-cols-12">
            <ExitMixCard
              className="lg:col-span-8"
              loading={loading}
              trades={trades}
            />
            <KillBudgetCard
              className="lg:col-span-4"
              equity={equity}
              health={health}
              loading={loading}
            />
          </div>

          <RecentFills loading={loading} trades={trades} />
        </div>
      )}
    </>
  )
}
