import type { Decision, NewsItem } from "@/lib/api"
import { usd } from "@/lib/format"

const DAY_MS = 86_400_000

/** kestreld writes `model <m> refused:<bool> latency:<ms>` plus optional
 *  ` refundable:<kind>:<excerpt>` and ` searched:"q1","q2"` on refusal paths. */
export type ReasonMeta = {
  model: string | null
  latencyMs: number | null
  refused: boolean
  refundable: string | null
  searched: string[]
  review: boolean
}

const MODEL_RE = /\bmodel\s+([^\s|]+)/
const LATENCY_RE = /\blatency:(\d+)/
const LATENCY_LOOSE_RE = /(\d+)\s*ms\b/
const REFUNDABLE_RE = /\brefundable:([a-z_]+)/
const QUOTED_RE = /"([^"]+)"/g

export function parseReason(reason: string | null | undefined): ReasonMeta {
  const src = reason ?? ""
  const named = MODEL_RE.exec(src)?.[1] ?? null
  const piped = src.includes("|") ? src.split("|")[0].trim() : ""
  const searchedAt = src.indexOf("searched:")
  const searchTail = searchedAt >= 0 ? src.slice(searchedAt) : ""
  const rawLatency =
    LATENCY_RE.exec(src)?.[1] ?? LATENCY_LOOSE_RE.exec(src)?.[1] ?? null

  return {
    model: named ?? (piped.length > 0 ? piped : null),
    // refusal is `refused:true` in the reason ONLY — never the bare word "refused"
    refused: /\brefused:true\b/.test(src),
    latencyMs: rawLatency == null ? null : Number(rawLatency),
    refundable: REFUNDABLE_RE.exec(src)?.[1] ?? null,
    searched: [...searchTail.matchAll(QUOTED_RE)].map((m) => m[1]),
    review: src.trimStart().startsWith("review;"),
  }
}

export type DecisionBucket = "executed" | "skipped" | "refused"

/** Exactly one bucket per decision: a refusal is a stack failure first, an outcome second. */
export function bucketOf(d: Decision): DecisionBucket {
  if (parseReason(d.reason).refused) return "refused"
  return d.executed ? "executed" : "skipped"
}

export type DecisionDay = {
  date: string
  executed: number
  skipped: number
  refused: number
  total: number
}

export function utcDay(ts: number): string {
  return new Date(ts).toISOString().slice(0, 10)
}

function startOfUtcDay(ts: number): number {
  const d = new Date(ts)
  return Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate())
}

/** `days` UTC buckets ending today — empty days are kept so the matrix keeps its shape. */
export function decisionDays(
  decisions: Decision[],
  days: number,
  now: number
): DecisionDay[] {
  const start = startOfUtcDay(now) - (days - 1) * DAY_MS
  const buckets = new Map<string, DecisionDay>()
  for (let i = 0; i < days; i++) {
    const date = utcDay(start + i * DAY_MS)
    buckets.set(date, { date, executed: 0, skipped: 0, refused: 0, total: 0 })
  }
  for (const d of decisions) {
    const day = buckets.get(utcDay(d.ts))
    if (!day) continue
    day[bucketOf(d)] += 1
    day.total += 1
  }
  return [...buckets.values()]
}

/** Only first / middle / last carry text — DotMatrix skips the empty ones. */
export function sparseLabels(dates: string[]): string[] {
  const mid = Math.floor(dates.length / 2)
  return dates.map((date, i) =>
    i === 0 || i === mid || i === dates.length - 1
      ? date.slice(5).replace("-", "/")
      : ""
  )
}

export function sparseDayLabels(days: DecisionDay[]): string[] {
  return sparseLabels(days.map((d) => d.date))
}

export function median(values: number[]): number | null {
  if (values.length === 0) return null
  const sorted = [...values].sort((a, b) => a - b)
  const mid = Math.floor(sorted.length / 2)
  return sorted.length % 2 === 1
    ? sorted[mid]
    : (sorted[mid - 1] + sorted[mid]) / 2
}

