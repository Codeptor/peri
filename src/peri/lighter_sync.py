"""One background event loop for the synchronous engine to drive async SDKs.

peri's engine is synchronous; Lighter's SDK is async and its client is
loop-bound. Rather than an asyncio.run per call (a fresh client, auth token
and TCP setup for every poll), every venue call goes through one long-lived
loop on a daemon thread. `call` blocks the engine thread until the coroutine
finishes, with a ceiling so a hung venue call raises instead of wedging the
cycle forever.
"""

import asyncio
import threading

DEFAULT_TIMEOUT_SECS = 60.0


class Bridge:
    def __init__(self):
        self._ready = threading.Event()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        if not self._ready.wait(timeout=10):
            raise RuntimeError("lighter bridge loop did not start")

    def _run(self) -> None:
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        self._ready.set()
        self._loop.run_forever()

    def call(self, coro, timeout: float = DEFAULT_TIMEOUT_SECS):
        """Run an already-built coroutine on the bridge loop and return its
        result. Coroutine objects are single-use, so build them inline at each
        call site: bridge.call(self._exec.fetch(...))."""
        assert self._loop is not None
        fut = asyncio.run_coroutine_threadsafe(coro, self._loop)
        return fut.result(timeout=timeout)

    def close(self, timeout: float = 10.0) -> None:
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=timeout)
