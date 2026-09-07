"""Backfill `positions.entry_fee` from real venue fills and correct the
realized PnL of rows recorded before entry fees were attributed.

Hyperliquid reports `closedPnl` GROSS and charges the entry fee on the OPENING
fill, so every close recorded as (closedPnl - its own fee) is overstated by the
entry side. New closes are correct; this repairs the history so the performance
digest is not a mix of two accountings.

    uv run python backfill_entry_fees.py            # report only
    uv run python backfill_entry_fees.py --yes      # write

Rows whose opening fill is outside the venue's returned fill window are left
ALONE and listed, rather than silently estimated.
"""

import argparse
import json
import sys
import time
import urllib.request

from peri.config import load_config
from peri.state import State

MATCH_WINDOW_SECS = 900


def fetch_fills(account: str, network: str) -> list[dict]:
    base = ("https://api.hyperliquid.xyz" if network == "mainnet"
            else "https://api.hyperliquid-testnet.xyz")
    req = urllib.request.Request(
        f"{base}/info", data=json.dumps({"type": "userFills", "user": account}).encode(),
        headers={"Content-Type": "application/json"})
    out = json.loads(urllib.request.urlopen(req, timeout=30).read())
    if not isinstance(out, list):
        raise SystemExit(f"unexpected userFills response: {out!r}")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--yes", action="store_true", help="write the corrections")
    ap.add_argument("--db", default="peri.db")
    args = ap.parse_args()

    cfg = load_config()
    if not cfg.hl_account:
        raise SystemExit("HL_ACCOUNT_ADDRESS is not set")
    fills = fetch_fills(cfg.hl_account, cfg.hl_network)
    opens: dict[str, list[dict]] = {}
    for f in fills:
        if str(f.get("dir", "")).startswith("Close"):
            continue
        coin = f.get("coin")
        if not isinstance(coin, str) or not coin:
            continue
        opens.setdefault(coin, []).append({
            "ts": float(f["time"]) / 1000.0,
            "fee": float(f.get("fee") or 0.0),
            "sz": float(f.get("sz") or 0.0),
            "tid": str(f.get("tid") or ""),
        })
    window_start = min((o["ts"] for v in opens.values() for o in v), default=None)
    print(f"opening fills available: {sum(len(v) for v in opens.values())} "
          f"across {len(opens)} markets, oldest "
          f"{time.strftime('%Y-%m-%d %H:%MZ', time.gmtime(window_start)) if window_start else '-'}")

    state = State(args.db)
    rows = [dict(r) for r in state.db.execute(
        "SELECT id, market, side, size, entry_px, opened_ts, closed_ts, realized_pnl,"
        " status, entry_fee, source FROM positions ORDER BY id")]

    used: set[str] = set()
    fixed, missing, already = [], [], []
    for row in rows:
        if (row["entry_fee"] or 0) > 0:
            already.append(row)
            continue
        candidates = [
            o for o in opens.get(row["market"], [])
            if o["tid"] not in used
            and abs(o["ts"] - row["opened_ts"]) <= MATCH_WINDOW_SECS
        ]
        if not candidates:
            missing.append(row)
            continue
        # an entry can fill in several clips (the manual BTC short took two);
        # peri holds one position per market at a time, so every opening fill
        # in the window belongs to this position
        candidates.sort(key=lambda o: abs(o["ts"] - row["opened_ts"]))
        claimed, size = [], 0.0
        for o in candidates:
            if claimed and size >= row["size"] * 0.999:
                break
            claimed.append(o)
            size += o["sz"]
        for o in claimed:
            used.add(o["tid"])
        fixed.append((row, sum(o["fee"] for o in claimed), len(claimed)))

    print(f"\npositions: {len(rows)} total · {len(already)} already attributed · "
          f"{len(fixed)} matched to an opening fill · {len(missing)} unmatched")
    total_fee = 0.0
    for row, fee, clips in fixed:
        total_fee += fee
        closed = row["status"] == "closed" and row["realized_pnl"] is not None
        before = row["realized_pnl"] if closed else None
        after = (before - fee) if closed else None
        print(f"  #{row['id']:>3} {row['market']:<12} {row['side']:<5} "
              f"entry_fee ${fee:.4f}"
              + (f" ({clips} clips)" if clips > 1 else "")
              + (f" · realized ${before:+.4f} -> ${after:+.4f}" if closed
                 else " · still open (fee recorded, not yet realized)"))
    if missing:
        print("\n  UNMATCHED (opening fill outside the venue's fill window — left "
              "untouched, these stay overstated):")
        for row in missing:
            est = row["entry_px"] * row["size"] * 0.00105
            print(f"  #{row['id']:>3} {row['market']:<12} opened "
                  f"{time.strftime('%m-%d %H:%MZ', time.gmtime(row['opened_ts']))} "
                  f"· overstated by roughly ${est:.4f}")
    print(f"\ntotal entry fees to attribute: ${total_fee:.4f}")

    if not args.yes:
        print("\nreport only — add --yes to write")
        return 0

    for row, fee, _clips in fixed:
        state.db.execute("UPDATE positions SET entry_fee=? WHERE id=?", (fee, row["id"]))
        if row["status"] == "closed" and row["realized_pnl"] is not None:
            state.db.execute(
                "UPDATE positions SET realized_pnl=realized_pnl-? WHERE id=?",
                (fee, row["id"]))
    state.db.commit()
    print(f"\nwrote {len(fixed)} corrections · ledger realized total is now "
          f"${state.realized_total():+.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
