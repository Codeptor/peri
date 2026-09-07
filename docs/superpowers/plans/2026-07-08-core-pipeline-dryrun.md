# Trench Copy-Trade Bot — Plan 1: Core Pipeline (Dry-Run) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working daemon that reads the Telegram group "the caller group", filters to @caller1, parses his messages into structured trade intents, applies risk checks, and records what it *would* trade — via a dry-run router that places no real orders.

**Architecture:** A single asyncio pipeline: Telethon listener → parser (regex fast-path → LLM fallback) → risk manager → position manager → venue router. In Plan 1 the router uses a `DryRunAdapter` that records intended actions to SQLite instead of executing. Every unit is pure/testable in isolation; the listener extracts a `handle_message()` function so the whole pipeline is unit-testable without live Telegram.

**Tech Stack:** Python 3.12, uv, Telethon (MTProto), Pydantic v2, httpx (LLM gateway), sqlite3 (stdlib), tomllib (stdlib), pytest + pytest-asyncio.

## Global Constraints

- **Python 3.12+**, package manager **uv** only (never pip/venv directly). Run tests with `uv run pytest`.
- **Signal source:** only messages from allowlisted senders (`@caller1`) are ever parsed for action.
- **Min trade:** 5 USDC margin (user minimum); default leverage 3x; min-notional guard must clear each venue's minimum order value.
- **Dry-run is the default mode.** Plan 1 never places a real order; the router only records intended actions.
- **Never trade on a guess:** intents below the configured confidence threshold are treated as NoOp (dry-run) or routed to a confirm-DM (live, Plan 2+).
- **Idempotency:** every acted `message_id` is persisted; a message is never processed twice (survives restart).
- **Stale-signal guard:** ignore any message older than `stale_secs` (default 120) — no replaying backlog as fresh trades.
- **Secrets** (`.env`, `botta.session`, keys) are gitignored, never committed. `.env` already holds `TG_API_ID`/`TG_API_HASH`.
- **Asset universe (v1):** SOL, BTC, HYPE, GOLD, ETH (normalize case/`$`/aliases). Unknown assets → NoOp.
- Commit messages: concise imperative, scoped prefixes (`feat:`, `test:`, `fix:`). No co-author lines.

---

## File Structure

```
botta/
  pyproject.toml              # uv project + deps + pytest config
  config.toml                 # risk knobs, asset→venue map, telegram ids, mode
  src/botta/
    __init__.py
    config.py                 # load config.toml + .env → Config
    models.py                 # Intent models (Entry/Manage/Exit/NoOp) + normalize_asset
    state.py                  # SQLite: idempotency, positions, trades, daily
    llm.py                    # LLM gateway client (mocked in tests)
    parser.py                 # regex fast-path → LLM fallback → Intent
    risk.py                   # sizing, min-notional, caps, dedupe, stale, kill switch
    positions.py              # resolve position_ref → open position (or ambiguous)
    router.py                 # Adapter protocol + DryRunAdapter
    notifier.py               # notification formatting + control-command parsing
    listener.py               # Telethon wiring + handle_message()
    app.py                    # entrypoint: builds deps, runs listener
  tests/
    __init__.py
    fixtures/__init__.py
    fixtures/messages.py      # synthetic message-shape fixtures
    test_config.py
    test_models.py
    test_state.py
    test_parser.py
    test_risk.py
    test_positions.py
    test_router.py
    test_notifier.py
    test_pipeline.py
```

---

### Task 1: Project scaffold

**Files:**
- Create: `pyproject.toml`, `src/botta/__init__.py`, `tests/__init__.py`, `tests/fixtures/__init__.py`, `config.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: an installable `botta` package; `uv run pytest` executable.

- [ ] **Step 1: Write `pyproject.toml`**

```toml
[project]
name = "botta"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
    "telethon>=1.36",
    "pydantic>=2.7",
    "httpx>=0.27",
]

[dependency-groups]
dev = ["pytest>=8", "pytest-asyncio>=0.23", "ruff>=0.6"]

[tool.pytest.ini_options]
pythonpath = ["src"]
asyncio_mode = "auto"
testpaths = ["tests"]

[tool.ruff]
line-length = 110
```

- [ ] **Step 2: Create package files**

```bash
mkdir -p src/botta tests/fixtures
touch src/botta/__init__.py tests/__init__.py tests/fixtures/__init__.py
```

- [ ] **Step 3: Write `config.toml`**

```toml
mode = "dry"                       # "dry" | "live"

[telegram]
group_id = -1001234567890          # the caller group
allowlist = ["caller1"]              # usernames (no @) whose calls are acted on
control_user = 0                   # your own user id for /commands + DMs (set later)

[risk]
margin_usdc = 5.0
leverage = 3.0
default_sl_pct = 1.5               # used when a call gives no SL
max_concurrent = 3
daily_cap = 15
slippage_pct = 0.5
kill_switch_pct = 15.0             # halt if aggregate equity down this % from day open
stale_secs = 120
confidence_threshold = 0.6

[venues]
default_perp = "hyperliquid"
# asset -> venue ("hyperliquid" | "drift" | "jupiter")
[venues.asset_map]
SOL = "hyperliquid"
BTC = "hyperliquid"
HYPE = "hyperliquid"
GOLD = "hyperliquid"
ETH = "hyperliquid"
```

- [ ] **Step 4: Add a placeholder test and run it**

```python
# tests/test_smoke.py
def test_smoke():
    assert True
```

Run: `uv run pytest -q`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add pyproject.toml config.toml src tests
git commit -m "feat: scaffold botta uv project + pytest"
```

---

### Task 2: Config loader

**Files:**
- Create: `src/botta/config.py`, `tests/test_config.py`

**Interfaces:**
- Consumes: `config.toml`, `.env`.
- Produces:
  - `load_config(path: str = "config.toml") -> Config`
  - `Config` dataclass: `mode: str`, `telegram: TelegramCfg`, `risk: RiskCfg`, `venues: VenuesCfg`, `tg_api_id: int`, `tg_api_hash: str`.
  - `TelegramCfg(group_id:int, allowlist:list[str], control_user:int)`
  - `RiskCfg(margin_usdc, leverage, default_sl_pct, max_concurrent, daily_cap, slippage_pct, kill_switch_pct, stale_secs, confidence_threshold)`
  - `VenuesCfg(default_perp:str, asset_map:dict[str,str])`

- [ ] **Step 1: Write the failing test**

```python
# tests/test_config.py
from botta.config import load_config

def test_load_config(tmp_path, monkeypatch):
    (tmp_path / "config.toml").write_text(
        'mode="dry"\n'
        '[telegram]\ngroup_id=-100\nallowlist=["caller1"]\ncontrol_user=42\n'
        '[risk]\nmargin_usdc=5.0\nleverage=3.0\ndefault_sl_pct=1.5\nmax_concurrent=3\n'
        'daily_cap=15\nslippage_pct=0.5\nkill_switch_pct=15.0\nstale_secs=120\nconfidence_threshold=0.6\n'
        '[venues]\ndefault_perp="hyperliquid"\n[venues.asset_map]\nSOL="hyperliquid"\n'
    )
    (tmp_path / ".env").write_text("TG_API_ID=123\nTG_API_HASH=abc\n")
    monkeypatch.chdir(tmp_path)
    cfg = load_config()
    assert cfg.mode == "dry"
    assert cfg.telegram.group_id == -100
    assert cfg.telegram.allowlist == ["caller1"]
    assert cfg.risk.margin_usdc == 5.0
    assert cfg.venues.asset_map["SOL"] == "hyperliquid"
    assert cfg.tg_api_id == 123
    assert cfg.tg_api_hash == "abc"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_config.py -q`
