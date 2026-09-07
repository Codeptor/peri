"use client"

import Link from "next/link"
import { usePathname } from "next/navigation"
import { useTheme } from "next-themes"
import { useEffect, useState } from "react"
import {
  RiAlertLine,
  RiBrainLine,
  RiRadarLine,
  RiChatAiLine,
  RiDashboardLine,
  RiExchange2Line,
  RiBookOpenLine,
  RiNewspaperLine,
  RiPlayLine,
  RiPauseLine,
  RiPulseLine,
} from "@remixicon/react"

import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
} from "@workspace/ui/components/sidebar"
import { Separator } from "@workspace/ui/components/separator"
import { Badge } from "@workspace/ui/components/badge"
import { Button } from "@workspace/ui/components/button"
import { ThemeSwitcher } from "@workspace/ui/components/kibo-ui/theme-switcher"
import { usePoll } from "@/lib/use-poll"
import { useLiveDashboard } from "@/lib/use-live-dashboard"
import { api } from "@/lib/api"
import { ago, istTime, signedUsd } from "@/lib/format"
import { cn } from "@workspace/ui/lib/utils"

const NAV = [
  { href: "/", label: "Overview", icon: RiDashboardLine },
  { href: "/analyst", label: "Analyst", icon: RiChatAiLine },
  { href: "/decisions", label: "Decisions", icon: RiBrainLine },
  { href: "/trades", label: "Trades", icon: RiExchange2Line },
  { href: "/intel", label: "Intel", icon: RiNewspaperLine },
  { href: "/signals", label: "Signals", icon: RiRadarLine },
  { href: "/memory", label: "Memory", icon: RiBookOpenLine },
]

export function IstClock() {
  const [nowMs, setNowMs] = useState<number | null>(null)

  useEffect(() => {
    const tick = () => setNowMs(Date.now())
    tick()
    const timer = window.setInterval(tick, 1000)
    return () => window.clearInterval(timer)
  }, [])

  return (
    <time
      aria-label="Current time in India Standard Time"
      className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums"
      dateTime={nowMs == null ? undefined : new Date(nowMs).toISOString()}
      title="India Standard Time"
    >
      {nowMs == null ? "--:--:-- IST" : istTime(nowMs)}
    </time>
  )
}

