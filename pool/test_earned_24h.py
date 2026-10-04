#!/usr/bin/env python3
"""`earned_24h` on /api/miner/<addr>: coinbase outputs plus make-goods from the last 24 hours.

    python3 pool/test_earned_24h.py

Imports server.py the way test_gateway_builds.py does (a throwaway DB, and a config.json only if
none exists). The make-good side runs the real code over a blocks.jsonl and a makegood-queue
written to a temp directory; only the node (coinbase reads, confirmations) is stubbed.
"""
import json
import os
import sqlite3
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
TMP = Path(tempfile.mkdtemp(prefix="earned-"))
os.environ["POOL_DB"] = str(TMP / "pool.sqlite")
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server
finally:
    if _made_cfg:
        _cfg.unlink()

A = "bc1qminera"
B = "bc1qminerb"
P = "bc1qpool"
NOW = 1_800_000_000
SINCE = NOW - 86400
REWARD = 312_500_000


def h(n):
    return f"{n:064x}"


def btc(by_sats):
    """A coinbase the way coinbase_splits returns it: address -> float BTC."""
    return {a: s / 1e8 for a, s in by_sats.items()}


class Chain:
    """The blocks one test sees: found_blocks rows, coinbases, primed's log and the queue."""

    def __init__(self, case):
        self.dir = Path(tempfile.mkdtemp(prefix="chain-", dir=TMP))
        self.fb_rows = []
        self.splits = {}
        self.log = []
        self.confs = {}
        for folder in ("pending", "sent", "failed"):
            (self.dir / "queue" / folder).mkdir(parents=True)
        patches = {
            "BLOCKS_LOG": self.dir / "blocks.jsonl",
            "MAKEGOOD_QUEUE": self.dir / "queue",
            "coinbase_splits": lambda blockhash: self.splits.get(blockhash),
            "prime_doc": lambda max_age=4.0: {"pool": {"address": P}, "blocks": list(self.log)},
            "owed_settlements": dict,
            "tx_confirmations": lambda txid: self.confs.get(txid),
            "rpc": lambda method, params=None: None,
            "_block_log_latest_cache": {"sig": None, "latest": {}},
            "_makegood_owed_cache": {"sig": None, "rows": []},
            "_makegood_jobs_cache": {"sig": None, "jobs": {}},
        }
        for name, value in patches.items():
            p = mock.patch.object(server, name, value)
            p.start()
            case.addCleanup(p.stop)

    def block(self, height, ts, onchain, kind="split", split=(), owed=0, job=None, confs=None, found=True):
        """One block. `onchain` is the coinbase in sats (None: the node cannot be asked);
        `split`/`owed` are what primed logged; `job` is the queue folder its make-good is in."""
        if found:
            self.fb_rows.append({"height": height, "hash": h(height), "ts": ts})
        if onchain is not None:
            self.splits[h(height)] = btc(onchain)
        if kind != "split":
            self.log.append(
                {"ts": ts, "height": height, "hash": h(height), "kind": kind, "owed_sats": owed, "split": [list(x) for x in split]}
            )
        if job:
            txid = f"{height:064x}"[::-1]
            (self.dir / "queue" / job / f"makegood-{height}.json").write_text(json.dumps({"id": f"makegood-{height}", "txid": txid}))
            if confs is not None:
                self.confs[txid] = confs
        return self

    def earned(self, address, now=NOW):
        with (self.dir / "blocks.jsonl").open("w") as f:
            f.writelines(json.dumps(rec) + "\n" for rec in self.log)
        makegoods = server.makegood_rows_for(address, tip=1000)
        return server.earned_window(address, self.fb_rows, makegoods, now=now)


