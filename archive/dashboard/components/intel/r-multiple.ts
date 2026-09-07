import type { CounterfactualRow, Trade } from "@/lib/api"

/**
 * R-multiple derivation — the one place the wire turns raw fills into risk-normalised
 * outcomes. Kept pure and separate from the cards so the arithmetic can be read (and
 * argued with) without wading through JSX.
 *
 * ## Why it has to be derived at all
 *
 * An R multiple is `net ÷ risk`, where risk is the money the position had on the line:
 * `|entry_px − sl_px| × size`. `sl_px` lives in kestreld's `positions` table, but
 * `/api/positions` serves `status='open'` rows only — the moment a position closes, the
 * one number the R multiple needs stops being on the wire. Nothing else projects it.
 *
 * So the stop distance is *recovered*, per close, from what the wire does carry:
 *
 * 1. **Stopped out** (`sl` fill) — the stop fill *is* the stop:
 *    `risk = |entry_px − exit_px| × size`. Exact to the paper book's slippage
 *    (`Store::resolve_close_fill` fills at mark/VWAP, not at the trigger).
 * 2. **Took profit** (`tp` fill) — kestreld pins `tp_pct = 2.0 × stop_pct`
 *    (`sizing.rs`), so the target sits exactly 2R out and
 *    `risk = |entry_px − exit_px| × size ÷ 2`.
 * 3. **Vetoed / time-stopped** — the exit price is wherever the reviewer got out and
 *    carries no bracket information. These closes only resolve when
 *    `/api/analytics` has *replayed* their bracket: a counterfactual row with
 *    `bracket_outcome: "sl"` pays `−risk − exit_fee`, one with `"tp"` pays
 *    `2·risk − exit_fee`, so `risk ≈ |bracket_pnl|` and `bracket_pnl ÷ 2`
 *    respectively — overstated by that one exit fee (≈7.5bp of notional, under 1% of R).
 *    `"expiry"` resolves nothing and stays unplotted.
 * 4. Anything left is reported as unresolved rather than guessed.
 *
 * `net` is the position's whole-life net: every exit's realized pnl minus **every** fee
 * including the entry's. A clean stop-out therefore lands a little below −1R — that gap
 * is the cost of trading, and it belongs on the chart.
 */

/** Ledger close reasons. Everything that is not tp/sl/veto_close (time stops, manual) is `other`. */
export type ExitKind = "tp" | "sl" | "veto_close" | "other"

export type Close = {
  positionId: number
  market: string
  kind: ExitKind
  /** ms epoch of the final exit fill */
  closedTs: number
  /** whole-life net: every exit's realized pnl minus every fee, entry fee included */
  net: number
  /** `|entry − sl| × size` in USD — null when no source recovers the stop distance */
  risk: number | null
  /** `net ÷ risk` — null exactly when `risk` is */
  r: number | null
}

export type CloseSet = {
  /** every close paired with its opening fill, oldest first */
  closes: Close[]
  /** closes whose opening fill sits outside the fetched trade window, so nothing pairs */
  unpaired: number
}

const KIND_OF: Record<string, ExitKind> = {
  tp: "tp",
  sl: "sl",
  veto_close: "veto_close",
}

export function exitKind(action: string): ExitKind {
  return KIND_OF[action] ?? "other"
}

export const KIND_LABEL: Record<ExitKind, string> = {
  tp: "Take profit",
  sl: "Stop loss",
  veto_close: "Veto close",
  other: "Other exit",
}

/** A risk of zero or a non-finite one is not a denominator — it is missing data. */
function usable(value: number | null): number | null {
  return value != null && Number.isFinite(value) && value > 0 ? value : null
}

/** Stop distance read straight off the bracket fill that closed the position. */
function riskFromFills(
  entryPx: number,
  size: number,
  exits: Trade[]
): number | null {
  const stop = exits.find((t) => t.action === "sl")
  if (stop) return Math.abs(entryPx - stop.px) * size
  const target = exits.find((t) => t.action === "tp")
  // tp_pct = 2 × stop_pct (kestreld sizing.rs): the target is exactly two stops away.
  if (target) return (Math.abs(entryPx - target.px) * size) / 2
  return null
}

/** Stop distance implied by a replayed bracket, for the closes no fill can resolve. */
function riskFromBracket(row: CounterfactualRow | undefined): number | null {
  if (!row) return null
  if (row.bracket_outcome === "sl") return Math.abs(row.bracket_pnl)
  if (row.bracket_outcome === "tp") return row.bracket_pnl / 2
  return null
}

/**
 * Pair every close in `trades` with its opening fill and attach an R multiple where the
 * stop distance is recoverable. `cfRows` fills the gap for vetoed closes.
 */
export function deriveCloses(
  trades: Trade[],
  cfRows: CounterfactualRow[]
): CloseSet {
  const byPosition = new Map<number, Trade[]>()
  for (const t of trades) {
    const rows = byPosition.get(t.position_id)
    if (rows) rows.push(t)
    else byPosition.set(t.position_id, [t])
  }
  const bracketOf = new Map<number, CounterfactualRow>()
  for (const row of cfRows) bracketOf.set(row.position_id, row)

  const closes: Close[] = []
  let unpaired = 0

  for (const [positionId, rows] of byPosition) {
    const exits = rows
      .filter((t) => t.action !== "open")
      .sort((a, b) => a.ts - b.ts)
    if (exits.length === 0) continue // still open
    const open = rows.find((t) => t.action === "open")
    if (!open) {
      unpaired += 1
      continue
    }

    const last = exits[exits.length - 1]
    const net =
      exits.reduce((sum, t) => sum + (t.realized_pnl ?? 0), 0) -
      rows.reduce((sum, t) => sum + t.fee, 0)
    const risk = usable(
      riskFromFills(open.px, open.size, exits) ??
        riskFromBracket(bracketOf.get(positionId))
    )

    closes.push({
      positionId,
      market: last.market,
      kind: exitKind(last.action),
      closedTs: last.ts,
      net,
      risk,
      r: risk == null ? null : net / risk,
    })
  }

  closes.sort((a, b) => a.closedTs - b.closedTs)
  return { closes, unpaired }
}

/** `+1.85R` · `-0.40R` — signed, two decimals, always suffixed. */
export function formatR(value: number): string {
  return `${value > 0 ? "+" : ""}${value.toFixed(2)}R`
}
