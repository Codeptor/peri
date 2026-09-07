"use client"

import * as React from "react"

import { api, type Analytics, type GatesState, type Trade } from "@/lib/api"
import { AnalyticsCard } from "@/components/intel/analytics-card"
import { GatesCard } from "@/components/intel/gates-card"
import { deriveCloses } from "@/components/intel/r-multiple"
import { RMultipleCard } from "@/components/intel/r-multiple-card"
import { RollingDiscipline } from "@/components/intel/rolling-discipline"

/** Gate state turns over on every entry attempt; 15s is fast enough to catch a bench. */
const GATES_POLL_MS = 15_000

/**
 * The ledger side moves on closes, not on ticks, and `/api/analytics` is the one endpoint
 * that does real work on the read path — it drains the counterfactual backlog, replaying
 * vetoed brackets against 1m candles under a 12s budget. Polling that at gate cadence
 * would keep a candle fetch permanently in flight for numbers that change a few times a
 * day, so it rides its own minute.
 */
const LEDGER_POLL_MS = 60_000

/** One page of fills. Closes older than this fall out of the R timeline, not out of the totals. */
const TRADES_CAP = 600

type Fetched<T> = { data: T | null; note: string | null }

const EMPTY: Fetched<never> = { data: null, note: null }

/**
 * `fetchJSON` throws `"<path> <status>"`, so a wedged rail (503) and an unreachable
 * daemon are told apart here and rendered as a neutral row — never an error boundary.
 */
function unavailableNote(reason: unknown): string {
  const msg = reason instanceof Error ? reason.message : String(reason)
  const code = /(\d{3})$/.exec(msg)?.[1]
  return code ? `kestreld responded ${code}` : "kestreld unreachable"
}

function settle<T>(result: PromiseSettledResult<T>): Fetched<T> {
  return result.status === "fulfilled"
    ? { data: result.value, note: null }
    : { data: null, note: unavailableNote(result.reason) }
}

/**
 * The learning loop: why the trader is benched, whether its decisions are paying, and
 * what the closes say about the risk it took to get there.
 *
 * Both cadences settle independently of each other's failures — a wedged gate rail never
 * blanks the ledger cards, and a slow analytics replay never delays a gate flip.
 */
export function LearningLoop() {
  const [gates, setGates] = React.useState<Fetched<GatesState>>(EMPTY)
  const [gatesLoaded, setGatesLoaded] = React.useState(false)
  // null on the server AND on the first client render, so SSR and hydration match;
  // seeded from the first settle, which is the only place cooldown countdowns read it.
  const [now, setNow] = React.useState<number | null>(null)

  const [analytics, setAnalytics] = React.useState<Fetched<Analytics>>(EMPTY)
  const [trades, setTrades] = React.useState<Fetched<Trade[]>>(EMPTY)
  const [ledgerLoaded, setLedgerLoaded] = React.useState(false)

  React.useEffect(() => {
    let mounted = true
    const load = () =>
      Promise.allSettled([api.gates()]).then(([g]) => {
        if (!mounted) return
        setGates(settle(g))
        setGatesLoaded(true)
        setNow(Date.now())
      })
    void load()
    const poll = setInterval(() => void load(), GATES_POLL_MS)
    return () => {
      mounted = false
      clearInterval(poll)
    }
  }, [])

  React.useEffect(() => {
    let mounted = true
    const load = () =>
      Promise.allSettled([api.analytics(), api.trades(TRADES_CAP)]).then(
        ([a, t]) => {
          if (!mounted) return
          setAnalytics(settle(a))
          setTrades(settle(t))
          setLedgerLoaded(true)
        }
      )
    void load()
    const poll = setInterval(() => void load(), LEDGER_POLL_MS)
    return () => {
      mounted = false
      clearInterval(poll)
    }
  }, [])

  // The counterfactual rows are the only source for a vetoed close's stop distance, so the
  // two payloads are joined here rather than inside either card. `rows` is optional at
  // runtime only: a fixture captured before the field existed still has to render.
  const closeSet = React.useMemo(
    () =>
      trades.data == null
        ? null
        : deriveCloses(trades.data, analytics.data?.counterfactuals.rows ?? []),
    [trades.data, analytics.data]
  )

  return (
    <>
      <div className="grid min-w-0 gap-5 xl:grid-cols-2">
        <GatesCard
          gates={gates.data}
          loaded={gatesLoaded}
          note={gates.note}
          now={now}
        />
        <AnalyticsCard
          analytics={analytics.data}
          loaded={ledgerLoaded}
          note={analytics.note}
        />
      </div>

      <div className="grid min-w-0 gap-5 xl:grid-cols-[1.35fr_1fr]">
        <RMultipleCard
          closeSet={closeSet}
          loaded={ledgerLoaded}
          note={trades.note}
        />
        <RollingDiscipline
          closeSet={closeSet}
          loaded={ledgerLoaded}
          note={trades.note}
        />
      </div>
    </>
  )
}
