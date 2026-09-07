"use client"

import { useChat } from "@ai-sdk/react"
import {
  RiBrainLine,
  RiChatAiLine,
  RiDatabase2Line,
  RiLoader4Line,
  RiSearchEyeLine,
  RiSendPlane2Line,
  RiShieldCheckLine,
  RiStopCircleLine,
} from "@remixicon/react"
import { DefaultChatTransport } from "ai"
import { useEffect, useMemo, useRef, useState } from "react"
import { Streamdown } from "streamdown"

import { Badge } from "@workspace/ui/components/badge"
import { Button } from "@workspace/ui/components/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@workspace/ui/components/card"
import { Textarea } from "@workspace/ui/components/textarea"
import { cn } from "@workspace/ui/lib/utils"
import { ProposalCard } from "@/components/analyst/proposal-card"
import { api, type ChatContextEvent, type TradeProposal } from "@/lib/api"
import {
  historyToMessages,
  lastUserText,
  proposalFromMessage,
  type AnalystMessage,
} from "@/lib/chat"
import { ago, signedUsd, stamp, usd } from "@/lib/format"
import { useLiveDashboard } from "@/lib/use-live-dashboard"

const SUGGESTIONS = [
  "Summarize every open position and its live risk.",
  "Who placed the current open orders?",
  "Assess the best setup now, but do not prepare a trade yet.",
  "Check whether every position has both venue brackets.",
]

type Activity = { label: string; detail?: string; tone?: "normal" | "error" }

function messageText(message: AnalystMessage): string {
  return message.parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
    .join("")
}

function resultTimestamp(message: AnalystMessage): number | null {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type === "data-result") return part.data.message.ts
  }
  return message.metadata?.ts ?? null
}

function searchCount(message: AnalystMessage): number {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type !== "data-result") continue
    const tools = part.data.message.metadata.tools
    return Array.isArray(tools) ? tools.length : 0
  }
  return 0
}

