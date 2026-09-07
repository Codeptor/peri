# dash v2 Registry Rebuild Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild all four dash pages + shell from live shadcn registry components (bklit/evilcharts/kibo/openstatus) with lightweight-charts live candles carrying entry/SL/TP price lines.

**Architecture:** Registry components are vendored via `pnpm dlx shadcn@latest add @registry/item`, rethemed to the dash's oklch tokens, and composed per page. One new server-side data path (Next route handler → HL public candleSnapshot). Everything else reads the frozen traderd API via existing `lib/api.ts` + `lib/ws.ts`.

**Tech Stack:** Next 16.3 App Router, Tailwind v4, shadcn base-nova (Base UI), bklit (visx), evilcharts (recharts family only), kibo-ui, openstatus, lightweight-charts, @number-flow/react.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-09-dash-registry-rebuild-design.md`. PAPER-ONLY: no keys, no signing, no live-order paths — the HL candles route is read-only public market data.
- `lib/api.ts` types are frozen (spec §4 field-for-field) — never modify.
- Gate after EVERY task: `cd dash && pnpm build` green. Additionally at Tasks 3 and 8: `NEXT_PUBLIC_FIXTURES=1 pnpm build` green.
- pnpm only (`packageManager: pnpm@11.2.2`). No echarts. evilcharts recharts-family items only.
- Registry code: install → retheme to `app/globals.css` tokens → commit in the same task. Never leave uncommitted registry vendor drops.
- **Docs+MCP during impl (user directive):** before first use of a library API, pull current docs — context7 MCP (`resolve-library-id` → `query-docs`) for lightweight-charts and @number-flow/react; `node_modules/next/dist/docs/` for Next 16.3 route handlers; shadcn MCP `view_items_in_registries` (or `pnpm dlx shadcn@latest view`) to inspect registry items before installing.
- After installing any registry item, **Read its installed source** to learn real props before composing — never guess prop names.
- Commit messages: `feat(dash): …` / `fix(dash): …`, no co-author lines.

## File Structure

```
dash/
  components.json                     # T1 modify: registries
  app/globals.css                     # T1 modify: theme evolution
  lib/risk.ts                         # T1 create: DAILY_LOSS_LIMIT_PCT mirror
  lib/stats.ts                        # T4 create: pure trade-derived selectors
  lib/candles.ts                      # T3 create: fetch + useCandles hook
  app/api/hl/candles/route.ts         # T3 create: HL candleSnapshot proxy
  components/candle-panel.tsx         # T3 create: lightweight-charts wrapper
  components/market-ticker.tsx        # T2 create: kibo ticker tape feed
  components/command-palette.tsx      # T2 create: ⌘K jump
  components/app-sidebar.tsx          # T2 modify: sparkline + health dots
  app/layout.tsx                      # T2 modify: ticker + palette mount
  components/market-sheet.tsx         # T6 create: candle sheet for any market
  app/page.tsx                        # T4 rewrite (desk)
  app/positions/page.tsx              # T5 rewrite (book)
  app/markets/page.tsx                # T6 rewrite (screener)
  app/intel/page.tsx                  # T7 rewrite (wire)
  fixtures/candles.json               # T3 create
  components/ui/*                     # registry installs land here (+ kibo/bklit dirs per their config)
  README.md                           # T1 registries table, T8 full refresh
```

---

### Task 1: Foundation — registries, deps, theme

**Files:**
- Modify: `dash/components.json` (registries block)
- Modify: `dash/app/globals.css` (theme evolution)
- Create: `dash/lib/risk.ts`
- Modify: `dash/README.md` (registry table rows)

**Interfaces:**
- Produces: components.json registries `@bklit @evilcharts @kibo-ui @openstatus @coss @tweakcn @shadcn-dashboard`; CSS vars `--chart-1..6`, `--glow-long`, `--glow-short`; `lib/risk.ts` exports `DAILY_LOSS_LIMIT_PCT: number`.

- [ ] **Step 1: Update components.json registries**

```json
"registries": {
  "@bklit": "https://bklit.com/r/{name}.json",
  "@evilcharts": "https://evilcharts.com/r/{name}.json",
  "@kibo-ui": "https://www.kibo-ui.com/r/{name}.json",
  "@openstatus": "https://openstatus.dev/r/{name}.json",
  "@coss": "https://coss.com/ui/r/{name}.json",
  "@shadcn-dashboard": "https://shadcndashboard.dev/r/{name}.json",
  "@tweakcn": "https://tweakcn.com/r/themes/{name}.json"
}
```

(`@originui` removed — dead host. `@bklit` uses final host `bklit.com`, the documented `ui.bklit.com` 301s there.)

- [ ] **Step 2: Install npm deps**

Run: `cd dash && pnpm add lightweight-charts @number-flow/react`
Then verify versions landed in package.json; look up both packages' current majors via context7 before first use (T3/T4).

- [ ] **Step 3: Theme evolution in globals.css**

Keep every existing token. Add (values final, tune only if build reveals conflicts):

```css
/* chart ramp beyond long/short — multi-series surfaces */
--chart-1: var(--long);
--chart-2: var(--short);
--chart-3: oklch(0.75 0.12 85);   /* amber */
--chart-4: oklch(0.70 0.10 250);  /* steel blue */
--chart-5: oklch(0.72 0.12 310);  /* violet */
--chart-6: oklch(0.65 0.02 0);    /* neutral */
--glow-long: 0 0 12px oklch(0.68 0.16 150 / 45%);
--glow-short: 0 0 12px oklch(0.61 0.21 25 / 45%);
--surface-raised: oklch(0.19 0 0); /* elevated chart cards */
```

Remove the README-era prohibition comment on gradients if present in CSS comments. `prefers-reduced-motion` block stays.

- [ ] **Step 4: lib/risk.ts mirroring traderd.toml**

Read `traderd/traderd.toml`, find the daily loss limit knob (grep `daily` / `loss`), mirror the value:

```ts
// Mirrors traderd/traderd.toml [risk] daily loss limit — update if the toml changes.
export const DAILY_LOSS_LIMIT_PCT = <value from toml>;
```

(If traderd.toml has no such knob, grep `traderd/src/risk*.rs` for the hardcoded default and mirror that, citing the file in the comment.)

- [ ] **Step 5: README registry table** — replace the stale status column (kibo 500 / origin moved) with the verified 2026-08-09 evening state from the spec.

- [ ] **Step 6: Gate + commit**

Run: `cd dash && pnpm build` → green.
`git add -A dash && git commit -m "feat(dash): registry roster v2, chart/glow tokens, risk mirror"`

---

### Task 2: Shell — ticker tape, palette, sidebar

**Files:**
- Create: `dash/components/market-ticker.tsx`, `dash/components/command-palette.tsx`
- Modify: `dash/components/app-sidebar.tsx`, `dash/app/layout.tsx`
- Registry installs: `@kibo-ui/ticker`, `@shadcn` `command` + `dialog` (base-nova resolved), `@bklit/stat-card-line-01` (sidebar sparkline basis)

**Interfaces:**
- Consumes: `api.snapshot() api.equity() api.health()` (lib/api.ts), `useLiveFeed` (lib/ws.ts).
- Produces: `<MarketTicker/>` (no props, self-polling 15s); `<CommandPalette/>` (no props, global ⌘K, `router.push('/markets?m='+coin)` on market select); sidebar renders equity sparkline (poll `api.equity(120)` every 30s) + three health dots (api ok / ws_connected / pipe fresh = latest `api.news(1)` item ts < 30min).

- [ ] **Step 1: Inspect then install registry items** — `pnpm dlx shadcn@latest view @kibo-ui/ticker` (or shadcn MCP view) to see files/props, then `pnpm dlx shadcn@latest add @kibo-ui/ticker command @bklit/stat-card-line-01`. Read installed sources.
- [ ] **Step 2: market-ticker.tsx** — map snapshot rows to ticker items: `pct24h = prev_day_px > 0 ? (mid - prev_day_px)/prev_day_px : null`, color `pct24h >= 0 ? long : short`, `onClick → router.push('/markets?m='+market)`. Mount in `app/layout.tsx` under the top bar. Poll `api.snapshot()` every 15s, pause when `document.hidden`.
- [ ] **Step 3: command-palette.tsx** — shadcn `Command` in dialog, ⌘K/ctrl-K listener; groups: Pages (4 routes), Markets (from snapshot poll shared via module-scope cache or its own fetch on open). Mount in layout.
- [ ] **Step 4: app-sidebar.tsx** — add sparkline (bklit stat-card-line internals rethemed, 120-point equity, long/short stroke by first-vs-last) + health dots row (`bg-long`/`bg-short` 6px dots + tooltip labels).
- [ ] **Step 5: Retheme** all installed items to tokens (mono numerics in ticker, hairline separators, no stray palette classes).
- [ ] **Step 6: Gate + commit** — `pnpm build` green; `git commit -m "feat(dash): shell v2 — market tape, ⌘K palette, sidebar sparkline+health"`.

---

### Task 3: Candle infrastructure

**Files:**
- Create: `dash/app/api/hl/candles/route.ts`, `dash/lib/candles.ts`, `dash/components/candle-panel.tsx`, `dash/fixtures/candles.json`

**Interfaces:**
- Produces:
  - `GET /api/hl/candles?coin=SOL&interval=1m&lookback_h=12` → `Candle[]`
  - `lib/candles.ts`: `export type Candle = { t: number; o: number; h: number; l: number; c: number; v: number }`; `export function useCandles(coin: string | null, interval: "1m" | "5m" | "15m"): { candles: Candle[] | null; error: string | null }` (fixtures-aware; subscribes to ws mids and mutates last bar in place via returned array identity change).
  - `components/candle-panel.tsx`: `export function CandlePanel(props: { coin: string; interval?: "1m" | "5m" | "15m"; height?: number; lines?: { entry?: number; sl?: number; tp?: number }; })` — renders lightweight-charts candlesticks + one priceLine per defined line (entry `--long`, sl `--short`, tp `--warning`), falls back to a line series of closes when the candle fetch errors (xyz: fallback path).

- [ ] **Step 1: Probe HL naming for HIP-3 coins** (records the xyz: risk verdict):

```bash
curl -s https://api.hyperliquid.xyz/info -H 'Content-Type: application/json' \
  -d '{"type":"candleSnapshot","req":{"coin":"xyz:NATGAS","interval":"1m","startTime":<now-2h ms>,"endTime":<now ms>}}' | head -c 300