Expected: FAIL (ModuleNotFoundError: botta.config).

- [ ] **Step 3: Write `src/botta/config.py`**

```python
import os
import tomllib
from dataclasses import dataclass


@dataclass
class TelegramCfg:
    group_id: int
    allowlist: list[str]
    control_user: int


@dataclass
class RiskCfg:
    margin_usdc: float
    leverage: float
    default_sl_pct: float
    max_concurrent: int
    daily_cap: int
    slippage_pct: float
    kill_switch_pct: float
    stale_secs: int
    confidence_threshold: float


@dataclass
class VenuesCfg:
    default_perp: str
    asset_map: dict[str, str]


@dataclass
class Config:
    mode: str
    telegram: TelegramCfg
    risk: RiskCfg
    venues: VenuesCfg
    tg_api_id: int
    tg_api_hash: str


def _load_env(path: str = ".env") -> dict[str, str]:
    env: dict[str, str] = {}
    if os.path.exists(path):
        for line in open(path):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    return env


def load_config(path: str = "config.toml") -> Config:
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    env = _load_env()
    t, r, v = raw["telegram"], raw["risk"], raw["venues"]
    return Config(
        mode=raw["mode"],
        telegram=TelegramCfg(t["group_id"], list(t["allowlist"]), t["control_user"]),
        risk=RiskCfg(
            r["margin_usdc"], r["leverage"], r["default_sl_pct"], r["max_concurrent"],
            r["daily_cap"], r["slippage_pct"], r["kill_switch_pct"], r["stale_secs"],
            r["confidence_threshold"],
        ),
        venues=VenuesCfg(v["default_perp"], dict(v["asset_map"])),
        tg_api_id=int(env.get("TG_API_ID", os.environ.get("TG_API_ID", "0"))),
        tg_api_hash=env.get("TG_API_HASH", os.environ.get("TG_API_HASH", "")),
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_config.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/config.py tests/test_config.py
git commit -m "feat: config loader (config.toml + .env)"
```

---

### Task 3: Asset normalization + Intent models

**Files:**
- Create: `src/botta/models.py`, `tests/test_models.py`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `normalize_asset(s: str) -> str | None` — uppercases, strips `$`, maps aliases (SOLANA→SOL, BITCOIN→BTC); returns None if not in the v1 universe.
  - `Side = Literal["long","short"]`
  - `EntryIntent(kind="entry", asset:str, side:Side, entry:float|str="market", tps:list[float]=[], sl:float|None, sl_pct:float|None, confidence:float, source_msg_id:int)`
  - `ManageIntent(kind="manage", op:Literal["book_partial","move_sl","trail_sl"], pct:float=50.0, sl_target:str|None, position_ref:str|None, confidence:float, source_msg_id:int)`
  - `ExitIntent(kind="exit", position_ref:str|None, confidence:float, source_msg_id:int)`
  - `NoOp(kind="noop", confidence:float=1.0, source_msg_id:int)`
  - `Intent = EntryIntent | ManageIntent | ExitIntent | NoOp`

- [ ] **Step 1: Write the failing test**

```python
# tests/test_models.py
from botta.models import normalize_asset, EntryIntent, NoOp

def test_normalize_asset():
    assert normalize_asset("sol") == "SOL"
    assert normalize_asset("$BTC") == "BTC"
    assert normalize_asset("Solana") == "SOL"
    assert normalize_asset("random") is None

def test_entry_intent_defaults():
    e = EntryIntent(asset="SOL", side="long", confidence=0.9, source_msg_id=1)
    assert e.kind == "entry"
    assert e.entry == "market"
    assert e.tps == []
    assert e.sl is None

def test_noop():
    assert NoOp(source_msg_id=5).kind == "noop"
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_models.py -q`
Expected: FAIL (ModuleNotFoundError: botta.models).

- [ ] **Step 3: Write `src/botta/models.py`**

```python
from typing import Literal, Optional, Union

from pydantic import BaseModel

Side = Literal["long", "short"]

_UNIVERSE = {"SOL", "BTC", "HYPE", "GOLD", "ETH"}
_ALIASES = {"SOLANA": "SOL", "BITCOIN": "BTC", "ETHEREUM": "ETH", "XAU": "GOLD", "GOLDUSD": "GOLD"}


def normalize_asset(s: str) -> Optional[str]:
    t = s.strip().upper().lstrip("$")
    t = _ALIASES.get(t, t)
    return t if t in _UNIVERSE else None


class EntryIntent(BaseModel):
    kind: Literal["entry"] = "entry"
    asset: str
    side: Side
    entry: Union[float, Literal["market"]] = "market"
    tps: list[float] = []
    sl: Optional[float] = None
    sl_pct: Optional[float] = None
    confidence: float
    source_msg_id: int


class ManageIntent(BaseModel):
    kind: Literal["manage"] = "manage"
    op: Literal["book_partial", "move_sl", "trail_sl"]
    pct: float = 50.0
    sl_target: Optional[str] = None
    position_ref: Optional[str] = None
    confidence: float
    source_msg_id: int


class ExitIntent(BaseModel):
    kind: Literal["exit"] = "exit"
    position_ref: Optional[str] = None
    confidence: float
    source_msg_id: int


class NoOp(BaseModel):
    kind: Literal["noop"] = "noop"
    confidence: float = 1.0
    source_msg_id: int


Intent = Union[EntryIntent, ManageIntent, ExitIntent, NoOp]
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_models.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/models.py tests/test_models.py
git commit -m "feat: intent models + asset normalization"
```

---

### Task 4: SQLite state (idempotency, positions, trades, daily)

**Files:**
- Create: `src/botta/state.py`, `tests/test_state.py`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `Position` dataclass: `id:int, venue:str, asset:str, side:str, entry:float, size:float, leverage:float, sl_price:float|None, status:str, opened_ts:float`.
  - `State(db_path:str)` with:
    - `is_processed(msg_id:int) -> bool`
    - `mark_processed(msg_id:int, intent_json:str, acted:bool) -> None`
    - `add_position(venue, asset, side, entry, size, leverage, sl_price) -> Position`
    - `open_positions() -> list[Position]`
    - `open_position_for(asset:str) -> Position | None`
    - `update_size(pos_id:int, size:float) -> None`
    - `update_sl(pos_id:int, sl_price:float) -> None`
    - `close_position(pos_id:int) -> None`
    - `entries_today(day:str) -> int`

- [ ] **Step 1: Write the failing test**

