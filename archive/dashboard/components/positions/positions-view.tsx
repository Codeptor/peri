"use client"

import * as React from "react"
import {
  Alert02Icon,
  ChartLineData01Icon,
  Layers01Icon,
  Refresh01Icon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import type { CandleInterval } from "@/lib/candles"
import { relativeTime, usd } from "@/lib/format"
import { MAX_CONCURRENT } from "@/lib/risk"
import { cn } from "@/lib/utils"
import { PageHeader } from "@/components/blocks/page-header"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { FlatBook } from "@/components/positions/flat-book"
import { HistoryTable } from "@/components/positions/history-table"
import { PositionCard } from "@/components/positions/position-card"
import { PositionHero } from "@/components/positions/position-hero"
import { RiskStrip } from "@/components/positions/risk-strip"
import { useBook, useNow } from "@/components/positions/use-book"
import { NO_SPARK, usePnlSparks } from "@/components/positions/use-sparks"
import { bookTotals, pnlTone } from "@/components/positions/util"

function BookSkeleton() {
  return (
    <div className="flex flex-col gap-5">
      <Skeleton className="h-[560px] w-full rounded-lg" />
      <Skeleton className="h-[160px] w-full rounded-lg" />
      <Skeleton className="h-[280px] w-full rounded-lg" />
    </div>
  )
}

export function PositionsView() {
  const { positions, trades, loading, refreshing, error, updatedAt, refresh } =
    useBook()
  const [selectedId, setSelectedId] = React.useState<number | null>(null)
  const [candleInterval, setCandleInterval] =
    React.useState<CandleInterval>("1m")
  const now = useNow()
  const sparks = usePnlSparks(positions)

  const sorted = React.useMemo(
    () => [...positions].sort((a, b) => b.opened_ts - a.opened_ts),
    [positions]
  )
  const selected = sorted.find((p) => p.id === selectedId) ?? sorted[0] ?? null
  const totals = bookTotals(positions)
  const open = positions.length
  const empty = positions.length === 0 && trades.length === 0
  const atCap = open >= MAX_CONCURRENT

  return (
    <>
      <PageHeader
        actions={
          <Button onClick={refresh} size="sm">
            <HugeiconsIcon
              className={cn(refreshing && "animate-spin")}
              icon={Refresh01Icon}
            />
            Refresh
          </Button>
        }
        icon={ChartLineData01Icon}
        meta={
          <>
            <StatusPill
              tone={atCap ? "warning" : open > 0 ? "accent" : "neutral"}
            >
              <span className="font-mono">{`${open}/${MAX_CONCURRENT}`}</span>
              open
            </StatusPill>
            <span>
              <span className="font-mono">{usd(totals.margin)}</span> margin
            </span>
            <span className={pnlTone(totals.unrealized)}>
              <span className="font-mono">{usd(totals.unrealized)}</span>{" "}
              unrealized
            </span>
            <span>
              {updatedAt == null ? (
                "syncing…"
              ) : (
                <>
                  updated{" "}
                  <span className="font-mono">{relativeTime(updatedAt)}</span>
                </>
              )}
            </span>
            {error ? (
              <StatusPill tone="short">kestreld unreachable</StatusPill>
            ) : null}
          </>
        }
        title="Positions"
      />

      <div className="flex flex-col gap-5">
        {loading && empty ? (
          <BookSkeleton />
        ) : error && empty ? (
          <SectionCard icon={Alert02Icon} title="Book unavailable">
            <p className="max-w-prose text-[13px] text-muted-foreground">
              kestreld did not answer — the daemon is probably down. Start it
              and retry.
            </p>
            <div className="mt-4 flex items-center gap-3">
              <Button onClick={refresh} size="sm">
                Retry
              </Button>
              <code className="truncate font-mono text-[11px] text-muted-foreground">
                {error}
              </code>
            </div>
          </SectionCard>
        ) : (
          <>
            {selected ? (
              <>
                <PositionHero
                  interval={candleInterval}
                  onIntervalChange={setCandleInterval}
                  position={selected}
                  spark={sparks.get(selected.id) ?? NO_SPARK}
                />
                <RiskStrip positions={positions} />
                <SectionCard
                  action={
                    <span className="text-xs text-muted-foreground">
                      <span className="font-mono">{`${open}/${MAX_CONCURRENT}`}</span>{" "}
                      slots used
                    </span>
                  }
                  icon={Layers01Icon}
                  title="Open positions"
                >
                  <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                    {sorted.map((position) => (
                      <PositionCard
                        key={position.id}
                        now={now}
                        onSelect={setSelectedId}
                        position={position}
                        selected={position.id === selected.id}
                      />
                    ))}
                  </div>
                </SectionCard>
              </>
            ) : (
              <FlatBook loading={loading} trades={trades} />
            )}
            <HistoryTable loading={loading} trades={trades} />
          </>
        )}
      </div>
    </>
  )
}
