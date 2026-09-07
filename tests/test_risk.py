import time

import pytest

from peri.config import RiskCfg
from peri.fees import ZERO
from peri.models import OpenAction
from peri.risk import Approved, Guard, Refusal
from peri.state import State

CFG = RiskCfg(risk_pct=1.5, max_leverage=20.0, max_concurrent=3, daily_entry_cap=6,
              kill_switch_pct=15.0, min_rr=2.0, stale_call_secs=900,
              cooldown_secs=3600, stop_cooldown_secs=14400, min_notional=10.0,
              slippage_pct=5.0, paper_bankroll=1000.0)

DAY = "2026-08-27"
NO_RESERVED = frozenset()


def mk(tmp_path):
    state = State(str(tmp_path / "t.db"))
    state.open_day(DAY, 1000.0)
    return Guard(CFG, state, conviction_min=0.75), state


def act(**kw) -> OpenAction:
    base = dict(market="BTC", side="long", conviction=0.8, stop=78000.0,
                take_profit=84000.0, leverage=10, margin_mode="isolated",
                rationale="r", invalidation="i")
    base.update(kw)
    return OpenAction(**base)


MARK = 80000.0


def test_happy_path_sizing_exact(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(), equity=1000.0, mark=MARK, market_max_lev=40, day=DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)
    # risk = 1.5% of 1000 = $15; stop dist = 2000/80000 = 2.5% -> notional = 600
    assert v.size_usd_risk == pytest.approx(15.0)
    assert v.notional == pytest.approx(600.0)
    assert v.leverage == 10
    assert v.margin_mode == "isolated"
    assert v.margin == pytest.approx(60.0)


def test_gate_order_kill_first(tmp_path):
    g, s = mk(tmp_path)
    s.trip_kill(DAY)
    v = g.gate_open(act(conviction=0.5), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED)  # conviction ALSO bad
    assert isinstance(v, Refusal) and "kill" in v.reason


def test_max_concurrent(tmp_path):
    g, s = mk(tmp_path)
    for m in ("A", "B", "C"):
        s.add_position(m, "long", 1, 1, 10, 1, None, None, 0.8, "own")
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "concurrent" in v.reason


def test_user_authorized_entry_can_bypass_only_max_concurrent(tmp_path):
    g, s = mk(tmp_path)
    for market in ("A", "B", "C"):
        s.add_position(market, "long", 1, 1, 10, 10, None, None, 0.8, "own")
    allowed = g.gate_open(
        act(), 1000.0, MARK, 40, DAY,
        available_margin=1000.0, reserved_order_markets=NO_RESERVED,
        enforce_max_concurrent=False,
    )
    assert isinstance(allowed, Approved)

    for _ in range(CFG.daily_entry_cap):
        s.bump_entries(DAY)
    still_capped = g.gate_open(
        act(market="ETH"), 1000.0, MARK, 40, DAY,
        available_margin=1000.0, reserved_order_markets=NO_RESERVED,
        enforce_max_concurrent=False,
    )
    assert isinstance(still_capped, Refusal)
    assert "daily entry cap" in still_capped.reason


def test_external_positions_do_not_consume_peri_capacity(tmp_path):
    g, s = mk(tmp_path)
    external = ("SOL", "ETH", "xyz:NVDA", "xyz:MRNA", "xyz:KIOXIA")
    for market in external:
        s.add_position(market, "long", 100, 1, 100, 3, None, None, None, "external")

    v = g.gate_open(
        act(), 1000.0, MARK, 40, DAY,
        available_margin=1000.0,
        reserved_order_markets=frozenset(external),
    )

    assert isinstance(v, Approved)


def test_daily_cap(tmp_path):
    g, s = mk(tmp_path)
    for _ in range(6):
        s.bump_entries(DAY)
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "daily" in v.reason


def test_dup_market(tmp_path):
    g, s = mk(tmp_path)
    s.add_position("BTC", "short", 80000, 0.01, 800, 3, None, None, 0.8, "own")
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "already open" in v.reason


