# Trench Copy-Trade Bot — Plan 2: Hyperliquid Execution Adapter

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A `HyperliquidAdapter` implementing the existing `router.Adapter` protocol that places real perp orders on Hyperliquid (leverage, market open, reduce-only TP/SL triggers, partial close, SL move, full close), wired into the app so `mode="live"` uses it while `mode="dry"` keeps the `DryRunAdapter`. Validated on **testnet first**.

**Architecture:** The adapter wraps injected `Exchange` + `Info` clients (dependency injection → unit-testable with fakes, no network in unit tests). A factory builds the real clients from config/secrets. Sizing converts USD notional → coin size via mark price + szDecimals. The listener already calls `router.open/partial_close/move_sl/close`; only the app's adapter *selection* and concrete-SL resolution change.

**Tech Stack:** `hyperliquid-python-sdk>=0.24`, `eth-account`, existing stack.

## Global Constraints

- **Verified SDK surface only** (some public docs show a non-existent `place_order(order_type='trigger_stop_loss')` — do NOT use it). Real API: `Exchange(wallet, base_url, account_address=)`, `exchange.order(coin, is_buy, sz, limit_px, order_type, reduce_only=)`, `exchange.market_open(coin, is_buy, sz, slippage=)`, `exchange.market_close(coin, sz=)`, `exchange.update_leverage(lev, coin, is_cross=)`, `exchange.cancel(coin, oid)`, `info.all_mids()`, `info.meta()`, `info.user_state(addr)`, `info.open_orders(addr)`.
- **Testnet before mainnet.** Build + verify against `constants.TESTNET_API_URL`. Flip to mainnet only after a clean testnet run.
- **Agent wallet** (approve_agent / UI-generated API wallet): can trade, **cannot withdraw**. Its key lives in `.env` (`HL_AGENT_KEY`), gitignored. `account_address` is the **main** wallet's public address (`HL_ACCOUNT_ADDRESS`).
- **Isolated margin**, leverage = `int(config.risk.leverage)`.
- **Min-notional guard** (Plan 1 risk manager) already blocks sub-$10 orders; keep it.
- Trigger orders are **reduce-only**. TP/SL sizes must equal the live position size (two TPs → 50/50).
- Unit tests **never** hit the network — inject fake `Exchange`/`Info`. Real placement is verified only by the manual testnet task.

---

## Task 0: Prerequisites (user, one-time — not code)

- [ ] Create a Hyperliquid account; on **testnet** (app.hyperliquid-testnet.xyz) claim faucet USDC.
- [ ] Generate an **API/agent wallet** (More → API, or `approve_agent()`); copy the agent private key.
- [ ] Put in `.env`: `HL_ACCOUNT_ADDRESS=0x<main wallet public address>` and `HL_AGENT_KEY=0x<agent private key>`.
- [ ] Add `hl_network = "testnet"` under `[venues]` in `config.toml`.

---

## Task 1: Hyperliquid config + client factory

**Files:**
- Modify: `src/botta/config.py` (add `hl_account`, `hl_agent_key`, `hl_network` to Config/VenuesCfg), `config.toml`
- Create: `src/botta/hl_client.py`, `tests/test_hl_client.py`

**Interfaces:**
- Produces: `build_clients(cfg) -> tuple[Exchange, Info, str]` returning `(exchange, info, account_address)`; `resolve_base_url(network:str) -> str`.

- [ ] **Step 1: Write the failing test** (pure URL resolution, no network)

```python
# tests/test_hl_client.py
from botta.hl_client import resolve_base_url

def test_resolve_base_url():
    assert "testnet" in resolve_base_url("testnet").lower()
    assert resolve_base_url("mainnet").startswith("https://")
```

- [ ] **Step 2: Run → fail** — `uv run --group dev pytest tests/test_hl_client.py -q` (ModuleNotFoundError).

- [ ] **Step 3: Implement `src/botta/hl_client.py`**

```python
import eth_account
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants


def resolve_base_url(network: str) -> str:
    return constants.TESTNET_API_URL if network == "testnet" else constants.MAINNET_API_URL


def build_clients(cfg):
    base = resolve_base_url(cfg.venues.hl_network)
    wallet = eth_account.Account.from_key(cfg.hl_agent_key)
    exchange = Exchange(wallet, base, account_address=cfg.hl_account)
    info = Info(base, skip_ws=True)
    return exchange, info, cfg.hl_account
```

- [ ] **Step 4: Extend config** — add `hl_network: str` to `VenuesCfg`, `hl_account: str` + `hl_agent_key: str` to `Config` (read from `.env` like the TG creds), default `hl_network="testnet"`. Add `hl_network = "testnet"` to `config.toml [venues]`. Update `tests/test_config.py` to assert the defaults.

