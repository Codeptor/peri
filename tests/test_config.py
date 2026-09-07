import pytest

from peri.config import load_config

TOML = """
mode = "dry"
hl_network = "testnet"
route_builder_fee = true

[telegram]
group_id = -100123
callers = ["h3rkk"]
control_user = 0

[analyst]
cycle_secs = 900
timeout_secs = 60
retries = 2
temperature = 0.2
max_output_tokens = 4000
conviction_min = 0.75

[universe]
native_allow = ["BTC", "SOL"]
dexes = ["xyz", "io"]
dex_volume_floor = 2000000.0
top_movers = 12

[risk]
risk_pct = 1.5
max_leverage = 10.0
max_concurrent = 3
daily_entry_cap = 6
kill_switch_pct = 15.0
min_rr = 2.0
stale_call_secs = 900
cooldown_secs = 3600
stop_cooldown_secs = 14400
min_notional = 10.0
slippage_pct = 5.0
paper_bankroll = 1000.0

[news]
rss = ["https://example.com/rss"]
max_headlines = 12

[notify]
chat_id = 0
"""


def write(tmp_path, toml=TOML, env="ANALYST_API_KEY=k\nANALYST_BASE_URL=https://api.example.com\nANALYST_MODEL=m\n"):
    (tmp_path / "config.toml").write_text(toml)
    (tmp_path / ".env").write_text(env)
    return str(tmp_path / "config.toml"), str(tmp_path / ".env")


def test_loads_full_config(tmp_path):
    cfg_path, env_path = write(tmp_path)
    cfg = load_config(cfg_path, env_path)
    assert cfg.mode == "dry"
    assert cfg.hl_network == "testnet"
    assert cfg.telegram.callers == ["h3rkk"]
    assert cfg.analyst.conviction_min == 0.75
    assert cfg.universe.dexes == ["xyz", "io"]
    assert cfg.risk.stop_cooldown_secs == 14400
    assert cfg.analyst_api_key == "k"
    assert cfg.analyst_model == "m"


def test_env_file_wins_over_shell(tmp_path, monkeypatch):
    monkeypatch.setenv("ANALYST_MODEL", "stale-shell-model")
    cfg_path, env_path = write(tmp_path)
    cfg = load_config(cfg_path, env_path)
    assert cfg.analyst_model == "m"


def test_shell_fills_missing_env(tmp_path, monkeypatch):
    monkeypatch.setenv("TAVILY_UNUSED", "x")
    monkeypatch.setenv("HL_ACCOUNT_ADDRESS", "0xabc")
    cfg_path, env_path = write(tmp_path, env="ANALYST_API_KEY=k\n")
    cfg = load_config(cfg_path, env_path)
    assert cfg.hl_account == "0xabc"


def test_bad_mode_rejected(tmp_path):
    cfg_path, env_path = write(tmp_path, toml=TOML.replace('mode = "dry"', 'mode = "yolo"'))
    with pytest.raises(ValueError):
        load_config(cfg_path, env_path)


def test_missing_section_rejected(tmp_path):
    cfg_path, env_path = write(tmp_path, toml=TOML.replace("[risk]", "[riskx]"))
    with pytest.raises(KeyError):
        load_config(cfg_path, env_path)


def test_caller_wake_skip_tools_default_and_override(tmp_path):
    cfg_path, env_path = write(tmp_path)
    assert load_config(cfg_path, env_path).analyst.caller_wake_skip_tools is True
    cfg_path, env_path = write(tmp_path, toml=TOML.replace(
        "conviction_min = 0.75", "conviction_min = 0.75\ncaller_wake_skip_tools = false"))
    assert load_config(cfg_path, env_path).analyst.caller_wake_skip_tools is False


def test_identity_reads_from_env_not_the_shared_toml(tmp_path):
    """config.toml is committed and shared, so whose account/chat it is lives in
    .env. Env wins; the file value is only the fallback."""
    cfg_path, env_path = write(tmp_path, env=(
        "ANALYST_API_KEY=k\nPERI_MODE=live\nHL_NETWORK=mainnet\n"
        "TG_GROUP_ID=-100999\nTG_CONTROL_USER=42\nTG_NOTIFY_CHAT_ID=43\n"
        "TG_CALLERS=alice, bob\n"))
    cfg = load_config(cfg_path, env_path)
    assert cfg.mode == "live"
    assert cfg.hl_network == "mainnet"
    assert cfg.telegram.group_id == -100999
    assert cfg.telegram.control_user == 42
    assert cfg.telegram.callers == ["alice", "bob"]
    assert cfg.notify.chat_id == 43


def test_identity_falls_back_to_toml_when_env_is_absent(tmp_path):
    cfg_path, env_path = write(tmp_path)
    cfg = load_config(cfg_path, env_path)
    assert cfg.mode == "dry"
    assert cfg.telegram.group_id == -100123
    assert cfg.telegram.callers == ["h3rkk"]
    assert cfg.notify.chat_id == 0


def test_placeholder_identity_loads_without_ids(tmp_path):
    """A fresh clone gets a config with the ids stripped out. It must still load
    so `--once` in dry mode runs before any Telegram wiring exists."""
    toml = (TOML.replace("group_id = -100123", "group_id = 0")
                .replace('callers = ["h3rkk"]', "callers = []"))
    cfg_path, env_path = write(tmp_path, toml=toml)
    cfg = load_config(cfg_path, env_path)
    assert cfg.telegram.group_id == 0
    assert cfg.telegram.callers == []


def test_bad_mode_from_env_rejected(tmp_path):
    cfg_path, env_path = write(tmp_path, env="ANALYST_API_KEY=k\nPERI_MODE=yolo\n")
    with pytest.raises(ValueError):
        load_config(cfg_path, env_path)