def test_pending_entry_market_blocks_duplicate(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(
        act(), 1000.0, MARK, 40, DAY,
        available_margin=1000.0,
        reserved_order_markets=frozenset({"BTC"}),
    )
    assert isinstance(v, Refusal) and "venue order already open" in v.reason


def test_pending_entries_count_toward_max_concurrent(tmp_path):
    g, s = mk(tmp_path)
    s.add_position("A", "long", 1, 1, 10, 1, None, None, 0.8, "own")
    v = g.gate_open(
        act(), 1000.0, MARK, 40, DAY,
        available_margin=1000.0,
        reserved_order_markets=frozenset({"B", "C"}),
    )
    assert isinstance(v, Refusal) and "concurrent" in v.reason


def test_cooldown(tmp_path):
    g, s = mk(tmp_path)
    s.set_cooldown("BTC", time.time() + 600, "sl")
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "cooldown" in v.reason


def test_conviction_floor(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(conviction=0.74), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "conviction" in v.reason


def test_stale_mirror_refused_fresh_ok(tmp_path):
    g, s = mk(tmp_path)
    now = time.time()
    s.add_tg_message(7, now - 2000, "caller1", "BTC long", True)
    v = g.gate_open(act(source="mirror", mirror_msg_id=7), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED, now=now)
    assert isinstance(v, Refusal) and "stale" in v.reason
    s.add_tg_message(8, now - 30, "caller1", "BTC long", True)
    v = g.gate_open(act(source="mirror", mirror_msg_id=8), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED, now=now)
    assert isinstance(v, Approved)


def test_mirror_without_msg_id_refused(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(source="mirror"), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "resolvable" in v.reason


def test_stop_wrong_side(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(stop=81000.0), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "wrong side" in v.reason


def test_rr_floor(tmp_path):
    g, _ = mk(tmp_path)
    # risk 2000, reward 3000 -> RR 1.5 < 2.0
    v = g.gate_open(act(take_profit=83000.0), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "RR" in v.reason


def test_leverage_must_be_supported_without_clamping(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(leverage=20), 1000.0, MARK, market_max_lev=15, day=DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "20x exceeds venue maximum 15x" in v.reason
    v = g.gate_open(act(leverage=10), 1000.0, MARK, market_max_lev=15, day=DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved) and v.leverage == 10


def test_min_notional_refuses_not_inflates(tmp_path):
    g, _ = mk(tmp_path)
    # equity $50 -> risk $0.75; stop dist 10% -> notional $7.5 < $10
    # (cross: a 10% stop at 10x isolated is inside the liquidation band)
    v = g.gate_open(act(stop=72000.0, take_profit=96000.0, margin_mode="cross"),
                    50.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "min" in v.reason


def test_required_margin_must_fit_authoritative_available_margin(tmp_path):
    g, _ = mk(tmp_path)
    # $600 notional at 10x needs exactly $60 authoritative available margin.
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=59.99,
                    reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal)
    assert "margin $60.00 exceeds available $59.99" in v.reason
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=60.0,
                    reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)


def test_ten_and_twenty_keep_notional_but_change_margin(tmp_path):
    g, _ = mk(tmp_path)
    ten = g.gate_open(act(leverage=10), 1000.0, MARK, 40, DAY,
                      available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    twenty = g.gate_open(act(leverage=20, margin_mode="cross"), 1000.0, MARK, 40, DAY,
                         available_margin=1000.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(ten, Approved) and isinstance(twenty, Approved)
    assert ten.notional == pytest.approx(twenty.notional)
    assert ten.margin == pytest.approx(twenty.margin * 2)
    assert twenty.margin_mode == "cross"


def test_short_side_math(tmp_path):
    g, _ = mk(tmp_path)
    v = g.gate_open(act(side="short", stop=82000.0, take_profit=76000.0),
                    1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)
    assert v.notional == pytest.approx(600.0)


def test_asymmetric_cooldowns(tmp_path):
    g, s = mk(tmp_path)
    now = time.time()
    g.cooldown_after_close("BTC", "sl", now=now)
    g.cooldown_after_close("ETH", "tp", now=now)
    assert s.cooldown_until("BTC", now + 3601) is not None    # 4h stop cooldown holds
    assert s.cooldown_until("ETH", now + 3601) is None        # 1h tp cooldown expired


def test_tp_net_floor_refuses_weak_projection(tmp_path):
    import dataclasses
    g, _ = mk(tmp_path)
    g.cfg = dataclasses.replace(CFG, tp_net_floor_usd=3.0, risk_pct=2.0)
    # equity 67 -> risk 1.34; 2R -> gross 2.68; fees on ~67 notional ≈ 0.14 -> net ~2.54 < 3
    v = g.gate_open(act(leverage=10), 67.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "projected net TP" in v.reason
    # 4R (tp 88000): gross 5.36 - fees ~0.14 = ~5.22 >= 3 -> approved
    v = g.gate_open(act(leverage=10, take_profit=88000.0), 67.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)


def test_tp_net_floor_disabled_at_zero(tmp_path):
    g, _ = mk(tmp_path)
    assert g.cfg.tp_net_floor_usd == 0.0
    v = g.gate_open(act(leverage=10), 67.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)  # same weak projection passes when floor off


def test_profitable_stop_gets_normal_cooldown(tmp_path):
    g, s = mk(tmp_path)
    now = time.time()
    g.cooldown_after_close("NVDA-WIN", "sl", now=now, pnl=0.60)   # winning trail-stop
    g.cooldown_after_close("NVDA-LOSS", "sl", now=now, pnl=-1.2)  # losing stop-out
    g.cooldown_after_close("NVDA-UNK", "sl", now=now)             # unknown -> conservative
    assert s.cooldown_until("NVDA-WIN", now + 3601) is None       # 1h only
    assert s.cooldown_until("NVDA-LOSS", now + 3601) is not None  # 4h holds
    assert s.cooldown_until("NVDA-UNK", now + 3601) is not None   # 4h holds


# -- 2026-08-29 post-mortem rails -----------------------------------------
RAILS = RiskCfg(risk_pct=2.0, max_leverage=20.0, max_concurrent=1, daily_entry_cap=3,
                kill_switch_pct=15.0, min_rr=2.0, stale_call_secs=900,
                cooldown_secs=3600, stop_cooldown_secs=14400, min_notional=10.0,
                slippage_pct=5.0, paper_bankroll=1000.0,
                day_loss_halt_pct=6.0, min_stop_pct=2.0, atr_stop_mult=4.0,
                max_range_pos_long=0.80, min_range_pos_short=0.20,
                equity_open_blackout_mins=30, equity_close_blackout_mins=30,
                breakeven_at_r=1.0, time_stop_secs=10800, entry_expiry_secs=7200)

# 2026-08-28 18:00Z — a Friday, 14:00 in New York: mid-session, no blackout.
MID_SESSION = 1787940000.0


def rails_guard(tmp_path):
    state = State(str(tmp_path / "rails.db"))
    state.open_day(DAY, 1000.0)
    return Guard(RAILS, state, conviction_min=0.75), state


def feats(**kw) -> dict:
    base = {"range24h_pos": 0.5, "atr15m_pct": 0.4}
    base.update(kw)
    return base


def test_operator_pause_refuses_every_entry(tmp_path):
    g, s = rails_guard(tmp_path)
    s.set_paused(True)
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION)
    assert isinstance(v, Refusal) and "paused" in v.reason
    s.set_paused(False)
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION), Approved)


def test_day_loss_halt_stops_digging(tmp_path):
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    day_pnl_pct=-6.4, now=MID_SESSION)
    assert isinstance(v, Refusal) and "-6% soft limit" in v.reason
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    day_pnl_pct=-5.9, now=MID_SESSION), Approved)


def test_no_chasing_the_range_edge(tmp_path):
    g, _ = rails_guard(tmp_path)
    long_top = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                           reserved_order_markets=NO_RESERVED,
                           features=feats(range24h_pos=0.96), now=MID_SESSION)
    assert isinstance(long_top, Refusal) and "chasing the high" in long_top.reason
    short_low = g.gate_open(act(side="short", stop=82000.0, take_profit=76000.0),
                            1000.0, MARK, 40, DAY, available_margin=1000.0,
                            reserved_order_markets=NO_RESERVED,
                            features=feats(range24h_pos=0.05), now=MID_SESSION)
    assert isinstance(short_low, Refusal) and "chasing the low" in short_low.reason
    # a short at the top of the range is exactly the trade we want to allow
    assert isinstance(
        g.gate_open(act(side="short", stop=82000.0, take_profit=76000.0),
                    1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED,
                    features=feats(range24h_pos=0.95), now=MID_SESSION), Approved)


def test_noise_width_stops_refused_by_percent_and_by_atr(tmp_path):
    g, _ = rails_guard(tmp_path)
    tight = g.gate_open(act(stop=79000.0, take_profit=83000.0),  # 1.25% stop
                        1000.0, MARK, 40, DAY, available_margin=1000.0,
                        reserved_order_markets=NO_RESERVED, features=feats(),
                        now=MID_SESSION)
    assert isinstance(tight, Refusal) and "2.0% floor" in tight.reason
    # 2.5% clears the percent floor but not 4x a 0.9% ATR
    atr_bound = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                            reserved_order_markets=NO_RESERVED,
                            features=feats(atr15m_pct=0.9), now=MID_SESSION)
    assert isinstance(atr_bound, Refusal) and "ATR15m" in atr_bound.reason


