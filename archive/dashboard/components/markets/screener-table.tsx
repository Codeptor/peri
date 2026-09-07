"use client"

import * as React from "react"
import {
  ArrowDown01Icon,
  ArrowUp01Icon,
  ArrowUpDownIcon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import { compact, fmtPrice, signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { StatusPill } from "@/components/blocks/status-pill"

import {
  FEATURES,
  featureHue,
  type FeatureKey,
  featureRatio,
  fundingBp,
  MarketName,
  type ScreenerRow,
  signedTextClass,
  tintStyle,
} from "./shared"

export type ScreenerTableProps = {
  rows: ScreenerRow[]
  /** column maxima from the whole universe — filtering must not move the heat */
  maxima: Map<FeatureKey, number>
  loading: boolean
  error: string | null
  selected: string | null
  onSelect: (market: string) => void
}

type SortKey =
  | "market"
  | "mark"
  | "chg24h"
  | "funding"
  | "open_interest"
  | "day_ntl_vlm"
  | (typeof FEATURES)[number]["key"]

type SortState = { key: SortKey; dir: "asc" | "desc" }

const FEATURE_KEYS = new Set<string>(FEATURES.map((f) => f.key))

function sortValue(row: ScreenerRow, key: SortKey): number | string | null {
  if (key === "market") return row.market
  if (key === "mark") return row.mark
  if (key === "chg24h") return row.chg24h
  if (FEATURE_KEYS.has(key)) {
    const f = row.data.features
    return f ? f[key as keyof typeof f] : null
  }
  return row.data[key as "funding" | "open_interest" | "day_ntl_vlm"]
}

/** Nulls sink to the bottom whichever way the column is pointing. */
function compareRows(a: ScreenerRow, b: ScreenerRow, sort: SortState): number {
  const av = sortValue(a, sort.key)
  const bv = sortValue(b, sort.key)
  if (av == null && bv == null) return 0
  if (av == null) return 1
  if (bv == null) return -1
  const sign = sort.dir === "asc" ? 1 : -1
  if (typeof av === "string" || typeof bv === "string") {
    return sign * String(av).localeCompare(String(bv))
  }
  return sign * (av - bv)
}

function SortHead({
  align = "right",
  children,
  className,
  columnKey,
  onSort,
  sort,
}: {
  align?: "left" | "right"
  children: React.ReactNode
  className?: string
  columnKey: SortKey
  onSort: (key: SortKey) => void
  sort: SortState
}) {
  const active = sort.key === columnKey
  const icon = !active
    ? ArrowUpDownIcon
    : sort.dir === "asc"
      ? ArrowUp01Icon
      : ArrowDown01Icon
  return (
    <TableHead
      aria-sort={
        active ? (sort.dir === "asc" ? "ascending" : "descending") : "none"
      }
      className={cn(
        "h-9 px-3 text-xs font-medium tracking-normal normal-case",
        align === "right" ? "text-right" : "text-left",
        className
      )}
    >
      <button
        className={cn(
          "group inline-flex items-center gap-1 transition-colors hover:text-foreground",
          active && "text-foreground"
        )}
        onClick={() => onSort(columnKey)}
        type="button"
      >
        {children}
        <HugeiconsIcon
          className={cn(
            active
              ? "text-primary-ink"
              : "opacity-0 transition-opacity group-hover:opacity-50"
          )}
          icon={icon}
          size={12}
          strokeWidth={2.2}
        />
      </button>
    </TableHead>
  )
}

/** Mark price with a brief tinted flash on each tick — subtle, 600ms. */
function PriceCell({ value }: { value: number }) {
  const previous = React.useRef(value)
  const [dir, setDir] = React.useState<"up" | "down" | null>(null)

  React.useEffect(() => {
    if (value === previous.current) return
    const next = value > previous.current ? "up" : "down"
    previous.current = value
    setDir(next)
    const timer = setTimeout(() => setDir(null), 600)
    return () => clearTimeout(timer)
  }, [value])

  return (
    <span
      className={cn(
        "font-mono transition-colors duration-500",
        dir === "up" && "text-long-ink",
        dir === "down" && "text-short-ink"
      )}
    >
      {fmtPrice(value)}
    </span>
  )
}

const NUM_CELL = "h-11 px-3 text-right font-mono text-[13px]"

/** The universe, sortable, with feature cells washed by their own extremes. */
export function ScreenerTable({
  rows,
  maxima,
  loading,
  error,
  selected,
  onSelect,
}: ScreenerTableProps) {
  const [sort, setSort] = React.useState<SortState>({
    key: "day_ntl_vlm",
    dir: "desc",
  })

  const onSort = React.useCallback((key: SortKey) => {
    setSort((prev) =>
      prev.key === key
        ? { key, dir: prev.dir === "asc" ? "desc" : "asc" }
        : { key, dir: key === "market" ? "asc" : "desc" }
    )
  }, [])

  const sorted = React.useMemo(
    () => [...rows].sort((a, b) => compareRows(a, b, sort)),
    [rows, sort]
  )

  const columnCount = 6 + FEATURES.length

  return (
    <Table>
      <TableHeader>
        <TableRow className="hover:bg-transparent">
          <SortHead align="left" columnKey="market" onSort={onSort} sort={sort}>
            Market
          </SortHead>
          <SortHead columnKey="mark" onSort={onSort} sort={sort}>
            Mark
          </SortHead>
          <SortHead columnKey="chg24h" onSort={onSort} sort={sort}>
            24h chg
          </SortHead>
          {FEATURES.map((meta) => (
            <SortHead
              columnKey={meta.key}
              key={meta.key}
              onSort={onSort}
              sort={sort}
            >
              {meta.head}
            </SortHead>
          ))}
          <SortHead
            className="hidden xl:table-cell"
            columnKey="funding"
            onSort={onSort}
            sort={sort}
          >
            Funding bp
          </SortHead>
          <SortHead
            className="hidden lg:table-cell"
            columnKey="open_interest"
            onSort={onSort}
            sort={sort}
          >
            Open interest
          </SortHead>
          <SortHead columnKey="day_ntl_vlm" onSort={onSort} sort={sort}>
            24h vol
          </SortHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {loading && rows.length === 0
          ? [0, 1, 2, 3, 4, 5].map((i) => (
              <TableRow
                className="h-11 border-b-0 hover:bg-transparent"
                key={i}
              >
                <TableCell className="h-11 px-3" colSpan={columnCount}>
                  <Skeleton className="h-4 w-full rounded-sm" />
                </TableCell>
              </TableRow>
            ))
          : null}

        {!loading && sorted.length === 0 ? (
          <TableRow className="border-b-0 hover:bg-transparent">
            <TableCell
              className="h-24 px-3 text-center text-[13px] text-muted-foreground"
              colSpan={columnCount}
            >
              {error ?? "No markets match this filter."}
            </TableCell>
          </TableRow>
        ) : null}

        {sorted.map((row) => {
          const f = row.data.features
          return (
            <TableRow
              className={cn(
                "h-11 cursor-pointer border-b-0 transition-colors hover:bg-cell",
                selected === row.market && "bg-primary/12 hover:bg-primary/16"
              )}
              key={row.market}
              onClick={() => onSelect(row.market)}
            >
              <TableCell className="h-11 px-3">
                <span className="flex items-center gap-2">
                  <button
                    className="text-[13px] transition-colors hover:text-primary-ink"
                    onClick={(event) => {
                      event.stopPropagation()
                      onSelect(row.market)
                    }}
                    type="button"
                  >
                    <MarketName market={row.market} />
                  </button>
                  {row.nominee ? (
                    <StatusPill tone="accent">Nominee</StatusPill>
                  ) : null}
                </span>
              </TableCell>

              <TableCell className={NUM_CELL}>
                <PriceCell value={row.mark} />
              </TableCell>

              <TableCell className={cn(NUM_CELL, signedTextClass(row.chg24h))}>
                {signedPct(row.chg24h)}
              </TableCell>

              {FEATURES.map((meta) => {
                if (!f) {
                  return (
                    <TableCell
                      className={cn(NUM_CELL, "text-muted-foreground")}
                      key={meta.key}
                    >
                      —
                    </TableCell>
                  )
                }
                const value = f[meta.key]
                const ratio = featureRatio(meta, value, maxima)
                return (
                  <TableCell
                    className={cn(
                      NUM_CELL,
                      meta.signed && signedTextClass(value)
                    )}
                    key={meta.key}
                    style={tintStyle(ratio, featureHue(meta, value))}
                  >
                    {meta.format(value)}
                  </TableCell>
                )
              })}

              <TableCell className={cn(NUM_CELL, "hidden xl:table-cell")}>
                {fundingBp(row.data.funding)}
              </TableCell>
              <TableCell className={cn(NUM_CELL, "hidden lg:table-cell")}>
                {compact(row.data.open_interest)}
              </TableCell>
              <TableCell className={NUM_CELL}>
                {compact(row.data.day_ntl_vlm)}
              </TableCell>
            </TableRow>
          )
        })}
      </TableBody>
    </Table>
  )
}
