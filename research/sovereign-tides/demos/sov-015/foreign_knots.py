#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-015: foreign canaries on Knots 29.4.2 regtest. The real lazarus-canary sidecar (rnd/sov-015,
`[foreign] enabled`) fans every own canary's twin C′ out to the other gateways' registered nodes;
primed judges own, shared, decoy and foreign canaries from its own template checks over real DATUM
sessions, and clusters gateways that carry each other's canaries (shared-template-source).

Nodes (loopback, rpc BASE+2i, p2p BASE+2i+1, datadirs under $SOV015_RUN):
  P   the pool node (primed's). -blockprioritysize=0 -blockmintxfee=1.5 sat/vB, wallet "pool"
  H1  honest gateway node (-> P)          H2  honest gateway node (-> P, H1)
  X   the node the proxy registered (-> P, H1; runs and relays, its gateway ignores it)
  T   a third party's node (-> P, H2), NOT the pool's. It listens on its own port and on three
      more (T_PORTS): the three farm gateways each registered one of them, so to the sidecar they
      are three nodes. It is one mempool and one template.
  S   the sidecar's signer: no peers, no wallet, never broadcasts

Gateways (`gwsim`, real DATUM sessions into primed under an enrolled G, one job per template,
answering request_full_block), fed templates by this harness:
  knots-h1, knots-h2        their own node's getblocktemplate                  never flagged, never clustered
  tfarm-1, tfarm-2, tfarm-3 T's getblocktemplate                               one shared-source cluster
  knots-proxy               P's template (SOV-004's control)                   flagged

