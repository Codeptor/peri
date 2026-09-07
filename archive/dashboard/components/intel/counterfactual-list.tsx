"use client"

import * as React from "react"

import type {
  CounterfactualRow,
  CounterfactualSummary,
  ExitMixDay,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import { DotMatrix } from "@/components/blocks/dot-matrix"
import type { Hue } from "@/components/blocks/icon-chip"
import { StatusPill } from "@/components/blocks/status-pill"
import {
  pnlInk,
  signedUsd,
  sparseLabels,
  utcStamp,
} from "@/components/intel/intel-utils"

const ROWS = 8
const CELL = 7
const GAP = 3

type Series = {
  key: "tp" | "sl" | "veto_close" | "other"
  label: string
  color: string
}

/** Same four exits, same four colours as the stacked bar above it. */
const SERIES: Series[] = [
  { key: "tp", label: "Take profit", color: "var(--long)" },
  { key: "sl", label: "Stop loss", color: "var(--short)" },
  { key: "veto_close", label: "Veto close", color: "var(--warning)" },
  { key: "other", label: "Other", color: "var(--chart-6)" },
]

/** What the bracket would have done, in the same hue language as the exit mix. */
const OUTCOME: Record<string, { label: string; tone: Hue }> = {
  tp: { label: "Take profit", tone: "long" },
  sl: { label: "Stop loss", tone: "short" },
  expiry: { label: "Expired", tone: "neutral" },
}

function Stat({
  label,
  value,
  tone,
}: {
  label: string
  value: string
  tone?: string
}) {
  return (
    <div className="min-w-0">
      <div className="text-[11.5px] leading-tight text-muted-foreground">
        {label}
      </div>
      <div
        className={cn(
          "mt-1 truncate font-mono text-[13px] font-medium tabular-nums",
          tone
        )}
      >
        {value}
      </div>
    </div>
  )
}

/**
 * One replayed veto. The headline is the delta, because that single signed number is the
 * verdict on the decision; the two pnls it came from sit underneath where they can be
 * checked but never have to be subtracted by eye.
 */
function VetoRow({ row }: { row: CounterfactualRow }) {
  const delta = row.actual_pnl - row.bracket_pnl
  const outcome = OUTCOME[row.bracket_outcome] ?? {
    label: row.bracket_outcome,
    tone: "neutral" as Hue,
  }

  return (
    <li className="border-t border-border py-2.5 first:border-t-0 first:pt-0">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-mono text-[13px] font-medium">{row.market}</span>
        <StatusPill tone={row.side === "long" ? "long" : "short"}>
          {row.side === "long" ? "Long" : "Short"}
        </StatusPill>
        <StatusPill tone={outcome.tone}>{outcome.label}</StatusPill>
        <span
          className={cn(
            "ml-auto font-mono text-[13px] font-semibold tabular-nums",
            pnlInk(delta)
          )}
        >
          {signedUsd(delta)}
        </span>
      </div>
      <div className="mt-1 flex flex-wrap items-baseline gap-x-1.5 text-[11.5px] leading-4 text-muted-foreground">
        <span className="font-mono">{utcStamp(row.closed_ts)}</span>
        <span>· actual</span>
        <span className={cn("font-mono tabular-nums", pnlInk(row.actual_pnl))}>
          {signedUsd(row.actual_pnl)}
        </span>
        <span>vs bracket</span>
        <span className={cn("font-mono tabular-nums", pnlInk(row.bracket_pnl))}>
          {signedUsd(row.bracket_pnl)}
        </span>
      </div>
    </li>
  )
}

function SeriesMatrix({
  series,
  days,
  labels,
  max,
}: {
  series: Series
  days: ExitMixDay[]
  labels: string[]
  max: number
}) {
  const total = days.reduce((sum, d) => sum + d[series.key], 0)
  const columns = days.map((d) => ({
    value: d[series.key],
    color: series.color,
    label: `${d.date} · ${d[series.key]} ${series.label.toLowerCase()}`,
  }))

  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        <span
          className="size-1.5 shrink-0 rounded-full"
          style={{ backgroundColor: series.color }}
        />
        <span className="truncate text-[11.5px] text-muted-foreground">
          {series.label}
        </span>
        <span className="ml-auto font-mono text-[13px] font-medium">
          {total}
        </span>
      </div>
      <DotMatrix
        cellSize={CELL}
        className="mt-2"
        columns={columns}
        gap={GAP}
        max={max}
        rows={ROWS}
        xLabels={labels}
      />
    </div>
  )
}

