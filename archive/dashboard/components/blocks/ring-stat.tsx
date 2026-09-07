"use client"

import * as React from "react"

import { cn } from "@/lib/utils"
import { prefersReducedMotion } from "@/components/blocks/chart-geometry"

export type RingStatProps = {
  pct: number
  color?: string
  size?: number
  label?: string
  sublabel?: string
  className?: string
}

const SWEEP_MS = 620

/** Layout effect on the client, plain effect on the server (never runs there). */
const useIsoLayoutEffect =
  typeof window === "undefined" ? React.useEffect : React.useLayoutEffect

/**
 * Thin radial progress ring with the mono percentage in the middle.
 *
 * The arc sweeps from empty on mount and from wherever it currently sits on a
 * value change, so a budget that jumps is *seen* to jump. Under
 * `prefers-reduced-motion` it snaps — no sweep, no counting numerals.
 */
export function RingStat({
  pct,
  color = "var(--accent-orange)",
  size = 56,
  label,
  sublabel,
  className,
}: RingStatProps) {
  const target = Math.min(100, Math.max(0, Number.isFinite(pct) ? pct : 0))

  // State seeds at the target so server HTML and the first client render agree;
  // the ref seeds at 0 so the mount sweep still starts from an empty ring.
  const [shown, setShown] = React.useState(target)
  const shownRef = React.useRef(0)
  const frameRef = React.useRef<number | null>(null)

  useIsoLayoutEffect(() => {
    const apply = (v: number) => {
      shownRef.current = v
      setShown(v)
    }
    if (prefersReducedMotion()) {
      apply(target)
      return
    }
    const from = shownRef.current
    if (from === target) return
    apply(from)
    const started = performance.now()
    const step = (now: number) => {
      const t = Math.min(1, (now - started) / SWEEP_MS)
      const eased = 1 - Math.pow(1 - t, 3)
      apply(from + (target - from) * eased)
      if (t < 1) frameRef.current = requestAnimationFrame(step)
    }
    frameRef.current = requestAnimationFrame(step)
    return () => {
      if (frameRef.current != null) cancelAnimationFrame(frameRef.current)
    }
  }, [target])

  const stroke = Math.max(3, Math.round(size / 14))
  const r = (size - stroke) / 2
  const circumference = 2 * Math.PI * r
  const dash = (circumference * shown) / 100

  return (
    <div className={cn("flex flex-col items-center gap-2", className)}>
      <div className="relative shrink-0" style={{ height: size, width: size }}>
        <svg
          aria-label={`${Math.round(target)}%`}
          className="block"
          height={size}
          role="img"
          width={size}
        >
          <circle
            cx={size / 2}
            cy={size / 2}
            fill="none"
            r={r}
            stroke="var(--cell)"
            strokeWidth={stroke}
          />
          <circle
            cx={size / 2}
            cy={size / 2}
            fill="none"
            r={r}
            stroke={color}
            strokeDasharray={`${dash} ${circumference - dash}`}
            strokeLinecap="round"
            strokeWidth={stroke}
            transform={`rotate(-90 ${size / 2} ${size / 2})`}
          />
        </svg>
        <span
          className="absolute inset-0 grid place-items-center font-mono font-semibold"
          style={{ fontSize: Math.round(size * 0.23) }}
        >
          {`${Math.round(shown)}%`}
        </span>
      </div>
      {label || sublabel ? (
        <div className="text-center">
          {label ? (
            <div className="text-xs text-muted-foreground">{label}</div>
          ) : null}
          {sublabel ? (
            <div className="mt-0.5 font-mono text-[11px] text-foreground">
              {sublabel}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}
