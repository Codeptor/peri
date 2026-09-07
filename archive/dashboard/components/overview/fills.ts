import type { Trade } from "@/lib/api"
import { usd } from "@/lib/format"
import { netPnl } from "@/lib/stats"
import type { Hue } from "@/components/blocks/icon-chip"

/**
 * The fill taxonomy, in one place: how a `Trade.action` is named, which hue it
 * carries, and what it looks like as a mark on the equity curve. The fills table
 * and the hero's fill markers must never disagree about what a `veto_close` is.
 */
export const ACTION_TONE: Record<string, Hue> = {
  open: "accent",
  tp: "long",
  sl: "short",
  veto_close: "warning",
  time_stop: "neutral",
}

export const ACTION_LABEL: Record<string, string> = {
  open: "Open",
  tp: "Take profit",
  sl: "Stop loss",
  veto_close: "Veto close",
  time_stop: "Time stop",
}

/** Sentence-case fallback for any action kestreld adds after this build. */
export function actionLabel(action: string): string {
  const known = ACTION_LABEL[action]
  if (known) return known
  const words = action.replace(/_/g, " ")
  return words.charAt(0).toUpperCase() + words.slice(1)
}

/** Mark colour + fill style for a fill plotted on the equity curve. */
export type FillMark = { color: string; hollow: boolean }

const MARK: Record<string, FillMark> = {
  open: { color: "var(--accent-orange)", hollow: true },
  tp: { color: "var(--long)", hollow: false },
  sl: { color: "var(--short)", hollow: false },
  veto_close: { color: "var(--warning)", hollow: false },
}

/**
 * An entry is hollow because nothing has resolved yet; every close is solid.
 * Unknown closes fall back to the neutral chart hue rather than borrowing a
 * PnL colour they have not earned.
 */
export function fillMark(action: string): FillMark {
  return MARK[action] ?? { color: "var(--chart-6)", hollow: false }
}

/** Signed net of one fill — `+$42.30` · `-$3.10`. An entry is pure fee drag. */
export function fillNet(t: Trade): string {
  const net = netPnl(t)
  const sign = net > 0 ? "+" : net < 0 ? "-" : ""
  return `${sign}${usd(Math.abs(net))}`
}
