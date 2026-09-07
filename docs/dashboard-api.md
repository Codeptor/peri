# peri dashboard API — contract for the shadcn project

Base: `http://127.0.0.1:7411` (localhost only, read-only, no auth, **no CORS by
design**). In your Next.js app, proxy through your own origin:

```ts
// next.config.ts
async rewrites() {
  return [{ source: "/peri/:path*", destination: "http://127.0.0.1:7411/:path*" }];
}
// client code fetches "/peri/api/..." — the browser never crosses origins.
```

## Endpoints

### `GET /api/status`
```json
{"mode":"dry","network":"mainnet","model":"qwen3.8-max","cycle_secs":900,
 "version":"peri","last_decision_ts":1787824918.32,"open_positions":1,
 "realized_total":-12.77}
```

### `GET /api/decisions?limit=50` — the pipeline feed (light)
Array, newest first. Heavy fields trimmed for the list view: `prompt` omitted,
`reasoning` truncated to 400 chars.
```json
[{"id":27,"ts":1787824918.32,"trigger":"scheduled","market_view":"...",
  "model":"qwen3.8-max","latency_ms":57802,"status":"ok",
  "reasoning":"<first 400 chars>",
  "actions":[{"kind":"open","market":"xyz:NVDA","side":"long","conviction":0.8,
              "stop":214.9,"take_profit":226.0,"leverage":5,"source":"own",
              "mirror_msg_id":null,"rationale":"...","invalidation":"..."}],
  "tool_log":[{"tool":"web_search","args":{"query":"NVDA earnings"},
               "result":"<search results the model saw>"}]}]
```
`status` is `"ok"` or `"error"` (analyst down — cycle skipped; `reasoning`
carries the error).

### `GET /api/decisions/{id}` — the full glass-box trace
Same shape PLUS untruncated `reasoning` (the model's chain of thought) and
`prompt` (the EXACT context bundle the model saw: account, positions with prior
rationale/invalidation, candidates table, telegram, news, closes). 404 if absent.

> The killer page: decision detail = prompt (what it saw) → tool_log (what it
> searched) → reasoning (what it thought) → actions (what it decided) → join
> refusals/positions on ts/market (what the guard + executor did).

### `GET /api/positions` — open positions
```json
[{"id":3,"market":"SOL","side":"long","entry_px":101.05,"size":6.3722,
  "notional":643.9,"leverage":5.0,"stop_px":98.7,"tp_px":105.8,
  "conviction":0.78,"source":"own","rationale":"...","invalidation":"...",
  "status":"open","opened_ts":1787836594.5}]
```
No live mark/uPnL here — pull marks client-side if you want them live
(HL public API) or show entry/stops/tp; source can be "own" | "mirror" |
"external" (adopted from venue).

### `GET /api/closes?limit=50` — closed trades, newest first
```json
[{"market":"HYPE","side":"long","entry_px":82.31,"close_px":82.02,
  "realized_pnl":-5.01,"close_reason":"analyst","conviction":0.77,
  "closed_ts":1787825600.1}]
```
`close_reason`: `"tp"` | `"sl"` | `"analyst"` | `"external"`.

### `GET /api/refusals?limit=50` — what the guard blocked and why
```json
[{"id":1,"ts":1787826158.9,"market":"xyz:ZHIPU","action_json":"{...}",
  "reason":"xyz:ZHIPU in cooldown for 1739s"}]
```

### `GET /api/daily?limit=60` — per-UTC-day equity opens + counters
```json
[{"day":"2026-08-27","open_equity":1000.0,"entries":4,"kill_tripped":0}]
```

### `GET /api/news?limit=30` — telegram news-channel items (24h window)
```json
[{"source":"WatcherGuru","ts":1787830000.0,"text":"JUST IN: ..."}]
```
(RSS headlines are fetched per-cycle and visible inside each decision's
`prompt`; this endpoint is the live TG-channel stream.)

### `GET /api/tg?limit=50` — trading-group messages, oldest first
```json
[{"msg_id":9,"ts":1787830000.0,"sender":"h3rkk","text":"SOL Long ...",
  "is_caller":1}]
```

## Notes for the UI
- All timestamps are unix seconds UTC — show UTC + IST.
- `latency_ms` on decisions = full analyst time incl. tool rounds.
- Poll cadence: decisions change at most every ~15 min (or on caller wake);
  10–30s polling is plenty. No websocket in v1.
- Money renders: `font-mono tabular-nums`; dark-first.
- Suggested pages: Overview (status, equity, positions, last view) ·
  Decisions (feed → detail trace) · Trades (closes + refusals) · Intel
  (news + tg feed).
