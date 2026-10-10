#!/usr/bin/env python3
"""DATUM worker names: primed's `clients[].workers` become one miner row per worker (XBT-140).

Imports server.py read-only (throwaway DB, config.json only if none exists).
"""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
os.environ.setdefault("POOL_DB", str(Path(tempfile.mkdtemp(prefix="workers-")) / "pool.sqlite"))
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server  # noqa: E402
finally:
    if _made_cfg:
        _cfg.unlink()

ADDR = "bc1qvchspt9gm5dq0geq3kxx53k3n87znwwvwc30t0"
OTHER = "bc1qk3kxstl02hqnhynwtx0zws7merw6ynut52vtzs"
GW = "aabbccddeeff0011"


def worker(name, ghs=10.0, work=600, shares=6, age=4, identity=ADDR, **more):
    return {"identity": identity, "name": name, "hashrate_ghs": ghs, "work": work, "shares": shares, "last_share_s": age, **more}


def client(workers, overflow=(), fee_path="datum", gateway=GW, tag="Farm One", **more):
    c = {"id": 7, "gateway": gateway, "fee_path": fee_path, "identity": ADDR, "secondary_tag": tag, "offline": False}
    if workers is not None:
        c["workers"] = list(workers)
        c["workers_overflow"] = list(overflow)
    c.update(more)
    return c


def info(hr=30.0, ww=5_000, **more):
    return {
        "window_work": ww, "window_percent": 2.5, "window_sats": 7_000, "window_shares": 40, "credits": 40,
        "hr_ghs": hr, "last_share_s": 3.0, "fee_path": "datum", "stratum_work": 0, **more,
    }


