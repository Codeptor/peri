"use client"

import * as React from "react"
import { ComputerIcon, Moon02Icon, Sun01Icon } from "@hugeicons/core-free-icons"
import { HugeiconsIcon, type IconSvgElement } from "@hugeicons/react"
import { useTheme } from "next-themes"

import { cn } from "@/lib/utils"

type Mode = { value: string; label: string; icon: IconSvgElement }

/** Three-state so "system" stays reachable after a manual pick. */
const MODES: Mode[] = [
  { value: "light", label: "Light", icon: Sun01Icon },
  { value: "dark", label: "Dark", icon: Moon02Icon },
  { value: "system", label: "System", icon: ComputerIcon },
]

/** Sun / moon / system pill group. Active leg = card surface, matching `Segmented`. */
const NEVER_CHANGES = () => () => {}

export function ThemeToggle({ className }: { className?: string }) {
  const { theme, setTheme } = useTheme()

  // next-themes has no theme on the server; the group renders inert until
  // hydration so the markup matches and no leg flashes as active.
  const mounted = React.useSyncExternalStore(
    NEVER_CHANGES,
    () => true,
    () => false
  )

  return (
    <div
      aria-label="Theme"
      className={cn(
        "inline-flex items-center gap-0.5 rounded-full border border-border bg-surface-2 p-0.5 dark:bg-background",
        className
      )}
      role="group"
    >
      {MODES.map((mode) => {
        const active = mounted && theme === mode.value
        return (
          <button
            aria-pressed={active}
            className={cn(
              "grid size-6 place-items-center rounded-full border transition-colors",
              active
                ? "border-border bg-card text-foreground dark:border-transparent dark:bg-surface-2"
                : "border-transparent text-muted-foreground hover:text-foreground"
            )}
            key={mode.value}
            onClick={() => setTheme(mode.value)}
            title={mode.label}
            type="button"
          >
            <HugeiconsIcon icon={mode.icon} size={13} strokeWidth={1.9} />
            <span className="sr-only">{mode.label}</span>
          </button>
        )
      })}
    </div>
  )
}