export type AnalystStats = {
  total: number
  refused: number
  rate: number | null
  p50LatencyMs: number | null
  model: string | null
}

export function analystStats(decisions: Decision[]): AnalystStats {
  let refused = 0
  let model: string | null = null
  const latencies: number[] = []
  for (const d of decisions) {
    const meta = parseReason(d.reason)
    if (meta.refused) refused += 1
    if (meta.latencyMs != null && Number.isFinite(meta.latencyMs)) {
      latencies.push(meta.latencyMs)
    }
    if (model == null && meta.model != null) model = meta.model
  }
  return {
    total: decisions.length,
    refused,
    rate: decisions.length === 0 ? null : refused / decisions.length,
    p50LatencyMs: median(latencies),
    model,
  }
}

export type NewsStats = { last: number | null; day: number; sources: number }

export function newsStats(items: NewsItem[], now: number): NewsStats {
  const sources = new Set<string>()
  let day = 0
  let last: number | null = null
  for (const n of items) {
    sources.add(n.source)
    if (now - n.ts < DAY_MS) day += 1
    if (last == null || n.ts > last) last = n.ts
  }
  return { last, day, sources: sources.size }
}

/** Newest-first union of the live-prepended list and a fresh poll, deduped by key. */
export function mergeFeed<T>(
  live: T[],
  polled: T[],
  key: (item: T) => string,
  ts: (item: T) => number,
  cap: number
): T[] {
  const seen = new Map<string, T>()
  for (const item of polled) seen.set(key(item), item)
  for (const item of live) if (!seen.has(key(item))) seen.set(key(item), item)
  return [...seen.values()].sort((a, b) => ts(b) - ts(a)).slice(0, cap)
}

export const decisionKey = (d: Decision): string =>
  `${d.ts}|${d.market}|${d.action}`

export function formatUptime(seconds: number | null | undefined): string {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return "—"
  const s = Math.floor(seconds)
  const pad = (n: number) => String(n).padStart(2, "0")
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return `${d}d ${pad(h)}h`
  if (h > 0) return `${h}h ${pad(m)}m`
  if (m > 0) return `${m}m ${pad(s % 60)}s`
  return `${s}s`
}

export function formatLatency(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "—"
  return ms >= 1000 ? `${(ms / 1000).toFixed(2)}s` : `${Math.round(ms)}ms`
}

/** Span in ms → `840ms` · `1.8s` · `12m 05s` · `2h 07m`. Ages and countdowns both. */
export function formatDurationMs(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "—"
  if (ms < 1000) return `${Math.round(ms)}ms`
  const secs = Math.floor(ms / 1000)
  if (secs < 60) return `${(ms / 1000).toFixed(1)}s`
  const pad = (n: number) => String(n).padStart(2, "0")
  const mins = Math.floor(secs / 60)
  if (mins < 60) return `${mins}m ${pad(secs % 60)}s`
  return `${Math.floor(mins / 60)}h ${pad(mins % 60)}m`
}

/** Epoch ms → `14:32 UTC`. Timezone-free by construction, so SSR and hydration agree. */
export function utcHm(ts: number | null | undefined): string {
  if (ts == null || !Number.isFinite(ts)) return "—"
  return `${new Date(ts).toISOString().slice(11, 16)} UTC`
}

/** Epoch ms → `08-09 14:30`. Same timezone-free construction, with the day kept. */
export function utcStamp(ts: number): string {
  const iso = new Date(ts).toISOString()
  return `${iso.slice(5, 10)} ${iso.slice(11, 16)}`
}

/** Signed USD for a pnl figure: `+$12.40` · `-$3.10` · `$0.00`. */
export function signedUsd(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—"
  const sign = n > 0 ? "+" : n < 0 ? "-" : ""
  return `${sign}${usd(Math.abs(n))}`
}

/**
 * PnL *text* colour. The ink tokens, never the raw hue: `--long` lands near 2.8:1
 * on the light card while `--long-ink` clears ~5:1 in both themes.
 */
export function pnlInk(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n) || n === 0)
    return "text-muted-foreground"
  return n > 0 ? "text-long-ink" : "text-short-ink"
}
