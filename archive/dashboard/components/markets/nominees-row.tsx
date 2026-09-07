"use client"

import type { Nominee } from "@/lib/api"
import { signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"
import { Skeleton } from "@/components/ui/skeleton"
import { HUE_TINT } from "@/components/blocks/icon-chip"
import { StatusPill } from "@/components/blocks/status-pill"

import {
  EmptyNote,
  MarketName,
  META_LABEL,
  signedFixed,
  signedTextClass,
  TILE_LABEL,
  titleCase,
} from "./shared"

export type NomineesRowProps = {
  nominees: Nominee[]
  loading: boolean
  selected: string | null
  onSelect: (market: string) => void
}

type Chip = { label: string; value: string; className?: string }

function chipsFor(nominee: Nominee): Chip[] {
  const f = nominee.features
  return [
    {
      label: "1h ret",
      value: signedPct(f.r1h),
      className: signedTextClass(f.r1h),
    },
    { label: "Funding z", value: signedFixed(f.funding_z) },
    { label: "Range", value: `${Math.round(f.range_pos * 100)}%` },
    { label: "Vol 1h", value: f.vol1h.toFixed(2) },
  ]
}

/** Ranked nominee cards: rank chip, market, side, score + bar, feature chips. */
export function NomineesRow({
  nominees,
  loading,
  selected,
  onSelect,
}: NomineesRowProps) {
  if (loading && nominees.length === 0) {
    return (
      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
        {[0, 1, 2].map((i) => (
          <Skeleton className="h-[160px] rounded-md" key={i} />
        ))}
      </div>
    )
  }

  if (nominees.length === 0) {
    return (
      <EmptyNote>
        No nominees in the latest scan — the analyst found nothing worth a look.
      </EmptyNote>
    )
  }

  const ranked = [...nominees].sort((a, b) => b.score - a.score)
  const top = Math.max(...ranked.map((n) => Math.abs(n.score)), 0.0001)

  return (
    <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
      {ranked.map((nominee, i) => {
        const active = selected === nominee.market
        return (
          <button
            className={cn(
              // selection is carried by the border + ring, never by swapping the
              // ground: a primary tint over the light card reads *lighter* than
              // the surface-2 tile it replaces, inverting the intended emphasis.
              "flex flex-col gap-4 rounded-md border bg-surface-2 p-4 text-left transition-colors",
              active
                ? "border-primary/60 ring-1 ring-primary/25"
                : "border-border hover:border-primary/30"
            )}
            key={`${nominee.market}-${nominee.ts}`}
            onClick={() => onSelect(nominee.market)}
            type="button"
          >
            <div className="flex items-center gap-2.5">
              <span
                className={cn(
                  "grid size-7 shrink-0 place-items-center rounded-[0.625rem] font-mono text-[11px] font-semibold",
                  i === 0 ? HUE_TINT.accent : HUE_TINT.neutral
                )}
              >
                {i + 1}
              </span>
              <MarketName
                className="truncate text-[14px]"
                market={nominee.market}
              />
              <StatusPill
                className="ml-auto"
                tone={nominee.side_hint === "long" ? "long" : "short"}
              >
                {titleCase(nominee.side_hint)}
              </StatusPill>
            </div>

            <div>
              <div className="flex items-baseline gap-2">
                <span className="font-mono text-[22px] leading-none font-semibold tracking-tight">
                  {nominee.score.toFixed(2)}
                </span>
                <span className={META_LABEL}>score</span>
              </div>
              <div className="mt-3 h-1 w-full overflow-hidden rounded-full bg-cell">
                <div
                  className="h-full rounded-full bg-accent-orange"
                  style={{
                    width: `${Math.min(100, (Math.abs(nominee.score) / top) * 100)}%`,
                  }}
                />
              </div>
            </div>

            <div className="grid grid-cols-4 gap-1.5">
              {chipsFor(nominee).map((chip) => (
                <div
                  className="rounded-sm bg-cell px-2 py-1.5"
                  key={chip.label}
                >
                  <div className={cn(TILE_LABEL, "truncate")}>{chip.label}</div>
                  <div
                    className={cn(
                      "mt-0.5 truncate font-mono text-[12px] font-medium",
                      chip.className
                    )}
                  >
                    {chip.value}
                  </div>
                </div>
              ))}
            </div>
          </button>
        )
      })}
    </div>
  )
}