```python
# tests/test_state.py
from botta.state import State

def test_idempotency(tmp_path):
    s = State(str(tmp_path / "b.db"))
    assert not s.is_processed(10)
    s.mark_processed(10, "{}", acted=True)
    assert s.is_processed(10)

def test_positions(tmp_path):
    s = State(str(tmp_path / "b.db"))
    p = s.add_position("hyperliquid", "SOL", "long", 81.2, 15.0, 3.0, 79.9)
    assert p.id > 0
    assert s.open_position_for("SOL").entry == 81.2
    assert len(s.open_positions()) == 1
    s.update_size(p.id, 7.5)
    s.update_sl(p.id, 81.2)
    reloaded = s.open_position_for("SOL")
    assert reloaded.size == 7.5 and reloaded.sl_price == 81.2
    s.close_position(p.id)
    assert s.open_position_for("SOL") is None
    assert s.open_positions() == []
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_state.py -q`
Expected: FAIL (ModuleNotFoundError: botta.state).

- [ ] **Step 3: Write `src/botta/state.py`**

```python
import sqlite3
import time
from dataclasses import dataclass
from typing import Optional


@dataclass
class Position:
    id: int
    venue: str
    asset: str
    side: str
    entry: float
    size: float
    leverage: float
    sl_price: Optional[float]
    status: str
    opened_ts: float


_SCHEMA = """
CREATE TABLE IF NOT EXISTS processed_messages (
    msg_id INTEGER PRIMARY KEY, acted INTEGER, intent_json TEXT, ts REAL);
CREATE TABLE IF NOT EXISTS positions (
    id INTEGER PRIMARY KEY AUTOINCREMENT, venue TEXT, asset TEXT, side TEXT,
    entry REAL, size REAL, leverage REAL, sl_price REAL, status TEXT, opened_ts REAL);
CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT, position_id INTEGER, action TEXT,
    price REAL, size REAL, ts REAL);
CREATE TABLE IF NOT EXISTS daily (
    day TEXT PRIMARY KEY, start_equity REAL, realized_pnl REAL,
    entries_count INTEGER, killswitch_tripped INTEGER);
"""


class State:
    def __init__(self, db_path: str):
        self.db = sqlite3.connect(db_path)
        self.db.row_factory = sqlite3.Row
        self.db.executescript(_SCHEMA)
        self.db.commit()

    def is_processed(self, msg_id: int) -> bool:
        cur = self.db.execute("SELECT 1 FROM processed_messages WHERE msg_id=?", (msg_id,))
        return cur.fetchone() is not None

    def mark_processed(self, msg_id: int, intent_json: str, acted: bool) -> None:
        self.db.execute(
            "INSERT OR REPLACE INTO processed_messages VALUES (?,?,?,?)",
            (msg_id, int(acted), intent_json, time.time()),
        )
        self.db.commit()

    def _row_to_pos(self, r: sqlite3.Row) -> Position:
        return Position(r["id"], r["venue"], r["asset"], r["side"], r["entry"], r["size"],
                        r["leverage"], r["sl_price"], r["status"], r["opened_ts"])

    def add_position(self, venue, asset, side, entry, size, leverage, sl_price) -> Position:
        cur = self.db.execute(
            "INSERT INTO positions (venue,asset,side,entry,size,leverage,sl_price,status,opened_ts)"
            " VALUES (?,?,?,?,?,?,?, 'open', ?)",
            (venue, asset, side, entry, size, leverage, sl_price, time.time()),
        )
        self.db.commit()
        return self.open_positions_by_id(cur.lastrowid)

    def open_positions_by_id(self, pos_id: int) -> Position:
        r = self.db.execute("SELECT * FROM positions WHERE id=?", (pos_id,)).fetchone()
        return self._row_to_pos(r)

    def open_positions(self) -> list[Position]:
        rows = self.db.execute("SELECT * FROM positions WHERE status='open' ORDER BY opened_ts").fetchall()
        return [self._row_to_pos(r) for r in rows]

    def open_position_for(self, asset: str) -> Optional[Position]:
        r = self.db.execute(
            "SELECT * FROM positions WHERE status='open' AND asset=? ORDER BY opened_ts DESC LIMIT 1",
            (asset,),
        ).fetchone()
        return self._row_to_pos(r) if r else None

    def update_size(self, pos_id: int, size: float) -> None:
        self.db.execute("UPDATE positions SET size=? WHERE id=?", (size, pos_id))
        self.db.commit()

    def update_sl(self, pos_id: int, sl_price: float) -> None:
        self.db.execute("UPDATE positions SET sl_price=? WHERE id=?", (sl_price, pos_id))
        self.db.commit()

    def close_position(self, pos_id: int) -> None:
        self.db.execute("UPDATE positions SET status='closed' WHERE id=?", (pos_id,))
        self.db.commit()

    def entries_today(self, day: str) -> int:
        r = self.db.execute("SELECT entries_count FROM daily WHERE day=?", (day,)).fetchone()
        return r["entries_count"] if r else 0
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_state.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/state.py tests/test_state.py
git commit -m "feat: sqlite state (idempotency + positions)"
```

---

### Task 5: Message fixtures

**Files:**
- Create: `tests/fixtures/messages.py`

**Interfaces:**
- Consumes: nothing.
- Produces: `ENTRIES: list[str]`, `MANAGES: list[str]`, `EXITS: list[str]`, `NOISE: list[str]` — synthetic sampled @caller1 messages (`⏎` replaced with real newlines).

- [ ] **Step 1: Write `tests/fixtures/messages.py`**

```python
# Synthetic message shapes; no real messages are reproduced.
ENTRIES = [
    "SOL Long\n\nTP: im taking 108, safer would be 105\n\nSL: wide, im not setting one",
    "ARB LONG\n\nTP: 0.21\nsafer TP: 0.198\n\nSL: didn't place any",
    "AVAX LONG\n\nTP: 9.15\nSL: put it where you're comfortable",
    "GOLD SHORT\n\nTP: 4144\nSL: 4169",
    "BTC SHORT\n\ncalm down on leverage, can get choppy",
    "DOGE Short (Down)\nsmall lev",
    "Going long UNI here",
    "SOL Short",
]
MANAGES = [
    "going good, book 50% profit here\nalso move SL to 82.35",
    "book 50% here\nmove sl to entry",
    "moving good, book 50% here",
    "moved my SL to +6%",
]
EXITS = [
    "hit full tp, gg",
    "closed the rest",
    "out of the LTC short. looks strong now",
    "Closing this.",
    "conservative tp reached",
]
NOISE = [
    "gm gm",
    "anyone holding any trade?",
    "SOL long?",
    "Can enter now ?",
    "only look for short entries here with tight sl",
    "📊 TRADE REPORT\n1 GOLD LONG LOSS\n2 BTC SHORT WIN",
    "**JUST IN:** US revokes license that allowed Iran to sell oil.",
]
```

- [ ] **Step 2: Verify import works**

Run: `uv run python -c "from tests.fixtures.messages import ENTRIES; print(len(ENTRIES))"`
Expected: prints `8`.

- [ ] **Step 3: Commit**

