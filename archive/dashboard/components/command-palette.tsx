"use client"

import * as React from "react"
import { useRouter } from "next/navigation"
import { Dialog } from "@base-ui/react/dialog"
import {
  Analytics01Icon,
  ChartLineData01Icon,
  DashboardSquare01Icon,
  GridViewIcon,
  Moon02Icon,
  RefreshIcon,
  SatelliteIcon,
  Search01Icon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react"
import { useTheme } from "next-themes"

import { api, type MarketRow } from "@/lib/api"
import { fmtPrice } from "@/lib/format"
import { cn } from "@/lib/utils"

/**
 * The sidebar search field is the palette's trigger, and it lives in a file this
 * component does not own — so the trigger is bound by delegation instead of by a
 * prop. Anything carrying `data-command-trigger` opens the palette too, which is
 * the seam to use once the field can be marked directly.
 */
const TRIGGER_SELECTOR =
  '[data-command-trigger], aside input[aria-label="Search"]'

/**
 * The universe runs to dozens of markets, so the resting list shows only the head
 * of it — the palette at rest is a menu, not a dump. A query lifts the cap
 * entirely, which is what makes *every* tracked market reachable by typing.
 */
const MARKET_ROWS_IDLE = 8

type Group = "Pages" | "Markets" | "Actions"

type Item = {
  id: string
  group: Group
  label: string
  /** tickers and ids render mono; prose renders sans */
  mono?: boolean
  /** right-aligned annotation — a route, a price, what the action does */
  hint?: string
  /** matched but never shown, so "intel" finds "Wire" */
  keywords?: string
  icon: IconSvgElement
  run: () => void
}

type Scored = { item: Item; score: number; matched: number[]; index: number }

/**
 * Subsequence match, greedy left-to-right. Consecutive runs and word starts score
 * highest, gaps cost, and a long label pays for its length so an exact-ish hit
 * outranks a long string that merely contains the same letters.
 */
function fuzzy(
  query: string,
  text: string
): { score: number; matched: number[] } | null {
  if (query.length === 0) return { score: 0, matched: [] }
  const q = query.toLowerCase()
  const t = text.toLowerCase()
  const matched: number[] = []
  let score = 0
  let qi = 0
  let prev = -2

  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (t[i] !== q[qi]) continue
    let bonus = 0
    if (i === prev + 1) bonus += 8
    if (i === 0) bonus += 12
    else if (!/[a-z0-9]/.test(t[i - 1])) bonus += 6
    score += 10 + bonus - Math.min(6, i - prev - 1)
    matched.push(i)
    prev = i
    qi += 1
  }
  if (qi < q.length) return null
  return { score: score - Math.round(text.length / 6), matched }
}

/** Query hits inside the label are lit in the action hue; the rest stays inherited. */
function Highlighted({ text, matched }: { text: string; matched: number[] }) {
  if (matched.length === 0) return <>{text}</>
  const set = new Set(matched)
  return (
    <>
      {Array.from(text, (ch, i) =>
        set.has(i) ? (
          <span className="font-semibold text-primary-ink" key={i}>
            {ch}
          </span>
        ) : (
          <span key={i}>{ch}</span>
        )
      )}
    </>
  )
}

function Kbd({ children }: { children: React.ReactNode }) {
  return (
    <kbd className="rounded-[5px] bg-cell px-1.5 py-0.5 font-mono text-[10px] leading-4 text-muted-foreground">
      {children}
    </kbd>
  )
}

/**
 * ⌘K palette: every page, every tracked market, and the two desk actions, behind
 * one subsequence matcher. The dialog primitive supplies the focus trap, escape
 * and scroll lock; list navigation is ours because the rows are not focusable —
 * focus stays in the input the whole time, which is what makes typing continuous.
 */
