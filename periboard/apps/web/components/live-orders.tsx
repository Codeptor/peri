"use client"

import Link from "next/link"
import { useEffect, useState } from "react"

import { Badge } from "@workspace/ui/components/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@workspace/ui/components/card"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@workspace/ui/components/table"
import {
  StatusBlankContainer,
  StatusBlankDescription,
  StatusBlankTitle,
} from "@workspace/ui/components/blocks/status-blank"
import type { LiveOrder, LiveSnapshot } from "@/lib/api"
import { ago, px, stamp, usd } from "@/lib/format"
import { snapshotAgeMs } from "@/lib/live-stream"
import { cn } from "@workspace/ui/lib/utils"

const roleLabel: Record<LiveOrder["role"], string> = {
  stop_loss: "Stop loss",
  take_profit: "Take profit",
  entry: "Entry",
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border bg-muted/20 px-3 py-2">
      <div className="text-muted-foreground text-[10px] font-medium tracking-wider uppercase">
        {label}
      </div>
      <div className="mt-1 font-mono text-sm font-semibold tabular-nums">{value}</div>
    </div>
  )
}

export function LiveOrders({
  snapshot,
  connected,
  error,
  source,
}: {
  snapshot: LiveSnapshot | null
  connected: boolean
  error: string | null
  source: "websocket" | "poll" | "connecting"
}) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])

  const staleMs = snapshot ? snapshotAgeMs(snapshot, now) : Infinity
  const stale = staleMs > 12_000
  const runtime = snapshot?.runtime
  const running = runtime?.phase && runtime.phase !== "idle"
  const elapsed = running && runtime.started_ts
    ? Math.max(0, Math.round(now / 1000 - runtime.started_ts))
    : null

  return (
    <Card>
      <CardHeader className="gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="space-y-1">
          <CardTitle className="text-sm font-medium">Live venue orders</CardTitle>
          <div className="text-muted-foreground flex flex-wrap items-center gap-2 text-xs">
            <span>{snapshot ? `${snapshot.orders.length} resting on Hyperliquid` : "connecting…"}</span>
            {snapshot && <span className="font-mono">as of {stamp(snapshot.as_of_ts)}</span>}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-1.5">
          <Badge variant={connected ? "default" : "secondary"}>
            {source === "websocket" ? "WS LIVE" : source === "poll" ? "POLL FALLBACK" : "CONNECTING"}
          </Badge>
          {snapshot && (
            <Badge variant={stale ? "destructive" : "outline"} className="font-mono tabular-nums">
              {stale ? `STALE ${Math.round(staleMs / 1000)}s` : `FRESH ${Math.round(staleMs / 1000)}s`}
            </Badge>
          )}
          {runtime && (
            <Badge variant={running ? "secondary" : "outline"} className="font-mono tabular-nums">
              {running ? `QWEN ${runtime.phase.toUpperCase()} · ${elapsed}s` : "QWEN IDLE"}
            </Badge>
          )}
        </div>
      </CardHeader>
      <CardContent className="space-y-4">
        {error && (
          <div className="border-destructive/40 bg-destructive/5 text-destructive rounded-md border px-3 py-2 text-xs">
            Live stream degraded: {error}. Last valid snapshot remains visible.
          </div>
        )}

        {snapshot && (
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
            <Metric label="Account equity" value={usd(snapshot.account.equity)} />
            <Metric label="Free collateral" value={usd(snapshot.account.available_margin)} />
            <Metric label="Held collateral" value={usd(snapshot.account.held_collateral)} />
            <Metric label="Position margin" value={usd(snapshot.account.total_margin_used)} />
          </div>
        )}

        {!snapshot ? (
          <div className="flex justify-center py-10"><Spinner /></div>
        ) : snapshot.orders.length === 0 ? (
          <StatusBlankContainer className="py-8">
            <StatusBlankTitle>No resting venue orders</StatusBlankTitle>
            <StatusBlankDescription>Open positions currently have no visible brackets or entries.</StatusBlankDescription>
          </StatusBlankContainer>
        ) : (
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead>Market</TableHead>
                <TableHead>Role</TableHead>
                <TableHead>Side / size</TableHead>
                <TableHead className="text-right">Trigger</TableHead>
                <TableHead>Placed by</TableHead>
                <TableHead>Position</TableHead>
                <TableHead>Placed</TableHead>
                <TableHead>Decision</TableHead>
                <TableHead className="text-right">OID</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {snapshot.orders.map((order) => (
                <TableRow key={String(order.oid)}>
                  <TableCell className="font-mono font-medium">{order.market}</TableCell>
                  <TableCell>
                    <Badge
                      variant="outline"
                      className={cn(
                        order.role === "stop_loss" && "border-red-500/30 text-red-500",
                        order.role === "take_profit" && "border-emerald-500/30 text-emerald-500",
                        order.role === "entry" && "border-amber-500/30 text-amber-500"
                      )}
                    >
                      {roleLabel[order.role]}
                    </Badge>
                  </TableCell>
                  <TableCell className="whitespace-nowrap">
                    <span className="font-medium uppercase">{order.side}</span>{" "}
                    <span className="text-muted-foreground font-mono tabular-nums">{order.size ?? "—"}</span>
                  </TableCell>
                  <TableCell className="text-right font-mono tabular-nums">
                    {px(order.role === "entry" ? order.limit_px : order.trigger_px)}
                  </TableCell>
                  <TableCell>
                    <div className="font-medium">{order.placed_by}</div>
                    <div className="text-muted-foreground text-[11px]">
                      {order.route} · {order.attribution}
                    </div>
                  </TableCell>
                  <TableCell className="text-xs">
                    {order.position_source === "external"
                      ? "Operator · Qwen-managed"
                      : order.position_source === "own"
                        ? "Peri"
                        : "No linked position"}
                  </TableCell>
                  <TableCell className="whitespace-nowrap text-xs">
                    <div className="font-mono">{stamp(order.placed_ts)}</div>
                    <div className="text-muted-foreground">{ago(order.placed_ts)}</div>
                  </TableCell>
                  <TableCell>
                    {order.decision_id ? (
                      <Link
                        className="font-mono text-xs underline-offset-4 hover:underline"
                        href={`/decisions/${order.decision_id}`}
                      >
                        #{order.decision_id}
                      </Link>
                    ) : "—"}
                  </TableCell>
                  <TableCell className="text-muted-foreground text-right font-mono text-[11px] tabular-nums">
                    {order.oid}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}
