"use client"

import { useState } from "react"
import { RiDeleteBinLine, RiPushpinFill } from "@remixicon/react"

import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@workspace/ui/components/card"
import { ScrollArea } from "@workspace/ui/components/scroll-area"
import { Badge } from "@workspace/ui/components/badge"
import { Button } from "@workspace/ui/components/button"
import { Input } from "@workspace/ui/components/input"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  StatusBlankContainer,
  StatusBlankDescription,
  StatusBlankTitle,
} from "@workspace/ui/components/blocks/status-blank"
import { api, type PerfBucket } from "@/lib/api"
import { ago, signedUsd } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"
import { cn } from "@workspace/ui/lib/utils"

function Pane({
  title,
  count,
  hint,
  children,
}: {
  title: string
  count?: number
  hint: string
  children: React.ReactNode
}) {
  return (
    <Card className="flex h-full min-h-0 flex-col gap-0 py-0">
      <CardHeader className="flex-row items-baseline justify-between border-b py-3 [.border-b]:pb-3">
        <CardTitle className="flex items-baseline gap-2 text-sm font-medium">
          {title}
          {count != null && (
            <span className="text-muted-foreground font-mono text-xs tabular-nums">
              {count}
            </span>
          )}
        </CardTitle>
        <span className="text-muted-foreground text-[11px]">{hint}</span>
      </CardHeader>
      <CardContent className="min-h-0 flex-1 p-0">{children}</CardContent>
    </Card>
  )
}

function Row({ label, b }: { label: string; b: PerfBucket }) {
  return (
    <div className="grid grid-cols-[1fr_auto_auto_auto_auto] items-baseline gap-3 px-4 py-1.5 text-[13px]">
      <span className="truncate">{label}</span>
      <span className="text-muted-foreground font-mono text-xs tabular-nums">
        {b.n}
      </span>
      <span
        className={cn(
          "font-mono text-xs tabular-nums",
          b.win_rate == null
            ? "text-muted-foreground"
            : b.win_rate >= 0.5
              ? "text-emerald-500"
              : "text-red-500"
        )}
      >
        {b.win_rate == null ? "—" : `${Math.round(b.win_rate * 100)}%`}
      </span>
      <span
        className={cn(
          "w-16 text-right font-mono text-xs tabular-nums",
          b.pnl >= 0 ? "text-emerald-500" : "text-red-500"
        )}
      >
        {signedUsd(b.pnl)}
      </span>
      <span className="text-muted-foreground w-14 text-right font-mono text-xs tabular-nums">
        {b.avg_r == null ? "—" : `${b.avg_r >= 0 ? "+" : ""}${b.avg_r.toFixed(2)}R`}
      </span>
    </div>
  )
}

function Group({
  title,
  buckets,
}: {
  title: string
  buckets: Record<string, PerfBucket>
}) {
  const entries = Object.entries(buckets)
  if (entries.length === 0) return null
  return (
    <div className="border-t py-1.5">
      <div className="text-muted-foreground px-4 pt-1 pb-0.5 text-[11px] tracking-wide uppercase">
        {title}
      </div>
      {entries.map(([label, b]) => (
        <Row key={label} label={label} b={b} />
      ))}
    </div>
  )
}

