import pytest

from peri.hl_client import resolve_base_url
from hyperliquid.utils import constants


def test_resolve_base_url():
    assert resolve_base_url("testnet") == constants.TESTNET_API_URL
    assert resolve_base_url("mainnet") == constants.MAINNET_API_URL


def test_build_clients_requires_wallet_material(tmp_path, monkeypatch):
    from peri.config import load_config
    from peri.hl_client import build_clients
    from tests.test_config import write
    cfg_path, env_path = write(tmp_path)
    for k in ("HL_ACCOUNT_ADDRESS", "HL_AGENT_KEY"):
        monkeypatch.delenv(k, raising=False)
    cfg = load_config(cfg_path, env_path)
    with pytest.raises(RuntimeError):
        build_clients(cfg)
