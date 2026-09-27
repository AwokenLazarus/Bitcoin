#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Derive XBT-NTA test vectors from the XBT-065 functional test and check them on a real node.

Reuses the helpers of feature_xbt_nta.py (Knots 5164677): same keys, same digest, same
signer (BIP340, aux = 32 zero bytes, so signatures are deterministic). Every vector is
submitted to an enforcing regtest node and its verdict is recorded, so the JSON is what
the C++ rule actually did, not what the Python thinks it should do.

The setup blocks carry wall-clock times, so every run has a new hashPrevBlock. With
--keep FILE, vectors already in FILE (same name, same node verdict) are copied from it
byte for byte and only new ones are added; the BIP quotes the kept ones.

x10 (over the payee cap) needs the capped node (Knots v29.4.2.knots20260508 plus all three
patches in ../patches/knots-v29.4.2/): KNOTS_DIR selects the Knots tree whose
feature_xbt_nta.py and build are used.

Run (ports NTA_PORT_BASE..+59, datadirs under run/):
    KNOTS_DIR=/path/to/knots NTA_PORT_BASE=32700 python3 vectors/nta_vectors.py \
        --configfile /path/to/knots/build/test/config.ini \
        --tmpdir "$PWD/run/vectors" --out vectors/nta-vectors.json
"""

import json
import os
import sys

KNOTS = os.path.expanduser(os.environ.get("KNOTS_DIR", "knots"))
sys.path.insert(0, os.path.join(KNOTS, "test", "functional"))

_out = "vectors/nta-vectors.json"
if "--out" in sys.argv:
    i = sys.argv.index("--out")
    _out = sys.argv[i + 1]
    del sys.argv[i:i + 2]
_keep = None
if "--keep" in sys.argv:
    i = sys.argv.index("--keep")
    _keep = sys.argv[i + 1]
    del sys.argv[i:i + 2]

import feature_xbt_nta as F  # noqa: E402  (applies NTA_PORT_BASE on import)
from test_framework.messages import COIN, ser_string, ser_uint256  # noqa: E402
from test_framework.script import CScript, OP_RETURN  # noqa: E402
from test_framework.util import assert_equal  # noqa: E402
import struct  # noqa: E402


def preimage(script, height, nbits, prev):
    return ser_string(bytes(script)) + struct.pack("<i", height) + struct.pack("<I", nbits) + ser_uint256(prev)


class NtaVectors(F.XbtNtaTest):
    def set_test_params(self):
        super().set_test_params()
        self.num_nodes = 1
        self.extra_args = self.extra_args[:1]

    def run_test(self):
        enf = self.nodes[0]
        alice_sk, alice = F.keypair(b"alice")
        bob_sk, bob = F.keypair(b"bob")
        pool_sk, _ = F.keypair(b"pool")
        _, cust = F.keypair(b"customer")
        sub = 50 * COIN
        vectors = []

        self.generatetodescriptor(enf, F.NTA_HEIGHT - 1, "raw(51)", sync_fun=self.no_op)
        assert enf.getdeploymentinfo()["deployments"]["nta"]["active"]

        def record(name, t, outs, block, expect, payees):
            got = self.submit(enf, block)
            assert_equal(got, expect)
            carried = [bytes(sc)[6:] for sc, _ in outs if len(bytes(sc)) == 70 and bytes(sc)[:6] == bytes([0x6a, 0x44]) + F.NTA_PREFIX]
            atts = []
            for i, (script, sk) in enumerate(payees):
                digest = F.att_hash(script, t.height, t.nbits, t.prev)
                atts.append({
                    "payee_script": bytes(script).hex(),
                    "payee_key": bytes(script)[2:].hex(),
                    "signer_seckey": sk.hex(),
                    "preimage": preimage(script, t.height, t.nbits, t.prev).hex(),
                    "digest": digest.hex(),
                    "carried_sig": carried[i].hex() if i < len(carried) else None,
                })
            vectors.append({
                "name": name,
                "height": t.height,
                "nBits": f"{t.nbits:08x}",
                "hashPrevBlock": f"{t.prev:064x}",
                "coinbase_outputs": [{"value": v, "scriptPubKey": bytes(s).hex()} for s, v in outs],
                "attestations": atts,
                "coinbase_tx": block.vtx[0].serialize().hex(),
                "result": "valid" if expect is None else expect,
            })

        # Negative vectors first, all on the same parent, so they share height/nBits/prev.
        t = F.Tmpl(enf)
        a, b, c = F.p2tr(alice), F.p2tr(bob), F.p2tr(cust)

        outs = [(c, sub), (F.att_script(t.sign(c, pool_sk)), 0)]
        record("x03 gifted: pool key signs the customer's payee digest", t, outs, t.block(outs), "bad-nta-sig",
               [(c, pool_sk)])

        outs = [(a, sub // 2), (b, sub // 2),
                (F.att_script(t.sign(b, bob_sk)), 0), (F.att_script(t.sign(a, alice_sk)), 0)]
        record("x05 attestations in the wrong payee order", t, outs, t.block(outs), "bad-nta-sig",
               [(a, alice_sk), (b, bob_sk)])

        sig = t.sign(a, alice_sk)
        outs = [(a, sub), (CScript([OP_RETURN, F.NTA_PREFIX + sig[:63]]), 0)]
        record("x09 truncated attestation is an ordinary OP_RETURN", t, outs, t.block(outs), "bad-nta-count",
               [(a, alice_sk)])

        cap = F.cap_payees(F.MAX_NTA_PAYEES + 1)
        outs = [(script, COIN // 100) for script, _ in cap] + [(F.att_script(t.sign(script, sk)), 0) for script, sk in cap]
        record(f"x10 {F.MAX_NTA_PAYEES + 1} distinct payees, all validly attested: over the cap", t, outs,
               t.block(outs), "bad-nta-too-many", cap)

        outs = [(a, sub), (F.att_script(sig), 0)]
        record("x01 one signed payee (valid)", t, outs, t.block(outs), None, [(a, alice_sk)])

        t = F.Tmpl(enf)
        outs = [(a, sub // 4), (b, sub // 2), (a, sub // 4),
                (F.att_script(t.sign(a, alice_sk)), 0), (F.att_script(t.sign(b, bob_sk)), 0)]
        record("x02 split, repeated script shares one attestation (valid)", t, outs, t.block(outs), None,
               [(a, alice_sk), (b, bob_sk)])
        assert_equal(enf.getblockcount(), F.NTA_HEIGHT + 1)

        if _keep:
            kept = {v["name"]: v for v in json.load(open(_keep))["vectors"]}
            for i, v in enumerate(vectors):
                if v["name"] in kept:
                    assert_equal(kept[v["name"]]["result"], v["result"])  # this node agrees
                    vectors[i] = kept[v["name"]]
        with open(_out, "w") as f:
            json.dump({"source": "Knots feature_xbt_nta.py helpers (5164677, cap from rnd/sov-014); "
                                 "verdicts from a regtest node",
                       "tag": "XBT-NTA/attestation",
                       "max_payees": F.MAX_NTA_PAYEES,
                       "attestation_prefix": F.NTA_PREFIX.hex(),
                       "vectors": vectors}, f, indent=2)
        self.log.info(f"wrote {len(vectors)} vectors to {_out}")


if __name__ == "__main__":
    NtaVectors(__file__).main()
