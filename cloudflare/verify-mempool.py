#!/usr/bin/env python3
"""Compare the mempool Pages build with the node's nginx, across languages and route types.

The build rebrands a few files after capturing them (mempool-site/postprocess.py). The node's
bytes get the same rules here before the comparison, so everything else still has to match
byte for byte.
Usage: verify-mempool.py <origin> <candidate>"""
import hashlib, json, os, re, sys, time, urllib.request, urllib.error

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "mempool-site"))
import postprocess

origin, cand = (a.rstrip("/") for a in sys.argv[1:3])

def get(base, path, headers=None):
    h = {"User-Agent": "Mozilla/5.0 lazarus-verify", "Accept-Encoding": "identity"}
    h.update(headers or {})
    try:
        with urllib.request.urlopen(urllib.request.Request(base + path, headers=h), timeout=60) as r:
            return r.status, r.read(), r.headers
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers

# What the build derived from its own files, read back from the candidate.
version = postprocess.theme_version(get(cand, "/lazarus/theme.css")[1], get(cand, "/lazarus/theme.js")[1])
crest = get(cand, "/lazarus/" + postprocess.CREST)[0] == 200

def expected(path, body, headers):
    """The node's bytes as the build would have left them."""
    try:
        if "text/html" in (headers.get("Content-Type") or ""):
            return postprocess.shell(body.decode(), version, crest).encode()
        if path == "/resources/config.js":
            return postprocess.config(body.decode()).encode()
        if path.endswith("/favicons/site.webmanifest"):
            return postprocess.manifest(body.decode()).encode()
    except UnicodeDecodeError:
        pass
    return body

bad = n = 0
def same(path, headers=None, label=""):
    global bad, n
    so, bo, ho = get(origin, path, headers)
    bo = expected(path, bo, ho)
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
          "/resources/mining-pools/default.svg", "/resources/favicons/favicon.ico", "/lazarus/chi-rho.svg",
          "/resources/pools-v2.json", "/robots.txt", "/resources/nope.png", "/resources/favicons/site.webmanifest"):
    same(p)
# The footer links to the licence files (mempool is AGPL-3.0). The build ships them whether or
# not the node's nginx serves them, so they are checked on the candidate alone.
import os
_theme = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "node", "umbrel", "mempool-theme", "www")
for name in ("theme.css", "theme.js"):
    sc, bc, _ = get(cand, "/lazarus/" + name)
    n += 1
    if sc == 200 and bc == open(os.path.join(_theme, name), "rb").read():
        print(f"ok   /lazarus/{name}: matches the checkout")
    else:
        bad += 1
        print(f"DIFF /lazarus/{name}: candidate {sc}/{len(bc)} differs from the checkout")
for p in ("/lazarus/NOTICE", "/lazarus/LICENSE"):
    st, body, hh = get(cand, p)
    n += 1
    if not (st == 200 and body.strip() and "text/plain" in (hh.get("Content-Type") or "")):
        bad += 1
        print(f"DIFF {p}: {st}/{len(body)} {hh.get('Content-Type')}")
# The pie's gateway bands read pool-tags.json, which the worker fetches live from the node
# (pools-sync rewrites it every 3 min). A build-time copy would freeze the bands at deploy.
import json, time
st, body, hh = get(cand, "/lazarus/pool-tags.json")
n += 1
try:
    age = time.time() - json.loads(body)["generated"]
except Exception:
    age = None
if st == 200 and age is not None and age < 900:
    print(f"ok   /lazarus/pool-tags.json: generated {int(age)} s ago")
else:
    bad += 1
    print(f"DIFF /lazarus/pool-tags.json: {st}, generated {'?' if age is None else f'{int(age)} s'} ago (stale or unreadable: is pools-sync running?)")
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

# What the worker adds on top of the node: checked on the candidate alone.
def check(what, ok, detail=""):
    global bad
    bad += not ok
    print(f"{'ok  ' if ok else 'DIFF'} {what}{': ' + detail if detail and not ok else ''}")

_, _, hh = get(cand, "/api/v1/blocks/tip/height")
cookies = hh.get_all("Set-Cookie") or []
check("no Set-Cookie on /api/v1/blocks/tip/height", not cookies, "sets " + ", ".join(c.split("=")[0] for c in cookies))  # names only
check("no X-Powered-By on /api/v1/blocks/tip/height", not hh.get("X-Powered-By"))
st, shell, hh = get(cand, "/", {"Accept-Language": "en-US"})
check("Strict-Transport-Security on /", bool(hh.get("Strict-Transport-Security")))
main = re.search(r'src="(main\.[0-9a-f]+\.js)"', shell.decode())
_, _, hh = get(cand, "/" + main.group(1), {"Accept-Language": "en-US", "Accept-Encoding": "br, gzip"}) if main else (0, b"", {})
check("Content-Encoding on the main bundle", bool(hh.get("Content-Encoding")), "sent uncompressed" if main else "no main bundle in the shell")
t = time.monotonic()
st, body, _ = get(cand, "/api/v1/services/accelerator/accelerations")
took = time.monotonic() - t
check("/api/v1/services/accelerator/accelerations is [] from the worker", st == 200 and body == b"[]" and took < 2, f"{st} {body[:40]!r} in {took:.2f}s")
_, _, hh = get(cand, "/api/v1/blocks")
check("X-Edge-Cache on /api/v1/blocks", hh.get("X-Edge-Cache") in ("HIT", "MISS"), f"got {hh.get('X-Edge-Cache')}")
sys.exit(1 if bad else 0)
