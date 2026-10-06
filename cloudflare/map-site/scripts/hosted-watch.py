#!/usr/bin/env python3
"""Which DATUM gateways are run by the pool itself.

A pool that points its stratum hashers at gateways it runs puts a gateway tag on the coinbase,
and on chain that block looks like a miner's own node built it. It did not. This watcher proves
which gateway names are the pool's:

  1. Hold a stratum v1 session on every public stratum port (a seed list, plus the ports
     reorg.watch's hourly probe finds handing out work). Never submit a share.
  2. A Sia-style job carries h2: a hash over every header field above the coinbase (version,
     prev, height, merkle root, time, bits, tx count, flags, xor key). A miner cannot change it.
  3. For each new block, compute h2 from its header. If a public port handed out that h2, the
     gateway behind that port built the block, and the block's gateway tag is a pool-run name.

Writes hosted.json and, when it changes, puts it in the map Worker's KV as `hostedgateways`,
where the naughty list reads it. usage: hosted-watch.py [--once-secs N] [--no-kv]
"""
import collections, hashlib, json, os, socket, struct, subprocess, sys, threading, time, urllib.request

EXPLORER = "https://mempool.lazarus-xbt.xyz/api"
DIRECTORY = "https://reorg.watch/pools.json"
KV_NAMESPACE = "8242de880afd41f79e03b586eb16e227"
SEED = [f"{h}.alphapool.tech:{p}" for h in ("us1", "us2", "eu1", "sg1") for p in (7333, 5555)] + ["us1.alphapool.tech:7777"]
# ctrlpool's XBT port: a stock datum_gateway that mines through another pool's window (its own
# site says so, not which pool). On 6 Oct 2026 its US server was a gateway on Lazarus.
SEED += [f"{h}.ctrlpool.com:4333" for h in ("stratum", "us.stratum", "asia.stratum")]
# Names a stock gateway ships with: many unrelated operators carry them, so a match proves the
# block and nothing about the name.
GENERIC = {"", "datum user", "datum gateway", "gateway", "datum"}
JOB_MEMORY = 3 * 3600
STATE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "hosted.json") if "--state" not in sys.argv else sys.argv[sys.argv.index("--state") + 1]
UA = {"User-Agent": "lazarus-hosted-watch/0.1 (+https://lazarus-xbt.xyz/naughtylist/)"}
USER = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.watch"

lock = threading.Lock()
jobs = {}        # h2 hex -> [first seen, set of endpoints]
listening = {}   # endpoint -> {"ip", "jobs", "last", "error"}


def log(*a):
    print(time.strftime("%FT%TZ", time.gmtime()), *a, flush=True)


def fold(s):
    return "".join(c for c in str(s or "").lower() if c.isascii() and c.isalnum())


def get(url, raw=False):
    d = urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=25).read()
    return d.decode() if raw else json.loads(d)


def tagged(tag, data):
    t = hashlib.sha256(tag.encode()).digest()
    return hashlib.sha256(t + t + data).digest()


def h2_of(header_hex):
    """h2 of a 164-byte v2 header, as `HeaderV2::h2` in prime/wire and lazarus/protocol."""
    b = bytes.fromhex(header_hex)
    if len(b) != 164:
        raise ValueError(f"header is {len(b)} bytes")
    h1d = (b[0:4] + b[4:36][::-1] + b[128:132] + b[36:68] + b[68:72] + b"\0" + b[72:76]
           + struct.pack("<I", struct.unpack("<H", b[108:110])[0]) + b[110:112]
           + tagged("Bitcoin block hash PoW XOR key", b[112:128]))
    return tagged("Merge-mining hook", tagged("Bitcoin block header 1", h1d) + bytes(32) + b[132:164]).hex()


def coinbase_tags(raw_hex):
    """(primary, secondary) of a DATUM coinbase: the push holding `<primary> 0x0F <secondary>`."""
    sig, j = bytes.fromhex(raw_hex or ""), 0
    while j < len(sig):
        op = sig[j]; j += 1
        if 1 <= op <= 75: n = op
        elif op == 0x4C and j < len(sig): n = sig[j]; j += 1
        else: break
        push = sig[j:j + n]; j += n
        if b"\x0f" in push:
            a, _, c = push.partition(b"\x0f")
            return a.decode("latin1"), c.rstrip(b"\0").decode("latin1")
    return "", ""


def listen(endpoint, stop):
    host, port = endpoint.rsplit(":", 1)
    st = listening.setdefault(endpoint, {"jobs": 0})
    while not stop.is_set():
        try:
            s = socket.create_connection((host, int(port)), timeout=10)
            st["ip"], st["error"] = s.getpeername()[0], None
            s.settimeout(5)
            for i, (m, p) in enumerate((("mining.subscribe", ["lazarus-hosted-watch/0.1"]), ("mining.authorize", [USER, "x"])), 1):
                s.sendall((json.dumps({"id": i, "method": m, "params": p}) + "\n").encode())
            buf, heard = b"", time.time()
            while not stop.is_set() and time.time() - heard < 300:
                try: d = s.recv(65536)
                except socket.timeout: continue
                if not d: break
                buf += d; heard = time.time()
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    try: m = json.loads(line)
                    except ValueError: continue
                    q = m.get("params") if m.get("method") == "mining.notify" else None
                    # 3 zero bytes, h2, 4 zero bytes
                    if q and len(q) > 2 and isinstance(q[2], str) and len(q[2]) == 78:
                        with lock:
                            e = jobs.setdefault(q[2][6:70], [int(time.time()), set()])
                            e[1].add(endpoint)
                        st["jobs"] += 1; st["last"] = int(time.time())
            s.close()
        except Exception as e:  # a port that is down is news, not a crash
            st["error"] = str(e)[:100]
        stop.wait(30)


