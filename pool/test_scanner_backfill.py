#!/usr/bin/env python3
"""The found-block scanner holds a tagged block primed's log lacks, and the one-off backfill.

    python3 pool/test_scanner_backfill.py

Imports server.py the way test_earned_24h.py does, but with writes on: the scanner under test
writes. The database is a throwaway, primed's log is a real blocks.jsonl in a temp directory,
and only the node is stubbed (a dict of blocks answering getblockcount/getblockhash/getblock).
"""

import contextlib
import io
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
TMP = Path(tempfile.mkdtemp(prefix="scanner-"))
os.environ["POOL_DB"] = str(TMP / "pool.sqlite")
os.environ.pop("POOL_UI_NO_WRITE", None)
sys.path.insert(0, str(POOL))
sys.path.insert(0, str(POOL / "tools"))
try:
    import server
finally:
    if _made_cfg:
        _cfg.unlink()
import backfill_found_blocks as backfill

A = "bc1qminera"
B = "bc1qminerb"
P = "bc1qpool"
REWARD = 312_500_000
TABLES = (
    "found_blocks",
    "solo_blocks",
    "meta",
    "scan_pending",
    "rounds",
    "round_payouts",
    "round_work",
)


def bh(n, fork=0):
    return f"{n:062x}{fork:02x}"


def coinbase(tag):
    return (b"\x03\x54\xe2\x0e\x07" + tag.encode()).hex() if tag else "0354e20e"


class Node:
    """A chain the tests extend and reorg, a primed block log, and a clean database."""

    def __init__(self, case, tip):
        self.blocks = {}  # hash -> block, as getblock verbosity 2 returns it
        self.main = {}  # height -> hash
        self.down = ()  # RPC methods the node does not answer; True for all of them
        self.log = Path(tempfile.mkdtemp(prefix="node-", dir=TMP)) / "blocks.jsonl"
        self.log.write_text("")
        for t in TABLES:
            server.db(f"DELETE FROM {t}", write=True)
        server.db(
            "INSERT INTO meta(key,value) VALUES('scan_height',?)",
            (str(tip),),
            write=True,
        )
        server.ensure_open_round()
        for name, value in {
            "rpc": self.rpc,
            "BLOCKS_LOG": self.log,
            "_block_log_latest_cache": {"sig": None, "latest": {}},
        }.items():
            p = mock.patch.object(server, name, value)
            p.start()
            case.addCleanup(p.stop)
        self.out = io.StringIO()
        redirect = contextlib.redirect_stdout(self.out)
        redirect.__enter__()
        case.addCleanup(redirect.__exit__, None, None, None)
        for height in range(tip - 2, tip + 1):
            self.mine(height)

    def rpc(self, method, params=None):
        if self.down is True or method in self.down:
            return None
        if method == "getblockcount":
            return max(self.main)
        if method == "getblockhash":
            return self.main.get(params[0])
        if method == "getblock":
            return self.blocks.get(params[0])
        raise AssertionError(method)

    def mine(self, height, tag="", pays=None, fork=0):
        """Put a block at `height` on the main chain (replacing what was there: a reorg)."""
        pays = pays or {P: REWARD}
        vout = [
            {"value": sats / 1e8, "scriptPubKey": {"address": a}}
            for a, sats in pays.items()
        ]
        h = bh(height, fork)
        self.blocks[h] = {
            "hash": h,
            "time": 1_790_000_000 + height,
            "tx": [{"vin": [{"coinbase": coinbase(tag)}], "vout": vout}],
        }
        self.main[height] = h
        return h

    def prime_logs(self, height, h, kind="split"):
        with self.log.open("a") as f:
            f.write(
                json.dumps(
                    {
                        "ts": 1_790_000_000 + height,
                        "height": height,
                        "hash": h,
                        "kind": kind,
                    }
                )
                + "\n"
            )

    def work(self, **by_address):
        for addr, w in by_address.items():
            server.db(
                "INSERT OR REPLACE INTO round_work(address,work,last_diff_acc) VALUES(?,?,0)",
                (addr, w),
                write=True,
            )

    @staticmethod
    def found():
        return {
            r["height"]: r["hash"]
            for r in server.db("SELECT height, hash FROM found_blocks")
        }

    @staticmethod
    def pending():
        return {
            r["height"]: r["hash"]
            for r in server.db("SELECT height, hash FROM scan_pending")
        }

    @staticmethod
    def rounds():
        return [
            (r["height"], r["status"], r["total_work"])
            for r in server.db("SELECT * FROM rounds ORDER BY id")
        ]

    @staticmethod
    def round_work():
        return {
            r["address"]: r["work"]
            for r in server.db("SELECT address, work FROM round_work")
        }


