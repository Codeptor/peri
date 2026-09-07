type BrowserLocation = { protocol: string; host: string }

export function liveWebSocketUrl(location: BrowserLocation): string {
  const protocol = location.protocol === "https:" ? "wss:" : "ws:"
  return `${protocol}//${location.host}/peri/ws`
}

export type SnapshotFrame = {
  type: "snapshot"
  data: {
    as_of_ts: number
    account: Record<string, unknown>
    positions: unknown[]
    orders: unknown[]
    realized: Record<string, unknown>
    closes: unknown[]
    runtime: Record<string, unknown>
  }
}

export type ErrorFrame = { type: "error"; error: string }

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

export function parseLiveFrame(raw: string): SnapshotFrame | ErrorFrame | null {
  let frame: unknown
  try {
    frame = JSON.parse(raw)
  } catch {
    return null
  }
  if (!isRecord(frame)) return null
  if (frame.type === "error" && typeof frame.error === "string") {
    return { type: "error", error: frame.error }
  }
  if (frame.type !== "snapshot" || !isRecord(frame.data)) return null
  const data = frame.data
  if (
    typeof data.as_of_ts !== "number" ||
    !isRecord(data.account) ||
    !Array.isArray(data.positions) ||
    !Array.isArray(data.orders) ||
    !isRecord(data.realized) ||
    !Array.isArray(data.closes) ||
    !isRecord(data.runtime)
  ) {
    return null
  }
  return frame as SnapshotFrame
}

export function snapshotAgeMs(snapshot: { as_of_ts: number }, nowMs = Date.now()): number {
  return Math.max(0, nowMs - snapshot.as_of_ts * 1000)
}

export function snapshotStaleError(snapshot: {
  stale?: boolean
  stale_reason?: string | null
}): string | null {
  if (!snapshot.stale) return null
  return snapshot.stale_reason || "Live venue snapshot is stale"
}
