import eth_account
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants


def resolve_base_url(network: str) -> str:
    return constants.TESTNET_API_URL if network == "testnet" else constants.MAINNET_API_URL


def build_clients(cfg):
    """Build (exchange, info, account_address) from config + .env secrets.

    The wallet is the *agent* key (trade-not-withdraw); account_address is the
    main wallet's public address the agent trades on behalf of. On mainnet the
    Exchange is constructed dex-aware so builder-dex coins ("xyz:NVDA") resolve;
    the builder dex does not exist on testnet.
    """
    if not (cfg.hl_account and cfg.hl_agent_key):
        raise RuntimeError("live mode needs HL_ACCOUNT_ADDRESS and HL_AGENT_KEY in .env")
    base = resolve_base_url(cfg.hl_network)
    wallet = eth_account.Account.from_key(cfg.hl_agent_key)
    # NB: perp_dexs REPLACES the coin map — must include "" (native) explicitly,
    # or native coins like SOL become unmappable (KeyError in name_to_asset)
    perp_dexs = (["", *cfg.universe.dexes] if cfg.hl_network == "mainnet" else None)
    exchange = Exchange(wallet, base, account_address=cfg.hl_account, perp_dexs=perp_dexs)
    info = Info(base, skip_ws=True)
    return exchange, info, cfg.hl_account