- [ ] **Step 5: Run → pass** (`resolve_base_url` test + config test). Add `hyperliquid-python-sdk` to `pyproject.toml` deps.

- [ ] **Step 6: Commit** — `feat: hyperliquid config + client factory`

---

## Task 2: Sizing helper (USD notional → coin size)

**Files:** Create `src/botta/hl_sizing.py`, `tests/test_hl_sizing.py`

**Interfaces:**
- `sz_decimals(meta:dict, coin:str) -> int` — reads `szDecimals` for the coin from `info.meta()`.
- `notional_to_size(notional:float, mark:float, sz_decimals:int) -> float` — `round(notional/mark, sz_decimals)`.

- [ ] **Step 1: Failing test**

```python
# tests/test_hl_sizing.py
from botta.hl_sizing import notional_to_size, sz_decimals

def test_notional_to_size():
    assert notional_to_size(15.0, 150.0, 2) == 0.1     # 15/150 = 0.1
    assert notional_to_size(15.0, 3.0, 1) == 5.0

def test_sz_decimals():
    meta = {"universe": [{"name": "SOL", "szDecimals": 2}, {"name": "BTC", "szDecimals": 5}]}
    assert sz_decimals(meta, "SOL") == 2
    assert sz_decimals(meta, "BTC") == 5
```

- [ ] **Step 2: Run → fail.**

- [ ] **Step 3: Implement**

```python
def sz_decimals(meta: dict, coin: str) -> int:
    for a in meta["universe"]:
        if a["name"] == coin:
            return int(a["szDecimals"])
    raise ValueError(f"unknown coin {coin}")


def notional_to_size(notional: float, mark: float, sz_decimals: int) -> float:
    return round(notional / mark, sz_decimals)
```

- [ ] **Step 4: Run → pass. Step 5: Commit** — `feat: hyperliquid sizing helper`

---

## Task 3: HyperliquidAdapter — open (leverage + market + TP/SL), mocked

**Files:** Create `src/botta/hl_adapter.py`, `tests/test_hl_adapter.py`

**Interfaces (implements `router.Adapter`):**
- `HyperliquidAdapter(exchange, info, account_address, default_sl_pct)`.
- `open(asset, side, sizing, tps, sl) -> dict` — set isolated leverage; `market_open`; read `avgPx`; resolve SL price (given `sl` else `default_sl_pct` from fill); place reduce-only SL + TP triggers (2 TPs → 50/50). Returns `{"action":"open","asset","side","entry_px","sl":sl_price,"tps","size","oids":[...]}`.
- Helpers: `_mark(asset)`, `_size(asset, notional)`, `_fill_px(resp)`, `_default_sl(entry_px, side)`.

> **VERIFY ON TESTNET (Task 7) before trusting.** Signing/response shapes must be confirmed against the installed SDK — unit tests below only lock the adapter's *logic* using fakes.

- [ ] **Step 1: Failing test** (fake Exchange/Info records calls)

```python
# tests/test_hl_adapter.py
from botta.hl_adapter import HyperliquidAdapter
from botta.risk import Sizing

class FakeExchange:
    def __init__(self): self.calls = []
    def update_leverage(self, lev, coin, is_cross=True): self.calls.append(("lev", coin, lev, is_cross))
    def market_open(self, coin, is_buy, sz, slippage=0.05):
        self.calls.append(("open", coin, is_buy, sz))
        return {"status": "ok", "response": {"data": {"statuses": [{"filled": {"avgPx": "150.0", "oid": 1}}]}}}
    def order(self, coin, is_buy, sz, limit_px, order_type, reduce_only=False):
        self.calls.append(("order", coin, is_buy, sz, order_type["trigger"]["tpsl"], reduce_only))
        return {"status": "ok", "response": {"data": {"statuses": [{"resting": {"oid": 99}}]}}}

class FakeInfo:
    def all_mids(self): return {"SOL": "150.0"}
    def meta(self): return {"universe": [{"name": "SOL", "szDecimals": 2}]}

def test_open_long_sets_leverage_market_and_sl():
    ex, info = FakeExchange(), FakeInfo()
    a = HyperliquidAdapter(ex, info, "0xacct", default_sl_pct=1.5)
    out = a.open("SOL", "long", Sizing(5.0, 3.0, 15.0), tps=[153.0], sl=None)
    kinds = [c[0] for c in ex.calls]
    assert kinds[0] == "lev" and ex.calls[0][3] is False          # isolated
    assert ("open", "SOL", True, 0.1) in ex.calls                  # 15/150 = 0.1
    assert out["entry_px"] == 150.0
    assert out["sl"] == round(150.0 * (1 - 0.015), 4)             # default SL 1.5% below for long
    assert any(c[0] == "order" and c[4] == "sl" and c[5] is True for c in ex.calls)
    assert any(c[0] == "order" and c[4] == "tp" for c in ex.calls)
```