class Earned24h(unittest.TestCase):
    def test_block_inside_and_outside_the_window(self):
        c = Chain(self)
        c.block(100, NOW - 90_000, {A: 2_000_000, P: REWARD - 2_000_000})  # 25 h ago
        c.block(101, SINCE - 1, {A: 30_000, P: REWARD - 30_000})  # one second too old
        c.block(102, SINCE, {A: 40_000, P: REWARD - 40_000})  # exactly 24 h ago
        c.block(103, NOW - 3600, {A: 1_000_000, B: 500_000, P: REWARD - 1_500_000})
        c.block(104, NOW - 60, {B: 700_000, P: REWARD - 700_000})  # paid B, not A
        e = c.earned(A)
        self.assertEqual(e["block_sats"], 1_040_000)
        self.assertEqual(e["blocks"], 2)
        self.assertEqual(e["makegood_sats"], 0)
        self.assertEqual(e["makegood_blocks"], 0)
        self.assertEqual(e["total_sats"], 1_040_000)
        self.assertEqual(e["window_s"], 86400)
        self.assertEqual(e["since_ts"], SINCE)
        for k in ("block_sats", "makegood_sats", "total_sats", "makegood_failed_sats"):
            self.assertIs(type(e[k]), int, k)

    def test_sats_are_summed_as_integers(self):
        # 0.1 + 0.2 style float sums drift; ten outputs of 0.00000001-odd BTC must add exactly.
        c = Chain(self)
        for i in range(10):
            c.block(200 + i, NOW - 100 - i, {A: 10_000_001, P: REWARD - 10_000_001})
        e = c.earned(A)
        self.assertEqual(e["block_sats"], 100_000_010)
        self.assertEqual(e["blocks"], 10)

    def test_pool_only_block_pays_the_miner_as_a_make_good(self):
        c = Chain(self)
        split = [(A, 700_000), (B, 300_000), (P, REWARD - 1_000_000)]
        c.block(300, NOW - 7200, {P: REWARD}, kind="pool-only", split=split, owed=REWARD, job="pending")
        c.block(299, NOW - 100_000, {P: REWARD}, kind="pool-only", split=[(A, 5_000), (P, REWARD - 5_000)], owed=REWARD, job="pending")
        e = c.earned(A)
        self.assertEqual(e["block_sats"], 0)
        self.assertEqual(e["blocks"], 0, "a pool-only coinbase has no output for the miner")
        self.assertEqual(e["makegood_sats"], 700_000, "the make-good on the 28-hour-old block is outside the window")
        self.assertEqual(e["makegood_blocks"], 1)
        self.assertEqual(e["total_sats"], 700_000)

    def test_partial_block_placed_payee_and_dropped_payee(self):
        # The usual partial: A's output is in the coinbase in full, B's was dropped. A earns
        # from the coinbase alone and B from the make-good alone.
        c = Chain(self)
        split = [(A, 400_000), (B, 250_000), (P, REWARD - 650_000)]
        c.block(400, NOW - 1800, {A: 400_000, P: REWARD - 400_000}, kind="partial", split=split, owed=250_000)
        a, b = c.earned(A), c.earned(B)
        self.assertEqual((a["block_sats"], a["makegood_sats"], a["total_sats"]), (400_000, 0, 400_000))
        self.assertEqual((b["block_sats"], b["makegood_sats"], b["total_sats"]), (0, 250_000, 250_000))
        self.assertEqual(a["total_sats"] + b["total_sats"], 650_000, "together: the miners' issued split, once")

    def test_partial_block_same_address_gets_output_and_make_good(self):
        # Two issued entries for one address, one placed and one dropped. Both are real and
        # separate: the coinbase paid the first, the make-good pays the second, and the sum is
        # what the block issued to the address -- nothing counted twice.
        c = Chain(self)
        split = [(A, 400_000), (B, 250_000), (A, 150_000), (P, REWARD - 800_000)]
        c.block(401, NOW - 1800, {A: 400_000, P: REWARD - 400_000}, kind="partial", split=split, owed=400_000)
        e = c.earned(A)
        self.assertEqual(e["block_sats"], 400_000)
        self.assertEqual(e["makegood_sats"], 150_000)
        self.assertEqual(e["total_sats"], sum(s for a, s in split if a == A))
        self.assertEqual((e["blocks"], e["makegood_blocks"]), (1, 1))
        rows = server.makegood_rows_for(A, tip=1000)
        self.assertEqual([(r["sats"], r["verified"]) for r in rows], [(150_000, True)], "unpaid tail sums to primed's owed_sats")

    def test_every_status_but_failed_counts(self):
        c = Chain(self)
        def po(sats):
            return {"kind": "pool-only", "split": [(A, sats), (P, REWARD - sats)], "owed": REWARD}
        c.block(500, NOW - 500, {P: REWARD}, **po(1))  # owed: booked, not signed yet
        c.block(501, NOW - 400, {P: REWARD}, **po(20), job="pending")  # queued
        c.block(502, NOW - 300, {P: REWARD}, **po(300), job="sent", confs=0)  # broadcast
        c.block(503, NOW - 200, {P: REWARD}, **po(4_000), job="sent", confs=3)  # paid
        c.block(504, NOW - 100, {P: REWARD}, **po(50_000), job="failed")  # failed
        e = c.earned(A)
        rows = {r["height"]: r["status"] for r in server.makegood_rows_for(A, tip=1000)}
        self.assertEqual(rows, {500: "owed", 501: "queued", 502: "broadcast", 503: "paid", 504: "failed"})
        self.assertEqual(e["makegood_sats"], 4_321, "owed + queued + broadcast + paid")
        self.assertEqual(e["makegood_blocks"], 4)
        self.assertEqual(e["makegood_failed_sats"], 50_000, "a failed make-good is reported beside the total")
        self.assertEqual(e["total_sats"], 4_321)

    def test_window_uses_the_block_time_for_both_lines(self):
        # primed logged the find a minute before the header time; the header time decides, so
        # the coinbase output and the make-good of one block are in or out together.
        c = Chain(self)
        split = [(A, 400_000), (A, 150_000), (P, REWARD - 550_000)]
        c.block(600, SINCE - 30, {A: 400_000, P: REWARD - 400_000}, kind="partial", split=split, owed=150_000)
        c.log[-1]["ts"] = SINCE + 30
        e = c.earned(A)
        self.assertEqual((e["block_sats"], e["makegood_sats"]), (0, 0))

    def test_mining_to_the_pool_address_leaves_the_fee_out(self):
        c = Chain(self)
        c.block(700, NOW - 50, {A: 1_000_000, P: REWARD - 1_000_000})
        c.log.append({"hash": h(700), "kind": "split", "split": [[A, 1_000_000], [P, 50_000]], "owed_sats": 0})
        e = c.earned(P)
        self.assertEqual(e["block_sats"], 50_000, "the pool wallet's own TIDES share, not the fee beside it")

    def test_unreadable_coinbase_is_null_not_zero(self):
        c = Chain(self)
        c.block(800, NOW - 3600, {A: 1_000_000, P: REWARD - 1_000_000})
        c.block(801, NOW - 60, None)
        self.assertIsNone(c.earned(A))

    def test_unreadable_make_good_record_is_null(self):
        c = Chain(self)
        c.block(810, NOW - 3600, {A: 1_000_000, P: REWARD - 1_000_000})
        # a pool-only block primed logged, not scanned by the site yet, coinbase unreadable
        c.block(811, NOW - 60, None, kind="pool-only", split=[(A, 9), (P, REWARD - 9)], owed=REWARD, found=False)
        self.assertIsNone(c.earned(A), "a make-good list known to be short is not summed")
        c2 = Chain(self)
        c2.block(820, NOW - 3600, {A: 1_000_000, P: REWARD - 1_000_000})
        with mock.patch.object(server, "BLOCKS_LOG", c2.dir / "missing.jsonl"):
            self.assertIsNone(server.earned_window(A, c2.fb_rows, [], now=NOW), "no blocks.jsonl: make-goods unknown")