def test_builder_dex_entries_blacked_out_around_the_us_open_and_close(tmp_path):
    g, _ = rails_guard(tmp_path)
    xyz = dict(market="xyz:NVDA", stop=78000.0, take_profit=84000.0)
    # 09:32 New York — two minutes into the session (the CRWD trade)
    open_ts = MID_SESSION - (4 * 3600 + 28 * 60)
    v = g.gate_open(act(**xyz), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(), now=open_ts)
    assert isinstance(v, Refusal) and "opening blackout" in v.reason
    # 15:50 New York — ten minutes from the close
    close_ts = MID_SESSION + (1 * 3600 + 50 * 60)
    v = g.gate_open(act(**xyz), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(), now=close_ts)
    assert isinstance(v, Refusal) and "closing blackout" in v.reason
    # native crypto is never blacked out — it has no cash session
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=open_ts), Approved)


def test_resting_entry_prices_size_rr_and_must_not_cross(tmp_path):
    g, _ = rails_guard(tmp_path)
    # long resting BELOW the mark: sized from 78,400, not from 80,000
    v = g.gate_open(act(entry=78400.0, stop=76000.0, take_profit=84000.0),
                    1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION)
    assert isinstance(v, Approved)
    assert v.resting is True and v.entry_px == 78400.0
    # risk $20 / (2400/78400 = 3.061%) = $653.33
    assert v.notional == pytest.approx(20.0 / (2400.0 / 78400.0))
    crossing = g.gate_open(act(entry=80500.0, stop=76000.0, take_profit=84000.0),
                           1000.0, MARK, 40, DAY, available_margin=1000.0,
                           reserved_order_markets=NO_RESERVED, features=feats(),
                           now=MID_SESSION)
    assert isinstance(crossing, Refusal) and "rest BELOW the mark" in crossing.reason
    far = g.gate_open(act(entry=70000.0, stop=68000.0, take_profit=79000.0),
                      1000.0, MARK, 40, DAY, available_margin=1000.0,
                      reserved_order_markets=NO_RESERVED, features=feats(),
                      now=MID_SESSION)
    assert isinstance(far, Refusal) and "never fill" in far.reason


