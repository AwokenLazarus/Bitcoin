#!/usr/bin/env python3
"""Lazarus public mining-pool dashboard. Scrapes DATUM + Knots; no admin UI exposed."""
from __future__ import annotations

import json
import os
import re
import sqlite3
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from collections import defaultdict, deque
from urllib.parse import parse_qs, unquote, urlparse

ROOT = Path(__file__).resolve().parent
DB = Path(os.environ.get("POOL_DB") or (ROOT / "pool.sqlite"))
STATIC = ROOT / "static"
CONF = json.loads((ROOT / "config.json").read_text())
NO_WRITE = os.environ.get("POOL_UI_NO_WRITE") == "1"

POOL_FEE = float(CONF.get("pool_fee_percent", 0))
# Public-stratum fee when primed is not answering; primed's stats.json is authoritative.
STRATUM_FEE = float(CONF.get("stratum_fee_percent", 2.5))
STRATUM_HOST = CONF.get("stratum_host", "27.69.0.25")
STRATUM_PORT = int(CONF.get("stratum_port", 23334))
DATUM_URL = CONF.get("datum_url", "http://127.0.0.1:7152")
DATUM_CLIENT_URLS = [(DATUM_URL, "stratum", STRATUM_PORT)]
MEMPOOL_API = CONF.get("mempool_api", "http://10.21.21.27:8999")
COOKIE = Path(CONF.get("cookie_file", "/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie"))
AUTH_FILE = Path(CONF.get("datum_auth_file", "/home/umbrel/blake2b/secrets/datum-admin.env"))
EXPLORER = CONF.get("explorer_url", "https://mempool.awokenlazarus.xyz")
COINBASE_TAG = CONF.get("coinbase_tag", "Lazarus")
SUBSIDY = 3.125

PRIME_STATS = CONF.get("datum_prime_stats", "http://127.0.0.1:28916/stats.json")

# primed's stats.json, fetched at most every few seconds and kept as the last good copy.
# Everything the UI says about the Prime -- window, per-miner hashrate, gateways, blocks,
# owed, uptime, pubkey -- reads from this one document.
_prime_doc_cache = {"doc": {}, "ts": 0.0, "ok_ts": 0.0}


def prime_doc(max_age=4.0):
    now = time.time()
    if now - _prime_doc_cache["ts"] < max_age:
        return _prime_doc_cache["doc"]
    _prime_doc_cache["ts"] = now
    raw = curl(PRIME_STATS, timeout=3)
    try:
        doc = json.loads(raw) if raw else {}
    except Exception:
        doc = {}
    if isinstance(doc, dict) and doc.get("window") is not None:
        _prime_doc_cache["doc"] = doc
        _prime_doc_cache["ok_ts"] = now
    return _prime_doc_cache["doc"]


def prime_reachable(stale_after=30.0):
    prime_doc()
    return bool(_prime_doc_cache["doc"]) and (time.time() - _prime_doc_cache["ok_ts"]) < stale_after


def _datum_prime_pubkey():
    return ((prime_doc().get("pool") or {}).get("pubkey")) or ""


lock = threading.Lock()


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
    """Per-identity view of the TIDES window plus the Prime-wide figures, from primed's stats."""
    data = prime_doc()
    win = data.get("window") or {}
    pool = data.get("pool") or {}
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
            "credits": int(m.get("credits") or 0),
            # primed measures these itself from the credit stream; no need to estimate.
            "hr_ghs": float(m.get("hashrate_ghs") or 0),
            "last_share_s": float(m["last_share_s"]) if m.get("last_share_s") is not None else None,
            # Which fee schedule this identity's work is under: "datum" (own gateway) or
            # "stratum" (our public gateway). Mixed work is reported by primed as the
            # path that holds the majority; stratum_work is the public-stratum part.
            "fee_path": str(m.get("fee_path") or "").lower(),
            "stratum_work": int(float(m.get("stratum_work") or 0)),
        }
        if not by[ident]["fee_path"]:
            by[ident]["fee_path"] = "stratum" if by[ident]["stratum_work"] * 2 > work else "datum"
    try:
        stratum_fee_bps = int(pool.get("stratum_fee_bps") or pool.get("fee_bps") or 0)
    except (TypeError, ValueError):
        stratum_fee_bps = 0
    meta = {
        "stratum_fee_bps": stratum_fee_bps,
        "shares": int(win.get("shares") or 0),
        "work": 0,
        "target_work": 0,
        "window_multiple": 8,
        # window.identities in stats.json is the interned identity table (all-time); the
        # miners list is who holds work in the window right now.
        "identities": len(by),
        "identities_lifetime": int(win.get("identities") or len(by)),
        "fill_percent": float(win.get("fill_percent") or 0),
        "sample_value": int(win.get("sample_value") or 0),
        "sample_fee_sats": int(win.get("sample_fee_sats") or 0),
        "sample_pool_sats": int(win.get("sample_pool_sats") or 0),
        "hashrate_ghs": float((data.get("hashrate") or {}).get("pool_ghs") or 0),
        "hashrate_window_s": int((data.get("hashrate") or {}).get("window_s") or 0),
        "uptime_s": int(data.get("uptime_s") or 0),
        "started_ts": int(data.get("started_ts") or 0),
        "build": data.get("build") or {},
        "pool": pool,
        "node": data.get("node") or {},
        "totals": data.get("totals") or {},
        "clients": data.get("clients") or [],
        "blocks": data.get("blocks") or [],
        "owed_sats": int(data.get("owed") or 0),
        "reachable": prime_reachable(),
    }
    try:
        meta["work"] = int(float(win.get("work") or 0))
        meta["target_work"] = int(float(win.get("target_work") or 0))
    except (TypeError, ValueError):
        pass
    try:
        meta["window_multiple"] = int(pool.get("window_multiple") or 8)
    except (TypeError, ValueError):
        meta["window_multiple"] = 8
    return by, meta


def tides_window_snapshot():
    """Live TIDES window: N × network difficulty of accepted work, not today's hashrate."""
    meta = state.get("prime_meta") or {}
    work = float(meta.get("work") or 0)
    target = float(meta.get("target_work") or 0)
    try:
        multiple = int(meta.get("window_multiple") or 8)
    except (TypeError, ValueError):
        multiple = 8
    fill = (100.0 * work / target) if target > 0 else 0.0
    return {
        "window_multiple": multiple,
        "window_work": int(work),
        "window_target_work": int(target),
        "window_fill_percent": fill,
        "window_shares": int(meta.get("shares") or 0),
    }


# One unit of Prime window work is one difficulty-1 share (2**32 hashes).
_PRIME_HASHES_PER_WORK = float(1 << 32)
_PRIME_HR_CAP_GHS = 2000000.0


# Displayed hashrate uses a monotonic credit counter, not raw window_work.
# Once the TIDES window is full, Prime trims old rows: window_work can sit flat
# or fall between polls even while the miner is still submitting at full speed.
# We only ever *add* on positive deltas; a trim just updates last_work.
_HR_AVG_S = 300
_hr_hist = defaultdict(deque)
_hr_last_work = {}

