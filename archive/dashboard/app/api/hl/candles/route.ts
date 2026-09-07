import { type NextRequest, NextResponse } from "next/server";

// Read-only public market data proxy — no keys, no signing (PAPER invariant).
// coin naming verified 2026-08-09: traderd market names (incl. `xyz:` HIP-3
// prefix) map 1:1 to candleSnapshot coins; bare HIP-3 names 500.
const HL = "https://api.hyperliquid.xyz/info";
const INTERVALS = new Set(["1m", "5m", "15m"]);
const COIN_RE = /^[A-Za-z0-9:_-]{1,24}$/;

type RawCandle = {
  t: number;
  o: string;
  h: string;
  l: string;
  c: string;
  v: string;
};

export async function GET(req: NextRequest) {
  const coin = req.nextUrl.searchParams.get("coin") ?? "";
  const interval = req.nextUrl.searchParams.get("interval") ?? "1m";
  const lookbackH = Math.min(
    Math.max(Number(req.nextUrl.searchParams.get("lookback_h") ?? 12), 1),
    72
  );
  if (!(COIN_RE.test(coin) && INTERVALS.has(interval))) {
    return NextResponse.json({ error: "bad params" }, { status: 400 });
  }
  const end = Date.now();
  const start = end - lookbackH * 3_600_000;
  const res = await fetch(HL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      type: "candleSnapshot",
      req: { coin, interval, startTime: start, endTime: end },
    }),
    cache: "no-store",
  });
  if (!res.ok) {
    return NextResponse.json({ error: `hl ${res.status}` }, { status: 502 });
  }
  const raw = (await res.json()) as RawCandle[];
  if (!Array.isArray(raw) || raw.length === 0) {
    return NextResponse.json({ error: "no candles" }, { status: 404 });
  }
  return NextResponse.json(
    raw.map((k) => ({ t: k.t, o: +k.o, h: +k.h, l: +k.l, c: +k.c, v: +k.v }))
  );
}
