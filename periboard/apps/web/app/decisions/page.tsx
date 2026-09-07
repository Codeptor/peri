"use client"

import { useRouter } from "next/navigation"

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
import { ScrollArea } from "@workspace/ui/components/scroll-area"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import { api } from "@/lib/api"
import { stamp } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"
import { cn } from "@workspace/ui/lib/utils"

export default function Decisions() {
  const router = useRouter()
  const { data: decisions } = usePoll(() => api.decisions(100))

  return (
    <Card className="flex h-[calc(100dvh-7.5rem)] min-h-0 flex-col gap-0 py-0">
      <CardHeader className="flex-row items-baseline justify-between border-b py-3 [.border-b]:pb-3">
        <CardTitle className="flex items-baseline gap-2 text-sm font-medium">
          Decision feed
          {decisions && (
            <span className="text-muted-foreground font-mono text-xs tabular-nums">
              {decisions.length}
            </span>
          )}
        </CardTitle>
        <span className="text-muted-foreground text-[11px]">
          every cycle, fully traced — click a row
        </span>
      </CardHeader>
      <CardContent className="min-h-0 flex-1 p-0">
        {decisions ? (
          <ScrollArea className="h-full">
            <Table className="[&_th]:bg-card [&_th]:sticky [&_th]:top-0 [&_th]:z-10">
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead>When</TableHead>
                <TableHead>Trigger</TableHead>
                <TableHead className="text-right">Latency</TableHead>
                <TableHead className="text-center">Searches</TableHead>
                <TableHead className="text-center">Verdict</TableHead>
                <TableHead className="w-full">Market view</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {decisions.map((d) => (
                <TableRow
                  key={d.id}
                  onClick={() => router.push(`/decisions/${d.id}`)}
                  className="cursor-pointer"
                >
                  <TableCell className="font-mono text-xs whitespace-nowrap">
                    {stamp(d.ts)}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant="secondary"
                      className="text-[10px] whitespace-nowrap"
                    >
                      {d.trigger}
                    </Badge>
                  </TableCell>
                  <TableCell className="text-right font-mono text-xs tabular-nums">
                    {((d.latency_ms ?? 0) / 1000).toFixed(1)}s
                  </TableCell>
                  <TableCell className="text-center font-mono text-xs tabular-nums">
                    {d.tool_log.length || "·"}
                  </TableCell>
                  <TableCell className="text-center">
                    {d.status === "error" ? (
                      <Badge variant="destructive" className="text-[10px]">
                        error
                      </Badge>
                    ) : d.actions.length ? (
                      <Badge className="text-[10px]">
                        {d.actions.length} act
                      </Badge>
                    ) : (
                      <span className="text-muted-foreground text-xs">hold</span>
                    )}
                  </TableCell>
                  <TableCell
                    className={cn(
                      "max-w-0 truncate text-xs",
                      d.status === "error" && "text-red-500"
                    )}
                    title={d.market_view ?? undefined}
                  >
                    {d.market_view || "—"}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
            </Table>
          </ScrollArea>
        ) : (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        )}
      </CardContent>
    </Card>
  )
}
