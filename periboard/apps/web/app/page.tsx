"use client"

import Link from "next/link"
import { Area, ReferenceLine, XAxis, YAxis } from "recharts"

import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@workspace/ui/components/card"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@workspace/ui/components/table"
import { Badge } from "@workspace/ui/components/badge"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  StatusBlankContainer,
  StatusBlankDescription,
  StatusBlankTitle,
} from "@workspace/ui/components/blocks/status-blank"
import { EvilAreaChart } from "@/components/evilcharts/charts/recharts-area-chart"
import { LiveOrders } from "@/components/live-orders"
import { api, type Close, type LiveClose } from "@/lib/api"
import { ago, dayStamp, px, signedUsd, usd } from "@/lib/format"
import { useLiveDashboard } from "@/lib/use-live-dashboard"
import { usePoll } from "@/lib/use-poll"
import { cn } from "@workspace/ui/lib/utils"

function cumulative(closes: ReadonlyArray<Close | LiveClose>) {
  const asc = [...closes].sort((a, b) => a.closed_ts - b.closed_ts)
  let run = 0
  return asc.map((c) => {
    run += c.realized_pnl
    return { t: dayStamp(c.closed_ts), pnl: Number(run.toFixed(2)) }
  })
}

function Kpi({
  label,
  children,
  sub,
}: {
  label: string
  children: React.ReactNode
  sub?: React.ReactNode
}) {
  return (
    <Card className="gap-2 py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-muted-foreground text-[11px] font-medium tracking-wider uppercase">
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-1.5 px-4">
        <div className="font-mono text-2xl leading-none font-semibold tabular-nums">
          {children}
        </div>
        {sub && <div className="text-muted-foreground text-xs">{sub}</div>}
      </CardContent>
    </Card>
  )
}