_GW_HR_AVG_S = 300
_gw_diff_hist = defaultdict(deque)
_gw_last_diff = {}


def _rolling_credit_hr(hist, last_map, addr, value, ts, avg_s):
    prev = last_map.get(addr)
    total = hist[addr][-1][1] if hist[addr] else 0.0
    last_inc = ts
    if prev is None:
        last_map[addr] = value
    else:
        if value > prev:
            total += value - prev
            last_inc = ts
        last_map[addr] = value
    q = hist[addr]
    q.append((float(ts), float(total)))
    cutoff = ts - avg_s
    while len(q) > 1 and q[0][0] < cutoff:
        q.popleft()
    if len(q) < 2:
        est = 0.0
    else:
        dc = q[-1][1] - q[0][1]
        dt = max(1.0, q[-1][0] - q[0][0])
        if dt < 15 or dc < 0.5:
            est = 0.0
        else:
            est = dc * _PRIME_HASHES_PER_WORK / dt / 1e9
            if est > _PRIME_HR_CAP_GHS:
                est = _PRIME_HR_CAP_GHS
    last_share_s = float(max(0.0, ts - last_inc))
    return est, last_share_s


# --- Authoritative per-identity hashrate, straight from Prime's credited work. ---
# Prime's ledger records each credited share in 60s buckets of difficulty-1 work. Delivered
# hashrate is simply that work over a rolling wall-clock window times 2**32. This is the payout
# source of truth, is stateless, and (unlike window_work deltas or gateway diff sums) does not
# under-count busy or reconnecting miners. Cached briefly so a request storm does not reparse.
LEDGER_PATH = Path(CONF.get("ledger_path", "/home/umbrel/blake2b/lazarus-prime/ledger.json"))
LEDGER_HR_WINDOW_S = int(CONF.get("ledger_hr_window_s", 600))
_ledger_hr_cache = {"ts": 0.0, "by_addr": {}, "pool_ghs": 0.0, "age": {}}


def _ledger_hashrate(window_s=None):
    """(by_addr_ghs, pool_ghs): delivered hashrate per identity and for the pool.

    primed measures these from its credit stream and reports them in stats.json
    (``window.miners[].hashrate_ghs``, ``hashrate.pool_ghs``); that is the source. The
    old Prime's ``ledger.json`` walk is kept only as a fallback while stats are unreachable.
    """
    now = time.time()
    if now - _ledger_hr_cache["ts"] < 5 and _ledger_hr_cache["by_addr"]:
        return _ledger_hr_cache["by_addr"], _ledger_hr_cache["pool_ghs"]
    doc = prime_doc()
    win = doc.get("window") or {}
    if win.get("miners") is not None and prime_reachable():
        by, age = {}, {}
        for m in win.get("miners") or []:
            ident = (m.get("identity") or "").strip()
            if not ident:
                continue
            by[ident] = min(float(m.get("hashrate_ghs") or 0), _PRIME_HR_CAP_GHS)
            if m.get("last_share_s") is not None:
                age[ident] = float(m["last_share_s"])
        pool = float((doc.get("hashrate") or {}).get("pool_ghs") or sum(by.values()))
        _ledger_hr_cache.update({"ts": now, "by_addr": by, "pool_ghs": pool, "age": age})
        return by, pool
    window_s = int(window_s or LEDGER_HR_WINDOW_S)
    try:
        with open(LEDGER_PATH) as f:
            credits = json.load(f).get("credits", [])
    except Exception:
        return _ledger_hr_cache["by_addr"], _ledger_hr_cache["pool_ghs"]
    by, age, pool = {}, {}, 0.0
    if credits:
        newest = max(c["ts"] for c in credits)
        cut = newest - window_s
        work, last = {}, {}
        for c in credits:
            ident = c["identity"]
            if c["ts"] > cut:
                work[ident] = work.get(ident, 0) + c["work"]
            if c["ts"] > last.get(ident, 0):
                last[ident] = c["ts"]
        for ident, w in work.items():
            ghs = min(w * _PRIME_HASHES_PER_WORK / float(window_s) / 1e9, _PRIME_HR_CAP_GHS)
            by[ident] = ghs
            pool += ghs
        for ident, ts_last in last.items():
            age[ident] = float(max(0, newest - ts_last))
    _ledger_hr_cache.update({"ts": now, "by_addr": by, "pool_ghs": pool, "age": age})
    return by, pool


def _ledger_last_share_s(addr):
    _ledger_hashrate()
    return _ledger_hr_cache["age"].get(addr)


def _prime_hr_from_work(addr, work, ts):
    by, _pool = _ledger_hashrate()
    est = float(by.get(addr) or 0.0)
    last = _ledger_last_share_s(addr)
    last_share_s = last if last is not None else 0.0
    return est, last_share_s, work, ts


def _gateway_hr_from_diff(addr, diff_total, ts):
    return _rolling_credit_hr(_gw_diff_hist, _gw_last_diff, addr, diff_total, ts, _GW_HR_AVG_S)[0]


def _update_gateway_hr(miners, ts):
    by = defaultdict(int)
    for m in miners or []:
        if (m.get("via") or "") != "stratum":
            continue
        addr = m.get("address") or ""
        if addr:
            by[addr] += int(m.get("diff_acc") or 0)
    rates = {}
    for addr, diff in by.items():
        rates[addr] = _gateway_hr_from_diff(addr, diff, ts)
    state["gateway_hr"] = rates
    return rates


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
    live_ids = set(by)
    for row in db("SELECT address FROM prime_miners") or []:
        addr = row["address"]
        if addr in live_ids:
            continue
        db(
            "UPDATE prime_miners SET share_percent=0, hr_ghs_est=0 WHERE address=?",
            (addr,),
            write=True,
        )


def _fee_percent_for_path(fee_path):
    """Fee rate (percent) primed applies to work that arrived on `fee_path`."""
    pool = prime_doc().get("pool") or {}
    stratum = str(fee_path or "").lower() == "stratum"
    try:
        if stratum:
            return int(pool.get("stratum_fee_bps") or pool.get("fee_bps") or round(STRATUM_FEE * 100)) / 100.0
        return int(pool.get("fee_bps") or round(POOL_FEE * 100)) / 100.0
    except (TypeError, ValueError):
        return STRATUM_FEE if stratum else POOL_FEE


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
    # Not in the live TIDES window — leftover sqlite work/percent is yesterday's credit,
    # not what a block found now would pay. Do not show it as current attribution.
    row = db("SELECT peak_work, last_ts, hr_ghs_est, work_seen_ts FROM prime_miners WHERE address=?", (address,), one=True)
    if not row:
        return {}
    seen = int(row["work_seen_ts"] or 0) if row["work_seen_ts"] else 0
    age = (int(time.time()) - seen) if seen else 10**9
    hr = float(row["hr_ghs_est"] or 0)
    if age >= 600:
        hr = 0.0
    elif hr > 1e-6:
        hr = hr * max(0.0, 1.0 - age / 600.0)
    return {
        "window_work": 0,
        "window_percent": 0.0,
        "window_sats": 0,
        "payable": False,
        "window_peak": int(row["peak_work"] or 0),
        "window_last_ts": int(row["last_ts"] or 0),
        "hr_ghs": hr,
        "last_share_s": float(age) if seen else 0.0,
    }


