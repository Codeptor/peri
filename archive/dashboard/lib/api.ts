// Types mirror spec §4 field-for-field — do not drift
export type Features = {
  r5m: number;
  r1h: number;
  r24h: number;
  vol1h: number;
  funding_z: number;
  range_pos: number;
};

export type MarketRow = {
  market: string;
  mid: number;
  mark: number;
  oracle: number;
  funding: number;
  open_interest: number;
  day_ntl_vlm: number;
  prev_day_px: number;
  features: Features | null;
};

export type Snapshot = {
  ts: number;
  markets: MarketRow[];
};

export type Nominee = {
  ts: number;
  market: string;
  side_hint: "long" | "short";
  score: number;
  features: Features;
};

export type Position = {
  id: number;
  market: string;
  side: "long" | "short";
  entry_px: number;
  size: number;
  leverage: number;
  margin: number;
  sl_px: number;
  tp_px: number;
  opened_ts: number;
  mark_px: number;
  unrealized_pnl: number;
  roe: number;
  analyst: string;
};

export type Trade = {
  id: number;
  position_id: number;
  market: string;
  action: string;
  px: number;
  size: number;
  fee: number;
  realized_pnl: number | null;
  ts: number;
  analyst: string;
};

export type EquityPoint = {
  ts: number;
  equity: number;
};

export type Decision = {
  ts: number;
  market: string;
  action: "open" | "skip";
  side: "long" | "short" | null;
  conviction: number;
  thesis: string;
  horizon_hours: number | null;
  vetoed: boolean;
  executed: boolean;
  reason: string;
  analyst: string;
};

export type NewsItem = {
  id: number;
  ts: number;
  source: string;
  title: string;
  body: string;
  url: string;
  markets: string[];
};

export type Health = {
  ok: boolean;
  uptime_s: number;
  ws_connected: boolean;
  markets_tracked: number;
  kill_switch?: boolean;
  equity?: number;
};

// ── /api/gates ── live entry-gate state, mirroring traderd `api::GatesResp` field-for-field.
// Every field is a projection of the rails the entry path actually runs, never a re-derivation.
export type KillGate = {
  /** the latch itself: true means every entry is refused until 00:00 UTC */
  active: boolean;
  /** equity the UTC day opened at; the floor is `day_open * (1 - threshold_px_pct/100)` */
  day_open: number;
  threshold_px_pct: number;
};

export type DailyGate = {
  count: number;
  cap: number;
};

export type MorningGate = {
  /** the budget only binds while this is true (12:00:00.000 UTC releases it) */
  before_noon_utc: boolean;
  count: number;
  budget: number;
};

export type StalenessGate = {
  /** age of the state an entry would rest on: max(snapshot stamp age, ws frame age) */
  age_ms: number;
  max_ms: number;
  stale: boolean;
};

export type RegimeGate = {
  /** null when the BTC row or its features are absent — the gate is then inactive */
  btc_vol1h: number | null;
  max: number;
  /** true only while the gate is REFUSING entries, not merely configured */
  active: boolean;
};

export type PerMarketGate = {
  market: string;
  entries_today: number;
  cap: number;
};

export type CooldownGate = {
  market: string;
  until_ts: number;
  /** "sl" = the longer post-stop window; "other" = the base window after tp / veto / time stop */
  cause: "sl" | "other";
};

export type GatesState = {
  kill: KillGate;
  daily: DailyGate;
  morning: MorningGate;
  staleness: StalenessGate;
  regime: RegimeGate;
  /** markets with at least one entry today, not only the ones already capped out */
  per_market: PerMarketGate[];
  /** only cooldowns still open at request time */
  cooldowns: CooldownGate[];
};

// ── /api/analytics ── decision quality, mirroring traderd `analytics::AnalyticsResp`.
export type ConvictionBucket = {
  /** fixed labels, always all three rows: "0.70-0.75" | "0.75-0.80" | "0.80+" */
  bucket: string;
  closes: number;
  wins: number;
  net_pnl: number;
};