export default function Overview() {
  const { data: status } = usePoll(api.status)
  const { data: positions } = usePoll(api.positions)
  const { data: closes } = usePoll(() => api.closes(200))
  const { data: daily } = usePoll(() => api.daily(30))
  const { data: decisions } = usePoll(() => api.decisions(1))
  const live = useLiveDashboard()

  const today = daily?.[0]
  const latest = decisions?.[0]
  const openPositions = live.data?.positions ?? positions
  const resting = status?.resting_entries ?? []
  const realizedCloses = live.data?.closes ?? closes
  const series = realizedCloses ? cumulative(realizedCloses) : []
  const last = series.at(-1)?.pnl ?? 0
  const wins = realizedCloses?.filter((c) => c.realized_pnl > 0).length ?? 0
  const total = realizedCloses?.length ?? 0
  const realizedTotal = live.data?.realized.total ?? status?.realized_total
  const realizedScope = live.data?.realized.scope ?? status?.realized_scope ?? "Peri ledger"

  return (
    <div className="space-y-4">
      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <Kpi
          label={`Realized PnL · ${status?.mode ?? "…"}`}
          sub={total
            ? `${total} close fill${total === 1 ? "" : "s"} · ${wins}/${total} profitable · ${realizedScope}`
            : `no closes in ${realizedScope}`}
        >
          <span
            className={cn(
              (realizedTotal ?? 0) >= 0
                ? "text-emerald-500"
                : "text-red-500"
            )}
          >
            {realizedTotal == null ? "…" : signedUsd(realizedTotal)}
          </span>
        </Kpi>
        <Kpi
          label="Open positions"
          sub={openPositions?.length ? openPositions.map((p) => p.market).join(" · ") : "flat"}
        >
          {openPositions?.length ?? "…"}
        </Kpi>
        <Kpi
          label={`Today · ${today?.day ?? "…"}`}
          sub={
            <span className="flex items-center gap-2">
              <span>{today?.entries ?? 0} entries</span>
              <span
                className={cn(
                  "font-medium",
                  today?.kill_tripped ? "text-red-500" : "text-emerald-500"
                )}
              >
                {today?.kill_tripped ? "KILL TRIPPED" : "kill armed"}
              </span>
            </span>
          }
        >
          {today ? usd(today.open_equity, 0) : "…"}
          <span className="text-muted-foreground ml-1.5 align-middle text-xs font-normal">
            open eq
          </span>
        </Kpi>
        <Kpi
          label="Analyst"
          sub={`cycle ${status ? status.cycle_secs / 60 : "…"}m · decided ${
            status?.last_decision_ts ? ago(status.last_decision_ts) : "—"
          }`}
        >
          <span className="text-base font-medium">{status?.model ?? "…"}</span>
        </Kpi>
      </div>

      <div className="grid items-stretch gap-4 lg:grid-cols-5">
        <Card className="lg:col-span-3">
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle className="text-sm font-medium">
              Cumulative realized PnL
            </CardTitle>
            <span
              className={cn(
                "font-mono text-sm font-semibold tabular-nums",
                last >= 0 ? "text-emerald-500" : "text-red-500"
              )}
            >
              {signedUsd(last)}
            </span>
          </CardHeader>
          <CardContent>
            {series.length > 1 ? (
              <EvilAreaChart
                className="h-56 w-full"
                config={{
                  pnl: {
                    label: "PnL",
                    color: last >= 0 ? "var(--chart-2)" : "var(--destructive)",
                  },
                }}
                data={series}
              >
                <XAxis
                  dataKey="t"
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10 }}
                  interval="preserveStartEnd"
                  minTickGap={48}
                />
                <YAxis
                  width={44}
                  tickLine={false}
                  axisLine={false}
                  tick={{ fontSize: 10 }}
                  tickFormatter={(v: number) => `$${v}`}
                  domain={[
                    (min: number) => Math.min(0, min),
                    (max: number) => Math.max(0, max),
                  ]}
                />
                <ReferenceLine y={0} strokeDasharray="3 3" strokeOpacity={0.4} />
                <Area dataKey="pnl" />
              </EvilAreaChart>
            ) : (
              <StatusBlankContainer className="h-56">
                <StatusBlankTitle>Not enough closes yet</StatusBlankTitle>
                <StatusBlankDescription>
                  The curve draws itself as trades resolve.
                </StatusBlankDescription>
              </StatusBlankContainer>
            )}
          </CardContent>
        </Card>

        <Card className="lg:col-span-2">
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle className="text-sm font-medium">Latest view</CardTitle>
            {latest && (
              <Link
                className="text-muted-foreground hover:text-foreground text-xs underline-offset-4 transition-colors hover:underline"
                href={`/decisions/${latest.id}`}
              >
                full trace →
              </Link>
            )}
          </CardHeader>
          <CardContent>
            {latest ? (
              <div className="flex h-full flex-col justify-between gap-3">
                <p className="text-sm leading-relaxed">{latest.market_view}</p>
                <div className="flex flex-wrap gap-1.5">
                  <Badge variant="secondary">{latest.trigger}</Badge>
                  <Badge variant="outline" className="font-mono tabular-nums">
                    {((latest.latency_ms ?? 0) / 1000).toFixed(1)}s
                  </Badge>
                  <Badge
                    variant={latest.actions.length ? "default" : "outline"}
                    className="font-mono tabular-nums"
                  >
                    {latest.actions.length
                      ? `${latest.actions.length} action${latest.actions.length === 1 ? "" : "s"}`
                      : "hold"}
                  </Badge>
                  <Badge variant="outline" className="font-mono tabular-nums">
                    {latest.tool_log.length} search
                    {latest.tool_log.length === 1 ? "" : "es"}
                  </Badge>
                </div>
              </div>
            ) : (
              <Spinner />
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm font-medium">Open positions</CardTitle>
        </CardHeader>
        <CardContent>
          {openPositions && openPositions.length > 0 ? (
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead>Market</TableHead>
                  <TableHead>Side</TableHead>
                  <TableHead className="text-right">Entry</TableHead>
                  <TableHead className="text-right">Size</TableHead>
                  <TableHead className="text-right">Lev</TableHead>
                  <TableHead className="text-right">Stop</TableHead>
                  <TableHead className="text-right">TP</TableHead>
                  <TableHead className="text-right">Conv</TableHead>
                  <TableHead>Age</TableHead>
                  <TableHead className="w-full">Thesis</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {openPositions.map((p) => (
                  <TableRow key={p.id ?? p.market}>
                    <TableCell className="font-mono font-medium">
                      {p.market}
                    </TableCell>
                    <TableCell>
                      <span
                        className={cn(
                          "font-medium uppercase",
                          p.side === "long" ? "text-emerald-500" : "text-red-500"
                        )}
                      >
                        {p.side}
                      </span>
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {px(p.entry_px)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {p.size}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {p.leverage}x
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums text-red-500/80">
                      {px(p.stop_px)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums text-emerald-500/80">
                      {px(p.tp_px)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {p.conviction ?? "—"}
                    </TableCell>
                    <TableCell className="text-muted-foreground text-xs whitespace-nowrap">
                      {ago(p.opened_ts)}
                    </TableCell>
                    <TableCell
                      className="text-muted-foreground max-w-0 truncate text-xs"
                      title={p.rationale ?? undefined}
                    >
                      {p.rationale ?? "—"}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : (
            <StatusBlankContainer className="py-8">
              <StatusBlankTitle>
                {resting.length > 0 ? "Flat — but working" : "Flat"}
              </StatusBlankTitle>
              <StatusBlankDescription>
                {resting.length > 0 ? (
                  <>
                    No position yet.{" "}
                    {resting.length === 1 ? "A maker entry is" : `${resting.length} maker entries are`}{" "}
                    resting with brackets attached:{" "}
                    {resting
                      .map(
                        (e) =>
                          `${e.side} ${e.market} @ ${e.entry_px} (stop ${e.stop_px} / tp ${e.tp_px})`
                      )
                      .join(", ")}
                    . It fills at that level or expires costing nothing.
                  </>
                ) : (
                  "No open positions right now."
                )}
              </StatusBlankDescription>
            </StatusBlankContainer>
          )}
        </CardContent>
      </Card>

      <LiveOrders
        connected={live.connected}
        error={live.error}
        snapshot={live.data}
        source={live.source}
      />
    </div>
  )
}
