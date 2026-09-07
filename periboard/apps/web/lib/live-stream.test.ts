import { describe, expect, test } from "bun:test"

import {
  liveWebSocketUrl,
  parseLiveFrame,
  snapshotAgeMs,
  snapshotStaleError,
} from "./live-stream"

describe("live dashboard stream", () => {
  test("uses the same dashboard origin and Next websocket rewrite", () => {
    expect(liveWebSocketUrl({ protocol: "http:", host: "192.168.1.6:3475" })).toBe(
      "ws://192.168.1.6:3475/peri/ws"
    )
    expect(liveWebSocketUrl({ protocol: "https:", host: "peri.example" })).toBe(
      "wss://peri.example/peri/ws"
    )
  })

  test("accepts complete snapshots and rejects malformed frames", () => {
    const snapshot = {
      as_of_ts: 123,
      account: { equity: 64.31 },
      positions: [],
      orders: [{ oid: 9 }],
      realized: { total: 4.67, close_count: 3, scope: "recent venue fills" },
      closes: [{ id: "close-1", realized_pnl: 4.52, closed_ts: 123 }],
      runtime: { phase: "idle" },
    }
    expect(parseLiveFrame(JSON.stringify({ type: "snapshot", data: snapshot }))).toEqual({
      type: "snapshot",
      data: snapshot,
    })
    expect(parseLiveFrame("not json")).toBeNull()
    expect(parseLiveFrame(JSON.stringify({ type: "snapshot", data: { orders: [] } }))).toBeNull()
    expect(parseLiveFrame(JSON.stringify({
      type: "snapshot",
      data: { ...snapshot, closes: undefined },
    }))).toBeNull()
    expect(parseLiveFrame(JSON.stringify({ type: "error", error: "venue down" }))).toEqual({
      type: "error",
      error: "venue down",
    })
  })

  test("measures upstream freshness from the server timestamp", () => {
    expect(snapshotAgeMs({ as_of_ts: 100 }, 103_500)).toBe(3500)
  })

  test("surfaces last-good snapshots as stale", () => {
    expect(snapshotStaleError({ stale: false, stale_reason: null })).toBeNull()
    expect(
      snapshotStaleError({ stale: true, stale_reason: "ClientError: venue 429" })
    ).toBe("ClientError: venue 429")
    expect(snapshotStaleError({ stale: true, stale_reason: null })).toBe(
      "Live venue snapshot is stale"
    )
  })
})
