#!/usr/bin/env python3
"""Lazarus public mining-pool dashboard. Scrapes DATUM + Knots; no admin UI exposed."""
from __future__ import annotations

import base64
import hashlib
import json
import re
import socket
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

ROOT = Path(__file__).resolve().parent
DB = ROOT / "pool.sqlite"
STATIC = ROOT / "static"
CONF = json.loads((ROOT / "config.json").read_text())

POOL_FEE = float(CONF.get("pool_fee_percent", 0.5))
STRATUM_HOST = CONF.get("stratum_host", "27.69.0.25")
STRATUM_PORT = int(CONF.get("stratum_port", 23334))
STRATUM_TCP_HOST = CONF.get("stratum_tcp_host", "127.0.0.1")
STRATUM_TCP_PORT = int(CONF.get("stratum_tcp_port", STRATUM_PORT))
STRATUM_GPU_PORT = int(CONF.get("stratum_gpu_port", 3333))
DATUM_URL = CONF.get("datum_url", "http://127.0.0.1:7152")
DATUM_GPU_URL = CONF.get("datum_gpu_url", "http://127.0.0.1:7153")
DATUM_CLIENT_URLS = [(DATUM_URL, "stratum", STRATUM_PORT), (DATUM_GPU_URL, "gpu", STRATUM_GPU_PORT)]
MEMPOOL_API = CONF.get("mempool_api", "http://10.21.21.27:8999")
COOKIE = Path(CONF.get("cookie_file", "/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie"))
AUTH_FILE = Path(CONF.get("datum_auth_file", "/home/umbrel/blake2b/secrets/datum-admin.env"))
EXPLORER = CONF.get("explorer_url", "https://mempool.awokenlazarus.xyz")
COINBASE_TAG = CONF.get("coinbase_tag", "Lazarus")
SUBSIDY = 3.125

PRIME_STATS = CONF.get("datum_prime_stats", "http://127.0.0.1:28916/stats.json")
_prime_pubkey_cache = {"v": "", "ts": 0}


def _datum_prime_pubkey():
    now = time.time()
    if _prime_pubkey_cache["v"] and now - _prime_pubkey_cache["ts"] < 30:
        return _prime_pubkey_cache["v"]
    raw = curl(PRIME_STATS, timeout=3)
    try:
        pk = (json.loads(raw).get("pool") or {}).get("pubkey") or ""
    except Exception:
        pk = ""
    if pk:
        _prime_pubkey_cache["v"] = pk
        _prime_pubkey_cache["ts"] = now
    return pk or _prime_pubkey_cache["v"]


lock = threading.Lock()
browser_stats = {}  # (address, worker) -> {hs, ts}
BROWSER_STAT_TTL = 45
BROWSER_HS_MAX = 5e8


def datum_user_pass():
    user, pw = "mike", ""
    if AUTH_FILE.exists():
        for line in AUTH_FILE.read_text().splitlines():
            if line.startswith("DATUM_ADMIN_USER="):
                user = line.split("=", 1)[1]
            if line.startswith("DATUM_ADMIN_PASSWORD="):
                pw = line.split("=", 1)[1]
    return user, pw


db_conn = sqlite3.connect(DB, check_same_thread=False)
db_conn.row_factory = sqlite3.Row
db_conn.executescript(
    """
    CREATE TABLE IF NOT EXISTS samples (
      ts INTEGER, address TEXT, worker TEXT, hr_ghs REAL, vdiff INTEGER,
      shares_acc INTEGER, shares_rej INTEGER, diff_acc INTEGER, last_share_s REAL
    );
    CREATE TABLE IF NOT EXISTS miners (
      address TEXT PRIMARY KEY, first_ts INTEGER, last_ts INTEGER,
      best_hr_ghs REAL, shares_acc INTEGER, shares_rej INTEGER, diff_acc INTEGER
    );
    CREATE TABLE IF NOT EXISTS pool_samples (
      ts INTEGER PRIMARY KEY, hr_ghs REAL, miners INTEGER, shares_acc INTEGER, shares_rej INTEGER
    );
    CREATE TABLE IF NOT EXISTS found_blocks (
      height INTEGER PRIMARY KEY, hash TEXT, ts INTEGER, reward_btc REAL,
      finder TEXT, pool_fee_btc REAL, miner_btc REAL, coinbase TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_samples_addr_ts ON samples(address, ts);
    CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
    CREATE TABLE IF NOT EXISTS round_work (
      address TEXT PRIMARY KEY, work REAL NOT NULL DEFAULT 0, last_diff_acc INTEGER DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS rounds (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      started_ts INTEGER, closed_ts INTEGER, height INTEGER, hash TEXT,
      reward_btc REAL, fee_btc REAL, miner_btc REAL, total_work REAL, status TEXT
    );
    CREATE TABLE IF NOT EXISTS round_payouts (
      round_id INTEGER, address TEXT, work REAL, share REAL, amount_btc REAL, status TEXT,
      PRIMARY KEY (round_id, address)
    );
    CREATE TABLE IF NOT EXISTS worker_shares (
      address TEXT NOT NULL,
      worker TEXT NOT NULL,
      last_shares_acc INTEGER DEFAULT 0,
      last_shares_rej INTEGER DEFAULT 0,
      lifetime_acc INTEGER DEFAULT 0,
      lifetime_rej INTEGER DEFAULT 0,
      PRIMARY KEY (address, worker)
    );
    CREATE TABLE IF NOT EXISTS prime_miners (
      address TEXT PRIMARY KEY,
      work REAL NOT NULL DEFAULT 0,
      share_percent REAL NOT NULL DEFAULT 0,
      last_ts INTEGER,
      peak_work REAL NOT NULL DEFAULT 0
    );
    """
)
db_conn.commit()


def _ensure_column(table, col, decl):
    cols = {r["name"] for r in db(f"PRAGMA table_info({table})")}
    if col not in cols:
        db(f"ALTER TABLE {table} ADD COLUMN {col} {decl}", write=True)


def _session_delta(cur, prev):
    cur = int(cur or 0)
    prev = int(prev or 0)
    return (cur - prev) if cur >= prev else cur


def backfill_share_lifetimes():
    done = db("SELECT value FROM meta WHERE key='shares_lifetime_backfilled'", one=True)
    if done and str(done["value"]) == "1":
        return
    rows = db("SELECT address, worker, ts, shares_acc, shares_rej FROM samples ORDER BY address, worker, ts")
    acc = {}
    for r in rows or []:
        key = (r["address"], r["worker"] or "")
        st = acc.setdefault(key, {"last_a": 0, "last_r": 0, "life_a": 0, "life_r": 0})
        cur_a = int(r["shares_acc"] or 0)
        cur_r = int(r["shares_rej"] or 0)
        st["life_a"] += _session_delta(cur_a, st["last_a"])
        st["life_r"] += _session_delta(cur_r, st["last_r"])
        st["last_a"], st["last_r"] = cur_a, cur_r
    for (addr, worker), st in acc.items():
        db(
            "INSERT OR REPLACE INTO worker_shares(address,worker,last_shares_acc,last_shares_rej,lifetime_acc,lifetime_rej) VALUES(?,?,?,?,?,?)",
            (addr, worker, st["last_a"], st["last_r"], st["life_a"], st["life_r"]),
            write=True,
        )
    for row in db("SELECT address FROM miners") or []:
        rollup_miner_shares(row["address"])
    db("INSERT OR REPLACE INTO meta(key,value) VALUES('shares_lifetime_backfilled','1')", write=True)


