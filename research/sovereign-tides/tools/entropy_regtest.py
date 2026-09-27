#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Template-entropy detector on a real regtest network (XBT-051, note C.2 #5 "verify").

Four ISOLATED Knots 29.4.2 regtest nodes, loopback only, spare ports, no wallet, no miner
processes (the pool node's own generate RPC finds blocks at regtest difficulty):
  P   the pool's node (Prime's). Finds a block every ~12-20 s.
  H1  honest gateway node, default policy, peers with P
  H2  honest gateway node, -datacarriersize=0 (a Knots-strict operator), peers with H1
  H3  honest gateway node, -blockmintxfee higher, peers with P and H2
Gateways polled every 0.5 s (getblocktemplate), as Prime would see their jobs:
  H1, H2, H3  their own node's template
  PROXY       P's template, relayed with 20-80 ms delay (a "DATUM" gateway fed by the pool node)
  MIMIC       P's template with its tx order shuffled and 0.3-1.5 s delay (a proxy trying to
              hide from the branch comparison)
  HYBRID      P's template plus up to 2 txs from H1's mempool that P lacks, 0.2-0.6 s delay (a
              proxy that also runs a mempool node to look independent)
Transactions (P2WSH OP_TRUE spends, some with an OP_RETURN, varied fees) are sent to random
nodes. Output: detector table + clusters, entropy_result.json. Stops every node on exit.

