"use client"

import * as React from "react"
import Link from "next/link"
import { usePathname } from "next/navigation"
import {
  Analytics01Icon,
  BirdIcon,
  ChartLineData01Icon,
  DashboardSquare01Icon,
  SatelliteIcon,
  SparklesIcon,
  Search01Icon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react"

import { api, type EquityPoint, type Health } from "@/lib/api"
import { usd } from "@/lib/format"
import { BANKROLL } from "@/lib/risk"
import { cn } from "@/lib/utils"
import { useLiveFeed } from "@/lib/ws"
import { IconChip } from "@/components/blocks/icon-chip"
import { StatusPill } from "@/components/blocks/status-pill"
import { TrendArea } from "@/components/blocks/trend-area"
import { ThemeToggle } from "@/components/theme-toggle"
import { Input } from "@/components/ui/input"

type NavItem = { href: string; label: string; icon: IconSvgElement }

const GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "Essentials",
    items: [
      { href: "/", label: "Overview", icon: DashboardSquare01Icon },
      { href: "/markets", label: "Markets", icon: Analytics01Icon },
    ],
  },
  {
    label: "Trading",
    items: [
      { href: "/positions", label: "Positions", icon: ChartLineData01Icon },
    ],
  },
  {
    label: "Intel",
    items: [
      { href: "/intel", label: "Wire", icon: SatelliteIcon },
      { href: "/analyst", label: "Analyst", icon: SparklesIcon },
    ],
  },
]

const ALL_ITEMS = GROUPS.flatMap((g) => g.items)
const NEWS_FRESH_MS = 30 * 60_000
const POLL_MS = 30_000
const NO_HANDLERS = {}

function useHealth(): Health | null {
  const [health, setHealth] = React.useState<Health | null>(null)
  React.useEffect(() => {
    let cancelled = false
    const tick = () => {
      api
        .health()
        .then((h) => {
          if (!cancelled) setHealth(h)
        })
        .catch(() => {
          if (!cancelled) setHealth(null)
        })
    }
    tick()
    const id = setInterval(tick, POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])
  return health
}

/** Global top-right: browser↔traderd websocket dot + kill-switch state. */
export function LiveStatus() {
  const { connected } = useLiveFeed(NO_HANDLERS)
  const health = useHealth()
  const kill = health?.kill_switch === true

  return (
    <div className="flex items-center gap-2">
      <span
        className={cn(
          "inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-[11.5px] leading-5 font-medium",
          connected
            ? "bg-long/12 text-long-ink"
            : "bg-cell text-muted-foreground"
        )}
      >
        <span
          className={cn(
            "size-1.5 shrink-0 rounded-full",
            connected ? "animate-pulse bg-long" : "bg-muted-foreground"
          )}
        />
        Live
      </span>
      <StatusPill tone={kill ? "short" : "neutral"}>
        {kill ? "Kill active" : "Kill off"}
      </StatusPill>
    </div>
  )
}

function Vital({ label, state }: { label: string; state: boolean | null }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className={cn(
          "size-1.5 shrink-0 rounded-full",
          state == null
            ? "bg-muted-foreground/40"
            : state
              ? "bg-long"
              : "bg-short"
        )}
      />
      <span className="text-[11px] text-muted-foreground">{label}</span>
    </span>
  )
}

function NavLink({ item, active }: { item: NavItem; active: boolean }) {
  return (
    <Link
      className={cn(
        "flex h-[38px] items-center gap-2.5 rounded-md px-2.5 text-[13.5px] transition-colors",
        active
          ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
          : "text-muted-foreground hover:bg-foreground/5 hover:text-foreground"
      )}
      href={item.href}
    >
      <HugeiconsIcon icon={item.icon} size={16} strokeWidth={1.8} />
      <span className="truncate">{item.label}</span>
    </Link>
  )
}