function LiveContext({
  requestContext,
}: {
  requestContext: ChatContextEvent | null
}) {
  const live = useLiveDashboard()
  const [nowMs, setNowMs] = useState<number | null>(null)

  useEffect(() => {
    const timer = setInterval(() => setNowMs(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [])

  const snapshot = live.data
  const stale = snapshot?.stale === true
  const ageSeconds =
    snapshot && nowMs != null
      ? Math.max(0, Math.round(nowMs / 1000 - snapshot.as_of_ts))
      : null

  return (
    <Card className="min-h-0 gap-0 py-0 xl:h-full">
      <CardHeader className="flex-row items-center justify-between border-b py-3 [.border-b]:pb-3">
        <CardTitle className="flex items-center gap-2 font-sans text-xs font-semibold">
          <RiDatabase2Line className="size-4 text-emerald-500" /> Live upstream
        </CardTitle>
        <Badge
          variant={live.connected && !stale ? "secondary" : "outline"}
          className="text-[10px]"
        >
          <span
            className={cn(
              "size-1.5",
              live.connected && !stale ? "bg-emerald-500" : "bg-amber-500"
            )}
          />
          {stale ? "stale" : live.source}
        </Badge>
      </CardHeader>
      <CardContent className="space-y-4 py-3">
        {snapshot ? (
          <>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <div className="text-[10px] tracking-wider text-muted-foreground uppercase">
                  Equity
                </div>
                <div className="mt-1 font-mono text-lg font-semibold tabular-nums">
                  {usd(snapshot.account.equity)}
                </div>
              </div>
              <div>
                <div className="text-[10px] tracking-wider text-muted-foreground uppercase">
                  Available
                </div>
                <div className="mt-1 font-mono text-lg font-semibold tabular-nums">
                  {usd(snapshot.account.available_margin)}
                </div>
              </div>
            </div>
            <div className="grid grid-cols-3 border-y py-2 text-center">
              <div>
                <div className="font-mono text-sm font-semibold">
                  {snapshot.positions.length}
                </div>
                <div className="text-[10px] text-muted-foreground uppercase">
                  positions
                </div>
              </div>
              <div className="border-x">
                <div className="font-mono text-sm font-semibold">
                  {snapshot.orders.length}
                </div>
                <div className="text-[10px] text-muted-foreground uppercase">
                  orders
                </div>
              </div>
              <div>
                <div
                  className={cn(
                    "font-mono text-sm font-semibold",
                    snapshot.realized.total >= 0
                      ? "text-emerald-500"
                      : "text-red-500"
                  )}
                >
                  {signedUsd(snapshot.realized.total)}
                </div>
                <div className="text-[10px] text-muted-foreground uppercase">
                  realized
                </div>
              </div>
            </div>
            <div className="space-y-2">
              <div className="text-[10px] tracking-wider text-muted-foreground uppercase">
                Open positions
              </div>
              {snapshot.positions.length ? (
                snapshot.positions.map((position) => (
                  <div
                    key={position.market}
                    className="border-l border-border pl-2"
                  >
                    <div className="flex items-center justify-between gap-2 text-xs">
                      <span className="font-semibold">
                        {position.market.replace(/^xyz:/, "")}
                      </span>
                      <span
                        className={cn(
                          "font-mono tabular-nums",
                          position.upnl >= 0
                            ? "text-emerald-500"
                            : "text-red-500"
                        )}
                      >
                        {signedUsd(position.upnl)}
                      </span>
                    </div>
                    <div className="mt-0.5 text-[10px] text-muted-foreground">
                      {position.side} · {position.leverage}x{" "}
                      {position.margin_mode} · SL {position.stop_px ?? "none"} ·
                      TP {position.tp_px ?? "none"}
                    </div>
                  </div>
                ))
              ) : (
                <div className="text-xs text-muted-foreground">Flat.</div>
              )}
            </div>
            <div className="font-mono text-[10px] text-muted-foreground">
              venue snapshot {ageSeconds == null ? "…" : `${ageSeconds}s old`} ·{" "}
              {stamp(snapshot.as_of_ts)}
            </div>
          </>
        ) : (
          <div className="flex items-center gap-2 py-6 text-xs text-muted-foreground">
            <RiLoader4Line className="size-4 animate-spin" /> Connecting to
            venue state…
          </div>
        )}

        {requestContext && (
          <div className="space-y-2 border-t pt-3">
            <div className="flex items-center justify-between">
              <div className="text-[10px] tracking-wider text-muted-foreground uppercase">
                Last answer context
              </div>
              <Badge variant="outline" className="text-[10px]">
                {ago(requestContext.as_of_ts)}
              </Badge>
            </div>
            <div className="grid grid-cols-2 gap-x-3 gap-y-1 font-mono text-[10px] text-muted-foreground">
              <span>{requestContext.venue_fills} venue fills</span>
              <span>{requestContext.telegram_messages} TG messages</span>
              <span>{requestContext.news_items} news items</span>
              <span>{requestContext.candidates.length} markets</span>
            </div>
          </div>
        )}
        {live.error && (
          <div className="border border-destructive/30 bg-destructive/5 p-2 text-[11px] text-destructive">
            {live.error}
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function ChatMessage({
  message,
  streaming,
  proposalOverride,
  onProposalUpdate,
}: {
  message: AnalystMessage
  streaming: boolean
  proposalOverride?: TradeProposal
  onProposalUpdate: (proposal: TradeProposal) => void
}) {
  const content = messageText(message)
  const proposal = proposalOverride ?? proposalFromMessage(message)
  const searches = searchCount(message)
  const timestamp = resultTimestamp(message)
  const user = message.role === "user"

  return (
    <article className={cn("flex gap-3", user && "justify-end")}>
      {!user && (
        <div className="mt-1 flex size-7 shrink-0 items-center justify-center border bg-muted text-foreground">
          <RiBrainLine className="size-4" />
        </div>
      )}
      <div
        className={cn("max-w-[min(48rem,92%)] min-w-0", user && "max-w-[85%]")}
      >
        {user ? (
          <div className="bg-primary px-3 py-2 text-sm leading-6 whitespace-pre-wrap text-primary-foreground">
            {content}
          </div>
        ) : (
          <div className="border bg-card px-3 py-2.5">
            <Streamdown
              animated={{ animation: "fadeIn", duration: 140, stagger: 6 }}
              className="text-sm leading-6"
              controls={{ code: { copy: true }, table: { copy: true } }}
              isAnimating={streaming}
              mode={streaming ? "streaming" : "static"}
              parseIncompleteMarkdown
              skipHtml
            >
              {content}
            </Streamdown>
            <div className="mt-2 flex items-center gap-2 text-[10px] text-muted-foreground">
              {streaming ? (
                <>
                  <RiLoader4Line className="size-3 animate-spin" /> Qwen
                  streaming
                </>
              ) : (
                <>
                  {timestamp ? stamp(timestamp) : "Qwen"}
                  {searches > 0 && (
                    <>
                      <span>·</span>
                      <RiSearchEyeLine className="size-3" /> {searches} search
                      {searches === 1 ? "" : "es"}
                    </>
                  )}
                </>
              )}
            </div>
          </div>
        )}
        {proposal && (
          <ProposalCard proposal={proposal} onUpdate={onProposalUpdate} />
        )}
      </div>
    </article>
  )
}

export function AnalystChat() {
  const transport = useMemo(
    () =>
      new DefaultChatTransport<AnalystMessage>({
        api: "/peri/api/chat",
        prepareSendMessagesRequest: ({ messages }) => ({
          body: { message: lastUserText(messages) },
        }),
      }),
    []
  )
  const [activity, setActivity] = useState<Activity | null>(null)
  const [requestContext, setRequestContext] = useState<ChatContextEvent | null>(
    null
  )
  const [input, setInput] = useState("")
  const [historyLoading, setHistoryLoading] = useState(true)
  const [olderLoading, setOlderLoading] = useState(false)
  const [oldestHistoryId, setOldestHistoryId] = useState<number | null>(null)
  const [hasOlder, setHasOlder] = useState(false)
  const [historyError, setHistoryError] = useState<string | null>(null)
  const [proposalOverrides, setProposalOverrides] = useState<
    Record<string, TradeProposal>
  >({})
  const bottomRef = useRef<HTMLDivElement>(null)
  const suppressNextScroll = useRef(false)

  const { messages, setMessages, sendMessage, status, error, stop } =
    useChat<AnalystMessage>({
      id: "peri-live-analyst",
      transport,
      throttle: 30,
      onData(part) {
        if (part.type === "data-context") {
          setRequestContext(part.data)
          setActivity({
            label: "Fresh venue context attached",
            detail: `${part.data.open_positions} positions · ${part.data.open_orders} orders`,
          })
        } else if (part.type === "data-tool") {
          const query =
            typeof part.data.args.query === "string"
              ? part.data.args.query
              : "live evidence"
          setActivity(
            part.data.phase === "start"
              ? {
                  label: `Searching ${part.data.tool.replace("_", " ")}`,
                  detail: query,
                }
              : { label: "Search evidence attached", detail: query }
          )
        } else if (part.type === "data-status") {
          const labels: Record<string, string> = {
            refreshing_context: "Refreshing every upstream source",
            analyst: "Qwen is analyzing live state",
            complete: "Answer grounded and persisted",
          }
          setActivity({ label: labels[part.data.phase] ?? part.data.phase })
        } else if (part.type === "data-error") {
          setActivity({ label: part.data.error, tone: "error" })
        } else if (part.type === "data-proposal") {
          setActivity({ label: "Immutable confirmation card ready" })
        }
      },
      onError(chatError) {
        setActivity({ label: chatError.message, tone: "error" })
      },
      onFinish() {
        setActivity((current) => (current?.tone === "error" ? current : null))
      },
    })

  useEffect(() => {
    let active = true
    void api
      .chatHistory(100)
      .then((history) => {
        if (!active) return
        setMessages(historyToMessages(history))
        setOldestHistoryId(history[0]?.id ?? null)
        setHasOlder(history.length === 100)
        setHistoryError(null)
      })
      .catch((cause) => {
        if (active)
          setHistoryError(
            cause instanceof Error ? cause.message : String(cause)
          )
      })
      .finally(() => {
        if (active) setHistoryLoading(false)
      })
    return () => {
      active = false
    }
  }, [setMessages])

  useEffect(() => {
    if (suppressNextScroll.current) {
      suppressNextScroll.current = false
      return
    }
    bottomRef.current?.scrollIntoView({
      behavior: status === "ready" ? "smooth" : "auto",
    })
  }, [messages, status])

  const busy = status === "submitted" || status === "streaming"

  async function submit() {
    const message = input.trim()
    if (!message || busy || historyLoading) return
    setInput("")
    setActivity({ label: "Sending analyst request" })
    try {
      await sendMessage({ text: message })
    } catch (cause) {
      setActivity({
        label: cause instanceof Error ? cause.message : String(cause),
        tone: "error",
      })
    }
  }

  async function loadOlder() {
    if (olderLoading || oldestHistoryId == null) return
    setOlderLoading(true)
    setHistoryError(null)
    try {
      const history = await api.chatHistory(100, oldestHistoryId)
      suppressNextScroll.current = true
      setMessages((current) => [...historyToMessages(history), ...current])
      setOldestHistoryId(history[0]?.id ?? oldestHistoryId)
      setHasOlder(history.length === 100)
    } catch (cause) {
      setHistoryError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setOlderLoading(false)
    }
  }

  function updateProposal(proposal: TradeProposal) {
    setProposalOverrides((current) => ({ ...current, [proposal.id]: proposal }))
  }

  return (
    <div className="grid min-h-[calc(100dvh-7.5rem)] gap-4 xl:h-[calc(100dvh-7.5rem)] xl:min-h-0 xl:grid-cols-[minmax(0,1fr)_21rem]">
      <Card className="flex min-h-[44rem] flex-col gap-0 py-0 xl:min-h-0">
        <CardHeader className="flex-row items-center justify-between gap-3 border-b py-3 [.border-b]:pb-3">
          <div className="min-w-0">
            <CardTitle className="flex items-center gap-2 font-sans text-sm font-semibold">
              <RiChatAiLine className="size-4 text-emerald-500" /> Live analyst
              <Badge variant="secondary" className="text-[10px]">
                <span className="size-1.5 bg-emerald-500" /> Qwen
              </Badge>
            </CardTitle>
            <p className="mt-1 text-[11px] text-muted-foreground">
              Venue-authoritative answers · read-only search · live actions
              require confirmation
            </p>
          </div>
          <Badge variant="outline" className="hidden gap-1 text-[10px] sm:flex">
            <RiShieldCheckLine /> text cannot execute
          </Badge>
        </CardHeader>

        <CardContent className="min-h-0 flex-1 overflow-y-auto p-0">
          <div className="mx-auto flex min-h-full max-w-4xl flex-col gap-5 px-4 py-5 sm:px-6">
            {historyLoading ? (
              <div className="flex flex-1 items-center justify-center gap-2 text-xs text-muted-foreground">
                <RiLoader4Line className="size-4 animate-spin" /> Loading
                durable conversation…
              </div>
            ) : messages.length === 0 ? (
              <div className="my-auto space-y-5 py-10 text-center">
                <div className="mx-auto flex size-11 items-center justify-center border bg-muted">
                  <RiBrainLine className="size-5" />
                </div>
                <div>
                  <h1 className="font-heading text-xl font-semibold">
                    Ask from the live account.
                  </h1>
                  <p className="mx-auto mt-2 max-w-lg text-sm leading-6 text-muted-foreground">
                    Qwen sees current venue positions, open orders, fills,
                    brackets, PnL, Telegram, news, decisions, notes, market
                    features, and bounded conversation history.
                  </p>
                </div>
                <div className="mx-auto grid max-w-2xl gap-2 sm:grid-cols-2">
                  {SUGGESTIONS.map((suggestion) => (
                    <Button
                      key={suggestion}
                      className="h-auto justify-start py-2 text-left whitespace-normal"
                      onClick={() => setInput(suggestion)}
                      variant="outline"
                    >
                      {suggestion}
                    </Button>
                  ))}
                </div>
              </div>
            ) : (
              <>
                {hasOlder && (
                  <Button
                    className="mx-auto"
                    disabled={olderLoading}
                    onClick={() => void loadOlder()}
                    size="sm"
                    variant="outline"
                  >
                    {olderLoading && <RiLoader4Line className="animate-spin" />}
                    {olderLoading ? "Loading…" : "Load older messages"}
                  </Button>
                )}
                {messages.map((message, index) => {
                  const proposal = proposalFromMessage(message)
                  return (
                    <ChatMessage
                      key={message.id}
                      message={message}
                      onProposalUpdate={updateProposal}
                      proposalOverride={
                        proposal ? proposalOverrides[proposal.id] : undefined
                      }
                      streaming={
                        busy &&
                        index === messages.length - 1 &&
                        message.role === "assistant"
                      }
                    />
                  )
                })}
              </>
            )}
            <div ref={bottomRef} />
          </div>
        </CardContent>

        <div className="border-t bg-background p-3 sm:p-4">
          <div className="mx-auto max-w-4xl">
            {(activity || error || historyError) && (
              <div
                aria-live="polite"
                className={cn(
                  "mb-2 flex items-center gap-2 text-[11px] text-muted-foreground",
                  (activity?.tone === "error" || error || historyError) &&
                    "text-destructive"
                )}
              >
                {busy && !error ? (
                  <RiLoader4Line className="size-3.5 animate-spin" />
                ) : (
                  <RiDatabase2Line className="size-3.5" />
                )}
                <span>{historyError ?? error?.message ?? activity?.label}</span>
                {activity?.detail && (
                  <span className="hidden truncate opacity-70 sm:inline">
                    · {activity.detail}
                  </span>
                )}
              </div>
            )}
            <div className="flex items-end gap-2">
              <label className="sr-only" htmlFor="analyst-message">
                Message Qwen analyst
              </label>
              <Textarea
                id="analyst-message"
                aria-describedby="analyst-help"
                className="max-h-40 min-h-16 resize-none text-sm leading-5"
                disabled={historyLoading}
                maxLength={4000}
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={(event) => {
                  if (
                    event.key === "Enter" &&
                    !event.shiftKey &&
                    !event.nativeEvent.isComposing
                  ) {
                    event.preventDefault()
                    void submit()
                  }
                }}
                placeholder="Ask about positions, orders, PnL, a market—or request a confirmation card…"
                value={input}
              />
              {busy ? (
                <Button
                  aria-label="Stop response"
                  onClick={() => void stop()}
                  size="icon-lg"
                  variant="outline"
                >
                  <RiStopCircleLine />
                </Button>
              ) : (
                <Button
                  aria-label="Send message"
                  disabled={!input.trim() || historyLoading}
                  onClick={() => void submit()}
                  size="icon-lg"
                >
                  <RiSendPlane2Line />
                </Button>
              )}
            </div>
            <div
              id="analyst-help"
              className="mt-1.5 flex justify-between text-[10px] text-muted-foreground"
            >
              <span>Enter to send · Shift+Enter for newline</span>
              <span className="font-mono tabular-nums">
                {input.length}/4000
              </span>
            </div>
          </div>
        </div>
      </Card>
      <LiveContext requestContext={requestContext} />
    </div>
  )
}
