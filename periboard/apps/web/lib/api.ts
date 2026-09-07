// Typed client for the peri daemon API (docs/dashboard-api.md).
// Client-side goes through the Next rewrite (/peri) so the browser never
// crosses origins; server-side hits the daemon directly.

const BASE = typeof window === "undefined" ? "http://127.0.0.1:7411" : "/peri"

export type Status = {
  mode: "dry" | "live"
  network: string
  model: string
  cycle_secs: number
  cycle_secs_quiet: number
  cycle_secs_active: number
  paused: boolean
  pause_state: PauseState
  resting_entries: RestingEntry[]
  version: string
  last_decision_ts: number | null
  open_positions: number
  peri_open_positions: number
  external_positions: number
  max_concurrent: number
  realized_total: number
  realized_close_count: number
  realized_scope: "recent venue fills" | "Peri ledger"
  runtime: RuntimeStatus
}

export type WakeResult = { ok: true; already_pending: boolean }

export type PauseState = {
  paused: boolean
  ts?: number
  by?: string
  changed?: boolean
  cancelled_entries?: string[]
}

export type StuckExecution = {
  id: string
  created_ts: number
  updated_ts: number
  origin: string
  kind: string
  stage: string
  status: string
  action: Record<string, unknown>
  result: { reason?: string } | null
}

export type Cohort = {
  id: string
  label: string
  range: string
  traders: number
  long_pct: number | null
  sentiment: string
}

export type AssetBias = {
  coin?: string
  smart_long_pct: number | null
  crowd_long_pct: number | null
  divergence: number | null
  long_traders?: number | null
  short_traders?: number | null
  cohorts?: { id: string; label: string; long_pct: number; sentiment: string }[]
}

export type CohortBias = {
  cohorts: Cohort[]
  total_traders: number | null
  assets: Record<string, AssetBias>
  fetched_ts: number | null
}

export type CalendarEvent = {
  id: number
  ts: number
  title: string
  impact: "high" | "medium" | "low"
  scope: string | null
}

export type OperatorNote = { id: number; ts: number; text: string }

export type OperatorBias = { text?: string; ts?: number; by?: string }

export type Lesson = {
  id: number
  ts: number
  market: string | null
  text: string
  source: "analyst" | "operator"
  decision_id: number | null
  pinned: number
}

export type PerfBucket = {
  n: number
  wins: number
  win_rate: number | null
  pnl: number
  avg_r: number | null
  median_hold_mins: number | null
}

export type Performance = {
  overall: PerfBucket
  by_entry_style: Record<string, PerfBucket>
  by_side: Record<string, PerfBucket>
  by_range_position: Record<string, PerfBucket>
  by_close_reason: Record<string, PerfBucket>
  worst_markets: Record<string, PerfBucket>
  best_markets: Record<string, PerfBucket>
}

export type RestingEntry = {
  id: number
  market: string
  side: "long" | "short"
  entry_px: number
  size: number
  notional: number
  leverage: number
  stop_px: number
  tp_px: number
  conviction: number | null
  rationale: string | null
  placed_ts: number
  expires_ts: number
}

type ActionBase = {
  market: string
  rationale: string
  side?: "long" | "short"
  conviction?: number
  stop?: number
  take_profit?: number
  leverage?: 10 | 20
  margin_mode?: "cross" | "isolated"
  source?: "own" | "mirror"
  mirror_msg_id?: number | null
  invalidation?: string
}

export type OpenAction = ActionBase & {
  kind: "open"
  side: "long" | "short"
  conviction: number
  stop: number
  take_profit: number
  leverage: 10 | 20
  margin_mode: "cross" | "isolated"
  source: "own" | "mirror"
  mirror_msg_id: number | null
  invalidation: string
}

export type CloseAction = ActionBase & { kind: "close" }

export type AdjustStopAction = ActionBase & {
  kind: "adjust_stop"
  stop: number
  take_profit: number
}

export type Action = OpenAction | CloseAction | AdjustStopAction

export type ProposalStatus =
  | "pending"
  | "authorizing"
  | "executed"
  | "refused"
  | "failed"
  | "expired"
  | "cancelled"
  | "needs_reconciliation"
  | "manual_review"