def directory_endpoints():
    out = {}
    try:
        for p in get(DIRECTORY).get("pools", []):
            for e in p.get("sv1_work") or []:
                if isinstance(e, str) and ":" in e: out[e] = p.get("pool", "")
    except Exception as e:
        log("directory:", e)
    return out


def load():
    try: return json.load(open(STATE))
    except (OSError, ValueError): return {"v": 1, "asOf": 0, "scanned": 0, "tags": {}, "blocks": {}, "endpoints": {}}


def note_block(state, b, now):
    """Returns True if the block was built behind a port we listen on."""
    h2 = h2_of(get(f"{EXPLORER}/block/{b['id']}/header", raw=True).strip())
    with lock:
        e = jobs.get(h2)
        ports = sorted(e[1]) if e else []
    if not ports: return False
    extras = b.get("extras") or {}
    primary, secondary = coinbase_tags(extras.get("coinbaseRaw"))
    key = f"{fold(primary)}|{secondary}"
    state["blocks"][str(b["height"])] = key
    if fold(secondary) in GENERIC or fold(secondary) == fold(primary):
        log(f"block {b['height']} {primary} / {secondary!r}: built behind {ports}; the name is generic, only the block is listed")
        return True
    t = state["tags"].setdefault(key, {"primary": primary, "secondary": secondary, "pool": (extras.get("pool") or {}).get("name", ""), "n": 0, "first": b["height"], "ports": []})
    t["n"] += 1; t["last"] = b["height"]; t["lastT"] = b.get("timestamp", now)
    t["ports"] = sorted(set(t["ports"]) | set(ports))
    log(f"block {b['height']} {primary} / {secondary}: built behind {ports} (proof {t['n']} for this name)")
    return True


def publish(state, kv):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f: json.dump(state, f, separators=(",", ":"))
    os.replace(tmp, STATE)
    if not kv: return True
    # wrangler finds the account from a config beside it, so run it where one names the namespace
    cfg = os.path.join(os.path.dirname(STATE), "wrangler.jsonc")
    if not os.path.exists(cfg):
        with open(cfg, "w") as f: json.dump({"name": "lazarus-hosted-watch", "compatibility_date": "2026-01-01", "kv_namespaces": [{"binding": "GALAXY", "id": KV_NAMESPACE}]}, f)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CF_", "CLOUDFLARE_"))}
    r = subprocess.run([os.path.expanduser("~/.local/bin/wrangler"), "kv", "key", "put", "hostedgateways", "--binding", "GALAXY", "--path", STATE, "--remote"],
                       capture_output=True, text=True, timeout=120, env=env, cwd=os.path.dirname(STATE))
    log("kv put:", "ok" if r.returncode == 0 else f"FAILED {r.returncode} {(r.stderr or r.stdout)[-300:]}")
    return r.returncode == 0


def main():
    kv = "--no-kv" not in sys.argv
    until = time.time() + float(sys.argv[sys.argv.index("--once-secs") + 1]) if "--once-secs" in sys.argv else None
    state, stop, threads, next_dir = load(), threading.Event(), {}, 0
    unsent = False  # a put that failed is tried again on the next pass
    started = time.time()
    while until is None or time.time() < until:
        now = int(time.time())
        if now >= next_dir:
            eps = {e: ("CTRL" if "ctrlpool" in e else "AlphaPool") for e in SEED}; eps.update(directory_endpoints()); next_dir = now + 3600
            for e in eps:
                if e not in threads:
                    threads[e] = threading.Thread(target=listen, args=(e, stop), daemon=True); threads[e].start()
            state["endpointPools"] = eps
            log(f"listening on {len(threads)} ports")
        changed = False
        try:
            page = get(f"{EXPLORER}/v1/blocks")
            for b in sorted(page, key=lambda b: b["height"]):
                # a block found before we were listening cannot be judged either way
                if str(b["height"]) in state["blocks"] or b["height"] <= state.get("scanned", 0) - 6 or b.get("timestamp", 0) < started - 600: continue
                if note_block(state, b, now): changed = True
            state["scanned"] = max(state.get("scanned", 0), page[0]["height"])
        except Exception as e:
            log("blocks:", e)
        with lock:
            for k in [k for k, v in jobs.items() if now - v[0] > JOB_MEMORY]: del jobs[k]
        state["asOf"] = now
        state["endpoints"] = {e: {k: v for k, v in s.items() if v is not None} for e, s in listening.items()}
        if len(state["blocks"]) > 4000:
            for h in sorted(state["blocks"], key=int)[:-4000]: del state["blocks"][h]
        if changed or unsent:
            try: unsent = not publish(state, kv)
            except Exception as e: unsent = True; log("publish:", e)
        stop.wait(45)
    publish(state, False)


if __name__ == "__main__":
    main()
