# Analyst Chat and Authorized Trade Proposals

**Date:** 2026-08-27  
**Status:** Approved for implementation by the operator's “build it asap” instruction

## Goal

Add an Analyst page to periboard where Bhanu can converse with the same Qwen
analyst used by Peri. Every message is answered from a fresh, venue-authoritative
Peri context plus bounded conversation history. Qwen may suggest one new trade or
position-management action, or translate a direct operator instruction into one,
but no model response can execute an order. Every live action requires a separate,
explicit confirmation on an immutable trade card.

Ordinary conversation is a first-class path, not merely a prelude to trading.
Bhanu can ask about any currently open position, resting order, fill, realized or
unrealized PnL, liquidation distance, bracket, thesis, invalidation, market, news
item, Telegram call, or recent decision and receive an answer grounded in the
fresh snapshot used for that exact message.

## Product contract

The chat supports two equivalent paths:

1. Bhanu asks for analysis and Qwen independently suggests a trade or position
   management action.
2. Bhanu directly requests an entry, a full position close, or an atomic SL+TP
   change and Qwen fills in any missing analytical fields.

Both paths produce a normal chat answer and, when requirements pass, one
confirmation card. Entry cards display market, side, reference mark, estimated
size/notional, leverage, cross/isolated mode, margin, SL/TP, reward:risk,
fee-adjusted stop loss and TP profit, rationale, invalidation, context time, and
expiry. Close cards display the exact live position fingerprint (ledger id,
market, side, entry, full size), reference mark, uPnL, estimated exit fee/net PnL,
rationale, context time, and expiry. Bracket cards display the same position
fingerprint, current and proposed SL/TP, mark, and rationale. Chat text, including
words such as “confirm”, “execute”, or “buy now”, never authorizes the action.
Only its card's confirmation does.

Position modification in this release means a full close or atomic replacement
of both SL and TP, matching Peri's existing `CloseAction` and `AdjustStopAction`
contracts. Partial closes, adding size, and changing leverage or margin mode on
an already-open position are out of scope and are never inferred from vague text.

## Live context, not a vector index

This feature is retrieval-style context injection rather than an embedding
database. On every user message, the backend calls `Engine.context_snapshot`
and uses the same `build_bundle` data as an autonomous cycle:

- authoritative unified account equity, held collateral, and available margin;
- every native and xyz venue position, including actual margin mode, live mark,
  leverage, margin, liquidation price, SL/TP, thesis, and invalidation;
- every resting venue order;
- screened market candidates and candle features;
- operator notes;
- recent Telegram messages, including caller flags and edited text already
  upserted by the feed (a stored caption is visible; binary media is not invented
  or described when no text representation exists);
- current RSS and Telegram news;
- recent venue closes and realized PnL.

Chat context additionally includes the ten most recent decision summaries
(timestamp, trigger, status, market view, and parsed actions) and deterministic
request-targeted market retrieval. Before screening, ticker-like tokens in the
user's message are resolved case-insensitively against exact native/xyz universe
tickers; every resolved ticker is forced into candidates and receives fresh
features. This lets Bhanu ask about any explicitly named supported ticker even
when it is not a top mover. An unknown or ambiguous name is reported as such
rather than answered from model memory. Historical decisions are labeled
historical and never override the fresh account/position/order block.

The SQLite chat history supplies bounded conversational memory. Fresh upstream
state always wins over earlier chat text. Bounded autonomous decision history is
retrieved directly from the existing decision ledger; it is not copied into a
second index.
If authoritative account, position, order, or mark retrieval fails, the request
fails visibly and Qwen does not answer that live-state question from cached chat
history or an older dashboard frame.

## Analyst contract

`OpenAction` gains a required `margin_mode` field with values `cross` or
`isolated`. Autonomous Qwen decisions and chat proposals must choose it. The
system prompt explains:

- identical position size and price movement have identical PnL in either mode;
- cross shares collateral with the cross account and can improve capital
  efficiency, but expands account-wide liquidation exposure;
- isolated ring-fences collateral and limits liquidation spillover;
- Qwen must choose based on the whole portfolio, available collateral, venue
  support, liquidation distance, and correlated exposure, and must explain the
  choice in its rationale.