export type TradeProposal = {
  id: string
  created_ts: number
  context_ts: number
  expires_ts: number
  action: Action
  preview: Record<string, unknown>
  status: ProposalStatus
  claimed_ts: number | null
  finished_ts: number | null
  result: Record<string, unknown> | null
}

export type ChatRecord = {
  id: number
  ts: number
  role: "user" | "assistant"
  content: string
  context_ts: number | null
  proposal_id: string | null
  metadata: Record<string, unknown>
  proposal: TradeProposal | null
}

export type ChatContextEvent = {
  as_of_ts: number
  account_equity: number
  available_margin: number
  open_positions: number
  open_orders: number
  venue_fills: number
  telegram_messages: number
  news_items: number
  candidates: string[]
}

export type ChatToolEvent = {
  phase: "start" | "result"
  tool: "web_search" | "deep_search" | string
  args: Record<string, unknown>
  result?: string
}

export type ChatResult = {
  message: ChatRecord
  proposal: TradeProposal | null
}

export type ToolCall = {
  tool: string
  args: { query?: string; [k: string]: unknown }
  result: string
}

export type Decision = {
  id: number
  ts: number
  trigger: string
  market_view: string | null
  model: string | null
  latency_ms: number | null
  status: "ok" | "error"
  reasoning: string | null
  prompt?: string | null
  actions: Action[]
  tool_log: ToolCall[]
}

export type Position = {
  id: number
  market: string
  side: "long" | "short"
  entry_px: number
  size: number
  notional: number
  leverage: number
  stop_px: number | null
  tp_px: number | null
  conviction: number | null
  source: string
  rationale: string | null
  invalidation: string | null
  status: string
  opened_ts: number
}

export type RuntimeStatus = {
  phase: "idle" | "context" | "analyst" | "executing"
  trigger?: string | null
  started_ts?: number | null
  snapshot_ts?: number | null
  last_trigger?: string | null
  last_started_ts?: number | null
  last_finished_ts?: number | null
}

export type LiveAccount = {
  abstraction: string
  equity: number
  spot_usdc_total: number
  held_collateral: number
  available_margin: number
  total_margin_used: number
  account_value_by_dex: Record<string, number>
  margin_used_by_dex: Record<string, number>
  withdrawable_by_dex: Record<string, number>
}

export type LivePosition = {
  id: number | null
  market: string
  side: "long" | "short"
  size: number
  entry_px: number
  leverage: number
  margin_mode: "cross" | "isolated" | "unknown"
  margin: number
  position_value: number
  upnl: number
  liquidation_px: number | null
  roe: number | null
  source: string
  stop_px: number | null
  tp_px: number | null
  conviction: number | null
  rationale: string | null
  invalidation: string | null
  opened_ts: number | null
}

export type LiveOrder = {
  oid: number | string
  market: string
  role: "stop_loss" | "take_profit" | "entry"
  side: "buy" | "sell"
  size: number | null
  limit_px: number | null
  trigger_px: number | null
  reduce_only: boolean
  placed_ts: number | null
  position_source: string | null
  placed_by: "Qwen via Peri" | "External / manual"
  route: "Trench" | "Venue"
  decision_id: number | null
  attribution: "exact decision match" | "unattributed"
}

export type LiveClose = Close & {
  id: string
  size: number
  gross_pnl: number
  fee: number
  close_reason: "venue"
}

export type LiveRealized = {
  total: number
  close_count: number
  scope: "recent venue fills" | "Peri ledger"
}

export type LiveSnapshot = {
  as_of_ts: number
  stale: boolean
  stale_reason: string | null
  account: LiveAccount
  positions: LivePosition[]
  orders: LiveOrder[]
  realized: LiveRealized
  closes: Array<Close | LiveClose>
  runtime: RuntimeStatus
}

export type Close = {
  market: string
  side: "long" | "short"
  entry_px: number
  close_px: number
  realized_pnl: number
  close_reason: "tp" | "sl" | "analyst" | "external" | "venue"
  conviction: number | null
  closed_ts: number
}

export type Refusal = {
  id: number
  ts: number
  market: string | null
  action_json: string
  reason: string
}

export type Daily = {
  day: string
  open_equity: number
  entries: number
  kill_tripped: number
}

export type NewsItem = { source: string; ts: number; text: string }

export type TgMessage = {
  msg_id: number
  ts: number
  sender: string | null
  text: string
  is_caller: number
  image_desc: string | null
}

