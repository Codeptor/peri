"""The sync/async bridge. Offline: a coroutine round-trips the loop thread."""

import asyncio

from peri.lighter_sync import Bridge


def test_call_runs_coroutine_and_returns_its_result():
    b = Bridge()
    try:
        async def echo(x):
            await asyncio.sleep(0)
            return x * 2

        assert b.call(echo(21)) == 42
    finally:
        b.close()


def test_call_surfaces_a_hung_venue_as_timeout():
    b = Bridge()
    try:
        import pytest

        async def hang():
            await asyncio.sleep(30)

        with pytest.raises(TimeoutError):
            b.call(hang(), timeout=0.1)
    finally:
        b.close()
