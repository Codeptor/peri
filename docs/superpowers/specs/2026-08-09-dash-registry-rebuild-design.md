# dash v2 — full registry rebuild (design)

**Date**: 2026-08-09 · **Status**: approved by user (scope: full rebuild · aesthetic: let it evolve · charts: best-of-breed mix + TradingView-style live candles)
**Scope**: `dash/` only. traderd API contract (spec §4) and `tg_news_pipe.py` unchanged. **PAPER-ONLY invariant holds**: the one new external data path (Hyperliquid public candles) is read-only market data, no keys, no signing.

## Goal

Recompose all four pages + shell of the botta terminal from live shadcn registry components, replacing every hand-rolled lookalike with real registry code, adding live TradingView-style candle charts with entry/SL/TP overlays, and letting the visual language evolve beyond the strict flat-terminal rules (gradients/glow/animation now permitted; near-black oklch base and emerald/red long-short semantics stay).

## Non-goals

- No traderd/pipe changes; no new daemon endpoints. Candles come from HL's public API via a dash route handler.
- No auth, no deployment — stays a local-only dashboard.
- No echarts: evilcharts is consumed in its **recharts** family only.

## Verified registry inventory (probed 2026-08-09, all serving JSON)

| registry | url template | verified items we use |
|---|---|---|
| `@bklit` | `https://bklit.com/r/{name}.json` (ui.bklit.com 301s here — configure final host) | area-chart, line-chart, bar-chart, scatter-chart, heatmap-chart, gauge-chart, projection-line, reference-area, profit-loss-line, shimmering-text, stat-card-area-01, stat-card-line-01, chart-stat-flow + shared primitives (chart-utils/context/animation/series, axes, tooltip, legend, markers, grid, background). visx-based. |
| `@evilcharts` | `https://evilcharts.com/r/{name}.json` | recharts-* family: ex-gradient-colors-area-chart, ex-glowing-desktop-* line/bar, donut pie variants, loading states. 271 items total. |
| `@kibo-ui` | `https://www.kibo-ui.com/r/{name}.json` | ticker, contribution-graph, table, list, pill, status, relative-time, code-block, banner, spinner, marquee. Back online (500'd on 2026-08-09 morning pass). |
| `@openstatus` | `https://openstatus.dev/r/{name}.json` | status-component, status-component-group, status-banner, status-timestamp, status-bar, status-feed, status-blank. |
| `@coss` | `https://coss.com/ui/r/{name}.json` | OriginUI's successor (originui.com is dead → replace in components.json). Item names TBD at install time via its registry index; used opportunistically for inputs/pills. |
| `@tweakcn` | (existing) | theme token reference only. |
| `@shadcn-dashboard` | (existing) | layout inspiration only, not installed. |

Dead/rejected: `@heatmap` (SPA, no JSON), `@termcn` (Ink/CLI components, not web), bklit `candlestick-chart` (SVG; we standardize on one candle engine).

## New npm deps

- `lightweight-charts` — TradingView's OSS canvas engine for all candle surfaces. Native priceLines for entry/SL/TP. Version + current API (v5 changed series creation) verified against docs at install time.
- `@visx/*` — pulled automatically by bklit registry installs.
- `@number-flow/react` — animated numerics for live-ticking stats.
- Whatever registry items declare (committed via lockfile). Kibo items may pull `radix-ui`; project is base-nova (`@base-ui/react`) — **mixed primitives accepted** (user-approved aesthetic evolution outweighs single-primitive purity; components are vendored source, so drift is controlled).

## Theme evolution (`app/globals.css`)

Keep: near-black oklch base (bg 0.12, card 0.165, hairline borders), `--long` emerald / `--short` red / `--warning` amber, mono tabular-nums for all numerics, `prefers-reduced-motion` kill-switch, thin scrollbars, PAPER pill.
Now permitted: gradient fills, glow (box/drop-shadow on chart strokes), spring animations, elevated chart-card surfaces (subtle 1-step lighter panels), NumberFlow ticking. Chart palette extends beyond emerald/red where multi-series needs it (oklch ramp defined once as `--chart-1..6`). README design-token section rewritten to match.

## Candle data architecture

```
HL public API (POST https://api.hyperliquid.xyz/info, type=candleSnapshot)
        ↑ server-side fetch (no CORS exposure, no keys)
app/api/hl/candles/route.ts   ← GET /api/hl/candles?coin=SOL&interval=1m&lookback_h=12
        ↓ normalized [{t,o,h,l,c,v}]
lib/candles.ts (useCandles hook): history fetch + live last-bar update from existing ws mids feed (lib/ws.ts)
        ↓
components/candle-panel.tsx (lightweight-charts): candles + entry/SL/TP priceLines + side/ROE header
```

- Route handler is server-side only; validates `coin` against the snapshot market list; `cache: no-store`.
- **Risk**: `xyz:*` HIP-3 coins may need dex-prefixed addressing in candleSnapshot or may not serve candles. Fallback (built, not hoped): markets whose candle fetch fails render a WS-built live line (bklit live-line-chart pattern) instead of candles. Probe both `xyz:NATGAS` naming forms during Task 3 and record the result in dash/README.
- Fixtures mode: `fixtures/candles.json` (synthetic OHLC per fixture market); `useCandles` reads it when `NEXT_PUBLIC_FIXTURES=1`; no live updates in fixtures mode. `pnpm build` and `NEXT_PUBLIC_FIXTURES=1 pnpm build` both stay green.
- Next 16.3 warning honored: read `node_modules/next/dist/docs/` route-handler guide before writing the route.

## Shell

- **Top bar** → kibo `ticker`: scrolling tape of all snapshot markets (last px + 24h% vs prev_day_px, long/short colored), click navigates to `/markets?m=<coin>`. Keeps `BOTTA_TERMINAL` ident, PAPER pill, `LiveStatus` (ws pulse + kill pill).
- **Sidebar**: existing nav + live mini equity sparkline (bklit stat-card-line pattern, ws-fed) + stack health dots (api/ws/pipe). Mobile bottom nav stays.
- **⌘K command palette** (`@shadcn` command + dialog): jump to page or any market (opens its candle sheet). Registered globally in layout.

## Pages

### `/` — the desk
1. Stat row: NumberFlow tiles — equity (ticking live), realized today, unrealized, fees today, win rate — equity tile as bklit `stat-card-area-01` hybrid.
2. Hero: bklit `area-chart` equity curve — gradient fill, `reference-area` shading drawdown-from-peak zones, `shimmering-text` loading, timeframe switcher (1h/8h/24h/all via `points` param).
3. Risk & mix row: kibo `contribution-graph` as daily-PnL calendar (aggregated client-side from trades); evilcharts donut — exit mix (tp/sl/veto_close); bklit `gauge-chart` — daily loss budget consumed.
4. Recent trades: kibo `table` + `pill` (action) + `relative-time`, flash-on-insert animation retained.

### `/positions` — the book
1. Hero candle panel (`candle-panel.tsx`): selected position's market; entry (emerald) / SL (red) / TP (amber) priceLines; header with side pill, size, leverage, NumberFlow uPnL/ROE.
2. Positions table (kibo `table`): side pill, entry/mark, SL/TP distance meters (% to stop as inline progress), tiny glowing uPnL sparkline (evilcharts, ws-fed), ROE. Row click selects hero market.
3. Aggregate strip: margin used, exposure gauge (bklit `gauge-chart`), open risk (Σ distance-to-SL × size).
4. Empty state: "book is flat" (openstatus `status-blank` pattern) + last 5 closes with realized PnL.

### `/markets` — the screener
1. Screener grid (kibo `table`, client-sorted): px, 24h%, funding (+funding_z), OI, day volume, r5m/r1h, vol1h, range_pos bar. Sort headers, text filter.
2. Tabbed viz: bklit `heatmap-chart` (markets × features z-intensity) | bklit `scatter-chart` (funding_z × r1h crowding map, dot size = volume).
3. Nominees rail: analyst picks — score bar, side-hint pill, features chips, `relative-time`.
4. Any market click (grid, tape, palette) → shadcn `sheet` with candle panel + market stats.

### `/intel` — the wire
1. News feed: kibo `list` — source pill, title/body, market tags (click → filter), `relative-time`, url out-link.
2. Decisions log: conviction meter (bklit mini bar), thesis prose, action/side pills, vetoed + refusal badges, executed check, kibo `code-block` raw-JSON expand.
3. Stack health: openstatus `status-component-group` — traderd API, WS, pipe freshness (last news ts), LLM refusal rate (parsed from decisions, refused:true only — keep dash refusal-fix semantics, commit 99cb9ef).
4. Refusal/skips mini bar chart (evilcharts recharts bar).

## Deletions

`components/ui/stat-card.tsx`, `components/ui/empty-state.tsx` (hand-rolled lookalikes) — removed once registry replacements land. `components/ui/chart.tsx` (shadcn/recharts wrapper) removed at cleanup **iff** nothing imports it after the rebuild.

## Build gates & sequencing

`pnpm build` (typecheck) green after every task; fixtures build green at Tasks 3 and 8. Registry code is vendored: install → retheme to tokens → commit per task. Task skeleton (detail in plan doc): 1 foundation (registries/deps/theme) → 2 shell → 3 candle infra → 4–7 pages (desk, book, screener, wire) → 8 cleanup + docs sync (dash/README, STATUS.md).

## Risks

- **Registry availability is weather** (kibo 500'd 12h ago, worked today): each task snapshots what it installs into git immediately; a downed registry mid-build blocks only its own task.
- **xyz: candles** — fallback line-chart path specified above; not a blocker.
- **base-nova × radix mixing** — accepted trade; if a kibo item fights base-nova styling, retheme locally (we own the source).
- **Bundle growth** (visx + lightweight-charts + recharts): acceptable for a local single-user dash; no perf budget enforced.