- [ ] **Step 2: Run → fail.**

- [ ] **Step 3: Implement `src/botta/hl_adapter.py`**

```python
from typing import Optional

from botta.hl_sizing import notional_to_size, sz_decimals
from botta.risk import Sizing
from botta.state import Position


class HyperliquidAdapter:
    def __init__(self, exchange, info, account_address: str, default_sl_pct: float):
        self.ex = exchange
        self.info = info
        self.account = account_address
        self.default_sl_pct = default_sl_pct

    def _mark(self, coin: str) -> float:
        return float(self.info.all_mids()[coin])

    def _size(self, coin: str, notional: float) -> float:
        return notional_to_size(notional, self._mark(coin), sz_decimals(self.info.meta(), coin))

    @staticmethod
    def _fill_px(resp: dict) -> float:
        return float(resp["response"]["data"]["statuses"][0]["filled"]["avgPx"])

    def _default_sl(self, entry_px: float, side: str) -> float:
        f = 1 - self.default_sl_pct / 100 if side == "long" else 1 + self.default_sl_pct / 100
        return round(entry_px * f, 4)

    def _trigger(self, coin, is_buy, sz, px, tpsl):
        ot = {"trigger": {"triggerPx": px, "isMarket": True, "tpsl": tpsl}}
        return self.ex.order(coin, is_buy, sz, px, ot, reduce_only=True)

    def open(self, asset: str, side: str, sizing: Sizing, tps: list[float], sl: Optional[float]) -> dict:
        coin, is_long = asset, side == "long"
        self.ex.update_leverage(int(sizing.leverage), coin, is_cross=False)
        size = self._size(coin, sizing.notional)
        resp = self.ex.market_open(coin, is_buy=is_long, sz=size, slippage=0.05)
        entry_px = self._fill_px(resp)
        sl_price = sl if sl is not None else self._default_sl(entry_px, side)
        oids = []
        self._trigger(coin, is_buy=not is_long, sz=size, px=sl_price, tpsl="sl")
        # split across TPs (2 -> 50/50)
        for i, tp in enumerate(tps):
            part = round(size / len(tps), sz_decimals(self.info.meta(), coin)) if tps else size
            self._trigger(coin, is_buy=not is_long, sz=part, px=tp, tpsl="tp")
        return {"action": "open", "asset": asset, "side": side, "entry_px": entry_px,
                "sl": sl_price, "tps": tps, "size": size, "oids": oids}
```

- [ ] **Step 4: Run → pass. Step 5: Commit** — `feat: hyperliquid adapter open (leverage + market + TP/SL)`

---

## Task 4: Adapter — partial_close, move_sl, close (mocked)

**Files:** Modify `src/botta/hl_adapter.py`, `tests/test_hl_adapter.py`