def _share_age_s(value, *, missing=1e9):
    """Seconds since last share. 0 means just now — do not treat it as missing."""
    if value is None:
        return missing
    try:
        return float(value)
    except (TypeError, ValueError):
        return missing


def _prime_is_live(info, window_s=180):
    info = info or {}
    hr = float(info.get("hr_ghs") or 0)
    age = _share_age_s(info.get("last_share_s"), missing=0.0 if hr > 1e-6 else 1e9)
    return hr > 1e-6 and age < window_s


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
    rec["fee_path"] = info.get("fee_path") or ""
    rec["via"] = rec.get("via") or ("prime" if rec.get("ua") in ("DATUM gateway", "Prime window") else "stratum")
    _led_by, _ = _ledger_hashrate()
    phr = float(_led_by.get(addr) or 0.0) or float(info.get("hr_ghs") or 0)
    gwh = float((state.get("gateway_hr") or {}).get(addr) or 0)
    rec["credited_hr_ghs"] = phr
    rec["gateway_hr_ghs"] = gwh
    # The session's own reported rate. Records are re-attached on every request, and hr_ghs
    # below becomes the address-level credited rate, so keep the first (true) value.
    if "firmware_hr_ghs" not in rec:
        rec["firmware_hr_ghs"] = float(rec.get("hr_ghs") or 0)
    if rec.get("via") in ("gateway", "prime") or rec.get("ua") in ("DATUM gateway", "Prime window"):
        ww = int(info.get("window_work") or rec.get("window_work") or 0)
        if ww and int(rec.get("shares_lifetime") or 0) < ww:
            rec["shares_lifetime"] = ww
            rec["shares_acc"] = ww
    display = phr if phr > 1e-6 else (gwh if gwh > 1e-6 else rec["firmware_hr_ghs"])
    if display > 1e-6:
        rec["hr_ghs"] = display
        if gwh > 1e-6 and rec.get("last_share_s") is not None:
            pass
        elif info.get("last_share_s") is not None:
            rec["last_share_s"] = _share_age_s(info.get("last_share_s"), missing=0.0)
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
            if m.get("ua") in ("DATUM gateway", "Prime window") or m.get("via") in ("gateway", "prime"):
                if m.get("via") != "stratum":
                    m["via"] = "prime"
            elif m.get("via") == "stratum":
                m["via"] = "stratum"
            elif m.get("host") or (m.get("ua") and m.get("ua") != "DATUM gateway"):
                m["via"] = "stratum"
        attach_share_fields(m)
    extras = []
    for addr, info in by.items():
        if addr in have:
            continue
        last_s = _share_age_s(info.get("last_share_s"), missing=0.0)
        ww = int(info.get("window_work") or 0)
        rec = {
            "address": addr,
            "worker": "window",
            "user": addr,
            "host": "",
            "hr_ghs": float(info.get("hr_ghs") or 0),
            "vdiff": 0,
            "diff_acc": ww,
            "shares_acc": ww,
            "shares_session": 0,
            "shares_lifetime": ww,
            "diff_rej": 0,
            "shares_rej": 0,
            "last_share_s": last_s,
            "ua": "Prime window",
            "online": _prime_is_live(info),
            "via": "prime",
            "window_work": ww,
            "window_percent": float(info.get("window_percent") or 0),
            "window_sats": int(info.get("window_sats") or 0),
        }
        attach_share_fields(rec)
        rec["online"] = rec.get("online") or _prime_is_live(rec)
        if rec.get("online"):
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
    if write and NO_WRITE:
        return None
    with lock:
        cur = db_conn.execute(q, args)
        if write:
            db_conn.commit()
            return cur.lastrowid
        rows = cur.fetchall()
        return rows[0] if one and rows else (rows if not one else None)


def curl(url, digest=False, timeout=3):
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


def online_miners():
    return merge_prime_online(list(state.get("miners") or []))


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
        try:
            phr, a, r = _scrape_datum_home(url)
            pool_hr += phr
            acc += a
            rej += r
            for rec in _scrape_datum_clients(url, via, port):
                key = (rec.get("address"), rec.get("worker"), rec.get("via"), rec.get("host"))
                if key in seen:
                    continue
                seen.add(key)
                miners.append(rec)
        except Exception as e:
            print("scrape", url, e, flush=True)
    ts = int(time.time())
    gateway_hr = _update_gateway_hr(miners, ts)
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
    state["prime"] = prime_by
    state["prime_meta"] = prime_meta
    merged = merge_prime_online(list(miners))
    _lby, _lpool = _ledger_hashrate()
    from_clients = sum(float(m.get("hr_ghs") or 0) for m in merged)
    live_hr = _lpool if _lpool > 1e-9 else (from_clients or pool_hr)
    n_miners = len(merged)
    if live_hr > 0:
        db(
            "INSERT OR REPLACE INTO pool_samples(ts,hr_ghs,miners,shares_acc,shares_rej) VALUES(?,?,?,?,?)",
            (ts, live_hr, n_miners, acc, rej),
            write=True,
        )
    return {"pool_hr_ghs": live_hr, "shares_acc": acc, "shares_rej": rej, "miners": miners, "ts": ts, "prime": prime_by, "prime_meta": prime_meta, "gateway_hr": gateway_hr}


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


def value_output_count(vouts):
    n = 0
    for v in vouts or []:
        if float(v.get("value") or 0) > 0:
            n += 1
    return n


def vout_address(v):
    spk = (v or {}).get("scriptPubKey") or {}
    return spk.get("address") or ((spk.get("addresses") or [None])[0])


def splits_from_vouts(vouts):
    by = {}
    for v in vouts or []:
        amt = float(v.get("value") or 0)
        addr = vout_address(v)
        if not addr or amt <= 0:
            continue
        by[addr] = by.get(addr, 0.0) + amt
    return by


# TIDES coinbase splits by block hash. None = RPC miss (do not cache).
_cb_split_cache = {}


def pool_output_parts(pool_addr, on_chain_btc, pb, reward_btc=None):
    """Split a pool-address coinbase output into miner TIDES share vs fee.

    Anyone who mines to the pool wallet (the S11 does) lands in the same
    script as the 0.5% fee. On-chain those are one output; the issued
    split still knows the miner share, so the fee is the remainder.
    """
    total = float(on_chain_btc or 0)
    if not pool_addr:
        return 0.0, total
    miner_sats = 0
    for item in (pb or {}).get("split") or []:
        if isinstance(item, dict):
            addr, sats = item.get("address"), item.get("sats")
        elif isinstance(item, (list, tuple)) and len(item) >= 2:
            addr, sats = item[0], item[1]
        else:
            continue
        if addr == pool_addr:
            miner_sats += int(sats or 0)
    if miner_sats:
        miner_btc = min(miner_sats / 1e8, total)
        return miner_btc, max(0.0, total - miner_btc)
    if reward_btc:
        fee = min(total, float(reward_btc) * (POOL_FEE / 100.0))
        return max(0.0, total - fee), fee
    return 0.0, total


