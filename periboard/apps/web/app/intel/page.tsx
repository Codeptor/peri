"use client"

import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@workspace/ui/components/card"
import { ScrollArea } from "@workspace/ui/components/scroll-area"
import { Badge } from "@workspace/ui/components/badge"
import { Pill } from "@workspace/ui/components/kibo-ui/pill"
import { Spinner } from "@workspace/ui/components/kibo-ui/spinner"
import {
  StatusBlankContainer,
  StatusBlankDescription,
  StatusBlankTitle,
} from "@workspace/ui/components/blocks/status-blank"
import { RiImageLine } from "@remixicon/react"
import { api } from "@/lib/api"
import { ago } from "@/lib/format"
import { usePoll } from "@/lib/use-poll"
import { cn } from "@workspace/ui/lib/utils"

function Pane({
  title,
  count,
  hint,
  children,
}: {
  title: string
  count?: number
  hint: string
  children: React.ReactNode
}) {
  return (
    <Card className="flex h-full min-h-0 flex-col gap-0 py-0">
      <CardHeader className="flex-row items-baseline justify-between border-b py-3 [.border-b]:pb-3">
        <CardTitle className="flex items-baseline gap-2 text-sm font-medium">
          {title}
          {count != null && (
            <span className="text-muted-foreground font-mono text-xs tabular-nums">
              {count}
            </span>
          )}
        </CardTitle>
        <span className="text-muted-foreground text-[11px]">{hint}</span>
      </CardHeader>
      <CardContent className="min-h-0 flex-1 p-0">{children}</CardContent>
    </Card>
  )
}

export default function Intel() {
  const { data: news } = usePoll(() => api.news(60), 30_000)
  const { data: tg } = usePoll(() => api.tg(120), 30_000)

  const den = tg ? [...tg].reverse() : null // newest first
  const callers = den?.filter((m) => m.is_caller).length ?? 0

  return (
    <div className="grid h-[calc(100dvh-7.5rem)] min-h-0 grid-rows-2 gap-4 lg:grid-cols-2 lg:grid-rows-1">
      <Pane
        title="News channels"
        count={news?.length}
        hint="telegram · fed into every decision"
      >
        {news == null ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : news.length === 0 ? (
          <StatusBlankContainer className="h-full">
            <StatusBlankTitle>Quiet so far</StatusBlankTitle>
            <StatusBlankDescription>
              Items appear as WatcherGuru &amp; co post.
            </StatusBlankDescription>
          </StatusBlankContainer>
        ) : (
          <ScrollArea className="h-full">
            <div className="divide-y">
              {news.map((n, i) => (
                <div key={i} className="space-y-1 px-4 py-2.5">
                  <div className="flex items-center gap-2">
                    <Pill className="h-5 px-1.5 text-[10px]">{n.source}</Pill>
                    <span className="text-muted-foreground text-[11px] tabular-nums">
                      {ago(n.ts)}
                    </span>
                  </div>
                  <p className="text-[13px] leading-snug">{n.text}</p>
                </div>
              ))}
            </div>
          </ScrollArea>
        )}
      </Pane>

      <Pane
        title="the caller group"
        count={den?.length}
        hint={`read verbatim by the model · ${callers} caller msgs`}
      >
        {den == null ? (
          <div className="flex h-full items-center justify-center">
            <Spinner />
          </div>
        ) : den.length === 0 ? (
          <StatusBlankContainer className="h-full">
            <StatusBlankTitle>No messages yet</StatusBlankTitle>
            <StatusBlankDescription>
              Group chatter and calls land here.
            </StatusBlankDescription>
          </StatusBlankContainer>
        ) : (
          <ScrollArea className="h-full">
            <div className="divide-y">
              {den.map((m) => (
                <div
                  key={m.msg_id}
                  className={cn(
                    "space-y-1 px-4 py-2.5",
                    m.is_caller &&
                      "border-l-2 border-l-amber-500 bg-amber-500/[0.06]"
                  )}
                >
                  <div className="flex items-center gap-2">
                    {m.is_caller ? (
                      <Badge className="h-5 gap-1 bg-amber-500/15 px-1.5 text-[10px] font-semibold text-amber-500">
                        CALLER · @{m.sender}
                      </Badge>
                    ) : (
                      <span className="text-muted-foreground text-xs font-medium">
                        @{m.sender ?? "anon"}
                      </span>
                    )}
                    <span className="text-muted-foreground text-[11px] tabular-nums">
                      {ago(m.ts)}
                    </span>
                  </div>
                  {m.text ? (
                    <p className="text-[13px] leading-snug whitespace-pre-wrap">
                      {m.text}
                    </p>
                  ) : null}
                  {m.image_desc ? (
                    <div className="flex gap-2 rounded border border-dashed px-2 py-1.5">
                      <RiImageLine className="text-muted-foreground mt-px size-3.5 shrink-0" />
                      <p className="text-muted-foreground text-[12px] leading-snug italic">
                        {m.image_desc}
                      </p>
                    </div>
                  ) : null}
                </div>
              ))}
            </div>
          </ScrollArea>
        )}
      </Pane>
    </div>
  )
}