def test_short_resting_entry_must_be_above_the_mark(tmp_path):
    g, _ = rails_guard(tmp_path)
    ok = g.gate_open(act(side="short", entry=81000.0, stop=83000.0, take_profit=75000.0),
                     1000.0, MARK, 40, DAY, available_margin=1000.0,
                     reserved_order_markets=NO_RESERVED,
                     features=feats(range24h_pos=0.6), now=MID_SESSION)
    assert isinstance(ok, Approved) and ok.entry_px == 81000.0
    bad = g.gate_open(act(side="short", entry=79000.0, stop=83000.0, take_profit=75000.0),
                      1000.0, MARK, 40, DAY, available_margin=1000.0,
                      reserved_order_markets=NO_RESERVED,
                      features=feats(range24h_pos=0.6), now=MID_SESSION)
    assert isinstance(bad, Refusal) and "rest ABOVE the mark" in bad.reason


def test_us_session_minutes_is_dst_aware_and_skips_weekends():
    from peri.risk import us_session_minutes
    # 2026-08-28 13:30Z is exactly 09:30 EDT
    assert us_session_minutes(1787923800.0)[0] == pytest.approx(0.0, abs=0.02)
    # 2026-01-30 14:30Z is exactly 09:30 EST
    assert us_session_minutes(1769783400.0)[0] == pytest.approx(0.0, abs=0.02)
    # 2026-08-29 is a Saturday
    assert us_session_minutes(1788019200.0) is None


# -- liquidation must sit outside the stop (2026-08-29 audit) -------------
def test_a_stop_beyond_the_isolated_liquidation_band_is_refused(tmp_path):
    """08-28's palladium trade: 20x isolated, 2.75% stop, on a 20x-max market
    whose liquidation sits ~2.5% away. The stop could never have fired."""
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(leverage=20, stop=77800.0, take_profit=86000.0),
                    1000.0, MARK, market_max_lev=20, day=DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=MID_SESSION)
    assert isinstance(v, Refusal)
    assert "liquidation band" in v.reason and "10x" in v.reason
    # the same stop at 10x has a 7.5% band — plenty of room
    assert isinstance(
        g.gate_open(act(leverage=10, stop=77800.0, take_profit=86000.0),
                    1000.0, MARK, 20, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION), Approved)
    # cross margin is backed by the whole account, so the band does not apply
    assert isinstance(
        g.gate_open(act(leverage=20, stop=77800.0, take_profit=86000.0,
                        margin_mode="cross"),
                    1000.0, MARK, 20, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION), Approved)