```bash
git add tests/fixtures/messages.py
git commit -m "test: add sampled @caller1 message fixtures"
```

---

### Task 6: Regex fast-path parser

**Files:**
- Create: `src/botta/parser.py`, `tests/test_parser.py`

**Interfaces:**
- Consumes: `models.normalize_asset`, `models.EntryIntent`.
- Produces:
  - `regex_parse(text: str, msg_id: int) -> EntryIntent | None` — matches a leading `<ASSET> LONG|SHORT` (case-insensitive) optionally followed by `TP:`/`SL:` lines; returns None if no clean match. Confidence 0.95 on a clean structured match.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_parser.py
from botta.parser import regex_parse

def test_regex_entry_with_tp_sl():
    e = regex_parse("GOLD SHORT\n\nTP: 4144\nSL: 4169", 1)
    assert e is not None
    assert e.asset == "GOLD" and e.side == "short"
    assert 4144 in e.tps
    assert e.sl == 4169.0

def test_regex_two_tps():
    e = regex_parse("HYPE LONG\nTP: 70\nconservative TP: 68.2", 2)
    assert e.asset == "HYPE" and e.side == "long"
    assert sorted(e.tps) == [68.2, 70.0]

def test_regex_bare_entry():
    e = regex_parse("SOL Short", 3)
    assert e.asset == "SOL" and e.side == "short" and e.tps == []

def test_regex_rejects_question():
    assert regex_parse("SOL long?", 4) is None

def test_regex_rejects_noise():
    assert regex_parse("only look for short entries here with tight sl", 5) is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_parser.py -q`
Expected: FAIL (ModuleNotFoundError: botta.parser).

- [ ] **Step 3: Write `src/botta/parser.py`**

```python
import re
from typing import Optional

from botta.models import EntryIntent, normalize_asset

_ENTRY_RE = re.compile(r"^\s*\$?([A-Za-z]{2,10})\s+(LONG|SHORT)\b", re.IGNORECASE)
_TP_RE = re.compile(r"TP[:\s]+.*?(\d+(?:\.\d+)?)", re.IGNORECASE)
_SL_RE = re.compile(r"\bSL[:\s]+.*?(\d+(?:\.\d+)?)", re.IGNORECASE)


def regex_parse(text: str, msg_id: int) -> Optional[EntryIntent]:
    first = text.strip().splitlines()[0] if text.strip() else ""
    if "?" in first:                       # questions are not calls
        return None
    m = _ENTRY_RE.match(first)
    if not m:
        return None
    asset = normalize_asset(m.group(1))
    if asset is None:
        return None
    side = m.group(2).lower()
    tps = [float(x) for x in _TP_RE.findall(text)]
    sl_matches = _SL_RE.findall(text)
    sl = float(sl_matches[0]) if sl_matches else None
    return EntryIntent(
        asset=asset, side=side, tps=sorted(set(tps)), sl=sl,
        confidence=0.95, source_msg_id=msg_id,
    )
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_parser.py -q`
Expected: PASS (5 passed).

- [ ] **Step 5: Commit**

```bash
git add src/botta/parser.py tests/test_parser.py
git commit -m "feat: regex fast-path entry parser"
```

---

### Task 7: LLM gateway client

**Files:**
- Create: `src/botta/llm.py`, `tests/test_llm.py`

**Interfaces:**
- Consumes: `.env` / env vars for `LIGHTNING_API_KEY`.
- Produces:
  - `LLMClient(api_key:str, model:str="openai/gpt-5-nano", billing:str="esoteric-j7sid/inference-optimization-project")` with `classify(text:str, context:dict) -> dict` returning a raw intent dict (`{"kind": "...", ...}`). Uses the Lightning OpenAI-compatible endpoint.
  - `_build_messages(text, context) -> list[dict]` (pure, testable).

> **VERIFY AT BUILD:** confirm the Lightning gateway base URL, auth header format
> (`Bearer <key>/<teamspace>`), and that `gpt-5-*` requires `max_completion_tokens`
> (not `max_tokens`) against current docs before relying on live calls. Unit tests here
> mock the HTTP call and do not hit the network.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_llm.py
from botta.llm import LLMClient

def test_build_messages_includes_context_and_schema():
    c = LLMClient(api_key="k")
    msgs = c._build_messages("book 50% here", {"open_positions": ["SOL long"], "recent": []})
    assert msgs[0]["role"] == "system"
    assert "entry" in msgs[0]["content"] and "manage" in msgs[0]["content"]
    assert "SOL long" in msgs[1]["content"]
    assert "book 50% here" in msgs[1]["content"]
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_llm.py -q`
Expected: FAIL (ModuleNotFoundError: botta.llm).

- [ ] **Step 3: Write `src/botta/llm.py`**

```python
import json

import httpx

_BASE = "https://lightning.ai/api/v1"

_SYSTEM = """You classify one Telegram trading message from a perp caller into a JSON intent.
Output ONLY compact JSON, one of these shapes:
{"kind":"entry","asset":"SOL","side":"long|short","entry":"market|<price>","tps":[<price>...],"sl":<price|null>,"confidence":0-1}
{"kind":"manage","op":"book_partial|move_sl|trail_sl","pct":<num>,"sl_target":"<price>|entry|+Npct|null","position_ref":"<ASSET>|current|null","confidence":0-1}
{"kind":"exit","position_ref":"<ASSET>|current|null","confidence":0-1}
{"kind":"noop","confidence":1}
Assets universe: SOL BTC HYPE GOLD ETH. Messages are Hinglish. Questions ("SOL long?"),
market commentary, news, and trade recaps are "noop". "book 50% here" -> manage/book_partial
referencing the currently-open position. Use provided open_positions to resolve references."""


class LLMClient:
    def __init__(self, api_key: str, model: str = "openai/gpt-5-nano",
                 billing: str = "esoteric-j7sid/inference-optimization-project"):
        self.model = model
        self.headers = {"Authorization": f"Bearer {api_key}/{billing}"}

    def _build_messages(self, text: str, context: dict) -> list[dict]:
        ctx = f"open_positions={context.get('open_positions', [])}\nrecent={context.get('recent', [])}"
        return [
            {"role": "system", "content": _SYSTEM},
            {"role": "user", "content": f"{ctx}\n\nMESSAGE:\n{text}"},
        ]

    def classify(self, text: str, context: dict) -> dict:
        payload = {
            "model": self.model,
            "messages": self._build_messages(text, context),
            "max_completion_tokens": 200,
        }
        r = httpx.post(f"{_BASE}/chat/completions", json=payload, headers=self.headers, timeout=20)
        r.raise_for_status()
        content = r.json()["choices"][0]["message"]["content"].strip()
        return json.loads(content)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_llm.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/llm.py tests/test_llm.py
git commit -m "feat: Lightning-gateway LLM client (classify)"
```

---

### Task 8: Parser compose (regex → LLM fallback) + confidence gate

**Files:**
- Modify: `src/botta/parser.py`
- Modify: `tests/test_parser.py`