export type MarketRecord = {
  market: string;
  trades: number;
  net_pnl: number;
  fees: number;
};

export type ExitMixDay = {
  /** UTC day, "YYYY-MM-DD"; ascending, today plus the previous 13 days */
  date: string;
  tp: number;
  sl: number;
  veto_close: number;
  other: number;
};

/**
 * One replayed veto — mirrors `analytics::CounterfactualRow`. `bracket_pnl - actual_pnl`
 * is the cost of that veto; the dashboard shows `actual - bracket`, so a positive edge
 * means closing early beat the position's own bracket.
 */
export type CounterfactualRow = {
  position_id: number;
  market: string;
  side: "long" | "short";
  closed_ts: number;
  actual_pnl: number;
  /** exit-side pnl the bracket would have paid; entry fee excluded on both sides */
  bracket_pnl: number;
  /** "tp" | "sl" | "expiry" — `analytics::BracketOutcome::as_str` */
  bracket_outcome: "tp" | "sl" | "expiry";
};

export type CounterfactualSummary = {
  computed: number;
  pending: number;
  /** realized pnl of the vetoed closes that resolved */
  net_actual: number;
  /** what those positions' own SL/TP brackets would have paid instead */
  net_bracket: number;
  /**
   * The `CF_ROWS_MAX` (50) most recently closed computed counterfactuals, newest first.
   * A window onto the same rows the totals sum — the totals stay whole-history, so past
   * 50 replays the rows no longer add up to `net_actual` / `net_bracket`.
   */
  rows: CounterfactualRow[];
};

export type Analytics = {
  conviction_buckets: ConvictionBucket[];
  per_market: MarketRecord[];
  exit_mix_daily: ExitMixDay[];
  counterfactuals: CounterfactualSummary;
};

export type AnalystAction = {
  type: "open" | "close";
  market: string;
  side: "long" | "short";
  sl_pct: number;
  tp_pct: number;
  conviction: number;
  executed: boolean;
  gate_refusals: string[];
};

export type AnalystChatMessage = {
  role: "user" | "analyst";
  ts: number;
  text: string;
  action_json: string | null;
};

export type AnalystCall = {
  id: number;
  ts: number;
  market: string;
  trigger: "decide" | "review" | "chat";
  prompt: string;
  response_raw: string;
  outcome_kind: string;
  parsed: Record<string, unknown> | null;
  latency_ms: number;
  analyst: string;
};

export type AnalystLeaderboardRow = {
  model: string;
  enabled: boolean;
  positions_open: number;
  closes: number;
  wins: number;
  win_rate: number;
  realized_pnl: number;
  unrealized_pnl: number;
  decides: number;
};

// Browser: same-origin proxy (avoids CORS — traderd binds only localhost, no CORS headers).
// Server (SSR/RSC): direct absolute URL — Next server-side fetch requires it.
const BASE = typeof window === "undefined" ? "http://127.0.0.1:7411" : "/traderd";

export function isFixtures(): boolean {
  return typeof process !== "undefined" && process.env.NEXT_PUBLIC_FIXTURES === "1";
}

async function fetchJSON<T>(path: string, init?: RequestInit): Promise<T> {
  const url = `${BASE}${path}`;
  const res = await fetch(url, { cache: "no-store", ...init });
  if (!res.ok) throw new Error(`${path} ${res.status}`);
  return (await res.json()) as T;
}

// fixtures mode: read from /fixtures/*.json served via /public/fixtures
async function fetchFixture<T>(name: string): Promise<T> {
  // client: fetch from public; server: dynamic import fallback
  if (typeof window !== "undefined") {
    const res = await fetch(`/fixtures/${name}.json`, { cache: "no-store" });
    if (!res.ok) throw new Error(`fixture ${name} ${res.status}`);
    return (await res.json()) as T;
  } else {
    // server-side: import via fs
    const fs = await import("fs/promises");
    const path = await import("path");
    const p = path.join(process.cwd(), "fixtures", `${name}.json`);
    try {
      const txt = await fs.readFile(p, "utf-8");
      return JSON.parse(txt) as T;
    } catch {
      // fallback to public/fixtures for Next build cwd = dash/
      const p2 = path.join(process.cwd(), "public", "fixtures", `${name}.json`);
      const txt2 = await fs.readFile(p2, "utf-8");
      return JSON.parse(txt2) as T;
    }
  }
}