def init_share_accounting():
    _ensure_column("miners", "shares_lifetime", "INTEGER DEFAULT 0")
    _ensure_column("miners", "shares_session", "INTEGER DEFAULT 0")
    _ensure_column("miners", "shares_rej_lifetime", "INTEGER DEFAULT 0")
    db(
        "CREATE TABLE IF NOT EXISTS prime_miners (address TEXT PRIMARY KEY, work REAL NOT NULL DEFAULT 0, share_percent REAL NOT NULL DEFAULT 0, last_ts INTEGER, peak_work REAL NOT NULL DEFAULT 0)",
        write=True,
    )
    _ensure_column("prime_miners", "work_seen", "REAL DEFAULT 0")
    _ensure_column("prime_miners", "work_seen_ts", "INTEGER")
    _ensure_column("prime_miners", "hr_ghs_est", "REAL DEFAULT 0")
    backfill_share_lifetimes()


def rollup_miner_shares(address):
    tot = db(
        "SELECT COALESCE(SUM(lifetime_acc),0) AS a, COALESCE(SUM(lifetime_rej),0) AS r, COALESCE(SUM(last_shares_acc),0) AS s FROM worker_shares WHERE address=?",
        (address,),
        one=True,
    )
    life_a = int(tot["a"]) if tot else 0
    life_r = int(tot["r"]) if tot else 0
    sess = int(tot["s"]) if tot else 0
    db(
        "UPDATE miners SET shares_lifetime=?, shares_rej_lifetime=?, shares_session=?, shares_acc=?, shares_rej=? WHERE address=?",
        (life_a, life_r, sess, life_a, life_r, address),
        write=True,
    )
    return life_a, life_r, sess


def credit_session_shares(address, worker, shares_acc, shares_rej):
    worker = worker or ""
    if not address:
        return 0, 0
    row = db("SELECT * FROM worker_shares WHERE address=? AND worker=?", (address, worker), one=True)
    cur_a, cur_r = int(shares_acc or 0), int(shares_rej or 0)
    if row:
        life_a = int(row["lifetime_acc"]) + _session_delta(cur_a, row["last_shares_acc"])
        life_r = int(row["lifetime_rej"]) + _session_delta(cur_r, row["last_shares_rej"])
        db(
            "UPDATE worker_shares SET last_shares_acc=?, last_shares_rej=?, lifetime_acc=?, lifetime_rej=? WHERE address=? AND worker=?",
            (cur_a, cur_r, life_a, life_r, address, worker),
            write=True,
        )
    else:
        life_a, life_r = cur_a, cur_r
        db(
            "INSERT INTO worker_shares(address,worker,last_shares_acc,last_shares_rej,lifetime_acc,lifetime_rej) VALUES(?,?,?,?,?,?)",
            (address, worker, cur_a, cur_r, life_a, life_r),
            write=True,
        )
    rollup_miner_shares(address)
    return life_a, cur_a


def address_share_totals(address):
    tot = db(
        "SELECT COALESCE(SUM(lifetime_acc),0) AS a, COALESCE(SUM(lifetime_rej),0) AS r, COALESCE(SUM(last_shares_acc),0) AS s FROM worker_shares WHERE address=?",
        (address,),
        one=True,
    )
    if not tot:
        return 0, 0, 0
    return int(tot["a"]), int(tot["r"]), int(tot["s"])


def pool_share_totals():
    tot = db("SELECT COALESCE(SUM(lifetime_acc),0) AS a, COALESCE(SUM(lifetime_rej),0) AS r FROM worker_shares", one=True)
    return (int(tot["a"]), int(tot["r"])) if tot else (0, 0)


def fetch_prime_window():
    raw = curl(PRIME_STATS, timeout=3)
    try:
        data = json.loads(raw) if raw else {}
    except Exception:
        return {}, {"shares": 0, "work": 0, "target_work": 0}
    win = data.get("window") or {}
    by = {}
    for m in win.get("miners") or []:
        ident = (m.get("identity") or "").strip()
        if not ident:
            continue
        try:
            work = int(float(m.get("work") or 0))
        except (TypeError, ValueError):
            work = 0
        by[ident] = {
            "window_work": work,
            "window_percent": float(m.get("share_percent") or 0),
            "window_sats": int(m.get("payout_sats") or 0),
            "payable": bool(m.get("payable")),
        }
    meta = {
        "shares": int(win.get("shares") or 0),
        "work": 0,
        "target_work": 0,
    }
    try:
        meta["work"] = int(float(win.get("work") or 0))
        meta["target_work"] = int(float(win.get("target_work") or 0))
    except (TypeError, ValueError):
        pass
    return by, meta


# One unit of Prime window work is one difficulty-1 share (2**32 hashes).
_PRIME_HASHES_PER_WORK = float(1 << 32)
_PRIME_HR_CAP_GHS = 200000.0
_prime_uptime_cache = {"ts": 0, "uptime": 0}


def _prime_uptime_s():
    now = time.time()
    if now - _prime_uptime_cache["ts"] < 15 and _prime_uptime_cache["uptime"]:
        return _prime_uptime_cache["uptime"]
    try:
        out = subprocess.check_output(
            ["ps", "-o", "etimes=", "-C", "ratum-prime"],
            stderr=subprocess.DEVNULL,
            text=True,
        ).split()
        up = int(out[0]) if out else 0
    except Exception:
        up = 0
    if up:
        _prime_uptime_cache.update({"ts": now, "uptime": up})
    return up


def _prime_window_avg_ghs(work):
    if work <= 0:
        return 0.0
    age = _prime_uptime_s() or 0
    age = max(120, age)  # never treat a brand-new Prime as a 60s spike
    raw = work * _PRIME_HASHES_PER_WORK / age / 1e9
    if raw > _PRIME_HR_CAP_GHS:
        return 0.0
    return raw


def _prime_hr_from_work(addr, work, ts):
    row = db(
        "SELECT work_seen, work_seen_ts, hr_ghs_est FROM prime_miners WHERE address=?",
        (addr,),
        one=True,
    )
    prev_w = float(row["work_seen"] or 0) if row else 0.0
    prev_t = int(row["work_seen_ts"] or 0) if row and row["work_seen_ts"] else 0
    prev_hr = float(row["hr_ghs_est"] or 0) if row else 0.0
    if prev_hr < 1e-6:
        prev_hr = 0.0
    seen_w, seen_t = prev_w, prev_t
    increased = False
    inst = 0.0
    if not prev_t:
        seen_w, seen_t = work, ts
    elif work + 0.5 < prev_w:
        seen_w, seen_t = work, ts
    elif work > prev_w + 0.5:
        dt = max(1, ts - prev_t)
        raw = (work - prev_w) * _PRIME_HASHES_PER_WORK / dt / 1e9
        if 0 < raw <= _PRIME_HR_CAP_GHS:
            inst = (0.4 * raw + 0.6 * prev_hr) if prev_hr > 1e-6 else raw
        increased = True
        seen_w, seen_t = work, ts
    window_avg = _prime_window_avg_ghs(work)
    # Instant rate when they are still landing work; otherwise the window average
    # so a miner who already banked this round is not shown as 0 H/s.
    est = inst if inst > 1e-6 else window_avg
    last_share_s = 0.0 if increased or not prev_t else float(max(0, ts - prev_t))
    return est, last_share_s, seen_w, seen_t