export type CounterfactualDetailProps = {
  counterfactuals: CounterfactualSummary
  exitMix: ExitMixDay[]
}

/**
 * The expanded veto counterfactual: the totals broken into their parts, then the
 * individual replays behind them, then when the vetoes happened.
 *
 * `counterfactuals.rows` carries the newest `CF_ROWS_MAX` (50) replays while the totals
 * stay whole-history, so past fifty vetoes the list is a window onto the summary rather
 * than its decomposition — said plainly under the list rather than left to be discovered.
 */
export function CounterfactualDetail({
  counterfactuals,
  exitMix,
}: CounterfactualDetailProps) {
  const edge = counterfactuals.net_actual - counterfactuals.net_bracket
  const perClose =
    counterfactuals.computed > 0 ? edge / counterfactuals.computed : null
  const resolution =
    counterfactuals.computed + counterfactuals.pending > 0
      ? (counterfactuals.computed /
          (counterfactuals.computed + counterfactuals.pending)) *
        100
      : null

  // Optional at runtime only: a fixture captured before the field existed still renders.
  const rows = counterfactuals.rows ?? []
  const windowed = rows.length < counterfactuals.computed

  const labels = React.useMemo(
    () => sparseLabels(exitMix.map((d) => d.date)),
    [exitMix]
  )
  const max = React.useMemo(
    () =>
      Math.max(
        1,
        exitMix.reduce(
          (hi, d) => Math.max(hi, d.tp, d.sl, d.veto_close, d.other),
          0
        )
      ),
    [exitMix]
  )

  return (
    <div className="mt-3 rounded-md border border-border bg-surface-2 p-4">
      <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
        <Stat label="Computed" value={String(counterfactuals.computed)} />
        <Stat label="Pending" value={String(counterfactuals.pending)} />
        <Stat
          label="Resolved"
          value={resolution == null ? "—" : `${resolution.toFixed(0)}%`}
        />
        <Stat
          label="Net actual"
          tone={pnlInk(counterfactuals.net_actual)}
          value={signedUsd(counterfactuals.net_actual)}
        />
        <Stat
          label="Net bracket"
          tone={pnlInk(counterfactuals.net_bracket)}
          value={signedUsd(counterfactuals.net_bracket)}
        />
        <Stat label="Edge" tone={pnlInk(edge)} value={signedUsd(edge)} />
      </div>

      <p className="mt-3 border-t border-border pt-3 text-[11.5px] leading-4 text-muted-foreground">
        {perClose == null ? (
          "No veto close has been replayed yet, so there is no edge to attribute."
        ) : (
          <>
            <span className={cn("font-mono tabular-nums", pnlInk(perClose))}>
              {signedUsd(perClose)}
            </span>
            {
              " per replayed close. A candle that touches both levels is scored as"
            }
            {" the stop, so the bracket is never flattered."}
          </>
        )}
      </p>

      {rows.length > 0 ? (
        <div className="mt-3.5 border-t border-border pt-3.5">
          <div className="text-[11.5px] text-muted-foreground">
            Per position · newest first ·{" "}
            <span className="font-mono">{rows.length}</span> replay
            {rows.length === 1 ? "" : "s"}
          </div>
          <ul className="mt-2 max-h-64 overflow-y-auto pr-1">
            {rows.map((row) => (
              <VetoRow key={row.position_id} row={row} />
            ))}
          </ul>
          <p className="mt-2 text-[11.5px] leading-4 text-muted-foreground">
            Delta is actual minus bracket — positive means the early close beat
            the position&apos;s own levels.
            {windowed ? (
              <>
                {" The list holds the newest "}
                <span className="font-mono">{rows.length}</span>
                {" of "}
                <span className="font-mono">{counterfactuals.computed}</span>
                {" replays, so it does not add up to the totals above."}
              </>
            ) : null}
          </p>
        </div>
      ) : null}

      <div className="mt-3.5 border-t border-border pt-3.5">
        <div className="text-[11.5px] text-muted-foreground">
          Exit mix by UTC day ·{" "}
          <span className="font-mono">{exitMix.length}</span> days · peak{" "}
          <span className="font-mono">{max}</span> in a day
        </div>
        {exitMix.length === 0 ? (
          <p className="mt-3 text-[13px] text-muted-foreground">
            No closes in the window.
          </p>
        ) : (
          <div className="mt-3 grid grid-cols-2 gap-x-5 gap-y-4">
            {SERIES.map((series) => (
              <SeriesMatrix
                days={exitMix}
                key={series.key}
                labels={labels}
                max={max}
                series={series}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}