export default function Memory() {
  const { data: perf } = usePoll(() => api.performance(), 30_000)
  const { data: lessons, reload: refresh } = usePoll(() => api.lessons(100), 30_000)
  const [draft, setDraft] = useState("")
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<string | null>(null)

  async function addLesson() {
    const text = draft.trim()
    if (!text) return
    setBusy(true)
    setNote(null)
    try {
      const r = await api.addLesson(text)
      setDraft("")
      setNote(r.duplicate ? "Already remembered" : null)
      refresh()
    } catch {
      setNote("Could not save")
    } finally {
      setBusy(false)
    }
  }

  async function forget(id: number) {
    await api.forgetLesson(id)
    refresh()
  }

  return (
    <div className="grid h-[calc(100dvh-7.5rem)] min-h-0 grid-rows-2 gap-4 lg:grid-cols-2 lg:grid-rows-1">
      <Pane
        title="Measured record"
        count={perf?.overall.n}
        hint="computed from every closed trade · in every prompt"
      >
        {perf == null ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : perf.overall.n === 0 ? (
          <StatusBlankContainer className="h-full">
            <StatusBlankTitle>No closed trades yet</StatusBlankTitle>
            <StatusBlankDescription>
              Attribution appears as positions close.
            </StatusBlankDescription>
          </StatusBlankContainer>
        ) : (
          <ScrollArea className="h-full">
            <div className="grid grid-cols-[1fr_auto_auto_auto_auto] gap-3 px-4 pt-3 pb-1 text-[10px] tracking-wide text-muted-foreground uppercase">
              <span>bucket</span>
              <span>n</span>
              <span>win</span>
              <span className="w-16 text-right">net</span>
              <span className="w-14 text-right">avg R</span>
            </div>
            <Row label="All trades" b={perf.overall} />
            <Group title="entry style" buckets={perf.by_entry_style} />
            <Group title="where in the 24h range" buckets={perf.by_range_position} />
            <Group title="side" buckets={perf.by_side} />
            <Group title="how it ended" buckets={perf.by_close_reason} />
            <Group title="costliest markets" buckets={perf.worst_markets} />
            <Group title="best markets" buckets={perf.best_markets} />
          </ScrollArea>
        )}
      </Pane>

      <Pane
        title="Memory"
        count={lessons?.length}
        hint="the only thing it carries between cycles"
      >
        <div className="flex h-full min-h-0 flex-col">
          <div className="flex items-center gap-2 border-b px-4 py-2.5">
            <Input
              className="h-8 text-[13px]"
              disabled={busy}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") addLesson()
              }}
              placeholder="Teach it a rule — pinned, never forgotten…"
              value={draft}
            />
            <Button
              disabled={busy || draft.trim().length === 0}
              onClick={addLesson}
              size="sm"
            >
              {busy ? "Saving…" : "Teach"}
            </Button>
          </div>
          {note && (
            <div className="text-muted-foreground border-b px-4 py-1.5 text-[11px]">
              {note}
            </div>
          )}
          {lessons == null ? (
            <div className="flex flex-1 items-center justify-center">
              <Spinner />
            </div>
          ) : lessons.length === 0 ? (
            <StatusBlankContainer className="flex-1">
              <StatusBlankTitle>Nothing learned yet</StatusBlankTitle>
              <StatusBlankDescription>
                The analyst writes a lesson when it can name why a trade worked
                or failed.
              </StatusBlankDescription>
            </StatusBlankContainer>
          ) : (
            <ScrollArea className="min-h-0 flex-1">
              <div className="divide-y">
                {lessons.map((l) => (
                  <div
                    key={l.id}
                    className={cn(
                      "group space-y-1 px-4 py-2.5",
                      l.pinned === 1 &&
                        "border-l-2 border-l-sky-500 bg-sky-500/[0.06]"
                    )}
                  >
                    <div className="flex items-center gap-2">
                      {l.pinned === 1 ? (
                        <Badge className="h-5 gap-1 bg-sky-500/15 px-1.5 text-[10px] font-semibold text-sky-500">
                          <RiPushpinFill className="size-3" />
                          OPERATOR
                        </Badge>
                      ) : (
                        <Badge
                          variant="outline"
                          className="h-5 px-1.5 text-[10px]"
                        >
                          analyst
                        </Badge>
                      )}
                      {l.market && (
                        <span className="font-mono text-[11px]">{l.market}</span>
                      )}
                      <span className="text-muted-foreground text-[11px] tabular-nums">
                        {ago(l.ts)}
                      </span>
                      <Button
                        aria-label="Forget this lesson"
                        className="ml-auto size-6 opacity-0 transition-opacity group-hover:opacity-100"
                        onClick={() => forget(l.id)}
                        size="icon"
                        variant="ghost"
                      >
                        <RiDeleteBinLine className="size-3.5" />
                      </Button>
                    </div>
                    <p className="text-[13px] leading-snug">{l.text}</p>
                  </div>
                ))}
              </div>
            </ScrollArea>
          )}
        </div>
      </Pane>
    </div>
  )
}
