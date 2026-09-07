"use client"

import * as React from "react"

import {
  api,
  type EquityPoint,
  type Health,
  type Position,
  type Trade,
} from "@/lib/api"
import { useLiveFeed } from "@/lib/ws"

const POLL_MS = 15_000
const EQUITY_POINTS = 500
const TRADE_LIMIT = 500

/**
 * kestreld can wedge a single endpoint while the rest stay sub-10ms, so no one
 * request is allowed to hold the board hostage. A wedged endpoint is also never
 * re-issued while its previous call is still outstanding — otherwise the poll
 * would pile up requests until the browser's per-host connection cap starves
 * the healthy endpoints.
 */
const REQUEST_TIMEOUT_MS = 8_000

type Outcome = "ok" | "fail" | "skip"

/** Data older than this is disclosed in the header instead of silently trusted. */
export const STALE_MS = 5 * 60_000

export type OverviewState = {
  equity: EquityPoint[]
  positions: Position[]
  trades: Trade[]
  health: Health | null
  loading: boolean
  /** Every endpoint failed — kestreld is unreachable. */
  failed: boolean
  refreshing: boolean
  connected: boolean
  /** Newest timestamp in the loaded data; every time window anchors to it. */
  asOf: number | null
  /** Wall clock sampled at each poll, so render stays pure. */
  now: number
  refresh: () => void
}

/** Re-mark a position against a live mid: uPnL = (mark − entry) × size × dir. */
function withMark(p: Position, mark: number): Position {
  const dir = p.side === "long" ? 1 : -1
  const unrealized = (mark - p.entry_px) * p.size * dir
  return {
    ...p,
    mark_px: mark,
    unrealized_pnl: unrealized,
    roe: p.margin > 0 ? (unrealized / p.margin) * 100 : p.roe,
  }
}

/**
 * The overview's whole data layer: snapshot fetch + 15s poll, live websocket
 * merge (mids re-mark open positions, equity ticks extend the curve), and the
 * `asOf` clock the page reads its windows from.
 */
export function useOverview(): OverviewState {
  const [equity, setEquity] = React.useState<EquityPoint[]>([])
  const [rawPositions, setRawPositions] = React.useState<Position[]>([])
  const [trades, setTrades] = React.useState<Trade[]>([])
  const [health, setHealth] = React.useState<Health | null>(null)
  const [marks, setMarks] = React.useState<Record<string, number>>({})
  const [loading, setLoading] = React.useState(true)
  const [failed, setFailed] = React.useState(false)
  const [refreshing, setRefreshing] = React.useState(false)
  const [now, setNow] = React.useState(0)

  const aliveRef = React.useRef(true)
  React.useEffect(() => {
    aliveRef.current = true
    return () => {
      aliveRef.current = false
    }
  }, [])

  const inFlightRef = React.useRef<Record<string, boolean>>({})

  const run = React.useCallback(
    <T>(
      key: string,
      call: () => Promise<T>,
      apply: (value: T) => void
    ): Promise<Outcome> => {
      if (inFlightRef.current[key]) return Promise.resolve<Outcome>("skip")
      inFlightRef.current[key] = true
      const request = call()
      void request
        .catch(() => {})
        .finally(() => {
          inFlightRef.current[key] = false
        })
      return Promise.race([
        request.then((value): Outcome => {
          if (aliveRef.current) apply(value)
          return "ok"
        }),
        new Promise<Outcome>((resolve) =>
          setTimeout(() => resolve("fail"), REQUEST_TIMEOUT_MS)
        ),
      ]).catch((): Outcome => "fail")
    },
    []
  )

  const load = React.useCallback(
    () =>
      Promise.all([
        run("equity", () => api.equity(EQUITY_POINTS), setEquity),
        run("positions", () => api.positions(), setRawPositions),
        run("trades", () => api.trades(TRADE_LIMIT), setTrades),
        run("health", () => api.health(), setHealth),
      ]).then((outcomes) => {
        if (!aliveRef.current) return
        // Skipped calls say nothing about reachability — only decided ones do.
        const decided = outcomes.filter((o) => o !== "skip")
        if (decided.length > 0) {
          setFailed(decided.every((o) => o === "fail"))
        }
        setNow(Date.now())
        setLoading(false)
      }),
    [run]
  )

  React.useEffect(() => {
    void load()
    const id = setInterval(() => void load(), POLL_MS)
    return () => clearInterval(id)
  }, [load])

  const refresh = React.useCallback(() => {
    setRefreshing(true)
    void load().finally(() => {
      if (aliveRef.current) setRefreshing(false)
    })
  }, [load])

  // `useLiveFeed` re-reads its handler bundle every render, so these closures
  // always see current state — no refs needed.
  const { connected } = useLiveFeed({
    mids: (incoming) => {
      setMarks((prev) => {
        let next: Record<string, number> | null = null
        for (const position of rawPositions) {
          const px = incoming[position.market]
          if (typeof px === "number" && prev[position.market] !== px) {
            next ??= { ...prev }
            next[position.market] = px
          }
        }
        return next ?? prev
      })
    },
    position: (incoming) => {
      setRawPositions((prev) => {
        const i = prev.findIndex((p) => p.id === incoming.id)
        if (i === -1) return [incoming, ...prev]
        const next = [...prev]
        next[i] = incoming
        return next
      })
    },
    equity: (point) => {
      setEquity((prev) => {
        const last = prev[prev.length - 1]
        if (last && point.ts <= last.ts) return prev
        return [...prev, point].slice(-EQUITY_POINTS)
      })
    },
  })

  const positions = React.useMemo(
    () =>
      rawPositions.map((p) => {
        const mark = marks[p.market]
        return typeof mark === "number" ? withMark(p, mark) : p
      }),
    [rawPositions, marks]
  )

  const asOf = React.useMemo(() => {
    let newest = 0
    const lastEquity = equity[equity.length - 1]
    if (lastEquity) newest = Math.max(newest, lastEquity.ts)
    for (const t of trades) if (t.ts > newest) newest = t.ts
    for (const p of positions) if (p.opened_ts > newest) newest = p.opened_ts
    return newest > 0 ? newest : null
  }, [equity, trades, positions])

  return {
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
  }
}
