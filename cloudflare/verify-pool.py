#!/usr/bin/env python3
"""Compare a Pages deployment of the pool site with the origin, route by route.
Usage: verify-pool.py <origin> <candidate>   e.g. http://<node>:8888 https://lazarus-pool.pages.dev"""
import hashlib, json, re, sys, urllib.request, urllib.error
sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent / "pool-site"))
from build import server_constants

origin, cand = (a.rstrip("/") for a in sys.argv[1:3])

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k): return None
opener = urllib.request.build_opener(NoRedirect)

def get(base, path, method="GET"):
    req = urllib.request.Request(base + path, method=method, headers={"User-Agent": "Mozilla/5.0 lazarus-verify", "Accept-Encoding": "identity"})
    try:
        with opener.open(req, timeout=30) as r:
            return r.status, r.read(), r.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers

pages, verify, indexnow, stratum_doc = server_constants()
exact = [p for p in pages] + ["/zh/" if p == "/" else "/zh" + p for p in pages]
exact += ["/robots.txt", "/llms.txt", "/.well-known/llms.txt", f"/{indexnow}.txt", stratum_doc, "/sitemap.xml", "/bingsiteauth.xml"] + verify
exact += ["/miner", "/miner/bc1qexampleexampleexampleexampleexample00", "/zh/miner/bc1qexampleexampleexampleexampleexample00"]
st, body, _ = get(origin, "/")
exact += sorted(set(re.findall(r'(?:src|href)="(/static/[^"?]+)', body.decode())))
st, body, _ = get(origin, "/miner")
exact += sorted(set(re.findall(r'(?:src|href)="(/static/[^"?]+)', body.decode())) - set(exact))

bad = 0
for p in exact:
    so, bo, ho = get(origin, p)
    sc, bc, hc = get(cand, p)
    same = so == sc and hashlib.sha256(bo).digest() == hashlib.sha256(bc).digest()
    to, tc = (ho.get("Content-Type") or "").split(";")[0], (hc.get("Content-Type") or "").split(";")[0]
    note = "" if to == tc else f"  ctype {to} -> {tc}"
    if not same or note:
        bad += not same
        print(f"{'DIFF' if not same else 'note'} {p}: {so}/{len(bo)} vs {sc}/{len(bc)}{note}")
print(f"static+pages: {len(exact)} checked, {bad} differ")

apis = ["/api/pool", "/api/price", "/api/v1/prices", "/api/miners", "/api/blocks", "/api/coinbaser", "/api/solo", "/api/gateways", "/api/payouts", "/api/makegoods", "/api/hardware", "/api/miner/notanaddress", "/api/nope"]
for p in apis:
    so, bo, _ = get(origin, p)
    sc, bc, hc = get(cand, p)
    try:
        ko, kc = sorted(json.loads(bo)), sorted(json.loads(bc))
    except Exception as e:
        ko, kc = "?", f"unparseable: {e}"
    ok = so == sc and ko == kc
    bad += not ok
    print(f"{'ok  ' if ok else 'DIFF'} {p}: {so} vs {sc} cache={hc.get('X-Lazarus-Cache')} acao={hc.get('Access-Control-Allow-Origin')}")
for p in ["/", "/api/pool", "/robots.txt"]:
    sc, bc, _ = get(cand, p, "HEAD")
    print(f"HEAD {p}: {sc} body={len(bc)}")
sys.exit(1 if bad else 0)
