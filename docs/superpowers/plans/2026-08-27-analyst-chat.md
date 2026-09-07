# Analyst Chat and Authorized Trades Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a live-context Qwen chat in periboard that can answer account/position questions, prepare one fully validated entry/full-close/SL+TP action, and execute it exactly once only after explicit confirmation.

**Architecture:** Extend the existing single `Analyst` and `Engine.context_snapshot` rather than introducing another model or data index. Persist chat, immutable proposals, position margin mode, and a shared pre-submit execution journal in SQLite; route confirmation through fresh context and the existing Guard/adapter. Add a full-height Next.js Analyst page using the loopback FastAPI API.

**Tech Stack:** Python 3.13, Pydantic, SQLite WAL, FastAPI, Hyperliquid SDK, pytest, Next.js 16, React 19, TypeScript, Tailwind, Streamdown, Bun.

**Execution mode:** Inline on the current branch per the operator's “build it asap” instruction. Preserve unrelated worktree changes and commit each task independently.

---

## File structure

- `src/peri/models.py`: required margin-mode and 10x/20x entry contract; chat response schema.
- `src/peri/risk.py`: exact leverage, available-margin, and unchanged stop-risk gates.
- `src/peri/state.py`: position mode migration plus chat, proposal, and execution-journal persistence.
- `src/peri/analyst.py`: reusable context rendering and same-Qwen chat completion with bounded tools/history.
- `src/peri/engine.py`: targeted context, action-specific previews, proposal lifecycle, confirmation, and shared action journal.
- `src/peri/hl_adapter.py`: pass selected cross/isolated mode and expose actual venue mode.
- `src/peri/router.py`: preserve selected mode in dry execution and shared fee estimates.
- `src/peri/api.py`: bounded chat/history/confirm endpoints.
- `src/peri/app.py`: inject engine chat callbacks into the loopback API.
- `config.toml`: operator-authorized 20x ceiling.
- `tests/test_{models,risk,state,analyst,engine,hl_adapter,api}.py`: focused backend contracts.
- `periboard/apps/web/lib/api.ts`: chat/proposal types and typed mutation calls.
- `periboard/apps/web/lib/chat.ts`: pure proposal freshness/status helpers.
- `periboard/apps/web/lib/chat.test.ts`: Bun tests for UI state helpers.
- `periboard/apps/web/app/analyst/page.tsx`: durable chat and confirmation-card interface.
- `periboard/apps/web/components/shell.tsx`: Analyst navigation/full-height layout.
- `periboard/apps/web/package.json`: loopback-only production bind.

### Task 1: Enforce margin mode and exact 10x/20x leverage

**Files:**
- Modify: `tests/test_models.py`
- Modify: `tests/test_risk.py`
- Modify: `tests/test_hl_adapter.py`
- Modify: `tests/test_router.py`
- Modify: `tests/test_notifier.py`
- Modify: `tests/test_engine.py`
- Modify: `tests/test_analyst.py`
- Modify: `src/peri/models.py`
- Modify: `src/peri/risk.py`
- Modify: `src/peri/hl_adapter.py`
- Modify: `src/peri/analyst.py`
- Modify: `src/peri/engine.py`
- Modify: `hl_smoke.py`
- Modify: `config.toml`

- [ ] **Step 1: Write failing model and guard tests**

Add cases proving `margin_mode` is required, only `cross|isolated` validates,
only leverage `10|20` validates, 20x on a 15x venue is refused rather than
clamped, 10x on a 15x venue passes, and stop-risk notional is identical at 10x
and 20x while margin halves. Add an exact refusal case where derived required
margin is one cent above authoritative `available_margin`, plus an equality case
that passes.

```python
def test_entry_leverage_is_exactly_ten_or_twenty():
    with pytest.raises(ValidationError):
        OpenAction(..., leverage=15, margin_mode="isolated")

def test_requested_leverage_above_venue_is_refused_not_clamped(...):
    verdict = guard.gate_open(action(leverage=20), ..., market_max_lev=15,
                              available_margin=100, ...)
    assert verdict.reason == "requested leverage 20x exceeds venue maximum 15x"
```

- [ ] **Step 2: Run focused tests and verify RED**

Run: `uv run --group dev pytest -q tests/test_models.py tests/test_risk.py tests/test_hl_adapter.py tests/test_router.py tests/test_notifier.py tests/test_engine.py`  
Expected: failures for missing fields/signatures and hardcoded `is_cross=False`.

- [ ] **Step 3: Implement the minimal contracts**

Use literal schema fields:

```python
MarginMode = Literal["cross", "isolated"]
EntryLeverage = Literal[10, 20]

class OpenAction(BaseModel):
    ...
    leverage: EntryLeverage
    margin_mode: MarginMode
```

