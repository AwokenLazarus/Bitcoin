#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-004: the hybrid proxy (pool template + its own node's mempool) against primed's canary
detector, on real Knots 29.4.2 regtest nodes with converged mempools.

Grown from rnd/a2-nta's prime/scripts/rnd-a2-knots.py (XBT-064). Isolated, loopback only,
ports BASE..BASE+9 (default 31720), datadirs under $SOV004_RUN. No miner processes: blocks come
from generatetodescriptor on whichever node "finds" them.

  P   the pool's node (Prime's). -blockprioritysize=0 -blockmintxfee=1.5 sat/vB, so a tx it
      holds at a lower modified feerate stays in its mempool and out of its templates.
  H1  honest gateway node, peers P
  H2  honest gateway node, peers P and H1
  X   node registered by the proxy and the mimic (runs, relays, ignored by its gateway)
  Y   node registered by the three hybrids; each hybrid reads Y's mempool

Gateways (polled every 0.25 s, as Prime would see their jobs):
  knots-h1, knots-h2   their own node's template
  knots-proxy          P's template T, 20-80 ms later
  knots-mimic          T reordered, 0.3-1.5 s later
  knots-hybrid         T ∪ (L∖T), where L is Y's mempool. Its own canaries win their conflicts.
                       This is the SOV-004 threat: pool template plus whatever its node has.
  knots-hybrid-filt    (T∩L) ∪ (L∖M), where M is P's mempool. It drops what its node lacks, adds
                       only what the pool never saw, and follows every pool exclusion.
  knots-hybrid-forced  (T∩L) ∪ (L∖T), which is L. It passes everything, and its template is its
                       own node's tx set (the bound, reported against Y's own getblocktemplate).

