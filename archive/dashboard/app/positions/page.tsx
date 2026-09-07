import type { Metadata } from "next"

import { PositionsView } from "@/components/positions/positions-view"

export const metadata: Metadata = {
  title: "Positions — kestrel",
  description: "Open paper book, stop/target meters and fill history.",
}

export default function PositionsPage() {
  return <PositionsView />
}
