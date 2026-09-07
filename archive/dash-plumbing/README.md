# botta terminal — dash

Local Next.js dashboard for the autonomous paper trader (`traderd` on 127.0.0.1:7411). Dark-first, dense, flat Bloomberg-style terminal — hairline borders, tabular mono numerics, 4/8/12/16 spacing, no glass/gradients. Local only, no auth.

## Stack
- Next.js 16 App Router + TypeScript + Tailwind v4
- shadcn/ui (base-nova) + multiple registries (see below)
- Recharts + sonner + lucide + next-themes
- `lib/api.ts` mirrors spec §4 field-for-field; `lib/ws.ts` live feed; `lib/format.ts` null-safe (`—`)

## Registries (`dash/components.json`)

| registry | url | purpose | status 2026-08-09 (evening re-probe) |
|---|---|---|---|
| `@shadcn` | `https://ui.shadcn.com/r/{name}.json` (implicit) | base primitives: button, card, table, badge, tabs, skeleton, chart, sonner, command, sheet | ✅ used via CLI |
| `@bklit` | `https://bklit.com/r/{name}.json` | visx chart system: area/line/bar/scatter/heatmap/gauge, projection-line, reference-area, shimmering-text, stat-card hybrids | ✅ 56 items, JSON verified (`ui.bklit.com` 301s to final host — configured direct) |
| `@evilcharts` | `https://evilcharts.com/r/{name}.json` | recharts-family variants only (gradient areas, glowing lines, donuts) — **no echarts items** | ✅ 271 items, JSON verified |
| `@kibo-ui` | `https://www.kibo-ui.com/r/{name}.json` | ticker, contribution-graph, table, list, pill, code-block, relative-time | ✅ recovered (500'd on morning pass) — 41 items verified |
| `@openstatus` | `https://openstatus.dev/r/{name}.json` | status-component-group, status-blank, status-timestamp — stack-health blocks | ✅ 26 items, JSON verified |
| `@coss` | `https://coss.com/ui/r/{name}.json` | OriginUI's successor (originui.com dead → removed); opportunistic inputs/pills | ✅ directory-listed; item names resolved at install time |
| `@shadcn-dashboard` | `https://shadcndashboard.dev/r/{name}.json` | dashboard blocks | configured, layout inspiration only |
| `@tweakcn` | `https://tweakcn.com/r/themes/{name}.json` | theme token reference | configured, tokens applied via `globals.css` (oklch dark) |

Rejected on probe: `@heatmap` (SPA shell, no JSON), `@termcn` (Ink/CLI components, not web). Spec: `docs/superpowers/specs/2026-08-09-dash-registry-rebuild-design.md`.

## shadcn tooling — MCP vs CLI (actual 2026-08-09)

| tool | expected | actual |
|---|---|---|
| `get_project_registries` | exposed | ✅ available — returned 4 configured |
| `view_items_in_registries` | exposed | ✅ available — tested `@shadcn/badge`, `@shadcn/card` (JSON OK) |
| `search_items_in_registries` | exposed | ❌ not exposed in this runtime (only 3/7 tools listed) |
| `get_add_command_for_items` | exposed | ❌ not exposed |
| `pnpm dlx shadcn@latest view/add` | fallback | ✅ used — `view @kibo-ui/status-card` → 500, `view @originui/stat-card` → HTML redirect, `view @shadcn/badge` → JSON OK |

Evidence recorded here per spec §4 task 1: searches for `"stat card"`, `"data table"`, `"chart"`, `"empty state"` were performed via `curl https://ui.shadcn.com/r/registries.json` + `curl https://www.kibo-ui.com/r/...` + `pnpm dlx shadcn@latest view` CLI. All 500/redirect failures logged; fallback was to craft `components/ui/stat-card.tsx` and `components/ui/empty-state.tsx` by hand following registry block patterns (rounded-full pills, hairline borders, muted tracks), keeping `pnpm build` green every commit.

## Data layer (unchanged — contracts frozen)
- `lib/api.ts` types = spec §4 field-for-field; `NEXT_PUBLIC_FIXTURES=1` reads `fixtures/*.json` via `/fixtures/*.json` (client) or `fs` (server)
- `lib/ws.ts` `useLiveFeed` → `ws://127.0.0.1:7411/ws`, 1s→10s backoff; raw `position` events enriched client-side from mids map
- `lib/format.ts` `usd/signedPct/compact/fmtPrice/relativeTime` accept `number|null|undefined`

## Run
```bash
cd dash
pnpm install
pnpm dev        # http://localhost:3000
pnpm build      # must be green after every task (typecheck)
NEXT_PUBLIC_FIXTURES=1 pnpm build  # fixtures mode still builds
```

## Layout
- Sidebar `components/app-sidebar.tsx` (client, `usePathname` active states, glyph pill, paper-mode footer, mobile bottom nav)
- Top bar `h-9` hairline, `BOTTA_TERMINAL` mono + `PAPER` pill + `LiveStatus` (ws dot `ws-pulse` + kill pill)
- All pages: consistent `h1` 11px mono tracking 0.20em + status chips, max-w 1600, `p-3.5 md:p-5`

## Design tokens (`app/globals.css`)
- Dark-first oklch: bg `0.12 0 0` near-black, card `0.165 0 0` panel, border `1 0 0 / 7.5%` hairline, muted `0.20`, semantic `--long` emerald `0.68 0.16 150`, `--short` red `0.61 0.21 25`, `--warning` amber `0.78 0.15 75`, chart-1 emerald / chart-2 red
- Mono tabular-nums globally via `font-mono` + `font-feature-settings:"tnum"`, radius `0.375rem`, spacing 4/8/12/16, hairline `1px` (flat, no shadows/blur), focus-visible `ring 2px`, thin scrollbar, `flash-long/short/amber` 520–700ms + `ws-pulse` 1.35s, `prefers-reduced-motion` disables all

## v2 registry rebuild (2026-08-09)

Spec `docs/superpowers/specs/2026-08-09-dash-registry-rebuild-design.md`, plan `docs/superpowers/plans/2026-08-09-dash-registry-rebuild.md`. Aesthetic evolved by design: gradients, glow (`--glow-long/short`), and spring animation are now permitted; near-black oklch base, emerald/red long/short semantics, mono tabular numerics, and `prefers-reduced-motion` kill-switch all remain.

### Candle data path (live TradingView-style charts)

```
HL public API (POST api.hyperliquid.xyz/info, candleSnapshot)   ← read-only, no keys (PAPER invariant)
   ↓ app/api/hl/candles/route.ts   GET /api/hl/candles?coin=SOL&interval=1m&lookback_h=12
   ↓ lib/candles.ts fetchCandles() (fixtures-aware)
   ↓ components/candle-panel.tsx   lightweight-charts 5.x canvas; entry/SL/TP as createPriceLine;
                                   last bar ticks live off ws mids (h/l/c mutation + series.update)
```

**xyz: verdict (probed 2026-08-09):** traderd market names map 1:1 to candleSnapshot coins — `xyz:NATGAS` returns bars, bare `NATGAS` 500s. No fallback path needed; the panel shows an in-place error + RETRY on transient failures.

### Shell
- `components/market-ticker.tsx` — kibo `Ticker` composables in a CSS marquee tape (`.tape-scroll`, pause on hover, reduced-motion off), 15s snapshot poll, click → `/markets?m=<coin>`
- `components/command-palette.tsx` — ⌘K jump to pages/markets (shadcn command + dialog)
- `components/app-sidebar.tsx` — bklit `LineChart` equity spark + api/ws/pipe vitals dots (30s poll)

### Pages
| page | composition |
|---|---|
| `/` desk | NumberFlow stat tiles, bklit `AreaChart` hero + underwater `ReferenceArea` + `shimmering-text` loading + 1h/8h/24h/all, kibo `contribution-graph` daily-PnL calendar (emerald/red by sign, level by \|net\| quantile), evilcharts donut exit mix, bklit `Gauge` kill budget (`lib/risk.ts` mirrors traderd.toml), fills table with kibo `Pill` |
| `/positions` book | `CandlePanel` hero for selected position (entry/SL/TP price lines, NumberFlow uPnL/ROE), risk strip (margin gauge, notional, Σ risk-to-SL), glow SVG spark per card (ws ring buffer), openstatus `status-blank` flat-book state with last 5 closes |
| `/markets` screener | sortable universe grid with **feature cells tinted as the heatmap** (alpha ∝ \|value\|/col-max), custom SVG crowding map (funding_z × r1h, area = $vol, click → sheet), `MarketSheet` (sheet + candles + stat grid) deep-linked via `?m=` (Suspense param bridge), nominee cards clickable |
| `/intel` wire | openstatus `StatusComponentGroup` stack health (api/ws/pipe/LLM), evilcharts stacked decisions/day (executed/skipped/refused), decision cards with conviction meter + kibo `code-block` raw-JSON expand, news cards with clickable market-tag filters. Refusal predicate = `refused:true` only (99cb9ef) |

### Registry lessons (recorded for the next pass)
- bklit **blocks** (`stat-card-*`) drag in `@central-icons-react/all` (license-gated preinstall, fails) — use bklit **core** items only; compose stat cards from `chart-stat-flow` + cores.
- bklit registry ships `"../components/…"` imports that assume a different root — rewrite to `@/components/…` after every install (sed in repo history).
- bklit `heatmap-chart`/`scatter-chart` are **Date-axis-only** (calendar / time-series; verified `HeatmapBin.date: Date`, scatter bisector<Date>) — unusable for category matrices or numeric-x scatters; removed.
- kibo `table` targets @tanstack/react-table v8 (v9 alpha breaks it), kibo `list` is a dnd-kit board, kibo `relative-time` is a timezone clock — all three removed as wrong-tool; kibo `code-block` needed `SiCss3→SiCss` (react-icons rename) and base-nova select/button patches; openstatus under-declares `date-fns`/`@date-fns/utc`.
- base-nova (Base UI) project: vendored registry code with radix-isms (`asChild`, `delayDuration`, radix select handler signatures) needs small local patches — we own the source.

### Invariants
- Sonner `toast.error` on every `api.*` catch; skeletons sized to content; stable keys; per-cell flash not full-table rerenders; ws ≤1/s (traderd side)
- All colors via tokens; `pnpm build` + `NEXT_PUBLIC_FIXTURES=1 pnpm build` green every commit
- Lint: app pages + authored lib/components are clean under the strict react-hooks (compiler-era) rules. Known debt (~60 findings, non-gating): vendored registry internals (bklit chart orchestration refs-in-render, kibo/evilcharts `any`s) + pre-existing `lib/ws.ts`/`live-status.tsx` ref patterns. `pnpm build` remains the gate.

## Ops note (2026-08-09, commit e82baeb)
Browser data path is same-origin only: `next.config.ts` rewrites `/traderd/:path*` → `http://127.0.0.1:7411/:path*`. `lib/api.ts` uses `/traderd` relative in the browser and the absolute URL server-side. Do NOT call `http://127.0.0.1:7411` from client code directly — browser CORS blocks it and traderd intentionally sends no CORS headers. WS (`ws://127.0.0.1:7411/ws`) is CORS-exempt and stays direct. Live WS broadcast `position` events are RAW (no mark_px/unrealized_pnl/roe) — pages enrich client-side from the mids map; `lib/format.ts` formatters all accept `number|null|undefined` (`—`).