def test_the_leverage_cap_is_judged_before_the_liquidation_band(tmp_path):
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(leverage=20, stop=77800.0, take_profit=86000.0),
                    1000.0, MARK, market_max_lev=15, day=DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=MID_SESSION)
    assert isinstance(v, Refusal) and "exceeds venue maximum" in v.reason


def test_isolated_liq_distance_matches_the_venue():
    from peri.risk import isolated_liq_distance
    # live check 2026-08-29: BTC 10x isolated, max 40 -> venue liquidation was
    # 8.57% away; the estimate runs slightly wide, which LIQ_SAFETY absorbs
    assert isolated_liq_distance(10, 40) == pytest.approx(0.0875)
    assert isolated_liq_distance(20, 20) == pytest.approx(0.025)
    assert isolated_liq_distance(0, 20) == 0.0


def test_a_resting_entry_flush_against_the_mark_is_refused(tmp_path):
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(entry=79999.0, stop=76000.0, take_profit=88000.0),
                    1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION)
    assert isinstance(v, Refusal) and "crosses and fills as a taker" in v.reason


def test_feature_gates_fail_closed_when_candles_are_missing(tmp_path):
    """The range-edge and ATR rails must never silently switch off because one
    candle feed was down — that is the 08-28 trade walking straight through."""
    g, _ = rails_guard(tmp_path)
    for missing in ({"atr15m_pct": 0.4}, {"range24h_pos": 0.5}, {}):
        v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                        reserved_order_markets=NO_RESERVED, features=missing,
                        now=MID_SESSION)
        assert isinstance(v, Refusal), missing
        assert "unavailable" in v.reason


def test_the_session_calendar_knows_holidays_and_half_days():
    from peri.risk import us_session_minutes
    from datetime import datetime
    from zoneinfo import ZoneInfo
    ny = ZoneInfo("America/New_York")

    def at(y, m, d, hh, mm):
        return datetime(y, m, d, hh, mm, tzinfo=ny).timestamp()

    # Thanksgiving 2026 is a full closure, not a normal Thursday session
    assert us_session_minutes(at(2026, 11, 26, 11, 0)) is None
    # ...and the Friday after closes at 13:00, so 12:50 is 10 minutes out
    since_open, until_close = us_session_minutes(at(2026, 11, 27, 12, 50))
    assert until_close == pytest.approx(10.0)
    assert since_open == pytest.approx(200.0)
    # a normal session still measures to 16:00
    _, normal_close = us_session_minutes(at(2026, 12, 1, 12, 50))
    assert normal_close == pytest.approx(190.0)


def test_a_half_day_close_still_triggers_the_closing_blackout(tmp_path):
    from datetime import datetime
    from zoneinfo import ZoneInfo
    g, _ = rails_guard(tmp_path)
    ts = datetime(2026, 11, 27, 12, 50, tzinfo=ZoneInfo("America/New_York")).timestamp()
    v = g.gate_open(act(market="xyz:NVDA"), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=ts)
    assert isinstance(v, Refusal) and "closing blackout" in v.reason


def test_no_builder_dex_entry_on_a_weekend_whatever_rth_only_says(tmp_path):
    """On a Saturday nothing on the xyz dex has a live underlying — equities are
    shut and CME commodities close Friday 17:00 ET until Sunday 18:00."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    g, _ = rails_guard(tmp_path)
    assert g.cfg.equity_rth_only is False        # the knob is OFF, for commodities
    sat = datetime(2026, 8, 29, 12, 0, tzinfo=ZoneInfo("America/New_York")).timestamp()
    # Saturday shuts everything, but for different reasons: stocks have no cash
    # session, and Globex is down from Friday 17:00 until Sunday 18:00
    stock = g.gate_open(act(market="xyz:NVDA"), 1000.0, MARK, 40, DAY,
                        available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                        features=feats(), now=sat)
    assert isinstance(stock, Refusal) and "weekend" in stock.reason
    for market in ("xyz:GOLD", "xyz:SP500"):
        v = g.gate_open(act(market=market), 1000.0, MARK, 40, DAY,
                        available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                        features=feats(), now=sat)
        assert isinstance(v, Refusal) and "Saturday" in v.reason, market
    # crypto has no session at all and trades right through
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=sat), Approved)


def test_commodities_trade_on_sunday_evening_when_globex_reopens(tmp_path):
    """2026-08-30 23:09 ET: xyz:CL was doing $18M a 90-minute window and moving
    tick by tick, while the blanket weekend rule refused it."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    ny = ZoneInfo("America/New_York")
    g, _ = rails_guard(tmp_path)

    def at(y, m, d, hh, mm):
        return datetime(y, m, d, hh, mm, tzinfo=ny).timestamp()

    def verdict(market, when):
        return g.gate_open(act(market=market), 1000.0, MARK, 40, DAY,
                           available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                           features=feats(), now=when)

    sunday_evening = at(2026, 8, 30, 23, 9)
    assert isinstance(verdict("xyz:CL", sunday_evening), Approved)
    assert isinstance(verdict("xyz:GOLD", sunday_evening), Approved)
    assert isinstance(verdict("xyz:SP500", sunday_evening), Approved)
    # ...but a single-name stock has no Sunday session at all
    stock = verdict("xyz:NVDA", sunday_evening)
    assert isinstance(stock, Refusal) and "weekend" in stock.reason