def persist_prime_miners(by, ts):
    for addr, info in by.items():
        work = int(info.get("window_work") or 0)
        hr, last_share_s, seen_w, seen_t = _prime_hr_from_work(addr, work, ts)
        info["hr_ghs"] = hr
        info["last_share_s"] = last_share_s
        prev = db("SELECT peak_work FROM prime_miners WHERE address=?", (addr,), one=True)
        peak = max(int(prev["peak_work"] or 0) if prev else 0, work)
        if prev:
            db(
                "UPDATE prime_miners SET work=?, share_percent=?, last_ts=?, peak_work=?, work_seen=?, work_seen_ts=?, hr_ghs_est=? WHERE address=?",
                (work, info.get("window_percent") or 0, ts, peak, seen_w, seen_t, hr, addr),
                write=True,
            )
        else:
            db(
                "INSERT INTO prime_miners(address,work,share_percent,last_ts,peak_work,work_seen,work_seen_ts,hr_ghs_est) VALUES(?,?,?,?,?,?,?,?)",
                (addr, work, info.get("window_percent") or 0, ts, peak, seen_w, seen_t, hr),
                write=True,
            )
        row = db("SELECT address FROM miners WHERE address=?", (addr,), one=True)
        if row:
            db("UPDATE miners SET last_ts=? WHERE address=?", (ts, addr), write=True)
        else:
            db(
                "INSERT INTO miners(address,first_ts,last_ts,best_hr_ghs,shares_acc,shares_rej,diff_acc,shares_lifetime,shares_session,shares_rej_lifetime) VALUES(?,?,?,?,?,?,?,?,?,?)",
                (addr, ts, ts, 0, 0, 0, info.get("window_work") or 0, 0, 0, 0),
                write=True,
            )


def prime_info_for(address):
    live = (state.get("prime") or {}).get(address) if isinstance(state.get("prime"), dict) else None
    if live:
        out = dict(live)
        if out.get("hr_ghs") is None:
            row = db("SELECT hr_ghs_est, work_seen_ts FROM prime_miners WHERE address=?", (address,), one=True)
            if row:
                out["hr_ghs"] = float(row["hr_ghs_est"] or 0)
                seen = int(row["work_seen_ts"] or 0) if row["work_seen_ts"] else 0
                out.setdefault("last_share_s", float(max(0, int(time.time()) - seen)) if seen else 0.0)
        return out
    row = db("SELECT work, share_percent, peak_work, last_ts, hr_ghs_est, work_seen_ts FROM prime_miners WHERE address=?", (address,), one=True)
    if not row:
        return {}
    seen = int(row["work_seen_ts"] or 0) if row["work_seen_ts"] else 0
    return {
        "window_work": int(row["work"] or 0),
        "window_percent": float(row["share_percent"] or 0),
        "window_sats": 0,
        "payable": True,
        "window_peak": int(row["peak_work"] or 0),
        "window_last_ts": int(row["last_ts"] or 0),
        "hr_ghs": float(row["hr_ghs_est"] or 0),
        "last_share_s": float(max(0, int(time.time()) - seen)) if seen else 0.0,
    }


def attach_share_fields(rec):
    addr = rec.get("address") or ""
    worker = rec.get("worker") or ""
    life_a, life_r, _ = address_share_totals(addr)
    wrow = db("SELECT lifetime_acc FROM worker_shares WHERE address=? AND worker=?", (addr, worker), one=True)
    rec["shares_lifetime"] = int(wrow["lifetime_acc"]) if wrow else life_a
    rec["shares_session"] = int(rec.get("shares_session") if rec.get("shares_session") is not None else (rec.get("shares_acc") or 0))
    rec["shares_acc"] = rec["shares_lifetime"] or rec["shares_session"]
    rec["shares_rej"] = life_r if rec.get("shares_rej") is None else rec.get("shares_rej")
    info = prime_info_for(addr)
    rec["window_work"] = int(info.get("window_work") or 0)
    rec["window_percent"] = float(info.get("window_percent") or 0)
    rec["window_sats"] = int(info.get("window_sats") or 0)
    rec["via"] = rec.get("via") or ("stratum" if rec.get("ua") != "DATUM gateway" else "gateway")
    phr = float(info.get("hr_ghs") or 0)
    if phr > 1e-6 and (rec.get("via") == "gateway" or rec.get("ua") == "DATUM gateway"):
        if not rec.get("hr_ghs") or float(rec.get("hr_ghs") or 0) < 1e-6:
            rec["hr_ghs"] = phr
        if info.get("last_share_s") is not None and not rec.get("last_share_s"):
            rec["last_share_s"] = float(info.get("last_share_s") or 0)
    return rec


def merge_prime_online(miners):
    by = state.get("prime") or {}
    if not isinstance(by, dict):
        by = {}
    have = {m.get("address") for m in miners}
    for m in miners:
        addr = m.get("address") or ""
        info = by.get(addr) or prime_info_for(addr)
        if info:
            m["window_work"] = int(info.get("window_work") or 0)
            m["window_percent"] = float(info.get("window_percent") or 0)
            m["window_sats"] = int(info.get("window_sats") or 0)
        if addr in by:
            if m.get("ua") == "DATUM gateway":
                m["via"] = "gateway"
            elif m.get("via") == "gpu":
                m["via"] = "gpu"
            elif (m.get("ua") or "").startswith("lazarus-web") or m.get("via") == "stratum":
                m["via"] = "stratum"
            elif m.get("host") or (m.get("ua") and m.get("ua") != "DATUM gateway"):
                m["via"] = "stratum"
        attach_share_fields(m)
    extras = []
    for addr, info in by.items():
        if addr in have:
            continue
        rec = {
            "address": addr,
            "worker": "gateway",
            "user": addr,
            "host": "",
            "hr_ghs": float(info.get("hr_ghs") or 0),
            "vdiff": 0,
            "diff_acc": int(info.get("window_work") or 0),
            "shares_acc": 0,
            "shares_session": 0,
            "shares_lifetime": 0,
            "diff_rej": 0,
            "shares_rej": 0,
            "last_share_s": float(info.get("last_share_s") or 0),
            "ua": "DATUM gateway",
            "online": True,
            "via": "gateway",
            "window_work": int(info.get("window_work") or 0),
            "window_percent": float(info.get("window_percent") or 0),
            "window_sats": int(info.get("window_sats") or 0),
        }
        attach_share_fields(rec)
        extras.append(rec)
    return miners + extras


def ensure_open_round():
    openr = db("SELECT id FROM rounds WHERE status='open' ORDER BY id DESC", one=True)
    if not openr:
        db(
            "INSERT INTO rounds(started_ts,status,total_work) VALUES(?, 'open', 0)",
            (int(time.time()),),
            write=True,
        )


def db(q, args=(), one=False, write=False):
    with lock:
        cur = db_conn.execute(q, args)
        if write:
            db_conn.commit()
            return cur.lastrowid
        rows = cur.fetchall()
        return rows[0] if one and rows else (rows if not one else None)