class PrimeWorkers(unittest.TestCase):
    """`rows(clients, prime, stratum)` is the one way these tests read the miner rows."""

    def rows(self, clients, prime=None, stratum=()):
        prime = {ADDR: info()} if prime is None else prime
        doc = {"window": {"miners": [{"identity": a, "hashrate_ghs": i["hr_ghs"], "last_share_s": i["last_share_s"]} for a, i in prime.items()]}}
        state = {"prime": prime, "prime_meta": {"clients": clients}, "gateway_hr": {m["address"]: m["hr_ghs"] for m in stratum}}
        server._ledger_hr_cache.update({"ts": 0, "by_addr": {}, "pool_ghs": 0.0, "age": {}})
        with mock.patch.object(server, "prime_doc", return_value=doc), mock.patch.object(
            server, "prime_reachable", return_value=True
        ), mock.patch.dict(server.state, state):
            out = server.merge_prime_online([dict(m) for m in stratum])
            self.rollup = server.rollup_online_by_address([dict(m) for m in out])
        server._ledger_hr_cache.update({"ts": 0, "by_addr": {}, "pool_ghs": 0.0, "age": {}})
        return out

    def test_two_workers_are_two_rows_with_the_window_on_the_address(self):
        rows = self.rows([client([worker("A301", ghs=20.0, work=4_000, shares=40), worker("A302", ghs=10.0, work=2_000, shares=20, age=9)])])
        self.assertEqual([r["worker"] for r in rows], ["A301", "A302"])
        a301, a302 = rows
        self.assertEqual((a301["via"], a301["gateway_name"], a301["gateway"], a301["online"]), ("prime", "Farm One", GW, True))
        # each row's own rate, count and age
        self.assertEqual((a301["path_hr_ghs"], a302["path_hr_ghs"]), (20.0, 10.0))
        self.assertEqual((a301["shares_lifetime"], a301["shares_session"], a302["shares_lifetime"]), (4_000, 40, 2_000))
        self.assertEqual((a301["last_share_s"], a302["last_share_s"]), (4.0, 9.0))
        self.assertEqual(a301["user"], f"{ADDR}.A301")
        # the window, percent, next-block sats and credited rate are the address's, on every row
        for r in rows:
            self.assertEqual((r["window_work"], r["window_percent"], r["window_sats"], r["window_shares"]), (5_000, 2.5, 7_000, 40))
            self.assertEqual(r["credited_hr_ghs"], 30.0)
            self.assertFalse(r.get("worker_names_missing"))
        # the pool's miners table: one line for the address, two workers, no double window
        self.assertEqual(len(self.rollup), 1)
        one = self.rollup[0]
        self.assertEqual((one["sessions"], one["via"], one["hr_ghs"], one["window_work"]), (2, "prime", 30.0, 5_000))
        self.assertEqual(one["shares_lifetime"], 6_000)

    def test_an_address_line_never_shows_less_than_its_window_work(self):
        # a gateway that reconnected a minute ago: its workers have counted little so far
        self.rows([client([worker("A301", work=10, shares=1), worker("A302", work=10, shares=1)])])
        self.assertEqual(self.rollup[0]["shares_lifetime"], 5_000)

    def test_no_worker_names_keeps_the_old_row_and_says_so(self):
        rows = self.rows([client([worker("", ghs=30.0)])])
        self.assertEqual([(r["worker"], r["ua"], r["via"]) for r in rows], [("window", "Prime window", "prime")])
        self.assertTrue(rows[0]["worker_names_missing"])
        self.assertEqual(rows[0]["shares_lifetime"], 5_000)

    def test_a_primed_that_reports_no_workers_keeps_the_old_row_and_says_nothing(self):
        rows = self.rows([client(None)])
        self.assertEqual([r["worker"] for r in rows], ["window"])
        self.assertFalse(rows[0]["worker_names_missing"])
        self.assertEqual(server.prime_workers_by_address([client(None)]), {})

    def test_named_and_bare_work_and_evicted_names_all_have_a_row(self):
        over = [worker("", ghs=3.0, work=90, shares=9, names=412)]
        over[0].pop("name")
        rows = self.rows([client([worker("A301", ghs=20.0), worker("", ghs=5.0, work=50, shares=5)], overflow=over)])
        self.assertEqual([r["worker"] for r in rows], ["A301", "", ""])
        self.assertTrue(rows[1]["worker_unnamed"])
        self.assertEqual((rows[1]["path_hr_ghs"], rows[1]["shares_lifetime"], rows[1]["user"]), (5.0, 50, ADDR))
        self.assertEqual((rows[2]["worker_overflow"], rows[2]["path_hr_ghs"]), (412, 3.0))
        self.assertFalse(any(r.get("worker_names_missing") for r in rows))

    def test_a_hostile_name_reaches_the_page_short_and_printable(self):
        flood = [worker(f"n{i:04}", ghs=0.01) for i in range(1_100)]
        nasty = worker("<script>alert(1)</script>" + "W" * 500 + "\x1b[2J矿", ghs=1.0)
        rows = self.rows([client(flood + [nasty, "junk", {"identity": "", "name": "x"}, worker("bad", ghs="NaN-ish")])])
        self.assertEqual(len(rows), 1_101)
        longest = max(rows, key=lambda r: len(r["worker"]))
        self.assertLessEqual(len(longest["worker"]), server.PRIME_WORKER_NAME_MAX)
        self.assertTrue(all(33 <= ord(ch) < 127 for r in rows for ch in r["worker"]))
        self.assertIn("<script>alert(1)</script>WWWWWWW", [r["worker"] for r in rows])
        # a huge reported rate is capped like every other Prime rate
        huge = self.rows([client([worker("A301", ghs=1e30)])])
        self.assertEqual(huge[0]["path_hr_ghs"], server._PRIME_HR_CAP_GHS)

    def test_a_worker_that_stopped_is_not_listed(self):
        rows = self.rows([client([worker("A301"), worker("gone", ghs=0.0, age=5_000), worker("stale", ghs=2.0, age=700)])])
        self.assertEqual([r["worker"] for r in rows], ["A301"])
        # every named worker stopped: back to the one row, and no hint, since names do arrive
        rows = self.rows([client([worker("gone", ghs=0.0, age=5_000)])])
        self.assertEqual([r["worker"] for r in rows], ["window"])
        self.assertFalse(rows[0]["worker_names_missing"])

    def test_workers_follow_their_address_not_the_gateway_s(self):
        # a public gateway whose own payout is ADDR carries a customer's machines too
        clients = [client([worker("A301"), worker("rig1", identity=OTHER, ghs=7.0)])]
        rows = self.rows(clients, prime={ADDR: info(), OTHER: info(hr=7.0, ww=900)})
        self.assertEqual(sorted((r["address"], r["worker"]) for r in rows), sorted([(ADDR, "A301"), (OTHER, "rig1")]))
        other = next(r for r in rows if r["address"] == OTHER)
        self.assertEqual((other["gateway_name"], other["window_work"]), ("Farm One", 900))

    def test_the_same_name_on_two_gateways_is_two_rows(self):
        clients = [client([worker("A301")]), client([worker("A301", ghs=4.0)], gateway="1122334455667788", tag="Farm Two")]
        rows = self.rows(clients)
        self.assertEqual([(r["worker"], r["gateway_name"]) for r in rows], [("A301", "Farm Two"), ("A301", "Farm One")])

    def test_the_house_gateway_and_offline_rows_are_not_read(self):
        house = client([worker("A301")], fee_path="stratum")
        offline = client([worker("A302")], offline=True)
        self.assertEqual(server.prime_workers_by_address([house, offline, "junk", None]), {})

    def test_the_gateway_side_of_a_stratum_miner_is_listed_by_worker(self):
        stratum = [{"address": ADDR, "worker": "S19", "hr_ghs": 10.0, "via": "stratum", "host": "1.2.3.4", "ua": "cgminer", "last_share_s": 2.0, "shares_acc": 5, "diff_acc": 50}]
        prime = {ADDR: info(hr=40.0, stratum_work=1_000)}
        rows = self.rows([client([worker("A301", ghs=18.0), worker("A302", ghs=12.0)])], prime=prime, stratum=stratum)
        self.assertEqual([(r["worker"], r["via"]) for r in rows], [("S19", "stratum"), ("A301", "prime"), ("A302", "prime")])
        self.assertEqual(self.rollup[0]["via"], "both")
        # without names the gateway side is the one row it was, with the hint
        rows = self.rows([client([worker("", ghs=30.0)])], prime=prime, stratum=stratum)
        self.assertEqual([(r["worker"], r["ua"]) for r in rows], [("S19", "cgminer"), ("Farm One", "DATUM gateway")])
        self.assertTrue(rows[1]["worker_names_missing"])

    def test_strings_in_both_languages(self):
        for lang in ("en", "zh-CN"):
            text = (POOL / "static" / "i18n" / f"{lang}.js").read_text()
            for key in ('"workerNamesMissing"', '"workerUnnamed"', '"workerMore"'):
                self.assertIn(key, text, f"{lang}: {key}")


if __name__ == "__main__":
    unittest.main()