Add `margin_mode` to `Approved`; pass `available_margin` into `gate_open`; reject
unsupported leverage before sizing; remove leverage clamping; preserve the
existing risk-percent notional. After sizing, refuse when `margin >
available_margin` with both values in the reason; never produce a preview the
venue cannot collateralize. In the live adapter call:

```python
self.ex.update_leverage(
    int(ap.leverage), coin, is_cross=ap.margin_mode == "cross"
)
```

Set `max_leverage = 20.0` in `config.toml` without changing `risk_pct`.
Add each candidate's `MarketInfo.max_leverage` to `build_bundle` and render it in
the autonomous context. Update both autonomous and chat system guidance to
require exactly 10x/20x, skip markets below 10x, use the displayed venue maximum,
and choose/explain cross versus isolated from portfolio collateral/liquidation
risk. Add analyst/engine assertions for the rendered maximum and instructions.
Update every direct `OpenAction`/`Approved` constructor, analyst JSON example,
engine gate call, fixture, notifier/router test, and `hl_smoke.py` in the same
increment so required fields and the new `available_margin` argument never leave
the branch in a non-buildable intermediate state.

- [ ] **Step 4: Run focused tests and full gates**

Run: `uv run --group dev pytest -q tests/test_models.py tests/test_risk.py tests/test_hl_adapter.py tests/test_router.py tests/test_notifier.py tests/test_engine.py`  
Expected: PASS.  
Run: `uv run --group dev pytest -q` and `uv run --group dev ruff check src tests`  
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add config.toml hl_smoke.py src/peri/models.py src/peri/risk.py src/peri/hl_adapter.py src/peri/analyst.py src/peri/engine.py tests/test_models.py tests/test_risk.py tests/test_hl_adapter.py tests/test_router.py tests/test_notifier.py tests/test_engine.py tests/test_analyst.py
git commit -m "feat(peri): choose cross or isolated entries"
```

### Task 2: Persist actual position mode, chat, proposals, and execution journals

**Files:**
- Modify: `tests/test_state.py`
- Modify: `tests/test_engine.py`
- Modify: `tests/test_hl_adapter.py`
- Modify: `src/peri/state.py`
- Modify: `src/peri/engine.py`
- Modify: `src/peri/hl_adapter.py`

- [ ] **Step 1: Write failing persistence tests**

Cover legacy position migration to `unknown`, new/synced margin mode, paginated
chat history with context/tool metadata, immutable proposal retrieval, atomic
`pending -> authorizing` claim, terminal idempotency, and nonterminal action
journal enumeration.
Also prove one `BEGIN IMMEDIATE` terminal finalizer updates the action journal,
optional proposal, position row, and decision/provenance link in one commit and
fully rolls back all four when any statement fails.

```python
def test_claim_proposal_is_atomic_and_single_use(tmp_path):
    state = State(...)
    state.create_trade_proposal(...)
    assert state.claim_trade_proposal("p1")["status"] == "authorizing"
    assert state.claim_trade_proposal("p1") is None
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `uv run --group dev pytest -q tests/test_state.py`  
Expected: missing schema/method failures.

- [ ] **Step 3: Add additive SQLite schema and focused methods**

Add `positions.margin_mode` through `_migrate`, plus `chat_messages`,
`trade_proposals`, and `action_executions` tables. Keep JSON opaque at storage
boundaries and use conditional SQL updates for claims/transitions. Add dedicated
`finalize_open_action`, `finalize_close_action`, and `finalize_adjust_action`
methods that perform all related ledger/proposal/journal/decision statements
inside one explicit SQLite transaction and commit once.

Extract `(position["leverage"] or {})["type"]` from live clearinghouse positions,
normalize it to `cross|isolated|unknown`, include it in `account_snapshot`, and
make `adopt_external`/`sync_position_snapshot` overwrite legacy or stale ledger
mode before `build_bundle`. Add adapter/engine assertions that Qwen/dashboard see
the venue value after reconciliation.

- [ ] **Step 4: Run state tests and full gates**

