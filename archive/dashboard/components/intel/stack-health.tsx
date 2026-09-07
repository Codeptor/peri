"use client"

import type * as React from "react"
import NumberFlow from "@number-flow/react"
import {
  AiBrain01Icon,
  RssIcon,
  ServerStack01Icon,
  WifiConnected01Icon,
  WifiDisconnected01Icon,
} from "@hugeicons/core-free-icons"
import type { IconSvgElement } from "@hugeicons/react"

import type { Decision, Health, NewsItem } from "@/lib/api"
import { relativeTime } from "@/lib/format"
import { Skeleton } from "@/components/ui/skeleton"
import type { Hue } from "@/components/blocks/icon-chip"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import {
  analystStats,
  formatLatency,
  formatUptime,
  newsStats,
} from "@/components/intel/intel-utils"

const NEWS_FRESH_MS = 30 * 60_000

type HealthCardProps = {
  title: string
  icon: IconSvgElement
  tone: Hue
  status: string
  metric: React.ReactNode
  detail: string
  loading?: boolean
}

function HealthCard({
  title,
  icon,
  tone,
  status,
  metric,
  detail,
  loading = false,
}: HealthCardProps) {
  return (
    <SectionCard
      action={<StatusPill tone={tone}>{status}</StatusPill>}
      icon={icon}
      title={title}
    >
      {loading ? (
        <div className="flex flex-col gap-2.5">
          <Skeleton className="h-8 w-24 rounded-md" />
          <Skeleton className="h-4 w-32 rounded-md" />
        </div>
      ) : (
        <>
          <div className="font-mono text-[26px] leading-9 font-semibold tracking-tight">
            {metric}
          </div>
          <div className="mt-1 truncate text-[13px] text-muted-foreground">
            {detail}
          </div>
        </>
      )}
    </SectionCard>
  )
}

export type StackHealthProps = {
  health: Health | null
  /** false until the first health poll settles — separates "loading" from "unreachable" */
  healthLoaded: boolean
  decisions: Decision[] | null
  news: NewsItem[] | null
  wsConnected: boolean
  wsEvents: number
  wsLastTs: number | null
  /** null until the client clock is seeded — keeps the first render deterministic */
  now: number | null
}

/** Four vitals across the top of the wire: kestrel api · live socket · news pipe · analyst LLM. */
export function StackHealth({
  health,
  healthLoaded,
  decisions,
  news,
  wsConnected,
  wsEvents,
  wsLastTs,
  now,
}: StackHealthProps) {
  const analyst = analystStats(decisions ?? [])
  // the 24h window is clock-dependent, so the pipe card stays in its loading
  // shape until `now` lands on the client — never a server-guessed count
  const pipeReady = news != null && now != null
  const pipe =
    news != null && now != null
      ? newsStats(news, now)
      : { last: null, day: 0, sources: 0 }
  const pipeFresh =
    now != null && pipe.last != null && now - pipe.last < NEWS_FRESH_MS

  const analystTone: Hue =
    analyst.rate == null
      ? "neutral"
      : analyst.rate >= 0.2
        ? "short"
        : analyst.rate > 0
          ? "warning"
          : "long"

  return (
    <div className="grid gap-5 sm:grid-cols-2 xl:grid-cols-4">
      <HealthCard
        detail={
          health
            ? `Uptime · ${health.markets_tracked} markets tracked`
            : "Daemon unreachable"
        }
        icon={ServerStack01Icon}
        loading={!healthLoaded}
        metric={health ? formatUptime(health.uptime_s) : "—"}
        status={health ? (health.ok ? "Online" : "Fault") : "No data"}
        title="Kestrel API"
        tone={health ? (health.ok ? "long" : "short") : "neutral"}
      />

      <HealthCard
        detail={
          wsLastTs != null
            ? `Events · last ${relativeTime(wsLastTs)}`
            : "Events · awaiting stream"
        }
        icon={wsConnected ? WifiConnected01Icon : WifiDisconnected01Icon}
        metric={<NumberFlow value={wsEvents} />}
        status={wsConnected ? "Streaming" : "Offline"}
        title="Live socket"
        tone={wsConnected ? "long" : "short"}
      />

      <HealthCard
        detail={
          pipe.last == null
            ? "Items in 24h · nothing ingested"
            : `Items in 24h · last ${relativeTime(pipe.last)} · ${pipe.sources} sources`
        }
        icon={RssIcon}
        loading={!pipeReady}
        metric={<NumberFlow value={pipe.day} />}
        status={pipe.last == null ? "Idle" : pipeFresh ? "Fresh" : "Stale"}
        title="News pipe"
        tone={pipe.last == null ? "neutral" : pipeFresh ? "long" : "warning"}
      />

      <HealthCard
        detail={
          analyst.total === 0
            ? "Refusal rate · no decisions logged"
            : `Refused ${analyst.refused} of ${analyst.total} · p50 ${formatLatency(analyst.p50LatencyMs)}${analyst.model ? ` · ${analyst.model}` : ""}`
        }
        icon={AiBrain01Icon}
        loading={decisions == null}
        metric={
          analyst.rate == null ? (
            "—"
          ) : (
            <NumberFlow
              format={{ style: "percent", maximumFractionDigits: 1 }}
              value={analyst.rate}
            />
          )
        }
        status={
          analyst.rate == null
            ? "No data"
            : analyst.rate >= 0.2
              ? "Refusing"
              : analyst.rate > 0
                ? "Degraded"
                : "Clean"
        }
        title="Analyst LLM"
        tone={analystTone}
      />
    </div>
  )
}
