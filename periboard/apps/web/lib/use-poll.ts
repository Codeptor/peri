"use client"

import { useCallback, useEffect, useRef, useState } from "react"

/** Poll an async loader on an interval; peri's ledger changes at most every
 * few minutes, so 15s default keeps the UI fresh without hammering. */
export function usePoll<T>(load: () => Promise<T>, intervalMs = 15_000) {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState<string | null>(null)
  const loadRef = useRef(load)
  loadRef.current = load

  const tick = useCallback(async () => {
    try {
      setData(await loadRef.current())
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    void tick()
    const t = setInterval(() => void tick(), intervalMs)
    return () => clearInterval(t)
  }, [tick, intervalMs])

  return { data, error, reload: tick }
}
