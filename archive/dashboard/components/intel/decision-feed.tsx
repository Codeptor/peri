"use client"

import { Idea01Icon } from "@hugeicons/core-free-icons"

import type { Decision } from "@/lib/api"
import { SectionCard } from "@/components/blocks/section-card"
import { DecisionCard } from "@/components/intel/decision-card"
import { FeedEmpty, FeedSkeleton } from "@/components/intel/feed-state"
import { decisionKey } from "@/components/intel/intel-utils"
import { MetaChip } from "@/components/intel/meta-chip"

export type DecisionFeedProps = {
  decisions: Decision[] | null
  filter: string | null
  onSelectMarket: (market: string) => void
}

/** The analyst's running log — newest first, scrolls inside the card. */
export function DecisionFeed({
  decisions,
  filter,
  onSelectMarket,
}: DecisionFeedProps) {
  const rows =
    decisions == null
      ? null
      : filter == null
        ? decisions
        : decisions.filter((d) => d.market === filter)

  return (
    <SectionCard
      action={
        <MetaChip label={filter ? "Filtered" : "Calls"}>
          {rows == null ? "—" : rows.length}
        </MetaChip>
      }
      className="min-w-0"
      icon={Idea01Icon}
      title="Decisions"
    >
      {rows == null ? (
        <FeedSkeleton rows={3} />
      ) : rows.length === 0 ? (
        <FeedEmpty
          hint={
            filter
              ? `No analyst calls logged for ${filter}.`
              : "The analyst has not logged a call yet — nominees are still cooking."
          }
          title="No decisions"
        />
      ) : (
        <div className="flex max-h-[42rem] flex-col gap-3 overflow-y-auto">
          {rows.map((d) => (
            <DecisionCard
              active={filter === d.market}
              decision={d}
              key={decisionKey(d)}
              onSelectMarket={onSelectMarket}
            />
          ))}
        </div>
      )}
    </SectionCard>
  )
}
