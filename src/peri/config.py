import os
import tomllib
from dataclasses import dataclass


@dataclass
class TelegramCfg:
    group_id: int
    callers: list[str]
    control_user: int
    # A caller who posts a chart and types nothing has still made a call.
    read_images: bool = True
    vision_model: str = ""            # blank = the analyst's own model
    max_image_mb: float = 8.0
    vision_max_per_hour: int = 30     # a flood of memes must not become a bill


@dataclass
class AnalystCfg:
    cycle_secs: int
    timeout_secs: int
    retries: int
    temperature: float
    max_output_tokens: int
    conviction_min: float
    max_tool_rounds: int = 3
    # Wall-clock ceiling for ONE decision across every retry. Without it,
    # retries * timeout_secs was the real bound: 3 x 300s = 900s of a caller
    # wake producing nothing, on 2026-08-31.
    total_deadline_secs: int = 420
    caller_wake_skip_tools: bool = True   # caller-message wakes decide from context, no searches
    # adaptive cadence: quiet tape cycles slowly, a moving tape cycles fast.
    # cycle_secs stays the fallback for both when these are left at 0.
    cycle_secs_quiet: int = 0
    cycle_secs_active: int = 0
    heat_atr_pct: float = 0.9   # median candidate ATR15m% at/above which the tape is "active"

    def quiet_secs(self) -> int:
        return self.cycle_secs_quiet or self.cycle_secs

    def active_secs(self) -> int:
        return self.cycle_secs_active or self.cycle_secs


@dataclass
class UniverseCfg:
    native_allow: list[str]
    dexes: list[str]          # builder dexes to merge with native, e.g. ["xyz", "io"]
    dex_volume_floor: float
    top_movers: int


@dataclass
class RiskCfg:
    risk_pct: float
    max_leverage: float
    max_concurrent: int
    daily_entry_cap: int
    kill_switch_pct: float
    min_rr: float
    stale_call_secs: int
    cooldown_secs: int
    stop_cooldown_secs: int
    min_notional: float
    slippage_pct: float
    paper_bankroll: float
    tp_net_floor_usd: float = 0.0   # refuse opens whose projected net TP $ is below this (0 = off)
    # -- 2026-08-29 post-mortem rails (every one of these was a loss on 08-28) --
    day_loss_halt_pct: float = 0.0      # halt NEW entries once the day is this far down (0 = off)
    min_stop_pct: float = 0.0           # minimum stop distance, % of entry (0 = off)
    atr_stop_mult: float = 0.0          # ...and at least this many ATR15m (0 = off)
    max_range_pos_long: float = 1.0     # refuse longs above this 24h-range position (1 = off)
    min_range_pos_short: float = 0.0    # refuse shorts below this 24h-range position (0 = off)
    equity_open_blackout_mins: int = 0   # no builder-dex entries this close after the US open
    equity_close_blackout_mins: int = 0  # ...or before the US close (0 = off)
    equity_rth_only: bool = False        # builder-dex entries only during regular US hours
    breakeven_at_r: float = 0.0         # move the stop to entry once this many R in profit (0 = off)
    time_stop_secs: int = 0             # close a position stuck below time_stop_min_r for this long
    time_stop_min_r: float = 0.5
    # Trailing: once a position is this far in front, the stop follows the
    # high-water mark at trail_atr_mult x ATR15m behind it. 0 disables.
    trail_start_r: float = 0.0
    trail_atr_mult: float = 1.5
    entry_expiry_secs: int = 7200       # a resting entry that never fills is cancelled after this
    event_blackout_mins: int = 0        # no new entry this soon before a HIGH-impact
                                        #   calendar event (0 = off). Entering minutes
                                        #   before NFP is a coin flip, not a thesis.
    max_mark_drift_pct: float = 0.5     # refuse a MARKET entry whose mark moved more
                                        #   than this while the analyst was deciding
                                        #   (0 = off). Matches the tolerance the chat
                                        #   confirmation path has always applied.


