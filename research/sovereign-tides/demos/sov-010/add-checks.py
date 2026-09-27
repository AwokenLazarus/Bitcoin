#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Patch a copy of a pre-SOV-008 demo harness in place so that every job snapshot it posts is
posted as a template check too.

Since SOV-P-002 W8 (rnd/sov-008), primed reads canary hits only from template checks. Harnesses
with no DATUM session post their job lists to the demo-only `/sovereignty/check` route instead,
as sov-008 did for the in-repo A2 feeders (prime/scripts/rnd-a2-feed.py). A snapshot is the
gateway's job list at time t, which is exactly what a check of that job returns. Primed keeps 256
checks per gateway (MAX_CHECKS), sized for W8's real cadence (one check ~45 s after a canary, at
most one per 300 s, plus the 600 s regular ones); these harnesses poll every 0.25 s, so a check
per snapshot would evict each canary's 12-180 s window within a minute. A gateway's snapshots are
therefore posted as checks at most every SOV010_CHECK_EVERY s (default 5: ~20 min of history).

The same W8 detector judges a canary only by jobs first seen 12-180 s after its delivery, so a
harness that mines a block every 8-14 s has an honest node mine each canary before it is due, and
reads as a miss. sov-008 moved its own harness (prime/scripts/rnd-a2-knots.py) to 30-50 s blocks;
the SOV-004 harness's hard-coded `rng.uniform(8, 14)` block gap becomes SOV004_BLOCK_MIN..MAX
(default 30..50) the same way. (XBT-071's demo already reads XBT071_GAP_MIN/MAX.)

    add-checks.py FILE.py      # exits 1 if FILE.py posts no snapshots
"""
import re
import sys

SNAP = re.compile(r'post\("/sovereignty/snapshot", \{"gateway": ([^,{}]+), \*\*(\w+)\}\)')


def patch(m: re.Match) -> str:
    gw, s = m.group(1), m.group(2)
    return (f'(post("/sovereignty/snapshot", {{"gateway": {gw}, **{s}}}), '
            f'post_check({gw}, {s}))')


HELPER = '''
_SOV010_LAST_CHECK = {}


def post_check(gw, s):
    """SOV-010: the gateway's job at s["t"] as a template check, at most one per SOV010_CHECK_EVERY s."""
    if s["t"] - _SOV010_LAST_CHECK.get(gw, float("-inf")) >= float(os.environ.get("SOV010_CHECK_EVERY", "5")):
        _SOV010_LAST_CHECK[gw] = s["t"]
        post("/sovereignty/check", {"gateway": gw, "t": s["t"], "txids": s["txids"]})


'''


path = sys.argv[1]
src = open(path).read()
out, n = SNAP.subn(patch, src)
out, gaps = re.subn(r"rng\.uniform\(8, 14\)",
                    'rng.uniform(float(os.environ.get("SOV004_BLOCK_MIN", "30")), '
                    'float(os.environ.get("SOV004_BLOCK_MAX", "50")))', out)
if n == 0:
    sys.exit(f"add-checks: no snapshot posts in {path}")
if out.count("\ndef post(") != 1:
    sys.exit(f"add-checks: no single `def post(` in {path}")
out = out.replace("\ndef post(", HELPER + "def post(", 1)
open(path, "w").write(out)
print(f"add-checks: {path}: {n} snapshot post(s) also post a check; {gaps} block gap(s) now 30-50 s")
