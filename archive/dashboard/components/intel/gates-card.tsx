"use client"

import type * as React from "react"
import { SecurityCheckIcon } from "@hugeicons/core-free-icons"

import type { CooldownGate, GatesState, PerMarketGate } from "@/lib/api"
import { usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { HUE_TINT, type Hue } from "@/components/blocks/icon-chip"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { Skeleton } from "@/components/ui/skeleton"
import { StateUnavailable } from "@/components/intel/feed-state"
import { formatDurationMs, utcHm } from "@/components/intel/intel-utils"

type GateRow = {
  key: string
  label: string
  tone: Hue
  status: string
  /** the numeral itself → mono */
  value: string
  /** one sans line saying what the numeral means, or what it is refusing */
  detail: string
}

/** Warn from three quarters of a budget onwards, refuse at the cap. */
function budgetTone(count: number, cap: number): Hue {
  if (cap <= 0 || count >= cap) return "short"
  return count >= cap * 0.75 ? "warning" : "long"
}

/**
 * One row per rail, in the order the entry path runs them: kill latch, daily cap,
 * morning pacing, data staleness, regime. Nothing here is re-derived — the numbers
 * are kestreld's own gate state, so a row and a refusal in the log cannot disagree.
 */
function gateRows(g: GatesState): GateRow[] {
  const seeded = Number.isFinite(g.kill.day_open) && g.kill.day_open > 0
  const floor = g.kill.day_open * (1 - g.kill.threshold_px_pct / 100)
  const dailyLeft = Math.max(0, g.daily.cap - g.daily.count)
  const morningLeft = Math.max(0, g.morning.budget - g.morning.count)
  const morningSpent = g.morning.count >= g.morning.budget
  const vol = g.regime.btc_vol1h

  return [
    {
      key: "kill",
      label: "Kill switch",
      tone: g.kill.active ? "short" : "long",
      status: g.kill.active ? "Latched" : "Armed",
      value: seeded ? usd(g.kill.day_open) : "—",
      detail: g.kill.active
        ? `Every entry refused until 00:00 UTC · floor ${usd(floor)}`
        : seeded
          ? `Day open · floor ${usd(floor)} at −${g.kill.threshold_px_pct.toFixed(1)}%`
          : "Day open not stamped yet — floor unset",
    },
    {
      key: "daily",
      label: "Daily cap",
      tone: budgetTone(g.daily.count, g.daily.cap),
      status: g.daily.count >= g.daily.cap ? "Capped" : "Open",
      value: `${g.daily.count} / ${g.daily.cap}`,
      detail:
        g.daily.count >= g.daily.cap
          ? "Daily entry cap reached — nothing opens until 00:00 UTC"
          : `${dailyLeft} entries left today`,
    },
    {
      key: "morning",
      label: "Morning budget",
      tone: g.morning.before_noon_utc
        ? budgetTone(g.morning.count, g.morning.budget)
        : "neutral",
      status: !g.morning.before_noon_utc
        ? "Released"
        : morningSpent
          ? "Spent"
          : "Binding",
      value: `${g.morning.count} / ${g.morning.budget}`,
      detail: !g.morning.before_noon_utc
        ? "Past 12:00 UTC — pacing no longer binds"
        : morningSpent
          ? "Morning pacing spent — entries wait for 12:00 UTC"
          : `${morningLeft} left before 12:00 UTC`,
    },
    {
      key: "staleness",
      label: "Data staleness",
      tone: g.staleness.stale ? "short" : "long",
      status: g.staleness.stale ? "Stale" : "Fresh",
      value: `${formatDurationMs(g.staleness.age_ms)} / ${Math.round(g.staleness.max_ms / 1000)}s`,
      detail: g.staleness.stale
        ? "Entries refused until the feed catches up"
        : "Feature age against the max an entry may rest on",
    },
    {
      key: "regime",
      label: "Regime",
      tone: vol == null ? "neutral" : g.regime.active ? "short" : "long",
      status: vol == null ? "No data" : g.regime.active ? "Blocking" : "Calm",
      value: `${vol == null ? "—" : vol.toFixed(2)} / ${g.regime.max.toFixed(2)}`,
      detail:
        vol == null
          ? "No BTC features — the gate is inactive"
          : g.regime.active
            ? "BTC 1h volatility above the ceiling"
            : "BTC 1h volatility against the ceiling",
    },
  ]
}

/** True when at least one rail is actively refusing entries right now. */
function isBenched(g: GatesState): boolean {
  return (
    g.kill.active ||
    g.daily.count >= g.daily.cap ||
    (g.morning.before_noon_utc && g.morning.count >= g.morning.budget) ||
    g.staleness.stale ||
    g.regime.active
  )
}

function GateRowView({ row }: { row: GateRow }) {
  return (
    <div className="border-b border-border py-3 first:pt-0 last:border-0 last:pb-0">
      <div className="flex items-center gap-3">
        <span className="min-w-0 flex-1 truncate text-[13px]">{row.label}</span>
        <StatusPill className="shrink-0" tone={row.tone}>
          {row.status}
        </StatusPill>
        <span className="w-24 shrink-0 text-right font-mono text-[13px] font-medium tabular-nums">
          {row.value}
        </span>
      </div>
      <div className="mt-1 text-[11.5px] leading-4 text-muted-foreground">
        {row.detail}
      </div>
    </div>
  )
}

function SubLabel({ children }: { children: React.ReactNode }) {
  return <div className="text-[11.5px] text-muted-foreground">{children}</div>
}

function EntryChip({ row }: { row: PerMarketGate }) {
  const capped = row.entries_today >= row.cap
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11.5px] leading-5",
        capped ? HUE_TINT.warning : HUE_TINT.neutral
      )}
      title={capped ? `${row.market} is out of entries today` : undefined}
    >
      <span className="font-mono font-medium">{row.market}</span>
      <span className="font-mono tabular-nums">
        {row.entries_today}/{row.cap}
      </span>
    </span>
  )
}

