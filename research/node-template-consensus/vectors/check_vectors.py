#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Check nta-vectors.json without Knots: recompute each preimage, digest and BIP340 check.

Uses the research model's independent secp256k1/BIP340 code (../xbt_secp.py in this folder).
A vector passes if the rules predict the node's verdict: more than max_payees distinct
payees is "bad-nta-too-many" whatever the signatures; otherwise every signature the
coinbase carries verifies under its payee key exactly when the node said "valid" (the x09
vector carries a truncated signature only).
"""
import json
import os
import struct
import sys

MODEL = os.environ.get("NTA_MODEL", os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
sys.path.insert(0, MODEL)
from xbt_secp import schnorr_verify, tagged_hash  # noqa: E402

path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "nta-vectors.json")
doc = json.load(open(path))
cap = doc["max_payees"]
bad = 0
for v in doc["vectors"]:
    prev = bytes.fromhex(v["hashPrevBlock"])[::-1]  # uint256 is serialized little-endian
    ok_all = True
    for a in v["attestations"]:
        script = bytes.fromhex(a["payee_script"])
        pre = bytes([len(script)]) + script + struct.pack("<i", v["height"]) + \
            struct.pack("<I", int(v["nBits"], 16)) + prev
        assert pre.hex() == a["preimage"], v["name"]
        dg = tagged_hash(doc["tag"], pre)
        assert dg.hex() == a["digest"], v["name"]
        sig = a["carried_sig"]
        ok_all &= sig is not None and len(sig) == 128 and \
            schnorr_verify(bytes.fromhex(a["payee_key"]), dg, bytes.fromhex(sig))
    n = len({a["payee_script"] for a in v["attestations"]})
    if n > cap:
        # the cap alone must explain the verdict, so x10 carries only valid signatures
        status = "ok" if v["result"] == "bad-nta-too-many" and ok_all else "MISMATCH"
    else:
        status = "ok" if ok_all == (v["result"] == "valid") else "MISMATCH"
    bad += status != "ok"
    print(f"{status:8} {v['result']:16} {v['name']}")
sys.exit(1 if bad else 0)