export function CommandPalette() {
  const router = useRouter()
  const { resolvedTheme, setTheme } = useTheme()

  const [open, setOpen] = React.useState(false)
  const [query, setQuery] = React.useState("")
  const [active, setActive] = React.useState(0)
  const [markets, setMarkets] = React.useState<MarketRow[] | null>(null)

  const inputRef = React.useRef<HTMLInputElement | null>(null)
  const listRef = React.useRef<HTMLDivElement | null>(null)
  const loadingRef = React.useRef(false)
  const pointerRef = React.useRef({ x: -1, y: -1 })

  /** Every entry point comes through here, so the palette always opens empty. */
  const openPalette = React.useCallback(() => {
    setQuery("")
    setActive(0)
    setOpen(true)
  }, [])

  const close = React.useCallback(() => setOpen(false), [])

  React.useEffect(() => {
    // Capture phase + preventDefault: the field must never take focus and show a
    // caret it cannot use — the palette owns the typing from the first press.
    const onPointerDown = (e: PointerEvent) => {
      if (!(e.target instanceof Element)) return
      if (!e.target.closest(TRIGGER_SELECTOR)) return
      e.preventDefault()
      openPalette()
    }
    // Keyboard users tab into the field; hand them the palette instead.
    const onFocusIn = (e: FocusEvent) => {
      if (!(e.target instanceof Element)) return
      const trigger = e.target.closest(TRIGGER_SELECTOR)
      if (!trigger) return
      if (trigger instanceof HTMLElement) trigger.blur()
      openPalette()
    }

    document.addEventListener("pointerdown", onPointerDown, true)
    document.addEventListener("focusin", onFocusIn)
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true)
      document.removeEventListener("focusin", onFocusIn)
    }
  }, [openPalette])

  // Re-bound per open state instead of reading a mirror ref: one listener swap
  // per toggle is cheaper than the class of bug a stale mirror invites.
  React.useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.key.toLowerCase() !== "k") return
      e.preventDefault()
      if (open) close()
      else openPalette()
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [open, openPalette, close])

  // The universe is only fetched when the palette is actually opened, and a failed
  // fetch leaves it null so the next open retries instead of caching an empty list.
  React.useEffect(() => {
    if (!open || markets != null || loadingRef.current) return
    loadingRef.current = true
    let cancelled = false
    api
      .snapshot()
      .then((snap) => {
        if (!cancelled) setMarkets(snap.markets)
      })
      .catch(() => {})
      .finally(() => {
        loadingRef.current = false
      })
    return () => {
      cancelled = true
    }
  }, [open, markets])

  const go = React.useCallback(
    (href: string) => {
      close()
      router.push(href)
    },
    [close, router]
  )

  const items = React.useMemo<Item[]>(() => {
    const pages: Item[] = [
      {
        id: "page:/",
        group: "Pages",
        label: "Overview",
        hint: "/",
        keywords: "desk equity home dashboard kpi",
        icon: DashboardSquare01Icon,
        run: () => go("/"),
      },
      {
        id: "page:/markets",
        group: "Pages",
        label: "Markets",
        hint: "/markets",
        keywords: "screener universe nominees crowding",
        icon: Analytics01Icon,
        run: () => go("/markets"),
      },
      {
        id: "page:/positions",
        group: "Pages",
        label: "Positions",
        hint: "/positions",
        keywords: "book open stops targets history fills",
        icon: ChartLineData01Icon,
        run: () => go("/positions"),
      },
      {
        id: "page:/intel",
        group: "Pages",
        label: "Wire",
        hint: "/intel",
        keywords: "intel decisions news analyst gates analytics",
        icon: SatelliteIcon,
        run: () => go("/intel"),
      },
    ]

    const marketItems: Item[] = (markets ?? []).map((m) => ({
      id: `market:${m.market}`,
      group: "Markets",
      label: m.market,
      mono: true,
      hint: fmtPrice(m.mark),
      keywords: "market jump screener",
      icon: GridViewIcon,
      run: () => go(`/markets?m=${encodeURIComponent(m.market)}`),
    }))

    const actions: Item[] = [
      {
        id: "action:theme",
        group: "Actions",
        label: "Toggle theme",
        hint: resolvedTheme === "dark" ? "To light" : "To dark",
        keywords: "dark light appearance colour color",
        icon: Moon02Icon,
        run: () => {
          close()
          setTheme(resolvedTheme === "dark" ? "light" : "dark")
        },
      },
      {
        id: "action:refresh",
        group: "Actions",
        label: "Refresh data",
        hint: "Re-render route",
        keywords: "reload poll kestreld snapshot",
        icon: RefreshIcon,
        run: () => {
          close()
          setMarkets(null)
          router.refresh()
        },
      },
    ]

    return [...pages, ...marketItems, ...actions]
  }, [markets, resolvedTheme, close, go, router, setTheme])

  const { sections, flat } = React.useMemo(() => {
    const q = query.trim()
    const scored: Scored[] = []
    items.forEach((item, index) => {
      const direct = fuzzy(q, item.label)
      if (direct) {
        // a label hit always outranks a hit that only the hidden keywords saw
        scored.push({
          item,
          score: direct.score + 20,
          matched: direct.matched,
          index,
        })
        return
      }
      const blob = `${item.label} ${item.hint ?? ""} ${item.keywords ?? ""}`
      const alt = fuzzy(q, blob)
      if (alt) scored.push({ item, score: alt.score, matched: [], index })
    })
    scored.sort((a, b) => b.score - a.score || a.index - b.index)

    const order: Group[] = []
    const byGroup = new Map<Group, Scored[]>()
    for (const row of scored) {
      const bucket = byGroup.get(row.item.group)
      if (bucket) bucket.push(row)
      else {
        byGroup.set(row.item.group, [row])
        order.push(row.item.group)
      }
    }

    const nextSections = order.map((group) => {
      const rows = byGroup.get(group) ?? []
      return {
        group,
        total: rows.length,
        rows:
          group === "Markets" && q === ""
            ? rows.slice(0, MARKET_ROWS_IDLE)
            : rows,
      }
    })

    let slot = 0
    const nextFlat: Scored[] = []
    const positioned = nextSections.map((section) => ({
      group: section.group,
      total: section.total,
      rows: section.rows.map((row) => {
        nextFlat.push(row)
        return { ...row, position: slot++ }
      }),
    }))
    return { sections: positioned, flat: nextFlat }
  }, [items, query])

  // Clamped on read rather than corrected in an effect: a query that shortens the
  // list must not cost a second render pass just to pull the cursor back in range.
  const cursor = flat.length === 0 ? -1 : Math.min(active, flat.length - 1)

  React.useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(
      '[data-active="true"]'
    )
    el?.scrollIntoView({ block: "nearest" })
  }, [cursor, query])

  /**
   * Scrolling a row under a stationary pointer fires pointermove, which would
   * yank the selection back the instant an arrow key moved it. Only real cursor
   * travel counts as a hover.
   */
  const onHover = (e: React.PointerEvent, position: number) => {
    const last = pointerRef.current
    if (e.clientX === last.x && e.clientY === last.y) return
    pointerRef.current = { x: e.clientX, y: e.clientY }
    setActive(position)
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (flat.length === 0) return
    if (e.key === "ArrowDown") {
      e.preventDefault()
      setActive((cursor + 1) % flat.length)
    } else if (e.key === "ArrowUp") {
      e.preventDefault()
      setActive((cursor - 1 + flat.length) % flat.length)
    } else if (e.key === "Home") {
      e.preventDefault()
      setActive(0)
    } else if (e.key === "End") {
      e.preventDefault()
      setActive(flat.length - 1)
    } else if (e.key === "Enter") {
      e.preventDefault()
      flat[cursor]?.item.run()
    }
  }

  return (
    <Dialog.Root onOpenChange={setOpen} open={open}>
      <Dialog.Portal>
        <Dialog.Backdrop className="fixed inset-0 z-50 bg-black/25 transition-opacity duration-150 data-ending-style:opacity-0 data-starting-style:opacity-0 supports-backdrop-filter:backdrop-blur-sm" />
        <Dialog.Popup
          className="fixed top-[12vh] left-1/2 z-50 flex w-[min(38rem,calc(100vw-2rem))] -translate-x-1/2 flex-col overflow-hidden rounded-xl border border-border bg-popover text-popover-foreground shadow-lg transition duration-150 data-ending-style:scale-[0.98] data-ending-style:opacity-0 data-starting-style:scale-[0.98] data-starting-style:opacity-0"
          initialFocus={inputRef}
          onKeyDown={onKeyDown}
        >
          <Dialog.Title className="sr-only">Command palette</Dialog.Title>

          <div className="flex items-center gap-2.5 border-b border-border px-4">
            <HugeiconsIcon
              className="shrink-0 text-muted-foreground"
              icon={Search01Icon}
              size={15}
              strokeWidth={1.8}
            />
            <input
              aria-label="Search pages, markets and actions"
              autoComplete="off"
              className="h-12 min-w-0 flex-1 bg-transparent text-[14px] outline-none placeholder:text-muted-foreground"
              onChange={(e) => {
                setQuery(e.target.value)
                setActive(0)
              }}
              placeholder="Jump to a page, a market, or run an action…"
              ref={inputRef}
              spellCheck={false}
              value={query}
            />
            <Kbd>Esc</Kbd>
          </div>

          <div
            className="max-h-[min(24rem,58vh)] overflow-y-auto p-2"
            ref={listRef}
          >
            {flat.length === 0 ? (
              <p className="px-2.5 py-8 text-center text-[13px] text-muted-foreground">
                No match for{" "}
                <span className="font-mono text-foreground">{query}</span>
              </p>
            ) : (
              sections.map((section) => (
                <div key={section.group}>
                  <div className="flex items-baseline gap-2 px-2.5 pt-2.5 pb-1.5 text-[11px] font-medium text-muted-foreground">
                    <span>{section.group}</span>
                    {section.rows.length < section.total ? (
                      <span className="font-mono font-normal">
                        {section.rows.length}/{section.total} · type to widen
                      </span>
                    ) : null}
                  </div>
                  {section.rows.map((row) => {
                    const on = row.position === cursor
                    return (
                      <button
                        className={cn(
                          "flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left transition-colors",
                          on
                            ? "bg-primary/12 ring-1 ring-primary/20"
                            : "hover:bg-foreground/5"
                        )}
                        data-active={on}
                        key={row.item.id}
                        onClick={() => row.item.run()}
                        // the row must not steal the caret — typing continues
                        // straight through a mis-click
                        onMouseDown={(e) => e.preventDefault()}
                        onPointerMove={(e) => onHover(e, row.position)}
                        tabIndex={-1}
                        type="button"
                      >
                        <HugeiconsIcon
                          className={cn(
                            "shrink-0",
                            on ? "text-primary-ink" : "text-muted-foreground"
                          )}
                          icon={row.item.icon}
                          size={15}
                          strokeWidth={1.8}
                        />
                        <span
                          className={cn(
                            "truncate text-[13.5px]",
                            row.item.mono && "font-mono font-medium"
                          )}
                        >
                          <Highlighted
                            matched={row.matched}
                            text={row.item.label}
                          />
                        </span>
                        {row.item.hint ? (
                          <span
                            className={cn(
                              "ml-auto shrink-0 truncate text-[11.5px] text-muted-foreground",
                              row.item.group === "Markets" && "font-mono"
                            )}
                          >
                            {row.item.hint}
                          </span>
                        ) : null}
                        <span
                          className={cn(
                            "shrink-0 font-mono text-[11px] text-primary-ink",
                            on ? "opacity-100" : "opacity-0",
                            row.item.hint ? "" : "ml-auto"
                          )}
                        >
                          ↵
                        </span>
                      </button>
                    )
                  })}
                </div>
              ))
            )}
          </div>

          <div className="flex items-center gap-3 border-t border-border px-4 py-2.5 text-[11px] text-muted-foreground">
            <span className="inline-flex items-center gap-1.5">
              <Kbd>↑</Kbd>
              <Kbd>↓</Kbd> navigate
            </span>
            <span className="inline-flex items-center gap-1.5">
              <Kbd>↵</Kbd> open
            </span>
            <span className="ml-auto inline-flex items-center gap-1.5">
              <Kbd>⌘K</Kbd> anywhere
            </span>
          </div>
        </Dialog.Popup>
      </Dialog.Portal>
    </Dialog.Root>
  )
}
