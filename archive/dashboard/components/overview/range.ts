import type { SegmentedOption } from "@/components/blocks/segmented"

export type RangeKey = "1h" | "8h" | "24h" | "all"

/** Window length per range key; `null` means the whole series. */
export const RANGE_MS: Record<RangeKey, number | null> = {
  "1h": 3_600_000,
  "8h": 28_800_000,
  "24h": 86_400_000,
  all: null,
}

/** Legs for the shared `Segmented` control — sans, sentence case. */
export const RANGE_OPTIONS: SegmentedOption<RangeKey>[] = [
  { value: "1h", label: "1h" },
  { value: "8h", label: "8h" },
  { value: "24h", label: "24h" },
  { value: "all", label: "All" },
]
