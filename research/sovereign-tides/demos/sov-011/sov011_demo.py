#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-011 readiness demo: XBT-NTA activation safety for DATUM gateways, on Knots regtest.

Two NTA-patched Knots 29.4.2 nodes, primed (s0 + sov-011), three gateways, stratum-grind miners
(--threads 2, one at a time, each under `timeout` in its own process group).

  node P   the pool's node (primed); node G the gateways' node. Both enforce nta@NTA_HEIGHT,
           a few hundred blocks past the setup height, so the whole run before step (c) is
           inside the default 2016-block probe window. A trickle of transactions keeps the
           mempool busy, so gateway jobs carry transactions like mainnet ones.
  gw-A     lazarus-gateway with nta_key_file: advertises nta-v1, signs NTA for its miner's P2TR
  gw-C     datum_gateway (FlyTheElephant1 a5f28aa) + the split-only patch: advertises nta-v1
  stock    lazarus-gateway with LAZARUS_GATEWAY_EMULATE_STOCK_V041=1 (called "emu" in the
           checks): reads coinbasers exactly as OCEAN v0.4.1 (datum_coinbaser.c:795: stop at the
           first script over 64 bytes), no nta-v1

  primed   nta-height, nta-probe-every-secs = PROBE_EVERY (the default is 3600: one probe per
           gateway per hour), nta-unready-policy = refuse; the TIDES window is seeded through
           the demo ingest so every coinbaser pays real payees ahead of the probe.

Optional REAL gateways (SOV-020), unmodified builds named in cfg "real" (readiness-demo.sh sets
them from STOCK_S, STOCK_I and OCEAN). What they do is recorded as observations (OBS lines and
observations.json), not pass/fail checks:
  stock-S  datum_gateway, the StartOS pin iohzrd 7491a50
  stock-I  datum_gateway, iohzrd c031568
  ocean    OCEAN datum_gateway 5b06123 (v0.4.1). No miner: it mines SHA256d, and its
           getblocktemplate fails on XBT without the blake2b rule, so it never asks for work.