Usage: entropy_regtest.py <bitcoind> [seconds] [rpc_base_port]
"""
import atexit
import base64
import hashlib
import json
import os
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

import entropy

BITCOIND = sys.argv[1]
SECS = float(sys.argv[2]) if len(sys.argv) > 2 else 150
BASE = int(sys.argv[3]) if len(sys.argv) > 3 else 30811
RDTS = "-rdtsexpiry=4102444800"
P2WSH_TRUE = b"\x00\x20" + hashlib.sha256(b"\x51").digest()
DESC = "raw(" + P2WSH_TRUE.hex() + ")"
rng = random.Random(51)
nodes = {}


def start(name, i, extra, connect=()):
    d = tempfile.mkdtemp(prefix=f"xbt051-ent-{name}-")
    rpcport, p2p = BASE + 2 * i, BASE + 2 * i + 1
    args = [f"-datadir={d}", "-regtest", "-dnsseed=0", "-server", f"-rpcport={rpcport}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-rpcuser=x", "-rpcpassword=xbt051", "-testactivationheight=blake2b@101",
            "-blake2b_headline=Lazarus", RDTS, "-disablewallet", f"-port={p2p}", "-bind=127.0.0.1", "-listen=1",
            "-dnsseed=0", "-fixedseeds=0"] + [f"-addnode=127.0.0.1:{BASE + 2 * j + 1}" for j in connect] + extra
    p = subprocess.Popen([BITCOIND] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    nodes[name] = {"p": p, "d": d, "port": rpcport}


def rpc(name, method, *params):
    body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{nodes[name]['port']}/", body,
                                 {"Authorization": "Basic " + base64.b64encode(b"x:xbt051").decode()})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)["result"]
    except urllib.error.HTTPError as e:
        raise RuntimeError(json.load(e).get("error")) from None


def stop_all():
    for n in nodes.values():
        n["p"].terminate()
    for n in nodes.values():
        try:
            n["p"].wait(timeout=60)
        except Exception:  # noqa: BLE001
            n["p"].kill()
        shutil.rmtree(n["d"], ignore_errors=True)


atexit.register(stop_all)


def wait_up(name):
    for _ in range(120):
        try:
            rpc(name, "getblockcount")
            return
        except Exception:  # noqa: BLE001
            time.sleep(0.5)
    raise SystemExit(f"{name} did not start")


def varint(n):
    return bytes([n]) if n < 0xFD else b"\xfd" + struct.pack("<H", n)


def tx(inputs, outputs):
    """inputs [(txid_hex, vout)], outputs [(sats, script)] -> hex; all inputs P2WSH(OP_TRUE)."""
    o = struct.pack("<I", 2) + b"\x00\x01" + varint(len(inputs))
    for txid, n in inputs:
        o += bytes.fromhex(txid)[::-1] + struct.pack("<I", n) + b"\x00" + b"\xfd\xff\xff\xff"
    o += varint(len(outputs))
    for v, s in outputs:
        o += struct.pack("<Q", v) + varint(len(s)) + s
    o += b"".join(b"\x01\x01\x51" for _ in inputs)
    return (o + b"\x00" * 4).hex()


def main():
    start("P", 0, [])
    start("H1", 1, [], connect=[0])
    start("H2", 2, ["-datacarriersize=0"], connect=[1])
    start("H3", 3, ["-blockmintxfee=0.00005"], connect=[0, 2])
    for n in nodes:
        wait_up(n)
    print("node:", rpc("P", "getnetworkinfo")["subversion"])
    rpc("P", "generatetodescriptor", 130, DESC)
    time.sleep(3)
    # fan out 20 mature coinbases into 400 small UTXOs
    utxos = []
    for h in range(1, 21):
        cb = rpc("P", "getblock", rpc("P", "getblockhash", h), 2)["tx"][0]
        v = round(cb["vout"][0]["value"] * 1e8)
        outs = [((v - 50_000) // 20, P2WSH_TRUE)] * 20
        txid = rpc("P", "sendrawtransaction", tx([(cb["txid"], 0)], outs))
        utxos += [(txid, i, outs[0][0]) for i in range(20)]
    rpc("P", "generatetodescriptor", 1, DESC)
    for _ in range(60):
        if all(rpc(n, "getblockcount") == rpc("P", "getblockcount") for n in nodes):
            break
        time.sleep(0.5)
    print("peers:", {n: rpc(n, "getconnectioncount") for n in nodes}, "height", rpc("P", "getblockcount"))

    snaps = {g: [] for g in ("POOL", "H1", "H2", "H3", "PROXY", "MIMIC", "HYBRID")}
    stop = threading.Event()
    lock = threading.Lock()

    def gbt(n):
        t = rpc(n, "getblocktemplate", {"rules": ["segwit", "blake2b"]})
        return {"t": time.time(), "prev": t["previousblockhash"], "txids": [x["txid"] for x in t["transactions"]]}

    def poll(node, name):
        while not stop.is_set():
            try:
                s = gbt(node)
                with lock:
                    snaps[name].append(s)
                if node == "P":
                    for gname, delay, shuffle in (("PROXY", rng.uniform(0.02, 0.08), False),
                                                  ("MIMIC", rng.uniform(0.3, 1.5), True)):
                        c = dict(s, txids=list(s["txids"]), t=s["t"] + delay)
                        if shuffle:
                            rng.shuffle(c["txids"])
                        with lock:
                            snaps[gname].append(c)
                    with lock:
                        h1 = snaps["H1"][-1]["txids"] if snaps["H1"] else []
                    own = [t for t in h1 if t not in set(s["txids"])][:2]
                    with lock:
                        snaps["HYBRID"].append(dict(s, txids=s["txids"] + own, t=s["t"] + rng.uniform(0.2, 0.6)))
            except Exception as e:  # noqa: BLE001
                print("poll", node, e, file=sys.stderr)
            time.sleep(0.5)

    def traffic():
        pool = list(utxos)
        while not stop.is_set() and pool:
            txid, n, v = pool.pop(rng.randrange(len(pool)))
            fee = rng.choice([300, 800, 2_000, 5_000, 20_000])
            outs = [(v - fee, P2WSH_TRUE)]
            if rng.random() < 0.3:
                outs.append((0, b"\x6a\x14" + rng.randbytes(20)))
            try:
                new = rpc(rng.choice(list(nodes)), "sendrawtransaction", tx([(txid, n)], outs))
                pool.append((new, 0, v - fee))
            except Exception:  # noqa: BLE001  (policy rejects on H2 are fine)
                pass
            time.sleep(rng.uniform(0.1, 0.6))

    def blocks():
        while not stop.is_set():
            time.sleep(rng.uniform(12, 20))
            rpc("P", "generatetodescriptor", 1, DESC)

    threads = [threading.Thread(target=poll, args=(n, "POOL" if n == "P" else n)) for n in nodes]
    threads += [threading.Thread(target=traffic), threading.Thread(target=blocks)]
    for t in threads:
        t.daemon = True
        t.start()
    time.sleep(SECS)
    stop.set()
    for t in threads:
        t.join(timeout=30)

    pool = snaps.pop("POOL")
    rows, cl = entropy.report(snaps, pool)
    print(f"\n{len(pool)} pool templates, height {rpc('P', 'getblockcount')}, run {SECS:.0f} s\n")
    print("| Gateway | Snapshots | Identical to pool | Branch sim | Tx-set Jaccard | Switch lag (s) | Lockstep | Incl. lag (s) | Lead | Subset | **Independence** |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    f2 = lambda x: "—" if x is None else f"{x:.2f}"  # noqa: E731
    for g, f in rows.items():
        print(f"| {g} | {f['snapshots']} | {f['identical']:.2f} | {f['branch_sim']:.2f} | {f['jaccard']:.2f} | "
              f"{f2(f['switch_lag'])} | {f['lockstep']:.2f} | {f2(f['incl_lag'])} | {f['lead']:.2f} | {f['subset']:.2f} | "
              f"**{f['independence']:.2f}** |")
    print("\nclusters (identical >= 80% both ways):", cl)
    json.dump({"rows": rows, "clusters": cl, "secs": SECS, "pool_templates": len(pool)},
              open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "entropy_result.json"), "w"), indent=1)


if __name__ == "__main__":
    main()