**Interfaces:** add `partial_close(pos, pct)`, `move_sl(pos, price)`, `close(pos)`, `_cancel_triggers(coin)`, `get_position(asset)`.
- `move_sl`/`close` cancel existing trigger orders for the coin (via `info.open_orders(account)` filtered to the coin's trigger orders → `exchange.cancel(coin, oid)`) then act — avoids persisting oids.

- [ ] **Step 1: Failing test** (extend fakes with `open_orders`, `cancel`, `market_close`)

```python
# add to tests/test_hl_adapter.py
from botta.state import Position

def _pos(asset="SOL", side="long", size=0.1):
    return Position(1, "hyperliquid", asset, side, 150.0, size, 3.0, 147.0, "open", 0.0)

def test_partial_close_reduces_half(monkeypatch):
    ex, info = FakeExchange(), FakeInfo()
    ex.market_close = lambda coin, sz=None: ex.calls.append(("close", coin, sz))
    a = HyperliquidAdapter(ex, info, "0xacct", 1.5)
    a.partial_close(_pos(size=0.1), 50)
    assert ("close", "SOL", 0.05) in ex.calls
```

- [ ] **Step 2: Run → fail. Step 3: Implement** (append)

```python
    def _cancel_triggers(self, coin: str) -> None:
        for o in self.info.open_orders(self.account):
            if o.get("coin") == coin and o.get("isTrigger"):
                self.ex.cancel(coin, o["oid"])

    def partial_close(self, pos: Position, pct: float) -> dict:
        sz = round(pos.size * pct / 100.0, sz_decimals(self.info.meta(), pos.asset))
        self.ex.market_close(pos.asset, sz=sz)
        return {"action": "partial_close", "asset": pos.asset, "pct": pct, "size": sz}

    def move_sl(self, pos: Position, price: float) -> dict:
        self._cancel_triggers(pos.asset)
        is_buy = pos.side == "short"
        self._trigger(pos.asset, is_buy=is_buy, sz=pos.size, px=price, tpsl="sl")
        return {"action": "move_sl", "asset": pos.asset, "price": price}

    def close(self, pos: Position) -> dict:
        self._cancel_triggers(pos.asset)
        self.ex.market_close(pos.asset)
        return {"action": "close", "asset": pos.asset}

    def get_position(self, asset: str):
        st = self.info.user_state(self.account)
        for p in st.get("assetPositions", []):
            pos = p.get("position", {})
            if pos.get("coin") == asset and float(pos.get("szi", 0)) != 0:
                return pos
        return None
```

- [ ] **Step 4: Run → pass. Step 5: Commit** — `feat: hyperliquid adapter manage/close`

---

## Task 5: Live-mode wiring + concrete SL

**Files:** Modify `src/botta/app.py`, `src/botta/listener.py`

**Interfaces:** app builds `HyperliquidAdapter` when `cfg.mode=="live"`, else `DryRunAdapter`. The listener already passes `intent.sl` (may be None) to `router.open`; in live mode the adapter fills the concrete default SL from the fill price (Task 3), closing the Plan 1 deferral — no listener change needed beyond confirming `sl` passthrough.

- [ ] **Step 1: Implement adapter selection in `app.py`**

```python
from botta.hl_adapter import HyperliquidAdapter
from botta.hl_client import build_clients

def _make_router(cfg, recorder):
    if cfg.mode == "live":
        exchange, info, acct = build_clients(cfg)
        return HyperliquidAdapter(exchange, info, acct, cfg.risk.default_sl_pct)
    return DryRunAdapter(recorder)
```

Wire `deps.router = _make_router(cfg, recorder)`. Print the venue/network in the startup banner.

- [ ] **Step 2: Verify app still imports + full suite green** — `uv run python -c "import botta.app"` && `uv run --group dev pytest -q`.

- [ ] **Step 3: Commit** — `feat: select live Hyperliquid adapter by mode`

---

## Task 6: Testnet integration smoke (manual — the real verification)

> No unit test — this is where the SDK signing/response shapes are actually confirmed.

- [ ] **Step 1:** Ensure `.env` has testnet `HL_ACCOUNT_ADDRESS`/`HL_AGENT_KEY`, `config.toml` `hl_network="testnet"`, testnet USDC funded.
- [ ] **Step 2:** Write `hl_smoke.py` (repo root): build clients, `open("SOL","long", Sizing(5,3,15), tps=[<mark*1.02>], sl=None)`, print the response + `info.user_state`, then `close(pos)`. Run `uv run python hl_smoke.py`.
- [ ] **Step 3:** Confirm on the testnet UI: position opened at ~mark, isolated 3x, one SL + one TP reduce-only trigger present, then flat after close. Fix any signature/parsing mismatch against the installed SDK.
- [ ] **Step 4: Commit** — `chore: hyperliquid testnet smoke script`

---

## Task 7: End-to-end live dry→testnet run

- [ ] Set `mode="live"` (still `hl_network="testnet"`), run `uv run botta`, wait for a real @h3rkk call, confirm the position + triggers appear on testnet, `/status` reflects it. Then decide on mainnet cutover (separate, deliberate step: `hl_network="mainnet"`, real USDC, small size).

## Self-Review

- Adapter implements every `router.Adapter` method (open/partial_close/move_sl/close/get_position) → Tasks 3–4. ✅
- Concrete SL (Plan 1 deferral) resolved from fill price → Task 3 `_default_sl`. ✅
- Isolated leverage, szDecimals sizing, reduce-only triggers, testnet-first → Global Constraints + Tasks 2/3/6. ✅
- Kill switch on **live equity** (`info.user_state` margin summary) is a follow-up (Task 8, below) — Plan 1's risk manager still enforces dedupe/caps/min-notional/stale.
- Unit tests inject fakes (no network); real behavior gated to Task 6 testnet. ✅

## Task 8 (follow-up): live-equity kill switch

Wire `info.user_state(account)["marginSummary"]["accountValue"]` into a day-open snapshot; if down ≥ `kill_switch_pct`, halt new entries + alert. Small addition to the risk manager once live balances are available.
