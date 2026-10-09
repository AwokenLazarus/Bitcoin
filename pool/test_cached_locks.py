#!/usr/bin/env python3
"""`cached()` under nested and crossed keys, and with a builder that never returns (XBT-134).

    python3 pool/test_cached_locks.py

On 2026-10-08 a reader hung for two hours: a miner page's builder asked for `solo` while holding
the compute lock that `solo` hashes to, waited on itself for ever, and every later request for a
key on that lock queued behind it until the process ran out of open files. Each test here runs
the calls in threads and fails if one is still alive after a few seconds, so a deadlock is a
failed test, not a hung run.
"""
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
TMP = Path(tempfile.mkdtemp(prefix="cached-"))
os.environ.setdefault("POOL_DB", str(TMP / "pool.sqlite"))
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server
finally:
    if _made_cfg:
        _cfg.unlink()

JOIN_S = 5.0


def stripe(key):
    return server._cache_compute_locks.index(server._compute_lock(key))


def miner_key_on(target, skip=()):
    """A `("miner", addr)` key whose compute lock is lock number `target`."""
    for i in range(100_000):
        key = ("miner", f"bc1qtest{i}")
        if stripe(key) == target and key not in skip:
            return key
    raise AssertionError(f"no miner key on lock {target}")


def run(*fns):
    """Each fn in its own thread. Returns (results, errors) by position; fails on a hang."""
    results, errors = [None] * len(fns), [None] * len(fns)

    def call(i, fn):
        try:
            results[i] = fn()
        except Exception as e:  # noqa: BLE001 (carried back to the test thread, which asserts on it)
            errors[i] = e

    threads = [threading.Thread(target=call, args=(i, fn), daemon=True) for i, fn in enumerate(fns)]
    for t in threads:
        t.start()
    deadline = time.time() + JOIN_S
    for t in threads:
        t.join(max(0.0, deadline - time.time()))
    hung = [i for i, t in enumerate(threads) if t.is_alive()]
    if hung:
        raise AssertionError(f"deadlock: call(s) {hung} still waiting after {JOIN_S:.0f}s")
    return results, errors


class CachedCase(unittest.TestCase):
    def setUp(self):
        with server._resp_cache_lock:
            server._resp_cache.clear()
            server._cache_refreshing.clear()
        for lock in server._cache_compute_locks:
            self.assertFalse(lock.locked(), "an earlier test left a compute lock held")

    def age(self, key, seconds):
        """Make the stored copy of `key` look `seconds` old."""
        with server._resp_cache_lock:
            ts, val = server._resp_cache[key]
            server._resp_cache[key] = (ts - seconds, val)


class NestedKeys(CachedCase):
    def test_inner_key_on_the_lock_the_thread_holds(self):
        """The incident: `("miner", addr)` builds, and reads `solo`, which shares its lock."""
        outer = miner_key_on(stripe("solo"))
        (got,), (err,) = run(lambda: server.cached(outer, 5.0, lambda: {"solo": server.cached("solo", 3.0, lambda: "S")}))
        self.assertIsNone(err)
        self.assertEqual(got, {"solo": "S"})
        self.assertEqual(server.cache_peek("solo"), "S", "the inner payload is stored too")
        self.assertFalse(server._compute_lock(outer).locked())

    def test_every_address_that_shares_a_lock_with_solo_or_overflow(self):
        """Not one lucky address: all of them, for both inner keys the builders read."""
        n = 0
        for inner in ("solo", "overflow"):
            for i in range(640):
                key = ("miner", f"bc1qsweep{i}")
                if stripe(key) != stripe(inner):
                    continue
                n += 1
                with server._resp_cache_lock:
                    server._resp_cache.pop(inner, None)
                (got,), (err,) = run(lambda k=key, inner=inner: server.cached(k, 5.0, lambda: server.cached(inner, 3.0, lambda: 1)))
                self.assertIsNone(err)
                self.assertEqual(got, 1)
        self.assertGreater(n, 0, "no colliding address in the sample")

    def test_three_deep_on_one_lock(self):
        """`hardware` reads `pool`, which reads `overflow`."""
        a = miner_key_on(stripe("overflow"))
        b = miner_key_on(stripe("overflow"), skip=(a,))
        (got,), (err,) = run(
            lambda: server.cached(a, 5.0, lambda: server.cached(b, 5.0, lambda: server.cached("overflow", 5.0, lambda: "deep")))
        )
        self.assertIsNone(err)
        self.assertEqual(got, "deep")

    def test_two_threads_each_wanting_the_others_lock(self):
        """Thread 1 holds lock X and wants a key on Y; thread 2 holds Y and wants a key on X.
        The barrier makes sure both hold their outer lock before either asks for the inner."""
        x, y = stripe("solo"), stripe("overflow")
        if x == y:  # 1 run in 64: move one of them
            y = (x + 1) % len(server._cache_compute_locks)
        outer_x, inner_x = miner_key_on(x), miner_key_on(x, skip=(miner_key_on(x),))
        outer_y, inner_y = miner_key_on(y), miner_key_on(y, skip=(miner_key_on(y),))
        both_hold = threading.Barrier(2, timeout=JOIN_S)

        def build(inner, val):
            def fn():
                both_hold.wait()
                return server.cached(inner, 5.0, lambda: val)

            return fn

        results, errors = run(
            lambda: server.cached(outer_x, 5.0, build(inner_y, "from-y")),
            lambda: server.cached(outer_y, 5.0, build(inner_x, "from-x")),
        )
        self.assertEqual(errors, [None, None])
        self.assertEqual(results, ["from-y", "from-x"])

    def test_nesting_flag_is_cleared_after_a_builder_raises(self):
        def boom():
            raise ValueError("node down")

        with self.assertRaises(ValueError):
            server.cached("k", 5.0, boom)
        self.assertEqual(getattr(server._cache_tls, "building", 0), 0)
        self.assertFalse(server._compute_lock("k").locked())