def curl(url, digest=False, timeout=8):
    cmd = ["curl", "-sS", "--max-time", str(timeout)]
    if digest:
        u, p = datum_user_pass()
        cmd += ["--digest", "-u", f"{u}:{p}"]
    cmd.append(url)
    try:
        return subprocess.check_output(cmd, stderr=subprocess.DEVNULL).decode("utf-8", "replace")
    except Exception:
        return ""


def rpc(method, params=None):
    if not COOKIE.exists():
        return None
    auth = COOKIE.read_text().strip()
    payload = json.dumps({"jsonrpc": "1.0", "id": "p", "method": method, "params": params or []})
    try:
        out = subprocess.check_output(
            [
                "curl", "-sS", "--max-time", "8", "--user", auth,
                "--data-binary", payload, "-H", "content-type:text/plain",
                "http://127.0.0.1:9332",
            ],
            stderr=subprocess.DEVNULL,
        )
        return json.loads(out).get("result")
    except Exception:
        return None


def parse_hr(s):
    m = re.search(r"([0-9.]+)\s*(H|KH|MH|GH|TH|PH)/s", s or "", re.I)
    if not m:
        return 0.0
    n = float(m.group(1))
    unit = m.group(2).upper()
    mul = {"H": 1e-9, "KH": 1e-6, "MH": 1e-3, "GH": 1, "TH": 1e3, "PH": 1e6}
    return n * mul.get(unit, 1)


def split_user(u):
    u = (u or "").strip()
    if "." in u:
        addr, worker = u.split(".", 1)
    else:
        addr, worker = u, ""
    return addr, worker


def prune_browser_stats(now=None):
    now = time.time() if now is None else now
    dead = [k for k, v in browser_stats.items() if now - v["ts"] > BROWSER_STAT_TTL]
    for k in dead:
        browser_stats.pop(k, None)


def lookup_browser_hs(address, worker):
    now = time.time()
    prune_browser_stats(now)
    st = browser_stats.get((address, worker or ""))
    if st and now - st["ts"] <= BROWSER_STAT_TTL:
        return float(st["hs"])
    return None


def record_browser_stat(user, hs):
    addr, worker = split_user(user)
    if not addr or not re.match(r"^(bc1|[13])[a-zA-HJ-NP-Z0-9]{20,90}$", addr):
        return False
    if worker and not re.match(r"^[A-Za-z0-9._-]{0,32}$", worker):
        return False
    try:
        hs = float(hs)
    except (TypeError, ValueError):
        return False
    if hs < 0 or hs > BROWSER_HS_MAX:
        return False
    with lock:
        browser_stats[(addr, worker)] = {"hs": hs, "ts": time.time()}
    return True


def apply_browser_hr(miners):
    for m in miners or []:
        reported = lookup_browser_hs(m.get("address"), m.get("worker"))
        ua = m.get("ua") or ""
        worker = m.get("worker") or ""
        if reported is not None and (
            ua.startswith("lazarus-web") or worker.startswith("web") or worker == "browser"
        ):
            m["hr_ghs"] = reported / 1e9
    return miners


def online_miners():
    miners = apply_browser_hr(list(state.get("miners") or []))
    have = {(m.get("address"), m.get("worker") or "") for m in miners}
    now = time.time()
    prune_browser_stats(now)
    extras = []
    with lock:
        items = list(browser_stats.items())
    for (addr, worker), st in items:
        if now - st["ts"] > BROWSER_STAT_TTL:
            continue
        if (addr, worker) in have:
            continue
        extras.append(
            {
                "address": addr,
                "worker": worker,
                "user": f"{addr}.{worker}" if worker else addr,
                "host": "",
                "hr_ghs": float(st["hs"]) / 1e9,
                "vdiff": 0,
                "diff_acc": 0,
                "shares_acc": 0,
                "shares_session": 0,
                "shares_lifetime": 0,
                "diff_rej": 0,
                "shares_rej": 0,
                "last_share_s": 0,
                "ua": "lazarus-web/0.1",
                "online": True,
            }
        )
    return merge_prime_online(miners + extras)


def ascii_from_hex(hx):
    try:
        raw = bytes.fromhex(hx)
    except Exception:
        return ""
    return "".join(chr(x) if 32 <= x < 127 else "." for x in raw)


def _scrape_datum_home(url):
    home = curl(url + "/")
    text_home = re.sub(r"<[^>]+>", " ", home)
    pool_hr = parse_hr(
        re.search(r"Estimated Hashrate:\s*([0-9.]+\s*\w+/s(?:ec)?)", text_home, re.I).group(1)
        if re.search(r"Estimated Hashrate:\s*([0-9.]+\s*\w+/s(?:ec)?)", text_home, re.I)
        else ""
    )
    if "GH/sec" in text_home:
        m = re.search(r"Estimated Hashrate:\s*([0-9.]+)\s*GH", text_home)
        if m:
            pool_hr = float(m.group(1))
    acc = rej = 0
    ma = re.search(r"Local Shares Accepted:\s*(\d+)", text_home)
    mr = re.search(r"Local Shares Rejected:\s*(\d+)", text_home)
    if ma:
        acc = int(ma.group(1))
    if mr:
        rej = int(mr.group(1))
    return pool_hr, acc, rej


def _scrape_datum_clients(url, via, stratum_port):
    clients = curl(url + "/clients", digest=True)
    miners = []
    for row in re.findall(r"<TR>(.*?)</TR>", clients, re.I | re.S):
        tds = re.findall(r"<TD[^>]*>(.*?)</TD>", row, re.I | re.S)
        if len(tds) < 11 or "Auth Username" in tds[2]:
            continue

        def strip(x):
            return re.sub(r"<[^>]+>", "", x).strip()

        host = strip(tds[1])
        user = strip(tds[2])
        last = strip(tds[4])
        vdiff = strip(tds[5])
        diffa = strip(tds[6])
        diffr = strip(tds[7])
        hr_s = strip(tds[8])
        ua = strip(tds[10])
        addr, worker = split_user(user)
        am = re.search(r"(\d+)\s*\((\d+)\)", diffa)
        rm = re.search(r"(\d+)\s*\((\d+)\)", diffr)
        last_s = 0.0
        lm = re.search(r"([0-9.]+)\s*s", last)
        if lm:
            last_s = float(lm.group(1))
        hr_ghs = parse_hr(hr_s)
        reported = lookup_browser_hs(addr, worker)
        if reported is not None and (ua.startswith("lazarus-web") or worker.startswith("web") or worker == "browser"):
            hr_ghs = reported / 1e9
        rec = {
            "address": addr,
            "worker": worker,
            "user": user,
            "host": host,
            "hr_ghs": hr_ghs,
            "vdiff": int(vdiff) if vdiff.isdigit() else 0,
            "diff_acc": int(am.group(1)) if am else 0,
            "shares_acc": int(am.group(2)) if am else 0,
            "diff_rej": int(rm.group(1)) if rm else 0,
            "shares_rej": int(rm.group(2)) if rm else 0,
            "last_share_s": last_s,
            "ua": ua,
            "online": True,
            "via": via,
            "stratum_port": stratum_port,
        }
        rec["shares_session"] = rec["shares_acc"]
        rec["shares_lifetime"] = rec["shares_acc"]
        miners.append(rec)
    return miners