# and retry with "NATGAS" if empty/error; record working form (or "no candles for HIP-3") in dash/README.md
```

- [ ] **Step 2: Read Next 16.3 route-handler doc** — `ls node_modules/next/dist/docs/` and read the route-handlers guide before writing route.ts.
- [ ] **Step 3: route.ts** (shape final; adjust only per Step 2 doc findings):

```ts
import { NextRequest, NextResponse } from "next/server";

const HL = "https://api.hyperliquid.xyz/info";
const INTERVALS = new Set(["1m", "5m", "15m"]);

export async function GET(req: NextRequest) {
  const coin = req.nextUrl.searchParams.get("coin") ?? "";
  const interval = req.nextUrl.searchParams.get("interval") ?? "1m";
  const lookbackH = Math.min(Number(req.nextUrl.searchParams.get("lookback_h") ?? 12), 72);
  if (!coin || !INTERVALS.has(interval))
    return NextResponse.json({ error: "bad params" }, { status: 400 });
  const end = Date.now();
  const start = end - lookbackH * 3600_000;
  const res = await fetch(HL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ type: "candleSnapshot", req: { coin, interval, startTime: start, endTime: end } }),
    cache: "no-store",
  });
  if (!res.ok) return NextResponse.json({ error: `hl ${res.status}` }, { status: 502 });
  const raw = (await res.json()) as Array<{ t: number; o: string; h: string; l: string; c: string; v: string }>;
  if (!Array.isArray(raw) || raw.length === 0)
    return NextResponse.json({ error: "no candles" }, { status: 404 });
  return NextResponse.json(raw.map((k) => ({ t: k.t, o: +k.o, h: +k.h, l: +k.l, c: +k.c, v: +k.v })));
}
```

- [ ] **Step 4: fixtures/candles.json** — generate synthetic walks for the fixture markets (python one-liner: 200 1m bars per market, seeded random walk around the fixture mid); `useCandles` serves `candles.json[coin]` when `NEXT_PUBLIC_FIXTURES=1` and skips ws updates.
- [ ] **Step 5: lib/candles.ts** — history fetch per interfaces above + live update: subscribe `useLiveFeed` mids; on tick for `coin`, extend/replace last bar (`h=max(h,px) l=min(l,px) c=px`, roll new bar when `now - lastBar.t >= intervalMs`).
- [ ] **Step 6: candle-panel.tsx** — FIRST pull lightweight-charts docs via context7 (v5 series API + priceLine options), THEN implement: `createChart(el, {layout:{background transparent, textColor from tokens}, grid hairlines, timeScale seconds)`; candlestick series colored `--long`/`--short`; `series.createPriceLine({ price, title, color, lineWidth: 1, lineStyle: dashed })` per entry/sl/tp; `series.update(bar)` on live mutation; `ResizeObserver` for width; dispose on unmount.
- [ ] **Step 7: Gates + commit** — `pnpm build` AND `NEXT_PUBLIC_FIXTURES=1 pnpm build` green; `git commit -m "feat(dash): candle infra — HL proxy, useCandles, lightweight-charts panel"`.

---

### Task 4: `/` — the desk

**Files:**
- Rewrite: `dash/app/page.tsx`
- Create: `dash/lib/stats.ts`
- Registry installs: `@bklit/stat-card-area-01 @bklit/area-chart @bklit/reference-area @bklit/shimmering-text @bklit/gauge-chart`, evilcharts donut pie (`ex-donut-echarts-pie-chart` has a recharts sibling — pick the `recharts` family donut, verify exact name via `view` before add), `@kibo-ui/contribution-graph @kibo-ui/table @kibo-ui/pill @kibo-ui/relative-time`

**Interfaces:**
- Produces `lib/stats.ts` (pure, UTC day bucketing, consumed by T5/T7):

```ts
import type { Trade } from "./api";
export type DayPnl = { date: string; net: number };            // date = YYYY-MM-DD UTC
export function isClose(t: Trade): boolean;                     // action !== "open"
export function netPnl(t: Trade): number;                       // (realized_pnl ?? 0) - fee
export function dailyNetPnl(trades: Trade[]): DayPnl[];         // all trades grouped by UTC day, Σ netPnl
export function exitMix(trades: Trade[]): { tp: number; sl: number; veto_close: number; other: number };
export function winRate(trades: Trade[]): number | null;        // closes with realized_pnl>0 / closes; null if 0 closes
export function todayStats(trades: Trade[], now: number): { realized: number; fees: number; closes: number };
export function lossBudgetUsed(equity: EquityPoint[], limitPct: number): number; // (dayPeak - last)/(dayStart*limitPct), clamped 0..1
```

- [ ] **Step 1: Install + Read registry items** (view first where props unclear).
- [ ] **Step 2: lib/stats.ts** with the exact signatures above (implement all; no stubs).
- [ ] **Step 3: Rebuild page.tsx** — rows per spec: NumberFlow stat tiles (equity live from ws/health poll, realized/fees/win-rate from `todayStats`/`winRate`); bklit area equity hero (gradient long-color fill, `reference-area` shading spans where equity < running peak − 1%, shimmering-text while loading, timeframe buttons 1h/8h/24h/all → `api.equity(points)` with points 120/960/2880/5000); contribution-graph fed `dailyNetPnl` (green/red intensity by |net| quantile); donut fed `exitMix`; gauge fed `lossBudgetUsed(equity, DAILY_LOSS_LIMIT_PCT)`; trades table (kibo table + pill + relative-time, 20 rows, existing flash-on-insert class names preserved).
- [ ] **Step 4: Retheme installed items** to tokens; delete any demo/example files the CLI dropped.
- [ ] **Step 5: Gate + commit** — `pnpm build`; `git commit -m "feat(dash): desk rebuilt — live tiles, bklit equity hero, pnl calendar, exit mix, loss gauge"`.

---

### Task 5: `/positions` — the book

**Files:**
- Rewrite: `dash/app/positions/page.tsx`
- Registry installs: evilcharts glowing line (recharts family — verify exact name via view; candidates `ex-glowing-desktop-*` recharts sibling), `@openstatus/status-blank` (or compose kibo `banner` if status-blank drags the whole status-page payload — decide by viewing its file list first)

**Interfaces:**
- Consumes: `CandlePanel` (T3), kibo table/pill/relative-time (T4 installs), `lib/stats.ts` `netPnl`, `useLiveFeed` for mark/uPnL ticks.

- [ ] **Step 1: Selection state** — `selected: number | null` (position id, default first open position); hero `<CandlePanel coin={pos.market} lines={{entry: pos.entry_px, sl: pos.sl_px, tp: pos.tp_px}} height={420}/>` + header (side pill, size, leverage, NumberFlow uPnL + ROE, live via ws mids enrichment already in lib/ws.ts).
- [ ] **Step 2: Table** — kibo table: rows clickable → select; columns side/entry/mark/uPnL(NumberFlow)/ROE + stop-distance meter: `pctToStop = |mark - sl| / |entry - sl|` rendered as inline 40px progress (long color → short color as it approaches 0), + 30-point uPnL sparkline from a ring buffer of ws ticks per position (client memory only).
- [ ] **Step 3: Aggregate strip** — margin used Σ, exposure gauge (Σ|size×mark| vs equity × leverage cap from traderd.toml mirrored constant if present in `lib/risk.ts`), open risk Σ(|mark−sl|×size).
- [ ] **Step 4: Empty state** — "book is flat" + last 5 closes (trades where isClose, with netPnl colored).
- [ ] **Step 5: Gate + commit** — `pnpm build`; `git commit -m "feat(dash): book rebuilt — live candle hero with entry/SL/TP lines, stop meters, risk strip"`.

---

### Task 6: `/markets` — the screener

**Files:**
- Rewrite: `dash/app/markets/page.tsx`
- Create: `dash/components/market-sheet.tsx`
- Registry installs: `@bklit/heatmap-chart @bklit/scatter-chart`, shadcn `sheet` + `tabs` (tabs already installed — reuse)

**Interfaces:**
- Produces: `<MarketSheet coin={string | null} row={MarketRow | null} onOpenChange={(open:boolean)=>void}/>` — sheet with CandlePanel (no lines) + stat grid (funding, funding_z, OI, day volume, range_pos). Reads nothing global; T2's palette and ticker reach it via `/markets?m=<coin>` (this page reads the param on mount and opens the sheet).

- [ ] **Step 1: Screener grid** — kibo table over snapshot: px (fmtPrice), 24h%, funding + funding_z (z badge colored |z|>2), OI, day volume (compact), r5m/r1h signed, vol1h, range_pos as 40px inline bar. Client sort on header click (default: day volume desc), text filter input. 10s snapshot poll (reuse existing page's poll pattern — read old page before deleting).
- [ ] **Step 2: Tabs viz** — Heatmap: markets (top 30 by volume) × 6 features, per-feature z-normalized color ramp (`--chart-*`); Scatter: x=funding_z y=r1h size=day volume, dot color by r24h sign, tooltip market name.
- [ ] **Step 3: market-sheet.tsx** + `?m=` param handling (`useSearchParams`, open on match, `router.replace` to clear on close).
- [ ] **Step 4: Nominees rail** — right column: score bar (0..1 → width), side-hint pill, features chips, relative-time.
- [ ] **Step 5: Gate + commit** — `pnpm build`; `git commit -m "feat(dash): screener rebuilt — sortable grid, feature heatmap, crowding scatter, candle sheet"`.

---

### Task 7: `/intel` — the wire

**Files:**
- Rewrite: `dash/app/intel/page.tsx`
- Registry installs: `@kibo-ui/list @kibo-ui/code-block`, `@openstatus/status-component-group` (+ its status-* deps as the registry resolves them), evilcharts recharts bar (verify name via view)

**Interfaces:**
- Consumes: `api.news api.decisions api.health`, `lib/stats.ts`, pipe-freshness rule from T2 (last news ts < 30min).

- [ ] **Step 1: News feed** — kibo list: source pill, title (bold) + body (clamp-3, expand), market tag chips (click toggles filter), relative-time, url out-link icon. Poll 30s.
- [ ] **Step 2: Decisions log** — per decision: action/side pills, conviction meter (inline bar 0..1), thesis prose (sans, not mono), horizon badge, `vetoed` red badge, `executed` check, refusal badge ONLY when the decision parses as refused:true (preserve 99cb9ef semantics — port the existing predicate from the old page before deleting it), code-block expand with raw JSON.
- [ ] **Step 3: Stack health** — openstatus status-component-group rethemed: traderd API (health.ok), WS (ws_connected), pipe (news freshness), LLM (refusal rate last 50 decisions, warn >20%). status-timestamp for last-updated.
- [ ] **Step 4: Refusal/skip bar chart** — last 7 UTC days: decisions per day stacked open/skip/refused (evilcharts bar, chart ramp colors).
- [ ] **Step 5: Gate + commit** — `pnpm build`; `git commit -m "feat(dash): wire rebuilt — news feed, decisions log, stack health, refusal chart"`.

---

### Task 8: Cleanup + docs sync

**Files:**
- Delete: `dash/components/ui/stat-card.tsx`, `dash/components/ui/empty-state.tsx`; `dash/components/ui/chart.tsx` iff `grep -r "ui/chart" dash/app dash/components` is empty
- Modify: `dash/README.md` (design-token section rewrite: evolved aesthetic, new registries table, candle architecture, xyz: verdict), `docs/superpowers/STATUS.md` (dash v2 entry + spec/plan links)

- [ ] **Step 1: Delete orphans** — remove the three files (chart.tsx conditional on grep), fix any dangling imports.
- [ ] **Step 2: README rewrite** — registries verified table, new deps, candle data path diagram, evolved token rules (gradients/glow permitted), page map.
- [ ] **Step 3: STATUS.md** — one entry: dash v2 shipped, commits list, spec+plan paths, open follow-ups if any.
- [ ] **Step 4: Final gates** — `pnpm build` + `NEXT_PUBLIC_FIXTURES=1 pnpm build` + `pnpm lint` all green.
- [ ] **Step 5: Commit** — `git commit -m "feat(dash): v2 cleanup — orphans removed, docs synced"`.

## Self-review notes

- Spec coverage: foundation→T1, shell→T2, candles+fixtures+xyz-probe→T3, desk→T4, book→T5, screener→T6, wire→T7, deletions+docs→T8. Command palette (spec Shell) → T2. Fixtures builds gated T3+T8 per spec. ✔
- Types: `Candle {t,o,h,l,c,v}` consistent across route/lib/panel; `lib/stats.ts` signatures consumed by T4/T5/T7 as written. `MarketSheet` prop name `onOpenChange` used consistently (T6 only). ✔
- No placeholders: registry prop unknowns are resolved by mandated view/Read steps, not guessed code. ✔