For new entries, leverage is an operator-authorized discrete policy: Qwen must
choose exactly `10x` or `20x`, preferring `20x` only when the venue supports it,
the stop is safely before estimated liquidation, collateral is adequate, and
portfolio correlation does not make shared risk unreasonable. `10x` is the only
fallback. Markets with a venue maximum below `10x` are ineligible for new entries;
the guard refuses rather than silently clamping below the requested leverage.
Candidate context includes the venue maximum so Qwen can avoid impossible
actions. `OpenAction` validates leverage as the literal set `{10, 20}`, and
`Guard.gate_open` independently rejects every other value and rejects a requested
value above the venue maximum; no `min(...)` leverage clamp remains. Existing
lower-leverage positions are not mutated. Configuration raises
the new-entry maximum from `10x` to `20x`, but stop-distance sizing continues to
risk only the configured 1.5% of equity. The prompt and confirmation card state
that leverage changes required margin and ROE, not PnL for a fixed position size.

The selected mode passes unchanged through `Guard` in `Approved` and reaches
the Hyperliquid SDK as `is_cross=margin_mode == "cross"`. Peri exposes the venue's
actual mode back into the next context and dashboard snapshot. Apart from the
explicit 10x/20x leverage policy above, conviction, stop sanity, RR, cooldown,
duplicate-market, daily cap, kill switch, minimum notional, and risk-percent
sizing rails remain unchanged. The max-concurrent rail remains load-bearing for
autonomous entries. A confirmed operator-authorized chat entry bypasses only
that one rail, so Bhanu can deliberately open a fourth or later Peri position;
it still passes every other gate, authoritative available-margin sizing, and
venue leverage support.
The guard also checks the authoritative available margin before producing an
actionable preview.

The positions ledger gains `margin_mode TEXT NOT NULL DEFAULT 'unknown'`. New
Peri positions persist the selected value. Live reconciliation reads the actual
venue leverage type and overwrites `unknown` or stale values, including legacy
rows, before building analyst context. Existing dry rows remain `unknown`; new
dry positions retain Qwen's selected mode. Dry simulation records the mode but
does not pretend to model account-wide cross-liquidation contagion.

The chat model emits strict JSON with an answer and zero or one proposed
discriminated `Action` (`OpenAction`, `CloseAction`, or `AdjustStopAction`). It has
the existing bounded search tools, but it has no execution
tool. Invalid JSON retries under the same bounded retry policy as autonomous
decisions; final failure is visible and produces no proposal.

## Proposal and confirmation lifecycle

An actionable proposal is stored in SQLite with a random identifier, canonical
action JSON, derived preview JSON, creation time, context time, expiry, and
status. It expires after 120 seconds and is single-use. Preview size is rounded
to the venue's `szDecimals`; prices use the existing Hyperliquid tick formatter.
Required margin is `reference_mark * rounded_size / leverage`. Estimated TP net
is directional gross profit minus `(reference_mark + take_profit) * size *
FEE_RATE`; estimated stop loss is directional gross loss plus
`(reference_mark + stop) * size * FEE_RATE`, where the existing `FEE_RATE=0.00105`
is one-side taker plus Trench builder fee. The card labels these as estimates.

Confirmation performs these steps under the engine's trade-operation lock:

1. Atomically claim the pending proposal; repeated clicks cannot execute twice.
2. Fetch a completely fresh context snapshot.
3. For an entry, re-run every risk gate except the explicitly operator-bypassed
   autonomous max-concurrent check, plus available-margin, market lookup, and
   venue-order/position duplicate checks. For close/bracket changes,
   require the same ledger id, market, side, entry, and full size as the card and
   re-run current mark/side/bracket validation.
4. Reject the proposal as stale if it expired; its reference mark moved by more
   than 0.5%; or any action field differs from stored canonical JSON. For entries,
   also reject when the fresh guard changes leverage/margin mode, rounded size,
   or changes derived notional/margin by more than 1%. For close/bracket actions,
   reject when the position fingerprint changes; for brackets, also reject if
   either proposed level is no longer on the valid side of the mark. No silent
   requote is allowed.
