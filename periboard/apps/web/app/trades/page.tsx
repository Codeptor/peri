"use client"

import { Bar, Cell, ReferenceLine, XAxis, YAxis } from "recharts"

import { Badge } from "@workspace/ui/components/badge"
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
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@workspace/ui/components/tabs"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  StatusBlankContainer,
  StatusBlankDescription,
  StatusBlankTitle,
} from "@workspace/ui/components/blocks/status-blank"
import { EvilBarChart } from "@/components/evilcharts/charts/recharts-bar-chart"
import { api, type Close, type LiveClose } from "@/lib/api"
import { dayStamp, px, signedUsd } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"
import { useLiveDashboard } from "@/lib/use-live-dashboard"

const REASON_COLORS: Record<string, string> = {
  tp: "text-emerald-500",
  sl: "text-red-500",
  analyst: "text-blue-400",
  external: "text-amber-500",
  venue: "text-violet-400",
}

function closeKey(close: Close | LiveClose) {
  return "id" in close && typeof close.id === "string"
    ? close.id
    : `${close.market}-${close.closed_ts}-${close.close_px}`
}

export default function Trades() {
  const { data: ledgerCloses } = usePoll(() => api.closes(200))
  const { data: refusals } = usePoll(() => api.refusals(100))
  const live = useLiveDashboard()
  const closes = live.data?.closes ?? ledgerCloses

  const bars = closes
    ? [...closes]
        .sort((a, b) => a.closed_ts - b.closed_ts)
        .map((c, i) => ({
          key: closeKey(c),
          label: `${c.market} #${i + 1}`,
          pnl: Number((c.realized_pnl ?? 0).toFixed(2)),
        }))
    : []

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle className="text-sm font-medium">
            {live.data ? "Per-close-fill realized PnL · venue" : "Per-trade realized PnL · ledger"}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {bars.length > 0 ? (
            <EvilBarChart
              className="h-52 w-full"
              config={{ pnl: { label: "PnL", color: "var(--chart-2)" } }}
              data={bars}
            >
              <XAxis dataKey="label" hide />
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
              <Bar dataKey="pnl">
                {bars.map((b) => (
                  <Cell
                    key={b.key}
                    fill={b.pnl >= 0 ? "var(--chart-2)" : "var(--destructive)"}
                    fillOpacity={0.85}
                  />
                ))}
              </Bar>
            </EvilBarChart>
          ) : (
            <StatusBlankContainer className="h-52">
              <StatusBlankTitle>No close fills in current history</StatusBlankTitle>
              <StatusBlankDescription>
                Bars appear as brackets fire.
              </StatusBlankDescription>
            </StatusBlankContainer>
          )}
        </CardContent>
      </Card>

      <Tabs defaultValue="closes">
        <TabsList>
          <TabsTrigger value="closes">
            Closes {closes ? `(${closes.length})` : ""}
          </TabsTrigger>
          <TabsTrigger value="refusals">
            Guard refusals {refusals ? `(${refusals.length})` : ""}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="closes">
          <Card>
            <CardContent className="pt-4">
              {closes ? (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Closed</TableHead>
                      <TableHead>Market</TableHead>
                      <TableHead>Side</TableHead>
                      <TableHead className="text-right">Entry</TableHead>
                      <TableHead className="text-right">Exit</TableHead>
                      <TableHead className="text-right">PnL</TableHead>
                      <TableHead>Reason</TableHead>
                      <TableHead className="text-right">Conv</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {closes.map((c) => (
                      <TableRow key={closeKey(c)}>
                        <TableCell className="whitespace-nowrap font-mono text-xs">
                          {dayStamp(c.closed_ts)}
                        </TableCell>
                        <TableCell className="font-mono">{c.market}</TableCell>
                        <TableCell>{c.side}</TableCell>
                        <TableCell className="text-right font-mono tabular-nums">
                          {px(c.entry_px)}
                        </TableCell>
                        <TableCell className="text-right font-mono tabular-nums">
                          {px(c.close_px)}
                        </TableCell>
                        <TableCell
                          className={`text-right font-mono tabular-nums ${
                            c.realized_pnl >= 0
                              ? "text-emerald-500"
                              : "text-red-500"
                          }`}
                        >
                          {signedUsd(c.realized_pnl)}
                        </TableCell>
                        <TableCell>
                          <span
                            className={`text-xs ${REASON_COLORS[c.close_reason] ?? ""}`}
                          >
                            {c.close_reason}
                          </span>
                        </TableCell>
                        <TableCell className="text-right font-mono tabular-nums">
                          {c.conviction ?? "—"}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              ) : (
                <Spinner />
              )}
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="refusals">
          <Card>
            <CardContent className="pt-4">
              {refusals && refusals.length > 0 ? (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>When</TableHead>
                      <TableHead>Market</TableHead>
                      <TableHead>Reason</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {refusals.map((r) => (
                      <TableRow key={r.id}>
                        <TableCell className="whitespace-nowrap font-mono text-xs">
                          {dayStamp(r.ts)}
                        </TableCell>
                        <TableCell className="font-mono">
                          {r.market ?? "—"}
                        </TableCell>
                        <TableCell className="text-sm">
                          <Badge variant="outline">{r.reason}</Badge>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              ) : (
                <StatusBlankContainer className="py-8">
                  <StatusBlankTitle>No refusals</StatusBlankTitle>
                  <StatusBlankDescription>
                    Every gate has let recent actions through.
                  </StatusBlankDescription>
                </StatusBlankContainer>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  )
}
