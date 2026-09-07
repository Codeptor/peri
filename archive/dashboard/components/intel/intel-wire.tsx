"use client"

import * as React from "react"
import {
  Cancel01Icon,
  RefreshIcon,
  SatelliteIcon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import { api, type Decision, type Health, type NewsItem } from "@/lib/api"
import { cn } from "@/lib/utils"
import { useLiveFeed } from "@/lib/ws"
import { PageHeader } from "@/components/blocks/page-header"
import { Button } from "@/components/ui/button"
import { DecisionFeed } from "@/components/intel/decision-feed"
import { DecisionThroughput } from "@/components/intel/decision-throughput"
import { decisionKey, mergeFeed } from "@/components/intel/intel-utils"
import { LearningLoop } from "@/components/intel/learning-loop"
import { NewsFeed } from "@/components/intel/news-feed"
import { StackHealth } from "@/components/intel/stack-health"

const POLL_MS = 20_000
const CLOCK_MS = 10_000
const WS_FLUSH_MS = 1_000
const DECISION_CAP = 200
const NEWS_CAP = 100

type WsStats = { events: number; lastTs: number | null }

/** /intel — the wire: stack vitals, decision throughput, analyst calls, news. */
export function IntelWire() {
  const [decisions, setDecisions] = React.useState<Decision[] | null>(null)
  const [news, setNews] = React.useState<NewsItem[] | null>(null)
  const [health, setHealth] = React.useState<Health | null>(null)
  const [healthLoaded, setHealthLoaded] = React.useState(false)
  const [refreshing, setRefreshing] = React.useState(false)
  const [filter, setFilter] = React.useState<string | null>(null)
  // null on the server AND on the first client render, so SSR and hydration
  // render byte-identically. It is seeded from the first `load()` settle (an
  // async callback, never the effect body) and then ticked by the clock
  // interval; every clock-derived view holds its skeleton until it lands.
  const [now, setNow] = React.useState<number | null>(null)
  const [ws, setWs] = React.useState<WsStats>({ events: 0, lastTs: null })

  const mounted = React.useRef(true)
  const pendingEvents = React.useRef(0)

  // state only ever lands in the settle callback — the mount effect itself renders nothing new
  const load = React.useCallback(
    () =>
      Promise.allSettled([
        api.decisions(DECISION_CAP),
        api.news(NEWS_CAP),
        api.health(),
      ]).then(([d, n, h]) => {
        if (!mounted.current) return
        if (d.status === "fulfilled") {
          setDecisions((prev) =>
            mergeFeed(
              prev ?? [],
              d.value,
              decisionKey,
              (x) => x.ts,
              DECISION_CAP
            )
          )
        } else {
          setDecisions((prev) => prev ?? [])
        }
        if (n.status === "fulfilled") {
          setNews((prev) =>
            mergeFeed(
              prev ?? [],
              n.value,
              (x) => String(x.id),
              (x) => x.ts,
              NEWS_CAP
            )
          )
        } else {
          setNews((prev) => prev ?? [])
        }
        setHealth(h.status === "fulfilled" ? h.value : null)
        setHealthLoaded(true)
        setNow(Date.now())
      }),
    []
  )

  const refresh = React.useCallback(() => {
    setRefreshing(true)
    void load().finally(() => {
      if (mounted.current) setRefreshing(false)
    })
  }, [load])

  React.useEffect(() => {
    mounted.current = true
    void load()
    const poll = setInterval(() => void load(), POLL_MS)
    const clock = setInterval(() => setNow(Date.now()), CLOCK_MS)
    // ws chatter is flushed on a timer so mids traffic can never re-render per message
    const flush = setInterval(() => {
      if (pendingEvents.current === 0) return
      const batch = pendingEvents.current
      pendingEvents.current = 0
      setWs((prev) => ({ events: prev.events + batch, lastTs: Date.now() }))
    }, WS_FLUSH_MS)
    return () => {
      mounted.current = false
      clearInterval(poll)
      clearInterval(clock)
      clearInterval(flush)
    }
  }, [load])

  const bump = () => {
    pendingEvents.current += 1
  }

  const { connected } = useLiveFeed({
    mids: bump,
    equity: bump,
    position: bump,
    decision: (d) => {
      bump()
      setDecisions((prev) =>
        mergeFeed([d], prev ?? [], decisionKey, (x) => x.ts, DECISION_CAP)
      )
    },
    news: (n) => {
      bump()
      setNews((prev) =>
        mergeFeed(
          [n],
          prev ?? [],
          (x) => String(x.id),
          (x) => x.ts,
          NEWS_CAP
        )
      )
    },
  })

  const selectMarket = React.useCallback((market: string) => {
    setFilter((prev) => (prev === market ? null : market))
  }, [])

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
        icon={SatelliteIcon}
        meta={
          <>
            <span>Stack vitals, analyst calls and the news pipe</span>
            <span>
              <span className="font-mono">{decisions?.length ?? 0}</span> calls
              · <span className="font-mono">{news?.length ?? 0}</span> headlines
            </span>
            {filter ? (
              <button
                className="inline-flex items-center gap-1.5 rounded-full bg-primary/12 px-2.5 py-0.5 text-xs font-medium text-primary-ink transition-colors hover:bg-primary/20"
                onClick={() => setFilter(null)}
                type="button"
              >
                Filtered to <span className="font-mono">{filter}</span>
                <HugeiconsIcon
                  icon={Cancel01Icon}
                  size={12}
                  strokeWidth={2.2}
                />
              </button>
            ) : null}
          </>
        }
        title="Wire"
      />

      <div className="flex flex-col gap-5">
        <StackHealth
          decisions={decisions}
          health={health}
          healthLoaded={healthLoaded}
          news={news}
          now={now}
          wsConnected={connected}
          wsEvents={ws.events}
          wsLastTs={ws.lastTs}
        />

        <LearningLoop />

        <DecisionThroughput decisions={decisions} now={now} />

        <div className="grid min-w-0 gap-5 xl:grid-cols-[1.35fr_1fr]">
          <DecisionFeed
            decisions={decisions}
            filter={filter}
            onSelectMarket={selectMarket}
          />
          <NewsFeed filter={filter} news={news} onSelectMarket={selectMarket} />
        </div>
      </div>
    </>
  )
}
