"""The fee schedule must have exactly one definition.

It had drifted into three: `router`'s named constants, two literals inlined in
`risk.gate_open` (which could not import router without a cycle), and a third
hardcoded value in `market.close_fee_rate`. Nothing bound them, so the gate that
decides whether a trade clears its costs could disagree with the ledger booking
them. These tests fail the moment they diverge again.
"""

import pathlib
import re

from peri import fees, market, router


def test_the_measured_schedule_is_what_the_account_actually_pays():
    """userFees on the live account, 2026-08-29: HL taker 4.5bp, HL maker 1.5bp,
    Trench builder 3bp on every order regardless of side or dex."""
    assert fees.HL_TAKER_RATE == 0.00045
    assert fees.HL_MAKER_RATE == 0.00015
    assert fees.BUILDER_RATE == 0.0003
    assert fees.TAKER_FEE_RATE == 0.00075          # 7.5bp
    assert fees.MAKER_FEE_RATE == 0.00045          # 4.5bp


def test_every_module_reads_the_same_numbers():
    assert router.FEE_RATE is fees.FEE_RATE
    assert router.TAKER_FEE_RATE is fees.TAKER_FEE_RATE
    assert router.MAKER_FEE_RATE is fees.MAKER_FEE_RATE
    assert market.close_fee_rate() == fees.TAKER_FEE_RATE


def test_a_resting_entry_pays_the_maker_side_and_a_market_order_the_taker_side():
    assert fees.entry_rate(resting=True) == fees.MAKER_FEE_RATE
    assert fees.entry_rate(resting=False) == fees.TAKER_FEE_RATE
    # the exit is always a taker: a stop or TP is a triggered market order
    assert fees.round_trip_rate(True) == fees.MAKER_FEE_RATE + fees.TAKER_FEE_RATE
    assert fees.round_trip_rate(False) == fees.TAKER_FEE_RATE * 2
    # the round trip the prompt quotes to the analyst as "~0.15% of notional"
    assert fees.round_trip_rate(False) == 0.0015


def test_schedules_agree_with_the_module_constants():
    assert fees.HL_SCHEDULE.taker_rate == fees.TAKER_FEE_RATE
    assert fees.HL_SCHEDULE.maker_rate == fees.MAKER_FEE_RATE
    assert fees.HL_SCHEDULE.entry_rate(True) == fees.entry_rate(True)
    assert fees.HL_SCHEDULE.round_trip_rate(False) == fees.round_trip_rate(False)


def test_lighter_schedule_is_exactly_zero():
    assert fees.ZERO.taker_rate == 0.0
    assert fees.ZERO.maker_rate == 0.0
    assert fees.ZERO.entry_rate(True) == 0.0
    assert fees.ZERO.entry_rate(False) == 0.0
    assert fees.ZERO.round_trip_rate(True) == 0.0
    assert fees.ZERO.round_trip_rate(False) == 0.0


def test_no_module_reintroduces_a_hardcoded_rate():
    """The literals are allowed in peri/fees.py and nowhere else."""
    src = pathlib.Path(__file__).resolve().parents[1] / "src" / "peri"
    literals = re.compile(r"0\.00045|0\.00075|0\.00015|0\.0003\b")
    offenders = {
        path.name: [n for n, line in enumerate(path.read_text().splitlines(), 1)
                    if literals.search(line)]
        for path in src.glob("*.py") if path.name != "fees.py"
    }
    assert not {k: v for k, v in offenders.items() if v}, offenders