The XBT lineages throw a coinbaser with a 70-byte script away whole ("Script length (70) is
invalid. Using default/empty") and mine pool-only jobs. With the sov-011 series' discard fix
(mit/0005, mit/0006) primed marks such a gateway nta-unready after two pool-only strikes before
activation, or on its first unattested pool-only share after it, and refuses it.

Steps
  a. hello: gw-A and gw-C advertise nta-v1 and are ready; stock is unknown. /nta.json readiness
     says so, with blocks until activation.
  b. before activation every gateway's coinbasers end with a 70-byte probe, advertiser or not
     (the flag is a claim; the first split-only C build to make it dropped every attestation).
     The stock miner's first full-split share has every payee and no probe -> nta-unready; its
     shares are all accepted. gw-C's miner's share keeps the probe -> ready on evidence.
     Then (if set) each real gateway mines until a verdict or STOCK_SHARES shares.
     Readiness metric: 2 ready, 1 unready, with each one's share of work.
  c. activation: P mines to nta-height - 1 by RPC. primed refuses the stock gateway: a DATUM
     server message and the 0x4e notice, both in its log; its reconnects are refused too and it
     holds no session. The metric shows 0 blocks to activation.
  d. after activation: a miner on gw-A finds a block, and one on gw-C does. Each is accepted by
     P and G and carries one attestation per payee.
  e. (stock-S set and still served) its miner mines after activation: what happens is recorded.
Prints RESULT PASS|FAIL.

usage: sov011_demo.py <cfg.json>   (written by readiness-demo.sh)
"""
import base64, hashlib, json, os, random, re, shutil, signal, subprocess, sys, threading, time
import urllib.error, urllib.request

CFG = json.load(open(sys.argv[1]))
BITCOIND, FUNC, RUN, RESULTS, BASE = CFG["bitcoind"], CFG["func"], CFG["run"], CFG["results"], CFG["base"]
PRIMED, GATEWAY, GRIND, CGW = CFG["primed"], CFG["gateway"], CFG["grind"], CFG["cgw"]
REAL = CFG.get("real", {})   # name -> (binary, lineage)
OBS = {}
STOCK_SECS = int(os.environ.get("SOV011_STOCK_SECS", "900"))
STOCK_SHARES = int(os.environ.get("SOV011_STOCK_SHARES", "3"))
sys.path.insert(0, FUNC)
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey  # noqa: E402
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey  # noqa: E402
from cryptography.hazmat.primitives.serialization import Encoding, PrivateFormat, PublicFormat, NoEncryption  # noqa: E402
from test_framework.key import compute_xonly_pubkey  # noqa: E402
from test_framework.segwit_addr import encode_segwit_address  # noqa: E402
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "sov-015"))
import knots_lab as kl  # noqa: E402

SETUP = int(os.environ.get("SOV011_SETUP_HEIGHT", "120"))
NTA_HEIGHT = SETUP + int(os.environ.get("SOV011_NTA_AHEAD", "300"))
PROBE_EVERY = int(os.environ.get("SOV011_PROBE_EVERY", "10"))
MINE_TIMEOUT = int(os.environ.get("SOV011_MINE_TIMEOUT", "1500"))
THREADS = os.environ.get("SOV011_MINER_THREADS", "2")
FAILS, CHECKS = [], []
procs = {}
nodes = {}


def check(ok, what):
    CHECKS.append({"ok": bool(ok), "what": what})
    print(("PASS  " if ok else "FAIL  ") + what, flush=True)
    if not ok:
        FAILS.append(what)


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


# ---------------------------------------------------------------- ports
P_NODE = {"P": 0, "G": 1}                       # rpc base+2i, p2p base+2i+1
PRIME_PORT, STATS_PORT = BASE + 15, BASE + 16
GW_PORTS = {"gw-A": (BASE + 20, BASE + 21), "stock": (BASE + 26, BASE + 27), "gw-C": (BASE + 30, BASE + 31),
            "stock-S": (BASE + 34, BASE + 35), "stock-I": (BASE + 38, BASE + 39), "ocean": (BASE + 42, BASE + 43)}
STATS = f"http://127.0.0.1:{STATS_PORT}"


# ---------------------------------------------------------------- keys (deterministic)
def secret(label):
    return hashlib.sha256(b"sov-011/" + label.encode()).digest()


def xonly(label):
    return compute_xonly_pubkey(secret(label))[0]


def p2tr_addr(label):
    return encode_segwit_address("bc", 1, xonly(label))


def p2tr_script(label):
    return bytes([0x51, 0x20]) + xonly(label)


LABELS = {"pool": "pool", "A": "A", "C": "C", "stock-m": "stock-miner", "S": "stock-S-miner", "I": "stock-I-miner"}
ADDR = {k: p2tr_addr(v) for k, v in LABELS.items()}
SCRIPT = {k: p2tr_script(v) for k, v in LABELS.items()}
WHO = {v.hex(): k for k, v in SCRIPT.items()}


def write_secret(path, data_hex):
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, data_hex.encode())
    os.close(fd)


def identity(label):
    """(ed25519 pk, seed, x25519 pk, x25519 sk) for a gateway's DATUM identity."""
    ed = Ed25519PrivateKey.from_private_bytes(secret(label + "-identity"))
    seed = ed.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    pk = ed.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    x = X25519PrivateKey.from_private_bytes(secret(label + "-box"))
    xpk = x.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    xsk = x.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    return pk, seed, xpk, xsk


def lazarus_identity(label, path):
    """lazarus-gateway identity key file: ed_pk | ed_sk(seed|pk) | x_pk | x_sk, hex."""
    pk, seed, xpk, xsk = identity(label)
    write_secret(path, (pk + seed + pk + xpk + xsk).hex())
    return pk.hex()


G_HEX = {}      # gateway name -> the 16-hex `gateway=` primed logs and /nta.json keys by


# ---------------------------------------------------------------- nodes
def start_node(name):
    i = P_NODE[name]
    d = os.path.join(RUN, f"knots-{name}")
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    args = [f"-datadir={d}", "-regtest", "-server", f"-rpcport={BASE + 2 * i}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-testactivationheight=blake2b@101", f"-testactivationheight=nta@{NTA_HEIGHT}",
            "-rdtsexpiry=4102444800", "-disablewallet", f"-port={BASE + 2 * i + 1}", "-bind=127.0.0.1", "-listen=1",
            "-dnsseed=0", "-fixedseeds=0", "-listenonion=0", "-discover=0", "-whitelist=noban@127.0.0.1",
            "-printtoconsole=0", "-par=1", "-dbcache=64"]
    args += [f"-addnode=127.0.0.1:{BASE + 2 * P_NODE[p] + 1}" for p in P_NODE if p != name]
    nodes[name] = {"d": d, "port": BASE + 2 * i, "cookie": os.path.join(d, "regtest", ".cookie")}
    procs[f"node-{name}"] = subprocess.Popen([BITCOIND] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def rpc(name, method, *params, timeout=60):
    auth = base64.b64encode(open(nodes[name]["cookie"], "rb").read().strip()).decode()
    body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{nodes[name]['port']}/", body, {"Authorization": "Basic " + auth})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.load(r)["result"]
    except urllib.error.HTTPError as e:
        raise RuntimeError(json.load(e).get("error")) from None


def wait_for(pred, secs=30, step=0.2):
    end = time.time() + secs
    while time.time() < end:
        try:
            if pred():
                return True
        except Exception:  # noqa: BLE001
            pass
        time.sleep(step)
    return False


def tip(n):
    return rpc(n, "getbestblockhash")


def height(n="P"):
    return rpc(n, "getblockcount")


def synced():
    return len({tip(n) for n in nodes}) == 1


# ---------------------------------------------------------------- primed
def get(path):
    with urllib.request.urlopen(STATS + path, timeout=30) as r:
        return json.load(r)


def post(path, body):
    token = open(os.path.join(RUN, "prime", "sovereignty.token")).read().strip()
    req = urllib.request.Request(STATS + path, json.dumps(body).encode(),
                                 {"Content-Type": "application/json", "X-Sovereignty-Token": token})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def prime_toml():
    d = os.path.join(RUN, "prime")
    return f"""listen = "127.0.0.1:{PRIME_PORT}"
stats-listen = "127.0.0.1:{STATS_PORT}"
data-dir = "{d}"
payout-address = "{ADDR['pool']}"
network = "mainnet"  # the rig convention: gateways take only bc1 usernames; scripts are identical
rpc = "http://127.0.0.1:{nodes['P']['port']}"
rpc-cookie = "{nodes['P']['cookie']}"
poll = 0.25
min-diff = 1
fee-bps = 0
stratum-fee-bps = 2500
min-payout = 1000
window-min-work = 1000000000000
house-loopback = false
sovereignty-mode = "observe"  # only for the demo ingest that seeds the window
sovereignty-demo = true
headline = "SOV-011 regtest"
nta-height = {NTA_HEIGHT}
nta-pool-key-file = "{os.path.join(RUN, 'keys', 'pool.nta')}"
nta-probe-every-secs = {PROBE_EVERY}
nta-unready-policy = "refuse"
"""


def start_prime():
    d = os.path.join(RUN, "prime")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "prime.toml")
    open(path, "w").write(prime_toml())
    shutil.copy(path, os.path.join(RESULTS, "prime.toml"))
    env = dict(os.environ, RUST_LOG="info,primed::session=info")
    procs["prime"] = subprocess.Popen([PRIMED, "-c", path, "run"], env=env,
                                      stdout=open(os.path.join(RESULTS, "prime.log"), "a"), stderr=subprocess.STDOUT)

    def up():
        if procs["prime"].poll() is not None:
            raise SystemExit(f"primed exited; see {RESULTS}/prime.log")
        with urllib.request.urlopen(STATS + "/healthz", timeout=2) as r:
            return r.status == 200
    if not wait_for(up, 30):
        raise SystemExit("primed did not come up")


def prime_pubkey():
    d = os.path.join(RUN, "prime")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "prime.toml")
    open(path, "w").write(prime_toml())
    return subprocess.check_output([PRIMED, "-c", path, "pubkey"], text=True).strip().split()[-1]


