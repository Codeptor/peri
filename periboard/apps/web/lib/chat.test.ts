import { describe, expect, test } from "bun:test"

import type { ChatRecord, TradeProposal } from "./api"
import {
  effectiveProposalStatus,
  historyToMessages,
  lastUserText,
  proposalFromMessage,
  proposalExecutionMode,
  proposalSecondsLeft,
  proposalTitle,
  type AnalystMessage,
} from "./chat"

const proposal: TradeProposal = {
  id: "p-1",
  created_ts: 100,
  context_ts: 99,
  expires_ts: 220,
  action: {
    kind: "open",
    market: "xyz:NVDA",
    side: "long",
    leverage: 20,
    margin_mode: "cross",
    stop: 218,
    take_profit: 236,
    conviction: 0.82,
    source: "own",
    mirror_msg_id: null,
    rationale: "breakout",
    invalidation: "loses 218",
  },
  preview: {
    kind: "open",
    market: "xyz:NVDA",
    notional: 20,
    required_margin: 1,
  },
  status: "pending",
  claimed_ts: null,
  finished_ts: null,
  result: null,
}

describe("analyst chat adapters", () => {
  test("sends only the newest user text to the daemon", () => {
    const messages: AnalystMessage[] = [
      { id: "1", role: "user", parts: [{ type: "text", text: "old" }] },
      { id: "2", role: "assistant", parts: [{ type: "text", text: "answer" }] },
      {
        id: "3",
        role: "user",
        parts: [
          { type: "text", text: " assess " },
          { type: "text", text: " NVDA " },
        ],
      },
    ]

    expect(lastUserText(messages)).toBe("assess  NVDA")
  })

  test("maps durable history and restores its confirmation card", () => {
    const records: ChatRecord[] = [
      {
        id: 10,
        ts: 101,
        role: "user",
        content: "Prepare it",
        context_ts: null,
        proposal_id: null,
        metadata: {},
        proposal: null,
      },
      {
        id: 11,
        ts: 102,
        role: "assistant",
        content: "Prepared.",
        context_ts: 99,
        proposal_id: "p-1",
        metadata: { latency_ms: 7 },
        proposal,
      },
    ]

    const messages = historyToMessages(records)

    expect(messages.map((message) => message.id)).toEqual([
      "history-10",
      "history-11",
    ])
    expect(proposalFromMessage(messages[1]!)).toEqual(proposal)
  })

  test("proposal timing is derived from server timestamps", () => {
    expect(proposalSecondsLeft(proposal, 150_000)).toBe(70)
    expect(effectiveProposalStatus(proposal, 150_000)).toBe("pending")
    expect(proposalSecondsLeft(proposal, 220_001)).toBe(0)
    expect(effectiveProposalStatus(proposal, 220_001)).toBe("expired")
    expect(
      effectiveProposalStatus({ ...proposal, status: "executed" }, 999_000)
    ).toBe("executed")
  })

  test("labels the exact immutable action", () => {
    expect(proposalTitle(proposal)).toBe("20x cross NVDA long")
    expect(
      proposalTitle({
        ...proposal,
        action: { kind: "close", market: "SOL", rationale: "done" },
      })
    ).toBe("Close full SOL position")
    expect(
      proposalTitle({
        ...proposal,
        action: {
          kind: "adjust_stop",
          market: "BTC",
          stop: 100,
          take_profit: 130,
          rationale: "protect",
        },
      })
    ).toBe("Replace BTC SL + TP")
  })

  test("confirmation copy follows the server preview mode", () => {
    expect(proposalExecutionMode(proposal)).toBe("paper")
    expect(
      proposalExecutionMode({
        ...proposal,
        preview: { ...proposal.preview, mode: "live" },
      })
    ).toBe("live")
  })
})
