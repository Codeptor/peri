"use client"

import * as React from "react"
import { PieChart01Icon } from "@hugeicons/core-free-icons"

import type { Trade } from "@/lib/api"
import { exitMix } from "@/lib/stats"
import { SectionCard } from "@/components/blocks/section-card"
import {
  SegmentBar,
  type SegmentBarSegment,
} from "@/components/blocks/segment-bar"
import { Skeleton } from "@/components/ui/skeleton"
import { CardNote } from "@/components/overview/micro"

/** How positions leave the book: tp / sl / veto / everything else. */
export function ExitMixCard({
  trades,
  loading,
  className,
}: {
  trades: Trade[]
  loading: boolean
  className?: string
}) {
  const { segments, total } = React.useMemo(() => {
    const mix = exitMix(trades)
    const next: SegmentBarSegment[] = [
      { label: "Take profit", count: mix.tp, color: "var(--long)" },
      { label: "Stop loss", count: mix.sl, color: "var(--short)" },
      { label: "Veto close", count: mix.veto_close, color: "var(--warning)" },
      { label: "Other", count: mix.other, color: "var(--chart-6)" },
    ]
    return {
      segments: next,
      total: next.reduce((sum, s) => sum + s.count, 0),
    }
  }, [trades])

  return (
    <SectionCard
      action={<CardNote>{total} closes</CardNote>}
      className={className}
      icon={PieChart01Icon}
      title="Exit mix"
    >
      {loading ? (
        <div className="flex flex-col gap-4">
          <Skeleton className="h-2.5 w-full rounded-full" />
          <Skeleton className="h-24 w-full rounded-md" />
        </div>
      ) : (
        <SegmentBar segments={segments} />
      )}
    </SectionCard>
  )
}