usage: foreign_knots.py <bitcoind> <seconds> <base_port> <primed listen host:port> <primed data dir> <sidecar dir> <gwsim>
"""
import atexit
import json
import os
import random
import signal
import subprocess
import sys
import threading
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import knots_lab as kl  # noqa: E402

BITCOIND, SECS, BASE, PRIME_LISTEN, PRIME_DIR, SIDECAR, GWSIM = sys.argv[1:8]
SECS, BASE = float(SECS), int(BASE)
STATS = os.environ["PRIME_STATS"]
RUN = os.environ["SOV015_RUN"]
SIDECAR_PY = os.environ["SIDECAR_PY"]
POOL_SCRIPT = "0014751e76e8199196d454941c45d1b3a323f1433bd6"
CANARY_UTXOS = int(os.environ.get("SOV015_CANARY_UTXOS", "480"))
CANARY_UTXO_SATS = 20_000
ROUND_SECS = float(os.environ.get("SOV015_ROUND_SECS", "15"))
SHARED_RELEASE = 600.0
BLOCK_GAP = (float(os.environ.get("SOV015_BLOCK_MIN", "60")), float(os.environ.get("SOV015_BLOCK_MAX", "120")))
MAX_LAG = float(os.environ.get("SOV015_MAX_LAG", "0.05"))
rng = random.Random(int(os.environ.get("SOV015_SEED", str(int(time.time())))))
ORDER = ["P", "H1", "H2", "X", "T", "S"]
lab = kl.Lab(BITCOIND, BASE, RUN, ORDER)
# T's extra p2p ports, one per farm gateway, above the six nodes' ports
T_PORTS = [BASE + 2 * len(ORDER) + 1 + i for i in range(3)]
HONEST = ("knots-h1", "knots-h2")
FARM = ("tfarm-1", "tfarm-2", "tfarm-3")
PROXY = "knots-proxy"
GATEWAYS = {"knots-h1": "H1", "knots-h2": "H2", PROXY: "X", "tfarm-1": "T", "tfarm-2": "T", "tfarm-3": "T"}
procs = []


def node_addr(name):
    if name in FARM:
        return f"127.0.0.1:{T_PORTS[FARM.index(name)]}"
    return lab.addr(GATEWAYS[name])


def stop_all():
    for p in procs:
        if p.poll() is None:
            p.terminate()
    for p in procs:
        try:
            p.wait(timeout=20)
        except Exception:  # noqa: BLE001
            p.kill()
    lab.stop_all(keep=bool(os.environ.get("SOV015_KEEP")))


def post_prime(path, body, token):
    req = urllib.request.Request(STATS + path, json.dumps(body).encode(),
                                 {"Content-Type": "application/json", "X-Sovereignty-Token": token})
    with urllib.request.urlopen(req, timeout=15) as r:
        return json.load(r)


def main():
    atexit.register(stop_all)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    os.makedirs(RUN, exist_ok=True)
    lab.start("P", ["-blockprioritysize=0", "-blockmintxfee=0.000015"])
    lab.start("H1", ["-disablewallet"], connect=["P"])
    lab.start("H2", ["-disablewallet"], connect=["P", "H1"])
    lab.start("X", ["-disablewallet"], connect=["P", "H1"])
    lab.start("T", ["-disablewallet"], connect=["P", "H2"], ports=T_PORTS)
    lab.start("S", ["-disablewallet", "-connect=0", "-listen=0"])
    lab.wait_up()
    rpc = lab.rpc
    print("node:", rpc("P", "getnetworkinfo")["subversion"], "T listens on", lab.addr("T"), "and", T_PORTS, flush=True)
    rpc("P", "createwallet", "pool")
    pool_addr = rpc("P", "getnewaddress", "", "bech32", wallet="pool")

    # the canary key: made here, handed to the sidecar's key file and the signer's RPC, never to P
    wif = kl.wif_regtest(os.urandom(32))
    info = rpc("S", "getdescriptorinfo", f"wpkh({wif})")
    canary_addr = rpc("S", "deriveaddresses", info["descriptor"])[0]
    canary_spk = bytes.fromhex(rpc("S", "validateaddress", canary_addr)["scriptPubKey"])
    os.makedirs(SIDECAR, exist_ok=True)
    key_file = os.path.join(SIDECAR, "canary.wif")
    fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, (wif + "\n").encode())
    os.close(fd)

    per_cb = 120
    canary_cbs = (CANARY_UTXOS + per_cb - 1) // per_cb
    rpc("P", "generatetodescriptor", 131 + canary_cbs, kl.DESC)
    time.sleep(2)
    utxos = []
    for h in range(1, 31 + canary_cbs):
        cb = rpc("P", "getblock", rpc("P", "getblockhash", h), 2)["tx"][0]
        v = round(cb["vout"][0]["value"] * 1e8)
        if h <= 30:
            outs = [((v - 50_000) // 24, kl.P2WSH_TRUE)] * 24
        elif h == 31:
            outs = [(100_000_000, bytes.fromhex(rpc("P", "getaddressinfo", pool_addr, wallet="pool")["scriptPubKey"]))]
            outs += [((v - 100_050_000), kl.P2WSH_TRUE)]
        else:
            outs = [(CANARY_UTXO_SATS, canary_spk)] * per_cb + [(v - per_cb * CANARY_UTXO_SATS - 100_000, kl.P2WSH_TRUE)]
        raw, txid = kl.op_true_tx([(cb["txid"], 0)], outs)
        assert rpc("P", "sendrawtransaction", raw, 0) == txid
        if h <= 30:
            utxos.extend((txid, i, outs[0][0]) for i in range(24))
    tip = rpc("P", "generatetodescriptor", 1, kl.DESC)[0]
    lab.wait_sync([n for n in ORDER if n != "S"], tip)
    print("peers:", {n: rpc(n, "getconnectioncount") for n in ORDER}, "height", rpc("P", "getblockcount"), flush=True)

    # v1 enrolment (SOV-P-002 §4.1) naming each gateway's node; the sidecar reads the nodes from
    # primed's GET /sovereignty/nodes (SOV-012)
    token = open(os.path.join(PRIME_DIR, "sovereignty.token")).read().strip()
    gw_keys, gw_g = {}, {}
    at_h = rpc("P", "getblockcount")
    at = f"{at_h} {rpc('P', 'getblockhash', at_h)}"
    for name in GATEWAYS:
        secret = os.urandom(64).hex()
        g = subprocess.check_output([GWSIM, "register", secret], text=True).split()[0]
        msg = "\n".join(["LZT1 register v1", "pool: lazarus-xbt", "chain: XBT", f"G: {g}",
                         f"node: {node_addr(name)}", f"tag: {name}", f"at: {at}"])
        mf = os.path.join(SIDECAR, f"{name}.enrol")
        open(mf, "w").write(msg)
        sig = subprocess.check_output([GWSIM, "sign", secret, mf], text=True).strip()
        r = post_prime("/sovereignty/enrol", {"message": msg, "g_sig": sig}, token)
        assert r.get("g") == g and r.get("status", "active") == "active", r
        gw_keys[name], gw_g[name] = secret, g
    names_of = {g: n for n, g in gw_g.items()}
    print(f"enrolled {len(gw_g)} gateways (v1) with their nodes; the three farm gateways name three ports of T", flush=True)

    toml = f'''network = "regtest"
nodes_source = "prime"
[prime]
url = "{STATS}"
token_file = "{PRIME_DIR}/sovereignty.token"
[pool_node]
url = "http://127.0.0.1:{lab.nodes['P']['port']}"
user = "x"
password = "sov015"
blockmintxfee_satvb = 1.5
[signer]
url = "http://127.0.0.1:{lab.nodes['S']['port']}"
user = "x"
password = "sov015"
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
[foreign]
enabled = true
fanout = 0
order = "own-ack"
max_lag_secs = {MAX_LAG}
'''
    cfg_path = os.path.join(SIDECAR, "canary.toml")
    open(cfg_path, "w").write(toml)

    gws = {}
    for name in GATEWAYS:
        log = open(os.path.join(RUN, f"{name}.gwsim.log"), "w")
        p = subprocess.Popen(["nice", "-n", "19", GWSIM, PRIME_LISTEN, os.path.join(PRIME_DIR, "prime.key"), gw_keys[name],
                              POOL_SCRIPT, name], stdin=subprocess.PIPE, stdout=log, stderr=log, text=True)
        procs.append(p)
        gws[name] = {"p": p, "last": None}
    raw_cache = {}
    stop = threading.Event()
    lock = threading.Lock()
    pool_templates = []
    seen = {n: set() for n in GATEWAYS}

    def push(name, tpl, txids):
        key = (tpl["previousblockhash"], tuple(txids))
        g = gws[name]
        if g["last"] == key:
            return
        g["last"] = key
        with lock:
            seen[name].update(txids)
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

    def own_feed(n, names):
        while not stop.is_set():
            try:
                t = gbt(n)
                ids = [x["txid"] for x in t["transactions"]]
                if n == "P":
                    with lock:
                        pool_templates.append((time.time(), ids))
                    time.sleep(rng.uniform(0.02, 0.08))
                for name in names:
                    push(name, t, ids)
            except Exception as e:  # noqa: BLE001
                print("feed", n, e, file=sys.stderr)
            stop.wait(1.0)

    def traffic():
        while not stop.is_set():
            if not utxos:
                return
            txid, n, v = utxos.pop(rng.randrange(len(utxos)))
            fee = rng.choice([300, 800, 2_000, 5_000])
            outs = [(v - fee, kl.P2WSH_TRUE)]
            if rng.random() < 0.3:
                outs.append((0, b"\x6a\x14" + rng.randbytes(20)))
            raw, new = kl.op_true_tx([(txid, n)], outs)
            try:
                rpc(rng.choice(["P", "P", "H1", "T"]), "sendrawtransaction", raw)
                utxos.append((new, 0, v - fee))
            except Exception:  # noqa: BLE001
                pass
            stop.wait(rng.uniform(0.3, 1.0))

    finders = []

    def blocks():
        while not stop.wait(rng.uniform(*BLOCK_GAP)):
            who = rng.choice(["P", "P", "H1", "H2"])
            try:
                rpc(who, "generatetodescriptor", 1, kl.DESC)
                finders.append(who)
            except Exception as e:  # noqa: BLE001
                print("block", who, e, file=sys.stderr)

    threads = [threading.Thread(target=own_feed, args=("H1", ["knots-h1"])),
               threading.Thread(target=own_feed, args=("H2", ["knots-h2"])),
               threading.Thread(target=own_feed, args=("T", list(FARM))),
               threading.Thread(target=own_feed, args=("P", [PROXY])),
               threading.Thread(target=traffic), threading.Thread(target=blocks)]
    for t in threads:
        t.daemon = True
        t.start()
    time.sleep(3)
    sc_log = open(os.path.join(RUN, "sidecar.log"), "w")
    sidecar = subprocess.Popen(["nice", "-n", "19", "python3", SIDECAR_PY, "-c", cfg_path, "run"], stdout=sc_log,
                               stderr=subprocess.STDOUT)
    procs.append(sidecar)
    time.sleep(SECS)
    sidecar.terminate()
    sidecar.wait(timeout=30)
    time.sleep(float(os.environ.get("SOV015_DRAIN", "55")))
    stop.set()
    for t in threads:
        t.join(timeout=30)

    # evidence
    ledger = [json.loads(line) for line in open(os.path.join(SIDECAR, "state", "canaries.jsonl"))]
    placed = [r for r in ledger if r["event"] == "placed"]
    status = json.loads(subprocess.check_output(["python3", SIDECAR_PY, "-c", cfg_path, "status"], text=True))
    kinds = {p["txid"]: p["kind"] for p in placed}
    placed_at = {p["txid"]: p["t"] for p in placed}
    pool_ids = {x for _, ids in pool_templates for x in ids}
    first_in_pool = {}
    for t, ids in pool_templates:
        for x in ids:
            first_in_pool.setdefault(x, t)
    leaks = sum(1 for x, k in kinds.items() if k == "own" and x in pool_ids)
    leaks += sum(1 for x, k in kinds.items() if k == "shared" and x in first_in_pool and first_in_pool[x] < placed_at[x] + SHARED_RELEASE)
    decoys = [x for x, k in kinds.items() if k == "decoy"]
    decoys_in_pool = sum(x in pool_ids for x in decoys)
    honest_decoys = {n: sum(x in seen[n] for x in decoys) for n in HONEST + FARM}
    # which gateway's own canary each C is, and which gateways' templates ever held it
    own_of = {p["txid"]: names_of.get(p["gateway"], p["gateway"]) for p in placed if p["kind"] == "own"}
    held = {n: {c for c in own_of if c in seen[n] and own_of[c] != n} for n in GATEWAYS}
    lags = [lag for p in placed for lag in (p.get("foreign_lags") or {}).values() if lag is not None]
    lags.sort()
    lag_stats = {"twins_acked": len(lags), "median": lags[len(lags) // 2] if lags else None,
                 "p99": lags[int(len(lags) * 0.99)] if lags else None, "max": lags[-1] if lags else None,
                 "over_max_lag": sum(x > MAX_LAG for x in lags)}
    with urllib.request.urlopen(STATS + "/sovereignty.json", timeout=60) as r:
        sov = json.load(r)
    rows = {names_of.get(g, g): f for g, f in (sov.get("gateways") or {}).items()}
    clusters = [sorted(names_of.get(g, g) for g in c) for c in sov.get("shared_sources") or []]

    # zero-leak key check (SOV-008): the canary key never in a wallet on the pool node
    iso = json.loads(subprocess.run(["python3", SIDECAR_PY, "-c", cfg_path, "check-isolation"], capture_output=True, text=True).stdout)
    ai = rpc("P", "getaddressinfo", canary_addr, wallet="pool")
    descs = json.dumps(rpc("P", "listdescriptors", True, wallet="pool"))
    signer = {"peers": rpc("S", "getconnectioncount"), "mempool": rpc("S", "getmempoolinfo")["size"]}
    key_ok = (iso.get("pool_wallets_knowing_it") == [] and not ai.get("ismine") and not ai.get("iswatchonly")
              and wif not in descs and signer == {"peers": 0, "mempool": 0})

    print(f"knots: {SECS:.0f} s + drain, height {rpc('P', 'getblockcount')}, blocks by {finders}")
    print(f"sidecar: rounds {status['rounds']}, delivered {status['delivered']}, delivery_failed {status['delivery_failed']}, "
          f"foreign twins {status.get('foreign')}, fees planned {status['fees_planned']} sat, paid {status['fees_paid']} sat")
    print(f"twin ack lag after C was sent (s): {json.dumps(lag_stats)}")
    print(f"canaries: {len(own_of)} own, {sum(k == 'shared' for k in kinds.values())} shared, {len(decoys)} decoys; "
          f"own/shared leaks into pool templates {leaks}; decoys in pool templates {decoys_in_pool}/{len(decoys)}; "
          f"decoys held by non-pool gateways {honest_decoys}; key isolation {'OK' if key_ok else 'FAIL'}")
    print(f"another gateway's own canary in a gateway's templates (any time): "
          f"{ {n: len(held[n]) for n in GATEWAYS} }")
    print(f"primed: shared_sources {clusters}; checks_refused {sov.get('store', {}).get('checks_refused')}")
    print("| gateway | checks | own hit/due | shared hit/due | decoys carried/due | foreign hit/due | independence | flagged | reasons | shared source with |")
    print("|---|---:|---:|---:|---:|---:|---:|---|---|---|")
    for name in GATEWAYS:
        f = rows.get(name) or {}
        peers = [names_of.get(g, g) for g in f.get("shared_source") or []]
        print(f"| {name} | {f.get('checks')} | {f.get('canary_hits')}/{f.get('canaries_due')} | {f.get('shared_hits')}/{f.get('shared_due')} | "
              f"{f.get('decoys_carried')}/{f.get('decoys_due')} | {f.get('foreign_hits')}/{f.get('foreign_due')} | {f.get('independence')} | "
              f"{f.get('flagged')} | {','.join(f.get('flag_reasons') or [])} | {','.join(peers)} |")
    by_source = {n: {names_of.get(s, s): v for s, v in ((rows.get(n) or {}).get("foreign") or {}).items()} for n in GATEWAYS}
    os.makedirs(os.path.join(RUN, "results"), exist_ok=True)
    out = os.path.join(RUN, "results", f"foreign-{int(time.time())}.json")
    json.dump({"secs": SECS, "finders": finders, "leaks": leaks, "decoys_in_pool": decoys_in_pool, "decoys": len(decoys),
               "rows": {n: rows.get(n) for n in GATEWAYS}, "foreign_by_source": by_source, "clusters": clusters,
               "held_foreign": {n: len(held[n]) for n in GATEWAYS}, "lags": lag_stats, "key_ok": key_ok,
               "sidecar": {k: status.get(k) for k in ("rounds", "delivered", "delivery_failed", "foreign", "fees_planned", "fees_paid")},
               "checks_refused": sov.get("store", {}).get("checks_refused")}, open(out, "w"), indent=1)
    print("result", out)

    problems = []
    if sorted(FARM) not in clusters:
        problems.append(f"the three T-fed gateways are not one cluster: {clusters}")
    for h in HONEST:
        f = rows.get(h) or {}
        if f.get("flagged") or f.get("shared_source") or any(h in c for c in clusters):
            problems.append(f"{h}: flagged={f.get('flagged')} {f.get('flag_reasons')} shared_source={f.get('shared_source')}")
        if f.get("evidence") != "canary" or not f.get("foreign_due"):
            problems.append(f"{h}: evidence={f.get('evidence')} foreign_due={f.get('foreign_due')}")
    if not (rows.get(PROXY) or {}).get("flagged"):
        problems.append(f"{PROXY} not flagged: {(rows.get(PROXY) or {}).get('flag_reasons')}")
    if any(PROXY in c for c in clusters):
        problems.append(f"{PROXY} clustered: {clusters}")
    if leaks:
        problems.append(f"{leaks} own/shared canaries leaked into pool templates")
    if not key_ok:
        problems.append("key isolation failed")
    if status["delivery_failed"]:
        problems.append(f"{status['delivery_failed']} own canaries not delivered")
    if problems:
        print("KNOTS_FAIL", *problems, sep="\n  ")
        sys.exit(1)
    print("KNOTS_OK")


if __name__ == "__main__":
    main()
