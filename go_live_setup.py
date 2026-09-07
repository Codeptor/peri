"""One-shot mainnet setup for peri, master-key signed (run it yourself):

  1. approve a fresh agent wallet named "peri"  (writes HL_AGENT_KEY to .env,
     never printed anywhere)
  2. move ALL spot USDC -> perp clearinghouse   (in-account, not a withdrawal)
  3. verify on-chain: agent listed, perp balance funded

The master private key is asked for at a hidden prompt, used for these two
signatures in-process, and never stored, logged, or echoed.

Run:  uv run python go_live_setup.py
"""

import getpass
import os

import eth_account
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants

from peri.config import load_config

ENV = ".env"


def set_env_key(key: str, value: str) -> None:
    lines = [ln.rstrip("\n") for ln in open(ENV)] if os.path.exists(ENV) else []
    lines = [ln for ln in lines if not ln.startswith(f"{key}=")]
    lines.append(f"{key}={value}")
    fd = os.open(ENV, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write("\n".join(lines) + "\n")
    os.chmod(ENV, 0o600)


def main():
    cfg = load_config()
    if not cfg.hl_account:
        raise SystemExit("HL_ACCOUNT_ADDRESS missing from .env")
    base = constants.MAINNET_API_URL
    info = Info(base, skip_ws=True)

    key = getpass.getpass("Trench wallet PRIVATE key (hidden, used for 2 signatures, "
                          "never stored): ").strip()
    if not key:
        raise SystemExit("no key provided")
    wallet = eth_account.Account.from_key(key)
    if wallet.address.lower() != cfg.hl_account.lower():
        raise SystemExit(f"that key is for {wallet.address}, not "
                         f"{cfg.hl_account} — wrong wallet")

    ex = Exchange(wallet, base)

    # 1) approve a fresh agent named "peri"
    result, agent_key = ex.approve_agent("peri")
    if not (isinstance(result, dict) and result.get("status") == "ok"):
        raise SystemExit(f"approve_agent failed: {result!r}")
    set_env_key("HL_AGENT_KEY", agent_key)
    agent_addr = eth_account.Account.from_key(agent_key).address
    print(f"agent approved: {agent_addr} (name 'peri') · key written to .env, not shown")

    # 2) spot -> perp: move the full spot USDC balance
    sp = info.post("/info", {"type": "spotClearinghouseState", "user": cfg.hl_account.lower()})
    usdc = next((float(b["total"]) for b in sp.get("balances", []) if b["coin"] == "USDC"), 0.0)
    if usdc > 0.5:
        amt = float(int(usdc * 100)) / 100  # floor to cents
        r = ex.usd_class_transfer(amt, to_perp=True)
        if not (isinstance(r, dict) and r.get("status") == "ok"):
            raise SystemExit(f"usd_class_transfer failed: {r!r}")
        print(f"moved ${amt:.2f} spot -> perp")
    else:
        print(f"spot USDC is ${usdc:.2f} — nothing to move")

    # 3) verify
    agents = info.post("/info", {"type": "extraAgents", "user": cfg.hl_account.lower()})
    ok_agent = any(a.get("address", "").lower() == agent_addr.lower() for a in agents)
    ch = info.post("/info", {"type": "clearinghouseState", "user": cfg.hl_account.lower()})
    perp_val = float(ch["marginSummary"]["accountValue"])
    print(f"verify: agent listed={ok_agent} · perp accountValue=${perp_val:.2f}")
    if ok_agent and perp_val > 1:
        print("\nDONE — peri can go live. Next: flip mode=\"live\" in config.toml "
              "and restart the daemon.")
    else:
        print("\nsomething didn't verify — do not flip live; investigate first")


if __name__ == "__main__":
    main()
