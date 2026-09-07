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

// Browser: same-origin proxy (avoids CORS — traderd binds only localhost, no CORS headers).
// Server (SSR/RSC): direct absolute URL — Next server-side fetch requires it.
const BASE = typeof window === "undefined" ? "http://127.0.0.1:7411" : "/traderd";

function isFixtures(): boolean {
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
};
