"use client"

import Link from "next/link"
import { useParams } from "next/navigation"

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
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  Snippet,
  SnippetCopyButton,
  SnippetHeader,
  SnippetTabsContent,
  SnippetTabsList,
  SnippetTabsTrigger,
} from "@workspace/ui/components/kibo-ui/snippet"
import {
  Reasoning,
  ReasoningContent,
  ReasoningTrigger,
} from "@/components/ai-elements/reasoning"
import {
  Tool,
  ToolContent,
  ToolHeader,
  ToolInput,
  ToolOutput,
} from "@/components/ai-elements/tool"
import { api } from "@/lib/api"
import { px, stamp } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"

export default function DecisionTrace() {
  const params = useParams<{ id: string }>()
  const { data: d, error } = usePoll(() => api.decision(params.id), 60_000)

  if (error) return <div className="text-red-500">failed to load: {error}</div>
  if (!d) return <Spinner />

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <Link
          href="/decisions"
          className="text-muted-foreground text-sm underline-offset-4 hover:underline"
        >
          ← feed
        </Link>
        <h1 className="text-lg font-semibold">Decision #{d.id}</h1>
        <Badge variant="secondary">{d.trigger}</Badge>
        <Badge variant="outline">{d.model}</Badge>
        <Badge variant="outline">{((d.latency_ms ?? 0) / 1000).toFixed(1)}s</Badge>
        <Badge variant={d.status === "ok" ? "default" : "destructive"}>
          {d.status}
        </Badge>
        <span className="text-muted-foreground font-mono text-xs">
          {stamp(d.ts)}
        </span>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-sm font-medium">Market view</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm leading-relaxed">{d.market_view || "—"}</p>
        </CardContent>
      </Card>

      {d.tool_log.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm font-medium">Searches ({d.tool_log.length})</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3">
            {d.tool_log.map((t, i) => (
              <Tool key={i} defaultOpen={false}>
                <ToolHeader
                  type={`tool-${t.tool}`}
                  state="output-available"
                  title={t.args.query ? `${t.tool}: ${t.args.query}` : t.tool}
                />
                <ToolContent>
                  <ToolInput input={t.args} />
                  <ToolOutput
                    errorText={undefined}
                    output={
                      <pre className="whitespace-pre-wrap p-3 font-mono text-xs">
                        {t.result}
                      </pre>
                    }
                  />
                </ToolContent>
              </Tool>
            ))}
          </CardContent>
        </Card>
      )}

      {d.reasoning && (
        <Card>
          <CardHeader>
            <CardTitle className="text-sm font-medium">Chain of thought</CardTitle>
          </CardHeader>
          <CardContent>
            <Reasoning isStreaming={false} defaultOpen>
              <ReasoningTrigger />
              <ReasoningContent>{d.reasoning}</ReasoningContent>
            </Reasoning>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="text-sm font-medium">Actions ({d.actions.length})</CardTitle>
        </CardHeader>
        <CardContent>
          {d.actions.length ? (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Kind</TableHead>
                  <TableHead>Market</TableHead>
                  <TableHead>Side</TableHead>
                  <TableHead className="text-right">Conv</TableHead>
                  <TableHead className="text-right">Stop</TableHead>
                  <TableHead className="text-right">TP</TableHead>
                  <TableHead className="text-right">Lev</TableHead>
                  <TableHead>Source</TableHead>
                  <TableHead>Rationale / invalidation</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {d.actions.map((a, i) => (
                  <TableRow key={i}>
                    <TableCell>
                      <Badge
                        variant={
                          a.kind === "open"
                            ? "default"
                            : a.kind === "close"
                              ? "destructive"
                              : "secondary"
                        }
                      >
                        {a.kind}
                      </Badge>
                    </TableCell>
                    <TableCell className="font-mono">{a.market}</TableCell>
                    <TableCell>{a.side ?? "—"}</TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {a.conviction ?? "—"}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {px(a.stop)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {px(a.take_profit)}
                    </TableCell>
                    <TableCell className="text-right font-mono tabular-nums">
                      {a.leverage ? `${a.leverage}x` : "—"}
                    </TableCell>
                    <TableCell>{a.source ?? "—"}</TableCell>
                    <TableCell className="max-w-80 text-xs">
                      <div className="truncate">{a.rationale}</div>
                      <div className="text-muted-foreground truncate">
                        ✕ {a.invalidation}
                      </div>
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          ) : (
            <p className="text-muted-foreground text-sm">
              hold — no actions this cycle
            </p>
          )}
        </CardContent>
      </Card>

      {d.prompt && (
        <Snippet defaultValue="prompt">
          <SnippetHeader>
            <SnippetTabsList>
              <SnippetTabsTrigger value="prompt">
                context bundle (what the model saw)
              </SnippetTabsTrigger>
            </SnippetTabsList>
            <SnippetCopyButton value={d.prompt} />
          </SnippetHeader>
          <SnippetTabsContent
            value="prompt"
            className="max-h-[32rem] overflow-y-auto text-xs leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]"
          >
            {d.prompt}
          </SnippetTabsContent>
        </Snippet>
      )}
    </div>
  )
}