def scrape():
    pool_hr = acc = rej = 0
    miners = []
    seen = set()
    for url, via, port in DATUM_CLIENT_URLS:
        phr, a, r = _scrape_datum_home(url)
        pool_hr += phr
        acc += a
        rej += r
        for rec in _scrape_datum_clients(url, via, port):
            key = (rec.get("address"), rec.get("worker"), rec.get("via"))
            if key in seen:
                continue
            seen.add(key)
            miners.append(rec)
    ts = int(time.time())
    for rec in miners:
        db(
            "INSERT INTO samples(ts,address,worker,hr_ghs,vdiff,shares_acc,shares_rej,diff_acc,last_share_s) VALUES(?,?,?,?,?,?,?,?,?)",
            (ts, rec["address"], rec["worker"], rec["hr_ghs"], rec["vdiff"], rec["shares_acc"], rec["shares_rej"], rec["diff_acc"], rec["last_share_s"]),
            write=True,
        )
        prev = db("SELECT * FROM miners WHERE address=?", (rec["address"],), one=True)
        if prev:
            db(
                "UPDATE miners SET last_ts=?, best_hr_ghs=MAX(best_hr_ghs,?), diff_acc=? WHERE address=?",
                (ts, rec["hr_ghs"], rec["diff_acc"], rec["address"]),
                write=True,
            )
        else:
            db(
                "INSERT INTO miners(address,first_ts,last_ts,best_hr_ghs,shares_acc,shares_rej,diff_acc,shares_lifetime,shares_session,shares_rej_lifetime) VALUES(?,?,?,?,?,?,?,?,?,?)",
                (rec["address"], ts, ts, rec["hr_ghs"], rec["shares_acc"], rec["shares_rej"], rec["diff_acc"], rec["shares_acc"], rec["shares_acc"], rec["shares_rej"]),
                write=True,
            )
        life, sess = credit_session_shares(rec["address"], rec["worker"], rec["shares_acc"], rec["shares_rej"])
        rec["shares_session"] = sess
        rec["shares_lifetime"] = life
        credit_round_work(rec["address"], rec["diff_acc"])
    db("DELETE FROM samples WHERE ts < ?", (ts - 3 * 86400,), write=True)
    db("DELETE FROM pool_samples WHERE ts < ?", (ts - 7 * 86400,), write=True)
    prime_by, prime_meta = fetch_prime_window()
    persist_prime_miners(prime_by, ts)
    stratum_addrs = {m.get("address") for m in miners}
    gw_hr = 0.0
    gw_n = 0
    for addr, info in prime_by.items():
        if addr in stratum_addrs:
            continue
        hr = float(info.get("hr_ghs") or 0)
        gw_hr += hr
        gw_n += 1
        db(
            "INSERT INTO samples(ts,address,worker,hr_ghs,vdiff,shares_acc,shares_rej,diff_acc,last_share_s) VALUES(?,?,?,?,?,?,?,?,?)",
            (ts, addr, "gateway", hr, 0, 0, 0, int(info.get("window_work") or 0), float(info.get("last_share_s") or 0)),
            write=True,
        )
        db(
            "UPDATE miners SET last_ts=?, best_hr_ghs=MAX(best_hr_ghs,?) WHERE address=?",
            (ts, hr, addr),
            write=True,
        )
    live_hr = (sum(m["hr_ghs"] for m in miners) + gw_hr) or pool_hr
    db(
        "INSERT OR REPLACE INTO pool_samples(ts,hr_ghs,miners,shares_acc,shares_rej) VALUES(?,?,?,?,?)",
        (ts, live_hr, len(miners) + gw_n, acc, rej),
        write=True,
    )
    return {"pool_hr_ghs": live_hr, "shares_acc": acc, "shares_rej": rej, "miners": miners, "ts": ts, "prime": prime_by, "prime_meta": prime_meta}



def credit_round_work(address, diff_acc):
    if not address:
        return
    ensure_open_round()
    row = db("SELECT work, last_diff_acc FROM round_work WHERE address=?", (address,), one=True)
    prev = int(row["last_diff_acc"]) if row else 0
    cur = int(diff_acc or 0)
    delta = cur - prev if cur >= prev else cur
    if delta < 0:
        delta = 0
    if row:
        db(
            "UPDATE round_work SET work=work+?, last_diff_acc=? WHERE address=?",
            (delta, cur, address),
            write=True,
        )
    else:
        db(
            "INSERT INTO round_work(address,work,last_diff_acc) VALUES(?,?,?)",
            (address, float(delta), cur),
            write=True,
        )


def close_round_for_block(height, blockhash, reward, fee_btc, miner_btc):
    ensure_open_round()
    already = db("SELECT id FROM rounds WHERE height=?", (height,), one=True)
    if already:
        return
    openr = db("SELECT id FROM rounds WHERE status='open' ORDER BY id DESC", one=True)
    rid = int(openr["id"])
    rows = db("SELECT address, work FROM round_work WHERE work > 0")
    total = sum(float(r["work"]) for r in rows) if rows else 0.0
    db(
        "UPDATE rounds SET closed_ts=?, height=?, hash=?, reward_btc=?, fee_btc=?, miner_btc=?, total_work=?, status='immature' WHERE id=?",
        (int(time.time()), height, blockhash, reward, fee_btc, miner_btc, total, rid),
        write=True,
    )
    if total > 0:
        for r in rows:
            share = float(r["work"]) / total
            amt = miner_btc * share
            db(
                "INSERT OR REPLACE INTO round_payouts(round_id,address,work,share,amount_btc,status) VALUES(?,?,?,?,?,?)",
                (rid, r["address"], float(r["work"]), share, amt, "immature"),
                write=True,
            )
    db("DELETE FROM round_work", write=True)
    db("INSERT INTO rounds(started_ts,status,total_work) VALUES(?, 'open', 0)", (int(time.time()),), write=True)


def mature_rounds():
    tip = rpc("getblockcount") or 0
    rows = db("SELECT id, height FROM rounds WHERE status='immature'")
    for r in rows or []:
        if r["height"] and int(tip) >= int(r["height"]) + 100:
            db("UPDATE rounds SET status='payable' WHERE id=?", (r["id"],), write=True)
            db(
                "UPDATE round_payouts SET status='unpaid' WHERE round_id=? AND status='immature'",
                (r["id"],),
                write=True,
            )


