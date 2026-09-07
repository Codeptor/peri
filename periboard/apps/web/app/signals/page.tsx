"use client"

import { useState } from "react"
import { RiAlarmWarningLine } from "@remixicon/react"

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
import { api, type AssetBias } from "@/lib/api"
import { ago } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"
import { cn } from "@workspace/ui/lib/utils"

const BLACKOUT_MINS = 45

function Pane({
  title,
  hint,
  children,
}: {
  title: string
  hint: string
  children: React.ReactNode
}) {
  return (
    <Card className="flex h-full min-h-0 flex-col gap-0 py-0">
      <CardHeader className="flex-row items-baseline justify-between border-b py-3 [.border-b]:pb-3">
        <CardTitle className="text-sm font-medium">{title}</CardTitle>
        <span className="text-muted-foreground text-[11px]">{hint}</span>
      </CardHeader>
      <CardContent className="min-h-0 flex-1 p-0">{children}</CardContent>
    </Card>
  )
}

/** A divergence bar: profitable wallets vs losing ones, centred on zero. */
function Divergence({ value }: { value: number }) {
  const width = Math.min(50, (Math.abs(value) / 50) * 50)
  const positive = value > 0
  return (
    <div className="relative h-2 w-24 shrink-0 rounded-sm bg-muted">
      <div className="absolute inset-y-0 left-1/2 w-px bg-border" />
      <div
        className={cn(
          "absolute inset-y-0 rounded-sm",
          positive ? "bg-emerald-500" : "bg-red-500"
        )}
        style={
          positive
            ? { left: "50%", width: `${width}%` }
            : { right: "50%", width: `${width}%` }
        }
      />
    </div>
  )
}

function when(ts: number): { label: string; soon: boolean; imminent: boolean } {
  const mins = (ts - Date.now() / 1000) / 60
  if (mins < -60) return { label: "passed", soon: false, imminent: false }
  if (mins < 0) return { label: "just now", soon: true, imminent: true }
  if (mins < 60)
    return {
      label: `in ${Math.round(mins)}m`,
      soon: true,
      imminent: mins <= BLACKOUT_MINS,
    }
  if (mins < 60 * 36)
    return { label: `in ${(mins / 60).toFixed(1)}h`, soon: mins < 60 * 6, imminent: false }
  return { label: `in ${(mins / 1440).toFixed(1)}d`, soon: false, imminent: false }
}