def test_the_globex_calendar_is_respected_at_its_edges(tmp_path):
    from datetime import datetime
    from zoneinfo import ZoneInfo
    from peri.risk import cme_session_open
    ny = ZoneInfo("America/New_York")

    def at(y, m, d, hh, mm):
        return datetime(y, m, d, hh, mm, tzinfo=ny).timestamp()

    assert cme_session_open(at(2026, 8, 30, 17, 59)) == "Globex reopens at 18:00 ET on Sunday"
    assert cme_session_open(at(2026, 8, 30, 18, 1)) is None          # Sunday reopen
    assert cme_session_open(at(2026, 8, 29, 12, 0)) == "Globex is shut all Saturday"
    assert cme_session_open(at(2026, 8, 28, 17, 30)) is not None     # Friday, closed for the week
    assert cme_session_open(at(2026, 8, 28, 16, 30)) is None         # Friday, still trading
    assert cme_session_open(at(2026, 9, 1, 17, 30)) is not None      # Tuesday daily halt
    assert cme_session_open(at(2026, 9, 1, 18, 30)) is None          # ...and back after it

    g, _ = rails_guard(tmp_path)
    halt = g.gate_open(act(market="xyz:CL"), 1000.0, MARK, 40, DAY,
                       available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                       features=feats(), now=at(2026, 9, 1, 17, 30))
    assert isinstance(halt, Refusal) and "17:00-18:00 ET Globex halt" in halt.reason


def test_a_commodity_is_not_subject_to_the_equity_open_blackout(tmp_path):
    """Globex is already hours into its session by the time the cash market
    opens; the 09:30 blackout is an equity concern."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    g, _ = rails_guard(tmp_path)
    just_after_open = datetime(2026, 9, 1, 9, 32,
                               tzinfo=ZoneInfo("America/New_York")).timestamp()
    assert isinstance(
        g.gate_open(act(market="xyz:CL"), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=just_after_open), Approved)
    stock = g.gate_open(act(market="xyz:NVDA"), 1000.0, MARK, 40, DAY,
                        available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                        features=feats(), now=just_after_open)
    assert isinstance(stock, Refusal) and "opening blackout" in stock.reason


def test_a_korean_name_trades_on_its_own_exchange_hours(tmp_path):
    """xyz:SKHX is the largest market on the builder dex ($298M/24h). Blocking it
    outside New York hours refused SK Hynix during its own live session."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    g, _ = rails_guard(tmp_path)
    seoul = ZoneInfo("Asia/Seoul")

    def verdict(market, when):
        return g.gate_open(act(market=market), 1000.0, MARK, 40, DAY,
                           available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                           features=feats(), now=when)

    # Monday 12:09 KST — Seoul is mid-session, New York is asleep
    open_kst = datetime(2026, 8, 31, 12, 9, tzinfo=seoul).timestamp()
    assert isinstance(verdict("xyz:SKHX", open_kst), Approved)
    assert isinstance(verdict("xyz:SMSN", open_kst), Approved)
    # a US name at the same instant has no session at all
    assert isinstance(verdict("xyz:NVDA", open_kst), Refusal)

    shut = verdict("xyz:SKHX", datetime(2026, 8, 31, 18, 0, tzinfo=seoul).timestamp())
    assert isinstance(shut, Refusal) and "Seoul cash market is closed" in shut.reason
    weekend = verdict("xyz:SKHX", datetime(2026, 8, 29, 12, 0, tzinfo=seoul).timestamp())
    assert isinstance(weekend, Refusal) and "closed for the weekend" in weekend.reason


