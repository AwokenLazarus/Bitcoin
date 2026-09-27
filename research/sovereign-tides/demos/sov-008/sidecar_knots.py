#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-008: canaries placed by the real lazarus-canary sidecar, judged by primed from template
checks over real DATUM sessions (SOV-P-002 W8 + W9), on Knots 29.4.2 regtest.

Nodes (loopback, rpc BASE+2i, p2p BASE+2i+1, datadirs under $SOV008_RUN):
  P   the pool node (primed's). -blockprioritysize=0 -blockmintxfee=1.5 sat/vB. Has a wallet
      ("pool") so the zero-leak check has something to look in.
  H1  honest gateway node          H2  honest gateway node (peers P and H1)
  X   node the proxy registered (runs, relays, ignored by its gateway)
  Y   node the two hybrids registered; they read its mempool
  S   the sidecar's signer: no peers, no wallet, never broadcasts

Gateways: `gwsim` processes (real DATUM handshake to primed under a registered G, one job per
template, answering request_full_block), fed templates by this harness:
  knots-h1, knots-h2   their own node's getblocktemplate
  knots-proxy          P's template T
  knots-hybrid         T ∪ (L∖T), L = Y's mempool; its node's side of every conflict wins
  knots-hybrid-filt    (T∩L) ∪ (L∖M), M = P's mempool

The sidecar (lazarus/canary on rnd/sov-008) runs for real: own canaries over one-shot P2P to each
gateway's registered node, shared canaries and decoys through P, every canary reported to primed.
Primed schedules its own template checks. Nothing here posts snapshots or checks to primed.

usage: sidecar_knots.py <bitcoind> <seconds> <base_port> <primed listen host:port> <primed data dir> <sidecar dir> <gwsim>
"""
import atexit
import base64
import hashlib
import json
import os
import random
import shutil
import signal
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

BITCOIND, SECS, BASE, PRIME_LISTEN, PRIME_DIR, SIDECAR, GWSIM = sys.argv[1:8]
SECS, BASE = float(SECS), int(BASE)
STATS = os.environ.get("PRIME_STATS", "http://127.0.0.1:32016")
RUN = os.environ.get("SOV008_RUN", os.path.join(os.path.dirname(os.path.abspath(__file__)), "run"))
RDTS = "-rdtsexpiry=4102444800"
P2WSH_TRUE = b"\x00\x20" + hashlib.sha256(b"\x51").digest()
DESC = "raw(" + P2WSH_TRUE.hex() + ")"
POOL_SCRIPT = "0014751e76e8199196d454941c45d1b3a323f1433bd6"
CANARY_UTXOS = int(os.environ.get("SOV008_CANARY_UTXOS", "480"))
CANARY_UTXO_SATS = 20_000
ROUND_SECS = float(os.environ.get("SOV008_ROUND_SECS", "15"))
SHARED_RELEASE = 600.0   # the sidecar's default fees.shared_release_secs
BLOCK_GAP = (float(os.environ.get("SOV008_BLOCK_MIN", "60")), float(os.environ.get("SOV008_BLOCK_MAX", "120")))
rng = random.Random(int(os.environ.get("SOV008_SEED", str(int(time.time())))))
nodes = {}
procs = []
GATEWAYS = {"knots-h1": "H1", "knots-h2": "H2", "knots-proxy": "X", "knots-hybrid": "Y", "knots-hybrid-filt": "Y"}
HONEST = ("knots-h1", "knots-h2")
CAUGHT = {"knots-proxy": None, "knots-hybrid": "decoys-carried", "knots-hybrid-filt": "shared-canaries-missed"}
NODE_ORDER = ["P", "H1", "H2", "X", "Y", "S"]


def start(name, extra, connect=()):
    i = NODE_ORDER.index(name)
    d = os.path.join(RUN, f"knots-{name}")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    rpcport, p2p = BASE + 2 * i, BASE + 2 * i + 1
    args = [f"-datadir={d}", "-regtest", "-server", f"-rpcport={rpcport}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-rpcuser=x", "-rpcpassword=sov008", "-testactivationheight=blake2b@101",
            "-blake2b_headline=Lazarus", RDTS, "-dnsseed=0", "-fixedseeds=0"] + (
                [] if "-listen=0" in extra else [f"-port={p2p}", "-bind=127.0.0.1"]) + [
            "-listenonion=0", "-v2transport=0"] + [f"-addnode=127.0.0.1:{BASE + 2 * NODE_ORDER.index(j) + 1}" for j in connect] + extra
    p = subprocess.Popen(["nice", "-n", "10", BITCOIND] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    nodes[name] = {"p": p, "d": d, "port": rpcport, "p2p": p2p}


def rpc(name, method, *params, wallet=None):
    body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
    url = f"http://127.0.0.1:{nodes[name]['port']}/" + (f"wallet/{wallet}" if wallet else "")
    req = urllib.request.Request(url, body, {"Authorization": "Basic " + base64.b64encode(b"x:sov008").decode()})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.load(r)["result"]
    except urllib.error.HTTPError as e:
        raise RuntimeError(json.load(e).get("error")) from None


def stop_all():
    for p in procs:
        if p.poll() is None:
            p.terminate()
    for p in procs:
        try:
            p.wait(timeout=20)
        except Exception:  # noqa: BLE001
            p.kill()
    for n in nodes.values():
        n["p"].terminate()
    for n in nodes.values():
        try:
            n["p"].wait(timeout=60)
        except Exception:  # noqa: BLE001
            n["p"].kill()
    if not os.environ.get("SOV008_KEEP"):
        for n in nodes.values():
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


def op_true_tx(inputs, outputs):
    """Spend P2WSH(OP_TRUE) inputs; returns (hex, txid)."""
    ins = varint(len(inputs)) + b"".join(bytes.fromhex(t)[::-1] + struct.pack("<I", n) + b"\x00" + b"\xfd\xff\xff\xff"
                                         for t, n in inputs)
    outs = varint(len(outputs)) + b"".join(struct.pack("<Q", v) + varint(len(s)) + s for v, s in outputs)
    ver, lock = struct.pack("<I", 2), b"\x00" * 4
    txid = hashlib.sha256(hashlib.sha256(ver + ins + outs + lock).digest()).digest()[::-1].hex()
    wit = b"".join(b"\x01\x01\x51" for _ in inputs)
    return (ver + b"\x00\x01" + ins + outs + wit + lock).hex(), txid


def tx_inputs(raw_hex):
    """Outpoints a raw tx spends (for merging templates without conflicts)."""
    b = bytes.fromhex(raw_hex)
    pos = 6 if b[4:6] == b"\x00\x01" else 4
    n = b[pos]
    pos += 1
    out = []
    for _ in range(n):
        out.append((b[pos:pos + 32][::-1].hex(), struct.unpack("<I", b[pos + 32:pos + 36])[0]))
        pos += 36
        sl = b[pos]
        pos += 1 + sl + 4
    return out


B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def wif_regtest(secret: bytes) -> str:
    payload = b"\xef" + secret + b"\x01"
    data = payload + hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    n = int.from_bytes(data, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(data) - len(data.lstrip(b"\x00"))) + s


def post_prime(path, body, token):
    req = urllib.request.Request(STATS + path, json.dumps(body).encode(),
                                 {"Content-Type": "application/json", "X-Sovereignty-Token": token})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def main():
    atexit.register(stop_all)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    os.makedirs(RUN, exist_ok=True)
    start("P", ["-blockprioritysize=0", "-blockmintxfee=0.000015"])
    start("H1", ["-disablewallet"], connect=["P"])
    start("H2", ["-disablewallet"], connect=["P", "H1"])
    start("X", ["-disablewallet"], connect=["P", "H1"])
    start("Y", ["-disablewallet"], connect=["P", "H2"])
    start("S", ["-disablewallet", "-connect=0", "-listen=0"])
    for n in nodes:
        wait_up(n)
    print("node:", rpc("P", "getnetworkinfo")["subversion"])
    rpc("P", "createwallet", "pool")
    pool_addr = rpc("P", "getnewaddress", "", "bech32", wallet="pool")

    # the canary key: made here, handed to the sidecar's key file and the signer's RPC, never to P
    wif = wif_regtest(os.urandom(32))
    info = rpc("S", "getdescriptorinfo", f"wpkh({wif})")
    canary_addr = rpc("S", "deriveaddresses", info["descriptor"])[0]
    canary_spk = bytes.fromhex(rpc("S", "validateaddress", canary_addr)["scriptPubKey"])
    key_file = os.path.join(SIDECAR, "canary.wif")
    os.makedirs(SIDECAR, exist_ok=True)
    fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, (wif + "\n").encode())
    os.close(fd)

    per_cb = 120
    canary_cbs = (CANARY_UTXOS + per_cb - 1) // per_cb
    # every coinbase spent below must be mature (100 deep)
    rpc("P", "generatetodescriptor", 131 + canary_cbs, DESC)
    time.sleep(2)
    utxos = []
    for h in range(1, 31 + canary_cbs):
        cb = rpc("P", "getblock", rpc("P", "getblockhash", h), 2)["tx"][0]
        v = round(cb["vout"][0]["value"] * 1e8)
        if h <= 30:
            outs = [((v - 50_000) // 24, P2WSH_TRUE)] * 24
        elif h == 31:
            outs = [(100_000_000, bytes.fromhex(rpc("P", "getaddressinfo", pool_addr, wallet="pool")["scriptPubKey"]))]
            outs += [((v - 100_050_000), P2WSH_TRUE)]
        else:
            outs = [(CANARY_UTXO_SATS, canary_spk)] * per_cb + [(v - per_cb * CANARY_UTXO_SATS - 100_000, P2WSH_TRUE)]
        raw, txid = op_true_tx([(cb["txid"], 0)], outs)
        assert rpc("P", "sendrawtransaction", raw, 0) == txid
        if h <= 30:
            utxos.extend((txid, i, outs[0][0]) for i in range(24))
    tip = rpc("P", "generatetodescriptor", 1, DESC)[0]
    for _ in range(240):
        if all(rpc(n, "getbestblockhash") == tip for n in nodes if n != "S"):
            break
        time.sleep(0.5)
    else:
        raise SystemExit("nodes did not sync")
    print("peers:", {n: rpc(n, "getconnectioncount") for n in nodes}, "height", rpc("P", "getblockcount"))

    # primed: register every gateway's G the production way (signed; sovereignty-demo is off)
    token = open(os.path.join(PRIME_DIR, "sovereignty.token")).read().strip()
    gw_keys, gw_g = {}, {}
    # SOV-010: v1 enrolment (SOV-P-002 §4.1) naming each gateway's node; primed files the nodes in
    # sovereignty-nodes.json, which is the sidecar's nodes_file below
    node_of = {n: f"127.0.0.1:{nodes[GATEWAYS[n]]['p2p']}" for n in GATEWAYS}
    at_h = rpc("P", "getblockcount")
    at = f"{at_h} {rpc('P', 'getblockhash', at_h)}"
    os.makedirs(SIDECAR, exist_ok=True)
    for name in GATEWAYS:
        secret = os.urandom(64).hex()
        g = subprocess.check_output([GWSIM, "register", secret], text=True).split()[0]
        msg = "\n".join(["LZT1 register v1", "pool: lazarus-xbt", "chain: XBT", f"G: {g}",
                         f"node: {node_of[name]}", f"tag: {name}", f"at: {at}"])
        mf = os.path.join(SIDECAR, f"{name}.enrol")
        open(mf, "w").write(msg)
        sig = subprocess.check_output([GWSIM, "sign", secret, mf], text=True).strip()
        r = post_prime("/sovereignty/enrol", {"message": msg, "g_sig": sig}, token)
        assert r.get("g") == g and r.get("status", "active") == "active", r
        gw_keys[name], gw_g[name] = secret, g
    nodes_file = os.path.join(PRIME_DIR, "sovereignty-nodes.json")
    filed = {g: v["node"] for g, v in json.load(open(nodes_file)).items()}
    assert filed == {gw_g[n]: node_of[n] for n in GATEWAYS}, filed
    print(f"enrolled {len(filed)} gateways (v1); sovereignty-nodes.json holds their nodes; "
          f"the sidecar reads it as nodes_file", flush=True)
    names_of = {g: n for n, g in gw_g.items()}

    # the sidecar's config: the demo's fast cadence (every gateway due at every round, jittered +-20%)
    node_of = {n: f"127.0.0.1:{nodes[GATEWAYS[n]]['p2p']}" for n in GATEWAYS}
    toml = f'''network = "regtest"
nodes_source = "prime"
[prime]
url = "{STATS}"
token_file = "{PRIME_DIR}/sovereignty.token"
[pool_node]
url = "http://127.0.0.1:{nodes['P']['port']}"
user = "x"
password = "sov008"
blockmintxfee_satvb = 1.5
[signer]
url = "http://127.0.0.1:{nodes['S']['port']}"
user = "x"
password = "sov008"
[wallet]
key_file = "{key_file}"
state_dir = "{SIDECAR}/state"
[fees]
fee_rate_satvb = 10
[cadence]
tick_secs = 1
round_min_gap_secs = {ROUND_SECS}
new_secs = {ROUND_SECS * 0.8}
enrolled_secs = {ROUND_SECS * 0.8}
clean_secs = {ROUND_SECS * 0.8}
flagged_secs = {ROUND_SECS * 0.8}
jitter = 0.2
[delivery]
mode = "direct"
timeout_secs = 10
'''
    cfg_path = os.path.join(SIDECAR, "canary.toml")
    open(cfg_path, "w").write(toml)
    sidecar_py = os.environ.get("SIDECAR_PY", os.path.join(os.environ.get("WT", "."), "lazarus/canary/lazarus_canary.py"))

    # the gateways: real DATUM sessions into primed
    gws = {}
    for name in GATEWAYS:
        log = open(os.path.join(RUN, f"{name}.gwsim.log"), "w")
        p = subprocess.Popen(["nice", "-n", "10", GWSIM, PRIME_LISTEN, os.path.join(PRIME_DIR, "prime.key"), gw_keys[name],
                              POOL_SCRIPT, name], stdin=subprocess.PIPE, stdout=log, stderr=log, text=True)
        procs.append(p)
        gws[name] = {"p": p, "last": None}
    raw_cache = {}
    stop = threading.Event()
    lock = threading.Lock()
    pool_templates = []   # (t, [txids]) of P, for the leak check
    honest_seen = {n: set() for n in HONEST}

    def push(name, tpl, txids):
        key = (tpl["previousblockhash"], tuple(txids))
        g = gws[name]
        if g["last"] == key:
            return
        g["last"] = key
        line = json.dumps({"prev": tpl["previousblockhash"], "height": tpl["height"], "bits": tpl["bits"],
                           "value": tpl["coinbasevalue"], "txs": [raw_cache[x] for x in txids]})
        try:
            g["p"].stdin.write(line + "\n")
            g["p"].stdin.flush()
        except (BrokenPipeError, ValueError):
            pass

    def gbt(n):
        t = rpc(n, "getblocktemplate", {"rules": ["segwit", "blake2b"]})
        with lock:
            for x in t["transactions"]:
                raw_cache[x["txid"]] = x["data"]
        return t

    def own_feed(n, name):
        while not stop.is_set():
            try:
                t = gbt(n)
                ids = [x["txid"] for x in t["transactions"]]
                with lock:
                    honest_seen[name].update(ids)
                push(name, t, ids)
            except Exception as e:  # noqa: BLE001
                print("feed", name, e, file=sys.stderr)
            stop.wait(1.0)

    def pool_feed():
        while not stop.is_set():
            try:
                t = gbt("P")
                T = [x["txid"] for x in t["transactions"]]
                with lock:
                    pool_templates.append((time.time(), T))
                L = rpc("Y", "getrawmempool")
                M = set(rpc("P", "getrawmempool"))
                for x in L:
                    if x not in raw_cache:
                        try:
                            raw_cache[x] = rpc("Y", "getrawtransaction", x)
                        except RuntimeError:
                            pass
                L = [x for x in L if x in raw_cache]
                Ls, Ts = set(L), set(T)
                spent_by_l = {o for x in L for o in tx_inputs(raw_cache[x])}
                naive = [x for x in T if x in Ls or not (set(tx_inputs(raw_cache[x])) & spent_by_l)]
                naive += [x for x in L if x not in Ts]
                filt = [x for x in T if x in Ls] + [x for x in L if x not in M]
                time.sleep(rng.uniform(0.02, 0.08))
                push("knots-proxy", t, T)
                push("knots-hybrid", t, naive)
                push("knots-hybrid-filt", t, filt)
            except Exception as e:  # noqa: BLE001
                print("pool feed", e, file=sys.stderr)
            stop.wait(1.0)

    def traffic():
        while not stop.is_set():
            if not utxos:
                return
            txid, n, v = utxos.pop(rng.randrange(len(utxos)))
            fee = rng.choice([300, 800, 2_000, 5_000])
            outs = [(v - fee, P2WSH_TRUE)]
            if rng.random() < 0.3:
                outs.append((0, b"\x6a\x14" + rng.randbytes(20)))
            raw, new = op_true_tx([(txid, n)], outs)
            try:
                rpc("P", "sendrawtransaction", raw)
                utxos.append((new, 0, v - fee))
            except Exception:  # noqa: BLE001
                pass
            stop.wait(rng.uniform(0.3, 1.0))

    finders = []

    def blocks():
        while not stop.wait(rng.uniform(*BLOCK_GAP)):
            who = rng.choice(["P", "P", "H1", "H2"])
            try:
                rpc(who, "generatetodescriptor", 1, DESC)
                finders.append(who)
            except Exception as e:  # noqa: BLE001
                print("block", who, e, file=sys.stderr)

    threads = [threading.Thread(target=own_feed, args=("H1", "knots-h1")), threading.Thread(target=own_feed, args=("H2", "knots-h2")),
               threading.Thread(target=pool_feed), threading.Thread(target=traffic), threading.Thread(target=blocks)]
    for t in threads:
        t.daemon = True
        t.start()
    time.sleep(3)
    sc_log = open(os.path.join(RUN, "sidecar.log"), "w")
    sidecar = subprocess.Popen(["python3", sidecar_py, "-c", cfg_path, "run"], stdout=sc_log, stderr=subprocess.STDOUT)
    procs.append(sidecar)
    time.sleep(SECS)
    sidecar.terminate()
    sidecar.wait(timeout=30)
    # let the checks of the last rounds land, then stop feeding
    time.sleep(float(os.environ.get("SOV008_DRAIN", "55")))
    stop.set()
    for t in threads:
        t.join(timeout=30)

    # evidence
    placed = [json.loads(line) for line in open(os.path.join(SIDECAR, "state", "canaries.jsonl")) if '"placed"' in line]
    resolved = [json.loads(line) for line in open(os.path.join(SIDECAR, "state", "canaries.jsonl")) if '"resolved"' in line]
    status = json.loads(subprocess.check_output(["python3", sidecar_py, "-c", cfg_path, "status"], text=True))
    kinds = {p["txid"]: p["kind"] for p in placed}
    placed_at = {p["txid"]: p["t"] for p in placed}
    pool_ids = {x for _, ids in pool_templates for x in ids}
    first_in_pool = {}
    for t, ids in pool_templates:
        for x in ids:
            first_in_pool.setdefault(x, t)
    # a shared canary's priority is lifted after shared_release_secs (the pool may mine it then);
    # before that, and for own canaries ever, a pool template holding one is a leak
    release = SHARED_RELEASE
    leaks = sum(1 for x, k in kinds.items() if k == "own" and x in pool_ids)
    leaks += sum(1 for x, k in kinds.items() if k == "shared" and x in first_in_pool and first_in_pool[x] < placed_at[x] + release)
    decoys = [x for x, k in kinds.items() if k == "decoy"]
    decoys_in_pool = sum(x in pool_ids for x in decoys)
    honest_decoys = {n: sum(x in honest_seen[n] for x in decoys) for n in HONEST}
    with urllib.request.urlopen(STATS + "/sovereignty.json", timeout=60) as r:
        sov = json.load(r)
    rows = {names_of.get(g, g): f for g, f in (sov.get("gateways") or {}).items()}

    # zero-leak key check: the canary key never in a wallet on the pool node
    iso = json.loads(subprocess.run(["python3", sidecar_py, "-c", cfg_path, "check-isolation"], capture_output=True, text=True).stdout)
    ai = rpc("P", "getaddressinfo", canary_addr, wallet="pool")
    descs = json.dumps(rpc("P", "listdescriptors", True, wallet="pool"))
    in_wallet_utxos = [u for u in rpc("P", "listunspent", 0, 9_999_999, [], True, wallet="pool") if u.get("address") == canary_addr]
    wif_on_disk = []
    for root, _, files in os.walk(nodes["P"]["d"]):
        for fn in files:
            try:
                with open(os.path.join(root, fn), "rb") as fh:
                    if wif.encode() in fh.read():
                        wif_on_disk.append(fn)
            except OSError:
                pass
    signer = {"peers": rpc("S", "getconnectioncount"), "mempool": rpc("S", "getmempoolinfo")["size"]}
    key = {"sidecar_check": iso.get("pool_wallets_knowing_it"), "ismine": ai.get("ismine"), "iswatchonly": ai.get("iswatchonly"),
           "wif_in_pool_descriptors": wif in descs, "canary_utxos_in_pool_wallet": len(in_wallet_utxos),
           "wif_in_pool_datadir": wif_on_disk, "signer": signer}
    key_ok = (key["sidecar_check"] == [] and not key["ismine"] and not key["iswatchonly"] and not key["wif_in_pool_descriptors"]
              and not key["canary_utxos_in_pool_wallet"] and not wif_on_disk and signer == {"peers": 0, "mempool": 0})

    fees = {"rounds": status["rounds"], "placed": status["delivered"], "delivery_failed": status["delivery_failed"],
            "fees_planned": status["fees_planned"], "fees_paid": status["fees_paid"], "resolved": status["resolved"],
            "paid_by_kind": {k: sum(r["fee"] for r in resolved if r["kind"] == k) for k in ("own", "shared", "decoy")},
            "resolved_by_kind": {k: sum(1 for r in resolved if r["kind"] == k) for k in ("own", "shared", "decoy")},
            "own_confirmed_as": {w: sum(1 for r in resolved if r["kind"] == "own" and r["which"] == w) for w in ("C", "C'")}}
    rounds = max(1, status["rounds"])
    fees["planned_per_round"] = round(status["fees_planned"] / rounds, 1)
    print(f"knots: {SECS:.0f} s + drain, height {rpc('P', 'getblockcount')}, blocks by {finders}")
    print(f"sidecar: {json.dumps(fees)}")
    print(f"canaries: {sum(k == 'own' for k in kinds.values())} own, {sum(k == 'shared' for k in kinds.values())} shared, "
          f"{len(decoys)} decoys; own/shared leaks into pool templates {leaks}; decoys in pool templates "
          f"{decoys_in_pool}/{len(decoys)}; decoys held by honest gateways {honest_decoys}")
    print(f"key isolation: {json.dumps(key)} -> {'OK' if key_ok else 'FAIL'}")
    print(f"primed: checks_refused {sov.get('store', {}).get('checks_refused')}, store {sov.get('store')}")
    print("| gateway | checks | own hit/due | shared hit/due | decoys carried/due | evidence | independence | flagged | reasons |")
    print("|---|---:|---:|---:|---:|---|---:|---|---|")
    for name in GATEWAYS:
        f = rows.get(name) or {}
        print(f"| {name} | {f.get('checks')} | {f.get('canary_hits')}/{f.get('canaries_due')} | {f.get('shared_hits')}/{f.get('shared_due')} | "
              f"{f.get('decoys_carried')}/{f.get('decoys_due')} | {f.get('evidence')} | {f.get('independence')} | {f.get('flagged')} | "
              f"{','.join(f.get('flag_reasons') or [])} |")
    os.makedirs(os.path.join(RUN, "results"), exist_ok=True)
    out = os.path.join(RUN, "results", f"sidecar-{int(time.time())}.json")
    json.dump({"secs": SECS, "finders": finders, "leaks": leaks, "decoys_in_pool": decoys_in_pool, "decoys": len(decoys),
               "honest_decoys": honest_decoys, "rows": {n: rows.get(n) for n in GATEWAYS}, "key": key, "key_ok": key_ok,
               "fees": fees, "checks_refused": sov.get("store", {}).get("checks_refused")}, open(out, "w"), indent=1)
    print("result", out)

    problems = []
    for h in HONEST:
        f = rows.get(h) or {}
        if f.get("flagged") or f.get("evidence") != "canary" or (f.get("canary_source") != "checks"):
            problems.append(f"{h}: evidence={f.get('evidence')} flagged={f.get('flagged')} {f.get('flag_reasons')}")
        if f.get("decoys_carried") or honest_decoys[h]:
            problems.append(f"{h} carried a decoy")
    for g, reason in CAUGHT.items():
        f = rows.get(g) or {}
        if not f.get("flagged"):
            problems.append(f"{g} not flagged: {f.get('flag_reasons')} own {f.get('canary_hits')}/{f.get('canaries_due')}")
        elif reason and reason not in (f.get("flag_reasons") or []):
            problems.append(f"{g} flagged for {f.get('flag_reasons')}, expected {reason}")
    if leaks:
        problems.append(f"{leaks} own/shared canaries leaked into pool templates")
    if decoys and decoys_in_pool < len(decoys) // 2:
        problems.append(f"only {decoys_in_pool}/{len(decoys)} decoys reached the pool's template")
    if not key_ok:
        problems.append(f"key isolation failed: {key}")
    if status["delivery_failed"]:
        problems.append(f"{status['delivery_failed']} own canaries not delivered")
    if problems:
        print("KNOTS_FAIL", *problems, sep="\n  ")
        sys.exit(1)
    print("KNOTS_OK")


if __name__ == "__main__":
    main()