Canaries, every CANARY_EVERY s:
  own     per gateway: C to its registered node, conflicting twin C' (1 sat more fee) to P,
          which has `prioritisetransaction C 0 -1e8` first (XBT-064).
  shared  one per round: D to P, pre-prioritised to 110 sat of modified fee (1.15 sat/vB, under
          P's block floor). P keeps D in its mempool, relays it on its base fee, never templates it.
  decoy   one per round: Q with zero fee to P, pre-prioritised +1e6 sat. P templates Q. No peer
          accepts it (relay floor, feefilter on base fee), so no honest node ever has it.

Snapshots and canaries (with their kind) are posted to primed (PRIME_STATS); primed's verdict is
asserted: honest and forced not flagged, and proxy, mimic, hybrid and hybrid-filt flagged.

Usage: hybrid_knots.py <bitcoind> [seconds] [base_port]
"""
import atexit, base64, signal, hashlib, json, os, random, shutil, struct, subprocess, sys, threading, time
import urllib.error, urllib.request

BITCOIND = sys.argv[1]
SECS = float(sys.argv[2]) if len(sys.argv) > 2 else 120
BASE = int(sys.argv[3]) if len(sys.argv) > 3 else 31720
STATS = os.environ.get("PRIME_STATS", "http://127.0.0.1:31716")
RUN = os.environ.get("SOV004_RUN", os.path.join(os.path.dirname(os.path.abspath(__file__)), "run"))
RDTS = "-rdtsexpiry=4102444800"
P2WSH_TRUE = b"\x00\x20" + hashlib.sha256(b"\x51").digest()
DESC = "raw(" + P2WSH_TRUE.hex() + ")"
CANARY_EVERY = float(os.environ.get("SOV004_CANARY_EVERY", "3"))
POLL = 0.25
CANARY_FEE = 3_000  # ~31 sat/vB: well above any honest node's block min fee
SHARED_MODIFIED = 110  # sat on a 96 vB tx: over P's 1 sat/vB relay floor, under its 1.5 sat/vB block floor
rng = random.Random(int(os.environ.get("SOV004_SEED", str(int(time.time())))))
nodes = {}
GATEWAYS = {"knots-h1": "H1", "knots-h2": "H2", "knots-proxy": "X", "knots-mimic": "X",
            "knots-hybrid": "Y", "knots-hybrid-filt": "Y", "knots-hybrid-forced": "Y"}
HONEST = ("knots-h1", "knots-h2")
CAUGHT = ("knots-hybrid", "knots-hybrid-filt", "knots-mimic", "knots-proxy")


def start(name, i, extra, connect=()):
    d = os.path.join(RUN, f"knots-{name}")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    rpcport, p2p = BASE + 2 * i, BASE + 2 * i + 1
    args = [f"-datadir={d}", "-regtest", "-server", f"-rpcport={rpcport}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-rpcuser=x", "-rpcpassword=sov004", "-testactivationheight=blake2b@101",
            "-blake2b_headline=Lazarus", RDTS, "-disablewallet", f"-port={p2p}", "-bind=127.0.0.1",
            "-listen=1", "-dnsseed=0", "-fixedseeds=0"] + [
                f"-addnode=127.0.0.1:{BASE + 2 * j + 1}" for j in connect] + extra
    p = subprocess.Popen(["nice", "-n", "10", BITCOIND] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    nodes[name] = {"p": p, "d": d, "port": rpcport}


def rpc(name, method, *params):
    body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{nodes[name]['port']}/", body,
                                 {"Authorization": "Basic " + base64.b64encode(b"x:sov004").decode()})
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
    """inputs [(txid_hex, vout)], outputs [(sats, script)] -> (hex, txid); inputs are P2WSH(OP_TRUE)."""
    ins = varint(len(inputs)) + b"".join(
        bytes.fromhex(t)[::-1] + struct.pack("<I", n) + b"\x00" + b"\xfd\xff\xff\xff" for t, n in inputs)
    outs = varint(len(outputs)) + b"".join(struct.pack("<Q", v) + varint(len(s)) + s for v, s in outputs)
    ver, lock = struct.pack("<I", 2), b"\x00" * 4
    txid = hashlib.sha256(hashlib.sha256(ver + ins + outs + lock).digest()).digest()[::-1].hex()
    wit = b"".join(b"\x01\x01\x51" for _ in inputs)
    return (ver + b"\x00\x01" + ins + outs + wit + lock).hex(), txid


_SOV010_LAST_CHECK = {}


def post_check(gw, s):
    """SOV-010: the gateway's job at s["t"] as a template check, at most one per SOV010_CHECK_EVERY s."""
    if s["t"] - _SOV010_LAST_CHECK.get(gw, float("-inf")) >= float(os.environ.get("SOV010_CHECK_EVERY", "5")):
        _SOV010_LAST_CHECK[gw] = s["t"]
        post("/sovereignty/check", {"gateway": gw, "t": s["t"], "txids": s["txids"]})



def sov012_resolve(cans):
    """SOV-012: post how each canary settled (the sidecar's job); returns (posted, within 12 s)."""
    posted = early = 0
    for c in cans:
        for x in (c["txid"], c.get("twin")):
            if not x:
                continue
            o = rpc("P", "gettxout", x, 0, False)
            if not o:
                continue
            height = rpc("P", "getblockheader", o["bestblock"], True)["height"] - o["confirmations"] + 1
            bt = rpc("P", "getblockheader", rpc("P", "getblockhash", height), True)["time"]
            r = post("/sovereignty/canary/resolved", {"txid": c["txid"], "confirmed": x, "height": height, "time": bt})
            if not r.get("ok"):
                print("SOV012 resolved refused", r, file=sys.stderr)
            posted += 1
            early += bt < c["t"] + 12
            break
    return posted, early


def sov012_line(label, before, after, names):
    for n, key in names:
        b, a = before.get(key) or {}, after.get(key) or {}
        print(f"SOV012 {label} {n} own {b.get('canary_hits')}/{b.get('canaries_due')} -> "
              f"{a.get('canary_hits')}/{a.get('canaries_due')} rate {b.get('canary_rate')} -> {a.get('canary_rate')} "
              f"flagged {b.get('flagged')} -> {a.get('flagged')}", flush=True)


def post(path, body):
    req = urllib.request.Request(STATS + path, json.dumps(body).encode(), {"Content-Type": "application/json", "X-Sovereignty-Token": os.environ["SOV_TOKEN"]})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def main():
    atexit.register(stop_all)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))  # so atexit stops the nodes
    os.makedirs(RUN, exist_ok=True)
    start("P", 0, ["-blockprioritysize=0", "-blockmintxfee=0.000015"])
    start("H1", 1, [], connect=[0])
    start("H2", 2, [], connect=[0, 1])
    start("X", 3, [], connect=[0, 1])
    start("Y", 4, [], connect=[0, 2])
    for n in nodes:
        wait_up(n)
    print("node:", rpc("P", "getnetworkinfo")["subversion"])
    rpc("P", "generatetodescriptor", 130, DESC)
    time.sleep(2)
    utxos, reserve = [], []
    for h in range(1, 31):
        cb = rpc("P", "getblock", rpc("P", "getblockhash", h), 2)["tx"][0]
        v = round(cb["vout"][0]["value"] * 1e8)
        outs = [((v - 50_000) // 24, P2WSH_TRUE)] * 24
        raw, txid = tx([(cb["txid"], 0)], outs)
        assert rpc("P", "sendrawtransaction", raw) == txid
        (reserve if h <= 20 else utxos).extend((txid, i, outs[0][0]) for i in range(24))
    tip = rpc("P", "generatetodescriptor", 1, DESC)[0]
    for _ in range(240):  # canaries spend these outputs on every node: all must have them
        if all(rpc(n, "getbestblockhash") == tip for n in nodes):
            break
        time.sleep(0.5)
    else:
        raise SystemExit(f"nodes did not sync to {tip}: " + str({n: rpc(n, "getblockcount") for n in nodes}))
    print("peers:", {n: rpc(n, "getconnectioncount") for n in nodes}, "height", rpc("P", "getblockcount"))

    snaps = {g: [] for g in ("POOL", *GATEWAYS)}
    own_node = []  # Y's own getblocktemplate, for the forced hybrid's bound (not posted)
    canaries = []  # {"gateway", "t", "txid", "kind"}
    twin_of = {}   # own canary -> the twin P holds
    stop = threading.Event()
    lock = threading.Lock()
    ulock = threading.Lock()

    def take_utxo(pool=utxos):
        with ulock:
            return pool.pop(rng.randrange(len(pool))) if pool else None

    def gbt(n):
        t = rpc(n, "getblocktemplate", {"rules": ["segwit", "blake2b"]})
        return {"t": time.time(), "prev": t["previousblockhash"], "txids": [x["txid"] for x in t["transactions"]]}

    def hybrids(s):
        """The three hybrids' templates from P's template s, Y's mempool and P's mempool."""
        mine = rpc("Y", "getrawmempool")
        pool_mp = set(rpc("P", "getrawmempool"))
        t = time.time() + rng.uniform(0.05, 0.3)
        T, L = s["txids"], set(mine)
        Tset = set(T)
        with lock:
            beaten = {twin_of[c] for c in L if c in twin_of}
        naive = [x for x in T if x not in beaten] + [x for x in mine if x not in Tset]
        filt = [x for x in T if x in L] + [x for x in mine if x not in pool_mp]
        forced = [x for x in T if x in L] + [x for x in mine if x not in Tset]
        return [(g, dict(s, t=t, txids=txs)) for g, txs in
                (("knots-hybrid", naive), ("knots-hybrid-filt", filt), ("knots-hybrid-forced", forced))]

    def poll(node, name):
        while not stop.is_set():
            try:
                s = gbt(node)
                derived = []
                if node == "P":
                    derived.append(("knots-proxy", dict(s, txids=list(s["txids"]), t=s["t"] + rng.uniform(0.02, 0.08))))
                    m = list(s["txids"])
                    rng.shuffle(m)
                    derived.append(("knots-mimic", dict(s, txids=m, t=s["t"] + rng.uniform(0.3, 1.5))))
                    derived += hybrids(s)
                with lock:
                    (own_node if node == "Y" else snaps[name]).append(s)
                    for g, d in derived:
                        snaps[g].append(d)
            except Exception as e:  # noqa: BLE001
                print("poll", node, e, file=sys.stderr)
            time.sleep(POLL)

    def traffic():
        while not stop.is_set():
            u = take_utxo()
            if u is None:
                return
            txid, n, v = u
            fee = rng.choice([300, 800, 2_000, 5_000])
            outs = [(v - fee, P2WSH_TRUE)]
            if rng.random() < 0.3:
                outs.append((0, b"\x6a\x14" + rng.randbytes(20)))
            raw, new = tx([(txid, n)], outs)
            try:
                rpc("P", "sendrawtransaction", raw)  # converged: all traffic enters at the pool
                with ulock:
                    utxos.append((new, 0, v - fee))
            except Exception:  # noqa: BLE001
                pass
            time.sleep(rng.uniform(0.2, 0.8))

    def canary_loop():
        while CANARY_EVERY > 0 and not stop.is_set():
            for gw, node in GATEWAYS.items():
                u = take_utxo(reserve)  # confirmed: every node already has the input
                if u is None:
                    return
                txid, n, v = u
                raw, cid = tx([(txid, n)], [(v - CANARY_FEE, P2WSH_TRUE)])
                twin, tid = tx([(txid, n)], [(v - CANARY_FEE - 1, P2WSH_TRUE)])
                try:
                    # the pool node must never template C: set the delta before it can arrive
                    rpc("P", "prioritisetransaction", cid, 0, -100_000_000)
                    with lock:
                        twin_of[cid] = tid
                    rpc(node, "sendrawtransaction", raw)
                    t0 = time.time()
                    rpc("P", "sendrawtransaction", twin)
                    with lock:
                        canaries.append({"gateway": gw, "t": t0, "txid": cid, "kind": "own"})
                except Exception as e:  # noqa: BLE001
                    print("canary", gw, e, file=sys.stderr)
            for kind in ("shared", "decoy"):
                u = take_utxo(reserve)
                if u is None:
                    return
                txid, n, v = u
                fee = CANARY_FEE if kind == "shared" else 0
                raw, cid = tx([(txid, n)], [(v - fee, P2WSH_TRUE)])
                delta = SHARED_MODIFIED - fee if kind == "shared" else 1_000_000
                try:
                    rpc("P", "prioritisetransaction", cid, 0, delta)
                    t0 = time.time()
                    rpc("P", "sendrawtransaction", raw, 0)
                    with lock:
                        canaries.append({"gateway": "*", "t": t0, "txid": cid, "kind": kind})
                except Exception as e:  # noqa: BLE001
                    print("canary", kind, e, file=sys.stderr)
            stop.wait(CANARY_EVERY)

    finders = []

    def blocks():
        while not stop.wait(rng.uniform(float(os.environ.get("SOV004_BLOCK_MIN", "30")), float(os.environ.get("SOV004_BLOCK_MAX", "50")))):
            who = rng.choice(["P", "P", "H1", "H2"])
            try:
                rpc(who, "generatetodescriptor", 1, DESC)
                finders.append(who)
            except Exception as e:  # noqa: BLE001
                print("block", who, e, file=sys.stderr)

    threads = [threading.Thread(target=poll, args=(n, "POOL" if n == "P" else f"knots-{n.lower()}"))
               for n in ("P", "H1", "H2", "Y")]
    threads += [threading.Thread(target=f) for f in (traffic, canary_loop, blocks)]
    for t in threads:
        t.daemon = True
        t.start()
    time.sleep(SECS)
    stop.set()
    for t in threads:
        t.join(timeout=30)

    kinds = {c["txid"]: c["kind"] for c in canaries}
    pool_txids = {x for s in snaps["POOL"] for x in s["txids"]}
    # own and shared canaries must never reach a pool template; decoys must, and no honest node may have one
    leaks = sum(1 for x, k in kinds.items() if k != "decoy" and x in pool_txids)
    decoys = [x for x, k in kinds.items() if k == "decoy"]
    decoys_in_pool = sum(x in pool_txids for x in decoys)
    honest_decoys = {g: sum(x in {y for s in snaps[g] for y in s["txids"]} for x in decoys) for g in HONEST}

    # bound: every tx the forced hybrid templates, its own node templated too (Y's getblocktemplate
    # is cached up to 5 s, so "too" means within 6 s), and it carries nothing only the pool had
    def seen_near(ss):
        idx = {}
        for o in ss:
            for x in o["txids"]:
                idx.setdefault(x, []).append(o["t"])
        return lambda x, t: any(abs(u - t) <= 6 for u in idx.get(x, ()))

    in_own, in_pool = seen_near(own_node), seen_near(snaps["POOL"])
    fx = [(x, s["t"]) for s in snaps["knots-hybrid-forced"] for x in s["txids"]]
    forced_in_own = round(sum(in_own(x, t) for x, t in fx) / len(fx), 3) if fx else None
    forced_pool_only = round(sum(in_pool(x, t) and not in_own(x, t) for x, t in fx) / len(fx), 3) if fx else None

    for gw, ss in snaps.items():
        for s in ss:
            (post("/sovereignty/snapshot", {"gateway": gw, **s}), post_check(gw, s))
    for c in canaries:
        post("/sovereignty/canary", c)
    with urllib.request.urlopen(STATS + "/sovereignty.json", timeout=600) as r:
        sov012_before = json.load(r).get("gateways") or {}
    sov012_n = sov012_resolve([dict(c, twin=twin_of.get(c["txid"])) for c in canaries])
    print(f"SOV012 hybrid settled {sov012_n[0]} canaries, {sov012_n[1]} within 12 s of delivery", flush=True)
    with urllib.request.urlopen(STATS + "/sovereignty.json", timeout=600) as r:
        sov = json.load(r)
    gws = sov.get("gateways") or {}
    sov012_line("hybrid", sov012_before, gws, [(g, g) for g in GATEWAYS])
    print(f"knots: {SECS:.0f} s, height {rpc('P', 'getblockcount')}, blocks by {finders}, "
          f"snapshots {({k: len(v) for k, v in snaps.items()})}")
    print(f"canaries: {sum(k == 'own' for k in kinds.values())} own, {sum(k == 'shared' for k in kinds.values())} shared, "
          f"{len(decoys)} decoys; own/shared leaks into pool templates {leaks}; decoys in pool templates "
          f"{decoys_in_pool}/{len(decoys)}; decoys held by honest gateways {honest_decoys}")
    print(f"forced hybrid: {forced_in_own:.1%} of its template txs were in its own node's templates (within 6 s); "
          f"{forced_pool_only:.1%} came from a pool template its own node never had")
    print("| gateway | identical | jaccard | own hit/due | shared hit/due | decoys carried/due | evidence | independence | flagged | reasons |")
    print("|---|---:|---:|---:|---:|---:|---|---:|---|---|")
    rows = {}
    for name in GATEWAYS:
        f = gws.get(name) or {}
        rows[name] = f
        print(f"| {name} | {f.get('identical', 0):.2f} | {f.get('jaccard', 0):.2f} | "
              f"{f.get('canary_hits')}/{f.get('canaries_due')} | {f.get('shared_hits')}/{f.get('shared_due')} | "
              f"{f.get('decoys_carried')}/{f.get('decoys_due')} | {f.get('evidence')} | "
              f"{f.get('independence', 0):.3f} | {f.get('flagged')} | {','.join(f.get('flag_reasons') or [])} |")
    flagged = sorted(g for g in sov.get("flagged") or [] if g.startswith("knots-"))
    print("flagged (knots)", flagged)
    os.makedirs(os.path.join(RUN, "results"), exist_ok=True)
    out = os.path.join(RUN, "results", f"knots-{int(time.time())}.json")
    with open(out, "w") as fh:
        json.dump({"secs": SECS, "finders": finders, "leaks": leaks, "decoys_in_pool": decoys_in_pool,
                   "decoys": len(decoys), "honest_decoys": honest_decoys, "rows": rows, "flagged": flagged,
                   "forced_in_own": forced_in_own, "forced_pool_only": forced_pool_only}, fh, indent=1)
    print("result", out)

    problems = []
    if flagged != sorted(CAUGHT):
        problems.append(f"flagged {flagged} != {sorted(CAUGHT)}")
    for h in (*HONEST, "knots-hybrid-forced"):
        f = rows[h]
        if f.get("evidence") != "canary" or f.get("flagged"):
            problems.append(f"{h}: {f.get('evidence')} flagged={f.get('flagged')} {f.get('flag_reasons')}")
    for h in HONEST:
        if rows[h].get("decoys_carried") or honest_decoys[h]:
            problems.append(f"{h} carried a decoy")
    if rows["knots-hybrid"].get("flag_reasons") and "decoys-carried" not in rows["knots-hybrid"]["flag_reasons"]:
        problems.append(f"knots-hybrid not caught by decoys: {rows['knots-hybrid'].get('flag_reasons')}")
    if leaks:
        problems.append(f"{leaks} own/shared canaries leaked into pool templates")
    if decoys and decoys_in_pool < len(decoys) // 2:
        problems.append(f"only {decoys_in_pool}/{len(decoys)} decoys reached the pool's template")
    if problems:
        print("KNOTS_FAIL", *problems, sep="\n  ")
        sys.exit(1)
    print("KNOTS_OK")


if __name__ == "__main__":
    main()
