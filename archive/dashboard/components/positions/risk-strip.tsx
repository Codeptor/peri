"use client"

import NumberFlow from "@number-flow/react"
import { WeightScaleIcon } from "@hugeicons/core-free-icons"

import type { Position } from "@/lib/api"
import { signedPct, usd } from "@/lib/format"
import { BANKROLL, MARGIN_MAX, MAX_CONCURRENT } from "@/lib/risk"
import { RingStat } from "@/components/blocks/ring-stat"
import { SectionCard } from "@/components/blocks/section-card"
import { StatTile } from "@/components/positions/stat-tile"
import { bookTotals, pnlTone } from "@/components/positions/util"

const MONEY = {
  style: "currency",
  currency: "USD",
  signDisplay: "exceptZero",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
} as const

const MARGIN_CAP = MARGIN_MAX * MAX_CONCURRENT

export type RiskStripProps = {
  positions: Position[]
}

/** Aggregate exposure across the open book: margin, notional, stop risk, uPnL. */
export function RiskStrip({ positions }: RiskStripProps) {
  const totals = bookTotals(positions)
  const usedPct = Math.min(100, (totals.margin / MARGIN_CAP) * 100)
  const avgLeverage =
    positions.length > 0
      ? positions.reduce((s, p) => s + p.leverage, 0) / positions.length
      : 0

  return (
    <SectionCard icon={WeightScaleIcon} title="Book risk">
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <div className="flex items-center gap-4 rounded-[10px] bg-surface-2 px-3.5 py-3">
          <RingStat
            color={usedPct >= 80 ? "var(--warning)" : "var(--accent-orange)"}
            pct={usedPct}
            size={52}
          />
          <div className="min-w-0">
            <div className="truncate text-xs text-muted-foreground">
              Margin used
            </div>
            <div className="mt-1.5 truncate font-mono text-[15px] leading-5 font-semibold">
              {usd(totals.margin)}
            </div>
            <div className="mt-1 truncate text-[11.5px] text-muted-foreground">
              of <span className="font-mono">{usd(MARGIN_CAP)}</span> cap
            </div>
          </div>
        </div>

        <StatTile
          label="Notional exposure"
          sub={
            positions.length > 0 ? (
              <>
                <span className="font-mono">{positions.length}</span> open ·{" "}
                <span className="font-mono">{`${avgLeverage.toFixed(1)}×`}</span>{" "}
                avg leverage
              </>
            ) : (
              "flat"
            )
          }
          value={usd(totals.notional)}
        />

        <StatTile
          label="Open risk to SL"
          sub={
            <>
              <span className="font-mono">
                {`${((totals.riskToSl / BANKROLL) * 100).toFixed(2)}%`}
              </span>{" "}
              of bankroll
            </>
          }
          value={totals.riskToSl > 0 ? usd(-totals.riskToSl) : usd(0)}
          valueClassName={totals.riskToSl > 0 ? "text-short-ink" : undefined}
        />

        <StatTile
          label="Unrealized"
          sub={
            totals.roe == null ? (
              "—"
            ) : (
              <>
                <span className="font-mono">{signedPct(totals.roe)}</span> on
                margin
              </>
            )
          }
          value={<NumberFlow format={MONEY} value={totals.unrealized} />}
          valueClassName={pnlTone(totals.unrealized)}
        />
      </div>
    </SectionCard>
  )
}
