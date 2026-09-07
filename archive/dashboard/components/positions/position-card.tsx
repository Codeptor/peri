"use client"

import type { Position } from "@/lib/api"
import { fmtPrice, signedPct, usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { StatusPill } from "@/components/blocks/status-pill"
import { AnalystChip } from "@/components/analyst/analyst-chip"
import { Meter } from "@/components/positions/meter"
import { StatTile } from "@/components/positions/stat-tile"
import {
  distancePct,
  heldFor,
  pnlTone,
  SL_COLOR,
  slProgress,
  TP_COLOR,
  tpProgress,
} from "@/components/positions/util"

const TILE = "rounded-[8px] px-3 py-2.5"

export type PositionCardProps = {
  position: Position
  selected: boolean
  onSelect: (id: number) => void
  now: number | null
}

/**
 * One open position: side accent edge, entry/mark/ROE tiles, SL/TP meters.
 * Nested inside a `card` section, so it takes the ground each theme uses for a
 * sub-panel — white + hairline in light, the recessed page tone in dark — which
 * keeps its `surface-2` tiles and meter tracks a visible step away in both.
 */
export function PositionCard({
  position,
  selected,
  onSelect,
  now,
}: PositionCardProps) {
  const long = position.side === "long"

  return (
    <button
      aria-pressed={selected}
      className={cn(
        "relative w-full overflow-hidden rounded-md border bg-card py-4 pr-4 pl-5 text-left shadow-[var(--shadow-card)] transition-colors dark:bg-background",
        selected
          ? "border-primary/70 ring-1 ring-primary/25"
          : "border-border hover:border-primary/40"
      )}
      onClick={() => onSelect(position.id)}
      type="button"
    >
      <span
        className={cn(
          "absolute inset-y-0 left-0 w-[3px]",
          long ? "bg-long" : "bg-short"
        )}
      />

      <div className="flex items-center gap-2">
        <span className="truncate font-mono text-[14px] font-semibold tracking-tight">
          {position.market}
        </span>
        <StatusPill tone={long ? "long" : "short"}>
          {long ? "Long" : "Short"}
        </StatusPill>
        <AnalystChip analyst={position.analyst} />
        <span className="ml-auto shrink-0 rounded-full bg-surface-2 px-2 py-0.5 font-mono text-[11px] font-medium text-muted-foreground">
          {`${position.leverage}×`}
        </span>
      </div>

      <div className="mt-3 flex items-baseline gap-2">
        <span
          className={cn(
            "font-mono text-[18px] leading-6 font-semibold tracking-tight",
            pnlTone(position.unrealized_pnl)
          )}
        >
          {usd(position.unrealized_pnl)}
        </span>
        <span className="ml-auto shrink-0 font-mono text-[11px] text-muted-foreground">
          {now == null ? "—" : heldFor(position.opened_ts, now)}
        </span>
      </div>

      <div className="mt-3.5 grid grid-cols-3 gap-2">
        <StatTile
          className={TILE}
          label="Entry"
          value={fmtPrice(position.entry_px)}
        />
        <StatTile
          className={TILE}
          label="Mark"
          value={fmtPrice(position.mark_px)}
        />
        <StatTile
          className={TILE}
          label="ROE"
          value={signedPct(position.roe)}
          valueClassName={pnlTone(position.roe)}
        />
      </div>

      <div className="mt-4 flex flex-col gap-3">
        <Meter
          color={SL_COLOR}
          label="Stop loss"
          level={fmtPrice(position.sl_px)}
          progress={slProgress(position)}
          readout={signedPct(distancePct(position.mark_px, position.sl_px))}
        />
        <Meter
          color={TP_COLOR}
          label="Take profit"
          level={fmtPrice(position.tp_px)}
          progress={tpProgress(position)}
          readout={signedPct(distancePct(position.mark_px, position.tp_px))}
        />
      </div>
    </button>
  )
}
