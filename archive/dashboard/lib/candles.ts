export type Candle = {
  t: number; // bar open, epoch ms
  o: number;
  h: number;
  l: number;
  c: number;
  v: number;
};

export type CandleInterval = "1m" | "5m" | "15m";

export const INTERVAL_MS: Record<CandleInterval, number> = {
  "1m": 60_000,
  "5m": 300_000,
  "15m": 900_000,
};

export function isFixtures(): boolean {
  return (
    typeof process !== "undefined" && process.env.NEXT_PUBLIC_FIXTURES === "1"
  );
}

export async function fetchCandles(
  coin: string,
  interval: CandleInterval,
  lookbackH = 12
): Promise<Candle[]> {
  if (isFixtures()) {
    const res = await fetch("/fixtures/candles.json", { cache: "no-store" });
    if (!res.ok) throw new Error(`fixture candles ${res.status}`);
    const all = (await res.json()) as Record<string, Candle[]>;
    const bars = all[coin];
    if (!bars) throw new Error(`no fixture candles for ${coin}`);
    return bars;
  }
  const qs = new URLSearchParams({
    coin,
    interval,
    lookback_h: String(lookbackH),
  });
  const res = await fetch(`/api/hl/candles?${qs}`, { cache: "no-store" });
  if (!res.ok) throw new Error(`candles ${coin} ${res.status}`);
  return (await res.json()) as Candle[];
}