**Interfaces:**
- Consumes: `regex_parse`, `LLMClient.classify`, all intent models.
- Produces:
  - `dict_to_intent(d: dict, msg_id: int) -> Intent` — maps a raw LLM dict to a validated model; unknown/invalid → `NoOp`.
  - `parse(text:str, msg_id:int, context:dict, llm, threshold:float) -> Intent` — regex first (structured entry); else `llm.classify`; below-threshold confidence → `NoOp`.

- [ ] **Step 1: Write the failing test**

```python
# append to tests/test_parser.py
from botta.parser import parse, dict_to_intent
from botta.models import EntryIntent, ManageIntent, NoOp

class FakeLLM:
    def __init__(self, d): self.d = d
    def classify(self, text, context): return self.d

def test_dict_to_intent_manage():
    i = dict_to_intent({"kind":"manage","op":"book_partial","pct":50,"position_ref":"current","confidence":0.9}, 7)
    assert isinstance(i, ManageIntent) and i.pct == 50

def test_parse_uses_regex_for_structured_entry():
    i = parse("GOLD SHORT\nTP: 4144\nSL: 4169", 1, {}, FakeLLM({"kind":"noop","confidence":1}), 0.6)
    assert isinstance(i, EntryIntent) and i.asset == "GOLD"

def test_parse_falls_back_to_llm():
    i = parse("book 50% here", 2, {}, FakeLLM({"kind":"manage","op":"book_partial","pct":50,"position_ref":"current","confidence":0.9}), 0.6)
    assert isinstance(i, ManageIntent)

def test_parse_confidence_gate():
    i = parse("maybe long sol idk", 3, {}, FakeLLM({"kind":"entry","asset":"SOL","side":"long","confidence":0.3}), 0.6)
    assert isinstance(i, NoOp)
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_parser.py -q`
Expected: FAIL (ImportError: parse/dict_to_intent).

- [ ] **Step 3: Extend `src/botta/parser.py`**

```python
# add imports at top
from botta.models import EntryIntent, ExitIntent, Intent, ManageIntent, NoOp, normalize_asset

# append:
def dict_to_intent(d: dict, msg_id: int) -> Intent:
    kind = d.get("kind")
    conf = float(d.get("confidence", 0.0))
    try:
        if kind == "entry":
            asset = normalize_asset(str(d.get("asset", "")))
            if asset is None:
                return NoOp(source_msg_id=msg_id)
            return EntryIntent(asset=asset, side=d["side"], entry=d.get("entry", "market"),
                               tps=[float(x) for x in d.get("tps", [])],
                               sl=d.get("sl"), confidence=conf, source_msg_id=msg_id)
        if kind == "manage":
            return ManageIntent(op=d["op"], pct=float(d.get("pct", 50)),
                                sl_target=d.get("sl_target"), position_ref=d.get("position_ref"),
                                confidence=conf, source_msg_id=msg_id)
        if kind == "exit":
            return ExitIntent(position_ref=d.get("position_ref"), confidence=conf, source_msg_id=msg_id)
    except (KeyError, ValueError, TypeError):
        return NoOp(source_msg_id=msg_id)
    return NoOp(source_msg_id=msg_id)


def parse(text: str, msg_id: int, context: dict, llm, threshold: float) -> Intent:
    regex_hit = regex_parse(text, msg_id)
    if regex_hit is not None:
        return regex_hit
    intent = dict_to_intent(llm.classify(text, context), msg_id)
    if intent.kind != "noop" and intent.confidence < threshold:
        return NoOp(source_msg_id=msg_id)
    return intent
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_parser.py -q`
Expected: PASS (9 passed).

- [ ] **Step 5: Commit**

```bash
git add src/botta/parser.py tests/test_parser.py
git commit -m "feat: compose parser (regex + LLM + confidence gate)"
```

---

### Task 9: Risk manager

**Files:**
- Create: `src/botta/risk.py`, `tests/test_risk.py`

**Interfaces:**
- Consumes: `config.RiskCfg`, `config.VenuesCfg`, `state.State`, intent models.
- Produces:
  - `Decision(ok:bool, reason:str)` dataclass.
  - `Sizing(margin:float, leverage:float, notional:float)` dataclass.
  - `RiskManager(risk:RiskCfg, venues:VenuesCfg, state:State)` with:
    - `venue_for(asset:str) -> str`
    - `sizing() -> Sizing`
    - `min_notional_ok(venue:str) -> bool` (Hyperliquid min $10; drift/jupiter min $1; treat unknown as $10)
    - `check_entry(intent:EntryIntent, day:str) -> Decision` (dedupe, concurrent cap, daily cap, min-notional)
    - `is_stale(msg_epoch:float, now:float) -> bool`

- [ ] **Step 1: Write the failing test**

```python
# tests/test_risk.py
from botta.config import RiskCfg, VenuesCfg
from botta.models import EntryIntent
from botta.risk import RiskManager
from botta.state import State

def _rm(tmp_path, **over):
    r = RiskCfg(5.0, 3.0, 1.5, 3, 15, 0.5, 15.0, 120, 0.6)
    for k, v in over.items(): setattr(r, k, v)
    v = VenuesCfg("hyperliquid", {"SOL": "hyperliquid"})
    return RiskManager(r, v, State(str(tmp_path / "b.db")))

def test_sizing_and_min_notional(tmp_path):
    rm = _rm(tmp_path)
    s = rm.sizing()
    assert s.margin == 5.0 and s.notional == 15.0
    assert rm.min_notional_ok("hyperliquid") is True

def test_min_notional_rejects_below(tmp_path):
    rm = _rm(tmp_path, margin_usdc=2.0, leverage=1.0)   # $2 notional < $10
    assert rm.min_notional_ok("hyperliquid") is False

def test_dedupe_same_asset_side(tmp_path):
    rm = _rm(tmp_path)
    rm.state.add_position("hyperliquid", "SOL", "long", 81.0, 15.0, 3.0, None)
    e = EntryIntent(asset="SOL", side="long", confidence=0.9, source_msg_id=1)
    assert rm.check_entry(e, "2026-07-08").ok is False

def test_stale_guard(tmp_path):
    rm = _rm(tmp_path)
    assert rm.is_stale(1000.0, 2000.0) is True
    assert rm.is_stale(1950.0, 2000.0) is False
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_risk.py -q`
Expected: FAIL (ModuleNotFoundError: botta.risk).

- [ ] **Step 3: Write `src/botta/risk.py`**