@dataclass
class NewsCfg:
    rss: list[str]
    max_headlines: int
    tg_channels: list[str] = None  # type: ignore[assignment]


@dataclass
class WatchCfg:
    """Price-triggered wakes: the analyst sees a move as it starts, not at the
    next quarter-hour."""
    enabled: bool = False
    poll_secs: int = 30
    window_secs: int = 300
    move_pct: float = 1.0      # |move| over the window that wakes the analyst
    rewake_secs: int = 900     # per-market debounce
    breakout: bool = True      # also wake on a new 24h high/low
    breakout_pct: float = 0.15  # ...but only this far PAST it. A name grinding up
                               # re-makes its own high every poll: 61 of 64 wakes on
                               # 2026-08-30 were under 0.2% past the level (one was
                               # 0.5bp) and produced no profitable trade.


@dataclass
class NotifyCfg:
    chat_id: int


@dataclass
class Config:
    mode: str                      # "dry" | "live"
    hl_network: str                # "testnet" | "mainnet"
    route_builder_fee: bool
    telegram: TelegramCfg
    analyst: AnalystCfg
    universe: UniverseCfg
    risk: RiskCfg
    news: NewsCfg
    notify: NotifyCfg
    watch: WatchCfg = None  # type: ignore[assignment]
    # secrets (.env)
    tg_api_id: int = 0
    tg_api_hash: str = ""
    tg_bot_token: str = ""
    hl_account: str = ""
    hl_agent_key: str = ""
    analyst_api_key: str = ""
    analyst_base_url: str = ""
    analyst_model: str = ""
    # Retries swap to this model: shrinking the token budget does not reduce
    # latency on the DashScope endpoint (measured 2026-08-31), a faster model does.
    analyst_fallback_model: str = ""
    tavily_api_key: str = ""
    exa_api_key: str = ""


def _load_env(path: str = ".env") -> dict[str, str]:
    """Parse .env. File values win over inherited shell env (kestrel scar:
    a stale exported var silently overriding .env cost a debugging day)."""
    env: dict[str, str] = {}
    if os.path.exists(path):
        for line in open(path):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    return env


