# Plan C: `dash` — Fully Custom Trading Dashboard (Next.js + shadcn multi-registry)

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans. Self-contained. Normative contracts: spec `docs/superpowers/specs/2026-08-08-auto-trader-design.md` §4 — every payload type this app renders comes from there. traderd must be running on `127.0.0.1:7411` for live verification (it exists once Plan A lands; until then use the fixtures task below).

**Goal:** A local, fully custom, real-time dashboard for the paper trader: equity + PnL, live screener leaderboard, open positions, analyst decisions with theses, and the news stream — dark-first, built from **shadcn blocks pulled from multiple registries**.

**Tech Stack:** Next.js (App Router) + TypeScript, Tailwind, shadcn/ui with **multiple registries**, Recharts (shadcn charts), native WebSocket. Package manager: **pnpm**. Location: `~/botta/dash/`. LOCAL ONLY (no auth, binds localhost).

## Global Constraints

- **Use the shadcn MCP tools** (`mcp__shadcn__*`) to discover and install components/blocks: `get_project_registries` → `search_items_in_registries` → `view_items_in_registries` → `get_add_command_for_items`. Do NOT hand-roll a component that a registry block already provides; do NOT guess install commands — get them from the MCP.
- Registries: configure `components.json` with `@shadcn` plus at least two more public registries discovered via the MCP (candidates to search: tweakcn themes, OriginUI, Kibo UI — pick by what the searches return for "dashboard", "stat card", "data table", "chart"). Record the final registry list in `dash/README.md`.
- All server data flows through ONE typed client module (`lib/api.ts`) mirroring spec §4 exactly — no ad-hoc fetches in components. Live updates via `GET ws://127.0.0.1:7411/ws` with auto-reconnect; REST for initial loads.
- Money/percent formatting: one `lib/format.ts` (usd, signed pct, compact large numbers) used everywhere.
- Dark mode default; theme via a registry theme (tweakcn or equivalent); the design should read as a purpose-built trading terminal, not a generic admin template — take design cues from the user's taste: dense, technical, monospace numerics.
- After each task: `pnpm build` passes (typecheck included) and the page renders against live traderd (or fixtures pre-Plan-A); commit per task.

## File Structure

```
dash/
  app/layout.tsx            # shell: sidebar nav, theme, ws status indicator
  app/page.tsx              # Overview
  app/markets/page.tsx      # Screener & universe
  app/positions/page.tsx    # Positions & trades
  app/intel/page.tsx        # Decisions & news
  lib/api.ts                # typed REST client + payload types (spec §4)
  lib/ws.ts                 # useLiveFeed() hook (reconnecting WS → typed events)
  lib/format.ts
  components/               # shadcn + registry components land here
  fixtures/*.json           # recorded API payloads for offline dev
```

### Task 1: Scaffold + registries + theme

- [ ] `pnpm create next-app@latest dash` (TS, Tailwind, App Router, no src dir) inside `~/botta`; add `dash/node_modules`, `dash/.next` to root `.gitignore`.
- [ ] `pnpm dlx shadcn@latest init`. Then via **shadcn MCP**: `get_project_registries`, search "theme" / "dashboard" across registries, add ≥2 additional registries to `components.json`, install a dark theme (tweakcn-style) + base primitives (button, card, table, badge, tabs, sonner).
- [ ] Layout shell: sidebar (Overview / Markets / Positions / Intel), top bar with ws-connected dot + kill-switch badge placeholder. `pnpm build` green. Commit `feat(dash): scaffold + registries + theme shell`.

### Task 2: Typed data layer + fixtures