```python
from dataclasses import dataclass

from botta.config import RiskCfg, VenuesCfg
from botta.models import EntryIntent
from botta.state import State

_MIN_NOTIONAL = {"hyperliquid": 10.0, "drift": 1.0, "jupiter": 1.0}


@dataclass
class Decision:
    ok: bool
    reason: str = ""


@dataclass
class Sizing:
    margin: float
    leverage: float
    notional: float


class RiskManager:
    def __init__(self, risk: RiskCfg, venues: VenuesCfg, state: State):
        self.risk = risk
        self.venues = venues
        self.state = state

    def venue_for(self, asset: str) -> str:
        return self.venues.asset_map.get(asset, self.venues.default_perp)

    def sizing(self) -> Sizing:
        return Sizing(self.risk.margin_usdc, self.risk.leverage,
                      self.risk.margin_usdc * self.risk.leverage)

    def min_notional_ok(self, venue: str) -> bool:
        return self.sizing().notional >= _MIN_NOTIONAL.get(venue, 10.0)

    def is_stale(self, msg_epoch: float, now: float) -> bool:
        return (now - msg_epoch) > self.risk.stale_secs

    def check_entry(self, intent: EntryIntent, day: str) -> Decision:
        existing = self.state.open_position_for(intent.asset)
        if existing and existing.side == intent.side:
            return Decision(False, "duplicate: asset+side already open")
        if len(self.state.open_positions()) >= self.risk.max_concurrent:
            return Decision(False, "max concurrent positions reached")
        if self.state.entries_today(day) >= self.risk.daily_cap:
            return Decision(False, "daily entry cap reached")
        if not self.min_notional_ok(self.venue_for(intent.asset)):
            return Decision(False, "notional below venue minimum")
        return Decision(True)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_risk.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/risk.py tests/test_risk.py
git commit -m "feat: risk manager (sizing, min-notional, dedupe, caps, stale)"
```

---

### Task 10: Position reference resolver

**Files:**
- Create: `src/botta/positions.py`, `tests/test_positions.py`

**Interfaces:**
- Consumes: `state.Position`.
- Produces:
  - `AMBIGUOUS` sentinel (module constant).
  - `resolve_ref(ref: str | None, open_positions: list[Position]) -> Position | None | object` — returns the matching Position, `None` if no positions, or `AMBIGUOUS` when the reference can't be pinned to exactly one.

Resolution rules: explicit asset ref → position for that asset (or None). `None`/`"current"` → the single open position if exactly one; if multiple open → `AMBIGUOUS`.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_positions.py
from botta.positions import resolve_ref, AMBIGUOUS
from botta.state import Position

def _p(asset): return Position(1, "hyperliquid", asset, "long", 80.0, 15.0, 3.0, None, "open", 0.0)

def test_current_single():
    assert resolve_ref("current", [_p("SOL")]).asset == "SOL"

def test_current_multiple_is_ambiguous():
    assert resolve_ref(None, [_p("SOL"), _p("BTC")]) is AMBIGUOUS

def test_explicit_asset():
    assert resolve_ref("BTC", [_p("SOL"), _p("BTC")]).asset == "BTC"

def test_none_when_empty():
    assert resolve_ref("current", []) is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_positions.py -q`
Expected: FAIL (ModuleNotFoundError: botta.positions).

- [ ] **Step 3: Write `src/botta/positions.py`**

```python
from typing import Optional

from botta.models import normalize_asset
from botta.state import Position

AMBIGUOUS = object()


def resolve_ref(ref: Optional[str], open_positions: list[Position]):
    if not open_positions:
        return None
    if ref and ref.lower() != "current":
        asset = normalize_asset(ref)
        for p in open_positions:
            if p.asset == asset:
                return p
        return None
    if len(open_positions) == 1:
        return open_positions[0]
    return AMBIGUOUS
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_positions.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/positions.py tests/test_positions.py
git commit -m "feat: position reference resolver with ambiguity"
```

---

### Task 11: Venue router + DryRunAdapter

**Files:**
- Create: `src/botta/router.py`, `tests/test_router.py`

**Interfaces:**
- Consumes: `state.Position`, `risk.Sizing`.
- Produces:
  - `Adapter` Protocol: `open(asset, side, sizing, tps, sl) -> dict`, `partial_close(pos, pct) -> dict`, `move_sl(pos, price) -> dict`, `close(pos) -> dict`, `get_position(asset) -> dict | None`.
  - `DryRunAdapter(recorder:list)` implementing the protocol by appending action dicts to `recorder` and returning them (no network).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_router.py
from botta.risk import Sizing
from botta.router import DryRunAdapter
from botta.state import Position

def test_dryrun_records_actions():
    rec = []
    a = DryRunAdapter(rec)
    a.open("SOL", "long", Sizing(5.0, 3.0, 15.0), [83.4], 79.9)
    p = Position(1, "hyperliquid", "SOL", "long", 81.2, 15.0, 3.0, 79.9, "open", 0.0)
    a.partial_close(p, 50)
    a.move_sl(p, 81.2)
    a.close(p)
    assert [r["action"] for r in rec] == ["open", "partial_close", "move_sl", "close"]
    assert rec[0]["asset"] == "SOL" and rec[0]["notional"] == 15.0
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_router.py -q`
Expected: FAIL (ModuleNotFoundError: botta.router).

- [ ] **Step 3: Write `src/botta/router.py`**

```python
from typing import Optional, Protocol

from botta.risk import Sizing
from botta.state import Position


class Adapter(Protocol):
    def open(self, asset: str, side: str, sizing: Sizing, tps: list[float], sl: Optional[float]) -> dict: ...
    def partial_close(self, pos: Position, pct: float) -> dict: ...
    def move_sl(self, pos: Position, price: float) -> dict: ...
    def close(self, pos: Position) -> dict: ...
    def get_position(self, asset: str) -> Optional[dict]: ...


class DryRunAdapter:
    """Records intended actions instead of executing (Plan 1)."""

    def __init__(self, recorder: list):
        self.recorder = recorder

    def _rec(self, d: dict) -> dict:
        self.recorder.append(d)
        return d

    def open(self, asset, side, sizing: Sizing, tps, sl) -> dict:
        return self._rec({"action": "open", "asset": asset, "side": side,
                          "margin": sizing.margin, "leverage": sizing.leverage,
                          "notional": sizing.notional, "tps": tps, "sl": sl})

    def partial_close(self, pos: Position, pct: float) -> dict:
        return self._rec({"action": "partial_close", "asset": pos.asset, "pct": pct})

    def move_sl(self, pos: Position, price: float) -> dict:
        return self._rec({"action": "move_sl", "asset": pos.asset, "price": price})

    def close(self, pos: Position) -> dict:
        return self._rec({"action": "close", "asset": pos.asset})

    def get_position(self, asset: str):
        return None
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_router.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/router.py tests/test_router.py
git commit -m "feat: venue adapter protocol + dry-run adapter"
```

---

### Task 12: Notifier + control-command parsing

**Files:**
- Create: `src/botta/notifier.py`, `tests/test_notifier.py`

**Interfaces:**
- Consumes: nothing (pure formatting).
- Produces:
  - `format_action(action: dict, venue: str, mode: str) -> str` — human line, `[DRY]` prefix when `mode == "dry"`.
  - `parse_command(text: str) -> str | None` — returns one of `status|pause|resume|flat|mode_dry|mode_live` or None.

- [ ] **Step 1: Write the failing test**

```python
# tests/test_notifier.py
from botta.notifier import format_action, parse_command

def test_format_open_dry():
    line = format_action(
        {"action": "open", "asset": "SOL", "side": "long", "margin": 5.0,
         "leverage": 3.0, "notional": 15.0, "tps": [83.4], "sl": 79.9},
        "hyperliquid", "dry")
    assert line.startswith("[DRY]")
    assert "SOL" in line and "long" in line and "hyperliquid" in line

def test_parse_command():
    assert parse_command("/status") == "status"
    assert parse_command("/flat") == "flat"
    assert parse_command("/mode live") == "mode_live"
    assert parse_command("gm") is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_notifier.py -q`
