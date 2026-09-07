import type { UIMessage } from "ai"

import type {
  ChatContextEvent,
  ChatRecord,
  ChatResult,
  ChatToolEvent,
  ProposalStatus,
  TradeProposal,
} from "./api"

export type AnalystDataParts = {
  context: ChatContextEvent
  tool: ChatToolEvent
  proposal: TradeProposal
  status: { phase: string }
  error: { phase?: string; error: string }
  result: ChatResult
}

export type AnalystMessage = UIMessage<
  { ts?: number; context_ts?: number | null },
  AnalystDataParts
>

export function lastUserText(messages: AnalystMessage[]): string {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]
    if (message?.role !== "user") continue
    return message.parts
      .filter((part) => part.type === "text")
      .map((part) => part.text)
      .join("")
      .trim()
  }
  return ""
}

export function historyToMessages(records: ChatRecord[]): AnalystMessage[] {
  return records.map((record) => {
    const parts: AnalystMessage["parts"] = [
      { type: "text", text: record.content },
    ]
    if (record.proposal) {
      parts.push({ type: "data-proposal", data: record.proposal })
    }
    return {
      id: `history-${record.id}`,
      role: record.role,
      metadata: { ts: record.ts, context_ts: record.context_ts },
      parts,
    }
  })
}

export function proposalFromMessage(
  message: AnalystMessage
): TradeProposal | null {
  for (let index = message.parts.length - 1; index >= 0; index -= 1) {
    const part = message.parts[index]
    if (part?.type === "data-result" && part.data.proposal) {
      return part.data.proposal
    }
    if (part?.type === "data-proposal") return part.data
  }
  return null
}

export function proposalSecondsLeft(
  proposal: TradeProposal,
  nowMs = Date.now()
): number {
  return Math.max(0, Math.ceil(proposal.expires_ts - nowMs / 1000))
}

export function effectiveProposalStatus(
  proposal: TradeProposal,
  nowMs = Date.now()
): ProposalStatus {
  if (
    proposal.status === "pending" &&
    proposalSecondsLeft(proposal, nowMs) === 0
  ) {
    return "expired"
  }
  return proposal.status
}

function marketLabel(market: string): string {
  return market.replace(/^xyz:/, "")
}

export function proposalTitle(proposal: TradeProposal): string {
  const { action } = proposal
  if (action.kind === "open") {
    return `${action.leverage}x ${action.margin_mode} ${marketLabel(action.market)} ${action.side}`
  }
  if (action.kind === "close") {
    return `Close full ${marketLabel(action.market)} position`
  }
  return `Replace ${marketLabel(action.market)} SL + TP`
}

export function proposalExecutionMode(
  proposal: TradeProposal
): "live" | "paper" {
  return proposal.preview.mode === "live" ? "live" : "paper"
}