class MinerPayload(unittest.TestCase):
    """The field as /api/miner/<addr> serves it, from the found_blocks table."""

    def setUp(self):
        self.chain = Chain(self)
        # Prime's live window for the address: not what is under test, and its table gains
        # columns in migrations a read-only (POOL_UI_NO_WRITE) import does not run.
        p = mock.patch.object(server, "prime_info_for", lambda address: {})
        p.start()
        self.addCleanup(p.stop)
        self.addCleanup(self.wipe)
        self.wipe()

    def wipe(self):
        with sqlite3.connect(os.environ["POOL_DB"]) as con:
            con.execute("DELETE FROM found_blocks")

    def found(self):
        now = int(time.time())
        c = self.chain
        c.block(900, now - 90_000, {A: 2_000_000, P: REWARD - 2_000_000})
        c.block(901, now - 3600, {A: 1_000_000, P: REWARD - 1_000_000})
        split = [(A, 700_000), (P, REWARD - 700_000)]
        c.block(902, now - 600, {P: REWARD}, kind="pool-only", split=split, owed=REWARD, job="pending")
        with (c.dir / "blocks.jsonl").open("w") as f:
            f.writelines(json.dumps(rec) + "\n" for rec in c.log)
        with sqlite3.connect(os.environ["POOL_DB"]) as con:
            con.executemany("INSERT INTO found_blocks(height,hash,ts) VALUES(:height,:hash,:ts)", c.fb_rows)
        return now

    def test_payload_carries_the_window(self):
        now = self.found()
        out = server.miner_payload(A)
        e = out["earned_24h"]
        self.assertEqual((e["block_sats"], e["makegood_sats"], e["total_sats"], e["blocks"]), (1_000_000, 700_000, 1_700_000, 1))
        self.assertAlmostEqual(e["since_ts"], now - 86400, delta=5)
        self.assertEqual(round(out["paid_btc"] * 1e8), 3_000_000, "lifetime figures still walk every block")
        json.dumps(out)

    def test_payload_is_null_when_splits_cannot_be_read(self):
        self.found()
        # The unreadable coinbase is the 25-hour-old one: the payout walk gave up on it
        # (`payouts is None`), so the page gets null rather than a partial answer.
        del self.chain.splits[h(900)]
        out = server.miner_payload(A)
        self.assertIn("earned_24h", out)
        self.assertIsNone(out["earned_24h"])

    def test_no_blocks_in_the_window_is_a_real_zero(self):
        with (self.chain.dir / "blocks.jsonl").open("w"):
            pass
        e = server.miner_payload(A)["earned_24h"]
        self.assertEqual((e["block_sats"], e["makegood_sats"], e["total_sats"], e["blocks"]), (0, 0, 0, 0))


class Page(unittest.TestCase):
    KEYS = ("earned24", "earned24Split", "earned24Note", "earned24Failed", "earned24Since", "earned24Unavailable", "earned24UnavailableSub")

    def test_strings_in_both_languages(self):
        js = (POOL / "static" / "shared.js").read_text(encoding="utf-8")
        for locale in ("en", "zh-CN"):
            miner = server._i18n_dict(locale)["miner"]
            for k in self.KEYS:
                self.assertTrue(miner.get(k), f"{locale} has no miner.{k}")
                self.assertIn(f'"miner.{k}"', js, f"shared.js never shows miner.{k}")
        en, zh = (server._i18n_dict(x)["miner"] for x in ("en", "zh-CN"))
        self.assertNotEqual(en["earned24Unavailable"], zh["earned24Unavailable"])
        self.assertNotIn("0", en["earned24Unavailable"] + zh["earned24Unavailable"], "unavailable is a word, not a zero")


if __name__ == "__main__":
    unittest.main(verbosity=2)
