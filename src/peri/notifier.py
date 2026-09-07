"""Operator notifications: stdout always; Telegram bot when configured.
A notify failure is logged and swallowed — alerts must never kill the engine."""

from datetime import datetime, timezone, timedelta

import httpx

_IST = timezone(timedelta(hours=5, minutes=30))


def stamp(ts: float) -> str:
    utc = datetime.fromtimestamp(ts, tz=timezone.utc)
    ist = utc.astimezone(_IST)
    return f"{utc:%H:%M}Z/{ist:%H:%M}IST"


class Notifier:
    def __init__(self, bot_token: str = "", chat_id: int = 0):
        self.bot_token = bot_token
        self.chat_id = chat_id

    def send(self, text: str) -> None:
        print(f"[peri] {text}", flush=True)
        if not (self.bot_token and self.chat_id):
            return
        try:
            httpx.post(f"https://api.telegram.org/bot{self.bot_token}/sendMessage",
                       json={"chat_id": self.chat_id, "text": text}, timeout=10)
        except httpx.HTTPError as e:
            print(f"[peri] notify failed: {e!r}", flush=True)


def fmt_open(mode: str, ap, entry_px: float) -> str:
    return (f"OPEN {ap.market} {ap.side} @ {entry_px} · notional ${ap.notional:.2f} "
            f"{ap.leverage:g}x (margin ${ap.margin:.2f}, risk ${ap.size_usd_risk:.2f}) "
            f"· SL {ap.stop_px} TP {ap.tp_px} · [{mode}]")


def fmt_close(market: str, reason: str, close_px: float, pnl: float) -> str:
    return f"CLOSE {market} ({reason}) @ {close_px} · realized ${pnl:+.2f}"


def fmt_refusal(market: str, reason: str) -> str:
    return f"refused {market}: {reason}"