5. Execute through the existing adapter with Trench builder fee. A shared action
   journal persists each irreversible boundary. Entry stages are `prepared`,
   `entry_submitted`, `entry_filled`, and `bracketing`; close and adjustment
   stages record `submitted` and the exact pre-action position/order fingerprint.
   No ambiguous venue mutation is automatically resubmitted after timeout/restart.
6. Record a normal decision with trigger `chat authorized <proposal-id>`, mark
   the proposal executed, and expose user-authorized provenance on the dashboard.

Any failure is terminal for that proposal and is displayed on its card. The user
must request a fresh quote rather than retrying ambiguous state.

Entry and bracket placement are not transactionally atomic at the venue. A new
`action_executions` table journals every autonomous action and chat-authorized
action before submission; it stores origin, kind, optional proposal/decision
identifiers, canonical action, exact pre-action position/order fingerprint,
creation/submission/response times, returned fill identifiers and price when
known, progress, status, and terminal result. For opens it also stores expected
rounded size and the absence/baseline size of any pre-existing venue position. If the
entry is confirmed filled but either protective trigger fails, Peri cancels any
partial trigger set and immediately submits a reduce-only market close. If that
close is ambiguous or fails, the execution journal and optional proposal enter
`needs_reconciliation`, a high-priority notification is sent, and all new entries
are blocked. Startup and the normal
reconciler inspect unfinished execution journals before trading: they never
resubmit an ambiguous mutation.

A recovered venue position is “matching” only when the journal recorded no
pre-existing position and Peri can identify exactly one post-journal open fill
for the same market, direction, and rounded size (within one venue size step),
using the returned fill identifier when available. Without an identifier, the
fill timestamp must fall between two seconds before recorded submission and the
earlier of 30 seconds after submission or the recorded client response/timeout
time; exactly one fill must match. A fill outside that bounded window or multiple
candidates means `manual_review`; zero matches after the window closes means the
execution failed. Peri never attaches brackets to
or closes exposure based only on market name. For one exact match, it adopts or
updates the position ledger with the venue entry/size/mode and stored thesis,
ensures both stored protective triggers match, records/links the decision if it
was not already durable, then atomically marks the execution journal and optional
proposal `executed`. If it cannot establish both brackets, it follows the
reduce-only cleanup path. `manual_review`, ambiguous cleanup, or any nonterminal
journal blocks all new entries until reconciliation reaches either a fully
bracketed, fully recorded matching position or confirmed no matching exposure.

For an unfinished full close, recovery marks it executed only when the exact
fingerprinted position is absent and a matching post-submission close fill exists;
an unchanged, partially changed, or replaced position becomes `manual_review`
without another close order. For an unfinished bracket change, recovery marks it
executed only when the exact same position remains and both live reduce-only
triggers equal the stored SL/TP; any other state becomes `manual_review` without
reapplying the change. These terminal transitions update the action journal,
proposal, position ledger, and decision provenance consistently. Any
`manual_review` action blocks new entries but does not prevent risk-reducing
autonomous closes.

Forward execution preserves existing protection. A full close submits and
confirms the reduce-only close before canceling its old brackets; if the close
fails or is ambiguous, the old SL/TP remain live. An SL/TP replacement snapshots
the old trigger OIDs, places and verifies the complete new reduce-only pair first,
then cancels only the old OIDs and verifies one final pair. Failure while placing
the new pair cancels only newly created triggers and leaves the old pair intact.
A crash with both pairs live is recovered from journaled old/new OIDs by keeping
the verified requested pair and canceling the old pair; ambiguity enters
`manual_review`. Peri never uses the current cancel-all-first sequence for an
authorized or autonomous bracket replacement.

## Persistence and API

SQLite adds:

- `chat_messages`: timestamp, role, content, context timestamp, optional proposal
  identifier, and metadata JSON containing the bounded search tool log/URLs;
- `trade_proposals`: immutable action/preview, lifecycle timestamps, status, and
  final execution or refusal result;
- `action_executions`: the pre-submission execution journal shared by autonomous
  and chat-authorized opens, closes, and bracket changes.

The loopback FastAPI service adds:

- `GET /api/chat` for bounded history;
- `POST /api/chat` with `{ "message": "..." }`;
- `POST /api/chat/proposals/{id}/confirm` with no model-controlled payload.

