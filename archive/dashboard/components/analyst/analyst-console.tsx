"use client"

import * as React from "react"
import { useChat } from "@ai-sdk/react"
import {
  RefreshIcon,
  SentIcon,
  SparklesIcon,
  StopIcon,
} from "@hugeicons/core-free-icons"
import { HugeiconsIcon } from "@hugeicons/react"
import { DefaultChatTransport, type UIMessage } from "ai"
import { Streamdown } from "streamdown"

import {
  api,
  isFixtures,
  type AnalystAction,
  type AnalystCall,
  type AnalystChatMessage,
  type AnalystLeaderboardRow,
} from "@/lib/api"
import { cn } from "@/lib/utils"
import {
  AnalystChip,
  analystLabel,
  analystShortName,
} from "@/components/analyst/analyst-chip"
import { PageHeader } from "@/components/blocks/page-header"
import { StatusPill } from "@/components/blocks/status-pill"
import { Button } from "@/components/ui/button"

type AnalystUIMessage = UIMessage<{ ts: number }, { action: AnalystAction }>

const CHAT_API = "/traderd/api/analyst/chat"

function now() {
  return globalThis.Date.now()
}

function ist(ts: number) {
  return new Intl.DateTimeFormat("en-IN", {
    timeZone: "Asia/Kolkata",
    day: "2-digit",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    hour12: true,
  }).format(ts)
}

function actionFrom(message: AnalystChatMessage): AnalystAction | null {
  if (!message.action_json) return null
  try {
    return JSON.parse(message.action_json) as AnalystAction
  } catch {
    return null
  }
}

function historyToMessages(history: AnalystChatMessage[]): AnalystUIMessage[] {
  return history.map((message, index) => {
    const action = actionFrom(message)
    return {
      id: `history-${message.ts}-${index}`,
      role: message.role === "user" ? "user" : "assistant",
      metadata: { ts: message.ts },
      parts: [
        { type: "text", text: message.text, state: "done" },
        ...(action ? [{ type: "data-action" as const, data: action }] : []),
      ],
    }
  })
}

async function fixtureChatFetch(input: RequestInfo | URL, init?: RequestInit) {
  if (!isFixtures()) return fetch(input, init)

  const fixture = await fetch("/fixtures/analyst-chat.json", {
    cache: "no-store",
  })
  if (!fixture.ok) return fixture
  const reply = (await fixture.json()) as {
    reply: string
    action: AnalystAction | null
  }
  const frames = [
    { type: "start" },
    { type: "text-start", id: "0" },
    { type: "text-delta", id: "0", delta: reply.reply },
    { type: "text-end", id: "0" },
    ...(reply.action ? [{ type: "data-action", data: reply.action }] : []),
    { type: "finish" },
  ]
  return new Response(
    `${frames.map((frame) => `data: ${JSON.stringify(frame)}`).join("\n\n")}\n\ndata: [DONE]\n\n`,
    {
      headers: { "Content-Type": "text/event-stream" },
    }
  )
}

function Markdown({
  children,
  mono = false,
  isStreaming = false,
}: {
  children: string
  mono?: boolean
  isStreaming?: boolean
}) {
  return (
    <Streamdown
      className={cn(
        "[&_a]:text-primary-ink [&_a]:underline [&_code]:rounded [&_code]:bg-cell [&_code]:px-1 [&_code]:py-0.5 [&_pre]:overflow-x-auto [&_pre]:rounded-md [&_pre]:bg-cell [&_pre]:p-3 [&_table]:w-full [&_table]:border-collapse [&_td]:border [&_td]:border-border [&_td]:p-2 [&_th]:border [&_th]:border-border [&_th]:bg-cell [&_th]:p-2",
        mono && "font-mono text-[11px] leading-relaxed"
      )}
      isAnimating={isStreaming}
    >
      {children}
    </Streamdown>
  )
}