def coinbase_splits(blockhash):
    """Address -> BTC actually paid in that block's coinbase."""
    if not blockhash:
        return None
    if blockhash in _cb_split_cache:
        return _cb_split_cache[blockhash]
    blk = rpc("getblock", [blockhash, 2])
    if not blk:
        return None
    tx0 = (blk.get("tx") or [None])[0] or {}
    by = splits_from_vouts(tx0.get("vout"))
    _cb_split_cache[blockhash] = by
    return by


def payout_status_for_height(height, tip):
    if not height or not tip:
        return "paid"
    if int(tip) < int(height) + 100:
        return "immature"
    return "paid"


def restore_unsplit_effort():
    """If a Lazarus block paid only the pool address, put that round's work back."""
    db(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT)",
        write=True,
    )
    if db("SELECT value FROM meta WHERE key='unsplit_restored'", one=True):
        return
    fb = db("SELECT height, hash FROM found_blocks ORDER BY height DESC", one=True)
    if not fb or not fb["hash"]:
        return
    blk = rpc("getblock", [fb["hash"], 2]) or {}
    tx0 = (blk.get("tx") or [None])[0] or {}
    if value_output_count(tx0.get("vout")) >= 2:
        db("INSERT OR REPLACE INTO meta(key,value) VALUES('unsplit_restored',?)", ("skip",), write=True)
        return
    rnd = db("SELECT id FROM rounds WHERE height=?", (fb["height"],), one=True)
    if not rnd:
        return
    rid = int(rnd["id"])
    rows = db("SELECT address, work FROM round_payouts WHERE round_id=?", (rid,)) or []
    ensure_open_round()
    restored = 0
    for r in rows:
        addr, work = r["address"], float(r["work"] or 0)
        if not addr or work <= 0:
            continue
        existing = db("SELECT work FROM round_work WHERE address=?", (addr,), one=True)
        if existing:
            db("UPDATE round_work SET work=work+? WHERE address=?", (work, addr), write=True)
        else:
            db("INSERT INTO round_work(address,work,last_diff_acc) VALUES(?,?,0)", (addr, work), write=True)
        restored += 1
    db("UPDATE rounds SET status='unsplit' WHERE id=?", (rid,), write=True)
    db("UPDATE round_payouts SET status='carried' WHERE round_id=?", (rid,), write=True)
    db("INSERT OR REPLACE INTO meta(key,value) VALUES('unsplit_restored',?)", (str(fb["height"]),), write=True)
    print("restored_unsplit", fb["height"], "identities", restored, flush=True)