Expected: FAIL (ModuleNotFoundError: botta.notifier).

- [ ] **Step 3: Write `src/botta/notifier.py`**

```python
from typing import Optional


def format_action(action: dict, venue: str, mode: str) -> str:
    prefix = "[DRY] " if mode == "dry" else ""
    a = action["action"]
    if a == "open":
        return (f"{prefix}Opened {action['asset']} {action['side'].upper()} "
                f"{action['leverage']:g}x · {action['margin']:g} USDC "
                f"· TP {action['tps']} · SL {action['sl']} · [{venue}]")
    if a == "partial_close":
        return f"{prefix}Booked {action['pct']:g}% {action['asset']} · [{venue}]"
    if a == "move_sl":
        return f"{prefix}Moved SL {action['asset']} → {action['price']} · [{venue}]"
    if a == "close":
        return f"{prefix}Closed {action['asset']} · [{venue}]"
    return f"{prefix}{a} {action.get('asset', '')}"


def parse_command(text: str) -> Optional[str]:
    t = text.strip().lower()
    table = {"/status": "status", "/pause": "pause", "/resume": "resume", "/flat": "flat",
             "/mode dry": "mode_dry", "/mode live": "mode_live"}
    return table.get(t)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_notifier.py -q`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/botta/notifier.py tests/test_notifier.py
git commit -m "feat: notification formatting + control commands"
```

---

### Task 13: Pipeline wiring — `handle_message`

**Files:**
- Create: `src/botta/listener.py`, `tests/test_pipeline.py`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `Deps` dataclass bundling `config, state, risk, router, llm` plus a `recorder:list` and a `notify(str)` callable.
  - `Msg` protocol-ish shape used for testing: object with `.id:int`, `.text:str`, `.sender_username:str|None`, `.epoch:float`.
  - `handle_message(msg, deps, now:float) -> str | None` — the full pure pipeline: allowlist → idempotency → stale guard → parse → route by intent kind (entry/manage/exit) → risk → dry-run action → persist → return the notification string (or None if ignored).

- [ ] **Step 1: Write the failing test**

```python
# tests/test_pipeline.py
from dataclasses import dataclass
from datetime import date

from botta.config import RiskCfg, VenuesCfg, TelegramCfg, Config
from botta.risk import RiskManager
from botta.state import State
from botta.router import DryRunAdapter
from botta.listener import handle_message, Deps

@dataclass
class Msg:
    id: int
    text: str
    sender_username: str
    epoch: float

class FakeLLM:
    def classify(self, text, context): return {"kind": "noop", "confidence": 1}

def _deps(tmp_path, mode="dry"):
    cfg = Config(mode=mode,
                 telegram=TelegramCfg(-100, ["caller1"], 42),
                 risk=RiskCfg(5.0, 3.0, 1.5, 3, 15, 0.5, 15.0, 120, 0.6),
                 venues=VenuesCfg("hyperliquid", {"SOL": "hyperliquid"}),
                 tg_api_id=1, tg_api_hash="x")
    state = State(str(tmp_path / "b.db"))
    rec, notes = [], []
    return Deps(config=cfg, state=state, risk=RiskManager(cfg.risk, cfg.venues, state),
                router=DryRunAdapter(rec), llm=FakeLLM(), recorder=rec,
                notify=lambda s: notes.append(s)), rec, notes

def test_entry_from_allowlisted_records_open(tmp_path):
    deps, rec, notes = _deps(tmp_path)
    m = Msg(1, "SOL LONG\nTP: 83.4\nSL: 79.9", "caller1", 1000.0)
    out = handle_message(m, deps, now=1000.0)
    assert rec and rec[0]["action"] == "open"
    assert deps.state.open_position_for("SOL") is not None
    assert out.startswith("[DRY]")

def test_ignores_non_allowlisted(tmp_path):
    deps, rec, notes = _deps(tmp_path)
    m = Msg(2, "SOL LONG", "randomguy", 1000.0)
    assert handle_message(m, deps, now=1000.0) is None
    assert rec == []

def test_idempotent(tmp_path):
    deps, rec, notes = _deps(tmp_path)
    m = Msg(3, "SOL LONG", "caller1", 1000.0)
    handle_message(m, deps, now=1000.0)
    handle_message(m, deps, now=1000.0)
    assert len([r for r in rec if r["action"] == "open"]) == 1

def test_stale_ignored(tmp_path):
    deps, rec, notes = _deps(tmp_path)
    m = Msg(4, "SOL LONG", "caller1", 1000.0)
    assert handle_message(m, deps, now=5000.0) is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `uv run pytest tests/test_pipeline.py -q`
Expected: FAIL (ModuleNotFoundError: botta.listener).

- [ ] **Step 3: Write `src/botta/listener.py`**

```python
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Callable, Optional

from botta.config import Config
from botta.models import EntryIntent, ExitIntent, ManageIntent
from botta.parser import parse
from botta.positions import AMBIGUOUS, resolve_ref
from botta.risk import RiskManager
from botta.router import Adapter
from botta.state import State
from botta.notifier import format_action


@dataclass
class Deps:
    config: Config
    state: State
    risk: RiskManager
    router: Adapter
    llm: object
    recorder: list
    notify: Callable[[str], None]


def _emit(deps: Deps, action: dict, venue: str) -> str:
    line = format_action(action, venue, deps.config.mode)
    deps.notify(line)
    return line


def handle_message(msg, deps: Deps, now: float) -> Optional[str]:
    if msg.sender_username not in deps.config.telegram.allowlist:
        return None
    if deps.state.is_processed(msg.id):
        return None
    if deps.risk.is_stale(msg.epoch, now):
        deps.state.mark_processed(msg.id, "{}", acted=False)
        return None

    open_pos = deps.state.open_positions()
    ctx = {"open_positions": [f"{p.asset} {p.side}" for p in open_pos], "recent": []}
    intent = parse(msg.text, msg.id, ctx, deps.llm, deps.risk.risk.confidence_threshold)

    result: Optional[str] = None
    acted = False
    day = datetime.fromtimestamp(now, tz=timezone.utc).strftime("%Y-%m-%d")

    if isinstance(intent, EntryIntent):
        decision = deps.risk.check_entry(intent, day)
        if decision.ok:
            venue = deps.risk.venue_for(intent.asset)
            sizing = deps.risk.sizing()
            sl_price = intent.sl
            if sl_price is None:
                base = intent.entry if isinstance(intent.entry, (int, float)) else 0.0
                sl_price = None  # concrete entry price unknown in dry-run; SL applied at execution (Plan 2)
            action = deps.router.open(intent.asset, intent.side, sizing, intent.tps, sl_price)
            deps.state.add_position(venue, intent.asset, intent.side, 0.0, sizing.notional,
                                    sizing.leverage, sl_price)
            result = _emit(deps, action, venue)
            acted = True
        else:
            deps.notify(f"skip entry {intent.asset}: {decision.reason}")

    elif isinstance(intent, ManageIntent):
        target = resolve_ref(intent.position_ref, open_pos)
        if target is AMBIGUOUS:
            deps.notify(f"⚠️ ambiguous manage ({intent.op}) — confirm which position")
        elif target is not None:
            venue = target.venue
            if intent.op == "book_partial":
                action = deps.router.partial_close(target, intent.pct)
                deps.state.update_size(target.id, target.size * (1 - intent.pct / 100.0))
            else:  # move_sl / trail_sl — price resolution happens at execution (Plan 2)
                action = deps.router.move_sl(target, target.sl_price or 0.0)
            result = _emit(deps, action, venue)
            acted = True

    elif isinstance(intent, ExitIntent):
        target = resolve_ref(intent.position_ref, open_pos)
        if target is AMBIGUOUS:
            deps.notify("⚠️ ambiguous exit — confirm which position")
        elif target is not None:
            action = deps.router.close(target)
            deps.state.close_position(target.id)
            result = _emit(deps, action, target.venue)
            acted = True

    deps.state.mark_processed(msg.id, intent.model_dump_json(), acted=acted)
    return result
```

