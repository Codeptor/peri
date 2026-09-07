from peri.notifier import Notifier, fmt_close, fmt_open, fmt_refusal, stamp
from peri.risk import Approved


def test_stdout_only_when_unconfigured(capsys):
    Notifier().send("hello")
    assert "[peri] hello" in capsys.readouterr().out


def test_fmt_open_contains_the_ticket():
    ap = Approved(market="xyz:NVDA", side="long", notional=130.0, size_usd_risk=2.0,
                  leverage=20.0, margin=6.5, margin_mode="cross",
                  stop_px=214.9, tp_px=226.0)
    line = fmt_open("live", ap, 218.15)
    for needle in ("xyz:NVDA", "long", "218.15", "20x", "214.9", "226", "[live]"):
        assert needle in line


def test_fmt_close_and_refusal():
    assert "realized $+4.40" in fmt_close("xyz:NVDA", "tp", 226.0, 4.4)
    assert "conviction" in fmt_refusal("BTC", "conviction 0.6 < floor")


def test_stamp_dual_timezone():
    s = stamp(1756251600.0)  # 2026-08-26 23:40 UTC-ish
    assert "Z/" in s and s.endswith("IST")