def close_round_for_block(height, blockhash, reward, fee_btc, miner_btc, vouts=None):
    ensure_open_round()
    already = db("SELECT id FROM rounds WHERE height=?", (height,), one=True)
    if already:
        return
    openr = db("SELECT id FROM rounds WHERE status='open' ORDER BY id DESC", one=True)
    rid = int(openr["id"])
    rows = db("SELECT address, work FROM round_work WHERE work > 0")
    total = sum(float(r["work"]) for r in rows) if rows else 0.0
    work_by = {r["address"]: float(r["work"]) for r in (rows or [])}
    db(
        "UPDATE rounds SET closed_ts=?, height=?, hash=?, reward_btc=?, fee_btc=?, miner_btc=?, total_work=?, status='immature' WHERE id=?",
        (int(time.time()), height, blockhash, reward, fee_btc, miner_btc, total, rid),
        write=True,
    )
    # Pay what the coinbase actually paid (TIDES). sqlite round_work is only a UI estimate.
    paid = splits_from_vouts(vouts) if vouts is not None else None
    if paid is None:
        paid = coinbase_splits(blockhash) or {}
    if paid:
        reward_split = sum(paid.values()) or 1.0
        for addr, amt in paid.items():
            share = amt / reward_split
            db(
                "INSERT OR REPLACE INTO round_payouts(round_id,address,work,share,amount_btc,status) VALUES(?,?,?,?,?,?)",
                (rid, addr, work_by.get(addr, 0.0), share, amt, "immature"),
                write=True,
            )
    elif total > 0:
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
            # Coinbase payout: already in that block. After 100 confs it is paid, not a balance we owe.
            db(
                "UPDATE round_payouts SET status='paid' WHERE round_id=? AND status='immature'",
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
        split = value_output_count(vouts) >= 2
        if not existed and split:
            close_round_for_block(height, h, reward, fee_btc, miner_btc, vouts)
        elif not split:
            print("unsplit_template", height, "keeping_round_work", flush=True)
    if last_ok >= start:
        db("INSERT OR REPLACE INTO meta(key,value) VALUES('scan_height',?)", (str(last_ok),), write=True)


state = {"pool_hr_ghs": 0, "shares_acc": 0, "shares_rej": 0, "miners": [], "ts": 0, "prime": {}, "prime_meta": {}}


def loop():
    global state
    while True:
        t0 = time.time()
        try:
            state = scrape()
        except Exception as e:
            print("scrape", e, flush=True)
        try:
            restore_unsplit_effort()
            scan_found_blocks()
            mature_rounds()
        except Exception as e:
            print("scan", e, flush=True)
        time.sleep(max(0.5, 10 - (time.time() - t0)))


def node_info():
    mi = rpc("getmininginfo") or {}
    bi = rpc("getblockchaininfo") or {}
    return {
        "height": mi.get("blocks") or bi.get("blocks"),
        "difficulty": mi.get("difficulty"),
        "networkhashps": mi.get("networkhashps"),
        "chain": bi.get("chain"),
    }


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


OWN_GATEWAY_UA_PREFIX = "lazarus-gateway/"


def _gateway_row(c):
    ua = str(c.get("user_agent") or "")
    return {
        "id": c.get("id"),
        "gateway": c.get("gateway"),
        "user_agent": ua,
        "generation": c.get("generation"),
        # The pool's own public stratum connects to Prime like anyone else's gateway.
        "own": ua.startswith(OWN_GATEWAY_UA_PREFIX),
        "fee_path": str(c.get("fee_path") or "").lower(),
        "identity": c.get("identity") or "",
        "connected_s": int(c.get("connected_s") or 0),
        "accepted": int(c.get("accepted") or 0),
        "rejected": int(c.get("rejected") or 0),
        "last_reject": c.get("last_reject") or "",
        "last_share_s": c.get("last_share_s"),
        "work": int(c.get("work") or 0),
        "coinbasers": int(c.get("coinbasers") or 0),
        "block_candidates": int(c.get("block_candidates") or 0),
    }


def _block_row(b):
    """One Prime block record for the UI: what the coinbase did and where the block stands."""
    split = b.get("split") or []
    submit = str(b.get("submit") or "pending")
    kind = str(b.get("kind") or "")
    # primed marks orphans on `kind` ("orphan:split"); `submit` is the node's submitblock verdict.
    if kind.startswith("orphan"):
        status = "orphaned"
    elif b.get("settled") is True:
        status = "in chain"
    elif submit in ("accepted", "duplicate"):
        status = "submitted"
    elif submit == "inconclusive":
        # valid block, but a competing tip: the node accepted it without making it the best chain.
        # It confirms once a block lands on top of it (a lagging gateway node), or ends up orphaned.
        status = "pending"
    elif submit.startswith("rejected"):
        status = "rejected"
    else:
        status = "pending"
    return {
        "height": b.get("height"),
        "hash": b.get("hash"),
        "ts": b.get("ts"),
        "kind": b.get("kind") or "",
        "submit": submit,
        "status": status,
        "finder": b.get("finder") or "",
        "gateway": b.get("gateway") or "",
        "coinbase_value": int(b.get("coinbase_value") or 0),
        "pool_sats": int(b.get("pool_sats") or 0),
        "owed_sats": int(b.get("owed_sats") or 0),
        "outputs": len(split),
        "split": [{"address": a, "sats": int(s)} for a, s in split if a],
    }


def prime_summary():
    """The Prime as the UI shows it: identity, health, gateways, window, blocks, owed."""
    _by, meta = fetch_prime_window()
    pool = meta.get("pool") or {}
    totals = meta.get("totals") or {}
    clients = [_gateway_row(c) for c in meta.get("clients") or []]
    clients.sort(key=lambda g: (g["own"], -g["work"]))
    blocks = [_block_row(b) for b in meta.get("blocks") or []]
    blocks.sort(key=lambda b: -(b["height"] or 0))
    pool_addr = pool.get("address") or ""
    for b in blocks:
        miner_to_pool = sum(int(o.get("sats") or 0) for o in b["split"] if o.get("address") == pool_addr)
        b["miner_to_pool_sats"] = miner_to_pool
        b["fee_sats"] = max(0, int(b.get("pool_sats") or 0) - miner_to_pool)
    try:
        fee_bps = int(pool.get("fee_bps") or round(POOL_FEE * 100))
    except (TypeError, ValueError):
        fee_bps = int(round(POOL_FEE * 100))
    return {
        "reachable": bool(meta.get("reachable")),
        "name": (meta.get("build") or {}).get("name") or "primed",
        "version": (meta.get("build") or {}).get("version") or "",
        "uptime_s": meta.get("uptime_s") or 0,
        "started_ts": meta.get("started_ts") or 0,
        "pubkey": pool.get("pubkey") or "",
        "prime_id": pool.get("prime_id"),
        "tag": pool.get("tag") or COINBASE_TAG,
        "address": pool.get("address") or "",
        "fee_bps": fee_bps,
        "stratum_fee_bps": int(meta.get("stratum_fee_bps") or fee_bps),
        "min_payout_sats": int(pool.get("min_payout") or 0),
        "advertise": pool.get("advertise") or "",
        "hashrate_ghs": meta.get("hashrate_ghs") or 0,
        "hashrate_window_s": meta.get("hashrate_window_s") or 0,
        "node": meta.get("node") or {},
        "window": {
            "multiple": meta.get("window_multiple") or 8,
            "work": meta.get("work") or 0,
            "target_work": meta.get("target_work") or 0,
            "fill_percent": meta.get("fill_percent") or 0,
            "identities": meta.get("identities") or 0,
            "identities_lifetime": meta.get("identities_lifetime") or 0,
            "shares": meta.get("shares") or 0,
            "sample_value": meta.get("sample_value") or 0,
            "sample_fee_sats": meta.get("sample_fee_sats") or 0,
            "sample_pool_sats": meta.get("sample_pool_sats") or 0,
        },
        "totals": {
            "shares_accepted": int(totals.get("shares_accepted") or 0),
            "shares_rejected": int(totals.get("shares_rejected") or 0),
            "work_accepted": int(totals.get("work_accepted") or 0),
            "lifetime_shares": int(totals.get("lifetime_shares") or 0),
            "lifetime_work": int(totals.get("lifetime_work") or 0),
            "connections": int(totals.get("connections") or 0),
            "handshake_failures": int(totals.get("handshake_failures") or 0),
            "coinbasers": int(totals.get("coinbasers") or 0),
            "block_candidates": int(totals.get("block_candidates") or 0),
            "blocks_submitted": int(totals.get("blocks_submitted") or 0),
        },
        "owed_sats": meta.get("owed_sats") or 0,
        "gateways": clients,
        "gateways_online": len(clients),
        "gateways_remote": sum(1 for g in clients if not g["own"]),
        "blocks": blocks,
    }


def prime_coinbaser_preview():
    """The coinbase Prime would dictate for the next block: the TIDES split at the
    current reward, every miner output in issue order, the pool's fee/remainder last.
    Straight from primed's own split (``window.miners[].payout_sats``), not recomputed."""
    by, meta = fetch_prime_window()
    value = int(meta.get("sample_value") or 0)
    miners = []
    for addr, info in by.items():
        sats = int(info.get("window_sats") or 0)
        if sats > 0 and info.get("payable"):
            miners.append({
                "address": addr,
                "sats": sats,
                "share_percent": float(info.get("window_percent") or 0),
                "work": int(info.get("window_work") or 0),
                "fee_path": info.get("fee_path") or "",
                "hr_ghs": float(info.get("hr_ghs") or 0),
                "last_share_s": info.get("last_share_s"),
            })
    miners.sort(key=lambda m: -m["sats"])
    miner_sats = sum(m["sats"] for m in miners)
    unpaid = [
        {"address": addr, "work": int(info.get("window_work") or 0), "share_percent": float(info.get("window_percent") or 0), "reason": "below min payout" if info.get("payable") else "address not payable"}
        for addr, info in by.items()
        if int(info.get("window_sats") or 0) <= 0 and int(info.get("window_work") or 0) > 0
    ]
    pool_sats = int(meta.get("sample_pool_sats") or max(0, value - miner_sats))
    fee_sats = int(meta.get("sample_fee_sats") or 0)
    pool_addr = (meta.get("pool") or {}).get("address") or ""
    outputs = [dict(m, to="miner") for m in miners]
    if pool_sats > 0:
        outputs.append({"address": pool_addr, "sats": pool_sats, "to": "pool"})
    return {
        "scheme": "TIDES",
        "value": value,
        "outputs": len(outputs),
        "miner_outputs": len(miners),
        "miner_sats": miner_sats,
        "pool_sats": pool_sats,
        "fee_sats": fee_sats,
        "fee_percent": (meta.get("pool") or {}).get("fee_bps", int(round(POOL_FEE * 100))) / 100.0,
        "stratum_fee_percent": int(meta.get("stratum_fee_bps") or 0) / 100.0,
        # Effective fee on this split: fee_sats over the block value. Between the DATUM
        # and stratum rates, weighted by whose work fills the window.
        "effective_fee_percent": (100.0 * fee_sats / value) if value else 0.0,
        "unplaced_sats": max(0, pool_sats - fee_sats),
        "pool_address": pool_addr,
        "window_multiple": meta.get("window_multiple") or 8,
        "window_fill_percent": meta.get("fill_percent") or 0,
        "miners": outputs,
        "unpaid": unpaid,
    }


def pool_payload():
    node = node_info()
    miners = online_miners()
    seen_addr = set()
    pool_hr = 0.0
    for m in miners:
        a = m.get("address") or ""
        if a in seen_addr:
            continue
        seen_addr.add(a)
        credited = float(m.get("credited_hr_ghs") or 0)
        pool_hr += credited if credited > 1e-6 else float(m.get("hr_ghs") or 0)
    _lby, _lpool = _ledger_hashrate()
    if _lpool > 1e-9:
        pool_hr = _lpool
    elif pool_hr < 1e-9:
        pool_hr = state.get("pool_hr_ghs") or 0
    online = len(seen_addr)
    net = float(node.get("networkhashps") or 0)
    first = db("SELECT MIN(first_ts) AS t FROM miners", one=True)
    first_ts = first["t"] if first and first["t"] else state.get("ts")
    share, ttf_s, nfound, expected, luck = luck_and_ttf(pool_hr, net, first_ts)
    est_btc_day = share * 144 * SUBSIDY * (1 - POOL_FEE / 100.0)
    known = db("SELECT COUNT(*) AS n FROM miners", one=True)
    hist = db("SELECT ts, hr_ghs, miners FROM pool_samples WHERE ts > ? ORDER BY ts", (int(time.time()) - 86400,))
    win = tides_window_snapshot()
    prime = prime_summary()
    nblocks = int(win["window_multiple"] or 8)
    fill = win["window_fill_percent"]
    datum_fee = prime["fee_bps"] / 100.0 if prime.get("reachable") else POOL_FEE
    stratum_fee = prime["stratum_fee_bps"] / 100.0 if prime.get("reachable") else STRATUM_FEE
    if datum_fee == 0 and stratum_fee == 0:
        fee_clause = "100%, no fee"
    elif datum_fee == stratum_fee:
        fee_clause = f"{100-datum_fee:g}% to miners, {datum_fee:g}% fee"
    else:
        fee_clause = f"{datum_fee:g}% fee through your own DATUM gateway, {stratum_fee:g}% on the public stratum"
    payout = (
        f"A found block pays the TIDES window in its coinbase ({fee_clause}): "
        f"{nblocks} network-blocks of accepted work, currently {fill:.0f}% full. "
        f"Hashrate is not your cut — a new rig starts near 0% and ramps as its work enters and older work ages out."
    )
    return {
        "name": "Lazarus",
        "tagline": "Proverbs 11:1",
        "fee_percent": POOL_FEE,
        "fees": {
            "datum_percent": datum_fee,
            "stratum_percent": stratum_fee,
            "note": "The fee is taken per miner from that miner's window share, by the path the work arrived on. Switching paths keeps the accepted work.",
        },
        "stratum": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "stratum_asic": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "host": STRATUM_HOST,
        "port": STRATUM_PORT,
        "pool_hr_ghs": pool_hr,
        "miners_online": online,
        "workers_online": online,
        "miners_seen": int(known["n"]) if known else online,
        "shares_accepted": pool_share_totals()[0] or (state.get("shares_acc") or 0),
        "shares_session": state.get("shares_acc") or 0,
        "shares_rejected": pool_share_totals()[1] or (state.get("shares_rej") or 0),
        "shares_note": "Accepted stays with your address whether you mine on the public stratum or through your own DATUM gateway. Window % is what the next block pays, not today's hashrate.",
        "window_shares": win["window_shares"],
        "window_work": win["window_work"],
        "window_target_work": win["window_target_work"],
        "window_fill_percent": fill,
        "window_multiple": nblocks,
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
        "payout": payout,
        "payout_scheme": "TIDES",
        "datum": {
            "pool_host": CONF.get("datum_prime_host", STRATUM_HOST),
            "pool_port": int(CONF.get("datum_prime_port", 28915)),
            "pool_pubkey": _datum_prime_pubkey(),
            "pool_pass_workers": True,
            "pool_pass_full_users": True,
            "pooled_mining_only": True,
        },
        "prime": prime,
        "payouts_onchain": True,
        "explorer": EXPLORER,
        "updated": state.get("ts") or int(time.time()),
        "history": [{"ts": r["ts"], "hr_ghs": r["hr_ghs"], "miners": r["miners"]} for r in hist],
    }


def rollup_online_by_address(online):
    """One row per address on the miners list; detail page keeps all sessions."""
    order = []
    by = {}
    for m in online or []:
        addr = m.get("address") or ""
        if not addr:
            continue
        if addr not in by:
            by[addr] = dict(m)
            by[addr]["sessions"] = 1
            order.append(addr)
            continue
        cur = by[addr]
        cur["sessions"] = int(cur.get("sessions") or 1) + 1
        cur["diff_acc"] = int(cur.get("diff_acc") or 0) + int(m.get("diff_acc") or 0)
        cur["shares_acc"] = int(cur.get("shares_acc") or 0) + int(m.get("shares_acc") or 0)
        cur["shares_session"] = int(cur.get("shares_session") or 0) + int(m.get("shares_session") or 0)
        cur["shares_lifetime"] = int(cur.get("shares_lifetime") or 0) + int(m.get("shares_lifetime") or 0)
        cur["firmware_hr_ghs"] = float(cur.get("firmware_hr_ghs") or 0) + float(m.get("firmware_hr_ghs") or m.get("hr_ghs") or 0)
        gwh = float((state.get("gateway_hr") or {}).get(addr) or 0)
        phr = float(cur.get("credited_hr_ghs") or 0)
        cur["hr_ghs"] = phr if phr > 1e-6 else (gwh if gwh > 1e-6 else cur["firmware_hr_ghs"])
        cur["credited_hr_ghs"] = phr
        cur["gateway_hr_ghs"] = gwh
        # 0 means "just now" and must survive the merge; only None is missing.
        merged_age = min(_share_age_s(cur.get("last_share_s")), _share_age_s(m.get("last_share_s")))
        if merged_age < 1e9:
            cur["last_share_s"] = merged_age
        if (m.get("host") or "") and (cur.get("host") or "") != (m.get("host") or ""):
            cur["host"] = f"{cur.get('host') or ''}+{m.get('host')}"[:120]
    out = []
    for addr in order:
        rec = by[addr]
        if int(rec.get("sessions") or 1) > 1 and not rec.get("worker"):
            rec["worker"] = f"{rec['sessions']} sessions"
        out.append(rec)
    return out


def miner_payload(address):
    recs = [m for m in online_miners() if m["address"] == address]
    hist = db(
        "SELECT ts, SUM(hr_ghs) AS hr FROM samples WHERE address=? AND ts > ? GROUP BY ts ORDER BY ts",
        (address, int(time.time()) - 86400),
    )
    stored = db("SELECT * FROM miners WHERE address=?", (address,), one=True)
    pinfo = prime_info_for(address) if address else {}
    _led_by, _ = _ledger_hashrate()
    credited = float(_led_by.get(address) or 0.0) or float(pinfo.get("hr_ghs") or 0)
    gwh = float((state.get("gateway_hr") or {}).get(address) or 0)
    firmware = sum(float(m.get("firmware_hr_ghs") or m.get("hr_ghs") or 0) for m in recs)
    # Prefer gateway accepted-diff rate (all stratum sessions); Prime window_work
    # under-counts once the TIDES window is full and trim lands in the same poll.
    hr = credited if credited > 1e-6 else (gwh if gwh > 1e-6 else firmware)
    node = node_info()
    net_ghs = (float(node.get("networkhashps") or 0)) / 1e9
    share = (hr / net_ghs) if net_ghs else 0
    est = share * 144 * SUBSIDY * (1 - POOL_FEE / 100.0)
    pool_hr = 0.0
    _seen = set()
    for m in online_miners():
        a = m.get("address") or ""
        if a in _seen:
            continue
        _seen.add(a)
        g = float((state.get("gateway_hr") or {}).get(a) or 0)
        c = float(m.get("credited_hr_ghs") or 0)
        pool_hr += c if c > 1e-6 else (g if g > 1e-6 else float(m.get("hr_ghs") or 0))
    pool_hr = pool_hr or 1e-9
    contrib = hr / pool_hr if pool_hr else 0
    tip = rpc("getblockcount") or 0
    fb_rows = db("SELECT height, hash, ts FROM found_blocks ORDER BY height DESC LIMIT 50") or []
    payouts = []
    paid_btc = 0.0
    immature_btc = 0.0
    used_chain = False
    for fb in fb_rows:
        splits = coinbase_splits(fb["hash"])
        if splits is None:
            used_chain = False
            payouts = None
            break
        used_chain = True
        amt = float(splits.get(address) or 0)
        if amt <= 0:
            continue
        reward_split = sum(splits.values()) or 1.0
        # Mining to the pool wallet must not count the 0.5% fee as miner earnings.
        pool_addr = ((prime_doc().get("pool") or {}).get("address") or "")
        if pool_addr and address == pool_addr:
            pb = next((b for b in (prime_doc().get("blocks") or []) if b.get("hash") == fb["hash"]), None)
            amt, _fee = pool_output_parts(pool_addr, amt, pb, reward_split)
            if amt <= 0:
                continue
        st = payout_status_for_height(fb["height"], tip)
        payouts.append(
            {
                "height": fb["height"],
                "hash": fb["hash"],
                "ts": fb["ts"],
                "miner_btc": amt,
                "share": amt / reward_split,
                "work": 0,
                "status": st,
                "round_status": st,
            }
        )
        if st == "immature":
            immature_btc += amt
        else:
            paid_btc += amt
    if not used_chain:
        payouts = db(
            "SELECT r.height, r.hash, r.closed_ts AS ts, p.amount_btc AS miner_btc, p.share, p.work, p.status, r.status AS round_status "
            "FROM round_payouts p JOIN rounds r ON r.id=p.round_id WHERE p.address=? ORDER BY r.height DESC LIMIT 50",
            (address,),
        )
        earned = db(
            "SELECT COALESCE(SUM(amount_btc),0) AS s FROM round_payouts WHERE address=? AND status IN ('paid','unpaid')",
            (address,),
            one=True,
        )
        immature = db(
            "SELECT COALESCE(SUM(amount_btc),0) AS s FROM round_payouts WHERE address=? AND status='immature'",
            (address,),
            one=True,
        )
        paid_btc = float(earned["s"]) if earned else 0
        immature_btc = float(immature["s"]) if immature else 0
    rw = db("SELECT work FROM round_work WHERE address=?", (address,), one=True)
    tw = db("SELECT COALESCE(SUM(work),0) AS s FROM round_work", one=True)
    my_work = float(rw["work"]) if rw else 0.0
    tot_work = float(tw["s"]) if tw else 0.0
    round_share = (my_work / tot_work) if tot_work else 0.0
    ttf_s = (600.0 / share) if share else None
    known = bool(stored or recs)
    life_a, life_r, sess_stored = address_share_totals(address) if address else (0, 0, 0)
    sess_live = sum(int(m.get("shares_session") or 0) for m in recs if (m.get("via") or "stratum") not in ("gateway", "prime")) if recs else 0
    if pinfo.get("window_percent"):
        round_share = float(pinfo["window_percent"]) / 100.0
    vias = {m.get("via") for m in recs if m.get("via")}
    if "stratum" in vias and ("gateway" in vias or "prime" in vias):
        via = "both"
    elif ("gateway" in vias or "prime" in vias) and "stratum" not in vias:
        via = "prime"
    elif recs:
        via = "stratum" if "stratum" in vias else (next(iter(vias)) if vias else "stratum")
    elif pinfo.get("window_work"):
        via = "prime"
        recs = [{
            "address": address, "worker": "window", "hr_ghs": float(pinfo.get("hr_ghs") or 0),
            "shares_acc": life_a or int(pinfo.get("window_work") or 0),
            "shares_session": 0, "shares_lifetime": life_a or int(pinfo.get("window_work") or 0),
            "shares_rej": life_r,
            "vdiff": 0, "diff_acc": int(pinfo.get("window_work") or 0),
            "last_share_s": _share_age_s(pinfo.get("last_share_s"), missing=0.0),
            "ua": "Prime window", "via": "prime",
            "window_work": pinfo.get("window_work") or 0, "window_percent": pinfo.get("window_percent") or 0,
        }]
    else:
        via = ""
    known = bool(stored or recs or pinfo.get("window_work") or life_a)
    last_s = min((_share_age_s(m.get("last_share_s")) for m in recs), default=1e9)
    if pinfo.get("last_share_s") is not None:
        last_s = min(last_s, _share_age_s(pinfo.get("last_share_s"), missing=0.0))
    is_online = (credited > 1e-6 and last_s < 180) or any(
        float(m.get("hr_ghs") or 0) > 1e-6 and _share_age_s(m.get("last_share_s"), missing=0.0) < 180
        for m in recs
        if (m.get("via") or "") in ("stratum", "both", "prime", "gateway")
    )
    best = float(stored["best_hr_ghs"] if stored and stored["best_hr_ghs"] is not None else (hr or 0))
    if best >= _PRIME_HR_CAP_GHS:
        best = hr
    win = tides_window_snapshot()
    return {
        "address": address if known else "",
        "known": known,
        "online": bool(is_online),
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
        "diff_acc": (recs[0].get("diff_acc", 0) if recs else 0) or (stored["diff_acc"] if stored and "diff_acc" in stored.keys() else 0),
        "first_seen": stored["first_ts"] if stored else None,
        "last_seen": stored["last_ts"] if stored else None,
        "best_hr_ghs": best if stored or recs else hr,
        "pool_contribution": contrib,
        "hashrate_pool_percent": contrib * 100.0,
        "est_btc_day": est,
        "est_btc_week": est * 7,
        "ttf_seconds": ttf_s,
        # The exact output primed would put in the next coinbase for this address, when it
        # has one; otherwise the proportional estimate.
        "block_payout_btc": (int(pinfo.get("window_sats") or 0) / 1e8) if pinfo.get("window_sats") else SUBSIDY * (1 - POOL_FEE / 100.0) * round_share,
        "fee_path": pinfo.get("fee_path") or "",
        "fee_percent_path": _fee_percent_for_path(pinfo.get("fee_path")),
        "paid_btc": paid_btc,
        "unpaid_btc": 0.0,
        "immature_btc": immature_btc,
        "round_work": my_work,
        "round_share": round_share,
        "window_multiple": win["window_multiple"],
        "window_fill_percent": win["window_fill_percent"],
        "blocks_found": [dict(r) for r in (payouts or [])],
        "fee_percent": POOL_FEE,
        "history": [{"ts": r["ts"], "hr_ghs": r["hr"]} for r in hist],
    }


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
        self.send_json({"error": "not found"}, 404)

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
                info = prime_info_for(d.get("address") or "")
                d["shares_lifetime"] = life_a or int(info.get("window_work") or d.get("shares_lifetime") or d.get("shares_acc") or 0)
                d["shares_session"] = int(sess or d.get("shares_session") or 0)
                d["shares_acc"] = d["shares_lifetime"]
                d["shares_rej"] = life_r or int(d.get("shares_rej") or 0)
                d["window_work"] = int(info.get("window_work") or 0)
                d["window_percent"] = float(info.get("window_percent") or 0)
                d["window_sats"] = int(info.get("window_sats") or 0)
                d["fee_path"] = info.get("fee_path") or ""
                d["via"] = "prime" if (d.get("address") in (state.get("prime") or {}) and not any(o.get("address")==d.get("address") and o.get("via") == "stratum" for o in online)) else d.get("via")
                try:
                    if float(d.get("best_hr_ghs") or 0) > _PRIME_HR_CAP_GHS:
                        d["best_hr_ghs"] = _PRIME_HR_CAP_GHS
                except (TypeError, ValueError):
                    pass
                d["hr_ghs"] = float(info.get("hr_ghs") or 0)
                seen_out.append(d)
            self.send_json({"online": rollup_online_by_address(online), "seen": seen_out})
            return
        if path.startswith("/api/miner/"):
            addr = path.split("/api/miner/", 1)[1].strip("/")
            self.send_json(miner_payload(addr))
            return
        if path == "/api/blocks":
            self.send_json({"blocks": mempool_blocks()})
            return
        if path == "/api/coinbaser":
            self.send_json(prime_coinbaser_preview())
            return
        if path == "/api/gateways":
            pr = prime_summary()
            self.send_json({"reachable": pr["reachable"], "gateways": pr["gateways"], "totals": pr["totals"]})
            return
        if path == "/api/payouts":
            tip = rpc("getblockcount") or 0
            fbs = db("SELECT height, hash, ts, reward_btc FROM found_blocks ORDER BY height DESC LIMIT 20") or []
            payouts = []
            chain_ok = True
            for fb in fbs:
                splits = coinbase_splits(fb["hash"])
                if splits is None:
                    chain_ok = False
                    break
                reward = float(fb["reward_btc"] or 0) or sum(splits.values()) or 1.0
                nval = len(splits)
                st = payout_status_for_height(fb["height"], tip)
                if nval < 2:
                    st = "unsplit"
                for addr, amt in sorted(splits.items(), key=lambda kv: -kv[1]):
                    payouts.append(
                        {
                            "height": fb["height"],
                            "hash": fb["hash"],
                            "ts": fb["ts"],
                            "finder": addr,
                            "miner_btc": amt,
                            "pool_fee_btc": 0.0,
                            "share": (amt / reward) if reward else 0,
                            "status": st,
                            "reward_btc": reward,
                        }
                    )
            if not chain_ok:
                rows = db(
                    "SELECT r.id, r.height, r.hash, r.closed_ts AS ts, r.reward_btc, r.fee_btc, r.miner_btc, r.total_work, r.status, "
                    "p.address AS finder, p.amount_btc AS miner_paid, p.share, p.work "
                    "FROM rounds r LEFT JOIN round_payouts p ON p.round_id=r.id "
                    "WHERE r.status!='open' ORDER BY r.height DESC LIMIT 200"
                )
                payouts = [
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
                ]
            pr = prime_summary()
            prime_blocks = {b["hash"]: b for b in pr["blocks"] if b.get("hash")}
            pool_addr = pr.get("address") or ""
            tagged = []
            for row in payouts:
                pb = prime_blocks.get(row.get("hash"))
                row["kind"] = pb["kind"] if pb else ""
                row["block_status"] = pb["status"] if pb else ""
                row["owed_sats"] = pb["owed_sats"] if pb else 0
                row["gateway"] = pb["gateway"] if pb else ""
                row["found_by"] = pb["finder"] if pb else ""
                if pool_addr and row.get("finder") == pool_addr:
                    miner_btc, fee_btc = pool_output_parts(pool_addr, row.get("miner_btc"), pb, row.get("reward_btc"))
                    reward = float(row.get("reward_btc") or 0) or 1.0
                    if miner_btc > 0:
                        m = dict(row)
                        m["to"] = "miner"
                        m["miner_btc"] = miner_btc
                        m["pool_fee_btc"] = 0.0
                        m["share"] = miner_btc / reward
                        tagged.append(m)
                    if fee_btc > 0:
                        f = dict(row)
                        f["to"] = "pool"
                        f["miner_btc"] = fee_btc
                        f["pool_fee_btc"] = fee_btc
                        f["share"] = fee_btc / reward
                        tagged.append(f)
                    if miner_btc <= 0 and fee_btc <= 0:
                        row["to"] = "pool"
                        tagged.append(row)
                else:
                    row["to"] = "miner"
                    tagged.append(row)
            payouts = tagged
            prime_by = state.get("prime") or {}
            current = sorted(
                (
                    {
                        "address": addr,
                        "work": inf.get("window_work") or 0,
                        "share": (float(inf.get("window_percent") or 0) / 100.0),
                    }
                    for addr, inf in prime_by.items()
                ),
                key=lambda r: -int(r["work"] or 0),
            )
            self.send_json(
                {
                    "scheme": "TIDES",
                    "fee_percent": POOL_FEE,
                    "maturity_blocks": 100,
                    "current_round": current,
                    "payouts": payouts,
                    "prime_blocks": list(prime_blocks.values()),
                }
            )
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
                    ".svg": "image/svg+xml",
                    ".png": "image/png",
                    ".ico": "image/x-icon",
                    ".webmanifest": "application/manifest+json",
                }
                self.send_file(fp, mime.get(fp.suffix, "application/octet-stream"))
                return
        self.send_json({"error": "not found"}, 404)


def main():
    if not NO_WRITE:
        ensure_open_round()
        init_share_accounting()
    threading.Thread(target=loop, daemon=True).start()
    host = CONF.get("listen_host", "0.0.0.0")
    port = int(os.environ.get("POOL_LISTEN_PORT") or CONF.get("listen_port", 8888))
    print(f"lazarus-pool http://{host}:{port}", flush=True)
    ThreadingHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
