#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Template sovereignty on XBT: who built each block's template, per pool and network-wide,
plus a verifier for Sovereign TIDES attestations ("LZT1", see rnd/a2
prime/wire/src/authorship.rs). P-003 A2, XBT-051. Read-only: public explorer GETs only.

  sovereignty.py fetch [--days 7]      cache blocks + coinbase outputs (cache/)
  sovereignty.py score [--days 7|1]    tables (markdown) + score.json
  sovereignty.py verify-fixture F      check a Rust-made LZT1 attestation, and that tampering breaks it

Builder of a block (rule f47db50, as XBT-047's recount):
  gateway  a DATUM coinbase whose secondary tag names someone other than the pool
  pool     no secondary tag, or one naming the pool itself (own stratum / own gateway)
  unattributed   the explorer could not name a pool
A block is *verified* when its coinbase carries an LZT1 commitment, the pool has published the
matching attestation + signature (attestations.json), it verifies, and its key G is not a house
key (gateways.json). Nothing on chain carries one yet, so today's verified share is 0.
"""
import argparse
import hashlib
import json
import os
import re
import sys
import time
import urllib.request

API = os.environ.get("XBT_API", "https://mempool.lazarus-xbt.xyz/api")
HERE = os.path.dirname(os.path.abspath(__file__))
CACHE = os.path.join(HERE, "cache")
MAGIC = b"LZT1"
DOMAIN = b"XBT-SOVEREIGN-TIDES/1\x00"
CLAIMED_WEIGHT = 0.5  # an unauthenticated gateway tag counts half; a verified claim counts 1


# ---------------------------------------------------------------- fetching (read-only)

def get(path, tries=4):
    for i in range(tries):
        try:
            req = urllib.request.Request(API + path, headers={"User-Agent": "xbt-051-sovereignty/0.1 (read-only research)"})
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)
        except Exception as e:  # noqa: BLE001 - retry anything, then give up loudly
            if i == tries - 1:
                raise RuntimeError(f"GET {path}: {e}") from e
            time.sleep(2 * (i + 1))


def load(name, default):
    p = os.path.join(CACHE, name)
    if os.path.exists(p):
        with open(p) as f:
            return json.load(f)
    return default


def save(name, obj):
    os.makedirs(CACHE, exist_ok=True)
    tmp = os.path.join(CACHE, name + ".tmp")
    with open(tmp, "w") as f:
        json.dump(obj, f)
    os.replace(tmp, os.path.join(CACHE, name))


def fetch(days, pause):
    blocks = {int(k): v for k, v in load("blocks.json", {}).items()}
    tip = get("/blocks/tip/height")
    cutoff = time.time() - days * 86400 - 3600
    h = tip
    while h > 0:
        page = get(f"/v1/blocks/{h}")
        for b in page:
            ex = b.get("extras", {})
            pool = ex.get("pool") or {}
            blocks[b["height"]] = {
                "height": b["height"], "hash": b["id"], "time": b["timestamp"], "tx_count": b["tx_count"],
                "prev": b["previousblockhash"], "script_sig": ex.get("coinbaseRaw", ""),
                "pool": pool.get("slug", "unknown"), "pool_name": pool.get("name", "Unknown"),
                "miner_names": pool.get("minerNames") or [], "match_rate": ex.get("matchRate"),
                "coinbase_address": ex.get("coinbaseAddress"),
            }
        oldest = min(b["timestamp"] for b in page)
        h = min(b["height"] for b in page) - 1
        if oldest < cutoff:
            break
        time.sleep(pause)
    save("blocks.json", blocks)
    print(f"blocks: tip {tip}, cached {len(blocks)}", file=sys.stderr)

    outs = load("outputs.json", {})
    todo = [b for b in blocks.values() if b["time"] >= cutoff and b["hash"] not in outs]
    for i, b in enumerate(sorted(todo, key=lambda b: -b["height"])):
        cb = get(f"/block/{b['hash']}/txs/0")[0]
        outs[b["hash"]] = [[o["value"], o["scriptpubkey"]] for o in cb["vout"]]
        if i % 100 == 99:
            save("outputs.json", outs)
            print(f"coinbase outputs: {i + 1}/{len(todo)}", file=sys.stderr)
        time.sleep(pause)
    save("outputs.json", outs)

    pools = load("pools.json", {})
    for slug in sorted({b["pool"] for b in blocks.values()} - set(pools)):
        if slug == "unknown":
            continue
        try:
            pools[slug] = get(f"/v1/mining/pool/{slug}")["pool"]
        except RuntimeError as e:
            print(e, file=sys.stderr)
            pools[slug] = {"name": slug, "slug": slug, "regexes": []}
        time.sleep(pause)
    save("pools.json", pools)


# ---------------------------------------------------------------- coinbase parsing

def pushes(script):
    """Data pushes of a script, stopping at the first thing that is not a push."""
    out, i = [], 0
    while i < len(script):
        op = script[i]
        i += 1
        if 1 <= op <= 75:
            n = op
        elif op == 0x4C and i < len(script):
            n, i = script[i], i + 1
        elif op == 0x4D and i + 1 < len(script):
            n, i = int.from_bytes(script[i:i + 2], "little"), i + 2
        elif op == 0 or 0x51 <= op <= 0x60:
            out.append(b"")
            continue
        else:
            break
        out.append(script[i:i + n])
        i += n
    return out


def tags(script_sig_hex):
    """(primary, secondary) from a coinbase scriptSig; secondary is '' unless the first push
    after the height is DATUM's `<primary> 0x0F <secondary> 0x00`."""
    p = pushes(bytes.fromhex(script_sig_hex or ""))
    if len(p) < 2:
        return "", ""
    t = p[1]
    if b"\x0f" in t:
        a, _, b = t.partition(b"\x0f")
        b = b.split(b"\x00", 1)[0]
        dec = lambda x: x.decode("utf-8", "replace").strip()  # noqa: E731
        if a and b and all(c >= 0x20 for c in b if c < 0x80):
            return dec(a), dec(b)
        return dec(a), ""
    return t.split(b"\x00", 1)[0].decode("utf-8", "replace").strip(), ""


def norm(s):
    return re.sub(r"[^a-z0-9]", "", s.lower())


def own_names(pool, primary):
    """Names that mean the pool itself: name, slug, primary tag, and its tag matchers."""
    names = {norm(pool.get("name", "")), norm(pool.get("slug", "")), norm(primary)}
    for rx in pool.get("regexes", []):
        names.add(norm(re.sub(r"\(\?[!=<][^)]*\)", "", rx)))
    return {n for n in names if n}


GENERIC = {"datum", "knots", "datumuser"}  # names the catch-all DATUM pool matches on; not a pool's own name


def names_pool(secondary, own):
    """A secondary tag names the pool itself if it equals one of its names, or contains one of
    5+ characters ("buy hashrate @ pow.re", "xorpool.com")."""
    t = norm(secondary)
    return any(t == n or (len(n) >= 5 and n not in GENERIC and n in t) for n in own)


def builder(block, pools):
    """(class, builder id) for one block."""
    if block["pool"] == "unknown":
        return "unattributed", "unknown:" + (block.get("coinbase_address") or "?")
    primary, secondary = tags(block["script_sig"])
    pool = pools.get(block["pool"], {"name": block["pool_name"], "slug": block["pool"], "regexes": []})
    if secondary and not names_pool(secondary, own_names(pool, primary)):
        return "gateway", f"{block['pool']}/{secondary}"
    return "pool", f"{block['pool']}/(pool)"


# ---------------------------------------------------------------- LZT1 attestations

def dsha(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def b2(b):
    return hashlib.blake2b(b, digest_size=32).digest()


def varint(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + n.to_bytes(2, "little")
    if n <= 0xFFFFFFFF:
        return b"\xfe" + n.to_bytes(4, "little")
    return b"\xff" + n.to_bytes(8, "little")


def branches_for_coinbase(other_txids):
    """Stratum merkle branches for the coinbase from the other txids (internal order)."""
    branches, level = [], [None] + list(other_txids)
    while len(level) > 1:
        branches.append(level[1])
        if len(level) % 2:
            level.append(level[-1])
        level = [None if a is None else dsha(a + b) for a, b in zip(level[::2], level[1::2])]
    return branches


def payout_digest(outputs, skip):
    kept = [o for i, o in enumerate(outputs) if i != skip]
    buf = varint(len(kept))
    for value, script in kept:
        buf += value.to_bytes(8, "little") + varint(len(script)) + script
    return dsha(buf)


A_LAYOUT = [("v", 1), ("g", 32), ("height", 4), ("prev_hash", 32), ("branches_hash", 32), ("coinbase_value", 8),
            ("n_tx", 4), ("mempool_digest", 16), ("node_tag", 16), ("t_gbt", 8), ("payout_digest", 32)]
A_LEN = sum(n for _, n in A_LAYOUT)
INTS = {"v", "height", "coinbase_value", "n_tx", "t_gbt"}


def decode_attestation(raw):
    if len(raw) != A_LEN or raw[0] != 1:
        raise ValueError("malformed")
    a, i = {}, 0
    for k, n in A_LAYOUT:
        a[k] = int.from_bytes(raw[i:i + n], "little") if k in INTS else raw[i:i + n]
        i += n
    return a


def find_commitment(outputs):
    """-> (index, attestation id, G) | None; raises ValueError on a malformed or duplicated one."""
    hits = [i for i, (_, s) in enumerate(outputs) if s[:1] == b"\x6a" and s[2:6] == MAGIC]
    if not hits:
        return None
    if len(hits) > 1:
        raise ValueError("multiple")
    v, s = outputs[hits[0]]
    if v != 0 or len(s) != 70 or s[1] != 68:
        raise ValueError("malformed")
    return hits[0], s[6:38], s[38:70]


def bip34_height(script_sig):
    n = script_sig[0]
    if 0x51 <= n <= 0x60:
        return n - 0x50
    if 1 <= n <= 4:
        return int.from_bytes(script_sig[1:1 + n], "little")
    raise ValueError("malformed")


def verify_attestation(outputs, script_sig, prev_internal, branches, a_raw, sig, expect_g=None):
    """Mirror of datum_wire::authorship::verify. -> G. Raises ValueError(missing|multiple|malformed|
    commitment-mismatch|wrong-key|bad-signature|wrong-template|wrong-payout)."""
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

    c = find_commitment(outputs)
    if c is None:
        raise ValueError("missing")
    i, aid, g = c
    a = decode_attestation(a_raw)
    if aid != b2(a_raw) or g != a["g"]:
        raise ValueError("commitment-mismatch")
    if expect_g is not None and expect_g != a["g"]:
        raise ValueError("wrong-key")
    try:
        Ed25519PublicKey.from_public_bytes(a["g"]).verify(sig, DOMAIN + a_raw)
    except InvalidSignature:
        raise ValueError("bad-signature") from None
    if a["prev_hash"] != prev_internal or a["height"] != bip34_height(script_sig) or a["branches_hash"] != b2(b"".join(branches)):
        raise ValueError("wrong-template")
    if a["payout_digest"] != payout_digest(outputs, i):
        raise ValueError("wrong-payout")
    return a["g"]


def parse_tx(raw):
    """Legacy-serialized tx -> (script_sig of input 0, [(value, script)])."""
    i = 4

    def vi():
        nonlocal i
        b = raw[i]
        i += 1
        if b < 0xFD:
            return b
        w = {0xFD: 2, 0xFE: 4, 0xFF: 8}[b]
        v = int.from_bytes(raw[i:i + w], "little")
        i += w
        return v

    assert vi() == 1, "one input"
    i += 36
    n = vi()
    sig = raw[i:i + n]
    i += n + 4
    outs = []
    for _ in range(vi()):
        v = int.from_bytes(raw[i:i + 8], "little")
        i += 8
        n = vi()
        outs.append((v, raw[i:i + n]))
        i += n
    return sig, outs


def check_block(block, outputs_json, directory, published):
    """Chain scan of one block: None (no commitment) or a status dict. A block with a commitment
    is only *verified* once the pool has published its A + signature (attestations/<hash>.json)."""
    outputs = [(v, bytes.fromhex(s)) for v, s in outputs_json]
    try:
        c = find_commitment(outputs)
    except ValueError as e:
        return {"status": str(e)}
    if c is None:
        return None
    pub = published.get(block["hash"])
    if not pub:
        return {"status": "committed-unpublished", "g": c[2].hex()}
    txids = get(f"/block/{block['hash']}/txids")
    branches = branches_for_coinbase([bytes.fromhex(t)[::-1] for t in txids[1:]])
    try:
        g = verify_attestation(outputs, bytes.fromhex(block["script_sig"]), bytes.fromhex(block["prev"])[::-1], branches,
                               bytes.fromhex(pub["attestation"]), bytes.fromhex(pub["sig"]))
    except ValueError as e:
        return {"status": str(e)}
    entry = directory.get(g.hex(), {})
    return {"status": "valid", "g": g.hex(), "house": bool(entry.get("house")), "name": entry.get("name"),
            "independence": entry.get("independence", 1.0)}


# ---------------------------------------------------------------- scores

def hhi_effective(counts):
    n = sum(counts)
    return 0.0 if n == 0 else 1.0 / sum((c / n) ** 2 for c in counts)


def nakamoto(counts):
    """Fewest builders holding more than half of the blocks."""
    n, acc = sum(counts), 0
    for k, c in enumerate(sorted(counts, reverse=True), 1):
        acc += c
        if acc * 2 > n:
            return k
    return 0


def sovereignty_score(n, claimed, verified):
    """0-100. verified <= claimed <= n."""
    return 0.0 if n == 0 else 100.0 * (verified + CLAIMED_WEIGHT * (claimed - verified)) / n


def median(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    m = len(xs) // 2
    return xs[m] if len(xs) % 2 else (xs[m - 1] + xs[m]) / 2


def score(blocks, pools, outputs, directory, days, now=None, published=None):
    published = published or {}
    now = now or max(b["time"] for b in blocks.values())
    win = [b for b in blocks.values() if b["time"] > now - days * 86400]
    per_pool, builders, claims = {}, {}, {}
    for b in win:
        cls, who = builder(b, pools)
        claim = check_block(b, outputs[b["hash"]], directory, published) if b["hash"] in outputs else None
        verified = bool(claim and claim["status"] == "valid" and not claim["house"])
        if claim:
            claims[b["height"]] = claim
        p = per_pool.setdefault(b["pool"], {"pool": b["pool_name"], "n": 0, "gateway": 0, "pool_built": 0,
                                           "unattributed": 0, "verified": 0, "builders": {}, "mr_pool": [], "mr_gw": [],
                                           "scanned": 0})
        p["n"] += 1
        p["scanned"] += b["hash"] in outputs
        p[{"gateway": "gateway", "pool": "pool_built", "unattributed": "unattributed"}[cls]] += 1
        p["verified"] += verified
        p["builders"][who] = p["builders"].get(who, 0) + 1
        (p["mr_gw"] if cls == "gateway" else p["mr_pool"]).append(b.get("match_rate"))
        builders[who] = builders.get(who, 0) + 1
    rows = []
    for slug, p in per_pool.items():
        counts = list(p["builders"].values())
        rows.append({
            "slug": slug, "pool": p["pool"], "blocks": p["n"], "gateway_built": p["gateway"], "pool_built": p["pool_built"],
            "unattributed": p["unattributed"], "verified": p["verified"], "coinbases_scanned": p["scanned"],
            "claimed_pct": round(100 * p["gateway"] / p["n"], 2), "verified_pct": round(100 * p["verified"] / p["n"], 2),
            "score": round(sovereignty_score(p["n"], p["gateway"], p["verified"]), 1),
            "builders": len(counts), "effective_builders": round(hhi_effective(counts), 2),
            "top_builder_pct": round(100 * max(counts) / p["n"], 1),
            "match_rate_pool": median(p["mr_pool"]), "match_rate_gateway": median(p["mr_gw"]),
        })
    rows.sort(key=lambda r: -r["blocks"])
    n = len(win)
    gw = sum(r["gateway_built"] for r in rows)
    ver = sum(r["verified"] for r in rows)
    pool_counts = [r["blocks"] for r in rows]
    net = {
        "days": days, "from": min(b["height"] for b in win), "to": max(b["height"] for b in win), "blocks": n,
        "gateway_built": gw, "pool_built": sum(r["pool_built"] for r in rows),
        "unattributed": sum(r["unattributed"] for r in rows), "verified": ver,
        "coinbases_scanned": sum(r["coinbases_scanned"] for r in rows), "claims_seen": len(claims),
        "score": round(sovereignty_score(n, gw, ver), 1),
        "pools": len(rows), "pool_nakamoto": nakamoto(pool_counts), "pool_effective": round(hhi_effective(pool_counts), 2),
        "builders": len(builders), "template_nakamoto": nakamoto(list(builders.values())),
        "template_effective": round(hhi_effective(list(builders.values())), 2),
        "top_builders": sorted(builders.items(), key=lambda kv: -kv[1])[:12],
    }
    return net, rows, claims


def markdown(net, rows, top=16):
    out = [f"**Window:** {net['days']} d, blocks {net['from']}–{net['to']} ({net['blocks']} blocks); "
           f"coinbases scanned for claims: {net['coinbases_scanned']}; claims found: {net['claims_seen']}.", "",
           "| Pool | Blocks | Gateway-built | Pool-built | Verified | Claimed % | Verified % | **Score** | Builders | Eff. builders | Top builder % | matchRate pool / gw |",
           "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|"]
    fmt = lambda x: "—" if x is None else f"{x:.0f}"  # noqa: E731
    for r in rows[:top]:
        out.append(f"| {r['pool']} | {r['blocks']} | {r['gateway_built']} | {r['pool_built']} | {r['verified']} | "
                   f"{r['claimed_pct']:.1f} | {r['verified_pct']:.1f} | **{r['score']:.1f}** | {r['builders']} | "
                   f"{r['effective_builders']:.2f} | {r['top_builder_pct']:.1f} | {fmt(r['match_rate_pool'])} / {fmt(r['match_rate_gateway'])} |")
    rest = rows[top:]
    if rest:
        n = sum(r["blocks"] for r in rest)
        g = sum(r["gateway_built"] for r in rest)
        out.append(f"| *{len(rest)} more pools* | {n} | {g} | {sum(r['pool_built'] for r in rest)} | "
                   f"{sum(r['verified'] for r in rest)} | {100 * g / n:.1f} | 0.0 | {sovereignty_score(n, g, 0):.1f} | | | | |")
    out += ["", f"**Network:** score **{net['score']}**; gateway-built {net['gateway_built']}/{net['blocks']} "
            f"({100 * net['gateway_built'] / net['blocks']:.1f}%), pool-built {net['pool_built']}, unattributed {net['unattributed']}, "
            f"verified {net['verified']}. Pools: {net['pools']} (Nakamoto {net['pool_nakamoto']}, effective {net['pool_effective']}). "
            f"Template builders: {net['builders']} (Nakamoto **{net['template_nakamoto']}**, effective {net['template_effective']}).", "",
            "Top template builders: " + ", ".join(f"{k} {v}" for k, v in net["top_builders"]) + "."]
    return "\n".join(out)


# ---------------------------------------------------------------- fixture check

def verify_fixture(path):
    with open(path) as f:
        fx = json.load(f)
    sig_script, outs = parse_tx(bytes.fromhex(fx["coinbase"]))
    others = [bytes.fromhex(t) for t in fx["txids"]]
    prev = bytes.fromhex(fx["prev_hash"])
    br = branches_for_coinbase(others)
    a_raw, sig, g = bytes.fromhex(fx["attestation"]), bytes.fromhex(fx["sig"]), bytes.fromhex(fx["pk"])
    results = []

    def expect(name, fn, want):
        try:
            got = "valid" if fn() == g else "valid-other-key"
        except ValueError as e:
            got = str(e)
        results.append((name, got, want, got == want))

    def v(outs_=outs, script=sig_script, prev_=prev, br_=br, a_=a_raw, sig_=sig, expect_g=None):
        return verify_attestation(outs_, script, prev_, br_, a_, sig_, expect_g)

    def edit(raw, field, new):
        off = sum(n for k, n in A_LAYOUT[:[k for k, _ in A_LAYOUT].index(field)])
        return raw[:off] + new + raw[off + len(new):]

    expect("commitment is 70 bytes, under the RDTS cap of 83", lambda: g if len(outs[-1][1]) == 70 else b"", "valid")
    expect("attestation verifies", lambda: v(), "valid")
    expect("with the session key", lambda: v(expect_g=g), "valid")
    expect("another session key", lambda: v(expect_g=b"\x01" * 32), "wrong-key")
    expect("tx order swapped", lambda: v(br_=branches_for_coinbase([others[1], others[0]] + others[2:])), "wrong-template")
    expect("tx dropped", lambda: v(br_=branches_for_coinbase(others[:-1])), "wrong-template")
    expect("another parent", lambda: v(prev_=b"\x00" * 32), "wrong-template")
    s2 = bytearray(sig_script)
    s2[1] ^= 1
    expect("another height", lambda: v(script=bytes(s2)), "wrong-template")
    stolen = [(outs[0][0], bytes.fromhex("0014" + "99" * 20))] + outs[1:]
    expect("A lifted onto another payout", lambda: v(outs_=stolen), "wrong-payout")
    expect("A edited (node_tag)", lambda: v(a_=edit(a_raw, "node_tag", b"\x00" * 16)), "commitment-mismatch")
    bad = bytearray(sig)
    bad[0] ^= 1
    expect("signature flipped", lambda: v(sig_=bytes(bad)), "bad-signature")
    expect("no commitment", lambda: v(outs_=outs[:-1]), "missing")
    expect("two commitments", lambda: v(outs_=outs + [outs[-1]]), "multiple")
    expect("commitment carries value", lambda: v(outs_=outs[:-1] + [(1, outs[-1][1])]), "malformed")
    return results


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    f = sub.add_parser("fetch")
    f.add_argument("--days", type=float, default=7)
    f.add_argument("--pause", type=float, default=0.2)
    s = sub.add_parser("score")
    s.add_argument("--days", type=float, default=7)
    s.add_argument("--json", default=os.path.join(HERE, "score.json"))
    v = sub.add_parser("verify-fixture")
    v.add_argument("path")
    a = ap.parse_args()
    if a.cmd == "fetch":
        fetch(a.days, a.pause)
    elif a.cmd == "score":
        blocks = {int(k): v for k, v in load("blocks.json", {}).items()}
        directory = json.load(open(os.path.join(HERE, "gateways.json")))
        published = json.load(open(os.path.join(HERE, "attestations.json")))
        net, rows, claims = score(blocks, load("pools.json", {}), load("outputs.json", {}), directory, a.days,
                                  published=published)
        print(markdown(net, rows))
        json.dump({"network": net, "pools": rows, "claims": claims}, open(a.json, "w"), indent=1)
    else:
        res = verify_fixture(a.path)
        for name, got, want, ok in res:
            print(f"{'ok  ' if ok else 'FAIL'} {name}: {got} (want {want})")
        sys.exit(0 if all(r[3] for r in res) else 1)


if __name__ == "__main__":
    main()