export default function Signals() {
  const { data: bias } = usePoll(api.cohortBias, 30_000)
  const { data: calendar } = usePoll(() => api.calendar(10), 60_000)
  const { data: opBias, reload: reloadBias } = usePoll(api.operatorBias, 30_000)
  const { data: notes, reload: reloadNotes } = usePoll(() => api.notes(20), 30_000)
  const [draftBias, setDraftBias] = useState("")
  const [draftNote, setDraftNote] = useState("")
  const [busy, setBusy] = useState(false)

  const assets = Object.entries(bias?.assets ?? {}) as [string, AssetBias][]

  async function saveBias() {
    if (!draftBias.trim()) return
    setBusy(true)
    try {
      await api.setOperatorBias(draftBias.trim())
      setDraftBias("")
      reloadBias()
    } finally {
      setBusy(false)
    }
  }

  async function saveNote() {
    if (!draftNote.trim()) return
    setBusy(true)
    try {
      await api.addNote(draftNote.trim())
      setDraftNote("")
      reloadNotes()
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="grid h-[calc(100dvh-7.5rem)] min-h-0 grid-rows-2 gap-4 lg:grid-cols-2 lg:grid-rows-1">
      <Pane
        title="Trench market bias"
        hint={
          bias?.fetched_ts
            ? `${bias.total_traders?.toLocaleString() ?? "?"} traders · ${ago(bias.fetched_ts)}`
            : "waiting for a cycle"
        }
      >
        {bias == null ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : bias.cohorts.length === 0 ? (
          <StatusBlankContainer className="h-full">
            <StatusBlankTitle>No read yet</StatusBlankTitle>
            <StatusBlankDescription>
              Positioning arrives with the next decision cycle.
            </StatusBlankDescription>
          </StatusBlankContainer>
        ) : (
          <ScrollArea className="h-full">
            <div className="text-muted-foreground px-4 pt-3 pb-1 text-[10px] tracking-wide uppercase">
              by cohort — wallets bucketed by realised PnL
            </div>
            {bias.cohorts.map((c) => (
              <div
                key={c.id}
                className="grid grid-cols-[1fr_auto_auto] items-baseline gap-3 px-4 py-1.5 text-[13px]"
              >
                <span className="truncate">
                  {c.label}
                  <span className="text-muted-foreground ml-2 text-[11px]">
                    {c.range} · {c.traders.toLocaleString()}
                  </span>
                </span>
                <span className="font-mono text-xs tabular-nums">
                  {c.long_pct == null ? "—" : `${Math.round(c.long_pct)}% long`}
                </span>
                <span
                  className={cn(
                    "w-32 text-right text-[11px]",
                    c.sentiment.includes("Bear")
                      ? "text-red-500"
                      : c.sentiment.includes("Bull")
                        ? "text-emerald-500"
                        : "text-muted-foreground"
                  )}
                >
                  {c.sentiment}
                </span>
              </div>
            ))}

            <div className="text-muted-foreground border-t px-4 pt-3 pb-1 text-[10px] tracking-wide uppercase">
              by asset — smart money minus the crowd
            </div>
            {assets.length === 0 ? (
              <div className="text-muted-foreground px-4 py-2 text-[13px]">
                No per-market read yet.
              </div>
            ) : (
              assets.map(([market, a]) => (
                <div
                  key={market}
                  className="grid grid-cols-[1fr_auto_auto_auto] items-center gap-3 px-4 py-1.5 text-[13px]"
                >
                  <span className="truncate font-mono text-xs">{market}</span>
                  <span className="text-muted-foreground font-mono text-[11px] tabular-nums">
                    {a.smart_long_pct == null ? "—" : `${Math.round(a.smart_long_pct)}`}
                    {" / "}
                    {a.crowd_long_pct == null ? "—" : `${Math.round(a.crowd_long_pct)}`}
                  </span>
                  {a.divergence == null ? (
                    <span className="w-24" />
                  ) : (
                    <Divergence value={a.divergence} />
                  )}
                  <span
                    className={cn(
                      "w-14 text-right font-mono text-xs tabular-nums",
                      (a.divergence ?? 0) > 0 ? "text-emerald-500" : "text-red-500"
                    )}
                  >
                    {a.divergence == null
                      ? "—"
                      : `${a.divergence > 0 ? "+" : ""}${Math.round(a.divergence)}pp`}
                  </span>
                </div>
              ))
            )}
            <p className="text-muted-foreground px-4 py-3 text-[11px] leading-snug">
              Positive means the wallets that make money are more long than the
              ones that lose it. Evidence about positioning — never a thesis on
              its own.
            </p>
          </ScrollArea>
        )}
      </Pane>

      <Pane
        title="Calendar & operator context"
        hint={`entries refused ${BLACKOUT_MINS}m before a HIGH event`}
      >
        <ScrollArea className="h-full">
          <div className="text-muted-foreground px-4 pt-3 pb-1 text-[10px] tracking-wide uppercase">
            economic calendar
          </div>
          {calendar == null ? (
            <div className="px-4 py-3">
              <Spinner />
            </div>
          ) : calendar.length === 0 ? (
            <div className="text-muted-foreground px-4 py-2 text-[13px]">
              Nothing scheduled — it syncs from Trench each cycle.
            </div>
          ) : (
            calendar.map((e) => {
              const w = when(e.ts)
              return (
                <div
                  key={e.id}
                  className={cn(
                    "flex items-baseline gap-2 px-4 py-1.5 text-[13px]",
                    w.imminent && e.impact === "high" && "bg-red-500/10"
                  )}
                >
                  <Badge
                    variant="outline"
                    className={cn(
                      "h-5 shrink-0 px-1.5 text-[10px] uppercase",
                      e.impact === "high"
                        ? "border-red-500/40 text-red-500"
                        : "text-muted-foreground"
                    )}
                  >
                    {e.impact}
                  </Badge>
                  <span
                    className={cn(
                      "w-20 shrink-0 font-mono text-[11px] tabular-nums",
                      w.soon ? "text-amber-500" : "text-muted-foreground"
                    )}
                  >
                    {w.label}
                  </span>
                  <span className="min-w-0 flex-1 truncate">{e.title}</span>
                  {w.imminent && e.impact === "high" && (
                    <RiAlarmWarningLine className="size-3.5 shrink-0 text-red-500" />
                  )}
                </div>
              )
            })
          )}

          <div className="text-muted-foreground border-t px-4 pt-3 pb-1 text-[10px] tracking-wide uppercase">
            your standing bias — advisory, weighed not enforced
          </div>
          {opBias?.text && (
            <p className="px-4 pb-2 text-[13px] leading-snug">
              {opBias.text}
              <span className="text-muted-foreground ml-2 text-[11px]">
                {opBias.ts ? ago(opBias.ts) : ""}
              </span>
            </p>
          )}
          <div className="flex items-center gap-2 px-4 pb-3">
            <Input
              className="h-8 text-[13px]"
              disabled={busy}
              onChange={(e) => setDraftBias(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveBias()
              }}
              placeholder="e.g. risk-off into Friday; favour shorts on retests…"
              value={draftBias}
            />
            <Button disabled={busy || !draftBias.trim()} onClick={saveBias} size="sm">
              Set
            </Button>
          </div>

          <div className="text-muted-foreground border-t px-4 pt-3 pb-1 text-[10px] tracking-wide uppercase">
            notes — context with a shelf life (72h)
          </div>
          {(notes ?? []).map((n) => (
            <div key={n.id} className="px-4 py-1.5 text-[13px] leading-snug">
              <span className="text-muted-foreground mr-2 font-mono text-[11px]">
                {ago(n.ts)}
              </span>
              {n.text}
            </div>
          ))}
          <div className="flex items-center gap-2 px-4 py-3">
            <Input
              className="h-8 text-[13px]"
              disabled={busy}
              onChange={(e) => setDraftNote(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveNote()
              }}
              placeholder="a dated fact the analyst should know this week…"
              value={draftNote}
            />
            <Button
              disabled={busy || !draftNote.trim()}
              onClick={saveNote}
              size="sm"
              variant="outline"
            >
              Add
            </Button>
          </div>
        </ScrollArea>
      </Pane>
    </div>
  )
}