def stop_proc(name, sig=signal.SIGTERM):
    p = procs.pop(name, None)
    if p is None:
        return
    try:
        p.send_signal(sig)
        p.wait(timeout=30)
    except Exception:  # noqa: BLE001
        p.kill()


def prime_log():
    with open(os.path.join(RESULTS, "prime.log"), errors="replace") as f:
        return f.read()


def gw_log(name):
    try:
        with open(os.path.join(RESULTS, f"{name}.log"), errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return ""


def readiness():
    time.sleep(0.6)
    return get("/nta.json")["readiness"]


def row(r, name):
    return next((x for x in r["list"] if x["gateway"] == G_HEX[name]), None)


def clients():
    time.sleep(0.6)
    return get("/stats.json").get("clients", [])


def client(name):
    """The gateway's live session. stats.json also lists a gateway that is gone but found a block,
    as a row with no session (id 0, connected_ts 0): not a session."""
    return next((c for c in clients() if c.get("gateway") == G_HEX[name] and c.get("connected_ts", 0) > 0), None)


# ---------------------------------------------------------------- gateways
def start_lazarus(name, pool_pk):
    sp, ap = GW_PORTS[name]
    d = os.path.join(RUN, name)
    os.makedirs(d, exist_ok=True)
    cfg = {"rpc": f"http://127.0.0.1:{nodes['G']['port']}", "rpc_cookie": nodes["G"]["cookie"],
           "prime_host": "127.0.0.1", "prime_port": PRIME_PORT, "pool_pubkey": pool_pk, "coinbase_tag": "Lazarus",
           "profile": "regtest", "stratum_listen": f"127.0.0.1:{sp}", "api_listen": f"127.0.0.1:{ap}", "vardiff_min": 1,
           "identity_key_file": os.path.join(RUN, "keys", f"{name}.identity")}
    if name == "gw-A":
        cfg["nta_key_file"] = os.path.join(RUN, "keys", "A.nta")
    env = dict(os.environ, RUST_LOG="info")
    if name == "stock":
        env["LAZARUS_GATEWAY_EMULATE_STOCK_V041"] = "1"
    path = os.path.join(d, "gateway.json")
    json.dump(cfg, open(path, "w"), indent=1)
    shutil.copy(path, os.path.join(RESULTS, f"{name}.json"))
    procs[name] = subprocess.Popen([GATEWAY, "--config", path], env=env,
                                   stdout=open(os.path.join(RESULTS, f"{name}.log"), "a"), stderr=subprocess.STDOUT)


def start_gwc(pool_pk):
    """The split-only patched datum_gateway on node G."""
    sp, _ap = GW_PORTS["gw-C"]
    d = os.path.join(RUN, "gw-C")
    os.makedirs(d, exist_ok=True)
    out = open(os.path.join(RESULTS, "gw-C.log"), "a")
    cfg = {
        "bitcoind": {"rpccookiefile": nodes["G"]["cookie"], "rpcurl": f"http://127.0.0.1:{nodes['G']['port']}",
                     "notify_fallback": True, "work_update_seconds": 5},
        "stratum": {"listen_addr": "127.0.0.1", "listen_port": sp, "vardiff_min": 1},
        "mining": {"pool_address": ADDR["C"], "coinbase_tag_primary": "Lazarus", "coinbase_tag_secondary": "gw-C",
                   "pow_algorithm": "auto"},
        "api": {"listen_port": 0},
        "logger": {"log_to_console": True, "log_to_file": False, "log_level_console": 1},
        "datum": {"pool_host": "127.0.0.1", "pool_port": PRIME_PORT, "pool_pubkey": pool_pk,
                  "pooled_mining_only": True, "identity_key_file": os.path.join(d, "identity.key"),
                  "lzt1_attest": False},
    }
    path = os.path.join(d, "datum_gateway.json")
    json.dump(cfg, open(path, "w"), indent=1)
    shutil.copy(path, os.path.join(RESULTS, "gw-C.json"))
    procs["gw-C"] = subprocess.Popen([CGW, "--config", path], cwd=d, stdout=out, stderr=subprocess.STDOUT)
    # It creates its identity (0600) on first start; its ed25519 key is what primed logs.
    key = os.path.join(d, "identity.key")
    wait_for(lambda: "ed25519_pk" in open(key).read(), 30)
    G_HEX["gw-C"] = next(l.split()[1] for l in open(key) if l.startswith("ed25519_pk"))[:16]


def start_real(name, pool_pk):
    """An unmodified datum_gateway build. Stock regenerates its
    DATUM identity at every start (datum_protocol.c:1926), so its key is read off primed's sessions."""
    binary, lineage = REAL[name]
    sp, ap = GW_PORTS[name]
    d = os.path.join(RUN, name)
    os.makedirs(d, exist_ok=True)
    mining = {"pool_address": ADDR[{"stock-S": "S", "stock-I": "I"}.get(name, "pool")], "coinbase_tag_primary": "Lazarus",
              "coinbase_tag_secondary": name}
    if lineage in ("iohzrd", "fte"):
        mining.update(blake2b_activation_height=101, blake2b_headline="Lazarus")
    cfg = {
        "bitcoind": {"rpccookiefile": nodes["G"]["cookie"], "rpcurl": f"http://127.0.0.1:{nodes['G']['port']}",
                     "notify_fallback": True, "work_update_seconds": 5},
        "stratum": {"listen_addr": "127.0.0.1", "listen_port": sp, "vardiff_min": 1},
        "mining": mining,
        "api": {"listen_addr": "127.0.0.1", "listen_port": ap, "admin_password": ""},
        "logger": {"log_to_console": True, "log_to_file": False, "log_level_console": 1},
        "datum": {"pool_host": "127.0.0.1", "pool_port": PRIME_PORT, "pool_pubkey": pool_pk,
                  "pool_pass_workers": True, "pool_pass_full_users": True, "pooled_mining_only": True,
                  "protocol_global_timeout": 60},
    }
    path = os.path.join(d, "datum_gateway.json")
    json.dump(cfg, open(path, "w"), indent=1)
    shutil.copy(path, os.path.join(RESULTS, f"{name}.json"))
    before = {c.get("gateway") for c in clients()}
    out = open(os.path.join(RESULTS, f"{name}.log"), "a")
    procs[name] = subprocess.Popen([binary, "-c", path], cwd=d, stdout=out, stderr=subprocess.STDOUT)
    new = []

    def arrived():
        new[:] = [c for c in clients() if c.get("gateway") and c.get("gateway") not in before
                  and c.get("gateway") not in G_HEX.values() and c.get("connected_ts", 0) > 0]
        return bool(new)
    if wait_for(arrived, 60, 1):
        G_HEX[name] = new[0]["gateway"][:16]
        OBS[f"{name}: session"] = {"gateway": G_HEX[name], "user_agent": new[0].get("user_agent")}
        log(f"  {name} ({lineage}) has a session: gateway={G_HEX[name]} ua={new[0].get('user_agent')!r}")
    else:
        OBS[f"{name}: session"] = "none within 60 s"
        log(f"  {name} ({lineage}): no session within 60 s; see {name}.log")


def obs(key, val):
    OBS[key] = val
    print(f"OBS   {key}: {json.dumps(val) if not isinstance(val, str) else val}", flush=True)


# ---------------------------------------------------------------- miners
MINER = {"miner-A": ("gw-A", ADDR["A"]), "stock-m": ("stock", ADDR["stock-m"]), "miner-C": ("gw-C", ADDR["C"]),
         "miner-S": ("stock-S", ADDR["S"]), "miner-I": ("stock-I", ADDR["I"])}


def run_miner(miner, until, secs):
    """Run one stratum-grind until `until()` holds (or the timeout); kill its whole group."""
    gw, user = MINER[miner]
    port = GW_PORTS[gw][0]
    out = open(os.path.join(RESULTS, f"miner-{miner}.log"), "a")
    secs = min(int(secs), 1800)
    cmd = ["timeout", "-k", "10", str(secs), GRIND, "--host", "127.0.0.1", "--port", str(port), "--user", user,
           "--threads", THREADS]
    t0 = time.time()
    ok = False
    # A gateway may drop a stratum client that found nothing for a while (lazarus-gateway did at
    # ~600 s); restart the miner until `until()` or the time is up.
    while not ok and time.time() - t0 < secs:
        p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
        procs[f"miner-{miner}"] = p
        try:
            wait_for(lambda: until() or p.poll() is not None, secs - (time.time() - t0) + 5, 0.5)
            ok = until()
            if not ok and p.poll() is not None:
                log(f"  {miner} exited after {time.time() - t0:.0f}s with no verdict yet; restarting it")
        finally:
            kill_group(p)
            procs.pop(f"miner-{miner}", None)
    return ok, time.time() - t0


def kill_group(p):
    for s in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(p.pid, s)
        except OSError:
            pass
        try:
            p.wait(timeout=5)
            break
        except Exception:  # noqa: BLE001
            pass


# ---------------------------------------------------------------- blocks
def coinbase(h, node="P"):
    b = rpc(node, "getblock", rpc(node, "getblockhash", h), 2)
    outs = []
    for o in b["tx"][0]["vout"]:
        s = o["scriptPubKey"]["hex"]
        kind = "nta" if s.startswith("6a444e544102") and len(s) == 140 else ("opret" if s.startswith("6a") else WHO.get(s, s[:16]))
        outs.append({"to": kind, "sats": round(o["value"] * 1e8), "script": s})
    return {"height": h, "hash": b["hash"], "outputs": outs}


def nta_shape(cb):
    payees = []
    for o in cb["outputs"]:
        if o["to"] in ("nta", "opret") or o["sats"] == 0:
            continue
        if o["script"] not in payees:
            payees.append(o["script"])
    return payees, sum(1 for o in cb["outputs"] if o["to"] == "nta")


def paid(cb):
    return {o["to"] for o in cb["outputs"] if o["sats"] > 0}


# ---------------------------------------------------------------- main
def main():
    os.makedirs(os.path.join(RUN, "keys"), exist_ok=True)
    os.makedirs(RESULTS, exist_ok=True)
    for label, f in (("pool", "pool.nta"), ("A", "A.nta")):
        write_secret(os.path.join(RUN, "keys", f), secret(label).hex())
    for name in ("gw-A", "stock"):
        G_HEX[name] = lazarus_identity(name, os.path.join(RUN, "keys", f"{name}.identity"))[:16]
    os.makedirs(os.path.join(RUN, "gw-C"), exist_ok=True)
    for n in P_NODE:
        start_node(n)
    try:
        return run()
    finally:
        for name in list(procs):
            if name.startswith("miner-"):
                kill_group(procs.pop(name))
        for name in ["gw-A", "gw-C", "stock", *REAL, "prime"]:
            stop_proc(name)
        for name in list(procs):
            stop_proc(name)


blocks = []
STOP = threading.Event()


def run():
    for n in nodes:
        if not wait_for(lambda: rpc(n, "getblockcount") is not None, 60):
            raise SystemExit(f"node {n} did not start")
    for n in nodes:
        for p in P_NODE:
            if p != n:
                try:
                    rpc(n, "addnode", f"127.0.0.1:{BASE + 2 * P_NODE[p] + 1}", "onetry")
                except Exception:  # noqa: BLE001
                    pass
    check(wait_for(lambda: all(rpc(n, "getconnectioncount") >= 1 for n in nodes), 60), "P and G are peers")
    log("node", rpc("P", "getnetworkinfo")["subversion"], f"nta@{NTA_HEIGHT} on P and G")
    # Mine the setup to P2WSH(OP_TRUE) and keep a trickle of transactions in the mempool, so
    # gateway jobs carry transactions like mainnet ones (primed's discard check skips
    # transaction-less jobs: stock's per-height empty job is one).
    rpc("P", "generatetodescriptor", SETUP, kl.DESC)
    check(wait_for(synced, 120), f"P and G synced at height {SETUP}; NTA from {NTA_HEIGHT}")
    cb = rpc("P", "getblock", rpc("P", "getblockhash", 1), 2)["tx"][0]
    v = round(cb["vout"][0]["value"] * 1e8)
    raw, txid = kl.op_true_tx([(cb["txid"], 0)], [((v - 100_000) // 200, kl.P2WSH_TRUE)] * 200)
    rpc("P", "sendrawtransaction", raw, 0)
    pool = [(txid, i, (v - 100_000) // 200) for i in range(200)]
    rng = random.Random(20)

    def traffic():
        while pool and not STOP.is_set():
            t, n, val = pool.pop(0)
            fee = rng.choice([500, 1500, 4000])
            raw, new = kl.op_true_tx([(t, n)], [(val - fee, kl.P2WSH_TRUE)])
            try:
                rpc(rng.choice(["P", "G"]), "sendrawtransaction", raw)
                pool.append((new, 0, val - fee))
            except Exception:  # noqa: BLE001
                pass
            STOP.wait(rng.uniform(3, 8))
    threading.Thread(target=traffic, daemon=True).start()

    pool_pk = prime_pubkey()
    start_prime()
    # Seed the TIDES window (demo ingest) so each coinbaser pays real payees ahead of any probe.
    for k, w in {"A": 300, "C": 300, "stock-m": 300, "S": 300, "I": 300}.items():
        r = post("/sovereignty/credit", {"identity": ADDR[k], "gateway": k, "work": w, "height": SETUP, "stratum": False})
        assert r.get("ok"), r
    for g in ("gw-A", "stock"):
        start_lazarus(g, pool_pk)
    start_gwc(pool_pk)
    ok = wait_for(lambda: all(client(g) for g in ("gw-A", "stock", "gw-C")), 90, 1)
    check(ok, f"primed has a session from gw-A, stock and gw-C; "
              f"gateway keys {G_HEX}")
    if not ok:
        return finish()
    for g in REAL:
        start_real(g, pool_pk)

    # ------------------------------------------------------------ (a) hello
    log("== (a) hellos")
    r = readiness()
    json.dump(r, open(os.path.join(RESULTS, "readiness-a.json"), "w"), indent=1)
    ra, rc, rs = row(r, "gw-A"), row(r, "gw-C"), row(r, "stock")
    check(ra and ra["status"] == "ready" and ra["evidence"] == "advertised nta-v1",
          f"(a) gw-A (lazarus-gateway) advertised nta-v1: ready ({ra and ra['user_agent']})")
    check(rc and rc["status"] == "ready" and rc["evidence"] == "advertised nta-v1",
          f"(a) gw-C (split-only datum_gateway) advertised nta-v1: ready ({rc and rc['user_agent']})")
    check(rs and rs["status"] == "unknown" and not rs["advertised"],
          f"(a) stock (v0.4.1 coinbaser parse, no nta-v1) is unknown until probed ({rs and rs['status']})")
    check(r["probing"] and not r["active"] and r["blocks_until_activation"] == NTA_HEIGHT - (SETUP + 1),
          f"(a) /nta.json readiness: probing, {r['blocks_until_activation']} blocks until activation at {NTA_HEIGHT}")

    # ------------------------------------------------------------ (b) probe
    log(f"== (b) probe: stock-m mines on the stock gateway until primed has a verdict")
    def verdict():
        x = row(readiness(), "stock")
        return x is not None and x["status"] != "unknown"
    ok, secs = run_miner("stock-m", verdict, MINE_TIMEOUT)
    log(f"  verdict after {secs:.0f}s ok={ok}")
    r = readiness()
    json.dump(r, open(os.path.join(RESULTS, "readiness-b.json"), "w"), indent=1)
    rs, ra, rc = row(r, "stock"), row(r, "gw-A"), row(r, "gw-C")
    plog = prime_log()
    probes = [l for l in plog.splitlines() if "NTA probe on coinbaser" in l]
    check(any(G_HEX["stock"] in l for l in probes) and rs["probes"]["sent"] >= 1,
          f"(b) primed put a 70-byte probe at the end of the stock gateway's coinbasers ({rs['probes']['sent']} probes)")
    check(ra["probes"]["sent"] >= 1 and rc["probes"]["sent"] >= 1,
          f"(b) the advertisers are probed too (gw-A {ra['probes']['sent']}, gw-C {rc['probes']['sent']})")
    applied = re.findall(r"coinbaser applied value=\d+ outputs=(\d+)", gw_log("stock"))
    check(rs["status"] == "nta-unready" and rs["evidence"] == "probe dropped" and "NTA probe DROPPED" in plog,
          f"(b) stock kept every payee and dropped the probe: nta-unready before activation "
          f"(its coinbasers held {sorted(set(applied))} outputs after its v0.4.1 parse)")
    cs = client("stock") or {}
    check(cs.get("accepted", 0) >= 1 and cs.get("rejected", 1) == 0,
          f"(b) the probe cost the stock gateway nothing: {cs.get('accepted')} shares accepted, {cs.get('rejected')} rejected")
    log("== (b) probe: miner-C mines on gw-C (split-only datum_gateway) until primed has a verdict")
    def verdict_c():
        x = row(readiness(), "gw-C")
        return x is not None and x["evidence"] != "advertised nta-v1"
    ok, secs = run_miner("miner-C", verdict_c, MINE_TIMEOUT)
    log(f"  verdict after {secs:.0f}s ok={ok}")
    r = readiness()
    rc = row(r, "gw-C")
    check(rc["status"] == "ready" and rc["evidence"] == "probe kept" and rc["probes"]["kept"] >= 1,
          f"(b) gw-C (split-only datum_gateway) kept the 70-byte probe after its payees: ready on evidence "
          f"({rc['evidence']})")
    # ---- (b') the REAL stock gateways: mine on each until primed has a verdict or it has
    # STOCK_SHARES accepted shares or STOCK_SECS pass. Everything here is an observation.
    for g, m in (("stock-S", "miner-S"), ("stock-I", "miner-I")):
        if g not in REAL or g not in G_HEX:
            continue
        log(f"== (b') probe: {m} mines on {g} ({REAL[g][1]}, unmodified) until a verdict or {STOCK_SHARES} shares")
        def done(g=g):
            x, c = row(readiness(), g), client(g) or {}
            return (x is not None and x["status"] != "unknown") or c.get("accepted", 0) >= STOCK_SHARES
        ok, secs = run_miner(m, done, STOCK_SECS)
        x, c = row(readiness(), g) or {}, client(g) or {}
        glog = gw_log(g)
        obs(f"(b') {g} after {secs:.0f}s of mining", {
            "status": x.get("status"), "evidence": x.get("evidence"), "advertised": x.get("advertised"),
            "user_agent": x.get("user_agent"), "probes": x.get("probes"),
            "accepted": c.get("accepted"), "rejected": c.get("rejected"),
            "pool_only_shares": c.get("pool_only_shares"), "pool_only_full_jobs": c.get("pool_only_full_jobs"),
            "gateway_log_script_len_70_invalid": glog.count("Script length (70) is invalid"),
            "gateway_log_using_default_empty": glog.count("Using default/empty"),
            "primed_probe_lines": sum(1 for l in prime_log().splitlines() if "NTA probe" in l and G_HEX[g] in l),
            "primed_pool_only_full_warning": any("FULL jobs whose coinbase pays only the pool" in l and G_HEX[g] in l
                                                for l in prime_log().splitlines())})
    json.dump(r := readiness(), open(os.path.join(RESULTS, "readiness-b.json"), "w"), indent=1)
    ra, rc, rs = row(r, "gw-A"), row(r, "gw-C"), row(r, "stock")
    check(ra["status"] == "ready" and rc["status"] == "ready" and rs["status"] == "nta-unready"
          and r["hashrate_share"]["unready"] is not None and r["hashrate_share"]["unready"] > 0,
          f"(b) readiness metric before activation: gw-A/gw-C ready, emu unready; all gateways {r['gateways']}, "
          f"work share {r['hashrate_share']}, {r['blocks_until_activation']} blocks to go")
    pre_h = height()
    if pre_h > SETUP:
        cb = coinbase(pre_h)
        payees, atts = nta_shape(cb)
        blocks.append({"phase": "b", "height": pre_h, "paid": sorted(paid(cb)), "atts": atts})
        log(f"  (b) a pre-activation block was found meanwhile at {pre_h}: pays {sorted(paid(cb))}")

    # ------------------------------------------------------------ (c) activation
    log(f"== (c) activation: P mines to {NTA_HEIGHT - 1} (next block is nta-height)")
    marks = len(prime_log())
    rpc("P", "generatetodescriptor", NTA_HEIGHT - 1 - height(), "raw(51)")
    check(wait_for(synced, 120) and height() == NTA_HEIGHT - 1, f"(c) P and G at {NTA_HEIGHT - 1}")
    refused = wait_for(lambda: "refused: nta-unready" in prime_log()[marks:], 60, 0.5)
    time.sleep(12)   # let the stock gateway reconnect and be refused again
    plog = prime_log()[marks:]
    slog = gw_log("stock")
    check(refused and G_HEX["stock"] in "".join(l for l in plog.splitlines() if "refused: nta-unready" in l),
          "(c) primed refused the stock gateway at activation, with an operator-facing reason in its log")
    check("Prime message: Prime refuses this gateway from block" in slog and "datum_coinbaser.c:795" in slog,
          "(c) the stock gateway logged Prime's DATUM server message (proto cmd 7) with the reason")
    check("Prime refuses this gateway (nta-unready)" in slog,
          "(c) and the 0x4e notice (a stock v0.4.1 logs it as an unknown mining command, 'Perhaps you need to upgrade')")
    r = readiness()
    json.dump(r, open(os.path.join(RESULTS, "readiness-c.json"), "w"), indent=1)
    rs = row(r, "stock")
    check(rs["refusals"] >= 2 and client("stock") is None,
          f"(c) its reconnects are refused too ({rs['refusals']} refusals) and it holds no session: no pooled work")
    check(r["active"] and r["blocks_until_activation"] == 0 and client("gw-A") and client("gw-C"),
          "(c) readiness shows NTA active, 0 blocks to go; gw-A and gw-C are still served")
    time.sleep(8)
    for g in REAL:
        if g not in G_HEX:
            continue
        glog = gw_log(g)
        x = row(readiness(), g) or {}
        obs(f"(c) {g} at activation", {
            "status": x.get("status"), "evidence": x.get("evidence"), "refusals": x.get("refusals"),
            "has_session": client(g) is not None,
            "primed_refused": any("refused: nta-unready" in l and G_HEX[g] in l for l in prime_log()[marks:].splitlines()),
            "logged_server_message": "DATUM Server message: Prime refuses" in glog,
            "logged_0x4e_notice": "Received unknown mining command 4E" in glog})

    # ------------------------------------------------------------ (d) the ready ones mine valid blocks
    miners = ["miner-A", "miner-C"]
    for m in miners:
        want = height() + 1
        log(f"== (d) block {want}: {m}")
        ok, secs = run_miner(m, lambda: height() >= want, MINE_TIMEOUT)
        wait_for(synced, 30)
        row_ = {"phase": "d", "miner": m, "ok": ok, "secs": round(secs, 1), "height": height()}
        if ok:
            cb = coinbase(want)
            payees, atts = nta_shape(cb)
            row_.update(paid=sorted(paid(cb)), atts=atts, payees=len(payees),
                        accepted_by=[n for n in nodes if rpc(n, "getblockhash", want) == cb["hash"]])
        blocks.append(row_)
        gw = MINER[m][0]
        check(ok and len(row_.get("accepted_by", [])) == 2,
              f"(d) {gw}-found block {want} (NTA active) accepted by P and G in {secs:.0f}s")
        if ok:
            check(atts >= 1 and atts == row_["payees"] and "pool" in row_["paid"],
                  f"(d) {gw}'s block carries one attestation per payee ({atts} for {row_['paid']})")
    # ---- (e) a real stock gateway that is still served after activation: what does it mine?
    g, m = "stock-S", "miner-S"
    if g in G_HEX and client(g):
        want = height() + 1
        marks = len(prime_log())
        glog0 = len(gw_log(g))
        log(f"== (e) {g} is still served after activation: {m} mines on it (NTA active) until a share/block or {STOCK_SECS}s")
        c0 = (client(g) or {}).get("accepted", 0)
        ok, secs = run_miner(m, lambda: height() >= want or (client(g) or {}).get("accepted", 0) > c0
                             or f"gateway={G_HEX[g]}" in prime_log()[marks:] and "refused" in prime_log()[marks:]
                             and any("refused" in l and G_HEX[g] in l for l in prime_log()[marks:].splitlines()), STOCK_SECS)
        time.sleep(15)   # let it be refused and reconnect
        time.sleep(5)
        plog, glog = prime_log()[marks:], gw_log(g)[glog0:]
        grab = lambda text, pats: [l for l in text.splitlines() if any(p in l for p in pats)][-8:]
        blk = None
        if height() >= want:
            cb = coinbase(want)
            blk = {"height": want, "paid": sorted(paid(cb)), "atts": nta_shape(cb)[1]}
        obs(f"(e) {g} mining after activation ({secs:.0f}s)", {
            "height_before": want - 1, "height_after": height(), "block": blk,
            "accepted_delta": (client(g) or {}).get("accepted", 0) - c0,
            "status": (row(readiness(), g) or {}).get("status"), "evidence": (row(readiness(), g) or {}).get("evidence"),
            "refusals": (row(readiness(), g) or {}).get("refusals"), "has_session": client(g) is not None,
            "logged_server_message": "DATUM Server message: Prime refuses" in glog,
            "logged_0x4e_notice": "Received unknown mining command 4E" in glog,
            "primed_lines": grab(plog, [G_HEX[g], "bad-nta", "refused", "invalid"]),
            "gateway_lines": grab(glog, ["bad-nta", "block", "Block", "submit", "Script length", "refus", "Prime"]),
            "node_G_bad_nta": sum("bad-nta" in l for l in open(os.path.join(nodes["G"]["d"], "regtest", "debug.log"), errors="replace")),
            "node_P_bad_nta": sum("bad-nta" in l for l in open(os.path.join(nodes["P"]["d"], "regtest", "debug.log"), errors="replace"))})
    return finish()


def finish():
    try:
        final = get("/nta.json")
    except Exception:  # noqa: BLE001
        final = None
    json.dump(OBS, open(os.path.join(RESULTS, "observations.json"), "w"), indent=1)
    summary = {"demo": "sov-011 readiness", "observations": OBS, "result": "PASS" if not FAILS else "FAIL",
               "fails": FAILS, "nta_height": NTA_HEIGHT, "setup": SETUP, "probe_every_secs": PROBE_EVERY,
               "gateways": G_HEX, "final_height": height() if nodes else None, "blocks": blocks}
    for name, obj in (("summary.json", summary), ("checks.json", CHECKS), ("nta-final.json", final)):
        json.dump(obj, open(os.path.join(RESULTS, name), "w"), indent=1)
    print(f"results: {RESULTS}")
    print("RESULT " + ("PASS" if not FAILS else f"FAIL ({len(FAILS)})"))
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
