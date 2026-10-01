#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Time Knots TestBlockValidity (submitblock) on NTA blocks.

Uses the Knots functional test's helpers (feature_xbt_nta.py: Tmpl, cap_payees,
att_script). One regtest node from KNOTS_DIR. Ports PORT_BASE..+59 (default
32640), which stays off 32300–32599.

Timed region is submitblock only. Python BIP340 signing (the test framework's
sign_schnorr) builds the blocks and is not included. submitblock runs
CheckBlock, ContextualCheckBlock (where the NTA rule lives) and, for an accepted
block, ConnectBlock of a coinbase-only block. That makes the 512-payee time an
upper bound on the consensus check.

Sizes: 1, 100, 512 attested payees (accepted) and 513 (bad-nta-too-many, before
any signature). NTA_BENCH_RUNS each (default 5; bench.sh --quick sets 1). The
513 blocks differ in nTime so the invalid-block cache cannot skip the check;
the attestation hash does not cover nTime.
"""

from __future__ import annotations

import json
import os
import sys
import time

if "KNOTS_DIR" not in os.environ:
    sys.exit("bench_blocks: set KNOTS_DIR to a Knots tree with the NTA patches")

_port = os.environ.get("PORT_BASE") or os.environ.get("NTA_PORT_BASE") or "32640"
os.environ["PORT_BASE"] = _port
os.environ["NTA_PORT_BASE"] = _port

KNOTS = os.environ["KNOTS_DIR"]
sys.path.insert(0, os.path.join(KNOTS, "test", "functional"))

import feature_xbt_nta as F  # noqa: E402
from test_framework.messages import COIN  # noqa: E402
from test_framework.test_framework import BitcoinTestFramework  # noqa: E402
from test_framework.util import assert_equal  # noqa: E402

OUT = os.environ.get("NTA_BLOCKS_JSON", "results/blocks.json")
VALUE = COIN // 10_000  # 513 of these is well under the 50 BTC subsidy


def run_count() -> int:
    raw = os.environ.get("NTA_BENCH_RUNS", "5")
    try:
        n = int(raw)
    except ValueError:
        sys.exit(f"bench_blocks: NTA_BENCH_RUNS must be an integer, got {raw!r}")
    if n < 1:
        sys.exit(f"bench_blocks: NTA_BENCH_RUNS must be >= 1, got {raw!r}")
    return n


RUNS = run_count()


class NtaBlockBench(BitcoinTestFramework):
    def set_test_params(self) -> None:
        self.num_nodes = 1
        self.setup_clean_chain = True
        self.extra_args = [
            [
                f"-testactivationheight=blake2b@{F.BLAKE2B_HEIGHT}",
                f"-testactivationheight=nta@{F.NTA_HEIGHT}",
                "-rdtsexpiry=4102444800",
            ]
        ]

    def setup_network(self) -> None:
        self.setup_nodes()

    def run_test(self) -> None:
        node = self.nodes[0]
        self.generatetodescriptor(
            node, F.NTA_HEIGHT - 1, "raw(51)", sync_fun=self.no_op
        )
        assert node.getdeploymentinfo()["deployments"]["nta"]["active"]

        payees = F.cap_payees(F.MAX_NTA_PAYEES + 1)
        # Warm the process and the accept path before the timed runs.
        warm, _meta = self._block(node, payees[:1])
        got, _ns = self._time_submit(node, warm)
        assert_equal(got, None)

        series = []
        for n in (1, 100, 512):
            samples = []
            for _ in range(RUNS):
                block, meta = self._block(node, payees[:n])
                got, ns = self._time_submit(node, block)
                assert_equal(got, None)
                assert_equal(node.getbestblockhash(), block.hash)
                samples.append({"ns": ns, "result": "valid", **meta})
            series.append({"payees": n, "expect": "valid", "samples": samples})

        # 513: same parent, distinct headers. Sign once; nTime is not in the digest.
        t = F.Tmpl(node)
        chosen = payees[: F.MAX_NTA_PAYEES + 1]
        outs = [(script, VALUE) for script, _sk in chosen]
        outs += [(F.att_script(t.sign(script, sk)), 0) for script, sk in chosen]
        samples = []
        tip = node.getbestblockhash()
        for _i in range(RUNS):
            t.time += 1
            block = t.block(outs)
            got, ns = self._time_submit(node, block)
            assert_equal(got, "bad-nta-too-many")
            assert_equal(node.getbestblockhash(), tip)
            samples.append(
                {
                    "ns": ns,
                    "result": got,
                    "coinbase_bytes": len(block.vtx[0].serialize()),
                    "height": t.height,
                }
            )
        series.append(
            {
                "payees": F.MAX_NTA_PAYEES + 1,
                "expect": "bad-nta-too-many",
                "samples": samples,
            }
        )

        parent = os.path.dirname(OUT)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with open(OUT, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "op": "submitblock (CheckBlock + ContextualCheckBlock + ConnectBlock if accepted)",
                    "port_base": int(os.environ["PORT_BASE"]),
                    "runs_each": RUNS,
                    "note": (
                        "Python signing is outside the timer. Accepted blocks also "
                        "ConnectBlock (coinbase only), so the 512 time is an upper bound "
                        "on the consensus check. 513 is refused in the payee scan, before BIP340."
                    ),
                    "series": series,
                },
                f,
                indent=2,
            )
            f.write("\n")
        self.log.info(f"wrote {OUT}")

    def _block(self, node, payees):
        t = F.Tmpl(node)
        outs = [(script, VALUE) for script, _sk in payees]
        outs += [(F.att_script(t.sign(script, sk)), 0) for script, sk in payees]
        block = t.block(outs)
        return block, {
            "coinbase_bytes": len(block.vtx[0].serialize()),
            "height": t.height,
        }

    @staticmethod
    def _time_submit(node, block):
        raw = block.serialize().hex()
        t0 = time.perf_counter_ns()
        got = node.submitblock(raw)
        return got, time.perf_counter_ns() - t0


if __name__ == "__main__":
    NtaBlockBench(__file__).main()
