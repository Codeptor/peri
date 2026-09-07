import { Suspense } from "react"

import { MarketsClient } from "@/components/markets/markets-client"
import { MarketsFallback } from "@/components/markets/markets-fallback"

export default function MarketsPage() {
  // `?m=` deep link is read with useSearchParams — the Suspense bridge keeps
  // the rest of the route prerenderable.
  return (
    <Suspense fallback={<MarketsFallback />}>
      <MarketsClient />
    </Suspense>
  )
}