export const api = {
  async snapshot(): Promise<Snapshot> {
    if (isFixtures()) return fetchFixture<Snapshot>("snapshot");
    return fetchJSON<Snapshot>("/api/snapshot");
  },
  async nominees(): Promise<Nominee[]> {
    if (isFixtures()) return fetchFixture<Nominee[]>("nominees");
    return fetchJSON<Nominee[]>("/api/nominees");
  },
  async positions(): Promise<Position[]> {
    if (isFixtures()) return fetchFixture<Position[]>("positions");
    return fetchJSON<Position[]>("/api/positions");
  },
  async trades(limit = 100): Promise<Trade[]> {
    if (isFixtures()) {
      const all = await fetchFixture<Trade[]>("trades");
      return all.slice(0, limit);
    }
    return fetchJSON<Trade[]>(`/api/trades?limit=${limit}`);
  },
  async equity(points = 500): Promise<EquityPoint[]> {
    if (isFixtures()) {
      const all = await fetchFixture<EquityPoint[]>("equity");
      return all.slice(-points);
    }
    return fetchJSON<EquityPoint[]>(`/api/equity?points=${points}`);
  },
  async decisions(limit = 50): Promise<Decision[]> {
    if (isFixtures()) {
      const all = await fetchFixture<Decision[]>("decisions");
      return all.slice(0, limit);
    }
    return fetchJSON<Decision[]>(`/api/decisions?limit=${limit}`);
  },
  async news(limit = 100): Promise<NewsItem[]> {
    if (isFixtures()) {
      const all = await fetchFixture<NewsItem[]>("news");
      return all.slice(0, limit);
    }
    return fetchJSON<NewsItem[]>(`/api/news?limit=${limit}`);
  },
  async health(): Promise<Health> {
    if (isFixtures()) return fetchFixture<Health>("health");
    return fetchJSON<Health>("/api/health");
  },
  // 503 when the gate state is wedged — the caller renders that, it never crashes the page.
  async gates(): Promise<GatesState> {
    if (isFixtures()) return fetchFixture<GatesState>("gates");
    return fetchJSON<GatesState>("/api/gates");
  },
  async analytics(): Promise<Analytics> {
    if (isFixtures()) return fetchFixture<Analytics>("analytics");
    return fetchJSON<Analytics>("/api/analytics");
  },
  async analystHistory(limit = 100): Promise<AnalystChatMessage[]> {
    if (isFixtures()) {
      const data = await fetchFixture<{ messages: AnalystChatMessage[] }>("analyst-history");
      return data.messages.slice(-limit);
    }
    const data = await fetchJSON<{ messages: AnalystChatMessage[] }>(
      `/api/analyst/chat/history?limit=${limit}`
    );
    return data.messages;
  },
  async analystCalls(limit = 50): Promise<AnalystCall[]> {
    if (isFixtures()) {
      const data = await fetchFixture<{ calls: AnalystCall[] }>("analyst-calls");
      return data.calls.slice(0, limit);
    }
    const data = await fetchJSON<{ calls: AnalystCall[] }>(`/api/analyst/calls?limit=${limit}`);
    return data.calls;
  },
  async analystLeaderboard(): Promise<AnalystLeaderboardRow[]> {
    if (isFixtures()) {
      const data = await fetchFixture<{ models: AnalystLeaderboardRow[] }>("analyst-leaderboard");
      return data.models;
    }
    const data = await fetchJSON<{ models: AnalystLeaderboardRow[] }>("/api/analyst/leaderboard");
    return data.models;
  },
};
