#!/usr/bin/env python3
"""Compare the mempool Pages build with the node's nginx, across languages and route types.
Usage: verify-mempool.py <origin> <candidate>"""
import hashlib, json, re, sys, urllib.request, urllib.error

origin, cand = (a.rstrip("/") for a in sys.argv[1:3])

def get(base, path, headers=None):
    h = {"User-Agent": "Mozilla/5.0 lazarus-verify", "Accept-Encoding": "identity"}
    h.update(headers or {})
    try:
        with urllib.request.urlopen(urllib.request.Request(base + path, headers=h), timeout=60) as r:
            return r.status, r.read(), r.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers

bad = n = 0
def same(path, headers=None, label=""):
    global bad, n
    so, bo, ho = get(origin, path, headers)
    sc, bc, hc = get(cand, path, headers)
    ok = so == sc and hashlib.sha256(bo).digest() == hashlib.sha256(bc).digest()
    n += 1
    if not ok:
        bad += 1
        print(f"DIFF {path} {label}: {so}/{len(bo)} vs {sc}/{len(bc)}")
    return bo

for label, hdr in (("en", {"Accept-Language": "en-US,en;q=0.9"}), ("zh", {"Accept-Language": "zh-CN,zh;q=0.9"}), ("de-cookie", {"Cookie": "lang=de", "Accept-Language": "fr"}), ("none", {}), ("xx", {"Accept-Language": "xx"})):
    shell = same("/", hdr, label).decode()
    for p in ("/mining", "/block/000000000000000000000000000000000000000000000000000000000000dead", "/address/bc1qtest", "/mining/pool/lazarus", "/api", "/docs/api/rest"):
        same(p, hdr, label)
    for ref in sorted(set(re.findall(r'(?:src|href)="([^":]+\.(?:js|css))', shell))):
        same("/" + ref.lstrip("/").split("?")[0], hdr, label)
    # a lazy chunk, which is where a mixed-language UI would come from
    same("/145.402fa264d97bba59.js", hdr, label)
for p in ("/de/", "/de/mining", "/zh/block/abc", "/pt-BR/", "/resources/config.js", "/resources/customize.js", "/resources/mining-pools/lazarus.svg",
          "/resources/mining-pools/default.svg", "/resources/favicons/favicon.ico", "/lazarus/theme.css", "/lazarus/theme.js", "/lazarus/chi-rho.svg",
          "/resources/pools-v2.json", "/robots.txt", "/resources/nope.png"):
    same(p)
# A browser revalidating its cache sends If-None-Match. The answer must be 304 (or the file again),
# never the HTML shell: a <script> that receives HTML leaves the explorer as a blank themed page.
for label, hdr in (("en", {"Accept-Language": "en-US"}), ("zh", {"Accept-Language": "zh-CN"})):
    shell = get(cand, "/", hdr)[1].decode()
    for ref in sorted(set(re.findall(r'(?:src|href)="([^":]+\.(?:js|css))', shell))) + ["145.402fa264d97bba59.js"]:
        path = "/" + ref.lstrip("/").split("?")[0]
        etag = get(cand, path, hdr)[2].get("ETag")
        st, body, hh = get(cand, path, dict(hdr, **{"If-None-Match": etag or '"none"'}))
        n += 1
        if not (st == 304 or (st == 200 and "text/html" not in (hh.get("Content-Type") or ""))):
            bad += 1
            print(f"DIFF {path} {label} revalidate: {st} {hh.get('Content-Type')}")
print(f"static: {n} checked, {bad} differ")

for p in ("/api/v1/blocks/tip/height", "/api/v1/fees/recommended", "/api/v1/prices", "/api/v1/mining/pools/1w", "/api/blocks/tip/hash", "/api/v1/backend-info", "/api/v1/nope"):
    so, bo, _ = get(origin, p)
    sc, bc, _ = get(cand, p)
    ok = so == sc and (bo == bc or (bo[:1] in b"{[" and type(json.loads(bo)) == type(json.loads(bc))))
    bad += not ok
    print(f"{'ok  ' if ok else 'DIFF'} {p}: {so} vs {sc}")
sys.exit(1 if bad else 0)
