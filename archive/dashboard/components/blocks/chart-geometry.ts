/**
 * Shared geometry + statistics for the hand-rolled SVG chart primitives.
 *
 * Every chart in `components/blocks` draws through these helpers so curves,
 * bar corners, domains and axis ticks are literally the same maths — a chart
 * that looks different from its neighbour is then a design choice, not drift.
 */

/** Axis/tick styling, shared so every chart's ticks are one visual language. */
export const AXIS_TICK_CLASS = "font-mono"
export const AXIS_TICK_SIZE = 10
export const AXIS_TICK_FILL = "var(--muted-foreground)"
/** Gridlines, baselines, crosshairs — the hairline weight of the design system. */
export const GRID_STROKE = "var(--border)"
/** Guides that assert a level (targets, thresholds) — dashed, never solid. */
export const GUIDE_DASH = "4 4"

/**
 * Read at animation time rather than cached: the preference can change
 * mid-session, and an animation that already respects it costs nothing to ask.
 */
export function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  )
}

/** Monotone cubic through every point — smooth, never overshoots the data. */
export function monotonePath(pts: [number, number][]): string {
  const n = pts.length
  if (n === 0) return ""
  if (n === 1) return `M ${pts[0][0]} ${pts[0][1]}`
  if (n === 2) return `M ${pts[0][0]} ${pts[0][1]} L ${pts[1][0]} ${pts[1][1]}`

  const dx: number[] = []
  const slope: number[] = []
  for (let i = 0; i < n - 1; i++) {
    dx[i] = pts[i + 1][0] - pts[i][0]
    slope[i] = dx[i] === 0 ? 0 : (pts[i + 1][1] - pts[i][1]) / dx[i]
  }

  const m: number[] = new Array(n)
  m[0] = slope[0]
  m[n - 1] = slope[n - 2]
  for (let i = 1; i < n - 1; i++) {
    if (slope[i - 1] * slope[i] <= 0) {
      m[i] = 0
    } else {
      const w1 = 2 * dx[i] + dx[i - 1]
      const w2 = dx[i] + 2 * dx[i - 1]
      m[i] = (w1 + w2) / (w1 / slope[i - 1] + w2 / slope[i])
    }
  }

  let d = `M ${pts[0][0]} ${pts[0][1]}`
  for (let i = 0; i < n - 1; i++) {
    const h = dx[i] / 3
    d += ` C ${pts[i][0] + h} ${pts[i][1] + m[i] * h} ${pts[i + 1][0] - h} ${pts[i + 1][1] - m[i + 1] * h} ${pts[i + 1][0]} ${pts[i + 1][1]}`
  }
  return d
}

/**
 * A bar rounded only on its far end. The baseline end stays square so signed
 * bars meet the axis cleanly instead of floating above it.
 */
export function roundedBarPath(
  x: number,
  y: number,
  w: number,
  h: number,
  radius: number,
  round: "top" | "bottom"
): string {
  const r = Math.max(0, Math.min(radius, w / 2, h))
  if (r === 0) return `M ${x} ${y} h ${w} v ${h} h ${-w} Z`
  if (round === "top") {
    return `M ${x} ${y + h} L ${x} ${y + r} Q ${x} ${y} ${x + r} ${y} L ${x + w - r} ${y} Q ${x + w} ${y} ${x + w} ${y + r} L ${x + w} ${y + h} Z`
  }
  return `M ${x} ${y} L ${x} ${y + h - r} Q ${x} ${y + h} ${x + r} ${y + h} L ${x + w - r} ${y + h} Q ${x + w} ${y + h} ${x + w} ${y + h - r} L ${x + w} ${y} Z`
}

/** [min, max] of a finite series; `[0, 0]` when it holds nothing usable. */
export function extent(values: number[]): [number, number] {
  let lo = Number.POSITIVE_INFINITY
  let hi = Number.NEGATIVE_INFINITY
  for (const v of values) {
    if (!Number.isFinite(v)) continue
    if (v < lo) lo = v
    if (v > hi) hi = v
  }
  if (lo > hi) return [0, 0]
  return [lo, hi]
}

/**
 * Open a domain by `frac` on both ends so marks never touch the frame. A
 * degenerate domain (every sample equal) opens around the value instead of
 * collapsing to a division by zero.
 */
export function padDomain(
  lo: number,
  hi: number,
  frac = 0.08
): [number, number] {
  if (hi > lo) {
    const pad = (hi - lo) * frac
    return [lo - pad, hi + pad]
  }
  const pad = Math.abs(lo) > 0 ? Math.abs(lo) * 0.25 : 1
  return [lo - pad, lo + pad]
}

/** Linear interpolation quantile over an already-sorted ascending series. */
function quantile(sorted: number[], q: number): number {
  const n = sorted.length
  if (n === 0) return 0
  if (n === 1) return sorted[0]
  const pos = (n - 1) * q
  const lo = Math.floor(pos)
  const hi = Math.min(n - 1, lo + 1)
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo)
}

/**
 * Freedman–Diaconis bin count, clamped to 8..20.
 *
 * FD sizes bins from the IQR, so it is not dragged around by the one huge
 * outlier that a max-min rule would let dictate the whole histogram. A zero
 * IQR (heavily tied data) falls back to the square-root rule.
 */
export function binCount(values: number[]): number {
  const finite = values.filter((v) => Number.isFinite(v))
  const n = finite.length
  if (n < 2) return 8
  const sorted = [...finite].sort((a, b) => a - b)
  const span = sorted[n - 1] - sorted[0]
  if (span <= 0) return 8
  const iqr = quantile(sorted, 0.75) - quantile(sorted, 0.25)
  const width = iqr > 0 ? (2 * iqr) / Math.cbrt(n) : 0
  const raw = width > 0 ? Math.ceil(span / width) : Math.ceil(Math.sqrt(n))
  return Math.max(8, Math.min(20, raw))
}

/**
 * Trailing rolling mean. The first `window - 1` points average what exists so
 * far rather than being dropped — the line starts where the data starts, which
 * is what a reader expects from a chart that also shows the raw series.
 */
export function rollingMean(values: number[], window: number): number[] {
  const w = Math.max(1, Math.floor(window))
  const out: number[] = new Array(values.length)
  let sum = 0
  for (let i = 0; i < values.length; i++) {
    sum += values[i]
    if (i >= w) sum -= values[i - w]
    out[i] = sum / Math.min(i + 1, w)
  }
  return out
}