def scan_found_blocks():
    tip = rpc("getblockcount")
    if not tip:
        return
    db(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT)",
        write=True,
    )
    row = db("SELECT value FROM meta WHERE key='scan_height'", one=True)
    if row and row["value"]:
        start = int(row["value"]) + 1
    else:
        start = max(961640, int(tip) - 80)
    if start > tip:
        return
    end = min(int(tip), start + 39)
    last_ok = start - 1
    for height in range(start, end + 1):
        h = rpc("getblockhash", [height])
        if not h:
            break
        blk = rpc("getblock", [h, 2])
        if not blk:
            break
        last_ok = height
        tx0 = (blk.get("tx") or [None])[0] or {}
        vin = (tx0.get("vin") or [{}])[0]
        cb = vin.get("coinbase") or ""
        text = ascii_from_hex(cb)
        if COINBASE_TAG not in text:
            continue
        vouts = tx0.get("vout") or []
        reward = sum(float(v.get("value") or 0) for v in vouts)
        addrs = []
        for v in vouts:
            spk = v.get("scriptPubKey") or {}
            a = spk.get("address") or (spk.get("addresses") or [None])[0]
            if a:
                addrs.append(a)
        miner_btc = reward * (1 - POOL_FEE / 100.0)
        fee_btc = reward * (POOL_FEE / 100.0)
        finder = addrs[0] if addrs else ""
        existed = db("SELECT height FROM found_blocks WHERE height=?", (height,), one=True)
        db(
            "INSERT OR REPLACE INTO found_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase) VALUES(?,?,?,?,?,?,?,?)",
            (height, h, blk.get("time"), reward, finder, fee_btc, miner_btc, text[:200]),
            write=True,
        )
        if not existed:
            close_round_for_block(height, h, reward, fee_btc, miner_btc)
    if last_ok >= start:
        db("INSERT OR REPLACE INTO meta(key,value) VALUES('scan_height',?)", (str(last_ok),), write=True)


state = {"pool_hr_ghs": 0, "shares_acc": 0, "shares_rej": 0, "miners": [], "ts": 0, "prime": {}, "prime_meta": {}}


def loop():
    global state
    while True:
        try:
            state = scrape()
        except Exception as e:
            print("scrape", e, flush=True)
        try:
            scan_found_blocks()
            mature_rounds()
        except Exception as e:
            print("scan", e, flush=True)
        time.sleep(10)


def node_info():
    mi = rpc("getmininginfo") or {}
    bi = rpc("getblockchaininfo") or {}
    return {
        "height": mi.get("blocks") or bi.get("blocks"),
        "difficulty": mi.get("difficulty"),
        "networkhashps": mi.get("networkhashps"),
        "chain": bi.get("chain"),
    }



def addr_script(addr):
    info = rpc("validateaddress", [addr]) or {}
    spk = info.get("scriptPubKey")
    if not spk:
        return b""
    try:
        return bytes.fromhex(spk)
    except Exception:
        return b""


def coinbaser_blob(value_sats):
    """DATUM v2 coinbaser: miner outputs only. Remainder (fee + dust) stays for the pool address."""
    value_sats = int(value_sats)
    if value_sats <= 0:
        return bytes([1]), []
    fee_bp = int(round(POOL_FEE * 100))
    rest = (value_sats * (10000 - fee_bp)) // 10000
    rows = db("SELECT address, work FROM round_work WHERE work > 0")
    merged = {}
    for r in rows or []:
        merged[r["address"]] = merged.get(r["address"], 0.0) + float(r["work"])
    total = sum(merged.values())
    outs = []
    used = 0
    if total > 0 and rest > 0:
        for addr, w in sorted(merged.items(), key=lambda x: -x[1]):
            amt = int(rest * w / total)
            if amt < 546 or used + amt > rest:
                continue
            script = addr_script(addr)
            if not script or not (2 <= len(script) <= 64):
                continue
            outs.append({"address": addr, "sats": amt, "script": script.hex()})
            used += amt
            if len(outs) >= 512:
                break
    blob = bytearray([1])
    for o in outs:
        script = bytes.fromhex(o["script"])
        blob += int(o["sats"]).to_bytes(8, "little")
        blob += bytes([len(script)])
        blob += script
    return bytes(blob), outs

def mempool_blocks():
    try:
        raw = curl(f"{MEMPOOL_API}/api/v1/blocks")
        blocks = json.loads(raw)
        out = []
        for b in blocks if isinstance(blocks, list) else []:
            pool = (b.get("extras") or {}).get("pool") or {}
            out.append(
                {
                    "height": b.get("height"),
                    "id": b.get("id"),
                    "timestamp": b.get("timestamp"),
                    "pool": pool.get("name") or "Unknown",
                    "tx_count": b.get("tx_count"),
                    "explorer": f"{EXPLORER}/block/{b.get('id')}",
                }
            )
        return out
    except Exception:
        return []


def luck_and_ttf(pool_hr_ghs, net_hs, first_ts):
    net_ghs = (float(net_hs) / 1e9) if net_hs else 0
    share = (pool_hr_ghs / net_ghs) if net_ghs else 0
    ttf_s = (600.0 / share) if share else None
    found = db("SELECT COUNT(*) AS n FROM found_blocks", one=True)
    nfound = int(found["n"]) if found else 0
    elapsed = max(0, int(time.time()) - int(first_ts or time.time()))
    expected = (elapsed / 600.0) * share if share else 0
    luck = (nfound / expected * 100.0) if expected > 0.01 else None
    return share, ttf_s, nfound, expected, luck


def pool_payload():
    node = node_info()
    miners = online_miners()
    online = len(miners)
    pool_hr = sum(m["hr_ghs"] for m in miners) or state.get("pool_hr_ghs") or 0
    net = float(node.get("networkhashps") or 0)
    first = db("SELECT MIN(first_ts) AS t FROM miners", one=True)
    first_ts = first["t"] if first and first["t"] else state.get("ts")
    share, ttf_s, nfound, expected, luck = luck_and_ttf(pool_hr, net, first_ts)
    est_btc_day = share * 144 * SUBSIDY * (1 - POOL_FEE / 100.0)
    known = db("SELECT COUNT(*) AS n FROM miners", one=True)
    hist = db("SELECT ts, hr_ghs, miners FROM pool_samples WHERE ts > ? ORDER BY ts", (int(time.time()) - 86400,))
    return {
        "name": "Lazarus",
        "tagline": "Proverbs 11:1",
        "fee_percent": POOL_FEE,
        "stratum": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "stratum_asic": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "stratum_gpu": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_GPU_PORT}",
        "host": STRATUM_HOST,
        "port": STRATUM_PORT,
        "port_gpu": STRATUM_GPU_PORT,
        "pool_hr_ghs": pool_hr,
        "miners_online": online,
        "workers_online": online,
        "miners_seen": int(known["n"]) if known else online,
        "shares_accepted": pool_share_totals()[0] or (state.get("shares_acc") or 0),
        "shares_session": state.get("shares_acc") or 0,
        "shares_rejected": pool_share_totals()[1] or (state.get("shares_rej") or 0),
        "shares_note": "Accepted is cumulative for an address and does not drop if you reconnect or switch from public stratum to your own DATUM gateway. Session is only this public-stratum connection. The payout window is Prime work for that same address — that is what a found block pays.",
        "window_shares": (state.get("prime_meta") or {}).get("shares") or 0,
        "window_work": (state.get("prime_meta") or {}).get("work") or 0,
        "height": node.get("height"),
        "difficulty": node.get("difficulty"),
        "network_hr_hs": net,
        "pool_share": share,
        "est_btc_day": est_btc_day,
        "ttf_seconds": ttf_s,
        "blocks_found": nfound,
        "blocks_expected": expected,
        "luck_percent": luck,
        "subsidy_btc": SUBSIDY,
        "finder_payout_btc": SUBSIDY * (1 - POOL_FEE / 100.0),
        "payout": f"Ocean-style coinbase: {100-POOL_FEE:g}% split by accepted work this round, paid in the found block; {POOL_FEE:g}% pool fee",
        "payout_scheme": "PROP",
        "datum": {
            "pool_host": CONF.get("datum_prime_host", STRATUM_HOST),
            "pool_port": int(CONF.get("datum_prime_port", 28915)),
            "pool_pubkey": _datum_prime_pubkey(),
            "pool_pass_workers": True,
            "pool_pass_full_users": True,
            "pooled_mining_only": True,
        },
        "payouts_onchain": True,
        "explorer": EXPLORER,
        "updated": state.get("ts") or int(time.time()),
        "history": [{"ts": r["ts"], "hr_ghs": r["hr_ghs"], "miners": r["miners"]} for r in hist],
    }


