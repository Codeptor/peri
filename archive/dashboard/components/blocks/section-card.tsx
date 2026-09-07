import type * as React from "react"
import type { IconSvgElement } from "@hugeicons/react"

import { cn } from "@/lib/utils"
import { IconChip } from "@/components/blocks/icon-chip"

export type SectionCardProps = {
  title: string
  icon?: IconSvgElement
  action?: React.ReactNode
  children: React.ReactNode
  className?: string
}

/** The standard floating card: icon chip + title + right-aligned action, then content. */
export function SectionCard({
  title,
  icon,
  action,
  children,
  className,
}: SectionCardProps) {
  return (
    <section
      className={cn(
        "rounded-lg border border-border bg-card shadow-[var(--shadow-card)]",
        className
      )}
    >
      <div className="flex items-center gap-3 px-5 pt-5 pb-4">
        {icon ? <IconChip icon={icon} /> : null}
        <h2 className="truncate text-[15px] font-semibold tracking-tight">
          {title}
        </h2>
        {action ? (
          <div className="ml-auto flex shrink-0 items-center gap-2">
            {action}
          </div>
        ) : null}
      </div>
      <div className="px-5 pb-5">{children}</div>
    </section>
  )
}