export function AppSidebar() {
  const pathname = usePathname()
  const health = useHealth()
  const [equity, setEquity] = React.useState<EquityPoint[]>([])
  const [pipeFresh, setPipeFresh] = React.useState<boolean | null>(null)

  React.useEffect(() => {
    let cancelled = false
    const tick = () => {
      api
        .equity(60)
        .then((points) => {
          if (!cancelled) setEquity(points)
        })
        .catch(() => {
          if (!cancelled) setEquity([])
        })
      api
        .news(1)
        .then((items) => {
          if (cancelled) return
          const ts = items[0]?.ts
          setPipeFresh(ts != null && Date.now() - ts < NEWS_FRESH_MS)
        })
        .catch(() => {
          if (!cancelled) setPipeFresh(null)
        })
    }
    tick()
    const id = setInterval(tick, POLL_MS)
    return () => {
      cancelled = true
      clearInterval(id)
    }
  }, [])

  const spark = React.useMemo(
    () => equity.map((p) => ({ ts: p.ts, value: p.equity })),
    [equity]
  )

  return (
    <aside className="sticky top-0 hidden h-svh w-[248px] shrink-0 flex-col gap-5 border-r border-sidebar-border bg-sidebar px-4 py-5 md:flex">
      <div className="flex items-center gap-2.5 px-1">
        <IconChip icon={BirdIcon} />
        <span className="text-sm font-semibold tracking-tight">Kestrel</span>
        <StatusPill className="ml-auto" tone="accent">
          Paper
        </StatusPill>
      </div>

      <div className="relative">
        <HugeiconsIcon
          className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted-foreground"
          icon={Search01Icon}
          size={14}
          strokeWidth={1.8}
        />
        <Input
          aria-label="Search"
          className="h-9 rounded-md border-transparent border-b-transparent bg-surface-2 pr-11 pl-9 text-[13px]"
          placeholder="Search…"
        />
        <kbd className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 font-mono text-[10px] text-muted-foreground">
          ⌘K
        </kbd>
      </div>

      <nav className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto">
        {GROUPS.map((group) => (
          <div key={group.label}>
            <div className="px-2.5 pb-1.5 text-xs font-medium text-muted-foreground">
              {group.label}
            </div>
            <div className="flex flex-col gap-0.5">
              {group.items.map((item) => (
                <NavLink
                  active={pathname === item.href}
                  item={item}
                  key={item.href}
                />
              ))}
            </div>
          </div>
        ))}
      </nav>

      <div className="flex flex-col gap-3">
        {/* the paper-mode card states a fact about the book, not an action —
            it carries the orange data hue, matching the Paper pill above. */}
        <div className="rounded-lg bg-accent-orange/10 p-3.5 ring-1 ring-accent-orange/20">
          <span className="text-xs font-medium text-accent-orange-ink">
            Paper mode
          </span>
          <div className="mt-2 flex items-baseline justify-between gap-2">
            <span className="text-xs text-muted-foreground">Bankroll</span>
            <span className="font-mono text-[15px] font-semibold">
              {usd(BANKROLL)}
            </span>
          </div>
          <TrendArea
            className="mt-2"
            data={spark}
            height={40}
            strokeWidth={1.75}
          />
          <div className="mt-3 flex items-center justify-between">
            <Vital label="Api" state={health ? health.ok : null} />
            <Vital label="Ws" state={health ? health.ws_connected : null} />
            <Vital label="Pipe" state={pipeFresh} />
          </div>
        </div>

        <div className="flex items-center justify-between px-1">
          <span className="text-xs text-muted-foreground">Theme</span>
          <ThemeToggle />
        </div>
      </div>
    </aside>
  )
}

export function MobileNav() {
  const pathname = usePathname()

  return (
    <nav className="fixed inset-x-0 bottom-0 z-40 flex h-16 items-stretch border-t border-sidebar-border bg-sidebar md:hidden">
      {ALL_ITEMS.map((item) => {
        const active = pathname === item.href
        return (
          <Link
            className={cn(
              "flex flex-1 flex-col items-center justify-center gap-1.5 transition-colors",
              active ? "text-primary-ink" : "text-muted-foreground"
            )}
            href={item.href}
            key={item.href}
          >
            <HugeiconsIcon icon={item.icon} size={18} strokeWidth={1.8} />
            <span className="text-[11px]">{item.label}</span>
          </Link>
        )
      })}
    </nav>
  )
}