def miner_payload(address):
    recs = [m for m in online_miners() if m["address"] == address]
    hist = db(
        "SELECT ts, SUM(hr_ghs) AS hr FROM samples WHERE address=? AND ts > ? GROUP BY ts ORDER BY ts",
        (address, int(time.time()) - 86400),
    )
    stored = db("SELECT * FROM miners WHERE address=?", (address,), one=True)
    hr = sum(m["hr_ghs"] for m in recs)
    node = node_info()
    net_ghs = (float(node.get("networkhashps") or 0)) / 1e9
    share = (hr / net_ghs) if net_ghs else 0
    est = share * 144 * SUBSIDY * (1 - POOL_FEE / 100.0)
    pool_hr = sum(m["hr_ghs"] for m in online_miners()) or 1e-9
    contrib = hr / pool_hr if pool_hr else 0
    payouts = db(
        "SELECT r.height, r.hash, r.closed_ts AS ts, p.amount_btc AS miner_btc, p.share, p.work, p.status, r.status AS round_status "
        "FROM round_payouts p JOIN rounds r ON r.id=p.round_id WHERE p.address=? ORDER BY r.height DESC LIMIT 50",
        (address,),
    )
    earned = db("SELECT COALESCE(SUM(amount_btc),0) AS s FROM round_payouts WHERE address=? AND status='paid'", (address,), one=True)
    unpaid = db("SELECT COALESCE(SUM(amount_btc),0) AS s FROM round_payouts WHERE address=? AND status='unpaid'", (address,), one=True)
    immature = db("SELECT COALESCE(SUM(amount_btc),0) AS s FROM round_payouts WHERE address=? AND status='immature'", (address,), one=True)
    rw = db("SELECT work FROM round_work WHERE address=?", (address,), one=True)
    tw = db("SELECT COALESCE(SUM(work),0) AS s FROM round_work", one=True)
    my_work = float(rw["work"]) if rw else 0.0
    tot_work = float(tw["s"]) if tw else 0.0
    round_share = (my_work / tot_work) if tot_work else 0.0
    ttf_s = (600.0 / share) if share else None
    known = bool(stored or recs)
    life_a, life_r, sess_stored = address_share_totals(address) if address else (0, 0, 0)
    sess_live = sum(int(m.get("shares_session") or 0) for m in recs if (m.get("via") or "stratum") != "gateway") if recs else 0
    pinfo = prime_info_for(address) if address else {}
    if pinfo.get("window_percent"):
        round_share = float(pinfo["window_percent"]) / 100.0
    vias = {m.get("via") for m in recs if m.get("via")}
    if ("stratum" in vias or "gpu" in vias) and "gateway" in vias:
        via = "both"
    elif "gateway" in vias and "stratum" not in vias and "gpu" not in vias:
        via = "gateway"
    elif "gpu" in vias and "stratum" not in vias:
        via = "gpu"
    elif recs:
        via = "stratum" if "stratum" in vias else (next(iter(vias)) if vias else "stratum")
    elif pinfo.get("window_work"):
        via = "gateway"
        recs = [{
            "address": address, "worker": "gateway", "hr_ghs": float(pinfo.get("hr_ghs") or 0), "shares_acc": life_a,
            "shares_session": 0, "shares_lifetime": life_a, "shares_rej": life_r,
            "vdiff": 0, "last_share_s": float(pinfo.get("last_share_s") or 0), "ua": "DATUM gateway", "via": "gateway",
            "window_work": pinfo.get("window_work") or 0, "window_percent": pinfo.get("window_percent") or 0,
        }]
    else:
        via = ""
    known = bool(stored or recs or pinfo.get("window_work") or life_a)
    return {
        "address": address if known else "",
        "known": known,
        "online": bool(recs) or bool(pinfo.get("window_work")),
        "workers": recs,
        "hr_ghs": hr,
        "shares_acc": life_a or int(pinfo.get("window_work") or 0),
        "shares_lifetime": life_a or int(pinfo.get("window_work") or 0),
        "shares_session": sess_live,
        "shares_rej": life_r,
        "via": via,
        "window_work": int(pinfo.get("window_work") or 0),
        "window_percent": float(pinfo.get("window_percent") or 0),
        "window_sats": int(pinfo.get("window_sats") or 0),
        "diff_acc": recs[0]["diff_acc"] if recs else (stored["diff_acc"] if stored else 0),
        "first_seen": stored["first_ts"] if stored else None,
        "last_seen": stored["last_ts"] if stored else None,
        "best_hr_ghs": stored["best_hr_ghs"] if stored else hr,
        "pool_contribution": contrib,
        "est_btc_day": est,
        "est_btc_week": est * 7,
        "ttf_seconds": ttf_s,
        "block_payout_btc": SUBSIDY * (1 - POOL_FEE / 100.0) * (round_share or contrib),
        "paid_btc": float(earned["s"]) if earned else 0,
        "unpaid_btc": float(unpaid["s"]) if unpaid else 0,
        "immature_btc": float(immature["s"]) if immature else 0,
        "round_work": my_work,
        "round_share": round_share,
        "blocks_found": [dict(r) for r in payouts],
        "fee_percent": POOL_FEE,
        "history": [{"ts": r["ts"], "hr_ghs": r["hr"]} for r in hist],
    }



