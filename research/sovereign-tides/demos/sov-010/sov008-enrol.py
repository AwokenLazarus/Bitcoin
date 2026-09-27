#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Patch a copy of the SOV-008 demo (demos/sov-008 @ 5737c30) for rnd/sov-s0.

sov-008 registered every gwsim gateway through the old signed `LZT1 register G=…` route, and
listed the gateways' nodes to the sidecar in `[[gateways]]`. Since sov-009 that route is demo-only
(this demo runs with sovereignty-demo off), and on rnd/sov-s0 the sidecar reads primed's
sovereignty-nodes.json. So in the copy:

  * gwsim gains `gwsim sign <G secret hex> <message file>`: ed25519 by G over
    "XBT-SOVEREIGN-TIDES/register\\0" ‖ message (SOV-P-002 §4.1), as `lazarus-gateway enrol` signs;
  * every gateway enrols with a v1 message naming its node (`POST /sovereignty/enrol`), and the
    demo asserts that sovereignty-nodes.json then holds exactly those nodes;
  * the sidecar's config drops `[[gateways]]` and sets `nodes_file` to primed's
    sovereignty-nodes.json, so every canary it places goes to a node it learned from that file.

  * SOV-012, with `prime` as the second argument: the sidecar sets `nodes_source = "prime"` and no
    `nodes_file`, so every node it canaries came from primed's `GET /sovereignty/nodes`.

    sov008-enrol.py <copy dir> [prime]
"""
import os
import sys

d = sys.argv[1]


def patch(path, old, new):
    s = open(path).read()
    if s.count(old) != 1:
        sys.exit(f"sov008-enrol: {path}: expected one copy of {old[:60]!r}")
    open(path, "w").write(s.replace(old, new))


patch(os.path.join(d, "gwsim/src/main.rs"), '''    if a.len() != 6 {
''', '''    // SOV-010: sign a registration message (SOV-P-002 §4.1) with G, as `lazarus-gateway enrol` does
    if a.len() == 4 && a[1] == "sign" {
        let g = Identity::from_secret_bytes(&hex::decode(&a[2]).expect("G hex").try_into().expect("64-byte G"));
        let msg = std::fs::read(&a[3]).expect("message file");
        let sig = g.sign(&[b"XBT-SOVEREIGN-TIDES/register\\0".as_slice(), &msg].concat());
        println!("{}", hex::encode(sig));
        return;
    }
    if a.len() != 6 {
''')

demo = os.path.join(d, "sidecar_knots.py")
patch(demo, '''    gw_keys, gw_g = {}, {}
    for name in GATEWAYS:
        secret = os.urandom(64).hex()
        g, sig = subprocess.check_output([GWSIM, "register", secret], text=True).split()
        r = post_prime("/sovereignty/register", {"g": g, "payout": "", "sig": sig}, token)
        assert r.get("g") == g, r
        gw_keys[name], gw_g[name] = secret, g
''', '''    gw_keys, gw_g = {}, {}
    # SOV-010: v1 enrolment (SOV-P-002 §4.1) naming each gateway's node; primed files the nodes in
    # sovereignty-nodes.json, which is the sidecar's nodes_file below
    node_of = {n: f"127.0.0.1:{nodes[GATEWAYS[n]]['p2p']}" for n in GATEWAYS}
    at_h = rpc("P", "getblockcount")
    at = f"{at_h} {rpc('P', 'getblockhash', at_h)}"
    os.makedirs(SIDECAR, exist_ok=True)
    for name in GATEWAYS:
        secret = os.urandom(64).hex()
        g = subprocess.check_output([GWSIM, "register", secret], text=True).split()[0]
        msg = "\\n".join(["LZT1 register v1", "pool: lazarus-xbt", "chain: XBT", f"G: {g}",
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
''')
source = ('nodes_source = "prime"\n' if sys.argv[2:] == ["prime"] else 'nodes_file = "{nodes_file}"\n')
patch(demo, """    toml = f'''network = "regtest"
""", """    toml = f'''network = "regtest"
""" + source)
patch(demo, """''' + "".join(f'[[gateways]]\\ng = "{gw_g[n]}"\\nnode = "{node_of[n]}"\\n' for n in GATEWAYS)
""", """'''
""")
print(f"sov008-enrol: patched {d}; the sidecar reads {'GET /sovereignty/nodes' if sys.argv[2:] == ['prime'] else 'nodes_file'}")
