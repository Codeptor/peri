"use client"

import * as React from "react"
import NumberFlow from "@number-flow/react"

import { compact, fmtPrice, relativeTime, signedPct } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetTitle,
} from "@/components/ui/sheet"
import { DeltaChip } from "@/components/blocks/delta-chip"
import { StatusPill } from "@/components/blocks/status-pill"
import { CandlePanel } from "@/components/candle-panel"

import {
  FEATURES,
  featureHue,
  type FeatureKey,
  featureRatio,
  fundingBp,
  MarketName,
  META_LABEL,
  type ScreenerRow,
  signedTextClass,
  SUB_TITLE,
  TILE_LABEL,
  titleCase,
  tintStyle,
} from "./shared"

export type MarketSheetProps = {
  market: string | null
  row: ScreenerRow | null
  maxima: Map<FeatureKey, number>
  /** top nominee score, so the score bar is scaled like the nominee cards */
  nomineeTop: number
  snapshotTs: number | null
  loading: boolean
  onClose: () => void
}

function Stat({
  label,
  value,
  className,
  style,
}: {
  label: string
  value: React.ReactNode
  className?: string
  style?: React.CSSProperties
}) {
  return (
    <div className="rounded-md bg-surface-2 px-3 py-2.5" style={style}>
      <div className={cn(TILE_LABEL, "truncate")}>{label}</div>
      <div className={cn("mt-1 font-mono text-[14px] font-medium", className)}>
        {value}
      </div>
    </div>
  )
}

/** Right-hand drill-down: candles + the full snapshot row, deep-linked by `?m=`. */
export function MarketSheet({
  market,
  row,
  maxima,
  nomineeTop,
  snapshotTs,
  loading,
  onClose,
}: MarketSheetProps) {
  const data = row?.data ?? null
  const nominee = row?.nominee ?? null
  const mark = row?.mark ?? null

  return (
    <Sheet
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
      open={market != null}
    >
      <SheetContent
        className="gap-0 p-0"
        side="right"
        style={{ width: "100%", maxWidth: 560 }}
      >
        {market ? (
          <>
            <div className="border-b border-border px-6 pt-6 pr-14 pb-5">
              <div className="flex items-center gap-2.5">
                {/* the title IS the ticker — an id, so mono survives (amendment §2). */}
                <SheetTitle className="font-mono text-[16px]">
                  <MarketName market={market} />
                </SheetTitle>
                {nominee ? (
                  <StatusPill
                    tone={nominee.side_hint === "long" ? "long" : "short"}
                  >
                    {titleCase(nominee.side_hint)}
                  </StatusPill>
                ) : null}
              </div>
              <SheetDescription className={cn(META_LABEL, "mt-1.5")}>
                {data ? (
                  <>
                    Snapshot{" "}
                    <span className="font-mono">
                      {relativeTime(snapshotTs)}
                    </span>
                  </>
                ) : loading ? (
                  "Loading snapshot"
                ) : (
                  "Not in the current snapshot"
                )}
              </SheetDescription>

              <div className="mt-4 flex items-end gap-3">
                <span className="font-mono text-[28px] leading-8 font-semibold tracking-tight">
                  {mark == null ? (
                    <span className="text-muted-foreground">—</span>
                  ) : (
                    <NumberFlow
                      format={{
                        minimumFractionDigits: 2,
                        maximumFractionDigits: mark >= 1000 ? 2 : 4,
                      }}
                      value={mark}
                    />
                  )}
                </span>
                <span className="pb-1.5">
                  <DeltaChip pct={row?.chg24h ?? null} />
                </span>
                <span className={cn(META_LABEL, "ml-auto pb-2")}>Mark</span>
              </div>
            </div>

            <div className="flex flex-1 flex-col gap-5 overflow-y-auto px-6 py-5">
              <div className="rounded-md bg-surface-2 p-2">
                <CandlePanel coin={market} height={240} interval="5m" />
              </div>

              {nominee ? (
                <div className="rounded-md border border-accent-orange/25 bg-accent-orange/10 px-4 py-3.5">
                  <div className="flex items-baseline gap-2">
                    {/* the ink, not the raw hue: orange sits near 2.2:1 on this tint in light mode */}
                    <span className="font-mono text-[20px] leading-none font-semibold text-accent-orange-ink">
                      {nominee.score.toFixed(2)}
                    </span>
                    <span className={META_LABEL}>nominee score</span>
                    <span className="ml-auto font-mono text-[11px] text-muted-foreground">
                      {relativeTime(nominee.ts)}
                    </span>
                  </div>
                  <div className="mt-3 h-1 w-full overflow-hidden rounded-full bg-cell">
                    <div
                      className="h-full rounded-full bg-accent-orange"
                      style={{
                        width: `${Math.min(100, (Math.abs(nominee.score) / Math.max(nomineeTop, 0.0001)) * 100)}%`,
                      }}
                    />
                  </div>
                </div>
              ) : null}

              <div>
                <div className={cn(SUB_TITLE, "mb-2.5")}>Book</div>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                  <Stat label="Mid" value={fmtPrice(data?.mid)} />
                  <Stat label="Oracle" value={fmtPrice(data?.oracle)} />
                  <Stat label="Prev day" value={fmtPrice(data?.prev_day_px)} />
                  <Stat label="Funding bp" value={fundingBp(data?.funding)} />
                  <Stat
                    label="Open interest"
                    value={compact(data?.open_interest)}
                  />
                  <Stat label="24h vol" value={compact(data?.day_ntl_vlm)} />
                </div>
              </div>

              <div>
                <div className={cn(SUB_TITLE, "mb-2.5")}>Features</div>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
                  {FEATURES.map((meta) => {
                    const features = data?.features ?? null
                    const value = features ? features[meta.key] : null
                    return (
                      <Stat
                        className={
                          meta.signed && value != null
                            ? signedTextClass(value)
                            : undefined
                        }
                        key={meta.key}
                        label={meta.head}
                        style={
                          value == null
                            ? undefined
                            : tintStyle(
                                featureRatio(meta, value, maxima),
                                featureHue(meta, value),
                                "var(--surface-2)"
                              )
                        }
                        value={value == null ? "—" : meta.format(value)}
                      />
                    )
                  })}
                </div>
              </div>

              <div className="flex items-center gap-2 border-t border-border pt-4">
                <span className={META_LABEL}>24h change</span>
                <span
                  className={cn(
                    "ml-auto font-mono text-[13px]",
                    signedTextClass(row?.chg24h ?? null)
                  )}
                >
                  {signedPct(row?.chg24h ?? null)}
                </span>
              </div>
            </div>
          </>
        ) : null}
      </SheetContent>
    </Sheet>
  )
}
