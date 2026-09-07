"""One-time approval of trench's builder fee for this account.

approveBuilderFee is a USER-SIGNED action — it must be signed by the MASTER wallet
key, not the agent key (an agent's signature would approve the agent's own empty
account instead). Pass the master key ad hoc; it is used for this one signature and
never stored:

    HL_MASTER_KEY=0x... uv run python approve_builder.py

Idempotent: checks current approval first and no-ops if already >= 0.03%.
Network comes from config.toml (hl_network) — run once per network you trade on.
"""

import getpass
import os

import eth_account
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info

from peri.config import load_config
from peri.hl_adapter import TRENCH_BUILDER
from peri.hl_client import resolve_base_url


def main():
    cfg = load_config()
    base = resolve_base_url(cfg.hl_network)
    if not cfg.hl_account:
        raise SystemExit("set HL_ACCOUNT_ADDRESS in .env first")

    info = Info(base, skip_ws=True)
    approved = info.post("/info", {"type": "maxBuilderFee", "user": cfg.hl_account.lower(),
                                   "builder": TRENCH_BUILDER["b"]})
    print(f"network={cfg.hl_network} · account={cfg.hl_account[:8]}… · "
          f"approved={approved} tenths-bp (need ≥{TRENCH_BUILDER['f']})")
    if isinstance(approved, int) and approved >= TRENCH_BUILDER["f"]:
        print("already approved — nothing to do")
        return

    master_key = os.environ.get("HL_MASTER_KEY", "") or getpass.getpass(
        "master wallet private key (hidden, used once, never stored): ")
    if not master_key:
        raise SystemExit("no key provided")
    wallet = eth_account.Account.from_key(master_key)
    if wallet.address.lower() != cfg.hl_account.lower():
        raise SystemExit(f"HL_MASTER_KEY is for {wallet.address}, not HL_ACCOUNT_ADDRESS — wrong key")

    ex = Exchange(wallet, base)
    resp = ex.approve_builder_fee(TRENCH_BUILDER["b"], "0.03%")
    print("approve response:", resp)
    approved = info.post("/info", {"type": "maxBuilderFee", "user": cfg.hl_account.lower(),
                                   "builder": TRENCH_BUILDER["b"]})
    print(f"now approved: {approved} tenths-bp {'✓' if approved >= TRENCH_BUILDER['f'] else '— FAILED?'}")


if __name__ == "__main__":
    main()
