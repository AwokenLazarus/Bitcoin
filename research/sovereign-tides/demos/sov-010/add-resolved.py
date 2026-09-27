#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-012: patch a copy of the XBT-071 or SOV-004 harness in place so that it measures fix 1
(a canary mined before its first due check is not due) on its own run.

Neither harness runs the canary sidecar, which is what reports settled canaries to primed in
production (`POST /sovereignty/canary/resolved`). The patch adds the sidecar's lookup: for each
canary, whichever of C / C' (or D, Q) is confirmed on the pool node P (`gettxout`), and the block
that confirmed it (height, header time). At the point where the harness reads its verdicts it then

  1. reads /sovereignty.json as it stands (BEFORE: no canary known to be settled, the old rule),
  2. posts every settlement,
  3. reads /sovereignty.json again (AFTER: fix 1), which the harness then checks as before,

and prints one `SOV012 <gateway> own <hits>/<due> -> <hits>/<due>` line per gateway, plus how many
canaries a block settled within CANARY_DUE (12 s) of delivery. Both reads are of the same run, so
the only difference is fix 1. Primed recomputes in demo mode when a POST changed anything.

    add-resolved.py FILE.py      # FILE.py is sov_nta_demo.py (XBT-071) or hybrid_knots.py (SOV-004)
"""
import sys

HELPER = '''

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

'''


def patch(src: str, old: str, new: str) -> str:
    if src.count(old) != 1:
        sys.exit(f"add-resolved: expected one {old!r}")
    return src.replace(old, new)


path = sys.argv[1]
src = open(path).read()
if "def sov012_resolve" in src:
    sys.exit(0)
if "\ndef post(" not in src:
    sys.exit(f"add-resolved: no `def post(` in {path}")
src = src.replace("\ndef post(", HELPER + "\ndef post(", 1)

if 'canaries = {"sovereign": [], "fake": []}' in src:  # XBT-071 sov_nta_demo.py
    src = patch(src, "twin, _ = tx([(txid, n)], [(v - CANARY_FEE - 1, P2WSH_TRUE)])",
                "twin, sov012_tid = tx([(txid, n)], [(v - CANARY_FEE - 1, P2WSH_TRUE)])")
    src = patch(src, 'canaries[name].append({"t": t0, "txid": cid})',
                'canaries[name].append({"t": t0, "txid": cid, "twin": sov012_tid})')
    src = patch(src, "    sov1 = sovereignty()\n",
                "    sov012_before = sovereignty()[\"gateways\"]\n"
                "    sov012_n = sov012_resolve(canaries[\"sovereign\"] + canaries[\"fake\"])\n"
                "    print(f\"SOV012 xbt071 settled {sov012_n[0]} canaries, {sov012_n[1]} within 12 s of delivery\", flush=True)\n"
                "    sov1 = sovereignty()\n"
                "    sov012_line(\"xbt071\", sov012_before, sov1[\"gateways\"], [(\"sovereign\", SOV.g_hex), (\"fake\", FAKE.g_hex)])\n")
    what = "XBT-071"
elif "twin_of = {}" in src:  # SOV-004 hybrid_knots.py
    src = patch(src, '    for c in canaries:\n        post("/sovereignty/canary", c)\n',
                '    for c in canaries:\n        post("/sovereignty/canary", c)\n'
                '    with urllib.request.urlopen(STATS + "/sovereignty.json", timeout=600) as r:\n'
                '        sov012_before = json.load(r).get("gateways") or {}\n'
                '    sov012_n = sov012_resolve([dict(c, twin=twin_of.get(c["txid"])) for c in canaries])\n'
                '    print(f"SOV012 hybrid settled {sov012_n[0]} canaries, {sov012_n[1]} within 12 s of delivery", flush=True)\n')
    src = patch(src, '    gws = sov.get("gateways") or {}\n',
                '    gws = sov.get("gateways") or {}\n'
                '    sov012_line("hybrid", sov012_before, gws, [(g, g) for g in GATEWAYS])\n')
    what = "SOV-004"
else:
    sys.exit(f"add-resolved: {path} is neither harness")
open(path, "w").write(src)
print(f"add-resolved: {path} ({what}) posts canary settlements and prints before/after hit rates")