export function Shell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname()
  const { theme, setTheme } = useTheme()
  const { data: status } = usePoll(api.status, 15_000)
  const live = useLiveDashboard()
  const [wakeState, setWakeState] = useState<
    "idle" | "pending" | "queued" | "already" | "error"
  >("idle")
  const [pauseBusy, setPauseBusy] = useState(false)
  const [pauseOverride, setPauseOverride] = useState<boolean | null>(null)
  const [pauseError, setPauseError] = useState<string | null>(null)

  // the poll is the source of truth; the override only covers the gap between
  // the click and the next /api/status so the button never lies about state
  const paused = pauseOverride ?? status?.paused ?? false
  const restingCount = status?.resting_entries?.length ?? 0
  // a single unfinished action journal refuses EVERY new entry, so it needs to
  // be impossible to miss and one click to clear
  const { data: stuck, reload: reloadStuck } = usePoll(api.executions, 30_000)
  const [resolving, setResolving] = useState<string | null>(null)

  async function resolveStuck(id: string) {
    setResolving(id)
    try {
      await api.resolveExecution(id)
      reloadStuck()
    } finally {
      setResolving(null)
    }
  }
  // a resting entry is a live maker order with its brackets already attached —
  // not a position, so it does not consume a concurrency slot, but it IS money
  // committed and the header should say so
  // the calendar GATES entries, so the next high-impact print belongs where the
  // eye already is, not two clicks away
  const { data: calendar } = usePoll(() => api.calendar(3), 60_000)
  const nextHigh = (calendar ?? [])
    .filter((e) => e.impact === "high" && e.ts > Date.now() / 1000)
    .sort((a, b) => a.ts - b.ts)[0]
  const minsToEvent = nextHigh ? (nextHigh.ts - Date.now() / 1000) / 60 : null
  const inBlackout = minsToEvent != null && minsToEvent <= 45

  const restingTitle = (status?.resting_entries ?? [])
    .map(
      (e) =>
        `${e.side} ${e.market} @ ${e.entry_px} (stop ${e.stop_px} / tp ${e.tp_px})`
    )
    .join("\n")

  useEffect(() => {
    if (pauseOverride !== null && status?.paused === pauseOverride) {
      setPauseOverride(null)
    }
  }, [status?.paused, pauseOverride])

  async function togglePause() {
    const next = !paused
    setPauseBusy(true)
    setPauseError(null)
    try {
      const result = next ? await api.pause() : await api.resume()
      setPauseOverride(result.paused)
    } catch {
      setPauseError(next ? "Pause failed" : "Resume failed")
    } finally {
      setPauseBusy(false)
    }
  }

  async function wakeQwen() {
    setWakeState("pending")
    try {
      const result = await api.wake()
      setWakeState(result.already_pending ? "already" : "queued")
    } catch {
      setWakeState("error")
    }
  }

  const wakeLabel = {
    idle: "Wake Qwen",
    pending: "Waking…",
    queued: "Qwen queued",
    already: "Already queued",
    error: "Retry wake",
  }[wakeState]
  const realizedTotal = live.data?.realized.total ?? status?.realized_total

  return (
    <SidebarProvider>
      <Sidebar collapsible="icon">
        <SidebarHeader>
          <div className="flex items-center gap-2 px-2 py-1.5">
            <RiPulseLine className="size-5 shrink-0 text-emerald-500" />
            <span className="text-base font-semibold tracking-tight group-data-[collapsible=icon]:hidden">
              peri
            </span>
            {status && (
              <Badge
                variant={status.mode === "live" ? "destructive" : "secondary"}
                className="h-5 px-1.5 text-[10px] tracking-wider uppercase group-data-[collapsible=icon]:hidden"
              >
                {status.mode}
              </Badge>
            )}
          </div>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupLabel>Pipeline</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {NAV.map((item) => (
                  <SidebarMenuItem key={item.href}>
                    <SidebarMenuButton
                      isActive={
                        item.href === "/"
                          ? pathname === "/"
                          : pathname.startsWith(item.href)
                      }
                      tooltip={item.label}
                      render={<Link href={item.href} />}
                    >
                      <item.icon />
                      <span>{item.label}</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
        <SidebarFooter>
          <div className="space-y-0.5 px-2 pb-2 text-[11px] leading-4 text-muted-foreground group-data-[collapsible=icon]:hidden">
            <div className="truncate font-mono">{status?.model ?? "…"}</div>
            <div>
              decided{" "}
              {status?.last_decision_ts ? ago(status.last_decision_ts) : "—"}
            </div>
          </div>
        </SidebarFooter>
      </Sidebar>
      <SidebarInset>
        <header className="sticky top-0 z-30 flex h-12 shrink-0 items-center gap-3 border-b bg-background/80 px-4 backdrop-blur">
          <SidebarTrigger className="-ml-1" />
          <Separator orientation="vertical" className="h-4" />
          <div className="flex min-w-0 items-center gap-3 text-sm">
            <span className="hidden truncate text-muted-foreground sm:inline">
              {status?.network ?? "…"}
            </span>
            {status && (
              <>
                <Separator
                  orientation="vertical"
                  className="hidden h-4 sm:block"
                />
                <span className="font-mono tabular-nums">
                  {status.peri_open_positions}/{status.max_concurrent} peri ·{" "}
                  {status.external_positions} external
                  {restingCount > 0 && (
                    <>
                      {" · "}
                      <span
                        className="text-amber-500"
                        title={restingTitle}
                      >
                        {restingCount} resting
                      </span>
                    </>
                  )}
                </span>
                <Separator orientation="vertical" className="h-4" />
                <span
                  className={cn(
                    "font-mono font-medium tabular-nums",
                    (realizedTotal ?? 0) >= 0
                      ? "text-emerald-500"
                      : "text-red-500"
                  )}
                >
                  {signedUsd(realizedTotal ?? status.realized_total)}
                </span>
              </>
            )}
          </div>
          <div className="ml-auto flex items-center gap-2">
            {nextHigh && minsToEvent != null && minsToEvent < 60 * 12 && (
              <span
                className={cn(
                  "hidden font-mono text-[11px] tabular-nums sm:inline",
                  inBlackout ? "text-red-500" : "text-muted-foreground"
                )}
                title={
                  inBlackout
                    ? `${nextHigh.title} — new entries are refused until it prints`
                    : nextHigh.title
                }
              >
                {inBlackout ? "BLACKOUT " : ""}
                {minsToEvent < 60
                  ? `${Math.round(minsToEvent)}m`
                  : `${(minsToEvent / 60).toFixed(1)}h`}{" "}
                to {nextHigh.title.split("(")[0]?.trim().slice(0, 28)}
              </span>
            )}
            <IstClock />
            <Button
              aria-live="polite"
              disabled={wakeState === "pending" || paused}
              onClick={wakeQwen}
              size="sm"
              title={
                paused
                  ? "Paused — resume before queuing a cycle"
                  : "Queue an immediate venue-authoritative Qwen cycle"
              }
              variant={wakeState === "error" ? "destructive" : "outline"}
            >
              <RiBrainLine />
              {wakeLabel}
            </Button>
            <Button
              aria-live="polite"
              aria-pressed={paused}
              disabled={pauseBusy}
              onClick={togglePause}
              size="sm"
              title={
                paused
                  ? "Resume: the risk engine accepts new entries again"
                  : "Kill switch: halt every new entry and cancel resting orders. Open positions keep their venue brackets."
              }
              variant={paused ? "default" : "destructive"}
            >
              {paused ? <RiPlayLine /> : <RiPauseLine />}
              {pauseBusy
                ? paused
                  ? "Resuming…"
                  : "Pausing…"
                : pauseError
                  ? pauseError
                  : paused
                    ? "Resume"
                    : "Pause"}
            </Button>
            <ThemeSwitcher
              value={(theme as "light" | "dark" | "system") ?? "dark"}
              onChange={setTheme}
            />
          </div>
        </header>
        {(stuck ?? []).length > 0 && (
          <div
            role="alert"
            className="space-y-1 border-b border-red-500/40 bg-red-500/10 px-4 py-2 text-xs text-red-600 dark:text-red-400"
          >
            {(stuck ?? []).map((e) => (
              <div key={e.id} className="flex items-center gap-2">
                <RiAlertLine className="size-3.5 shrink-0" />
                <span className="min-w-0 flex-1 truncate">
                  <span className="font-medium">
                    {e.kind} {String(e.action?.market ?? "")} is {e.status}
                  </span>
                  {" — this blocks every new entry. "}
                  {e.result?.reason ?? "check the venue before clearing."}
                </span>
                <Button
                  className="h-6 shrink-0 px-2 text-[11px]"
                  disabled={resolving === e.id}
                  onClick={() => resolveStuck(e.id)}
                  size="sm"
                  variant="destructive"
                  title="Mark it terminal so entries resume. Verify the venue first."
                >
                  {resolving === e.id ? "Clearing…" : "Clear"}
                </Button>
              </div>
            ))}
          </div>
        )}
        {paused && (
          <div
            role="status"
            className="flex items-center justify-center gap-2 border-b border-amber-500/40 bg-amber-500/10 px-4 py-1.5 text-xs font-medium text-amber-600 dark:text-amber-400"
          >
            <RiPauseLine className="size-3.5" />
            PAUSED — no new entries. Open positions keep their stop and
            take-profit at the venue and are still managed.
            {status?.pause_state?.ts
              ? ` Paused ${ago(status.pause_state.ts)}.`
              : ""}
          </div>
        )}
        <main
          className={cn(
            "w-full flex-1 space-y-4 p-4 lg:p-6",
            // intel is a full-viewport terminal — every other page keeps a readable column
            !["/analyst", "/intel", "/decisions", "/memory", "/signals"].includes(
              pathname
            ) &&
              "mx-auto max-w-[1440px]"
          )}
        >
          {children}
        </main>
      </SidebarInset>
    </SidebarProvider>
  )
}
