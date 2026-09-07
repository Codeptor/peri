"use client"

import NumberFlow from "@number-flow/react"
import { ChartCandlestickIcon } from "@hugeicons/core-free-icons"

import type { Position } from "@/lib/api"
import type { CandleInterval } from "@/lib/candles"
import { fmtPrice, signedPct, usd } from "@/lib/format"
import { cn } from "@/lib/utils"
import { DeltaChip } from "@/components/blocks/delta-chip"
import { SectionCard } from "@/components/blocks/section-card"
import { Segmented, type SegmentedOption } from "@/components/blocks/segmented"
import { StatusPill } from "@/components/blocks/status-pill"
import { AnalystChip } from "@/components/analyst/analyst-chip"
import type { TrendPoint } from "@/components/blocks/trend-area"
import { CandlePanel } from "@/components/candle-panel"
import { Meter } from "@/components/positions/meter"
import { PnlSpark } from "@/components/positions/pnl-spark"
import { StatTile } from "@/components/positions/stat-tile"
import { useNow } from "@/components/positions/use-book"
import {
  distancePct,
  heldFor,
  pnlTone,
  SL_COLOR,
  slProgress,
  TP_COLOR,
  tpProgress,
} from "@/components/positions/util"
import { VolumeStrip } from "@/components/positions/volume-strip"

const MONEY = {
  style: "currency",
  currency: "USD",
  signDisplay: "exceptZero",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
} as const

const INTERVALS: SegmentedOption<CandleInterval>[] = [
  { value: "1m", label: "1m" },
  { value: "5m", label: "5m" },
  { value: "15m", label: "15m" },
]

export type PositionHeroProps = {
  position: Position
  interval: CandleInterval
  onIntervalChange: (interval: CandleInterval) => void
  /** uPnL ring buffer for this position, from `usePnlSparks` */
  spark: TrendPoint[]
}

/** The selected position, full width: side/leverage/uPnL/ROE header + candles. */
export function PositionHero({
  position,
  interval,
  onIntervalChange,
  spark,
}: PositionHeroProps) {
  const now = useNow()
  const long = position.side === "long"

  return (
    <SectionCard
      action={
        <Segmented
          label="Candle interval"
          onChange={onIntervalChange}
          options={INTERVALS}
          size="sm"
          value={interval}
        />
      }
      icon={ChartCandlestickIcon}
      title="Selected position"
    >
      <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-3">
        <div className="flex min-w-0 flex-wrap items-center gap-2.5">
          <span className="font-mono text-[17px] font-semibold tracking-tight">
            {position.market}
          </span>
          <StatusPill tone={long ? "long" : "short"}>
            {long ? "Long" : "Short"}
          </StatusPill>
          <AnalystChip analyst={position.analyst} />
          <span className="rounded-full bg-surface-2 px-2.5 py-0.5 text-[11.5px] leading-5 text-muted-foreground">
            <span className="font-mono font-medium text-foreground">
              {`${position.leverage}×`}
            </span>{" "}
            leverage
          </span>
          <span className="text-[11.5px] text-muted-foreground">
            {now == null ? (
              "—"
            ) : (
              <>
                held{" "}
                <span className="font-mono">
                  {heldFor(position.opened_ts, now)}
                </span>
              </>
            )}
          </span>
        </div>
        <div className="flex flex-wrap items-end justify-end gap-x-5 gap-y-3">
          <PnlSpark points={spark} />
          <div className="flex items-baseline gap-2.5">
            <span
              className={cn(
                "font-mono text-[26px] leading-8 font-semibold tracking-tight",
                pnlTone(position.unrealized_pnl)
              )}
            >
              <NumberFlow format={MONEY} value={position.unrealized_pnl} />
            </span>
            <DeltaChip pct={position.roe} />
          </div>
        </div>
      </div>

      <div className="mt-5 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <StatTile label="Entry" value={fmtPrice(position.entry_px)} />
        <StatTile label="Mark" value={fmtPrice(position.mark_px)} />
        <StatTile
          label="Size"
          sub={
            <span className="font-mono">
              {usd(position.size * position.mark_px)}
            </span>
          }
          value={position.size}
        />
        <StatTile label="Margin" value={usd(position.margin)} />
      </div>

      {/* No `overflow-hidden`: the candle canvas is already inset from the
          rounded border, and clipping here would cut the volume tooltip off at
          the strip's own 46px box. */}
      <div className="mt-5 rounded-xl border border-border bg-surface-2 p-2.5">
        <CandlePanel
          coin={position.market}
          height={320}
          interval={interval}
          lines={{
            entry: position.entry_px,
            sl: position.sl_px,
            tp: position.tp_px,
          }}
        />
        <VolumeStrip
          className="mt-2 border-t border-border pt-2"
          coin={position.market}
          interval={interval}
        />
      </div>

      <div className="mt-5 grid gap-4 sm:grid-cols-2">
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
    </SectionCard>
  )
}