function ActionCard({ action }: { action: AnalystAction }) {
  const refused = !action.executed
  return (
    <div
      className={cn(
        "mt-2 rounded-lg border p-3 text-xs",
        action.executed
          ? "border-long/30 bg-long/8"
          : "border-short/30 bg-short/8"
      )}
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="font-mono font-semibold uppercase">
          {action.type} {action.market} {action.side}
        </span>
        <StatusPill tone={action.executed ? "long" : "short"}>
          {action.executed ? "Executed" : "Refused"}
        </StatusPill>
      </div>
      <div className="mt-2 grid grid-cols-3 gap-2 font-mono text-muted-foreground">
        <span>SL {action.sl_pct}%</span>
        <span>TP {action.tp_pct}%</span>
        <span>CONF {(action.conviction * 100).toFixed(0)}%</span>
      </div>
      {refused && action.gate_refusals.length > 0 ? (
        <ul className="mt-2 list-inside list-disc text-short-ink">
          {action.gate_refusals.map((refusal) => (
            <li key={refusal}>{refusal}</li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

function CallRow({ call }: { call: AnalystCall }) {
  const [expanded, setExpanded] = React.useState(false)
  const tone =
    call.outcome_kind === "ok"
      ? "long"
      : call.outcome_kind.includes("err")
        ? "short"
        : "warning"
  return (
    <article className="border-b border-border/70 py-3 last:border-0">
      <button
        aria-expanded={expanded}
        className="flex w-full items-center gap-2 text-left"
        onClick={() => setExpanded((value) => !value)}
        type="button"
      >
        <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
          {ist(call.ts)}
        </span>
        <span className="font-mono text-xs font-semibold">{call.market}</span>
        <AnalystChip analyst={call.analyst} />
        <StatusPill tone="accent">{call.trigger}</StatusPill>
        <StatusPill tone={tone}>{call.outcome_kind}</StatusPill>
        <span className="ml-auto font-mono text-[11px] text-muted-foreground">
          {call.latency_ms}ms
        </span>
      </button>
      {expanded ? (
        <div className="mt-3 grid gap-3 rounded-md bg-surface-2 p-3 text-xs">
          <div>
            <p className="mb-1 text-[11px] font-medium text-muted-foreground">
              PROMPT
            </p>
            <Markdown mono>{call.prompt}</Markdown>
          </div>
          <div>
            <p className="mb-1 text-[11px] font-medium text-muted-foreground">
              RAW RESPONSE
            </p>
            <Markdown mono>{call.response_raw}</Markdown>
          </div>
        </div>
      ) : null}
    </article>
  )
}

function ArenaLeaderboard({
  models,
  error,
}: {
  models: AnalystLeaderboardRow[] | null
  error: string | null
}) {
  const ranked = React.useMemo(
    () => [...(models ?? [])].sort((a, b) => b.realized_pnl - a.realized_pnl),
    [models]
  )

  return (
    <section className="mb-5 overflow-hidden rounded-xl border border-border bg-card">
      <div className="flex flex-wrap items-end justify-between gap-2 border-b border-border px-4 py-3">
        <div>
          <h2 className="text-sm font-semibold">Model arena</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">
            Shared paper book, independently managed.
          </p>
        </div>
        <span className="font-mono text-[11px] text-muted-foreground">
          {ranked.length} models
        </span>
      </div>
      {error ? (
        <p className="px-4 py-5 text-sm text-short-ink">{error}</p>
      ) : models === null ? (
        <p className="px-4 py-5 text-sm text-muted-foreground">
          Loading model arena…
        </p>
      ) : ranked.length === 0 ? (
        <p className="px-4 py-5 text-sm text-muted-foreground">
          No arena models are reporting yet.
        </p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[850px] text-left text-xs">
            <thead className="border-b border-border bg-surface-2/50 text-muted-foreground">
              <tr>
                <th className="px-4 py-2 font-medium">#</th>
                <th className="px-3 py-2 font-medium">Model</th>
                <th className="px-3 py-2 font-medium">Status</th>
                <th className="px-3 py-2 text-right font-medium">Open</th>
                <th className="px-3 py-2 text-right font-medium">W / L</th>
                <th className="px-3 py-2 text-right font-medium">Win rate</th>
                <th className="px-3 py-2 text-right font-medium">Realized</th>
                <th className="px-3 py-2 text-right font-medium">uPnL</th>
                <th className="px-4 py-2 text-right font-medium">Decides</th>
              </tr>
            </thead>
            <tbody>
              {ranked.map((row, index) => {
                const pnlTone =
                  row.realized_pnl > 0
                    ? "text-long-ink"
                    : row.realized_pnl < 0
                      ? "text-short-ink"
                      : "text-muted-foreground"
                const upnlTone =
                  row.unrealized_pnl > 0
                    ? "text-long-ink"
                    : row.unrealized_pnl < 0
                      ? "text-short-ink"
                      : "text-muted-foreground"
                return (
                  <tr
                    className={cn(
                      "border-b border-border/60 last:border-0",
                      index === 0 && "bg-primary/6"
                    )}
                    key={row.model}
                  >
                    <td className="px-4 py-2.5 font-mono text-muted-foreground">
                      {index + 1}
                    </td>
                    <td className="px-3 py-2.5">
                      <div className="flex items-center gap-2">
                        <AnalystChip analyst={row.model} />
                        {index === 0 ? (
                          <span className="text-[10px] font-semibold text-primary-ink">
                            LEADER
                          </span>
                        ) : null}
                      </div>
                      <p
                        className="mt-0.5 font-mono text-[10px] text-muted-foreground"
                        title={row.model}
                      >
                        {row.model}
                      </p>
                    </td>
                    <td className="px-3 py-2.5">
                      <span
                        className="inline-flex items-center gap-1.5"
                        title={row.enabled ? "Enabled" : "Disabled"}
                      >
                        <span
                          className={cn(
                            "size-1.5 rounded-full",
                            row.enabled ? "bg-long" : "bg-muted-foreground/50"
                          )}
                        />
                        {row.enabled ? "Active" : "Off"}
                      </span>
                    </td>
                    <td className="px-3 py-2.5 text-right font-mono">
                      {row.positions_open}
                    </td>
                    <td className="px-3 py-2.5 text-right font-mono">
                      {row.wins} / {Math.max(0, row.closes - row.wins)}
                    </td>
                    <td className="px-3 py-2.5 text-right font-mono">
                      {(row.win_rate * 100).toFixed(0)}%
                    </td>
                    <td
                      className={cn(
                        "px-3 py-2.5 text-right font-mono",
                        pnlTone
                      )}
                    >
                      {row.realized_pnl >= 0 ? "+" : ""}$
                      {row.realized_pnl.toFixed(2)}
                    </td>
                    <td
                      className={cn(
                        "px-3 py-2.5 text-right font-mono",
                        upnlTone
                      )}
                    >
                      {row.unrealized_pnl >= 0 ? "+" : ""}$
                      {row.unrealized_pnl.toFixed(2)}
                    </td>
                    <td className="px-4 py-2.5 text-right font-mono">
                      {row.decides}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  )
}

export function AnalystConsole() {
  const [calls, setCalls] = React.useState<AnalystCall[] | null>(null)
  const [leaderboard, setLeaderboard] = React.useState<
    AnalystLeaderboardRow[] | null
  >(null)
  const [loadError, setLoadError] = React.useState<string | null>(null)
  const [arenaError, setArenaError] = React.useState<string | null>(null)
  const [analystFilter, setAnalystFilter] = React.useState("all")
  const [draft, setDraft] = React.useState("")
  const scrollRef = React.useRef<HTMLDivElement>(null)
  const [pendingResponseTs, setPendingResponseTs] = React.useState(0)
  const transport = React.useMemo(
    () =>
      new DefaultChatTransport<AnalystUIMessage>({
        api: CHAT_API,
        fetch: fixtureChatFetch,
      }),
    []
  )
  const {
    messages,
    setMessages,
    sendMessage,
    status,
    error,
    stop,
    regenerate,
    clearError,
  } = useChat<AnalystUIMessage>({ transport, throttle: 30 })
  const busy = status === "submitted" || status === "streaming"

  const load = React.useCallback(async () => {
    const [history, feed, arena] = await Promise.allSettled([
      api.analystHistory(),
      api.analystCalls(),
      api.analystLeaderboard(),
    ])
    if (history.status === "fulfilled")
      setMessages(historyToMessages(history.value))
    if (feed.status === "fulfilled") setCalls(feed.value)
    else setCalls((current) => current ?? [])
    if (arena.status === "fulfilled") {
      setLeaderboard(arena.value)
      setArenaError(null)
    } else {
      setLeaderboard((current) => current ?? [])
      setArenaError("Model arena is unavailable. It may still be deploying.")
    }
    setLoadError(
      history.status === "rejected" || feed.status === "rejected"
        ? "Analyst service is unavailable. Reconnect to Kestrel and refresh this page."
        : null
    )
  }, [setMessages])

  React.useEffect(() => {
    void Promise.resolve().then(load)
  }, [load])

  React.useEffect(() => {
    const poll = () => {
      void Promise.allSettled([
        api.analystCalls(),
        api.analystLeaderboard(),
      ]).then(([feed, arena]) => {
        if (feed.status === "fulfilled") setCalls(feed.value)
        if (arena.status === "fulfilled") {
          setLeaderboard(arena.value)
          setArenaError(null)
        } else {
          setArenaError(
            "Model arena is unavailable. It may still be deploying."
          )
        }
      })
    }
    const interval = window.setInterval(poll, 15_000)
    return () => window.clearInterval(interval)
  }, [])

  React.useEffect(() => {
    scrollRef.current?.scrollTo({
      top: scrollRef.current.scrollHeight,
      behavior: "smooth",
    })
  }, [messages, status])

  const send = async (event: React.FormEvent) => {
    event.preventDefault()
    const text = draft.trim()
    if (!text || busy) return
    setDraft("")
    const submittedAt = now()
    setPendingResponseTs(submittedAt)
    clearError()
    await sendMessage({ text, metadata: { ts: submittedAt } })
    void Promise.all([api.analystHistory(), api.analystCalls()])
      .then(([history, feed]) => {
        setMessages(historyToMessages(history))
        setCalls(feed)
      })
      .catch(() => undefined)
  }

  const lastAssistant = [...messages]
    .reverse()
    .find((message) => message.role === "assistant")
  const filterModels = leaderboard?.map((row) => row.model) ?? []
  const filteredCalls = calls?.filter(
    (call) =>
      analystFilter === "all" || analystLabel(call.analyst) === analystFilter
  )

  return (
    <>
      <PageHeader
        icon={SparklesIcon}
        title="Analyst"
        meta={
          <span>Ask the paper trader and inspect every decision path.</span>
        }
        actions={
          <Button onClick={() => void load()} size="sm" variant="outline">
            <HugeiconsIcon icon={RefreshIcon} size={14} />
            Refresh
          </Button>
        }
      />
      {loadError || error ? (
        <p className="mb-4 rounded-md bg-short/10 px-3 py-2 text-xs text-short-ink">
          {loadError ?? error?.message}
        </p>
      ) : null}
      <ArenaLeaderboard error={arenaError} models={leaderboard} />
      <div className="grid min-w-0 gap-5 xl:grid-cols-[minmax(0,1fr)_minmax(420px,.85fr)]">
        <section className="flex min-h-[600px] flex-col overflow-hidden rounded-xl border border-border bg-card">
          <div className="border-b border-border px-4 py-3">
            <h2 className="text-sm font-semibold">Chat</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">
              Conversation with the analyst
            </p>
          </div>
          <div className="flex-1 overflow-y-auto p-4" ref={scrollRef}>
            <div className="space-y-4">
              {messages.length === 0 ? (
                <div className="grid min-h-56 place-items-center rounded-lg border border-dashed border-border bg-surface-2/50 p-6 text-center">
                  <div>
                    <p className="text-sm font-medium">No conversation yet</p>
                    <p className="mt-1 text-xs text-muted-foreground">
                      Ask about a market, risk gate, or recent decision.
                    </p>
                  </div>
                </div>
              ) : null}
              {messages.map((message) => (
                <div
                  className={cn(
                    "max-w-[88%]",
                    message.role === "user" ? "ml-auto" : "mr-auto"
                  )}
                  key={message.id}
                >
                  <div
                    className={cn(
                      "rounded-xl px-3 py-2.5 text-sm",
                      message.role === "user"
                        ? "bg-primary text-primary-foreground"
                        : "bg-surface-2 text-foreground"
                    )}
                  >
                    {message.parts.map((part, index) => {
                      if (part.type === "text") {
                        return message.role === "assistant" ? (
                          <Markdown
                            isStreaming={busy && message === messages.at(-1)}
                            key={index}
                          >
                            {part.text}
                          </Markdown>
                        ) : (
                          <p className="whitespace-pre-wrap" key={index}>
                            {part.text}
                          </p>
                        )
                      }
                      return null
                    })}
                    {message.role === "assistant" &&
                    busy &&
                    message === messages.at(-1) ? (
                      <span
                        className="mt-2 inline-block h-3 w-1 animate-pulse bg-primary"
                        aria-label="Analyst is typing"
                      />
                    ) : null}
                  </div>
                  {message.parts.map((part, index) =>
                    part.type === "data-action" ? (
                      <ActionCard action={part.data} key={index} />
                    ) : null
                  )}
                  <p
                    className={cn(
                      "mt-1 text-[11px] text-muted-foreground",
                      message.role === "user" && "text-right"
                    )}
                  >
                    {ist(message.metadata?.ts ?? pendingResponseTs)} IST
                  </p>
                </div>
              ))}
              {status === "submitted" ? (
                <div className="mr-auto rounded-xl bg-surface-2 px-3 py-2.5 text-xs text-muted-foreground">
                  Analyst is reading the tape…
                </div>
              ) : null}
            </div>
          </div>
          <form
            className="border-t border-border p-3"
            onSubmit={(event) => void send(event)}
          >
            <div className="flex gap-2">
              <input
                aria-label="Message analyst"
                className="h-10 min-w-0 flex-1 rounded-md border border-input bg-transparent px-3 text-sm outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/30"
                disabled={busy}
                onChange={(event) => setDraft(event.target.value)}
                placeholder="Ask about SOL, risk, or a decision…"
                value={draft}
              />
              {busy ? (
                <Button
                  onClick={() => void stop()}
                  size="sm"
                  type="button"
                  variant="outline"
                >
                  <HugeiconsIcon icon={StopIcon} size={14} /> Stop
                </Button>
              ) : (
                <Button disabled={!draft.trim()} size="sm" type="submit">
                  <HugeiconsIcon icon={SentIcon} size={14} /> Send
                </Button>
              )}
            </div>
            {!busy && lastAssistant ? (
              <button
                className="mt-2 text-xs text-primary-ink underline underline-offset-4"
                onClick={() => void regenerate()}
                type="button"
              >
                Regenerate last response
              </button>
            ) : null}
          </form>
        </section>
        <section className="rounded-xl border border-border bg-card">
          <div className="flex flex-wrap items-end justify-between gap-3 border-b border-border px-4 py-3">
            <div>
              <h2 className="text-sm font-semibold">Why it did what it did</h2>
              <p className="mt-0.5 text-xs text-muted-foreground">
                Click a call to inspect its prompt and raw model response.
              </p>
            </div>
            <label className="grid gap-1 text-[10px] font-medium tracking-wide text-muted-foreground uppercase">
              Analyst
              <select
                className="h-8 rounded-md border border-input bg-background px-2 text-xs font-normal text-foreground normal-case"
                onChange={(event) => setAnalystFilter(event.target.value)}
                value={analystFilter}
              >
                <option value="all">All models</option>
                {analystFilter !== "all" &&
                !filterModels.includes(analystFilter) ? (
                  <option value={analystFilter}>
                    {analystShortName(analystFilter)}
                  </option>
                ) : null}
                {filterModels.map((model) => (
                  <option key={model} value={model}>
                    {analystShortName(model)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className="max-h-[600px] overflow-y-auto px-4">
            {calls === null ? (
              <p className="py-4 text-sm text-muted-foreground">
                Loading decision feed…
              </p>
            ) : null}
            {filteredCalls?.length === 0 ? (
              <p className="py-4 text-sm text-muted-foreground">
                {calls?.length
                  ? "No calls match this analyst."
                  : "No analyst calls recorded."}
              </p>
            ) : null}
            {filteredCalls?.map((call) => (
              <CallRow call={call} key={call.id} />
            ))}
          </div>
        </section>
      </div>
    </>
  )
}
