#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""XBT-051: does Knots 29.4.2 with RDTS accept, relay and mine a coinbase carrying an
OP_RETURN of N bytes (the whole scriptPubKey)? And what does mempool policy say for an ordinary
transaction with the same output?

Two ISOLATED regtest nodes (temp datadirs, loopback only, spare ports, no wallet): A mines,
B peers only with A. For each size N the script builds A's next block with `generateblock
... submit=false`, splices an N-byte OP_RETURN output into the coinbase, re-grinds the BLAKE2b
v2 header and submits it to A, then checks that B received it over P2P. No miner processes are
started (the node's own generate RPC grinds regtest difficulty). Both nodes stop on exit.

Usage: rdts_opreturn_test.py <bitcoind> [rpc_base_port]
"""
import atexit
import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # xbtpow/ beside this file
from xbtpow.v2 import HeaderV2, nbits_to_target  # noqa: E402  (XBT-050's header-v2 port)

bitcoind = sys.argv[1]
base = int(sys.argv[2]) if len(sys.argv) > 2 else 30801
EXTRA = sys.argv[3:]  # e.g. -rdtsexpiry=4102444800
cli_bin = os.path.join(os.path.dirname(bitcoind), "bitcoin-cli")
SIZES = [38, 70, 77, 80, 81, 82, 83, 84, 85, 103]
nodes = {}


def start(name, rpcport, extra):
    d = tempfile.mkdtemp(prefix=f"xbt051-{name}-")
    args = [f"-datadir={d}", "-regtest", "-dnsseed=0", "-server", f"-rpcport={rpcport}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-rpcuser=x", "-rpcpassword=xbt051", "-testactivationheight=blake2b@101",
            "-blake2b_headline=Lazarus", "-disablewallet", "-fallbackfee=0"] + EXTRA + extra
    p = subprocess.Popen([bitcoind] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    nodes[name] = (p, d, rpcport)


def rpc(name, *a):
    _, _, port = nodes[name]
    r = subprocess.run([cli_bin, "-regtest", f"-rpcport={port}", "-rpcuser=x", "-rpcpassword=xbt051"]
                       + [a_ if isinstance(a_, str) else json.dumps(a_) for a_ in a], capture_output=True, text=True)
    out = (r.stdout or r.stderr).strip()
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return out


def stop_all():
    for name, (p, d, _) in nodes.items():
        try:
            rpc(name, "stop")
            p.wait(timeout=60)
        except Exception:  # noqa: BLE001
            p.kill()
        shutil.rmtree(d, ignore_errors=True)


atexit.register(stop_all)


def wait_up(name):
    for _ in range(120):
        if isinstance(rpc(name, "getblockcount"), int):
            return
        time.sleep(0.5)
    raise SystemExit(f"node {name} did not start")


def dsha(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def varint(n):
    return bytes([n]) if n < 0xFD else b"\xfd" + struct.pack("<H", n) if n <= 0xFFFF else b"\xfe" + struct.pack("<I", n)


def read_varint(b, i):
    x = b[i]
    if x < 0xFD:
        return x, i + 1
    w = {0xFD: 2, 0xFE: 4, 0xFF: 8}[x]
    return int.from_bytes(b[i + 1:i + 1 + w], "little"), i + 1 + w


def parse_tx(b, i):
    """-> (dict, end). Handles the segwit marker."""
    start = i
    ver = b[i:i + 4]
    i += 4
    segwit = b[i] == 0 and b[i + 1] == 1
    if segwit:
        i += 2
    n, i = read_varint(b, i)
    vin = []
    for _ in range(n):
        prev = b[i:i + 36]
        i += 36
        sl, i = read_varint(b, i)
        vin.append([prev, b[i:i + sl], b[i + sl:i + sl + 4]])
        i += sl + 4
    n, i = read_varint(b, i)
    vout = []
    for _ in range(n):
        v = b[i:i + 8]
        sl, i = read_varint(b, i + 8)
        vout.append([v, b[i:i + sl]])
        i += sl
    wit = []
    if segwit:
        for _ in vin:
            k, i = read_varint(b, i)
            items = []
            for _ in range(k):
                l_, i = read_varint(b, i)
                items.append(b[i:i + l_])
                i += l_
            wit.append(items)
    lock = b[i:i + 4]
    return {"ver": ver, "vin": vin, "vout": vout, "wit": wit, "lock": lock, "raw": b[start:i + 4]}, i + 4


def ser_tx(t, with_wit=True):
    wit = with_wit and any(t["wit"])
    o = t["ver"] + (b"\x00\x01" if wit else b"") + varint(len(t["vin"]))
    for prev, ss, seq in t["vin"]:
        o += prev + varint(len(ss)) + ss + seq
    o += varint(len(t["vout"]))
    for v, s in t["vout"]:
        o += v + varint(len(s)) + s
    if wit:
        for items in t["wit"]:
            o += varint(len(items)) + b"".join(varint(len(x)) + x for x in items)
    return o + t["lock"]


def op_return(total_len):
    """A scriptPubKey of exactly total_len bytes: OP_RETURN + one push."""
    for hdr_len, mk in ((1, lambda n: bytes([n])), (2, lambda n: b"\x4c" + bytes([n]))):
        n = total_len - 1 - hdr_len
        if (hdr_len == 1 and 0 <= n <= 75) or (hdr_len == 2 and 76 <= n <= 255):
            data = (b"LZT1" + bytes(range(256)))[:n]
            return b"\x6a" + mk(n) + data
    raise ValueError(total_len)


P2WSH_TRUE = b"\x00\x20" + hashlib.sha256(b"\x51").digest()
DESC = "raw(" + P2WSH_TRUE.hex() + ")"


CONTROLS = {
    # RDTS: a non-OP_RETURN output over 34 bytes is invalid; these prove the rules are on.
    "p2pk-35": b"\x21\x02" + bytes(range(1, 33)) + b"\xac",
    "bare-multisig-37": b"\x51\x21\x02" + bytes(range(1, 33)) + b"\x51\xae",
}


def coinbase_case(size):
    return coinbase_script_case(op_return(size))


def coinbase_script_case(script):
    blk = bytes.fromhex(rpc("A", "generateblock", DESC, [], "false")["hex"])
    hdr = HeaderV2.parse(blk[:164])
    ntx, i = read_varint(blk, 164)
    cb, j = parse_tx(blk, i)
    rest = blk[j:]
    cb["vout"].insert(0, [b"\x00" * 8, script])
    assert ntx == 1, "empty template expected"
    hdr = hdr.copy(merkle_root=dsha(ser_tx(cb, False)))
    t = nbits_to_target(hdr.nbits)
    while hdr.pow_int() > t:
        hdr.nonce += 1
    block = hdr.serialize() + varint(ntx) + ser_tx(cb) + rest
    res = rpc("A", "submitblock", block.hex())
    h = hdr.pow_hash().hex()
    on_a = rpc("A", "getbestblockhash") == h
    on_b = False
    for _ in range(40):
        if rpc("B", "getbestblockhash") == h:
            on_b = True
            break
        time.sleep(0.25)
    return {"submitblock": res or "null (accepted)", "tip_on_A": on_a, "relayed_to_B": on_b}


def mature_coinbase_utxo():
    """A spendable P2WSH(OP_TRUE) coinbase output from early in the chain."""
    for h in range(1, rpc("A", "getblockcount")):
        blk = rpc("A", "getblock", rpc("A", "getblockhash", h), 2)
        cb = blk["tx"][0]
        for n, o in enumerate(cb["vout"]):
            if o["scriptPubKey"]["hex"] == P2WSH_TRUE.hex() and o["value"] > 0.001:
                yield cb["txid"], n, round(o["value"] * 1e8)


def mempool_case(size, utxo):
    txid, n, sats = utxo
    tx = {"ver": struct.pack("<I", 2), "vin": [[bytes.fromhex(txid)[::-1] + struct.pack("<I", n), b"", b"\xfd\xff\xff\xff"]],
          "vout": [[struct.pack("<Q", sats - 20_000), P2WSH_TRUE], [b"\x00" * 8, op_return(size)]],
          "wit": [[b"\x51"]], "lock": b"\x00" * 4}
    r = rpc("A", "testmempoolaccept", [ser_tx(tx).hex()])
    r = r[0] if isinstance(r, list) else {"error": r}
    return {"allowed": r.get("allowed"), "reject": r.get("reject-reason") or r.get("error")}


def main():
    start("A", base, [f"-port={base + 1}", "-listen=1", "-bind=127.0.0.1", "-connect=0"])
    start("B", base + 2, ["-listen=0", f"-connect=127.0.0.1:{base + 1}"])
    wait_up("A")
    wait_up("B")
    print("node:", rpc("A", "getnetworkinfo")["subversion"])
    rpc("A", "generatetodescriptor", 110, DESC)
    for _ in range(60):
        if rpc("B", "getblockcount") == 110:
            break
        time.sleep(0.5)
    dep = rpc("A", "getdeploymentinfo")
    print("blake2b/rdts deployments:", {k: v.get("active") for k, v in dep.get("deployments", {}).items()
                                        if any(s in k.lower() for s in ("blake", "rdts", "reduced"))})
    print("B synced to", rpc("B", "getblockcount"))
    out = {"node": rpc("A", "getnetworkinfo")["subversion"], "flags": EXTRA, "controls": {}, "coinbase": {}, "mempool": {}}
    for name, script in CONTROLS.items():
        out["controls"][name] = r = coinbase_script_case(script)
        print(f"control coinbase {name}: {r}")
    for s in SIZES:
        out["coinbase"][s] = r = coinbase_case(s)
        print(f"coinbase OP_RETURN {s:3d} B: {r}")
    # policy side: spend a mature coinbase (skipped if regtest maturity is not reached)
    utxos = mature_coinbase_utxo()
    utxo = next(utxos)
    probe = mempool_case(38, utxo)
    if probe["reject"] and "premature" in str(probe["reject"]):
        need = rpc("A", "getblockcount")
        print(f"coinbase not mature at height {need}: {probe['reject']}; mining more")
        rpc("A", "generatetodescriptor", 400, DESC)
        probe = mempool_case(38, utxo)
    for s in SIZES:
        out["mempool"][s] = r = mempool_case(s, utxo)
        print(f"mempool tx OP_RETURN {s:3d} B: {r}")
    json.dump(out, open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "rdts_opreturn_result" + ("-" + "-".join(a.lstrip("-").split("=")[0] for a in EXTRA) if EXTRA else "") + ".json"), "w"), indent=1)


if __name__ == "__main__":
    main()
