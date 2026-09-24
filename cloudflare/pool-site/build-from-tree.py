#!/usr/bin/env python3
"""Build the pool site's Pages bundle from this git tree alone: no node, no tunnel, no secrets.

server.py renders every crawlable page from pool/static plus the per-path copy in its own source,
and none of that needs pool data (build.py already relies on this). So a build host can run
server.py against an empty database, snapshot it, and get the same bytes production serves.
Verified on 2026-09-23: a build from `main` matched the live site file for file, except the
sitemap's <lastmod> dates.

    python3 cloudflare/pool-site/build-from-tree.py            # -> cloudflare/pool-site/dist
    python3 cloudflare/pool-site/build-from-tree.py --out /tmp/x

This is the build command for the Cloudflare Pages Git integration (output directory
cloudflare/pool-site/dist). Nothing here talks to the network: /api/* keeps going through
_worker.js and the ORIGIN_URL / ACCESS_* variables the Pages project already holds.
"""
import argparse, json, os, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
POOL = ROOT / "pool"
PUBLIC_URL = "https://pool.lazarus-xbt.xyz"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(HERE / "dist"))
    ap.add_argument("--public-url", default=PUBLIC_URL)
    a = ap.parse_args()

    port = free_port()
    work = Path(tempfile.mkdtemp(prefix="lazarus-pages-build-"))
    cfg = POOL / "config.json"
    if cfg.exists():
        raise SystemExit(f"{cfg} exists; a tree build must not run against a real node config")
    cfg.write_text(json.dumps({"public_url": a.public_url, "listen_port": port}))
    env = dict(os.environ, POOL_DB=str(work / "pool.sqlite"))
    runner = (
        "import sys; sys.path.insert(0, sys.argv[1]); import server;"
        "server.prime_summary = lambda: {'blocks': []};"
        "server.PoolHTTPServer(('127.0.0.1', int(sys.argv[2])), server.Handler).serve_forever()"
    )
    proc = subprocess.Popen([sys.executable, "-c", runner, str(POOL), str(port)], env=env)
    try:
        origin = f"http://127.0.0.1:{port}"
        for _ in range(100):
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                    break
            except OSError:
                if proc.poll() is not None:
                    raise SystemExit("server.py exited before it started listening")
                time.sleep(0.1)
        else:
            raise SystemExit("server.py never started listening")
        subprocess.run([sys.executable, str(HERE / "build.py"), "--origin", origin, "--out", a.out, "--static-from", "repo"], check=True)
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        cfg.unlink(missing_ok=True)


if __name__ == "__main__":
    main()
