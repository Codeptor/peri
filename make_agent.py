"""Generate a fresh agent keypair for the bot.

Writes the private key straight into .env as HL_AGENT_KEY (never printed anywhere);
prints ONLY the agent's public address, which you authorize in the Hyperliquid UI.
The agent must be a different keypair from your main wallet — this makes one.
"""

import os

from eth_account import Account

ENV = ".env"


def main():
    acct = Account.create()
    key = acct.key.hex()
    if not key.startswith("0x"):
        key = "0x" + key

    lines = []
    if os.path.exists(ENV):
        lines = [ln.rstrip("\n") for ln in open(ENV)]
        if any(ln.startswith("HL_AGENT_KEY=") for ln in lines):
            raise SystemExit(".env already has HL_AGENT_KEY — delete that line first if you "
                             "really want a new agent (the old one stays authorized until removed in the UI)")
    lines.append(f"HL_AGENT_KEY={key}")
    fd = os.open(ENV, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write("\n".join(lines) + "\n")
    os.chmod(ENV, 0o600)  # O_CREAT mode only applies to new files; clamp pre-existing ones too

    print("agent keypair generated · private key written to .env (HL_AGENT_KEY) · not displayed")
    print(f"\nagent ADDRESS to authorize:\n\n  {acct.address}\n")
    print("app.hyperliquid.xyz (or -testnet) → More → API → paste this address (name it e.g. 'peri') → Authorize")


if __name__ == "__main__":
    main()