**Interfaces produced (all pages consume ONLY these):**
```ts
// lib/api.ts — types mirror spec §4 field-for-field
export type Features = { r5m:number; r1h:number; r24h:number; vol1h:number; funding_z:number; range_pos:number }
export type MarketRow = { market:string; mid:number; mark:number; oracle:number; funding:number; open_interest:number; day_ntl_vlm:number; prev_day_px:number; features:Features|null }
export type Position = { id:number; market:string; side:"long"|"short"; entry_px:number; size:number; leverage:number; margin:number; sl_px:number; tp_px:number; opened_ts:number; mark_px:number; unrealized_pnl:number; roe:number }
export type Decision = { ts:number; market:string; action:"open"|"skip"; side:"long"|"short"|null; conviction:number; thesis:string; horizon_hours:number|null; vetoed:boolean; executed:boolean; reason:string }
export type Trade = { id:number; position_id:number; market:string; action:string; px:number; size:number; fee:number; realized_pnl:number|null; ts:number }
export type NewsItem = { id:number; ts:number; source:string; title:string; body:string; url:string; markets:string[] }
export type EquityPoint = { ts:number; equity:number }
export const api = { snapshot, nominees, positions, trades, equity, decisions, news, health }  // fetch wrappers, BASE = http://127.0.0.1:7411
```
```ts
// lib/ws.ts
export function useLiveFeed(handlers: Partial<{ mids:(m:Record<string,number>)=>void; position:(p:Position)=>void; decision:(d:Decision)=>void; news:(n:NewsItem)=>void; equity:(e:EquityPoint)=>void }>): { connected:boolean }
// reconnect: 1s→10s capped backoff; parses {type,data} frames per spec §4
```
- [ ] Write both + `lib/format.ts`. Record fixtures: with traderd live `curl` each endpoint into `fixtures/`; else author minimal fixtures matching the types (marked TODO-refresh). A `NEXT_PUBLIC_FIXTURES=1` mode makes `api.*` read fixtures — keeps the dash buildable before Plan A finishes.
- [ ] `pnpm build` green. Commit `feat(dash): typed api client + live ws hook + fixtures`.

### Task 3: Overview page (the exemplar — sets the visual bar)

- [ ] Via shadcn MCP find + install: a stat-card/KPI block, an area/line chart block (Recharts-based). Compose: equity curve (area, 24h/7d toggle), stat tiles (equity, day PnL $ and %, open positions count, win rate, entries today vs cap), kill-switch banner (red, from `/api/health` + live), analyst health strip (last decision ts, refusal rate from recent decisions, model in use).
- [ ] Live: equity + position events re-render via `useLiveFeed`; ws dot in top bar reflects `connected`.
- [ ] Verify against live traderd (or fixtures) + `pnpm build`. Commit `feat(dash): overview — equity, KPIs, kill-switch`.

### Task 4: Markets page

- [ ] Registry data-table block: universe table (market, mid (live-flashing on tick), 1h%, 24h%, vol1h, funding_z, range_pos, day volume) sortable/filterable (native vs xyz toggle); nominees panel: current screener top-K as cards with score + side_hint + spark of r1h. Live mids via `useLiveFeed.mids` (throttled ≤1/s server-side already).
- [ ] Build + verify. Commit `feat(dash): markets — universe table + nominees`.

### Task 5: Positions & trades page

- [ ] Open positions cards/table: side badge, entry vs mark, leverage, margin, SL/TP distances as progress toward trigger, live uPnL/ROE (mids × position math client-side between events). Trade history table (action badges: open/tp/sl/time_stop/veto_close, realized PnL colored, fees). Empty-states from a registry block.
- [ ] Build + verify. Commit `feat(dash): positions + trade history`.

### Task 6: Intel page (decisions + news)

- [ ] Two live columns: analyst decisions feed (market, action, side, conviction meter, thesis text, model/latency/refusal from reason, executed/vetoed badges) and news stream (source badge `tg:…`/`rss`/`tavily`, matched-market chips, relative time, link out). Both prepend live via `useLiveFeed`.
- [ ] Build + verify. Commit `feat(dash): intel — decisions + news feeds`.

### Task 7: Polish pass

- [ ] Consistency sweep with the theme registry (spacing, mono numerics via `tabular-nums`, chart palette tokens); loading skeletons; error toasts (sonner) when REST fails / ws down >10s; favicon + title "botta terminal". Lighthouse-quick sanity (no console errors). `pnpm build` green. Commit `feat(dash): polish`.
