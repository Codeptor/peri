"use client"

import * as React from "react"
import {
  Activity03Icon,
  CoinsDollarIcon,
  ReceiptDollarIcon,
  Target02Icon,
  Wallet01Icon,
} from "@hugeicons/core-free-icons"
import type { Format } from "@number-flow/react"

import type { EquityPoint, Health, Position, Trade } from "@/lib/api"
import { BANKROLL } from "@/lib/risk"
import { isClose, todayStats, winRate } from "@/lib/stats"
import { KpiCard } from "@/components/blocks/kpi-card"

const USD: Format = {
  style: "currency",
  currency: "USD",
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
}

const USD_SIGNED: Format = { ...USD, signDisplay: "exceptZero" }

const PCT: Format = {
  style: "unit",
  unit: "percent",
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
}

type Hue = "accent" | "long" | "short" | "neutral"

const NO_TRADES_TODAY = { realized: 0, fees: 0, closes: 0, entries: 0 }

function sideHue(n: number | null): Hue {
  if (n == null || n === 0) return "neutral"
  return n > 0 ? "long" : "short"
}

/** Equity · Unrealized · Realized today · Fees today · Win rate. */
export function KpiRow({
  equity,
  positions,
  trades,
  health,
  asOf,
  loading,
}: {
  equity: EquityPoint[]
  positions: Position[]
  trades: Trade[]
  health: Health | null
  asOf: number | null
  loading: boolean
}) {
  const equityNow = equity[equity.length - 1]?.equity ?? health?.equity ?? null

  const unrealized = React.useMemo(
    () => positions.reduce((sum, p) => sum + p.unrealized_pnl, 0),
    [positions]
  )

  const roe = React.useMemo(() => {
    const margin = positions.reduce((sum, p) => sum + p.margin, 0)
    return margin > 0 ? (unrealized / margin) * 100 : null
  }, [positions, unrealized])

  const today = React.useMemo(
    () => (asOf == null ? NO_TRADES_TODAY : todayStats(trades, asOf)),
    [trades, asOf]
  )

  const rate = React.useMemo(() => winRate(trades), [trades])

  const closes = React.useMemo(() => trades.filter(isClose), [trades])
  const wins = closes.filter((t) => (t.realized_pnl ?? 0) > 0).length

  const equityDelta =
    equityNow == null ? null : ((equityNow - BANKROLL) / BANKROLL) * 100

  const realizedPct = (today.realized / BANKROLL) * 100
  const fills = today.entries + today.closes

  return (
    <div className="grid gap-5 sm:grid-cols-2 xl:grid-cols-5">
      <KpiCard
        context="vs bankroll"
        deltaPct={equityDelta}
        format={USD}
        hue="accent"
        icon={Wallet01Icon}
        label="Equity"
        loading={loading}
        value={equityNow}
      />
      <KpiCard
        context="ROE"
        deltaPct={roe}
        format={USD_SIGNED}
        hue={sideHue(unrealized)}
        icon={Activity03Icon}
        label="Unrealized"
        loading={loading}
        value={unrealized}
      />
      <KpiCard
        context="of bankroll"
        deltaPct={realizedPct}
        format={USD_SIGNED}
        hue={sideHue(today.realized)}
        icon={CoinsDollarIcon}
        label="Realized today"
        loading={loading}
        value={today.realized}
      />
      {/* Fees are a cost, never a gain: a signed green/red chip would read
          backwards, so the chip stays neutral and carries the fill count. */}
      <KpiCard
        deltaLabel={fills === 1 ? "1 fill" : `${fills} fills`}
        format={USD}
        hue="neutral"
        icon={ReceiptDollarIcon}
        label="Fees today"
        loading={loading}
        value={today.fees}
      />
      <KpiCard
        context={closes.length === 0 ? undefined : "closes"}
        deltaLabel={
          closes.length === 0 ? "No closes" : `${wins}/${closes.length}`
        }
        format={PCT}
        hue="accent"
        icon={Target02Icon}
        label="Win rate"
        loading={loading}
        value={rate}
      />
    </div>
  )
}