def load_config(path: str = "config.toml", env_path: str = ".env") -> Config:
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    env = _load_env(env_path)

    def sec(name: str) -> dict:
        if name not in raw:
            raise KeyError(f"config.toml missing [{name}] section")
        return raw[name]

    def ev(key: str, default: str = "") -> str:
        return env.get(key, os.environ.get(key, default))

    t, a, u, r, n, no = (sec("telegram"), sec("analyst"), sec("universe"),
                         sec("risk"), sec("news"), sec("notify"))
    w = raw.get("watch", {})

    def evi(key: str, fallback) -> int:
        """Identity that belongs to an operator, not to the repo. config.toml is
        committed and shared, so the chat ids and the live/dry switch read from
        .env first — a clone carries the tuning without carrying whose account
        it is, and this box keeps its own values out of every diff."""
        raw_val = ev(key, "").strip()
        return int(raw_val) if raw_val else int(fallback or 0)

    mode = ev("PERI_MODE", "").strip() or raw["mode"]
    if mode not in ("dry", "live"):
        raise ValueError(f"mode must be dry|live, got {mode!r}")
    network = ev("HL_NETWORK", "").strip() or raw["hl_network"]
    if network not in ("testnet", "mainnet"):
        raise ValueError(f"hl_network must be testnet|mainnet, got {network!r}")
    callers = [c.strip() for c in ev("TG_CALLERS", "").split(",") if c.strip()]

    return Config(
        mode=mode,
        hl_network=network,
        route_builder_fee=bool(raw.get("route_builder_fee", True)),
        telegram=TelegramCfg(evi("TG_GROUP_ID", t.get("group_id", 0)),
                             callers or list(t.get("callers", [])),
                             evi("TG_CONTROL_USER", t.get("control_user", 0)),
                             bool(t.get("read_images", True)),
                             str(t.get("vision_model", "")),
                             float(t.get("max_image_mb", 8.0)),
                             int(t.get("vision_max_per_hour", 30))),
        # keywords, not position: a field inserted into AnalystCfg silently
        # shifted every argument after it when these were positional
        analyst=AnalystCfg(
            cycle_secs=int(a["cycle_secs"]), timeout_secs=int(a["timeout_secs"]),
            retries=int(a["retries"]), temperature=float(a["temperature"]),
            max_output_tokens=int(a["max_output_tokens"]),
            conviction_min=float(a["conviction_min"]),
            max_tool_rounds=int(a.get("max_tool_rounds", 3)),
            total_deadline_secs=int(a.get("total_deadline_secs", 420)),
            caller_wake_skip_tools=bool(a.get("caller_wake_skip_tools", True)),
            cycle_secs_quiet=int(a.get("cycle_secs_quiet", 0)),
            cycle_secs_active=int(a.get("cycle_secs_active", 0)),
            heat_atr_pct=float(a.get("heat_atr_pct", 0.9))),
        universe=UniverseCfg(list(u["native_allow"]), list(u["dexes"]),
                             float(u["dex_volume_floor"]), int(u["top_movers"])),
        risk=RiskCfg(float(r["risk_pct"]), float(r["max_leverage"]), int(r["max_concurrent"]),
                     int(r["daily_entry_cap"]), float(r["kill_switch_pct"]), float(r["min_rr"]),
                     int(r["stale_call_secs"]), int(r["cooldown_secs"]),
                     int(r["stop_cooldown_secs"]), float(r["min_notional"]),
                     float(r["slippage_pct"]), float(r["paper_bankroll"]),
                     float(r.get("tp_net_floor_usd", 0.0)),
                     float(r.get("day_loss_halt_pct", 0.0)),
                     float(r.get("min_stop_pct", 0.0)),
                     float(r.get("atr_stop_mult", 0.0)),
                     float(r.get("max_range_pos_long", 1.0)),
                     float(r.get("min_range_pos_short", 0.0)),
                     int(r.get("equity_open_blackout_mins", 0)),
                     int(r.get("equity_close_blackout_mins", 0)),
                     bool(r.get("equity_rth_only", False)),
                     float(r.get("breakeven_at_r", 0.0)),
                     int(r.get("time_stop_secs", 0)),
                     float(r.get("time_stop_min_r", 0.5)),
                     float(r.get("trail_start_r", 0.0)),
                     float(r.get("trail_atr_mult", 1.5)),
                     int(r.get("entry_expiry_secs", 7200)),
                     int(r.get("event_blackout_mins", 0)),
                     float(r.get("max_mark_drift_pct", 0.5))),
        news=NewsCfg(list(n["rss"]), int(n["max_headlines"]),
                     list(n.get("tg_channels", []))),
        notify=NotifyCfg(evi("TG_NOTIFY_CHAT_ID", no.get("chat_id", 0))),
        watch=WatchCfg(bool(w.get("enabled", False)), int(w.get("poll_secs", 30)),
                       int(w.get("window_secs", 300)), float(w.get("move_pct", 1.5)),
                       int(w.get("rewake_secs", 900)), bool(w.get("breakout", True)),
                       float(w.get("breakout_pct", 0.3))),
        tg_api_id=int(ev("TG_API_ID", "0")),
        tg_api_hash=ev("TG_API_HASH"),
        tg_bot_token=ev("TG_BOT_TOKEN"),
        hl_account=ev("HL_ACCOUNT_ADDRESS"),
        hl_agent_key=ev("HL_AGENT_KEY"),
        analyst_api_key=ev("ANALYST_API_KEY"),
        analyst_base_url=ev("ANALYST_BASE_URL"),
        analyst_model=ev("ANALYST_MODEL"),
        analyst_fallback_model=ev("ANALYST_FALLBACK_MODEL", ""),
        tavily_api_key=ev("TAVILY_API_KEY"),
        exa_api_key=ev("EXA_API_KEY"),
    )