Messages are non-empty and capped at 4,000 characters. Proposal identifiers are
unguessable and all mutation endpoints stay loopback-only behind the same-origin
Next proxy. Periboard itself is bound to loopback so LAN clients cannot reach the
confirmation surface. The confirmation endpoint accepts only a server-stored
pending proposal; the browser never submits editable order fields.

SQLite retains chat history until deliberate operator maintenance. The model sees
the most recent 40 messages, with each message capped at 4,000 characters and a
20,000-character total history budget; older messages remain retrievable in the
UI through cursor pagination. `GET /api/chat` accepts `before_id`, defaults to 100,
and caps each page at 200 messages. `POST /api/chat` responds with the Vercel AI
SDK UI message SSE protocol (`x-vercel-ai-ui-message-stream: v1`). Qwen's strict
JSON envelope stays server-side: only the incrementally decoded `answer` string
is emitted as text deltas. Fresh-context, search-tool, proposal, status, result,
and explicit error events use typed data parts. A malformed response never gains
execution authority. The shadcn interface renders incomplete and final Markdown
with Streamdown, exposes stop/busy/error states, and allows up to the configured
180-second analyst timeout per attempt; model retries remain bounded by analyst
configuration. Assistant metadata, confirmation cards, and search provenance
round-trip through history after reload.

In `mode="dry"`, the complete proposal lifecycle executes against
`DryRunAdapter`, and every card/button/result says `paper` / `Confirm paper trade`.
In `mode="live"`, it says `live` / `Confirm live trade`. The UI never labels a
paper fill as a live order.

## Interface

Periboard adds an `Analyst` navigation item and full-height `/analyst` page:

- scrollable user/Qwen conversation with durable history;
- compact live-context stamp showing when Qwen's snapshot was taken;
- multiline composer with send state and clear failure text;
- an inline proposal card beneath the relevant Qwen response;
- a destructive-looking mode-aware confirmation action (`Confirm live trade` or
  `Confirm paper trade`) that includes market, side, margin mode, leverage, and
  expiry in its accessible label;
- explicit pending, expired, refused, executing, and executed states;
- after execution, actual entry, size, and position link/provenance.

The existing dashboard visual language, typography, responsive shell, and dark
theme are reused. The interface does not expose chain-of-thought; it presents the
analyst's answer, rationale, search citations returned by tools, and structured
trade facts.

## Concurrency and failure behavior

The autonomous cycle and confirmed chat execution cannot interleave their
context/execute critical sections. Chat analysis may run while the bot operates,
but confirmation always revalidates after it obtains the lock. Upstream context
failure, analyst failure, risk refusal, stale proposal, venue rejection, and
bracket failure are all loud. Partial venue state enters the explicit recovery
path and blocks new entries; it never degrades into a silently unbracketed or
differently sized order. An API request timeout does not cause a client retry to
duplicate an entry because proposal claiming is idempotent and recovery never
resubmits an ambiguous entry.

## Acceptance criteria

- Chat answers show evidence from the same fresh bundle supplied to Qwen's
  autonomous cycle and retain bounded history across reloads/restarts.
- Questions about currently open positions and orders reflect the upstream state
  fetched for that message, including its timestamp; upstream failure is shown
  explicitly rather than replaced with stale or invented values.
- Qwen may suggest an entry, full close, or atomic SL+TP replacement, or prepare
  one from a direct instruction.
- No chat message can directly execute an order.
- A pending confirmation card contains complete sizing, payoff, fee, bracket,
  margin-mode, freshness, and rationale information.
- Confirmation revalidates live state and executes at most once through existing
  risk rails and the Trench-routed adapter.
- Confirmed chat entries may exceed the autonomous three-position cap by
  bypassing only max-concurrent; all other gates and margin checks still apply.
- Autonomous and chat-generated entries choose and persist cross/isolated mode;
  the adapter no longer hardcodes isolated.
- The model schema and guard both reject leverage other than 10x/20x; requested
  20x on a 15x venue is refused rather than clamped, requested 10x on a 15x venue
  passes, and both leverage choices preserve the same stop-risk notional while
  changing required margin.
- Existing API/dashboard behavior remains compatible and all Python, TypeScript,
  lint, and production-build gates pass.
- Assistant text streams through the AI SDK protocol and renders with Streamdown;
  proposal data remains immutable server state outside the text stream.