def test_the_tokyo_lunch_break_is_respected(tmp_path):
    from datetime import datetime
    from zoneinfo import ZoneInfo
    from peri.risk import foreign_session_open
    tokyo = ZoneInfo("Asia/Tokyo")

    def at(hh, mm):
        return datetime(2026, 8, 31, hh, mm, tzinfo=tokyo).timestamp()

    assert foreign_session_open("KIOXIA", at(10, 0)) is None
    assert foreign_session_open("KIOXIA", at(12, 0)) is not None    # lunch
    assert foreign_session_open("KIOXIA", at(13, 0)) is None        # afternoon
    assert foreign_session_open("KIOXIA", at(16, 0)) is not None
    # an unmapped ticker is not claimed to be open or shut by this rule
    assert foreign_session_open("NVDA", at(10, 0)) is None


def test_a_resting_entry_is_judged_where_IT_sits_in_the_range(tmp_path):
    """2026-08-30: 12 resting entries in 36h were refused for 'chasing', while
    doing exactly what the refusal text asks — a bounce short resting ABOVE a
    market pinned at its low was judged at the mark's 0.09, not the entry's."""
    g, _ = rails_guard(tmp_path)
    # HYPE pinned at the bottom of its range; the analyst rests a short into the bounce
    feats_low = {"range24h_pos": 0.09, "atr15m_pct": 0.5,
                 "hi_24h": 88000.0, "lo_24h": 76000.0}
    bounce_short = act(side="short", entry=82000.0, stop=84500.0, take_profit=76500.0)
    v = g.gate_open(bounce_short, 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats_low,
                    now=MID_SESSION)
    assert isinstance(v, Approved), getattr(v, "reason", None)
    assert v.entry_px == 82000.0

    # ...but a short whose entry is itself down at the low is still chasing:
    # 78,000 in a 76,000-88,000 range is 0.17, under the 0.20 floor
    chase = act(side="short", entry=78000.0, stop=80100.0, take_profit=73500.0)
    v = g.gate_open(chase, 1000.0, 77000.0, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats_low,
                    now=MID_SESSION)
    assert isinstance(v, Refusal), getattr(v, "reason", None)
    assert "your entry" in v.reason and "chasing the low" in v.reason


def test_a_market_order_is_still_judged_at_the_mark(tmp_path):
    g, _ = rails_guard(tmp_path)
    feats_high = {"range24h_pos": 0.97, "atr15m_pct": 0.5,
                  "hi_24h": 81000.0, "lo_24h": 76000.0}
    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats_high,
                    now=MID_SESSION)
    assert isinstance(v, Refusal) and "the mark" in v.reason


def test_an_entry_outside_the_24h_range_is_clamped_not_wrapped(tmp_path):
    g, _ = rails_guard(tmp_path)
    feats = {"range24h_pos": 0.5, "atr15m_pct": 0.5, "hi_24h": 81000.0, "lo_24h": 76000.0}
    # a long resting far BELOW the 24h low: range position clamps to 0, not negative
    v = g.gate_open(act(entry=77000.0, stop=75000.0, take_profit=83000.0),
                    1000.0, 79000.0, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats, now=MID_SESSION)
    assert isinstance(v, Approved)


