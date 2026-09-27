#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Unit tests for the A2 prototype: tag parsing on real coinbases, builder rule, score math,
the LZT1 verifier (against the Rust fixture), and the entropy detector on synthetic templates."""
import json
import os
import random
import unittest

import entropy
import sovereignty as s

HERE = os.path.dirname(os.path.abspath(__file__))
ALPHA = {"name": "AlphaPool", "slug": "alphapool", "regexes": ["AlphaPool", "DATUM-AP(?![A-Za-z0-9])"]}
POWRE = {"name": "Pow.re", "slug": "powre", "regexes": ["Pow.re", "pow.re"]}
XOR = {"name": "Bitcoin Xor", "slug": "bitcoinxor", "regexes": ["Bitcoin Xor", "xorpool.com"]}


def blk(pool, script_sig):
    return {"pool": pool["slug"], "pool_name": pool["name"], "script_sig": script_sig}


def sig_for(primary, secondary):
    t = primary.encode() + (b"\x0f" + secondary.encode() + b"\x00" if secondary else b"\x00")
    return (b"\x03\xbf\xdc\x0e" + bytes([len(t)]) + t + b"\x07" + b"\x00" * 7).hex()


class Tags(unittest.TestCase):
    def test_real_alphapool_gateway_coinbase(self):
        # block 974015 as the explorer serves it
        raw = "03bfdc0e18416c706861506f6f6c0f5377656174792042656176657200070e92100150de710e9d81000000000000000000000000"
        self.assertEqual(s.tags(raw), ("AlphaPool", "Sweaty Beaver"))
        pools = {"alphapool": ALPHA}
        self.assertEqual(s.builder(blk(ALPHA, raw), pools), ("gateway", "alphapool/Sweaty Beaver"))

    def test_own_tag_rule(self):
        pools = {"alphapool": ALPHA, "powre": POWRE, "bitcoinxor": XOR}
        for pool, prim, sec in ((ALPHA, "AlphaPool", "AlphaPool"), (ALPHA, "DATUM-AP", "DATUM-AP"),
                                (POWRE, "Pow.re", "buy hashrate @ pow.re"), (XOR, "Bitcoin Xor", "xorpool.com"),
                                (ALPHA, "AlphaPool", "")):
            self.assertEqual(s.builder(blk(pool, sig_for(prim, sec)), pools)[0], "pool", (prim, sec))
        self.assertEqual(s.builder(blk(XOR, sig_for("Bitcoin Xor", "LPF")), pools)[0], "gateway")

    def test_generic_datum_names_are_not_own_names(self):
        dm = {"name": "DATUM Miners", "slug": "datumminers", "regexes": ["DATUM"]}
        self.assertEqual(s.builder(blk(dm, sig_for("DATUM", "DATUM User")), {"datumminers": dm})[0], "gateway")

    def test_utf8_gateway_name(self):
        raw = sig_for("Lazarus", "💰Pirate Lounge 🏴‍☠️")
        self.assertEqual(s.tags(raw)[1], "💰Pirate Lounge 🏴‍☠️")


class Scores(unittest.TestCase):
    def test_score_weights(self):
        self.assertEqual(s.sovereignty_score(100, 0, 0), 0)
        self.assertEqual(s.sovereignty_score(100, 100, 0), 50)
        self.assertEqual(s.sovereignty_score(100, 100, 100), 100)
        self.assertEqual(s.sovereignty_score(100, 60, 20), 40)

    def test_concentration(self):
        self.assertEqual(s.nakamoto([50, 30, 20]), 2)
        self.assertEqual(s.nakamoto([51, 49]), 1)
        self.assertAlmostEqual(s.hhi_effective([1, 1, 1, 1]), 4)
        self.assertAlmostEqual(s.hhi_effective([10]), 1)

    def test_score_over_synthetic_chain(self):
        blocks = {}
        for h in range(10):
            sec = "" if h < 4 else ("Tofu Toes" if h < 8 else "AlphaPool")
            blocks[h] = dict(blk(ALPHA, sig_for("AlphaPool", sec)), height=h, hash=f"{h:064x}", time=1000 + h,
                             prev="00" * 32, match_rate=None)
        net, rows, _ = s.score(blocks, {"alphapool": ALPHA}, {}, {}, days=1)
        self.assertEqual((rows[0]["gateway_built"], rows[0]["pool_built"]), (4, 6))
        self.assertEqual(rows[0]["score"], 20.0)
        self.assertEqual(net["template_nakamoto"], 1)


class Lzt1(unittest.TestCase):
    def test_rust_fixture_and_every_tamper(self):
        path = os.path.join(HERE, "fixture.json")
        if not os.path.exists(path):
            self.skipTest("run demo.sh to make fixture.json from rnd/a2")
        res = s.verify_fixture(path)
        self.assertTrue(all(r[3] for r in res), [r for r in res if not r[3]])
        self.assertGreaterEqual(len(res), 14)

    def test_branches_match_merkle_root(self):
        txids = [bytes([i]) * 32 for i in range(1, 8)]
        cb = b"\xab" * 32
        root = cb
        for b in s.branches_for_coinbase(txids):
            root = s.dsha(root + b)
        level = [cb] + txids
        while len(level) > 1:
            if len(level) % 2:
                level.append(level[-1])
            level = [s.dsha(level[i] + level[i + 1]) for i in range(0, len(level), 2)]
        self.assertEqual(root, level[0])


def synth(n=200, seed=1):
    """Pool node templates, an honest node with its own relay view, a proxy and a mimic."""
    rng = random.Random(seed)
    pool, honest, proxy, mimic = [], [], [], []
    mp_pool, mp_h, prev = [], [], "aa"
    for i in range(n):
        t = i * 0.5
        if i % 30 == 0:
            prev = f"{i:064x}"
            mp_pool, mp_h = [], []
        for _ in range(rng.randint(0, 2)):
            tx = f"{rng.getrandbits(256):064x}"
            # each tx reaches one node first, the other a few polls later
            (mp_pool if rng.random() < 0.5 else mp_h).append(tx)
        # it reaches the other node a few polls later
        for tx in list(mp_pool[-3:]):
            if tx not in mp_h and rng.random() < 0.4:
                mp_h.append(tx)
        for tx in list(mp_h[-3:]):
            if tx not in mp_pool and rng.random() < 0.4:
                mp_pool.append(tx)
        pool.append({"t": t, "prev": prev, "txids": list(mp_pool)})
        honest.append({"t": t + 0.01, "prev": prev, "txids": list(mp_h)})
        proxy.append({"t": t + 0.05, "prev": prev, "txids": list(mp_pool)})
        m = list(mp_pool)
        rng.shuffle(m)
        mimic.append({"t": t + 0.8, "prev": prev, "txids": m})
    return pool, honest, proxy, mimic


class Entropy(unittest.TestCase):
    def test_separates_honest_from_copies(self):
        pool, honest, proxy, mimic = synth()
        f = {k: entropy.features(v, pool) for k, v in (("honest", honest), ("proxy", proxy), ("mimic", mimic))}
        self.assertLess(f["proxy"]["independence"], 0.05)
        self.assertLess(f["mimic"]["independence"], 0.2)
        self.assertGreater(f["honest"]["independence"], 0.8)
        self.assertEqual(f["proxy"]["lead"], 0)

    def test_unknown_earns_nothing(self):
        pool, honest, _, _ = synth(n=10)
        f = entropy.features(honest, pool)
        self.assertTrue(f.get("insufficient"))
        self.assertEqual(f["independence"], 0.0)

    def test_clusters_group_the_proxy_with_the_pool(self):
        pool, honest, proxy, _ = synth()
        cl = entropy.clusters({"POOL": pool, "H": honest, "X": proxy})
        self.assertIn(["POOL", "X"], [sorted(c) for c in cl])

    def test_branch_similarity(self):
        a = entropy.branches(["11" * 32, "22" * 32, "33" * 32])
        self.assertEqual(entropy.branch_sim(a, a), 1.0)
        b = entropy.branches(["11" * 32, "22" * 32, "44" * 32])
        self.assertLess(entropy.branch_sim(a, b), 1.0)
        self.assertGreater(entropy.branch_sim(a, b), 0.0)


if __name__ == "__main__":
    unittest.main()
