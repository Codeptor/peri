"use client"

import { ArrowUpRight01Icon, RssIcon } from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"

import type { NewsItem } from "@/lib/api"
import { relativeTime } from "@/lib/format"
import { SectionCard } from "@/components/blocks/section-card"
import { StatusPill } from "@/components/blocks/status-pill"
import { FeedEmpty, FeedSkeleton } from "@/components/intel/feed-state"
import { MarketChip, MetaChip } from "@/components/intel/meta-chip"

function NewsCard({
  item,
  filter,
  onSelectMarket,
}: {
  item: NewsItem
  filter: string | null
  onSelectMarket: (market: string) => void
}) {
  return (
    <article className="rounded-md border border-border bg-surface-2 p-4">
      <div className="flex items-center gap-2">
        <StatusPill tone="neutral">{item.source}</StatusPill>
        <span className="ml-auto shrink-0 font-mono text-[11px] text-muted-foreground">
          {relativeTime(item.ts)}
        </span>
      </div>

      {item.url ? (
        <a
          className="mt-2.5 flex items-start gap-1.5 text-[13.5px] leading-snug font-semibold transition-colors hover:text-primary-ink"
          href={item.url}
          rel="noreferrer noopener"
          target="_blank"
        >
          <span className="min-w-0">{item.title}</span>
          <HugeiconsIcon
            className="mt-0.5 shrink-0 text-muted-foreground"
            icon={ArrowUpRight01Icon}
            size={13}
            strokeWidth={1.8}
          />
        </a>
      ) : (
        <div className="mt-2.5 text-[13.5px] leading-snug font-semibold">
          {item.title}
        </div>
      )}

      <p className="mt-1.5 line-clamp-2 text-[13px] leading-relaxed text-muted-foreground">
        {item.body}
      </p>

      {item.markets.length > 0 ? (
        <div className="mt-3 flex flex-wrap items-center gap-1.5">
          {item.markets.map((m) => (
            <MarketChip
              active={filter === m}
              key={m}
              market={m}
              onSelect={onSelectMarket}
            />
          ))}
        </div>
      ) : null}
    </article>
  )
}

export type NewsFeedProps = {
  news: NewsItem[] | null
  filter: string | null
  onSelectMarket: (market: string) => void
}

/** The news pipe as it lands — tag chips drive the page filter. */
export function NewsFeed({ news, filter, onSelectMarket }: NewsFeedProps) {
  const rows =
    news == null
      ? null
      : filter == null
        ? news
        : news.filter((n) => n.markets.includes(filter))

  return (
    <SectionCard
      action={
        <MetaChip label={filter ? "Filtered" : "Items"}>
          {rows == null ? "—" : rows.length}
        </MetaChip>
      }
      className="min-w-0"
      icon={RssIcon}
      title="News wire"
    >
      {rows == null ? (
        <FeedSkeleton rows={3} />
      ) : rows.length === 0 ? (
        <FeedEmpty
          hint={
            filter
              ? `Nothing tagged ${filter} in the pipe.`
              : "No headlines ingested yet — the Telegram sidecar feeds this."
          }
          title="Pipe quiet"
        />
      ) : (
        <div className="flex max-h-[42rem] flex-col gap-3 overflow-y-auto">
          {rows.map((n) => (
            <NewsCard
              filter={filter}
              item={n}
              key={n.id}
              onSelectMarket={onSelectMarket}
            />
          ))}
        </div>
      )}
    </SectionCard>
  )
}
