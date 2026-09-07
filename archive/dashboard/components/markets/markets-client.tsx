"use client"

import * as React from "react"
import { useRouter, useSearchParams } from "next/navigation"
import {
  Analytics01Icon,
  BarChartHorizontalIcon,
  ChartScatterIcon,
  GridViewIcon,
  Search01Icon,
  StopWatchIcon,
  Target02Icon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import {
  api,
  type Analytics,
  type Nominee,
  type Snapshot,
  type Trade,
} from "@/lib/api"
import { relativeTime, usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { useLiveFeed } from "@/lib/ws"
import { Input } from "@/components/ui/input"
import { PageHeader } from "@/components/blocks/page-header"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented } from "@/components/blocks/segmented"
import { StatusPill } from "@/components/blocks/status-pill"

import { CrowdingScatter } from "./crowding-scatter"
import { HoldTimeScatter } from "./hold-time-scatter"
import { MarketPnlBars } from "./market-pnl-bars"
import { MarketSheet } from "./market-sheet"
import { NomineesRow } from "./nominees-row"
import { holdLabel, holdTimeSample, medianHold, rankMarkets } from "./pnl-data"
import { ScreenerTable } from "./screener-table"
import {
  EmptyNote,
  featureMaxima,
  META_LABEL,
  type ScreenerRow,
} from "./shared"

const POLL_MS = 30_000
/**
 * The realized half of the page moves in fills, not ticks, and `/api/analytics`
 * drains the counterfactual backlog on the way out — a minute is plenty.
 */
const DESK_POLL_MS = 60_000
/** Fills read to rebuild round trips. Every hold-time caveat is a caveat about this number. */
const FILL_WINDOW = 400

type Scope = "all" | "nominated"

const SCOPES = [
  { value: "all" as const, label: "All markets" },
  { value: "nominated" as const, label: "Nominated" },
]

/** Sans label with its numeral kept in mono — the meta-row idiom. */
function Count({ n, children }: { n: number; children: React.ReactNode }) {
  return (
    <span className={META_LABEL}>
      <span className="font-mono">{n}</span> {children}
    </span>
  )
}

/** Markets desk: nominees, the sortable universe, and the crowding map. */
export function MarketsClient() {
  const router = useRouter()
  const searchParams = useSearchParams()
  const selected = searchParams.get("m")

  const [snapshot, setSnapshot] = React.useState<Snapshot | null>(null)
  const [nominees, setNominees] = React.useState<Nominee[]>([])
  const [mids, setMids] = React.useState<Record<string, number>>({})
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState<string | null>(null)
  const [scope, setScope] = React.useState<Scope>("all")
  const [query, setQuery] = React.useState("")

  const [analytics, setAnalytics] = React.useState<Analytics | null>(null)
  const [fills, setFills] = React.useState<Trade[]>([])
  const [deskLoading, setDeskLoading] = React.useState(true)
  const [analyticsDown, setAnalyticsDown] = React.useState(false)
  const [fillsDown, setFillsDown] = React.useState(false)

  React.useEffect(() => {
    let cancelled = false
    const tick = () => {
      Promise.allSettled([api.snapshot(), api.nominees()]).then(
        ([snap, noms]) => {
          if (cancelled) return
          if (snap.status === "fulfilled") setSnapshot(snap.value)
          if (noms.status === "fulfilled") setNominees(noms.value)
          setError(
            snap.status === "rejected"
              ? "kestreld unreachable — no snapshot."
              : null
          )
          setLoading(false)
        }
      )
    }
    tick()
    const id = setInterval(tick, POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  // The realized band polls on its own slower clock — and each endpoint reports
  // its own outage, so one wedged handler never blanks the other's card.
  React.useEffect(() => {
    let cancelled = false
    const tick = () => {
      Promise.allSettled([api.analytics(), api.trades(FILL_WINDOW)]).then(
        ([an, tr]) => {
          if (cancelled) return
          if (an.status === "fulfilled") setAnalytics(an.value)
          if (tr.status === "fulfilled") setFills(tr.value)
          setAnalyticsDown(an.status === "rejected")
          setFillsDown(tr.status === "rejected")
          setDeskLoading(false)
        }
      )
    }
    tick()
    const id = setInterval(tick, DESK_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  useLiveFeed({
    mids: (next) => setMids((prev) => ({ ...prev, ...next })),
  })

  const select = React.useCallback(
    (market: string | null) => {
      router.replace(
        market ? `/markets?m=${encodeURIComponent(market)}` : "/markets",
        { scroll: false }
      )
    },
    [router]
  )

  const closeSheet = React.useCallback(() => select(null), [select])

  const rows = React.useMemo<ScreenerRow[]>(() => {
    const best = new Map<string, Nominee>()
    for (const nominee of [...nominees].sort((a, b) => b.score - a.score)) {
      if (!best.has(nominee.market)) best.set(nominee.market, nominee)
    }
    return (snapshot?.markets ?? []).map((data) => {
      const mark = mids[data.market] ?? data.mark
      return {
        market: data.market,
        data,
        mark,
        chg24h:
          data.prev_day_px > 0
            ? ((mark - data.prev_day_px) / data.prev_day_px) * 100
            : null,
        nominee: best.get(data.market) ?? null,
      }
    })
  }, [snapshot, nominees, mids])

  const maxima = React.useMemo(() => featureMaxima(rows), [rows])

  const filtered = React.useMemo(() => {
    const needle = query.trim().toLowerCase()
    return rows.filter(
      (row) =>
        (scope === "all" || row.nominee != null) &&
        (needle === "" || row.market.toLowerCase().includes(needle))
    )
  }, [rows, scope, query])

  const selectedRow = React.useMemo(
    () => rows.find((row) => row.market === selected) ?? null,
    [rows, selected]
  )

  const nomineeTop = React.useMemo(
    () => nominees.reduce((hi, n) => Math.max(hi, Math.abs(n.score)), 0),
    [nominees]
  )

  const scanned = React.useMemo(
    () =>
      nominees.length > 0
        ? nominees.reduce((latest, n) => Math.max(latest, n.ts), 0)
        : null,
    [nominees]
  )

  const records = React.useMemo(() => analytics?.per_market ?? [], [analytics])
  const ranked = React.useMemo(() => rankMarkets(records), [records])
  /** markets with a non-zero result that the ±8 ranking pushed off the chart */
  const omitted = Math.max(
    0,
    records.filter((r) => r.net_pnl !== 0).length - ranked.length
  )
  const deskNet = records.reduce((sum, r) => sum + r.net_pnl, 0)

  const holds = React.useMemo(() => holdTimeSample(fills), [fills])
  const typicalHold = React.useMemo(() => medianHold(holds.points), [holds])

  return (
    <>
      <PageHeader
        actions={
          <div className="relative">
            <HugeiconsIcon
              className="absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground"
              icon={Search01Icon}
              size={14}
              strokeWidth={1.8}
            />
            <Input
              aria-label="Filter markets"
              className="h-9 w-56 rounded-lg border-border border-b-border bg-surface-2 pr-3 pl-9 text-[13px] focus-visible:border-ring focus-visible:border-b-ring md:text-[13px]"
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Filter markets"
              value={query}
            />
          </div>
        }
        icon={Analytics01Icon}
        meta={
          <>
            <Count n={rows.length}>markets</Count>
            <span aria-hidden="true" className="opacity-40">
              ·
            </span>
            <Count n={nominees.length}>nominated</Count>
            <span aria-hidden="true" className="opacity-40">
              ·
            </span>
            <span className={META_LABEL}>
              Snapshot{" "}
              <span className="font-mono">
                {relativeTime(snapshot?.ts ?? null)}
              </span>
            </span>
          </>
        }
        title="Markets"
      />

      <div className="flex flex-wrap items-center gap-3 pb-5">
        <Segmented
          label="Market scope"
          onChange={setScope}
          options={SCOPES}
          value={scope}
        />
        {error ? <StatusPill tone="short">Offline</StatusPill> : null}
        <span className={cn(META_LABEL, "ml-auto")}>
          <span className="font-mono">{filtered.length}</span> shown
        </span>
      </div>

      <div className="flex flex-col gap-5">
        <SectionCard
          action={
            scanned ? (
              <span className={META_LABEL}>
                Scanned{" "}
                <span className="font-mono">{relativeTime(scanned)}</span>
              </span>
            ) : (
              <span className={META_LABEL}>No scan yet</span>
            )
          }
          icon={Target02Icon}
          title="Nominees"
        >
          <NomineesRow
            loading={loading}
            nominees={nominees}
            onSelect={select}
            selected={selected}
          />
        </SectionCard>

        {/*
          The realized band: which markets this desk actually earns in, and
          whether the money comes from sitting still or from getting out fast.
          It sits under the nominees because both answer "where should the next
          entry go" — one from the live book, one from the fill log.
        */}
        <div className="grid gap-5 xl:grid-cols-2">
          <SectionCard
            action={
              <span className={META_LABEL}>
                Net{" "}
                <span
                  className={cn(
                    "font-mono",
                    analytics == null
                      ? undefined
                      : deskNet >= 0
                        ? "text-long-ink"
                        : "text-short-ink"
                  )}
                >
                  {analytics == null ? "—" : usd(deskNet)}
                </span>
              </span>
            }
            icon={BarChartHorizontalIcon}
            title="Realized by market"
          >
            {analyticsDown ? (
              <EmptyNote>
                kestreld did not answer{" "}
                <span className="font-mono">/api/analytics</span> — realized PnL
                per market is unavailable until it does.
              </EmptyNote>
            ) : (
              <MarketPnlBars
                loading={deskLoading}
                omitted={omitted}
                onSelect={select}
                rows={ranked}
                selected={selected}
              />
            )}
          </SectionCard>

          <SectionCard
            action={
              <span className={META_LABEL}>
                Median hold{" "}
                <span className="font-mono">{holdLabel(typicalHold)}</span>
              </span>
            }
            icon={StopWatchIcon}
            title="Hold time vs net"
          >
            {fillsDown ? (
              <EmptyNote>
                kestreld did not answer{" "}
                <span className="font-mono">/api/trades</span> — round trips
                cannot be rebuilt without the fill log.
              </EmptyNote>
            ) : (
              <HoldTimeScatter
                fillWindow={FILL_WINDOW}
                loading={deskLoading}
                sample={holds}
              />
            )}
          </SectionCard>
        </div>

        <SectionCard
          action={
            <span className={META_LABEL}>
              <span className="font-mono">{filtered.length}</span> of{" "}
              <span className="font-mono">{rows.length}</span> markets
            </span>
          }
          icon={GridViewIcon}
          title="Screener"
        >
          <ScreenerTable
            error={error}
            loading={loading}
            maxima={maxima}
            onSelect={select}
            rows={filtered}
            selected={selected}
          />
        </SectionCard>

        <SectionCard
          action={<span className={META_LABEL}>Funding z vs 1h return</span>}
          icon={ChartScatterIcon}
          title="Crowding map"
        >
          <CrowdingScatter
            loading={loading}
            onSelect={select}
            rows={filtered}
            selected={selected}
          />
        </SectionCard>
      </div>

      <MarketSheet
        loading={loading}
        market={selected}
        maxima={maxima}
        nomineeTop={nomineeTop}
        onClose={closeSheet}
        row={selectedRow}
        snapshotTs={snapshot?.ts ?? null}
      />
    </>
  )
}
