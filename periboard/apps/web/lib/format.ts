const IST_DATE_TIME = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Asia/Kolkata",
  day: "2-digit",
  month: "short",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
})

const IST_TIME = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Asia/Kolkata",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
})

export const usd = (n: number | null | undefined, digits = 2) =>
  n == null ? "—" : `${n < 0 ? "-" : ""}$${Math.abs(n).toFixed(digits)}`

export const signedUsd = (n: number | null | undefined) =>
  n == null ? "—" : `${n >= 0 ? "+" : "-"}$${Math.abs(n).toFixed(2)}`

export const px = (n: number | null | undefined) => {
  if (n == null) return "—"
  const digits = n >= 1000 ? 1 : n >= 10 ? 2 : 4
  return n.toFixed(digits)
}

export const pct = (n: number | null | undefined, digits = 2) =>
  n == null ? "—" : `${n >= 0 ? "+" : ""}${n.toFixed(digits)}%`

export function stamp(ts: number | null | undefined): string {
  if (ts == null) return "—"
  return `${IST_DATE_TIME.format(ts * 1000).replace(",", " ·")} IST`
}

export function istTime(ms: number): string {
  return `${IST_TIME.format(ms)} IST`
}

export function dayStamp(ts: number | null | undefined): string {
  if (!ts) return "—"
  return new Date(ts * 1000).toISOString().slice(5, 16).replace("T", " ") + "Z"
}

export function ago(ts: number | null | undefined): string {
  if (!ts) return "—"
  const s = Math.max(0, Date.now() / 1000 - ts)
  if (s < 90) return `${Math.round(s)}s ago`
  const m = s / 60
  if (m < 120) return `${Math.round(m)}m ago`
  return `${(m / 60).toFixed(1)}h ago`
}