function CooldownChip({ c, now }: { c: CooldownGate; now: number | null }) {
  const left = now == null ? null : c.until_ts - now
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11.5px] leading-5",
        c.cause === "sl" ? HUE_TINT.warning : HUE_TINT.neutral
      )}
    >
      <span className="font-mono font-medium">{c.market}</span>
      <span>{c.cause === "sl" ? "post-stop" : "cooldown"}</span>
      <span className="font-mono tabular-nums">until {utcHm(c.until_ts)}</span>
      {left != null && left > 0 ? (
        <span className="font-mono tabular-nums opacity-70">
          · {formatDurationMs(left)} left
        </span>
      ) : null}
    </span>
  )
}

export type GatesCardProps = {
  gates: GatesState | null
  /** set when the endpoint 503'd or was unreachable — renders the neutral row */
  note: string | null
  /** false until the first poll settles — separates "loading" from "unavailable" */
  loaded: boolean
  /** null until the client clock is seeded — keeps the first render deterministic */
  now: number | null
}

/** Why the trader is benched right now: every entry rail, one read. */
export function GatesCard({ gates, note, loaded, now }: GatesCardProps) {
  const rows = gates ? gateRows(gates) : []
  const benched = gates ? isBenched(gates) : false

  return (
    <SectionCard
      action={
        gates ? (
          <StatusPill tone={benched ? "short" : "long"}>
            {benched ? "Benched" : "Clear"}
          </StatusPill>
        ) : null
      }
      icon={SecurityCheckIcon}
      title="Gates"
    >
      {!loaded ? (
        <div className="flex flex-col gap-4">
          {Array.from({ length: 5 }, (_, i) => (
            <div className="flex flex-col gap-1.5" key={i}>
              <div className="flex items-center gap-3">
                <Skeleton className="h-4 w-28 rounded-md" />
                <Skeleton className="ml-auto h-4 w-16 rounded-full" />
                <Skeleton className="h-4 w-20 rounded-md" />
              </div>
              <Skeleton className="h-3 w-2/3 rounded-md" />
            </div>
          ))}
        </div>
      ) : gates == null ? (
        <StateUnavailable note={note} />
      ) : (
        <>
          <div className="flex flex-col">
            {rows.map((row) => (
              <GateRowView key={row.key} row={row} />
            ))}
          </div>

          <div className="mt-4 border-t border-border pt-3.5">
            <SubLabel>Entries today</SubLabel>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {gates.per_market.length === 0 ? (
                <span className="text-[13px] text-muted-foreground">
                  No market has been entered today
                </span>
              ) : (
                gates.per_market.map((row) => (
                  <EntryChip key={row.market} row={row} />
                ))
              )}
            </div>
          </div>

          <div className="mt-3.5">
            <SubLabel>Cooldowns</SubLabel>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {gates.cooldowns.length === 0 ? (
                <span className="text-[13px] text-muted-foreground">
                  None active
                </span>
              ) : (
                gates.cooldowns.map((c) => (
                  <CooldownChip c={c} key={c.market} now={now} />
                ))
              )}
            </div>
          </div>
        </>
      )}
    </SectionCard>
  )
}
