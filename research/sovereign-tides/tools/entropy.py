#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Template-entropy detector (research note C.2 #5), P-003 A2 / XBT-051.

Input: per-gateway template snapshots, as Prime would log them from DATUM jobs, plus the pool
node's own templates (Prime's node polled the same way):

    {"t": unix seconds, "prev": hex, "txids": [hex, ...]}      # txids in template order

Features per gateway (all against the pool node's own templates):
  identical     share of snapshots whose merkle branches equal a pool template from the last
                `window` seconds on the same parent (the pool's template, copied)
  branch_sim    mean b10c-style weighted branch similarity to the nearest pool template
                (branch i weighted 2^i: later branches cover more of the block)
  jaccard       mean tx-set overlap with the nearest pool template
  switch_lag    median seconds from the pool's first template on a new parent to the gateway's
  lockstep      share of parent switches the gateway made within `lockstep` seconds of the pool
  incl_lag      median seconds from a tx's first appearance in a pool template to the gateway's

  lead          share of the gateway's txs it put in a template before the pool's node did (by
                > lead_margin s), or that the pool never had
  subset        mean share of each (non-empty) snapshot's txs that the pool's recent templates
                already had (containment; 1.0 = nothing of its own)

Independence (what the bonus weights by):
  follower     = subset * (1 - min(1, lead / lead_ref))
  independence = 1 - max(identical, follower)
`identical` catches a straight copy. `follower` catches a copy that was reordered or delayed to
dodge it: a copy can never be ahead of its source, so its lead is ~0 and its tx set always sits
inside the pool's. An honest node hears about transactions from its own peers and is first on a
share of them (on a random relay graph, roughly half), so its lead clears lead_ref (0.25).
A copy that slips in a few txs of its own (HYBRID in the regtest) keeps containment near 1 and
lead near 0, so it still scores low.
Switch lag and lockstep are reported but not scored: on a LAN or loopback honest nodes switch in
lockstep too (XBT-051 regtest). Fewer than 20 comparable snapshots scores 0 (unknown earns
nothing).

Gateways are also clustered (identical-template share between each pair >= 0.8) so the public
score can count one builder per cluster.
"""
import hashlib
import statistics


def dsha(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def branches(txids):
    """Stratum merkle branches for the coinbase path, from the template's txids (hex)."""
    out, level = [], [None] + [bytes.fromhex(t)[::-1] for t in txids]
    while len(level) > 1:
        out.append(level[1])
        if len(level) % 2:
            level.append(level[-1])
        level = [None if a is None else dsha(a + b) for a, b in zip(level[::2], level[1::2])]
    return out


def branch_sim(a, b):
    """b10c-style weighted similarity of two branch lists (1.0 = same template)."""
    n = max(len(a), len(b))
    if n == 0:
        return 1.0
    w = [2 ** i for i in range(n)]
    same = sum(w[i] for i in range(n) if i < len(a) and i < len(b) and a[i] == b[i])
    return same / sum(w)


def jaccard(a, b):
    a, b = set(a), set(b)
    return 1.0 if not a and not b else len(a & b) / len(a | b)


def _prep(snaps):
    return [dict(s, br=branches(s["txids"])) for s in sorted(snaps, key=lambda s: s["t"])]


def _first_on_parent(snaps):
    first = {}
    for s in snaps:
        first.setdefault(s["prev"], s["t"])
    return first


def _first_seen(snaps):
    seen = {}
    for s in snaps:
        for t in s["txids"]:
            seen.setdefault(t, s["t"])
    return seen


def features(gw, pool, window=3.0, lockstep=0.25, lead_margin=0.1, lead_ref=0.25):
    gw, pool = _prep(gw), _prep(pool)
    by_prev = {}
    for p in pool:
        by_prev.setdefault(p["prev"], []).append(p)
    ident = sims = jacs = jhi = n = 0
    for s in gw:
        cands = [p for p in by_prev.get(s["prev"], []) if s["t"] - window <= p["t"] <= s["t"] + 0.05]
        if not cands:
            continue
        n += 1
        ident += any(p["br"] == s["br"] for p in cands)
        near = min(cands, key=lambda p: s["t"] - p["t"] if p["t"] <= s["t"] else 1e9)
        sims += branch_sim(s["br"], near["br"])
        j = jaccard(s["txids"], near["txids"])
        jacs += j
        jhi += j >= 0.98
    pf, gf = _first_on_parent(pool), _first_on_parent(gw)
    lags = [gf[p] - pf[p] for p in gf if p in pf]
    # skip the parent the run started on
    lags = lags[1:] if len(lags) > 1 else lags
    pseen, gseen = _first_seen(pool), _first_seen(gw)
    incl = [gseen[t] - pseen[t] for t in gseen if t in pseen]
    # A copy can never be ahead of its source: every tx it has, the pool's template had first.
    lead = sum(1 for t in gseen if t not in pseen or gseen[t] < pseen[t] - lead_margin) / len(gseen) if gseen else 0.0
    sub = subn = 0
    for s in gw:
        if not s["txids"]:
            continue
        recent = set()
        for p in by_prev.get(s["prev"], []):
            if s["t"] - window <= p["t"] <= s["t"] + 0.05:
                recent.update(p["txids"])
        if recent:
            subn += 1
            sub += len(set(s["txids"]) & recent) / len(s["txids"])
    f = {
        "snapshots": len(gw), "compared": n,
        "identical": ident / n if n else 0.0,
        "branch_sim": sims / n if n else 0.0,
        "jaccard": jacs / n if n else 0.0,
        "jaccard_hi": jhi / n if n else 0.0,
        "switches": len(lags),
        "switch_lag": statistics.median(lags) if lags else None,
        "lockstep": sum(abs(x) <= lockstep for x in lags) / len(lags) if lags else 0.0,
        "incl_lag": statistics.median(incl) if incl else None,
        "lead": lead,
        "subset": sub / subn if subn else 0.0,
    }
    f["follower"] = f["subset"] * (1.0 - min(1.0, f["lead"] / lead_ref))
    if n < 20:
        # Not enough overlap with the pool's templates to judge: unknown earns no bonus.
        f["independence"], f["insufficient"] = 0.0, True
    else:
        f["independence"] = round(max(0.0, 1.0 - max(f["identical"], f["follower"])), 3)
    return f


def pair_identical(a, b, window=3.0):
    """Share of a's snapshots whose branches equal one of b's (same parent, within window)."""
    a, b = _prep(a), _prep(b)
    idx = {}
    for s in b:
        idx.setdefault((s["prev"], tuple(s["br"])), []).append(s["t"])
    hits = sum(any(abs(t - s["t"]) <= window for t in idx.get((s["prev"], tuple(s["br"])), [])) for s in a)
    return hits / len(a) if a else 0.0


def clusters(gws, threshold=0.8):
    """Single-linkage clusters of gateways whose templates are identical >= threshold of the time."""
    names = sorted(gws)
    parent = {n: n for n in names}

    def root(x):
        while parent[x] != x:
            x = parent[x]
        return x

    for i, a in enumerate(names):
        for b in names[i + 1:]:
            if min(pair_identical(gws[a], gws[b]), pair_identical(gws[b], gws[a])) >= threshold:
                parent[root(a)] = root(b)
    out = {}
    for n in names:
        out.setdefault(root(n), []).append(n)
    return sorted(out.values(), key=len, reverse=True)


def report(gws, pool):
    rows = {g: features(s, pool) for g, s in gws.items()}
    cl = clusters(dict(gws, POOL=pool))
    return rows, cl
