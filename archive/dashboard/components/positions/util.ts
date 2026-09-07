import type { Position, Trade } from "@/lib/api"
import { realizedNetPnl } from "@/lib/stats"
import type { Hue } from "@/components/blocks/icon-chip"

export function clamp01(n: number): number {
  if (!Number.isFinite(n)) return 0
  return Math.min(1, Math.max(0, n))
}

/**
 * Fraction of the entry→level distance the mark has travelled, 0..1.
 * Side-agnostic: `level - entry` already carries the direction sign
 * (long: tp above / sl below; short: mirrored).
 */
function travelled(entry: number, level: number, mark: number): number {
  const span = level - entry
  if (!Number.isFinite(span) || span === 0) return 0
  return clamp01((mark - entry) / span)
}

export function slProgress(p: Position): number {
  return travelled(p.entry_px, p.sl_px, p.mark_px)
}

export function tpProgress(p: Position): number {
  return travelled(p.entry_px, p.tp_px, p.mark_px)
}

/** Position value at the current mark. */
export function notional(p: Position): number {
  return Math.abs(p.size * p.mark_px)
}

/** Loss (≥0) that lands if the stop fills from here. */
export function riskToSl(p: Position): number {
  const per = p.side === "long" ? p.mark_px - p.sl_px : p.sl_px - p.mark_px
  return Math.max(0, per * p.size)
}

/** Signed distance from mark to a level, in percent of mark. */
export function distancePct(mark: number, level: number): number | null {
  if (!Number.isFinite(mark) || mark === 0 || !Number.isFinite(level)) {
    return null
  }
  return ((level - mark) / mark) * 100
}

export type BookTotals = {
  margin: number
  notional: number
  riskToSl: number
  unrealized: number
  roe: number | null
}

export function bookTotals(positions: Position[]): BookTotals {
  const margin = positions.reduce((s, p) => s + p.margin, 0)
  return {
    margin,
    notional: positions.reduce((s, p) => s + notional(p), 0),
    riskToSl: positions.reduce((s, p) => s + riskToSl(p), 0),
    unrealized: positions.reduce((s, p) => s + p.unrealized_pnl, 0),
    roe:
      margin > 1e-9
        ? (positions.reduce((s, p) => s + p.unrealized_pnl, 0) / margin) * 100
        : null,
  }
}

const ACTION_TONE: Record<string, Hue> = {
  open: "accent",
  tp: "long",
  sl: "short",
  veto_close: "warning",
  time_stop: "neutral",
}

export function actionTone(action: string): Hue {
  return ACTION_TONE[action] ?? "neutral"
}

const ACTION_LABEL: Record<string, string> = {
  open: "Open",
  tp: "TP",
  sl: "SL",
  veto_close: "Veto close",
  time_stop: "Time stop",
}

/** Sentence-case fill label — acronyms stay capitalised, everything else reads as prose. */
export function actionLabel(action: string): string {
  const known = ACTION_LABEL[action]
  if (known) return known
  const words = action.replace(/_/g, " ")
  return words.charAt(0).toUpperCase() + words.slice(1)
}

/** Closed-fill net — gross realized minus the fee; entries have no booked PnL. */
export function fillNet(t: Trade): number | null {
  return realizedNetPnl(t)
}

/** `MM-DD HH:MM` in UTC — the desk's clock. */
export function utcStamp(ts: number): string {
  return new Date(ts).toISOString().slice(5, 16).replace("T", " ")
}

/** Coarse holding time: `2d 4h`, `3h 12m`, `18m`. */
export function heldFor(openedTs: number, now: number): string {
  const secs = Math.max(0, Math.floor((now - openedTs) / 1000))
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  if (h >= 24) return `${Math.floor(h / 24)}d ${h % 24}h`
  if (h >= 1) return `${h}h ${m}m`
  return `${m}m`
}

/**
 * Text colour for a signed money figure. Neutral at exactly flat.
 * The `-ink` tokens (not the raw hues) because a 13px `--long` numeral only
 * clears 3.3:1 on the light card, while `--long-ink` clears ~7:1 there and
 * stays a lifted mint in dark.
 */
export function pnlTone(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n) || n === 0)
    return "text-muted-foreground"
  return n > 0 ? "text-long-ink" : "text-short-ink"
}

/** Semantic fill/dot colours for meters — theme-aware, legible on a `surface-2` track. */
export const SL_COLOR = "var(--short-ink)"
export const TP_COLOR = "var(--warning-ink)"