SPLIT = {A: 200_000_000, B: 100_000_000, P: 12_500_000}


class ScannerHoldsTaggedBlocks(unittest.TestCase):
    def test_block_logged_a_few_scans_later_is_inserted(self):
        n = Node(self, 1000)
        n.work(**{A: 70.0, B: 30.0})
        h = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()  # the node has the block; primed's log does not, yet
        self.assertEqual(n.found(), {})
        self.assertEqual(n.pending(), {1001: h})
        n.mine(1002)
        server.scan_found_blocks()  # the scan moves on, the block stays held
        self.assertEqual(
            server.db("SELECT value FROM meta WHERE key='scan_height'", one=True)[
                "value"
            ],
            "1002",
        )
        self.assertEqual(n.pending(), {1001: h})
        self.assertEqual(
            n.round_work(), {A: 70.0, B: 30.0}, "a held block closes nothing"
        )
        n.prime_logs(1001, h)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: h})
        self.assertEqual(n.pending(), {})
        # No pool block came after it, so it ends the open round the way an on-time block does.
        self.assertEqual(n.rounds(), [(1001, "immature", 100.0), (None, "open", 0.0)])
        self.assertEqual(n.round_work(), {})
        paid = {
            r["address"]: round(r["amount_btc"] * 1e8)
            for r in server.db("SELECT * FROM round_payouts")
        }
        self.assertEqual(paid, SPLIT)

    def test_block_already_in_the_log_is_inserted_at_once(self):
        n = Node(self, 1000)
        h = n.mine(1001, "Lazarus", SPLIT)
        n.prime_logs(1001, h)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: h})
        self.assertEqual(n.pending(), {})

    def test_late_block_does_not_take_the_round_a_later_block_opened(self):
        n = Node(self, 1000)
        n.work(**{A: 70.0})
        late = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()
        on_time = n.mine(1002, "Lazarus", SPLIT)
        n.prime_logs(1002, on_time)
        server.scan_found_blocks()  # 1002 closes the round
        n.work(**{A: 5.0, B: 5.0})  # work on the round 1002 opened
        n.prime_logs(1001, late)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: late, 1002: on_time})
        self.assertEqual(
            n.rounds(),
            [(1002, "immature", 70.0), (None, "open", 0.0), (1001, "immature", 0.0)],
        )
        self.assertEqual(
            n.round_work(), {A: 5.0, B: 5.0}, "the open round keeps its work"
        )
        late_round = server.db("SELECT id FROM rounds WHERE height=1001", one=True)[
            "id"
        ]
        paid = {
            r["address"]: (round(r["amount_btc"] * 1e8), r["work"])
            for r in server.db(
                "SELECT * FROM round_payouts WHERE round_id=?", (late_round,)
            )
        }
        self.assertEqual(paid, {a: (s, 0.0) for a, s in SPLIT.items()})

    def test_bound_reached_gives_up_loudly(self):
        n = Node(self, 1000)
        h = n.mine(1001, "Lazarus", SPLIT)  # a forged tag: primed never logs it
        server.scan_found_blocks()
        for height in range(1002, 1001 + server.SCAN_PENDING_BLOCKS):
            n.mine(height)
        while int(
            server.db("SELECT value FROM meta WHERE key='scan_height'", one=True)[
                "value"
            ]
        ) < max(n.main):
            server.scan_found_blocks()
        self.assertEqual(
            n.pending(), {1001: h}, "one block short of the bound it is still held"
        )
        self.assertNotIn("ERROR", n.out.getvalue())
        n.mine(1001 + server.SCAN_PENDING_BLOCKS)
        server.scan_found_blocks()
        self.assertEqual(n.pending(), {})
        self.assertEqual(n.found(), {})
        self.assertIn(
            f"ERROR tagged_block_never_in_prime_log 1001 {h}", n.out.getvalue()
        )

    def test_rescans_do_not_insert_twice(self):
        n = Node(self, 1000)
        n.work(**{A: 70.0})
        h = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()
        n.prime_logs(1001, h)
        for _ in range(3):
            server.scan_found_blocks()
        n.work(**{B: 9.0})
        # the same height scanned again from the top (a scan_height rewound by hand)
        server.db("UPDATE meta SET value='1000' WHERE key='scan_height'", write=True)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: h})
        self.assertEqual(n.rounds(), [(1001, "immature", 70.0), (None, "open", 0.0)])
        self.assertEqual(n.round_work(), {B: 9.0})
        self.assertEqual(
            server.db("SELECT COUNT(*) AS n FROM round_payouts", one=True)["n"],
            len(SPLIT),
        )

    def test_reorged_out_block_is_not_inserted(self):
        n = Node(self, 1000)
        h = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()
        self.assertEqual(n.pending(), {1001: h})
        n.mine(1001, fork=1)  # another miner's block took the height
        n.mine(1002)
        n.prime_logs(1001, h)  # primed logs the share for the block that lost
        server.scan_found_blocks()
        self.assertEqual(n.found(), {})
        self.assertEqual(n.pending(), {})
        self.assertEqual(len(n.rounds()), 1, "only the open round")

    def test_reorg_to_another_pool_block_holds_the_new_hash(self):
        n = Node(self, 1000)
        old = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()
        new = n.mine(1001, "Lazarus", SPLIT, fork=1)
        n.prime_logs(1001, old)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {})
        self.assertEqual(n.pending(), {1001: new})
        n.prime_logs(1001, new)
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: new})

    def test_node_not_answering_keeps_the_block_held(self):
        n = Node(self, 1000)
        h = n.mine(1001, "Lazarus", SPLIT)
        server.scan_found_blocks()
        n.prime_logs(1001, h)
        for down in (True, ("getblockhash",), ("getblock",)):
            n.down = down
            server.scan_found_blocks()
            self.assertEqual(n.pending(), {1001: h}, down)
            self.assertEqual(n.found(), {}, down)
        n.down = ()
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: h})

    def test_pool_only_block_keeps_the_round_work(self):
        n = Node(self, 1000)
        n.work(**{A: 70.0})
        h = n.mine(1001, "Lazarus", {P: REWARD})
        server.scan_found_blocks()
        n.prime_logs(1001, h, kind="pool-only")
        server.scan_found_blocks()
        self.assertEqual(n.found(), {1001: h})
        self.assertEqual(n.round_work(), {A: 70.0})
        self.assertEqual(len(n.rounds()), 1)