- [ ] **Step 4: Run test to verify it passes**

Run: `uv run pytest tests/test_pipeline.py -q`
Expected: PASS (4 passed).

- [ ] **Step 5: Run the full suite**

Run: `uv run pytest -q`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/botta/listener.py tests/test_pipeline.py
git commit -m "feat: pipeline handle_message wiring (entry/manage/exit, dry-run)"
```

---

### Task 14: App entrypoint + live dry-run smoke

**Files:**
- Create: `src/botta/app.py`
- Modify: `pyproject.toml` (add `[project.scripts]`)

**Interfaces:**
- Consumes: `Deps`, `handle_message`, Telethon, `config.load_config`.
- Produces: `main()` — connects Telethon with the existing `botta.session`, registers a `NewMessage` handler on the group that adapts each Telethon message into the `Msg` shape and calls `handle_message`, and a `control` handler for `/commands` from `control_user`. Runs forever.

> This task has no unit test (it is live glue). It is validated by a manual dry-run
> against the real group. `handle_message` is already covered by Task 13.

- [ ] **Step 1: Write `src/botta/app.py`**

```python
import asyncio
import os
import time

from telethon import TelegramClient, events

from botta.config import load_config
from botta.listener import Deps, handle_message
from botta.llm import LLMClient
from botta.notifier import parse_command
from botta.risk import RiskManager
from botta.router import DryRunAdapter
from botta.state import State


async def main() -> None:
    cfg = load_config()
    state = State("botta.db")
    recorder: list = []
    client = TelegramClient("botta", cfg.tg_api_id, cfg.tg_api_hash)
    await client.connect()
    if not await client.is_user_authorized():
        raise SystemExit("session not authorized — run fetch_messages.py in a terminal first")

    async def notify(text: str) -> None:
        print(text)
        if cfg.telegram.control_user:
            await client.send_message(cfg.telegram.control_user, text)

    def notify_sync(text: str) -> None:
        asyncio.create_task(notify(text))

    llm = LLMClient(api_key=os.environ.get("LIGHTNING_API_KEY", ""))
    deps = Deps(config=cfg, state=state,
                risk=RiskManager(cfg.risk, cfg.venues, state),
                router=DryRunAdapter(recorder), llm=llm,
                recorder=recorder, notify=notify_sync)

    @client.on(events.NewMessage(chats=cfg.telegram.group_id))
    async def on_message(event):
        sender = await event.get_sender()
        uname = getattr(sender, "username", None)

        class M:
            id = event.message.id
            text = event.message.text or ""
            sender_username = uname
            epoch = event.message.date.timestamp()

        handle_message(M, deps, now=time.time())

    @client.on(events.NewMessage(chats=cfg.telegram.control_user, from_users=cfg.telegram.control_user))
    async def on_control(event):
        cmd = parse_command(event.message.text or "")
        if cmd == "status":
            await event.reply("\n".join(f"{p.asset} {p.side} [{p.venue}]"
                                        for p in state.open_positions()) or "flat")

    print(f"botta running · mode={cfg.mode} · watching {cfg.telegram.group_id}")
    await client.run_until_disconnected()


if __name__ == "__main__":
    asyncio.run(main())
```

- [ ] **Step 2: Add script entry to `pyproject.toml`**

```toml
[project.scripts]
botta = "botta.app:main"
```

- [ ] **Step 3: Manual dry-run smoke (live group, no orders)**

Run: `uv run python -m botta.app`
Expected: prints `botta running · mode=dry …`, then `[DRY] Opened …` lines whenever @caller1 posts an actionable call. Leave running through a couple of real calls; confirm parses + intended actions look right. Ctrl-C to stop.

- [ ] **Step 4: Commit**

```bash
git add src/botta/app.py pyproject.toml
git commit -m "feat: app entrypoint + live dry-run runner"
```

---

## Self-Review

**Spec coverage:**
- §4 Listener → Task 13/14 · Parser → Tasks 6–8 · Risk Manager → Task 9 · Position Manager → Task 10 · Venue Router → Task 11 · State → Task 4 · Notifier/Control → Task 12/14. ✅
- §5 Signal model → Task 3. §6 parser (regex+LLM+confidence+context) → Tasks 6–8; edit-handling + follow-up augmentation are **deferred to a Plan 1.1 follow-up** (noted below). §8 risk (sizing, min-notional, dedupe, caps, stale) → Task 9; **kill switch + aggregate equity** need live venue balances → **Plan 2**. §11 modes (dry default) → throughout. §7 execution adapters → **Plans 2–3**.
- Deferred within Plan 1 scope, tracked for Plan 1.1: `MessageEdited` re-parse, TP/SL follow-up augmentation, `daily.entries_count` increment on entry + `/pause /resume /flat /mode` full handlers (Task 14 implements `/status`; others are one-liners added when live-testing).

**Placeholder scan:** none — every step has runnable code and exact commands.

**Type consistency:** `Sizing`, `Position`, `Decision`, `Intent`, `Adapter.open(...)` signatures are defined once (Tasks 4/9/11/3) and consumed unchanged in Tasks 11/13. `handle_message(msg, deps, now)` matches its test harness. `format_action(action, venue, mode)` matches caller in `listener._emit`.

## Notes for Plan 2 / Plan 3

- **Plan 2 (Hyperliquid, testnet):** implement `HyperliquidAdapter(Adapter)` — agent-wallet approval, `market_open` at leverage/margin, reduce-only TP/SL triggers, `update_leverage`, isolated margin; replace `DryRunAdapter` when `mode=="live"`. Add real entry price capture (fills), concrete SL-price resolution (`entry`/`+N%`/default_sl_pct), kill switch on live equity, `daily` increments. Read the current `hyperliquid-python-sdk` docs first (spec §14).
- **Plan 3 (Solana):** `DriftAdapter` (driftpy, devnet) + `JupiterAdapter` (spot swaps). Router selects by `venues.asset_map`. Read current driftpy + Jupiter docs first.