class OneBuildPerKey(CachedCase):
    def test_a_cold_key_asked_for_by_many_is_built_once(self):
        """The compute lock is still what stops a herd: the fix must not have removed it."""
        calls = []
        start = threading.Barrier(8, timeout=JOIN_S)

        def fn():
            calls.append(1)
            time.sleep(0.2)
            return "built"

        def ask():
            start.wait()
            return server.cached("herd", 5.0, fn)

        results, errors = run(*[ask] * 8)
        self.assertEqual(errors, [None] * 8)
        self.assertEqual(results, ["built"] * 8)
        self.assertEqual(len(calls), 1)


class StuckBuilder(CachedCase):
    def setUp(self):
        super().setUp()
        self.release = threading.Event()
        self.building = threading.Event()
        self.stuck_key = miner_key_on(7)
        self.addCleanup(self._unstick)

        def hang():
            self.building.set()
            self.release.wait(30)
            return "late"

        self.stuck = threading.Thread(target=lambda: server.cached(self.stuck_key, 5.0, hang), daemon=True)
        self.stuck.start()
        self.assertTrue(self.building.wait(JOIN_S))

    def _unstick(self):
        self.release.set()
        self.stuck.join(JOIN_S)

    def test_same_lock_no_older_copy_is_busy_after_the_wait(self):
        other = miner_key_on(7, skip=(self.stuck_key,))
        with mock.patch.object(server, "_COMPUTE_WAIT_S", 0.3):
            t0 = time.time()
            (got,), (err,) = run(lambda: server.cached(other, 5.0, lambda: "never built"))
            took = time.time() - t0
        self.assertIsInstance(err, server.CacheBusy)
        self.assertIsNone(got)
        self.assertGreaterEqual(took, 0.3)
        self.assertLess(took, 2.0)
        self.assertIsNone(server.cache_peek(other), "a timed-out wait stores nothing")

    def test_same_lock_with_an_older_copy_gives_that_copy(self):
        other = miner_key_on(7, skip=(self.stuck_key,))
        server._cache_store(other, "last good")
        self.age(other, 3600)  # far past the stale bound, so the request has to rebuild
        with mock.patch.object(server, "_COMPUTE_WAIT_S", 0.3):
            (got,), (err,) = run(lambda: server.cached(other, 5.0, lambda: "never built"))
        self.assertIsNone(err)
        self.assertEqual(got, "last good")

    def test_a_key_on_another_lock_does_not_wait(self):
        other = miner_key_on(8)
        t0 = time.time()
        (got,), (err,) = run(lambda: server.cached(other, 5.0, lambda: "fresh"))
        self.assertIsNone(err)
        self.assertEqual(got, "fresh")
        self.assertLess(time.time() - t0, 1.0)

    def test_a_builder_that_reads_the_stuck_key_is_not_pinned(self):
        """Nested, and the inner lock is held by the stuck thread: build it here, don't wait."""
        outer = miner_key_on(9)
        (got,), (err,) = run(lambda: server.cached(outer, 5.0, lambda: server.cached(self.stuck_key, 5.0, lambda: "own build")))
        self.assertIsNone(err)
        self.assertEqual(got, "own build")

    def test_the_lock_is_free_again_once_the_builder_returns(self):
        self._unstick()
        self.assertEqual(server.cache_peek(self.stuck_key), "late")
        self.assertFalse(server._cache_compute_locks[7].locked())


class BusyIs503(unittest.TestCase):
    def test_handler_answers_503(self):
        h = object.__new__(server.Handler)
        h.path = "/api/pool"
        sent = []
        h.send_json = lambda obj, code=200, cache_s=0: sent.append((obj, code))
        with mock.patch.object(server.Handler, "_get", side_effect=server.CacheBusy("pool")):
            h.do_GET()
        self.assertEqual(sent, [({"error": "busy"}, 503)])


if __name__ == "__main__":
    unittest.main(verbosity=2)