WS_MAGIC = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _recvall(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return b""
        buf += chunk
    return buf


def ws_accept_key(key: str) -> str:
    return base64.b64encode(hashlib.sha1((key + WS_MAGIC).encode()).digest()).decode()


def ws_recv_frame(sock):
    hdr = _recvall(sock, 2)
    if not hdr:
        return None, None
    b0, b1 = hdr[0], hdr[1]
    opcode = b0 & 0x0F
    masked = bool(b1 & 0x80)
    length = b1 & 0x7F
    if length == 126:
        ext = _recvall(sock, 2)
        if not ext:
            return None, None
        length = int.from_bytes(ext, "big")
    elif length == 127:
        ext = _recvall(sock, 8)
        if not ext:
            return None, None
        length = int.from_bytes(ext, "big")
    mask = _recvall(sock, 4) if masked else b""
    payload = _recvall(sock, length) if length else b""
    if masked and payload:
        payload = bytes(payload[i] ^ mask[i % 4] for i in range(len(payload)))
    return opcode, payload


def ws_send_frame(sock, opcode, payload: bytes):
    header = bytearray([0x80 | opcode])
    n = len(payload)
    if n < 126:
        header.append(n)
    elif n < 65536:
        header.append(126)
        header.extend(n.to_bytes(2, "big"))
    else:
        header.append(127)
        header.extend(n.to_bytes(8, "big"))
    sock.sendall(bytes(header) + payload)


def bridge_stratum(client_sock):
    up = socket.create_connection((STRATUM_TCP_HOST, STRATUM_TCP_PORT), 10)
    up.settimeout(60)
    client_sock.settimeout(60)
    dead = threading.Event()

    def client_to_up():
        try:
            while not dead.is_set():
                try:
                    opcode, payload = ws_recv_frame(client_sock)
                except TimeoutError:
                    continue
                if opcode is None:
                    break
                if opcode == 8:
                    break
                if opcode == 9:
                    try:
                        ws_send_frame(client_sock, 10, payload or b"")
                    except Exception:
                        break
                    continue
                if opcode in (1, 2) and payload:
                    if not payload.endswith(b"\n"):
                        payload += b"\n"
                    up.sendall(payload)
        except Exception:
            pass
        dead.set()
        try:
            up.shutdown(socket.SHUT_RDWR)
        except Exception:
            pass

    def up_to_client():
        buf = b""
        try:
            while not dead.is_set():
                try:
                    chunk = up.recv(16384)
                except TimeoutError:
                    continue
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    line, buf = buf.split(b"\n", 1)
                    if line:
                        try:
                            ws_send_frame(client_sock, 1, line + b"\n")
                        except Exception:
                            dead.set()
                            return
        except Exception:
            pass
        dead.set()
        try:
            client_sock.shutdown(socket.SHUT_RDWR)
        except Exception:
            pass

    t = threading.Thread(target=up_to_client, daemon=True)
    t.start()
    client_to_up()
    t.join(2)
    try:
        up.close()
    except Exception:
        pass

class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return

    def send_json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def send_file(self, path, ctype):
        data = Path(path).read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        u = urlparse(self.path)
        if u.path != "/api/browser-stat":
            self.send_json({"error": "not found"}, 404)
            return
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            n = 0
        if n <= 0 or n > 4096:
            self.send_json({"error": "bad length"}, 400)
            return
        try:
            body = json.loads(self.rfile.read(n).decode("utf-8", "replace") or "{}")
        except Exception:
            self.send_json({"error": "bad json"}, 400)
            return
        user = (body.get("user") or "").strip()
        if not user:
            addr = (body.get("address") or "").strip()
            worker = (body.get("worker") or "").strip()
            user = f"{addr}.{worker}" if worker else addr
        if not record_browser_stat(user, body.get("hs")):
            self.send_json({"error": "bad stat"}, 400)
            return
        self.send_json({"ok": True})

    def do_GET(self):
        u = urlparse(self.path)
        path = unquote(u.path)
        if path == "/api/pool":
            self.send_json(pool_payload())
            return
        if path == "/api/miners":
            online = online_miners()
            seen = db("SELECT * FROM miners ORDER BY last_ts DESC LIMIT 200")
            seen_out = []
            for r in seen or []:
                d = dict(r)
                life_a, life_r, sess = address_share_totals(d.get("address") or "")
                d["shares_lifetime"] = life_a or int(d.get("shares_lifetime") or d.get("shares_acc") or 0)
                d["shares_session"] = int(sess or d.get("shares_session") or 0)
                d["shares_acc"] = d["shares_lifetime"]
                d["shares_rej"] = life_r or int(d.get("shares_rej") or 0)
                info = prime_info_for(d.get("address") or "")
                d["window_work"] = int(info.get("window_work") or 0)
                d["window_percent"] = float(info.get("window_percent") or 0)
                d["via"] = "gateway" if (d.get("address") in (state.get("prime") or {}) and not any(o.get("address")==d.get("address") and o.get("via") in ("stratum","gpu") for o in online)) else d.get("via")
                seen_out.append(d)
            self.send_json({"online": online, "seen": seen_out})
            return
        if path.startswith("/api/miner/"):
            addr = path.split("/api/miner/", 1)[1].strip("/")
            self.send_json(miner_payload(addr))
            return
        if path == "/api/blocks":
            self.send_json({"blocks": mempool_blocks()})
            return
        if path == "/api/coinbaser":
            qs = parse_qs(u.query)
            val = 0
            try:
                val = int((qs.get("value") or ["0"])[0])
            except Exception:
                val = 0
            if val <= 0:
                val = 312500000
            blob, outs = coinbaser_blob(val)
            miner_sats = sum(o["sats"] for o in outs)
            self.send_json({
                "hex": blob.hex(),
                "outputs": len(outs),
                "value": val,
                "miner_sats": miner_sats,
                "fee_sats": max(0, val - miner_sats),
                "fee_percent": POOL_FEE,
                "scheme": "PROP",
                "miners": [{"address": o["address"], "sats": o["sats"]} for o in outs],
            })
            return
        if path == "/api/payouts":
            rows = db(
                "SELECT r.id, r.height, r.hash, r.closed_ts AS ts, r.reward_btc, r.fee_btc, r.miner_btc, r.total_work, r.status, "
                "p.address AS finder, p.amount_btc AS miner_paid, p.share, p.work "
                "FROM rounds r LEFT JOIN round_payouts p ON p.round_id=r.id "
                "WHERE r.status!='open' ORDER BY r.height DESC LIMIT 200"
            )
            current = db("SELECT address, work FROM round_work ORDER BY work DESC")
            tw = sum(float(r["work"]) for r in current) if current else 0.0
            self.send_json(
                {
                    "scheme": "PROP",
                    "fee_percent": POOL_FEE,
                    "maturity_blocks": 100,
                    "current_round": [
                        {"address": r["address"], "work": r["work"], "share": (float(r["work"]) / tw) if tw else 0}
                        for r in (current or [])
                    ],
                    "payouts": [
                        {
                            "height": r["height"],
                            "hash": r["hash"],
                            "ts": r["ts"],
                            "finder": r["finder"],
                            "miner_btc": r["miner_paid"],
                            "pool_fee_btc": r["fee_btc"],
                            "share": r["share"],
                            "status": r["status"],
                            "reward_btc": r["reward_btc"],
                        }
                        for r in rows
                    ],
                }
            )
            return
        if path in ("/mine", "/stratum"):
            self.handle_stratum_ws()
            return
        if path in ("/", "/index.html"):
            self.send_file(STATIC / "index.html", "text/html; charset=utf-8")
            return
        if path.startswith("/static/"):
            fp = STATIC / path[len("/static/") :]
            if fp.resolve().is_relative_to(STATIC.resolve()) and fp.is_file():
                mime = {
                    ".css": "text/css",
                    ".js": "application/javascript",
                    ".wasm": "application/wasm",
                    ".html": "text/html; charset=utf-8",
                }
                self.send_file(fp, mime.get(fp.suffix, "application/octet-stream"))
                return
        self.send_json({"error": "not found"}, 404)

    def handle_stratum_ws(self):
        key = self.headers.get("Sec-WebSocket-Key")
        if not key:
            self.send_json({"error": "websocket required"}, 400)
            return
        accept = ws_accept_key(key)
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        try:
            bridge_stratum(self.connection)
        except Exception as e:
            print("ws-bridge", e, flush=True)


def main():
    ensure_open_round()
    init_share_accounting()
    threading.Thread(target=loop, daemon=True).start()
    host = CONF.get("listen_host", "0.0.0.0")
    port = int(CONF.get("listen_port", 8888))
    print(f"lazarus-pool http://{host}:{port}", flush=True)
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
