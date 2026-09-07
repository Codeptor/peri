"use client"

import * as React from "react"

import { api, type Position, type Trade } from "@/lib/api"
import { useLiveFeed } from "@/lib/ws"

const POLL_MS = 15_000
const TRADE_LIMIT = 100

export type Book = {
  positions: Position[]
  trades: Trade[]
  loading: boolean
  refreshing: boolean
  error: string | null
  updatedAt: number | null
  refresh: () => void
}

const CLOCK_MS = 30_000
const clockListeners = new Set<() => void>()
let clock = 0
let clockTimer: ReturnType<typeof setInterval> | null = null

function subscribeClock(onChange: () => void): () => void {
  clockListeners.add(onChange)
  if (clockTimer == null) {
    clock = Date.now()
    clockTimer = setInterval(() => {
      clock = Date.now()
      for (const listener of clockListeners) listener()
    }, CLOCK_MS)
  }
  return () => {
    clockListeners.delete(onChange)
    if (clockListeners.size === 0 && clockTimer != null) {
      clearInterval(clockTimer)
      clockTimer = null
    }
  }
}

/**
 * One shared wall clock on a coarse cadence — `null` on the server so the first
 * client render matches the markup, then a real timestamp once mounted.
 */
export function useNow(): number | null {
  return React.useSyncExternalStore(
    subscribeClock,
    () => (clock === 0 ? null : clock),
    () => null
  )
}

/** Re-price a position off a fresh mid — mirrors kestreld's `positions_handler`. */
function repriced(p: Position, mid: number): Position {
  const unrealized_pnl =
    p.side === "long"
      ? (mid - p.entry_px) * p.size
      : (p.entry_px - mid) * p.size
  return {
    ...p,
    mark_px: mid,
    unrealized_pnl,
    roe: Math.abs(p.margin) > 1e-9 ? (unrealized_pnl / p.margin) * 100 : 0,
  }
}

/**
 * The open book + fill history. Polls kestreld, re-prices open positions off the
 * live `mids` stream, and re-reads on any `position` event (the socket payload
 * carries no mark/PnL, so the REST snapshot stays authoritative).
 */
export function useBook(): Book {
  const [positions, setPositions] = React.useState<Position[]>([])
  const [trades, setTrades] = React.useState<Trade[]>([])
  const [loading, setLoading] = React.useState(true)
  const [refreshing, setRefreshing] = React.useState(false)
  const [error, setError] = React.useState<string | null>(null)
  const [updatedAt, setUpdatedAt] = React.useState<number | null>(null)
  const [epoch, setEpoch] = React.useState(0)

  const refresh = React.useCallback(() => setEpoch((e) => e + 1), [])

  React.useEffect(() => {
    let cancelled = false

    const load = () => {
      setRefreshing(true)
      Promise.all([api.positions(), api.trades(TRADE_LIMIT)])
        .then(([nextPositions, nextTrades]) => {
          if (cancelled) return
          setPositions(nextPositions)
          setTrades(nextTrades)
          setError(null)
          setUpdatedAt(Date.now())
        })
        .catch((e: unknown) => {
          if (!cancelled) setError(e instanceof Error ? e.message : String(e))
        })
        .finally(() => {
          if (cancelled) return
          setLoading(false)
          setRefreshing(false)
        })
    }

    load()
    const id = setInterval(load, POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [epoch])

  useLiveFeed({
    mids: (mids) => {
      setPositions((prev) => {
        let changed = false
        const next = prev.map((p) => {
          const mid = mids[p.market]
          if (mid == null || !Number.isFinite(mid) || mid <= 0) return p
          if (mid === p.mark_px) return p
          changed = true
          return repriced(p, mid)
        })
        return changed ? next : prev
      })
    },
    position: refresh,
  })

  return { positions, trades, loading, refreshing, error, updatedAt, refresh }
}
