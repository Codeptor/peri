# dashboard v3 — design language + build spec (reference-locked)

**Date**: 2026-08-09 · **Status**: user-approved direction (5 reference screenshots supplied; synthesized below) · **Build model**: lead-orchestrated Opus subagents; lead reviews every page via Playwright screenshots against this spec.
**Location**: `/home/esoteric/botta/dashboard` (Bun + Next 16.2.6 + Tailwind v4 + shadcn `base-sera` + hugeicons). PAPER-ONLY invariant unchanged. traderd API contract frozen (`dash-plumbing/lib/api.ts`).

## Why v2 died

v2 bolted four registries' competing aesthetics onto an 11px cramped mono terminal, unseen. v3 is the opposite: ONE design language, locked here, verified visually page by page.

## Reference synthesis (agents: this is the look — follow it literally)

Five reference dashboards share one language. In words:

1. **Ground**: calm, warm dark charcoal — not blue-black, not pure black. Cards float slightly lighter than the page. Hairline borders barely visible; separation comes from surface steps and spacing, not lines.
2. **Softness**: generous radius (~14px cards, ~10px inner tiles, pill-shaped chips), generous padding (20–24px card padding), airy gaps (16–20px). Nothing cramped. Density comes from information hierarchy, not small text.
3. **One warm accent**: orange. Used for the primary action, active nav, chart emphasis, brand moments. Semantic green/red exist ONLY for PnL/long/short/deltas — they never compete with orange for attention.
4. **Numerals are the heroes**: big mono semibold figures (24–30px) with tiny muted sans labels above (12–13px) and small tinted delta chips beside (pill, 10% tint bg, arrow glyph + percent, green/red).
5. **Icon chips**: every KPI/section lead gets a 28×28 rounded-square chip (10–12% tint of its hue) holding a 16px icon (hugeicons — the scaffold's library).
6. **Signature chart idiom — the dot matrix**: charts built from grids of small rounded squares (~8×8px, 2px radius, 2–3px gap). Inactive cells: 6% white. Active cells: solid accent/semantic color. Columns are time buckets; filled cell count = value. Three of five references use this; it is THE identifying visual of v3. Implemented once as a shared component, reused everywhere a bar chart would be.
7. **Other chart forms**: smooth area line with soft gradient fill fading to transparent (hero trends); one horizontal segmented bar (stacked, rounded ends, legend rows underneath with color dot + label + count) for composition breakdowns; thin radial progress rings for single percentages. Tooltips: dark floating card, 12px, rounded 10px.
8. **Sidebar**: 240–260px. Top: workspace ident row + collapse glyph. Search input (rounded-lg, muted). Grouped nav with 12px muted sans group labels ("Essentials", …), items = 16px icon + 13px label, active item = soft accent-tint pill, count badges right-aligned. Footer: a soft tinted card (paper-mode status) + user row.
9. **Top of page**: 18–20px semibold sans page title with small icon, filter/segmented controls row beneath (rounded-full segmented tabs like "Last 30 days | All | Active"), primary action button top-right (orange, rounded-lg).
10. **Tables**: airy rows (44–48px), 13px, sans labels + mono numerals, muted uppercase 11px header row, status as tinted pills with leading dot, row hover = 4% white. No zebra. No dense borders — a single hairline under header.
11. **Micro-labels**: where a label sits INSIDE a chart/tile context, mono uppercase 10–11px tracking-[0.14em] muted (one reference does this throughout and it reads beautifully with trading data).

## Tokens (Agent A rewrites `app/globals.css` `:root`/`.dark` with exactly these; dark-only product — root gets dark values, `.dark` mirrors them)

```css
--background: oklch(0.165 0.004 80);       /* warm near-black */
--foreground: oklch(0.95 0.005 80);
--card: oklch(0.205 0.005 80);             /* floating card */
--card-foreground: var(--foreground);
--popover: oklch(0.225 0.005 80);
--surface-2: oklch(0.245 0.006 80);        /* inner tiles, inputs, chip bgs */
--muted: oklch(0.245 0.006 80);
--muted-foreground: oklch(0.63 0.012 80);
--border: oklch(1 0 0 / 8%);
--input: oklch(1 0 0 / 10%);
--primary: oklch(0.72 0.165 55);           /* THE orange */
--primary-foreground: oklch(0.16 0.03 55);
--ring: oklch(0.72 0.165 55 / 60%);
--long: oklch(0.72 0.15 155);              /* soft emerald — PnL up / longs only */
--long-fg: oklch(0.14 0.03 155);
--short: oklch(0.65 0.19 25);              /* soft red — PnL down / shorts only */
--short-fg: oklch(0.97 0.01 25);
--warning: oklch(0.80 0.14 85);            /* amber — TP lines, caution */
--cell: oklch(1 0 0 / 6%);                 /* inactive matrix cell */
--chart-1: var(--primary);
--chart-2: var(--long);
--chart-3: oklch(0.70 0.10 230);           /* sky */
--chart-4: oklch(0.68 0.12 300);           /* violet */
--chart-5: var(--short);
--chart-6: oklch(0.60 0.01 80);            /* neutral */
--radius: 0.875rem;                        /* 14px cards */
--sidebar: oklch(0.185 0.004 80);
--sidebar-border: oklch(1 0 0 / 7%);
--sidebar-accent: oklch(0.72 0.165 55 / 12%);   /* active pill tint */
--sidebar-accent-foreground: oklch(0.85 0.09 55);
```

Fonts: Geist Sans (UI), Geist Mono (all numerals, timestamps, ids, micro-labels). `font-feature-settings: "tnum"` on mono.

## Shared primitives (Agent A builds in `components/blocks/`; pages MUST use these, never re-invent)

| component | contract |
|---|---|
| `KpiCard` | `{label, value: number\|null, format?: NumberFlow Format, deltaPct?: number\|null, deltaLabel?, icon: HugeiconsIcon, hue?: "primary"\|"long"\|"short"\|"neutral", loading}` — icon chip + label + NumberFlow mono figure + delta chip. |
| `DotMatrix` | `{columns: {value: number, color?: string, label?: string}[], max?: number, rows?: number (default 12), cellSize?: 8, gap?: 3, xLabels?: string[]}` — the signature chart. SVG. Inactive cells `var(--cell)`, active = column color (default `var(--primary)`). Tooltip per column via `<title>`. |
| `SegmentBar` | `{segments: {label, count, color}[]}` — one horizontal stacked rounded bar + legend rows (dot, label, mono count, pct). |
| `RingStat` | `{pct: 0..100, color?, size?: 56, label?, sublabel?}` — thin radial progress ring, mono pct in center. |
| `DeltaChip` | `{pct?: number\|null, text?: string}` — pill, tinted 10%, arrow + mono value, long/short color by sign. |
| `StatusPill` | `{tone: "long"\|"short"\|"warning"\|"neutral"\|"primary", children}` — tinted pill w/ leading dot. |
| `SectionCard` | `{title, icon?, action?: ReactNode, children, className?}` — the standard card: header row (icon chip + 14px semibold title + action right) + content. All page sections use it. |
| `PageHeader` | `{title, icon, meta?: ReactNode, actions?: ReactNode}` |
| `TrendArea` | soft gradient area chart for time series: `{data: {ts, value}[], color?, height?, live?}` — smooth curve, gradient fill to transparent, minimal axes (mono 10px), dark tooltip. Implementation free (recharts or hand-rolled SVG) but MUST match idiom 7. |

## Plumbing port (Agent A)

Copy from `/home/esoteric/botta/dash-plumbing/` into `dashboard/`: `lib/{api,ws,format,candles,risk,stats,utils→merge}.ts`, `app/api/hl/candles/route.ts`, `components/candle-panel.tsx` (restyle chrome colors only via tokens — its CSS-var reads already work), `fixtures/` → `fixtures/` + `public/fixtures/`, and the `/traderd` rewrite from `dash-plumbing/next.config.ts` merged into the scaffold's `next.config.ts`. Deps: `bun add lightweight-charts @number-flow/react`. **Read `dash-plumbing/README.md` "registry lessons" before touching any shadcn registry** (bklit blocks are poisoned; heatmap/scatter are Date-bound; kibo table targets tanstack v8; base-sera = Base UI — radix-isms need patching).

## Pages (Agents B–E; each owns ONLY its `app/<route>/page.tsx` + `components/<route>/*`)

Data: `lib/api.ts` (snapshot/nominees/positions/trades/equity/decisions/news/health), `lib/ws.ts` live feed, `lib/stats.ts` selectors, `lib/risk.ts` mirrors. All numerals NumberFlow or mono; all null-safe (`lib/format.ts`).

- **B `/` Overview**: KPI row ×5 (Equity w/ live tick, Unrealized, Realized today, Fees today, Win rate). Hero `TrendArea` equity (orange, range segmented-tabs 1h/8h/24h/all). Row: `DotMatrix` of last ~30 UTC days net PnL (green/red columns, height=|net|) · `SegmentBar` exit mix (tp=long, sl=short, veto=warning, other=neutral) · `RingStat` kill-budget used (killBudgetUsed × 100, color escalates long→warning→short at 50/80%). Recent fills table (10 rows: time, market, action StatusPill, px, net — idiom 10).
- **C `/positions`**: header w/ open-count + cap. Selected-position hero: `SectionCard` containing side/leverage/uPnL(NumberFlow)/ROE header + `CandlePanel` (entry/SL/TP lines). Aggregate strip (margin used + RingStat, notional exposure, open risk to SL, unrealized). Position cards grid (side accent edge, entry/mark/ROE tiles, SL/TP progress meters — soft rounded track, colored fill). Flat-book empty state (soft card, muted illustration-free, last 5 closes). History table.
- **D `/markets`**: nominees row (ranked cards: rank chip, market, side StatusPill, score + thin bar, feature chips). Screener table (sortable headers ↑↓, feature cells get SOFT tint by |z| — max 20% alpha; px flash on tick allowed but subtle). Crowding scatter (funding_z × r1h, dot size=volume, orange/neutral—NOT green/red rainbow; click → sheet). `MarketSheet` (shadcn sheet, CandlePanel + stat grid, `?m=` deep link via Suspense bridge).
- **E `/intel`**: stack-health row (4 small SectionCards: traderd api / ws / news pipe / analyst LLM — StatusPill + mono metric; refusal = `refused:true` in reason ONLY). Decisions/day `DotMatrix` (stacked hue by executed/skipped/refused → use three matrices side by side or color-mixed columns — agent picks the cleaner). Decisions feed (cards: market + action/side pills, conviction thin bar, thesis 13px sans, model+latency mono chips, raw JSON expand in a `<pre>` inside a `surface-2` tile — NO kibo code-block). News feed (source pill, title, body clamp, market tag chips → click filters).

## Shell (Agent A)

Layout: sidebar (idiom 8: BOTTA ident + PAPER StatusPill, search (⌘K opens palette later — stub input now), groups: **Essentials** Overview/Markets, **Trading** Positions/History→(anchor to overview table), **Intel** Wire; footer paper-mode tinted card w/ bankroll + equity spark (tiny `TrendArea`, 60pt) + api/ws/pipe dot row) + main region (PageHeader per page). Mobile: sidebar collapses to bottom nav (simple). Top-right global: LIVE ws dot + kill StatusPill.

## Gates (every agent, before reporting done)

`cd /home/esoteric/botta/dashboard && bun run build` green AND `NEXT_PUBLIC_FIXTURES=1 bun run build` green (fixtures mode must not regress). `bun run typecheck` clean. No edits outside owned files. Lead then screenshots via Playwright at 1440×900 against reference idioms — misses get redlined and redone.

## Hard rules

- ~~Dark-only~~ (superseded by Amendment v1.1). No gradients except chart fills. No glassmorphism. No glow. Shadows: at most `0 1px 2px oklch(0 0 0 / 20%)` on floating cards (light mode: `0 1px 2px oklch(0 0 0 / 6%)`).
- Orange is the only attention color; green/red are semantic-only.
- Every numeral mono. Every label ≤13px sans or 10–11px mono-caps. Page never scrolls horizontally.
- shadcn ui primitives (`table badge tabs skeleton input sheet button card`) via `bunx shadcn@latest add` are allowed; third-party registries only with the lessons doc read; NO new chart libraries beyond what plumbing brings (lightweight-charts) — TrendArea/DotMatrix/SegmentBar/RingStat are hand-rolled SVG.

---

## AMENDMENT v1.1 (2026-08-09, after round-1 visual review — user-locked)

Round 1 shipped near-black with mono-uppercase leaked into labels/buttons/nav — it read as the old terminal, not the references. The user then supplied the full canonical reference (**their own design**: light, airy, sans-everywhere) and required **both light and dark themes**. This amendment is normative over anything above it.

### 1. Two themes, toggleable

`next-themes` ThemeProvider returns (attribute="class", defaultTheme="system", enableSystem). A sun/moon toggle lives at the bottom of the sidebar next to the paper-mode card. Both palettes below are complete — every component must look intentional in both. The `.dark` class carries the dark palette; `:root` carries light.

**LIGHT (canonical — matches the user's own design):**
```css
--background: oklch(0.965 0.003 80);      /* soft warm light gray */
--foreground: oklch(0.24 0.01 80);
--card: oklch(0.995 0.001 80);            /* effectively white */
--popover: oklch(0.995 0.001 80);
--surface-2: oklch(0.945 0.004 80);       /* inner tiles, inputs, inactive chips */
--muted: oklch(0.945 0.004 80);
--muted-foreground: oklch(0.52 0.012 80);
--border: oklch(0.905 0.005 80);
--input: oklch(0.905 0.005 80);
--primary: oklch(0.70 0.165 55);          /* orange, slightly deeper for light bg */
--primary-foreground: oklch(0.99 0.01 55);
--long: oklch(0.62 0.15 155);  --long-fg: oklch(0.98 0.01 155);
--short: oklch(0.60 0.20 25);  --short-fg: oklch(0.98 0.01 25);
--warning: oklch(0.72 0.14 85);
--cell: oklch(0.30 0.01 80 / 7%);         /* inactive matrix cell on light */
--ring: oklch(0.70 0.165 55 / 55%);
--sidebar: oklch(0.975 0.002 80);
--sidebar-border: oklch(0.91 0.004 80);
--sidebar-accent: oklch(0.70 0.165 55 / 10%);
--sidebar-accent-foreground: oklch(0.50 0.13 55);
```

**DARK (warm charcoal — LIGHTER than round 1; matches the two dark references):**
```css
--background: oklch(0.215 0.006 75);
--foreground: oklch(0.94 0.005 75);
--card: oklch(0.26 0.007 75);
--popover: oklch(0.28 0.007 75);
--surface-2: oklch(0.305 0.008 75);
--muted: oklch(0.305 0.008 75);
--muted-foreground: oklch(0.67 0.012 75);
--border: oklch(1 0 0 / 10%);
--input: oklch(1 0 0 / 12%);
--primary: oklch(0.74 0.165 55);
--primary-foreground: oklch(0.17 0.03 55);
--long: oklch(0.74 0.145 155); --long-fg: oklch(0.15 0.03 155);
--short: oklch(0.67 0.185 25); --short-fg: oklch(0.97 0.01 25);
--warning: oklch(0.80 0.13 85);
--cell: oklch(1 0 0 / 7%);
--ring: oklch(0.74 0.165 55 / 60%);
--sidebar: oklch(0.235 0.006 75);
--sidebar-border: oklch(1 0 0 / 9%);
--sidebar-accent: oklch(0.74 0.165 55 / 14%);
--sidebar-accent-foreground: oklch(0.86 0.09 55);
```

chart-1..6 derive per-theme from the same hues (orange/emerald/sky/violet/red/neutral) at theme-appropriate lightness. Icon chips may use per-section pastel hues (sky, orange, emerald, violet — like the canonical design's chip row); orange remains the only ACTION color. UPDATE (user veto landed, Wave 1): --primary IS BLUE in both themes (light oklch(0.55 0.19 262), dark oklch(0.64 0.17 262)) matching the canonical design; orange lives on as --accent-orange for charts/data/brand only. Hue union renamed primary→accent accordingly.

### 2. Type discipline (replaces idiom 11's permissiveness)

- ALL labels, nav items, buttons, table headers, card titles, meta text: **sans, normal case** (13–14px labels, 12px table headers, 14px card titles medium, 15px semibold page sections).
- Mono is ONLY for: numeric figures, timestamps, ids/hashes, and axis tick labels. Never uppercase-tracked except axis ticks (10px, plain).
- Override base-sera's uppercase/tracking-widest Button + Badge styling in components/ui (own the source): sentence case, tracking-normal, rounded-lg.

### 3. Component redlines from round-1 screenshots

- **KpiCard**: vertical layout — row 1: icon chip + sans label (never truncate; no chip in this row); row 2: big mono figure (26–28px); row 3: DeltaChip + muted context text. Min-height ~120px, p-5.
- **TrendArea**: strokeWidth 2.25, gradient fill from 28% to 0, curve smooth; axis ticks 10px mono muted; card gets more height (~340px hero).
- **Buttons**: primary = orange rounded-lg sentence case ("Refresh", not "REFRESH").
- **Segmented controls**: rounded-full, active pill = card bg + border (light) / surface-2 (dark), NOT orange text on orange tint.
- **Sidebar**: items 38px tall, 13.5px sans; group labels 12px sans muted (not caps); count badges right-aligned; theme toggle in footer.
- **Page rhythm**: page gap 20px, card padding 20px, section titles sans 15px semibold; KPI row height equalized.
- **DotMatrix on light theme** must use `--cell` (dark cells on light ground) — verify contrast both themes.

### 4. Review gate

Playwright screenshots in BOTH themes (toggle via adding/removing `dark` class or emulating prefers-color-scheme) at 1440×900 for all four pages before the round is accepted.