async function get<T>(path: string): Promise<T> {
  const r = await fetch(`${BASE}${path}`, { cache: "no-store" })
  if (!r.ok) throw new Error(`${path} -> ${r.status}`)
  return (await r.json()) as T
}

async function post<T>(path: string): Promise<T> {
  const r = await fetch(`${BASE}${path}`, { method: "POST" })
  if (!r.ok) throw new Error(`${path} -> ${r.status}`)
  return (await r.json()) as T
}

async function postResult<T>(path: string): Promise<T> {
  const r = await fetch(`${BASE}${path}`, { method: "POST" })
  const body = (await r.json()) as T & { detail?: string }
  if (!r.ok && r.status !== 409) {
    throw new Error(body.detail ?? `${path} -> ${r.status}`)
  }
  return body
}

export const api = {
  status: () => get<Status>("/api/status"),
  live: () => get<LiveSnapshot>("/api/live"),
  wake: () => post<WakeResult>("/api/wake"),
  pause: () => post<PauseState & { ok: true }>("/api/pause"),
  lessons: (limit = 50) => get<Lesson[]>(`/api/lessons?limit=${limit}`),
  cohortBias: () => get<CohortBias>("/api/cohort-bias"),
  calendar: (days = 10) => get<CalendarEvent[]>(`/api/calendar?days=${days}`),
  notes: (limit = 20) => get<OperatorNote[]>(`/api/notes?limit=${limit}`),
  operatorBias: () => get<OperatorBias>("/api/bias"),
  setOperatorBias: async (text: string) => {
    const r = await fetch(`${BASE}/api/bias`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    })
    if (!r.ok) throw new Error(`/api/bias -> ${r.status}`)
    return (await r.json()) as OperatorBias
  },
  addNote: async (text: string) => {
    const r = await fetch(`${BASE}/api/notes`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text }),
    })
    if (!r.ok) throw new Error(`/api/notes -> ${r.status}`)
    return (await r.json()) as { ok: true }
  },
  executions: () => get<StuckExecution[]>("/api/executions"),
  resolveExecution: async (id: string) => {
    const r = await fetch(`${BASE}/api/executions/${id}/resolve`, { method: "POST" })
    const body = (await r.json()) as { ok?: true; detail?: string }
    if (!r.ok) throw new Error(body.detail ?? `resolve -> ${r.status}`)
    return body
  },
  performance: (days = 0) => get<Performance>(`/api/performance?days=${days}`),
  addLesson: async (text: string, market?: string | null) => {
    const r = await fetch(`${BASE}/api/lessons`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text, market: market || null }),
    })
    if (!r.ok) throw new Error(`/api/lessons -> ${r.status}`)
    return (await r.json()) as { ok: true; id: number | null; duplicate: boolean }
  },
  forgetLesson: async (id: number) => {
    const r = await fetch(`${BASE}/api/lessons/${id}`, { method: "DELETE" })
    if (!r.ok) throw new Error(`/api/lessons/${id} -> ${r.status}`)
    return (await r.json()) as { ok: true }
  },
  resume: () => post<PauseState & { ok: true }>("/api/resume"),
  decisions: (limit = 50) => get<Decision[]>(`/api/decisions?limit=${limit}`),
  decision: (id: number | string) => get<Decision>(`/api/decisions/${id}`),
  positions: () => get<Position[]>("/api/positions"),
  closes: (limit = 100) => get<Close[]>(`/api/closes?limit=${limit}`),
  refusals: (limit = 50) => get<Refusal[]>(`/api/refusals?limit=${limit}`),
  daily: (limit = 60) => get<Daily[]>(`/api/daily?limit=${limit}`),
  news: (limit = 30) => get<NewsItem[]>(`/api/news?limit=${limit}`),
  tg: (limit = 50) => get<TgMessage[]>(`/api/tg?limit=${limit}`),
  chatHistory: (limit = 100, beforeId?: number) =>
    get<ChatRecord[]>(
      `/api/chat?limit=${limit}${beforeId == null ? "" : `&before_id=${beforeId}`}`
    ),
  confirmProposal: (id: string) =>
    postResult<TradeProposal>(
      `/api/chat/proposals/${encodeURIComponent(id)}/confirm`
    ),
}
