import type * as React from "react"
import type { IconSvgElement } from "@hugeicons/react"

import { IconChip } from "@/components/blocks/icon-chip"

export type PageHeaderProps = {
  title: string
  icon: IconSvgElement
  meta?: React.ReactNode
  actions?: React.ReactNode
}

/** Page title row: icon chip + 19px semibold title, meta beneath, actions right. */
export function PageHeader({ title, icon, meta, actions }: PageHeaderProps) {
  return (
    <header className="flex flex-wrap items-start justify-between gap-4 pt-6 pb-5">
      <div className="flex min-w-0 items-start gap-3">
        <IconChip className="mt-0.5" icon={icon} />
        <div className="min-w-0">
          <h1 className="truncate text-[19px] leading-7 font-semibold tracking-tight">
            {title}
          </h1>
          {meta ? (
            <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs text-muted-foreground">
              {meta}
            </div>
          ) : null}
        </div>
      </div>
      {actions ? (
        <div className="flex shrink-0 flex-wrap items-center gap-2">
          {actions}
        </div>
      ) : null}
    </header>
  )
}
