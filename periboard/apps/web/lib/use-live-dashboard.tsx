"use client"

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react"

import { api, type LiveSnapshot } from "./api"
import {
  liveWebSocketUrl,
  parseLiveFrame,
  snapshotStaleError,
} from "./live-stream"

const FALLBACK_POLL_MS = 15_000
const MAX_RECONNECT_MS = 10_000

type LiveDashboardState = {
  data: LiveSnapshot | null
  connected: boolean
  error: string | null
  receivedAt: number | null
  source: "websocket" | "poll" | "connecting"
}

const LiveDashboardContext = createContext<LiveDashboardState | null>(null)

export function LiveDashboardProvider({ children }: { children: ReactNode }) {
  const [data, setData] = useState<LiveSnapshot | null>(null)
  const [connected, setConnected] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [receivedAt, setReceivedAt] = useState<number | null>(null)

  useEffect(() => {
    let active = true
    let socket: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let fallbackTimer: ReturnType<typeof setInterval> | null = null
    let retries = 0

    function acceptSnapshot(snapshot: LiveSnapshot) {
      if (!active) return
      setData(snapshot)
      setReceivedAt(Date.now())
      setError(snapshotStaleError(snapshot))
    }

    async function poll() {
      try {
        const snapshot = await api.live()
        acceptSnapshot(snapshot)
      } catch (cause) {
        if (active) setError(cause instanceof Error ? cause.message : String(cause))
      }
    }

    function startFallback() {
      if (fallbackTimer) return
      void poll()
      fallbackTimer = setInterval(() => void poll(), FALLBACK_POLL_MS)
    }

    function stopFallback() {
      if (!fallbackTimer) return
      clearInterval(fallbackTimer)
      fallbackTimer = null
    }

    function connect() {
      if (!active) return
      socket = new WebSocket(liveWebSocketUrl(window.location))
      socket.onopen = () => {
        retries = 0
        setConnected(true)
        setError(null)
        stopFallback()
      }
      socket.onmessage = (event) => {
        const frame = parseLiveFrame(String(event.data))
        if (!frame) {
          setError("malformed live dashboard frame")
          return
        }
        if (frame.type === "error") {
          setError(frame.error)
          return
        }
        acceptSnapshot(frame.data as unknown as LiveSnapshot)
      }
      socket.onerror = () => socket?.close()
      socket.onclose = () => {
        if (!active) return
        setConnected(false)
        startFallback()
        const delay = Math.min(1000 * 2 ** retries, MAX_RECONNECT_MS)
        retries += 1
        reconnectTimer = setTimeout(connect, delay)
      }
    }

    startFallback()
    connect()
    return () => {
      active = false
      stopFallback()
      if (reconnectTimer) clearTimeout(reconnectTimer)
      socket?.close()
    }
  }, [])

  const value = useMemo<LiveDashboardState>(() => ({
    data,
    connected,
    error,
    receivedAt,
    source: connected ? "websocket" : data ? "poll" : "connecting",
  }), [connected, data, error, receivedAt])

  return (
    <LiveDashboardContext.Provider value={value}>
      {children}
    </LiveDashboardContext.Provider>
  )
}

export function useLiveDashboard() {
  const value = useContext(LiveDashboardContext)
  if (!value) throw new Error("useLiveDashboard must be used within LiveDashboardProvider")
  return value
}
