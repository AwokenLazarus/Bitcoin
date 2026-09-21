#!/usr/bin/env python3
"""Local preview of the pool site.

    python3 dev/serve.py --tables /path/to/tables.json [--port 8899] [--node http://127.0.0.1:8888]

Runs the real pool/server.py handler read-only, so pages are rendered exactly as production
renders them and /static/* comes from this tree. /api/history is answered locally from a copy of
the node's pool_samples and found_blocks (dump them with the snippet in dev/README). Every
other /api/* call is forwarded to the live node, so the dashboard shows real data without this
process needing the node's RPC cookie, ledger or primed.
"""
import argparse, json, os, sqlite3, sys, tempfile, urllib.request, urllib.error
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--tables", required=True)
ap.add_argument("--port", type=int, default=8899)
ap.add_argument("--node", default="http://127.0.0.1:8888")
ap.add_argument("--no-history", action="store_true", help="forward /api/history too, to see the site against a node that lacks it")
a = ap.parse_args()

pool = Path(__file__).resolve().parent.parent / "pool"
work = Path(tempfile.mkdtemp(prefix="lazarus-dev-"))
dbfile = work / "pool.sqlite"
cfg = pool / "config.json"
made_cfg = not cfg.exists()
if made_cfg:
    cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz", "listen_port": a.port}))
os.environ["POOL_DB"] = str(dbfile)
sys.path.insert(0, str(pool))
try:
    import server  # creates the schema in the empty dev database
finally:
    if made_cfg:
        cfg.unlink()

data = json.loads(Path(a.tables).read_text())
con = sqlite3.connect(dbfile)
con.executemany("INSERT OR REPLACE INTO pool_samples VALUES (?,?,?,?,?)", data["pool_samples"])
con.executemany("INSERT OR REPLACE INTO found_blocks VALUES (?,?,?,?,?,?,?,?)", data["found_blocks"])
con.commit()
con.close()
newest = max(r[0] for r in data["pool_samples"])
server.rollup_pool_hourly(newest)
# The copy is a snapshot: shift "now" to its newest sample so every range has data to show.
_real_time = server.time.time
server.prime_summary = lambda: {"blocks": []}


class Dev(server.Handler):
    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/api/") and (path != "/api/history" or a.no_history):
            try:
                with urllib.request.urlopen(a.node + self.path, timeout=30) as r:
                    body, code, ctype = r.read(), r.status, r.headers.get("Content-Type", "application/json")
            except urllib.error.HTTPError as e:
                body, code, ctype = e.read(), e.code, e.headers.get("Content-Type", "application/json")
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()

    def send_file(self, path, ctype):
        # No browser caching while designing.
        data = Path(path).read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self._write_body(data)


print(f"dev preview on http://127.0.0.1:{a.port}  (api -> {a.node}, history from {a.tables})", flush=True)
server.PoolHTTPServer(("127.0.0.1", a.port), Dev).serve_forever()