Run: `uv run --group dev pytest -q tests/test_state.py` then full pytest/Ruff.  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/peri/state.py src/peri/engine.py src/peri/hl_adapter.py tests/test_state.py tests/test_engine.py tests/test_hl_adapter.py
git commit -m "feat(peri): persist analyst chat lifecycle"
```

### Task 3: Give the same Qwen a fresh conversational context

**Files:**
- Modify: `tests/test_analyst.py`
- Modify: `src/peri/models.py`
- Modify: `src/peri/analyst.py`

- [ ] **Step 1: Write failing chat transport tests**

Prove the chat request contains the full live bundle, actual margin mode,
available margin, recent decisions, targeted ticker, bounded prior messages, and
the current user request. Prove a normal answer may have no proposal and add a
valid + invalid case for every discriminated variant: complete `OpenAction`, full
`CloseAction`, and atomic `AdjustStopAction`. Tools remain read-only, and malformed
output exhausts bounded retries without a proposal.

- [ ] **Step 2: Verify RED**

Run: `uv run --group dev pytest -q tests/test_analyst.py`.

- [ ] **Step 3: Refactor context rendering and add `Analyst.chat`**

Extract the existing bundle sections into a reusable renderer; keep autonomous
JSON-only instructions separate. Add a chat system prompt whose only structured
output is:

```json
{"answer":"grounded response","proposal":null}
```

or one fully validated discriminated open, full-close, or atomic SL+TP proposal.
Feed at most 40 prior messages / 20,000
characters and reuse the current search tool loop. Never expose an execution
tool or chain-of-thought.

- [ ] **Step 4: Run focused/full gates and commit**

```bash
git add src/peri/analyst.py src/peri/models.py tests/test_analyst.py
git commit -m "feat(peri): add live-context analyst conversation"
```

### Task 4: Build previews, confirmation, and durable execution recovery

**Files:**
- Modify: `tests/test_engine.py`
- Modify: `tests/test_router.py`
- Modify: `tests/test_state.py`
- Modify: `src/peri/engine.py`
- Modify: `src/peri/router.py`
- Modify: `src/peri/hl_adapter.py`
- Modify: `src/peri/state.py`

- [ ] **Step 1: Write failing engine tests**

Cover: direct question returns fresh account/position data; explicit ticker is
forced into candidates; valid entry, full-close, and bracket preview formulas;
refusal creates no confirmation; expiry; >0.5% mark drift; entry size/notional/
margin drift; changed position fingerprints; wrong-side replacement brackets;
duplicate click; fresh gate refusal; successful paper/live actions; decision
provenance; cross/isolated propagation; and action-journal blocking.

Add failure-injection adapter cases for entry timeout, first/second bracket
failure, cleanup close failure, exact one-fill recovery, zero-fill failure, and
ambiguous matching fills entering `manual_review`. Assert recovery never
resubmits an entry. Add ambiguous full-close and bracket-update recovery cases
that only reconcile exact venue evidence and never resubmit. Assert all
nonterminal journals block new entries while risk-reducing closes remain allowed.
Add exact lower/upper no-ID fill-window boundary tests and unknown/ambiguous
ticker-resolution tests.
Assert `needs_reconciliation` and `manual_review` each send a high-priority
notifier message and remain visible in the persisted proposal result.

- [ ] **Step 2: Verify RED**

Run: `uv run --group dev pytest -q tests/test_engine.py tests/test_router.py tests/test_hl_adapter.py`.

- [ ] **Step 3: Implement targeted context and preview math**

Resolve exact ticker tokens against loaded contexts, add recent decision
summaries only for chat, calculate rounded size/margin/RR and fee-adjusted SL/TP
estimates with `FEE_RATE`; build close previews from full live position PnL/fees
and bracket previews from current/proposed levels. Persist the assistant message
and 120-second proposal.

- [ ] **Step 4: Implement confirmation and shared action journal**

Serialize autonomous/confirmed action critical sections. Claim once, refresh
context, enforce deterministic action-specific stale rules, and route both
origins through one journaled execution method. Persist every irreversible stage.
On bracket failure,
cancel partial triggers and reduce-only close; on ambiguity block entries and
reconcile by exact returned fill id or the bounded unique fill tuple.

For full closes, submit/confirm the reduce-only close before canceling old
brackets; ambiguous/failed closes leave protection untouched and never resubmit.
For SL+TP replacement, snapshot old OIDs, place and verify the complete new pair,
then cancel only the old OIDs. A forward placement failure cancels only newly
created triggers; a crash with both pairs is reconciled from journaled OIDs. Use
the Task 2 single-transaction terminal methods: open finalizes after both new
brackets exist, close after exact position absence + close fill, and adjustment
after the exact final trigger pair exists.

- [ ] **Step 5: Run focused/full gates and commit**

```bash
git add src/peri/engine.py src/peri/router.py src/peri/hl_adapter.py src/peri/state.py tests/test_engine.py tests/test_router.py tests/test_hl_adapter.py tests/test_state.py
git commit -m "feat(peri): confirm and recover authorized actions"
```

### Task 5: Expose the loopback chat API

**Files:**
- Modify: `tests/test_api.py`
- Modify: `src/peri/api.py`
- Modify: `src/peri/app.py`

- [ ] **Step 1: Write failing endpoint tests**

Test bounded/paginated history, missing callbacks returning 503, empty/oversized
messages returning 422, callback execution off the event loop, Vercel AI SDK SSE
parts/headers, successful reply/proposal shape, confirmation success,
stale/refused conflict, and idempotent already-executed response.

- [ ] **Step 2: Verify RED**

Run: `uv run --group dev pytest -q tests/test_api.py`.

- [ ] **Step 3: Add typed request models and callbacks**

Use Pydantic request validation and `asyncio.to_thread` for chat/confirmation.
Stream `start`, decoded answer deltas, typed context/tool/proposal/result events,
and `finish` under `x-vercel-ai-ui-message-stream: v1`; never stream hidden
reasoning or the raw JSON envelope. The confirm route accepts no order fields.
Extend `serve` and `app.main` wiring with `engine.chat`,
`engine.confirm_trade`, and `state.chat_history` callbacks.

- [ ] **Step 4: Run focused/full gates and commit**

```bash
git add src/peri/api.py src/peri/app.py tests/test_api.py
git commit -m "feat(peri): expose authorized analyst chat api"
```

### Task 6: Build the Analyst interface

**Files:**
- Create: `periboard/apps/web/lib/chat.ts`
- Create: `periboard/apps/web/lib/chat.test.ts`
- Create: `periboard/apps/web/app/analyst/page.tsx`
- Create: `periboard/apps/web/components/analyst/analyst-chat.tsx`
- Create: `periboard/apps/web/components/analyst/proposal-card.tsx`
- Modify: `periboard/apps/web/lib/api.ts`
- Modify: `periboard/apps/web/components/shell.tsx`
- Modify: `periboard/apps/web/package.json`

- [ ] **Step 1: Write failing pure UI-state tests**

Cover proposal expiry/status labels, live versus paper confirmation copy, and
button eligibility. Run `bun test apps/web/lib/chat.test.ts` and verify RED.

- [ ] **Step 2: Add typed API and helpers**

Define AI SDK UI-message data parts plus durable message, tool result, preview,
proposal, and confirmation result types. Configure `DefaultChatTransport` to
send only the newest user text, add `before_id` cursor pagination with a visible
load-older control, and preserve readable daemon errors. Bind `next dev/start`
to `127.0.0.1`.

- [ ] **Step 3: Build `/analyst`**

Create a responsive full-height two-column desktop / single-column mobile
console: conversation, context timestamps, Streamdown answers, tool/source list,
multiline composer, busy/error states, and inline immutable action-specific
proposal cards for entries, full closes, and SL+TP replacements. The mode-aware
confirmation button shows all decisive fields and cannot be triggered
by Enter in the composer. After confirmation, render actual execution or exact
refusal/stale state and refresh live dashboard data.
Render `needs_reconciliation`/`manual_review` as persistent high-severity states
rather than a retry button.

- [ ] **Step 4: Add navigation and validate**

Add `Analyst` to `NAV`, allow the page to use full viewport width, then from
`/home/esoteric/botta/periboard` run:

```bash
bun test
bun run --filter web typecheck
bun run --filter web lint
bun run --filter web build
```

Expected: tests/typecheck/build PASS; no new lint warnings.

- [ ] **Step 5: Commit in nested repository**

```bash
git -C periboard add apps/web
git -C periboard commit -m "feat(web): add authorized analyst console"
```

### Task 7: Adversarial review, deployment, and live read-only verification

**Files:**
- Modify if required: `docs/superpowers/STATUS.md`
- Refresh: `graft/`

- [ ] **Step 1: Run all gates from clean scoped diffs**

Run Python pytest/Ruff and periboard Bun tests/typecheck/lint/build. Inspect both
git statuses and ensure only scoped files are committed.

- [ ] **Step 2: Run read-only adversarial review**

Audit exact file:line evidence for unintended execution by chat text, mutable
confirmation payloads, duplicate confirmation, stale state, unsupported leverage,
cross-mode propagation, partial brackets, crash recovery, LAN exposure, and
history poisoning. Include confirmed full-close/SL+TP behavior, preservation of
old protection on forward failure, and every `manual_review` recovery state.
Verdict must be GO before deployment.

- [ ] **Step 3: Refresh graph and restart exact services**

Run `graft build`. Capture current Peri/dashboard PIDs and ports, TERM only those
exact processes, restart using existing service commands, and retain rollback
commit IDs.

- [ ] **Step 4: Verify deployed behavior without placing a live trade**

Check system/process/ports, loopback health, a fresh chat question
about a current position, a generated proposal card, and cancellation/expiry
without confirmation. Do not press live confirmation during deployment testing.
Verify autonomous cycles continue and dashboard WebSocket remains fresh.

- [ ] **Step 5: Record final project memory and report**

Add one concise project OptMem note with commits, endpoints, and authorization
invariant. Report the live URL, tests, commits, current position/order snapshot,
and the fact that no validation trade was executed.
