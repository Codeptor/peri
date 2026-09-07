import {
  Analytics01Icon,
  BarChartHorizontalIcon,
  ChartScatterIcon,
  GridViewIcon,
  StopWatchIcon,
  Target02Icon,
} from "@hugeicons/core-free-icons"

import { PageHeader } from "@/components/blocks/page-header"
import { SectionCard } from "@/components/blocks/section-card"
import { Skeleton } from "@/components/ui/skeleton"

import { META_LABEL } from "./shared"

/** Prerendered shell while the `?m=`-aware client half boots. */
export function MarketsFallback() {
  return (
    <>
      <PageHeader
        icon={Analytics01Icon}
        meta={<span className={META_LABEL}>Loading universe</span>}
        title="Markets"
      />
      <div className="flex flex-wrap items-center gap-3 pb-5">
        <Skeleton className="h-[30px] w-48 rounded-full" />
      </div>
      <div className="flex flex-col gap-5">
        <SectionCard icon={Target02Icon} title="Nominees">
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {[0, 1, 2].map((i) => (
              <Skeleton className="h-[160px] rounded-md" key={i} />
            ))}
          </div>
        </SectionCard>
        <div className="grid gap-5 xl:grid-cols-2">
          <SectionCard icon={BarChartHorizontalIcon} title="Realized by market">
            <Skeleton className="h-[210px] w-full rounded-md" />
          </SectionCard>
          <SectionCard icon={StopWatchIcon} title="Hold time vs net">
            <Skeleton className="h-[300px] w-full rounded-md" />
          </SectionCard>
        </div>
        <SectionCard icon={GridViewIcon} title="Screener">
          <div className="flex flex-col gap-3">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <Skeleton className="h-6 w-full rounded-sm" key={i} />
            ))}
          </div>
        </SectionCard>
        <SectionCard icon={ChartScatterIcon} title="Crowding map">
          <Skeleton className="h-[300px] w-full rounded-md" />
        </SectionCard>
      </div>
    </>
  )
}