class Backfill(unittest.TestCase):
    def chain(self):
        """Blocks 1001..1006 already scanned; found_blocks has only 1005."""
        n = Node(self, 1000)
        self.skipped_split = n.mine(
            1001, "Lazarus", SPLIT
        )  # logged late, scanner long gone
        self.skipped_pool_only = n.mine(1002, "Lazarus", {P: REWARD})
        n.mine(
            1003, "Lazarus", SPLIT, fork=1
        )  # the chain's 1003; primed logged the loser
        self.forged = n.mine(1004, "Lazarus", SPLIT)  # tagged, never in primed's log
        self.booked = n.mine(1005, "Lazarus", SPLIT)
        n.mine(1006, "Lazarus/solo", {A: REWARD - 1, P: 1})
        n.prime_logs(1001, self.skipped_split)
        n.prime_logs(1002, self.skipped_pool_only, kind="pool-only")
        n.prime_logs(1003, bh(1003), kind="orphan:split")
        n.prime_logs(1005, self.booked)
        n.prime_logs(1006, bh(1006))
        server.db("UPDATE meta SET value='1006' WHERE key='scan_height'", write=True)
        server.insert_found_block(
            1005,
            self.booked,
            n.blocks[self.booked],
            n.blocks[self.booked]["tx"][0]["vout"],
            "Lazarus",
        )
        server.close_round_for_block(
            1005, self.booked, 3.125, 0.0, 3.125, n.blocks[self.booked]["tx"][0]["vout"]
        )
        n.work(**{A: 5.0, B: 5.0})
        return n

    def test_plan_lists_exactly_the_skipped_pool_blocks(self):
        n = self.chain()
        blocks, skipped = backfill.plan(server)
        self.assertEqual(
            [(b.height, b.hash, b.source, b.outputs, b.reward_sats) for b in blocks],
            [
                (1001, self.skipped_split, "prime-log", 3, REWARD),
                (1002, self.skipped_pool_only, "prime-log", 1, REWARD),
            ],
        )
        self.assertEqual(
            [(s.height, s.why) for s in skipped],
            [(1006, "a solo block (those live in solo_blocks)")],
        )
        self.assertEqual(n.found(), {1005: self.booked}, "planning writes nothing")

    def test_apply_adds_rows_and_is_idempotent(self):
        n = self.chain()
        before_rounds = n.rounds()
        blocks, _ = backfill.plan(server)
        self.assertEqual(backfill.apply(server, blocks), 2)
        self.assertEqual(
            n.found(),
            {1001: self.skipped_split, 1002: self.skipped_pool_only, 1005: self.booked},
        )
        # The open round and its work are untouched; the split block got a round of its own.
        self.assertEqual(n.round_work(), {A: 5.0, B: 5.0})
        self.assertEqual(n.rounds(), before_rounds + [(1001, "immature", 0.0)])
        rid = server.db("SELECT id FROM rounds WHERE height=1001", one=True)["id"]
        paid = {
            r["address"]: round(r["amount_btc"] * 1e8)
            for r in server.db("SELECT * FROM round_payouts WHERE round_id=?", (rid,))
        }
        self.assertEqual(paid, SPLIT)
        row = server.db("SELECT * FROM found_blocks WHERE height=1001", one=True)
        self.assertEqual(
            (row["ts"], round(row["reward_btc"] * 1e8), row["finder"]),
            (1_790_001_001, REWARD, A),
        )
        snapshot = (
            n.found(),
            n.rounds(),
            server.db("SELECT COUNT(*) AS n FROM round_payouts", one=True)["n"],
        )
        again, _ = backfill.plan(server)
        self.assertEqual(again, [])
        self.assertEqual(
            backfill.apply(server, blocks),
            0,
            "the same plan applied twice adds nothing",
        )
        self.assertEqual(
            (
                n.found(),
                n.rounds(),
                server.db("SELECT COUNT(*) AS n FROM round_payouts", one=True)["n"],
            ),
            snapshot,
        )

    def test_also_adds_a_tagged_block_the_log_lacks_and_refuses_an_untagged_one(self):
        self.chain()
        blocks, skipped = backfill.plan(server, also=[1004, 999])
        self.assertIn(
            (1004, self.forged, "operator"),
            [(b.height, b.hash, b.source) for b in blocks],
        )
        self.assertIn(
            (999, "coinbase does not carry the 'Lazarus' tag"),
            [(s.height, s.why) for s in skipped],
        )
        self.assertNotIn(999, [b.height for b in blocks])

    def test_heights_the_scanner_still_owns_are_left_to_it(self):
        n = self.chain()
        held = n.mine(1007, "Lazarus", SPLIT)
        server.scan_found_blocks()  # holds 1007
        n.prime_logs(1007, held)
        ahead = n.mine(1008, "Lazarus", SPLIT)
        n.prime_logs(1008, ahead)
        blocks, skipped = backfill.plan(server)
        self.assertEqual([b.height for b in blocks], [1001, 1002])
        why = {s.height: s.why for s in skipped}
        self.assertEqual(why[1007], why[1008])
        self.assertIn("scanner", why[1007])

    def test_unreadable_source_stops_instead_of_listing_less(self):
        n = self.chain()
        for down in (True, ("getblockhash",), ("getblock",)):
            n.down = down
            with self.assertRaises(backfill.Incomplete, msg=down):
                backfill.plan(server)
        n.down = ()
        n.log.unlink()
        with self.assertRaises(backfill.Incomplete):
            backfill.plan(server)

    def test_dry_run_cannot_write(self):
        n = self.chain()
        blocks, _ = backfill.plan(server)
        with mock.patch.object(
            server, "NO_WRITE", True
        ):  # what main() sets without --apply
            backfill.apply(server, blocks)
        self.assertEqual(n.found(), {1005: self.booked})


if __name__ == "__main__":
    unittest.main()
