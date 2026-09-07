# `components/blocks` — the kestrel dashboard primitives

Everything here is hand-rolled SVG or plain markup on design-system tokens. No
chart library, no new dependencies. Import from the barrel (`@/components/blocks`)
or the module path — both work; the module path keeps the client boundary tight.

Every chart is a client component, measures its own container (`useChartTooltip`),
and renders nothing but a correctly sized box until that measurement lands — so
SSR and the first client paint agree.

## Chart contracts

Frozen. Page components import these names blindly.

| component     | props                                                                                                                                                                                                             | notes                                                                                                                                                                                                                                                                                                                                     |
| ------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `BarChart`    | `{ data: { x: string \| number; value: number; color? }[]; cumulative?: boolean; baseline?: number = 0; height? = 180; format? = usd; xLabels?: "sparse" \| "all" \| "none" = "sparse"; ariaLabel?; className? }` | Signed bars around `baseline` — above it `--long`, below `--short`, overridable per datum. `cumulative` overlays the running total as a smooth `--accent-orange` line **on its own y-domain** (a month of small daily nets against one large total would otherwise flatten the bars). Right-edge mono labels mark the data's own max/min. |
| `Histogram`   | `{ values: number[]; bins? = Freedman–Diaconis capped 8..20; height? = 180; format? = usd; highlightZero? = true; ariaLabel?; className? }`                                                                       | When the sample straddles zero the bin grid is anchored _at_ zero, so no bin mixes winners with losers. `highlightZero` colours bins by sign and dashes the zero line. Right-edge label is the peak bin count.                                                                                                                            |
| `ScatterPlot` | `{ points: { x: number; y: number; r? = 5; color?; label? }[]; xLabel?; yLabel?; xFormat?; yFormat?; guides?: { x?: number[]; y?: number[] }; height? = 260; ariaLabel?; className? }`                            | Numeric axes (nothing date-bound). `guides` draw dashed reference lines in data space and widen the domain so a threshold outside the data still shows. Axis _names_ are sans, axis _ticks_ mono. Big dots paint first so small ones stay hoverable.                                                                                      |
| `RollingLine` | `{ data: { x: number; value: number }[]; window? = 10; height? = 200; format? = usd; refLine?: number; xFormat?; ariaLabel?; className? }`                                                                        | Raw series faint (`--chart-6`), trailing mean prominent (`--accent-orange`). The first `window - 1` points average what exists so far, so the mean starts where the data starts. Pass `xFormat` to get x-axis ticks; without it the axis stays bare and the tooltip carries x.                                                            |
| `DotMatrix`   | `{ columns: { value: number; color?; label? }[]; max?; rows? = 12; cellSize? = 8; gap? = 3; xLabels?: string[]; ariaLabel?; className? }`                                                                         | The brand idiom — keep it for presence and counts, where quantised cells are a feature. Magnitudes that need reading off an axis belong in `BarChart`.                                                                                                                                                                                    |
| `SegmentBar`  | `{ segments: { label; count; color }[]; className? }`                                                                                                                                                             | One stacked rounded bar + legend rows. Hovering a segment dims the rest and tooltips count + share.                                                                                                                                                                                                                                       |
| `TrendArea`   | `{ data: { ts; value }[]; color?; height? = 160; live?; axis?; annotations? = axis; format? = usd; strokeWidth? = 2.25; markers?; bands?; refLines?; className? }`                                                | `annotations` pins the window's min / max / last to their points and drops the right-edge hi/lo pair — the same two numbers, said where they happened. Collision-avoided: the last value always wins, an extreme that _is_ the last point is dropped rather than doubled.                                                                 |
| `RingStat`    | `{ pct: 0..100; color?; size? = 56; label?; sublabel?; className? }`                                                                                                                                              | Arc sweeps from empty on mount and from its current position on a value change; snaps instantly under `prefers-reduced-motion`.                                                                                                                                                                                                           |

`format` / `xFormat` / `yFormat` take `(v: number) => string` — `lib/format.ts`
has `usd`, `signedPct`, `compact`, `fmtPrice`. Money-shaped charts default to
`usd`; `ScatterPlot` defaults to a plain numeric formatter.

### DotMatrix tooltip copy

`column.label` is split on `" · "` (space, middot, space): the first segment
becomes the muted meta line, the second the headline figure, the rest a trailer.

```
`${date} · ${usd(net)} · ${closes} closes`   →   2026-08-01
                                                 $12.30
                                                 3 closes
```

## Tooltip kit

One positioning and styling implementation for every chart, including bespoke
ones on pages (the markets crowding scatter adopts it directly).

```tsx
const { containerRef, width, height, tip, show, hide } = useChartTooltip<number>()

<div className="relative w-full" onPointerLeave={hide} ref={containerRef} style={{ height }}>
  <svg …>
    <rect … onPointerEnter={() => show(i, cx, topY)} />
  </svg>
  {tip ? (
    <TooltipCard
      boundsWidth={width}
      boundsHeight={height}
      meta="2026-08-09 14:30"      // mono, muted — timestamps, bucket ranges
      title="SOL"                  // sans
      value="+$42.30"              // mono, the headline figure
      rows={[{ label: "Share", value: "12%" }]}
      x={tip.x}
      y={tip.y}
    />
  ) : null}
</div>
```

- `useChartTooltip<T>()` owns both the ResizeObserver measurement and the hover
  state. `show(data, x, y)` anchors in container-local pixels.
- `TooltipCard` measures itself and clamps inside `boundsWidth`; it sits above
  the anchor and flips below when that would clip. `placement="above"` opts out
  of the flip for charts only a few pixels tall (`SegmentBar`).
- Colour small text with `*-ink` tokens (`--long-ink`, `--short-ink`,
  `--accent-orange-ink`) — the raw hue sits near 2.7:1 on the light card.
- Charts carry `role="img"` + an `aria-label` summary instead of per-mark
  `<title>`: a native title tooltip firing a second later on top of the real one
  is worse than either alone.

## Shared geometry — `chart-geometry.ts`

`monotonePath`, `roundedBarPath`, `extent`, `padDomain`, `binCount`,
`rollingMean`, `prefersReducedMotion`, plus the axis tokens every chart draws
ticks with (`AXIS_TICK_CLASS` / `_SIZE` / `_FILL`, `GRID_STROKE`, `GUIDE_DASH`).
Use these rather than re-deriving — one place to keep the charts one language.

## Non-chart blocks

`KpiCard`, `SectionCard`, `PageHeader`, `Segmented`, `StatusPill`, `DeltaChip`,
`IconChip`. `IconChip` also owns the `Hue` union and its tint/solid/var maps:
`accent` is the orange DATA hue, `long`/`short` are PnL-only, and blue
`--primary` is deliberately absent — it is the ACTION hue and never encodes a
value.

## Type discipline

Sans for labels, nav, buttons, card titles. Mono for numerals, timestamps, ids
and axis ticks. Both themes must read: check any new colour pair against its
ground before shipping it.
