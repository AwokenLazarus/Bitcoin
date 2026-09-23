#!/usr/bin/env python3
"""Export the pool site's static surface for Cloudflare Pages.

server.py renders every crawlable page from static/index.html plus per-path SEO copy, and none of
that depends on live pool data, so it can be captured once per deploy. Everything live stays
behind /api/*, which _worker.js proxies to the node.

    python3 build.py --origin http://<node>:8888      # snapshot what is live
    python3 build.py --origin http://127.0.0.1:8899       # or a local server.py from this tree

Pages serves `foo.html` at `/foo` and `dir/index.html` at `/dir/`, which is how the canonical
URLs already look (`/hardware`, `/zh/`, `/zh/hardware`).
"""

import argparse
import re
import shutil
import sys
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO_STATIC = HERE.parent.parent / "pool" / "static"


def fetch(origin, path):
    req = urllib.request.Request(origin + path, headers={"User-Agent": "lazarus-pages-build"})
    with urllib.request.urlopen(req, timeout=30) as r:
        if r.status != 200:
            raise SystemExit(f"{path}: HTTP {r.status}")
        return r.read(), r.headers.get("Content-Type", "")


def server_constants():
    """Pull the route tables out of server.py without importing it (importing opens the DB)."""
    src = (HERE.parent.parent / "pool" / "server.py").read_text(encoding="utf-8")
    block = re.search(r"^_SEO_PAGES = \{(.*?)^\}", src, re.S | re.M).group(1)
    pages = re.findall(r'^    "(/[^"]*)":', block, re.M)
    verify = re.findall(r'^    "(/[^"]+)": \(\n', re.search(r"^_SITE_VERIFY = \{(.*?)^\}", src, re.S | re.M).group(1), re.M)
    indexnow = re.search(r'^_INDEXNOW_KEY = "([0-9a-f]+)"', src, re.M).group(1)
    stratum_doc = re.search(r'^_STRATUM_DOC_PATH = "([^"]+)"', src, re.M).group(1)
    return pages, verify, indexnow, stratum_doc


def page_file(out, lang, path):
    base = out / "zh" if lang == "zh" else out
    return base / "index.html" if path == "/" else base / (path.lstrip("/") + ".html")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--origin", required=True, help="running server.py to snapshot, e.g. http://<node>:8888")
    ap.add_argument("--out", default=str(HERE / "dist"))
    ap.add_argument("--static-from", choices=("origin", "repo"), default="origin",
                    help="take /static/* from the origin (exactly what is live) or from pool/static in this tree")
    a = ap.parse_args()
    origin = a.origin.rstrip("/")
    out = Path(a.out)
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    pages, verify, indexnow, stratum_doc = server_constants()
    n = 0
    for lang, prefix in (("en", ""), ("zh", "/zh")):
        for p in pages:
            url = (prefix + p) if p != "/" else (prefix + "/")
            body, _ = fetch(origin, url)
            f = page_file(out, lang, p)
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_bytes(body)
            n += 1

    # The miner page reads its address from the URL client-side; _redirects maps /miner/* onto it.
    body, _ = fetch(origin, "/miner")
    (out / "miner.html").write_bytes(body)

    texts = ["/robots.txt", "/sitemap.xml", "/llms.txt", "/.well-known/llms.txt", f"/{indexnow}.txt", stratum_doc]
    # The Google proof is a .html path, which Pages would 308; _worker.js answers it instead.
    texts += [v for v in verify if not v.endswith(".html")] + ["/bingsiteauth.xml"]
    for p in dict.fromkeys(texts):
        body, _ = fetch(origin, p)
        f = out / p.lstrip("/")
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(body)
        n += 1

    static_out = out / "static"
    if a.static_from == "repo":
        shutil.copytree(REPO_STATIC, static_out)
    else:
        for src in sorted(REPO_STATIC.rglob("*")):
            if src.is_file():
                rel = src.relative_to(REPO_STATIC).as_posix()
                body, _ = fetch(origin, "/static/" + rel)
                f = static_out / rel
                f.parent.mkdir(parents=True, exist_ok=True)
                f.write_bytes(body)
    # server.py serves index.html only through the renderer, never as /static/index.html's twin.
    n += sum(1 for _ in static_out.rglob("*") if _.is_file())

    # 404.html also switches Pages off its single-page-app fallback, which answered every unknown
    # URL with the homepage and a 200. favicon.ico sits at the root because that is where browsers
    # and search engines look before reading any <link rel="icon">.
    for name in ("_headers", "_redirects", "_routes.json", "_worker.js", "404.html", "favicon.ico"):
        shutil.copy(HERE / name, out / name)
    print(f"wrote {n} files to {out}")


if __name__ == "__main__":
    sys.exit(main())