def test_asian_listings_use_their_own_exchange(tmp_path):
    """Verified against the listings: Z.ai/Zhipu (02513.HK) and MiniMax on the
    HKEX; Unitree (688836), CXMT and GigaDevice on Shanghai."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    from peri.risk import foreign_session_open

    def at(zone, hh, mm, day=31):
        return datetime(2026, 8, day, hh, mm, tzinfo=ZoneInfo(zone)).timestamp()

    assert foreign_session_open("ZHIPU", at("Asia/Hong_Kong", 10, 30)) is None
    assert foreign_session_open("MINIMAX", at("Asia/Hong_Kong", 12, 30)) is not None
    assert foreign_session_open("UNITREE", at("Asia/Shanghai", 10, 0)) is None
    assert foreign_session_open("CXMT", at("Asia/Shanghai", 12, 0)) is not None   # lunch
    assert foreign_session_open("GIGADEV", at("Asia/Shanghai", 14, 0)) is None
    assert foreign_session_open("UNITREE", at("Asia/Shanghai", 15, 30)) is not None
    assert foreign_session_open("SKHY", at("Asia/Seoul", 10, 0)) is None


def test_a_private_company_synthetic_never_closes(tmp_path):
    """io:ANTH has no exchange behind it — nothing opens or shuts, so it trades
    like crypto. It was being weekend-blocked as if it were a US stock."""
    from datetime import datetime
    from zoneinfo import ZoneInfo
    g, _ = rails_guard(tmp_path)
    sat = datetime(2026, 8, 29, 12, 0, tzinfo=ZoneInfo("America/New_York")).timestamp()
    v = g.gate_open(act(market="io:ANTH"), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=sat)
    assert isinstance(v, Approved), getattr(v, "reason", None)
    # ...but a company that HAS listed keeps exchange hours. SpaceX IPO'd on
    # Nasdaq on 2026-06-12, so xyz:SPCX is a US stock, not a private synthetic.
    for market in ("xyz:NVDA", "xyz:SPCX"):
        shut = g.gate_open(act(market=market), 1000.0, MARK, 40, DAY,
                           available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                           features=feats(), now=sat)
        assert isinstance(shut, Refusal), market
        assert "weekend" in shut.reason


def test_the_guard_refuses_a_lot_too_coarse_for_the_risk(tmp_path):
    """A szDecimals=0 market at $50 has a $50 minimum lot; a $29.50 risk budget
    cannot buy one without risking 69% more than approved."""
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(market="xyz:CHUNKY", stop=76000.0, take_profit=88000.0),
                    1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION, sz_decimals=0)
    assert isinstance(v, Refusal)
    assert "one venue lot" in v.reason or "too coarse" in v.reason


def test_the_approved_lot_is_exact_and_never_over_risks(tmp_path):
    g, _ = rails_guard(tmp_path)
    v = g.gate_open(act(stop=76000.0, take_profit=88000.0), 1000.0, MARK, 40, DAY,
                    available_margin=1000.0, reserved_order_markets=NO_RESERVED,
                    features=feats(), now=MID_SESSION, sz_decimals=3)
    assert isinstance(v, Approved)
    assert abs(v.size * 1000 - round(v.size * 1000)) < 1e-9      # a whole 0.001 lot
    assert v.notional == pytest.approx(v.size * MARK)
    assert abs(MARK - 76000.0) * v.size <= v.size_usd_risk * 1.05


def test_no_entry_into_a_scheduled_shock(tmp_path):
    """Opening minutes before NFP is a coin flip, not a thesis."""
    g, state = rails_guard(tmp_path)
    g.cfg.event_blackout_mins = 45
    nfp = MID_SESSION + 30 * 60
    state.add_calendar_event(nfp, "August jobs report", impact="high")

    v = g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION)
    assert isinstance(v, Refusal)
    assert "August jobs report lands in 30m" in v.reason
    assert "trade the reaction" in v.reason

    # an hour out is fine, and so is the moment after the print
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=nfp - 60 * 60), Approved)
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=nfp + 60), Approved)


def test_only_high_impact_events_blackout(tmp_path):
    g, state = rails_guard(tmp_path)
    g.cfg.event_blackout_mins = 45
    state.add_calendar_event(MID_SESSION + 15 * 60, "ISM Services", impact="medium")
    assert isinstance(
        g.gate_open(act(), 1000.0, MARK, 40, DAY, available_margin=1000.0,
                    reserved_order_markets=NO_RESERVED, features=feats(),
                    now=MID_SESSION), Approved)


def test_a_stop_exactly_at_the_floor_is_accepted(tmp_path):
    """1.8/90.0 is 0.019999999999999997 in binary, so a stop placed at precisely
    the 2.00% floor was refused as 'stop 2.00% from entry < 2.0% floor' — an
    unsatisfiable gate. The analyst hit this on xyz:BRENTOIL at 11:02Z."""
    from peri.risk import BOUNDARY_EPS

    dist_pct = (90.0 - 88.2) / 90.0 * 100      # entry 90.0, stop 88.2 — exactly 2%
    assert dist_pct < 2.0                       # ...but not in binary: 1.999999999999997
    assert not (dist_pct < 2.0 - BOUNDARY_EPS)  # and the epsilon clears it


def test_zero_fee_schedule_passes_what_hl_fees_refuse(tmp_path):
    """The projected-net-TP floor must price the venue it gates for: a setup
    whose edge is thinner than HL's 0.15% round trip still clears it on a
    zero-fee venue."""
    import dataclasses
    g, _ = mk(tmp_path)
    g.cfg = dataclasses.replace(CFG, tp_net_floor_usd=2.6, risk_pct=2.0)
    # equity 67 -> risk 1.34; 2R -> gross 2.68; HL fees ~0.10 -> net ~2.58 < 2.6
    v = g.gate_open(act(leverage=10), 67.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Refusal) and "projected net TP" in v.reason
    g.fees = ZERO
    v = g.gate_open(act(leverage=10), 67.0, MARK, 40, DAY,
                    available_margin=50.0, reserved_order_markets=NO_RESERVED)
    assert isinstance(v, Approved)
