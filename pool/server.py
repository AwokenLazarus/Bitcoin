#!/usr/bin/env python3
"""Lazarus public mining-pool dashboard. Scrapes DATUM + Knots; no admin UI exposed."""
from __future__ import annotations

import contextlib
import gzip
import html
import json
import os
import queue
import re
import socket
import sqlite3
import subprocess
import sys
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
STRATUM_FEE = float(CONF.get("stratum_fee_percent", 10.0))
STRATUM_HOST = CONF.get("stratum_host", "27.69.0.25")
STRATUM_PORT = int(CONF.get("stratum_port", 23334))
DATUM_URL = CONF.get("datum_url", "http://127.0.0.1:7152")
DATUM_CLIENT_URLS = [(DATUM_URL, "stratum", STRATUM_PORT)]
MEMPOOL_API = CONF.get("mempool_api", "http://10.21.21.27:8999")
COOKIE = Path(CONF.get("cookie_file", "/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie"))
RPC_URL = CONF.get("rpc_url", "http://127.0.0.1:9332")
AUTH_FILE = Path(CONF.get("datum_auth_file", "/home/umbrel/blake2b/secrets/datum-admin.env"))
EXPLORER = CONF.get("explorer_url", "https://mempool.awokenlazarus.xyz")
COINBASE_TAG = CONF.get("coinbase_tag", "Lazarus")
# Solo blocks carry their own tag so this scanner can tell them apart. A solo block pays its
# finder inside its own coinbase, so it must never close a TIDES round: the window did not
# earn it and is owed nothing from it. Checked before COINBASE_TAG, since "Lazarus/solo"
# contains "Lazarus".
SOLO_TAG = CONF.get("solo_coinbase_tag", "Lazarus/solo")
# Solo stratum gateways, in the order they should appear. Each serves /solo.json.
SOLO_APIS = CONF.get(
    "solo_apis",
    [
        {"name": "ASIC", "url": "http://127.0.0.1:7154", "port": 23335},
        {"name": "GPU", "url": "http://127.0.0.1:7155", "port": 3334},
    ],
)
SUBSIDY = 3.125

PRIME_STATS = CONF.get("datum_prime_stats", "http://127.0.0.1:28916/stats.json")
# Manual window make-goods: block hash → {txid, height}. The coinbase of a
# pool-only block cannot be rewritten; once the pool spends it to pay the window,
# record the payout here so the UI stops saying the window is still owed.
OWED_SETTLEMENTS_PATH = Path(CONF.get("owed_settlements", str(ROOT / "owed-settlements.json")))
_owed_settlements_cache = {"mtime": None, "doc": {}}


def owed_settlements():
    """block hash → {txid, height} for window debts paid after the fact."""
    try:
        mtime = OWED_SETTLEMENTS_PATH.stat().st_mtime
    except OSError:
        return {}
    if _owed_settlements_cache["mtime"] == mtime:
        return _owed_settlements_cache["doc"]
    try:
        raw = json.loads(OWED_SETTLEMENTS_PATH.read_text())
    except Exception:
        raw = {}
    doc = {}
    if isinstance(raw, dict):
        for h, rec in raw.items():
            if not isinstance(rec, dict) or not rec.get("txid"):
                continue
            doc[str(h).lower()] = {
                "txid": str(rec["txid"]),
                "height": rec.get("height"),
            }
    _owed_settlements_cache["mtime"] = mtime
    _owed_settlements_cache["doc"] = doc
    return doc


def apply_owed_settlement(row):
    """Attach the make-good tx, if we have one, to a Prime/payout block row."""
    rec = owed_settlements().get(str(row.get("hash") or "").lower())
    if rec:
        row["owed_txid"] = rec["txid"]
        row["owed_resolved"] = True
    else:
        row.setdefault("owed_txid", "")
        row.setdefault("owed_resolved", False)
    return row

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
# Readers are pooled, not shared behind `lock`; see _reader().
_READER_POOL = 12
_reader_pool = queue.LifoQueue(maxsize=_READER_POOL)


def datum_user_pass():
    user, pw = "mike", ""
    if AUTH_FILE.exists():
        for line in AUTH_FILE.read_text().splitlines():
            if line.startswith("DATUM_ADMIN_USER="):
                user = line.split("=", 1)[1]
            if line.startswith("DATUM_ADMIN_PASSWORD="):
                pw = line.split("=", 1)[1]
    return user, pw


db_conn = sqlite3.connect(DB, check_same_thread=False, timeout=10)
db_conn.row_factory = sqlite3.Row


def _pragma(sql):
    err = None
    for _ in range(40):
        try:
            return db_conn.execute(sql)
        except sqlite3.OperationalError as e:
            err = e
            time.sleep(0.25)
    print("pragma", sql, err, flush=True)
    return None


# Three UI processes share this file; WAL lets the read replicas overlap the writer.
_pragma("PRAGMA journal_mode=WAL")
_pragma("PRAGMA busy_timeout=8000")
_pragma("PRAGMA synchronous=NORMAL")
# An automatic checkpoint only runs when no reader is mid-scan, and a busy dashboard on a
# 15M-row samples table never leaves that gap: the WAL reached 1.7 GB, every read had to
# search it, and address pages went from milliseconds to over a minute. Cap it and let the
# writer keep it trimmed.
_pragma("PRAGMA journal_size_limit=268435456")
_pragma("PRAGMA wal_autocheckpoint=2000")
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
    -- Solo blocks. Deliberately not in `found_blocks`: everything downstream of that table
    -- (rounds, round_payouts, effort) is TIDES accounting, and a solo block belongs to the
    -- one miner named in its coinbase.
    CREATE TABLE IF NOT EXISTS solo_blocks (
      height INTEGER PRIMARY KEY, hash TEXT, ts INTEGER, reward_btc REAL,
      finder TEXT, pool_fee_btc REAL, miner_btc REAL, coinbase TEXT
    );
    -- Covers the address history chart: hr_ghs is in the index, so a day of one address's
    -- samples is read straight from it. Without hr_ghs each of the (over a million, for a
    -- big farm) matching rows costs a lookup into the 3 GB table and the query took 83s.
    CREATE INDEX IF NOT EXISTS idx_samples_addr_ts_hr ON samples(address, ts, hr_ghs);
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
    -- What a gateway operator calls themselves, learned from the secondary coinbase tag on a
    -- block that gateway found. Keyed by gateway (the signing key prefix, stable across
    -- reconnects); `identity` is its payout address, refreshed while it is connected.
    CREATE TABLE IF NOT EXISTS gateway_tags (
      gateway TEXT PRIMARY KEY,
      tag TEXT,
      identity TEXT,
      height INTEGER,
      ts INTEGER
    );
    CREATE INDEX IF NOT EXISTS idx_gateway_tags_identity ON gateway_tags(identity);
    """
)
db_conn.commit()


def _ensure_samples_ts_index():
    if NO_WRITE:
        return
    err = None
    for _ in range(40):
        try:
            db_conn.execute("CREATE INDEX IF NOT EXISTS idx_samples_ts ON samples(ts)")
            db_conn.commit()
            return
        except sqlite3.OperationalError as e:
            err = e
            time.sleep(0.5)
    if err:
        print("idx_samples_ts", err, flush=True)


threading.Thread(target=_ensure_samples_ts_index, daemon=True).start()


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
            # Earned in earlier blocks but not yet placed in a coinbase (under the payout
            # floor, or no room). Prime pays it on top of the next output that fits, so it
            # is already inside window_sats when it is; this is what is still waiting.
            "carry_sats": int(m.get("carry_sats") or 0),
            # DATUM rebate the next found block credits to this identity's balance (carry):
            # its share of the public stratum's fee point. Paid with a later output once the
            # balance clears the floor, so it is not part of window_sats.
            "rebate_sats": int(m.get("rebate_sats") or 0),
            "payable": bool(m.get("payable")),
            # Ledger writes one credit row per accepted share (no coalesce), so this
            # is accepted shares still inside the TIDES window for this identity.
            "window_shares": int(m.get("credits") or 0),
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
        stratum_fee_bps = _bps_or(pool.get("stratum_fee_bps"), _bps_or(pool.get("fee_bps"), 0))
    except (TypeError, ValueError):
        stratum_fee_bps = 0
    # How the window's work splits by path. The rebate pot is stratum work's fee point and
    # it is shared by DATUM work, so DATUM's uplift over its proportional share is
    # rebate × stratum_share / datum_share — the headline the UI advertises.
    stratum_work = sum(v["stratum_work"] for v in by.values())
    total_work = sum(v["window_work"] for v in by.values())
    datum_work = max(0, total_work - stratum_work)
    rebate_bps = int(pool.get("datum_rebate_bps") or 0)
    datum_uplift = (rebate_bps / 100.0) * (stratum_work / datum_work) if (rebate_bps and datum_work > 0) else 0.0
    meta = {
        "stratum_fee_bps": stratum_fee_bps,
        "datum_work": datum_work,
        "stratum_work": stratum_work,
        "datum_work_percent": (100.0 * datum_work / total_work) if total_work else 0.0,
        "stratum_work_percent": (100.0 * stratum_work / total_work) if total_work else 0.0,
        # Percent above its proportional share that DATUM work earns right now, from the rebate.
        "datum_uplift_percent": datum_uplift,
        "datum_miners": sum(1 for v in by.values() if v["fee_path"] != "stratum" and v["window_work"] > 0),
        # DATUM rebate: bps of stratum work's value handed to DATUM work, and of solo-block
        # rewards owed to it. 0 when primed predates the feature or has it off.
        "datum_rebate_bps": int(pool.get("datum_rebate_bps") or 0),
        "solo_rebate_bps": int(pool.get("solo_rebate_bps") or 0),
        "sample_rebate_sats": int(win.get("sample_rebate_sats") or 0),
        "sample_rebate_owed_credited_sats": int(win.get("sample_rebate_owed_credited_sats") or 0),
        "rebate_owed_sats": int(win.get("rebate_owed_sats") or 0),
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
        # TIDES carry: earnings the floor kept out of earlier coinbases, held per identity
        # and paid on top of the next output that clears it (out of the pool's remainder).
        "sample_carry_paid_sats": int(win.get("sample_carry_paid_sats") or 0),
        "sample_deferred_sats": int(win.get("sample_deferred_sats") or 0),
        "carry_total_sats": int(win.get("carry_total_sats") or 0),
        "carry_holders": int(win.get("carry_holders") or 0),
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
BLOCKS_LOG = Path(CONF.get("prime_blocks_log", str(LEDGER_PATH.with_name("blocks.jsonl"))))
LEDGER_HR_WINDOW_S = int(CONF.get("ledger_hr_window_s", 600))
_ledger_hr_cache = {"ts": 0.0, "by_addr": {}, "pool_ghs": 0.0, "age": {}}
# Lifetime finds per gateway, from primed's blocks.jsonl (survives Prime restarts).
_block_log_cache = {"sig": None, "found": {}, "finder": {}, "n": 0}


def gateway_finds_from_log():
    """Non-orphan finds per gateway signing-key prefix, latest line per block hash.

    primed's in-memory client.block_candidates resets on restart; this file does not.
    """
    try:
        st = BLOCKS_LOG.stat()
        sig = (st.st_mtime_ns, st.st_size)
    except OSError:
        return {}, {}, 0
    cached = _block_log_cache
    if cached["sig"] == sig:
        return cached["found"], cached["finder"], cached["n"]
    latest = {}
    try:
        with BLOCKS_LOG.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    rec = json.loads(line)
                except json.JSONDecodeError:
                    continue
                h = rec.get("hash")
                if h:
                    latest[h] = rec
    except OSError:
        return {}, {}, 0
    found = {}
    finder = {}
    last_ts = {}
    for rec in latest.values():
        if str(rec.get("kind") or "").startswith("orphan"):
            continue
        gw = str(rec.get("gateway") or "")
        if not gw:
            continue
        found[gw] = found.get(gw, 0) + 1
        ts = int(rec.get("ts") or 0)
        ident = str(rec.get("finder") or "")
        if ident and ts >= last_ts.get(gw, -1):
            last_ts[gw] = ts
            finder[gw] = ident
    n = sum(found.values())
    cached.update(sig=sig, found=found, finder=finder, n=n)
    return found, finder, n


def _merge_persistent_gateway_finds(clients):
    """Stamp lifetime finds onto live rows and re-add gateways that found blocks then dropped."""
    found, finder, nfound = gateway_finds_from_log()
    tags = {}
    for r in db("SELECT gateway, identity FROM gateway_tags") or []:
        if r["identity"]:
            tags[str(r["gateway"])] = str(r["identity"])
    by = {}
    extras = []
    for row in clients:
        gw = str(row.get("gateway") or "")
        if found:
            row["block_candidates"] = int(found.get(gw, 0)) if gw else int(row.get("block_candidates") or 0)
        row["offline"] = bool(row.get("offline"))
        if gw:
            by[gw] = row
        else:
            extras.append(row)
    for gw, n in found.items():
        if n <= 0 or gw in by:
            continue
        by[gw] = {
            "id": 0,
            "gateway": gw,
            "user_agent": "",
            "generation": "",
            "own": False,
            "offline": True,
            "fee_path": "datum",
            "identity": finder.get(gw) or tags.get(gw) or "",
            "connected_s": 0,
            "accepted": 0,
            "rejected": 0,
            "last_reject": "",
            "last_share_s": None,
            "work": 0,
            "coinbasers": 0,
            "block_candidates": n,
        }
    out = extras + list(by.values())
    out.sort(
        key=lambda g: (
            bool(g.get("offline")),
            g.get("own"),
            -(g.get("block_candidates") or 0),
            -(g.get("work") or 0),
        )
    )
    return out, nfound


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


def _bps_or(val, fallback):
    """Basis points from primed. 0 is a real fee (free), not 'missing'."""
    if val is None or val == "":
        return int(round(fallback))
    return int(val)


def _fee_percent_for_path(fee_path):
    """Fee rate (percent) primed applies to work that arrived on `fee_path`."""
    pool = prime_doc().get("pool") or {}
    stratum = str(fee_path or "").lower() == "stratum"
    try:
        if stratum:
            return _bps_or(pool.get("stratum_fee_bps"), _bps_or(pool.get("fee_bps"), STRATUM_FEE * 100)) / 100.0
        return _bps_or(pool.get("fee_bps"), POOL_FEE * 100) / 100.0
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
        "window_shares": 0,
        "carry_sats": 0,
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
    rec["window_shares"] = int(info.get("window_shares") or info.get("credits") or 0)
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
            m["window_shares"] = int(info.get("window_shares") or info.get("credits") or 0)
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
            "window_shares": int(info.get("window_shares") or info.get("credits") or 0),
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


def _new_reader():
    c = sqlite3.connect(DB, check_same_thread=False, timeout=10)
    c.row_factory = sqlite3.Row
    c.execute("PRAGMA busy_timeout=8000")
    return c


@contextlib.contextmanager
def _reader():
    """Borrow one of a few reader connections.

    One shared connection behind one lock serialised every query in the process, so a
    cold cache turned each request into a queue: the address-history query is seconds of
    work, and 24 of them back to back meant the inflight table filled and /api/miner
    started answering 503. WAL lets readers overlap each other and the writer.

    A small pool rather than one per thread: this server is thread-per-connection, and a
    fresh connection starts with an empty page cache, so a few long-lived readers are
    both bounded and warm."""
    try:
        c = _reader_pool.get_nowait()
    except queue.Empty:
        c = _new_reader()
    try:
        yield c
    finally:
        try:
            _reader_pool.put_nowait(c)
        except queue.Full:
            c.close()


def _checkpoint_wal():
    """Force the WAL back into the database. `wal_autocheckpoint` gives up whenever a
    reader is mid-scan, so on the writer we ask for it outright on a schedule; RESTART
    rather than TRUNCATE so it does not wait for readers to drain."""
    if NO_WRITE:
        return
    try:
        with lock:
            row = db_conn.execute("PRAGMA wal_checkpoint(RESTART)").fetchone()
        print("wal_checkpoint", tuple(row) if row else None, flush=True)
    except sqlite3.Error as e:
        print("wal_checkpoint", e, flush=True)


def db(q, args=(), one=False, write=False):
    if write and NO_WRITE:
        return None
    if write:
        # Writes stay on the one connection under the one lock: a single writer per
        # process, and only the :8888 instance writes at all.
        with lock:
            cur = db_conn.execute(q, args)
            db_conn.commit()
            return cur.lastrowid
    with _reader() as c:
        rows = c.execute(q, args).fetchall()
    return rows[0] if one and rows else (rows if not one else None)


def _curl_quote(s):
    """Quote a value for a curl config file (`-K`): double quotes, backslash escapes."""
    return '"' + str(s).replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def curl(url, digest=False, timeout=3, headers=None):
    # Credentials go to curl through a config file on stdin (`-K -`), never on the command
    # line, where every local process could read them from /proc/*/cmdline.
    cmd = ["curl", "-sS", "--max-time", str(timeout), "-K", "-"]
    conf = [f"url = {_curl_quote(url)}"]
    for h in headers or ():
        conf.append(f"header = {_curl_quote(h)}")
    if digest:
        u, p = datum_user_pass()
        conf += ["digest", f"user = {_curl_quote(f'{u}:{p}')}"]
    try:
        return subprocess.run(
            cmd, input="\n".join(conf).encode(), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            timeout=timeout + 5, check=True,
        ).stdout.decode("utf-8", "replace")
    except Exception:
        return ""


def rpc(method, params=None):
    if not COOKIE.exists():
        return None
    auth = COOKIE.read_text().strip()
    payload = json.dumps({"jsonrpc": "1.0", "id": "p", "method": method, "params": params or []})
    conf = "\n".join(
        [
            f"url = {_curl_quote(RPC_URL)}",
            f"user = {_curl_quote(auth)}",
            f"data-binary = {_curl_quote(payload)}",
            'header = "content-type:text/plain"',
        ]
    )
    try:
        out = subprocess.run(
            ["curl", "-sS", "--max-time", "8", "-K", "-"],
            input=conf.encode(), stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=15, check=True,
        ).stdout
        return json.loads(out).get("result")
    except Exception:
        return None


# Short-lived response cache for the anonymous API. Every one of these endpoints forks curl
# to bitcoind or the mempool API, or walks the database, per request; behind Cloudflare
# that is a fork-per-request the whole internet can drive. Payloads change on the order of
# seconds anyway.
_resp_cache = {}
_resp_cache_lock = threading.Lock()
_RESP_CACHE_MAX = 512
_BLOCKHASH_RE = re.compile(r"^[0-9a-fA-F]{64}$")
_cache_compute_locks = {}
_cache_refreshing = set()


def _compute_lock(key):
    with _resp_cache_lock:
        lock = _cache_compute_locks.get(key)
        if lock is None:
            lock = threading.Lock()
            _cache_compute_locks[key] = lock
        return lock


def cache_peek(key):
    """The last payload built for `key`, however old, or None. Lets the busy path answer
    a miner looking at its own stats with slightly stale numbers instead of a 503."""
    with _resp_cache_lock:
        hit = _resp_cache.get(key)
    return hit[1] if hit else None


def _cache_store(key, val):
    now = time.time()
    with _resp_cache_lock:
        if len(_resp_cache) >= _RESP_CACHE_MAX:
            for k in sorted(_resp_cache, key=lambda k: _resp_cache[k][0])[: _RESP_CACHE_MAX // 4]:
                _resp_cache.pop(k, None)
        _resp_cache[key] = (now, val)
    return val


def cached(key, ttl, fn):
    """Fresh hit, else last good payload while one thread refreshes."""
    now = time.time()
    with _resp_cache_lock:
        hit = _resp_cache.get(key)
        if hit and now - hit[0] < ttl:
            return hit[1]
        stale = hit[1] if hit else None
        refreshing = key in _cache_refreshing
    if stale is not None:
        if not refreshing:

            def _bg():
                try:
                    _cache_store(key, fn())
                except Exception as e:
                    print("cache", key, e, flush=True)
                finally:
                    with _resp_cache_lock:
                        _cache_refreshing.discard(key)

            with _resp_cache_lock:
                if key not in _cache_refreshing:
                    _cache_refreshing.add(key)
                    threading.Thread(target=_bg, daemon=True).start()
        return stale
    with _compute_lock(key):
        now = time.time()
        with _resp_cache_lock:
            hit = _resp_cache.get(key)
            if hit and now - hit[0] < ttl:
                return hit[1]
        return _cache_store(key, fn())


# BLAKE2b BTC (ticker BTCB2) USD: volume-weighted average of the two live listings.
# Each venue is weighted by its 24h quote volume (USDC/USDT treated as $1).
_NEOXA_BTCB2 = "https://neoxa.exchange/api/exchange/ticker/BTCB2_USDC"
_NONKYC_BTCB2 = "https://api.nonkyc.io/api/v2/ticker/BTCB2_USDT"
_price_lock = threading.Lock()
_price_cache = {"doc": None, "ts": 0.0, "refreshing": False}


def _pos_float(x):
    try:
        v = float(x)
    except (TypeError, ValueError):
        return None
    if v != v or v <= 0:
        return None
    return v


def _http_json(url, timeout=8):
    raw = curl(url, timeout=timeout)
    if not raw:
        return None
    try:
        return json.loads(raw)
    except Exception:
        return None


def _neoxa_btcb2_quote():
    d = _http_json(_NEOXA_BTCB2)
    if not isinstance(d, dict):
        return None, None
    t = d.get("ticker") or {}
    return _pos_float(t.get("lastPrice")), _pos_float(t.get("quoteVolume24h"))


def _nonkyc_btcb2_quote():
    d = _http_json(_NONKYC_BTCB2)
    if not isinstance(d, dict):
        return None, None
    last = _pos_float(d.get("last_price"))
    vol = _pos_float(d.get("usd_volume_est")) or _pos_float(d.get("target_volume"))
    return last, vol


def volume_weighted_usd(quotes):
    """quotes: iterable of (last, volume_usd). Weight by volume; fall back to a simple mean."""
    priced = [(p, v) for p, v in quotes if p is not None]
    if not priced:
        return None, "none"
    weighted = [(p, v) for p, v in priced if v]
    if weighted:
        return sum(p * v for p, v in weighted) / sum(v for _, v in weighted), "volume_weighted"
    return sum(p for p, _ in priced) / len(priced), "average"


def _fiat_from_sha_basket(usd):
    """Scale EUR/GBP/… from the mempool backend's SHA basket so those currencies stay relative to USD."""
    extras = {}
    raw = curl(f"{MEMPOOL_API.rstrip('/')}/api/v1/prices", timeout=3)
    try:
        fx = json.loads(raw) if raw else {}
    except Exception:
        fx = {}
    if not isinstance(fx, dict):
        return extras
    sha = _pos_float(fx.get("USD"))
    if not sha:
        return extras
    for k, v in fx.items():
        if k in ("time", "USD"):
            continue
        n = _pos_float(v)
        if n:
            extras[k] = round(usd * (n / sha), 2)
    return extras


def _price_quotes():
    box = {"neoxa": (None, None), "nonkyc": (None, None)}

    def n():
        box["neoxa"] = _neoxa_btcb2_quote()

    def k():
        box["nonkyc"] = _nonkyc_btcb2_quote()

    t1 = threading.Thread(target=n, daemon=True)
    t2 = threading.Thread(target=k, daemon=True)
    t1.start()
    t2.start()
    t1.join(10)
    t2.join(10)
    return box["neoxa"], box["nonkyc"]


def _price_compute():
    """BTCB2 USD plus mempool-shaped fiat keys. Last good print is kept if both books fail."""
    now = time.time()
    (neoxa_last, neoxa_vol), (nonkyc_last, nonkyc_vol) = _price_quotes()
    usd, method = volume_weighted_usd(((neoxa_last, neoxa_vol), (nonkyc_last, nonkyc_vol)))
    n_quotes = sum(1 for v in (neoxa_last, nonkyc_last) if v is not None)
    if usd is not None:
        doc = {
            "USD": round(usd, 2),
            "time": int(now),
            "stale": False,
            "pair": "BTCB2",
            "method": method,
            "sources": {
                "neoxa": {
                    "pair": "BTCB2_USDC",
                    "last": neoxa_last,
                    "volume": neoxa_vol,
                    "url": "https://neoxa.exchange/trade/BTCB2_USDC",
                },
                "nonkyc": {
                    "pair": "BTCB2_USDT",
                    "last": nonkyc_last,
                    "volume": nonkyc_vol,
                    "url": "https://nonkyc.io/market/BTCB2_USDT",
                },
            },
            "average_of": n_quotes,
        }
        doc.update(_fiat_from_sha_basket(usd))
        with _price_lock:
            _price_cache["doc"] = doc
            _price_cache["ts"] = now
        return doc
    with _price_lock:
        _price_cache["ts"] = now
        if _price_cache["doc"]:
            stale = dict(_price_cache["doc"])
            stale["stale"] = True
            return stale
    return {
        "USD": None,
        "time": int(now),
        "stale": True,
        "pair": "BTCB2",
        "method": "none",
        "sources": {
            "neoxa": {"pair": "BTCB2_USDC", "last": None, "volume": None, "url": "https://neoxa.exchange/trade/BTCB2_USDC"},
            "nonkyc": {"pair": "BTCB2_USDT", "last": None, "volume": None, "url": "https://nonkyc.io/market/BTCB2_USDT"},
        },
        "average_of": 0,
    }


def _price_refresh_bg():
    try:
        _price_compute()
    except Exception as e:
        print("price", e, flush=True)
    finally:
        with _price_lock:
            _price_cache["refreshing"] = False


def price_payload():
    now = time.time()
    with _price_lock:
        doc = _price_cache["doc"]
        age = now - _price_cache["ts"]
        if doc and age < 45:
            return doc
        if doc:
            if not _price_cache["refreshing"]:
                _price_cache["refreshing"] = True
                threading.Thread(target=_price_refresh_bg, daemon=True).start()
            stale = dict(doc)
            stale["stale"] = True
            return stale
    return _price_compute()


def mempool_prices_payload():
    """Same shape as stock mempool /api/v1/prices: {time, USD, EUR, …}."""
    p = price_payload()
    out = {"time": int(p.get("time") or time.time())}
    if p.get("USD") is not None:
        out["USD"] = p["USD"]
    for k, v in p.items():
        if k in ("USD", "time", "stale", "pair", "sources", "average_of", "method"):
            continue
        if isinstance(v, (int, float)):
            out[k] = v
    return out


# BT-Miners BTCB2 collection (WooCommerce Store API). Public, no key. Category 1470 is
# https://bt-miners.com/collections/btcb2-miners/ — BLAKE2b/Siacoin boxes that hash this chain.
_BT_MINERS_COLLECTION = "https://bt-miners.com/collections/btcb2-miners/"
_BT_MINERS_API = (
    "https://bt-miners.com/wp-json/wc/store/v1/products?category=1470&per_page=50&orderby=price&order=asc"
)
_HR_IN_TITLE = re.compile(r"(\d+(?:\.\d+)?)\s*(T|G|M|K)H/?s", re.I)
_hardware_lock = threading.Lock()
_hardware_cache = {"miners": None, "ts": 0.0, "refreshing": False, "error": ""}


def _wc_attr(product, name):
    want = name.strip().lower()
    for a in product.get("attributes") or []:
        if (a.get("name") or "").strip().lower() != want:
            continue
        terms = a.get("terms") or []
        if not terms:
            return ""
        default = next((t for t in terms if t.get("default")), terms[0])
        return str(default.get("name") or "")
    return ""


def _ths_from_product(product):
    """Advertised BLAKE2b hashrate in TH/s. Title first: dual-mode boxes (HS5) put HNS in the attribute."""
    title = html.unescape(product.get("name") or "")
    m = _HR_IN_TITLE.search(title)
    if m:
        n = float(m.group(1))
        unit = m.group(2).upper()
        return n * {"T": 1.0, "G": 1e-3, "M": 1e-6, "K": 1e-9}[unit]
    raw = _wc_attr(product, "Hashrate")
    try:
        hs = float(raw)
    except (TypeError, ValueError):
        return None
    return (hs / 1e12) if hs > 0 else None


def _usd_from_wc_prices(prices):
    if not isinstance(prices, dict):
        return None
    raw = prices.get("price")
    if raw in (None, "", []):
        return None
    try:
        minor = int(prices.get("currency_minor_unit") or 2)
        return float(raw) / (10 ** minor)
    except (TypeError, ValueError):
        return None


def _watts_from_product(product):
    raw = _wc_attr(product, "Power")
    try:
        w = float(raw)
    except (TypeError, ValueError):
        return None
    return w if w > 0 else None


def _normalize_bt_miners(raw):
    miners = []
    for p in raw or []:
        if not isinstance(p, dict):
            continue
        name = html.unescape(p.get("name") or "").strip()
        url = (p.get("permalink") or "").strip()
        if not name or not url:
            continue
        if "hosting" in name.lower() and "miner" not in name.lower():
            continue
        ths = _ths_from_product(p)
        if ths is None or ths <= 0:
            continue
        imgs = p.get("images") or []
        image = ""
        if imgs and isinstance(imgs[0], dict):
            image = imgs[0].get("thumbnail") or imgs[0].get("src") or ""
        watts = _watts_from_product(p)
        miners.append(
            {
                "id": p.get("id"),
                "name": name,
                "model": html.unescape(_wc_attr(p, "pcname") or name),
                "url": url,
                "sku": html.unescape(p.get("sku") or ""),
                "ths": ths,
                "watts": watts,
                "price_usd": _usd_from_wc_prices(p.get("prices") or {}),
                "in_stock": bool(p.get("is_in_stock")),
                "condition": html.unescape(_wc_attr(p, "Condition") or ""),
                "image": image,
            }
        )
    miners.sort(key=lambda m: (-(m.get("ths") or 0), m.get("name") or ""))
    return miners


def _hardware_fetch():
    raw = curl(
        _BT_MINERS_API,
        timeout=12,
        headers=(
            "User-Agent: LazarusPool/1.0 (+https://pool.awokenlazarus.xyz)",
            "Accept: application/json",
        ),
    )
    if not raw:
        raise RuntimeError("bt-miners catalog empty")
    data = json.loads(raw)
    if not isinstance(data, list):
        raise RuntimeError("bt-miners catalog unexpected")
    miners = _normalize_bt_miners(data)
    if not miners:
        raise RuntimeError("bt-miners catalog parsed empty")
    return miners


def _hardware_refresh_bg():
    try:
        miners = _hardware_fetch()
        with _hardware_lock:
            _hardware_cache["miners"] = miners
            _hardware_cache["ts"] = time.time()
            _hardware_cache["error"] = ""
    except Exception as e:
        print("hardware", e, flush=True)
        with _hardware_lock:
            _hardware_cache["error"] = str(e)
            if _hardware_cache["miners"] is None:
                _hardware_cache["ts"] = time.time()
    finally:
        with _hardware_lock:
            _hardware_cache["refreshing"] = False


def hardware_catalog(max_age=900.0):
    """Listed BTCB2 machines. Stale-while-revalidate so a shop outage does not stall the UI."""
    now = time.time()
    with _hardware_lock:
        miners = _hardware_cache["miners"]
        age = now - _hardware_cache["ts"]
        err = _hardware_cache["error"]
        if miners and age < max_age:
            return miners, err
        if not _hardware_cache["refreshing"]:
            _hardware_cache["refreshing"] = True
            threading.Thread(target=_hardware_refresh_bg, daemon=True).start()
        if miners:
            return miners, err
    try:
        miners = _hardware_fetch()
        with _hardware_lock:
            _hardware_cache["miners"] = miners
            _hardware_cache["ts"] = time.time()
            _hardware_cache["error"] = ""
        return miners, ""
    except Exception as e:
        print("hardware", e, flush=True)
        with _hardware_lock:
            _hardware_cache["error"] = str(e)
            return _hardware_cache["miners"] or [], str(e)


def hardware_payload():
    catalog, err = hardware_catalog()
    pool = cache_peek("pool") or {}
    if not pool.get("ths_btc_day"):
        try:
            pool = cached("pool", 5.0, pool_payload) or pool
        except Exception:
            pass
    px = price_payload()
    usd = px.get("USD")
    try:
        usd = float(usd) if usd is not None else None
    except (TypeError, ValueError):
        usd = None
    ths_gross = float(pool.get("ths_btc_day") or 0)
    ths_datum = float(pool.get("ths_btc_day_datum_bonus") or pool.get("ths_btc_day_datum") or 0)
    ths_stratum = float(pool.get("ths_btc_day_stratum") or 0)
    if ths_datum <= 0 and ths_gross > 0:
        ths_datum = ths_gross
    miners = []
    for m in catalog:
        ths = float(m.get("ths") or 0)
        xbt = (ths * ths_datum) if ths_datum > 0 and ths > 0 else None
        xbt_stratum = (ths * ths_stratum) if ths_stratum > 0 and ths > 0 else None
        row = dict(m)
        row["xbt_day"] = xbt
        row["usd_day"] = (xbt * usd) if xbt is not None and usd is not None else None
        row["xbt_day_stratum"] = xbt_stratum
        row["usd_day_stratum"] = (xbt_stratum * usd) if xbt_stratum is not None and usd is not None else None
        miners.append(row)
    return {
        "source": "BT-Miners",
        "source_url": _BT_MINERS_COLLECTION,
        "miners": miners,
        "price_usd": usd,
        "ths_btc_day": ths_datum,
        "ths_btc_day_stratum": ths_stratum,
        "difficulty": pool.get("difficulty"),
        "error": err or "",
        "note": "Estimates use current network difficulty and the 3.125 XBT base subsidy, through a DATUM gateway on Lazarus (bonus included). Electricity is not included. Prices are BT-Miners list prices.",
    }


# Identities are DATUM usernames minus the worker suffix: an address, or whatever a
# gateway forwarded. Anything else is not a miner we could know about.
_ADDRESS_RE = re.compile(r"^[A-Za-z0-9._~-]{1,128}$")
# Concurrent expensive requests; extras 503. Stats/static still run when this is full.
_inflight = threading.BoundedSemaphore(24)


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


def secondary_coinbase_tag(coinbase_hex):
    """The gateway operator's own name from a DATUM coinbase, or "" if it carries none.

    `datum_gateway` writes the first push after the BIP34 height as
    ``<primary tag> 0x0F <secondary tag> 0x00``: the pool sets the primary ("Lazarus"), whoever
    runs the gateway sets the secondary. Our own gateway leaves it empty.
    """
    try:
        raw = bytes.fromhex(coinbase_hex or "")
    except Exception:
        return ""
    if not raw:
        return ""
    height_len = raw[0]
    if not 1 <= height_len <= 8 or len(raw) <= 1 + height_len:
        return ""
    i = 1 + height_len
    push = raw[i]
    if push == 0x4C:  # OP_PUSHDATA1
        i += 1
        if i >= len(raw):
            return ""
        push = raw[i]
    tags = raw[i + 1 : i + 1 + push]
    if len(tags) != push or 0x0F not in tags:
        return ""
    tag = tags.split(b"\x0f", 1)[1].split(b"\x00", 1)[0]
    return "".join(chr(b) for b in tag if 32 <= b < 127).strip()[:40]


def _fetch_overflow():
    """Gateway overflow status and the sessions it is relaying to other pools.

    The gateway answers `/proxied.json` with `{"overflow": {...}, "proxied": [...]}`; an
    old gateway (or a solo one) has no such route and answers HTML, which is treated as
    "no overflow feature"."""
    try:
        raw = curl(DATUM_URL + "/proxied.json", timeout=3)
        doc = json.loads(raw)
    except Exception:
        return {"overflow": None, "proxied": []}
    if not isinstance(doc, dict):
        return {"overflow": None, "proxied": []}
    rows = []
    for p in doc.get("proxied") or []:
        addr, worker = split_user(p.get("user") or "")
        rows.append(
            {
                "address": p.get("identity") or addr,
                "worker": p.get("worker") or worker,
                "user": p.get("user") or "",
                "host": p.get("host") or "",
                "ua": p.get("ua") or "",
                "upstream": p.get("upstream") or "",
                "upstream_url": p.get("upstream_url") or "",
                "miner_url": p.get("miner_url") or p.get("upstream_url") or "",
                "connected_s": int(p.get("connected_s") or 0),
                "submits": int(p.get("submits") or 0),
                "accepted": int(p.get("accepted") or 0),
                "via": "relayed",
                "online": True,
            }
        )
    ov = doc.get("overflow")
    if isinstance(ov, dict):
        ov = {
            "mode": ov.get("mode"),
            "active": bool(ov.get("active")),
            "active_since_unix": ov.get("active_since_unix") or 0,
            "share_pct": float(ov.get("share_pct") or 0),
            "enter_pct": ov.get("enter_pct"),
            "exit_pct": ov.get("exit_pct"),
            "pool_hs": float(ov.get("pool_hs") or 0),
            "stratum_hs": float(ov.get("stratum_hs") or 0),
            "datum_hs": float(ov.get("datum_hs") or 0),
            "net_hs": float(ov.get("net_hs") or 0),
            "meter_ok": bool(ov.get("meter_ok")),
            "meter_updated_unix": ov.get("meter_updated_unix") or 0,
            "proxied_sessions": int(ov.get("proxied_sessions") or 0),
            "proxied_total": int(ov.get("proxied_total") or 0),
            "upstreams": [
                {
                    "name": u.get("name"),
                    "url": u.get("url"),
                    "host": u.get("host"),
                    "port": u.get("port"),
                    "healthy": bool(u.get("healthy")),
                    "sessions": int(u.get("sessions") or 0),
                }
                for u in (ov.get("upstreams") or [])
            ],
        }
    else:
        ov = None
    return {"overflow": ov, "proxied": rows}


def overflow_doc():
    return cached("overflow", 5.0, _fetch_overflow)


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
    last_prune = int(state.get("last_prune_ts") or 0)
    if ts - last_prune >= 1800:
        db("DELETE FROM samples WHERE ts < ?", (ts - 3 * 86400,), write=True)
        db("DELETE FROM pool_samples WHERE ts < ?", (ts - 7 * 86400,), write=True)
        state["last_prune_ts"] = ts
        _checkpoint_wal()
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
    # Pool wallet is not a TIDES payee here: the whole output is fee + remainder.
    # Do not fall back to pool_fee_percent (0% DATUM) — that tagged every remainder as a miner
    # and made the Found table's Pool column all zeros.
    if pb is not None:
        fee_sats = pb.get("fee_sats")
        if fee_sats is None and pb.get("pool_sats") is not None:
            fee_sats = max(0, int(pb.get("pool_sats") or 0) - int(pb.get("miner_to_pool_sats") or 0))
        if fee_sats is not None:
            fee_btc = min(total, int(fee_sats) / 1e8)
            return max(0.0, total - fee_btc), fee_btc
    return 0.0, total


def found_outputs_payload(blockhash):
    """Coinbase outputs for one found block, with the pool-address fee split applied."""
    splits = coinbase_splits(blockhash)
    if not splits:
        return None
    pr = prime_summary()
    pool_addr = pr.get("address") or ""
    pb = next((b for b in (pr.get("blocks") or []) if b.get("hash") == blockhash), None)
    fb = db("SELECT reward_btc FROM found_blocks WHERE hash=?", (blockhash,), one=True)
    reward = float((fb["reward_btc"] if fb else 0) or 0) or sum(splits.values()) or 1.0
    outs = []
    for addr, amt in sorted(splits.items(), key=lambda kv: -kv[1]):
        amt = float(amt or 0)
        if pool_addr and addr == pool_addr:
            miner_btc, fee_btc = pool_output_parts(pool_addr, amt, pb, reward)
            if miner_btc > 0:
                outs.append({"address": addr, "btc": miner_btc, "share": miner_btc / reward, "to": "miner"})
            if fee_btc > 0:
                outs.append({"address": addr, "btc": fee_btc, "share": fee_btc / reward if reward else 0, "to": "pool"})
        else:
            outs.append({"address": addr, "btc": amt, "share": amt / reward if reward else 0, "to": "miner"})
    return {"hash": blockhash, "outputs": outs}


_cb_split_table = {"ok": False}


def _init_cb_splits_table():
    if _cb_split_table["ok"] or NO_WRITE:
        return
    try:
        db(
            "CREATE TABLE IF NOT EXISTS coinbase_splits (hash TEXT, address TEXT, btc REAL, PRIMARY KEY (hash, address))",
            write=True,
        )
        _cb_split_table["ok"] = True
    except Exception as e:
        print("coinbase_splits table", e, flush=True)


def coinbase_splits(blockhash):
    """Address -> BTC actually paid in that block's coinbase.

    A coinbase never changes, so the split is kept in sqlite once fetched: a miner's
    lifetime Paid/Immature walks every block the pool found, which would otherwise be one
    getblock per block per restart."""
    if not blockhash:
        return None
    if blockhash in _cb_split_cache:
        return _cb_split_cache[blockhash]
    _init_cb_splits_table()
    try:  # read-only replicas share the writer's table
        rows = db("SELECT address, btc FROM coinbase_splits WHERE hash=?", (blockhash,)) or []
    except Exception:
        rows = []
    if rows:
        by = {r["address"]: float(r["btc"] or 0) for r in rows}
        _cb_split_cache[blockhash] = by
        return by
    blk = rpc("getblock", [blockhash, 2])
    if not blk:
        return None
    tx0 = (blk.get("tx") or [None])[0] or {}
    by = splits_from_vouts(tx0.get("vout"))
    _cb_split_cache[blockhash] = by
    if _cb_split_table["ok"] and by and int(blk.get("confirmations") or 0) >= 6:
        for addr, amt in by.items():
            db("INSERT OR REPLACE INTO coinbase_splits(hash,address,btc) VALUES(?,?,?)", (blockhash, addr, float(amt)), write=True)
    return by


MATURITY_CONFS = 100


def payout_status_for_height(height, tip):
    if not height or not tip:
        return "paid"
    if int(tip) < int(height) + MATURITY_CONFS:
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


def learn_gateway_tags(budget=4):
    """Learn what each gateway operator calls themselves, from blocks their gateway found.

    primed records *which* gateway found each block, so the secondary tag on that block's
    coinbase names that gateway's operator. Reading the tag off the block's own outputs would
    not: the first output is the largest payee in the window, usually somebody else entirely.

    One RPC per newly-learnable gateway, a few per tick. A row is written even when the block
    carried no tag, so gateways that never set one are not re-fetched until they find a newer
    block. Our own gateway is skipped -- its `identity` is just whichever stratum address it
    last reported, so a tag there would name the wrong miner.
    """
    if NO_WRITE:  # read-only replicas share the writer's database; do not spend RPC on it
        return
    meta = state.get("prime_meta") or {}
    blocks = meta.get("blocks") or []
    if not blocks:
        return
    own = {
        c.get("gateway")
        for c in meta.get("clients") or []
        if str(c.get("user_agent") or "").startswith(OWN_GATEWAY_UA_PREFIX)
    }
    best = {}
    for b in blocks:
        gw = str(b.get("gateway") or "")
        h = int(b.get("height") or 0)
        if not gw or gw in own or not h or not b.get("hash"):
            continue
        if h > int((best.get(gw) or (0, ""))[0]):
            best[gw] = (h, b["hash"])
    known = {
        r["gateway"]: int(r["height"] or 0)
        for r in db("SELECT gateway, height FROM gateway_tags") or []
    }
    spent = 0
    for gw, (h, hsh) in sorted(best.items(), key=lambda kv: -kv[1][0]):
        if spent >= budget:
            break
        if gw in known and known[gw] >= h:
            continue
        blk = rpc("getblock", [hsh, 2])
        spent += 1
        if not blk:
            continue
        vin = ((blk.get("tx") or [{}])[0].get("vin") or [{}])[0]
        tag = secondary_coinbase_tag(vin.get("coinbase") or "")
        db(
            "INSERT INTO gateway_tags(gateway,tag,height,ts) VALUES(?,?,?,?) "
            "ON CONFLICT(gateway) DO UPDATE SET tag=excluded.tag, height=excluded.height, ts=excluded.ts",
            (gw, tag, h, int(time.time())),
            write=True,
        )


def refresh_gateway_identities():
    """Keep each learned gateway's payout address (and live secondary tag) current."""
    if NO_WRITE:
        return
    ts = int(time.time())
    for c in (state.get("prime_meta") or {}).get("clients") or []:
        gw = str(c.get("gateway") or "")
        ident = str(c.get("identity") or "").strip()
        if not gw or str(c.get("user_agent") or "").startswith(OWN_GATEWAY_UA_PREFIX):
            continue
        tag = str(c.get("secondary_tag") or c.get("name") or "").strip()[:40]
        if ident:
            db("UPDATE gateway_tags SET identity=? WHERE gateway=?", (ident, gw), write=True)
        if tag:
            db(
                "INSERT INTO gateway_tags(gateway, tag, identity, height, ts) VALUES(?,?,?,?,?) "
                "ON CONFLICT(gateway) DO UPDATE SET tag=excluded.tag, identity=excluded.identity, ts=excluded.ts",
                (gw, tag, ident, 0, ts),
                write=True,
            )


def gateway_names_by_address():
    """address -> {name, gateway, connected} for addresses running their own DATUM gateway.

    `name` is the operator's `pool_tag_secondary`: from a live share when Prime has seen one,
    otherwise from a block that gateway found. Empty until then; the caller decides what to show.
    """
    meta = state.get("prime_meta") or {}
    tags = {}
    # Highest height last so a later find (or a live share written at height 0, then a
    # real block) wins when the same payout address has used more than one gateway key.
    for r in db("SELECT gateway, tag, identity, height FROM gateway_tags ORDER BY height ASC") or []:
        ident = str(r["identity"] or "").strip()
        if ident:
            tags[ident] = (str(r["gateway"]), str(r["tag"] or "").strip())
    out = {}
    live = set()
    for c in meta.get("clients") or []:
        ident = str(c.get("identity") or "").strip()
        if not ident or str(c.get("user_agent") or "").startswith(OWN_GATEWAY_UA_PREFIX):
            continue
        gw = str(c.get("gateway") or "")
        live.add(ident)
        live_tag = str(c.get("secondary_tag") or c.get("name") or "").strip()[:40]
        db_gw, db_tag = tags.get(ident, ("", ""))
        # Same payout address is the same operator even if they rotated the gateway key.
        name = live_tag or db_tag
        out[ident] = {"name": name, "gateway": gw or db_gw, "connected": True}
    for ident, (gw, tag) in tags.items():
        if ident not in live:
            out[ident] = {"name": tag, "gateway": gw, "connected": False}
    return out


def stamp_gateway_names(rows, names=None):
    """Attach `gateway_name` / `gateway` from `gateway_names_by_address` onto miner dicts."""
    names = names if names is not None else gateway_names_by_address()
    for rec in rows or []:
        who = names.get(rec.get("address") or "")
        if not who:
            continue
        rec["gateway_name"] = who.get("name") or ""
        rec["gateway"] = who.get("gateway") or ""
        rec["gateway_connected"] = bool(who.get("connected"))
    return rows


def solo_blocks_rows(limit=50):
    rows = db(
        "SELECT height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc FROM solo_blocks"
        " ORDER BY height DESC LIMIT ?",
        (limit,),
    )
    return [dict(r) for r in rows]


def _solo_is_hashing(m):
    """Connected or proving work right now — not a leftover identity from the solo book."""
    return float(m.get("hashrate_ghs") or 0) > 1e-9 or int(m.get("workers") or 0) > 0


def _collect_solo():
    """Merge the standalone solo gateways. Includes idle book identities.

    The gateways' /solo.json is a lifetime scoreboard (who ever submitted) plus whoever
    is connected. The public table only wants the hashing ones; the per-address page
    still needs the book so a finder can see their own row after they disconnect.
    """
    endpoints, miners = [], {}
    hashrate_ghs = 0.0
    for gw in SOLO_APIS:
        doc = {}
        try:
            raw = curl(gw["url"].rstrip("/") + "/solo.json", timeout=3)
            doc = json.loads(raw) if raw else {}
        except Exception:
            doc = {}
        up = bool(doc.get("mode") == "solo")
        ghs = float(doc.get("hashrate") or 0) / 1e9
        hashrate_ghs += ghs
        live_n = 0
        for m in doc.get("miners") or []:
            ident = m.get("identity") or ""
            if not ident:
                continue
            hr = float(m.get("hashrate") or 0) / 1e9
            workers = int(m.get("workers") or 0)
            e = miners.setdefault(ident, _solo_row(ident))
            e["hashrate_ghs"] += hr
            e["workers"] += workers
            e["work"] += int(m.get("work") or 0)
            e["shares"] += int(m.get("shares") or 0)
            e["blocks"] += int(m.get("blocks") or 0)
            e["best_diff"] = max(e["best_diff"], int(m.get("best_diff") or 0))
            e["via"] = gw.get("name") or "stratum"
            e["fee_percent"] = (doc.get("fee_bps") or 0) / 100.0
            if hr > 1e-9 or workers > 0:
                live_n += 1
        endpoints.append(
            {
                "name": gw.get("name") or doc.get("profile") or "solo",
                "host": STRATUM_HOST,
                "port": gw.get("port"),
                "online": up,
                "fee_percent": (doc.get("fee_bps") or 0) / 100.0,
                "height": doc.get("height") or 0,
                "template_age_s": doc.get("template_age_s"),
                "vardiff": doc.get("vardiff") or {},
                "hashrate_ghs": ghs,
                "miners": live_n,
            }
        )
    return endpoints, miners, hashrate_ghs


def solo_payload():
    """Everything the UI shows about solo.

    Solo is served only by standalone gateways: they build their own templates, pay the
    finder directly in the coinbase, and never talk to Prime. Nothing here touches the
    TIDES window and nothing here is ever owed.
    """
    endpoints, miners, hashrate_ghs = _collect_solo()
    blocks = solo_blocks_rows()
    # The chain is the authority on who found what; the gateways' own counters are only a
    # live view and reset if an instance is replaced.
    for b in blocks:
        if b["finder"] in miners:
            miners[b["finder"]]["blocks_onchain"] = miners[b["finder"]].get("blocks_onchain", 0) + 1
    live = [m for m in miners.values() if _solo_is_hashing(m)]
    live.sort(key=lambda m: (-m["hashrate_ghs"], -m["work"]))
    return {
        "enabled": any(e["online"] for e in endpoints),
        "fee_percent": next((e["fee_percent"] for e in endpoints if e["online"]), 2.5),
        "endpoints": endpoints,
        "hashrate_ghs": hashrate_ghs,
        "miners": live,
        "miner_count": len(live),
        "blocks": blocks,
        "blocks_found": len(blocks),
        "ts": int(time.time()),
    }


def _solo_row(ident):
    return {
        "address": ident,
        "hashrate_ghs": 0.0,
        "workers": 0,
        "work": 0,
        "shares": 0,
        "blocks": 0,
        "blocks_onchain": 0,
        "best_diff": 0,
        "via": "",
        "fee_percent": 0.0,
    }


def _addr_key(addr):
    """Bech32 is case-insensitive; base58 is not. Solo identities are already folded."""
    a = (addr or "").strip()
    if a.lower().startswith(("bc1", "tb1", "bcrt1")):
        return a.lower()
    return a


def _solo_row_for(miners, addr):
    key = _addr_key(addr)
    return next((m for m in miners if _addr_key(m.get("address")) == key), None)


def solo_miner_payload(addr):
    endpoints, miners, _hr = _collect_solo()
    blocks = solo_blocks_rows()
    me = miners.get(addr) or _solo_row_for(miners.values(), addr)
    if me:
        me = dict(me)
        me["blocks_onchain"] = sum(1 for b in blocks if _addr_key(b.get("finder")) == _addr_key(addr))
    key = _addr_key(addr)
    return {
        "address": addr,
        "found": me is not None,
        "solo": me or _solo_row(addr),
        "blocks": [b for b in blocks if _addr_key(b.get("finder")) == key],
        "endpoints": endpoints,
        "ts": int(time.time()),
    }


def pool_fee_script():
    """The scriptPubKey the pool's own outputs pay to.

    The script, not the address: the same script renders as a different address on a
    different network, and the solo gateways publish exactly this hex as `fee_script`.
    """
    return (((prime_doc().get("pool") or {}).get("script")) or CONF.get("payout_script") or "").lower()


def record_solo_block(height, blockhash, blk, tx0, coinbase_text):
    """A block found by a solo miner: logged, never settled.

    The finder is the value output that is not the pool's fee. Matching on the pool's
    script rather than on output order, because the order is the gateway's choice and not
    something the chain guarantees.
    """
    vouts = tx0.get("vout") or []
    reward = sum(float(v.get("value") or 0) for v in vouts)
    fee_spk = pool_fee_script()
    finder, fee_btc = "", 0.0
    for v in vouts:
        val = float(v.get("value") or 0)
        if val <= 0:
            continue
        spk = v.get("scriptPubKey") or {}
        if fee_spk and str(spk.get("hex") or "").lower() == fee_spk:
            fee_btc += val
            continue
        a = spk.get("address") or (spk.get("addresses") or [None])[0]
        if a and not finder:
            finder = a
    db(
        "INSERT OR REPLACE INTO solo_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase)"
        " VALUES(?,?,?,?,?,?,?,?)",
        (height, blockhash, blk.get("time"), reward, finder, fee_btc, reward - fee_btc, coinbase_text[:200]),
        write=True,
    )
    print("solo_block", height, finder or "?", "reward", round(reward, 8), "fee", round(fee_btc, 8), flush=True)


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
        vouts = tx0.get("vout") or []
        if SOLO_TAG in text:
            record_solo_block(height, h, blk, tx0, text)
            continue
        if COINBASE_TAG not in text:
            continue
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
            refresh_gateway_identities()
            learn_gateway_tags()
            warm_coinbase_splits()
        except Exception as e:
            print("scan", e, flush=True)
        time.sleep(max(0.5, 10 - (time.time() - t0)))


def warm_coinbase_splits(budget=8):
    """Fetch (and persist) the coinbase split of a few found blocks per pass, so a miner's
    lifetime Paid/Immature never has to walk the whole list against the node on demand."""
    if NO_WRITE:
        return
    rows = db("SELECT hash FROM found_blocks ORDER BY height DESC") or []
    n = 0
    for r in rows:
        h = r["hash"]
        if not h or h in _cb_split_cache:
            continue
        if coinbase_splits(h) is None:
            break
        n += 1
        if n >= budget:
            break


_node_info_cache = {"ts": 0.0, "doc": None}


def node_info():
    now = time.time()
    if _node_info_cache["doc"] and now - _node_info_cache["ts"] < 5:
        return _node_info_cache["doc"]
    mi = rpc("getmininginfo") or {}
    bi = rpc("getblockchaininfo") or {}
    doc = {
        "height": mi.get("blocks") or bi.get("blocks"),
        "difficulty": mi.get("difficulty"),
        "networkhashps": mi.get("networkhashps"),
        "chain": bi.get("chain"),
    }
    _node_info_cache["ts"] = now
    _node_info_cache["doc"] = doc
    return doc


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


# Bitcoin (and this BLAKE2b fork) measures block proof as difficulty × 2^32 hashes.
# getmininginfo.networkhashps is work/time over the last 120 blocks, so it already
# embeds however fast those blocks arrived. Mixing that nethash with a 600s target
# spacing double-counts a hot network: TTF comes out too long and luck too high.
POW2_32 = float(1 << 32)
RETARGET_BLOCKS = 2016
_luck_cache = {
    "ts": 0.0,
    "expected": None,
    "nfound": 0,
    "full_ts": 0.0,
    "last_ts": 0,
    "last_hs": 0.0,
    "sum_expected": 0.0,
    "since_ts": 0,
    "epoch_sig": None,
}
# Difficulty at each retarget height, from the node's block headers. Immutable once a
# boundary is buried, so it is kept for the life of the process.
_epoch_cache = {"by_height": {}, "list": [], "list_ts": 0.0, "list_first": None, "list_tip_epoch": None}


def hashes_per_block(difficulty):
    try:
        d = float(difficulty or 0)
    except (TypeError, ValueError):
        return 0.0
    return d * POW2_32 if d > 0 else 0.0


def _epoch_header(height):
    """(time, difficulty) of the block at `height`, or None when the node cannot answer."""
    hit = _epoch_cache["by_height"].get(height)
    if hit:
        return hit
    h = rpc("getblockhash", [int(height)])
    if not h:
        return None
    bh = rpc("getblockheader", [h])
    if not bh or bh.get("difficulty") is None:
        return None
    out = (int(bh.get("time") or 0), float(bh["difficulty"]))
    _epoch_cache["by_height"][height] = out
    return out


def difficulty_epochs(first_ts, tip_height):
    """Sorted [(start_ts, difficulty)] for every retarget epoch from the one containing
    `first_ts` to the tip. Empty when the node is unreachable (caller falls back to the
    current difficulty for everything).

    Difficulty on this chain has moved by whole multiples between retargets (the fork
    reset it and it has been climbing 4× a step since), so charging a week-old hash at
    today's difficulty understates expected blocks by an order of magnitude.
    """
    try:
        tip = int(tip_height or 0)
    except (TypeError, ValueError):
        tip = 0
    if tip <= 0:
        return _epoch_cache["list"]
    tip_epoch = tip // RETARGET_BLOCKS
    now = time.time()
    if (
        _epoch_cache["list"]
        and _epoch_cache["list_tip_epoch"] == tip_epoch
        and _epoch_cache["list_first"] is not None
        and _epoch_cache["list_first"] <= int(first_ts or 0)
        and now - _epoch_cache["list_ts"] < 300
    ):
        return _epoch_cache["list"]
    epochs = []
    h = tip_epoch * RETARGET_BLOCKS
    while h >= 0:
        hdr = _epoch_header(h)
        if hdr is None:
            return _epoch_cache["list"]  # node hiccup: keep whatever we had
        epochs.append((hdr[0], hdr[1]))
        # Stop once this epoch started before the first sample; it covers the rest.
        if hdr[0] <= int(first_ts or 0) or h == 0:
            break
        h -= RETARGET_BLOCKS
    epochs.sort()
    _epoch_cache.update({"list": epochs, "list_ts": now, "list_first": int(first_ts or 0), "list_tip_epoch": tip_epoch})
    return epochs


def _difficulty_at(epochs, ts, fallback):
    d = None
    for start, diff in epochs:
        if ts >= start:
            d = diff
        else:
            break
    return d if d is not None else fallback


def _luck_add_rows(rows, last_ts, last_hs, total, epochs, fallback_diff):
    """Accumulate expected blocks: Σ hashrate·dt / (difficulty(t)·2^32), difficulty
    taken from the retarget epoch each interval fell in."""
    for r in rows or []:
        ts = int(r["ts"] or 0)
        hs = float(r["hr_ghs"] or 0) * 1e9
        if last_ts:
            dt = ts - last_ts
            if 0 < dt < 3600:
                need = hashes_per_block(_difficulty_at(epochs, last_ts, fallback_diff))
                if need > 0:
                    total += last_hs * dt / need
        last_ts, last_hs = ts, hs
    return last_ts, last_hs, total


def expected_blocks_from_samples(difficulty, tip_height=None):
    """Expected pool blocks over pool_samples: ∫ pool_hashrate dt / (difficulty(t) × 2^32),
    with difficulty(t) from the chain's retarget history, not just today's value.

    Returns (expected, since_ts): since_ts is the first sample integrated, so the caller
    can count found blocks over the same span. Incremental after the first full pass; a
    full rescan once an hour (samples are pruned at 7 days, so luck is a rolling week).
    """
    need = hashes_per_block(difficulty)
    if need <= 0:
        return None, 0
    now = time.time()
    if now - _luck_cache["ts"] < 30 and _luck_cache["expected"] is not None:
        return _luck_cache["expected"], _luck_cache["since_ts"]
    first = db("SELECT MIN(ts) AS t FROM pool_samples", one=True)
    since_ts = int(first["t"]) if first and first["t"] else 0
    if since_ts <= 0:
        return None, 0
    epochs = difficulty_epochs(since_ts, tip_height)
    sig = (len(epochs), epochs[0] if epochs else None)
    last_ts = int(_luck_cache.get("last_ts") or 0)
    last_hs = float(_luck_cache.get("last_hs") or 0)
    total = float(_luck_cache.get("sum_expected") or 0)
    full_age = now - float(_luck_cache.get("full_ts") or 0)
    # A fresh epoch list can re-price old intervals (first successful RPC after a start
    # with the node down), so any change in it forces a full pass.
    if last_ts <= 0 or full_age > 3600 or sig != _luck_cache.get("epoch_sig") or since_ts != _luck_cache.get("since_ts"):
        last_ts, last_hs, total = _luck_add_rows(
            db("SELECT ts, hr_ghs FROM pool_samples ORDER BY ts") or [], 0, 0.0, 0.0, epochs, difficulty
        )
        _luck_cache["full_ts"] = now
    else:
        last_ts, last_hs, total = _luck_add_rows(
            db("SELECT ts, hr_ghs FROM pool_samples WHERE ts > ? ORDER BY ts", (last_ts,)) or [],
            last_ts,
            last_hs,
            total,
            epochs,
            difficulty,
        )
    if last_ts <= 0:
        return None, 0
    _luck_cache.update({
        "ts": now,
        "expected": total,
        "last_ts": last_ts,
        "last_hs": last_hs,
        "sum_expected": total,
        "since_ts": since_ts,
        "epoch_sig": sig,
    })
    return total, since_ts


def luck_and_ttf(pool_hr_ghs, net_hs, difficulty, first_ts=None, tip_height=None):
    """(share, ttf_s, nfound, expected, luck, interval, luck_found, luck_since_ts).

    nfound is every block the pool has found; luck compares only the blocks found
    inside the span the expected figure integrates (luck_found since luck_since_ts)."""
    pool_hs = float(pool_hr_ghs or 0) * 1e9
    need = hashes_per_block(difficulty)
    net_hs = float(net_hs or 0)
    if net_hs <= 0 and need:
        net_hs = need / 600.0
    share = (pool_hs / net_hs) if net_hs else 0.0
    # Mean time to a pool block at the current target, not 600 / share.
    ttf_s = (need / pool_hs) if pool_hs > 0 and need > 0 else None
    found = db("SELECT COUNT(*) AS n FROM found_blocks", one=True)
    nfound = int(found["n"]) if found else 0
    expected, since_ts = expected_blocks_from_samples(difficulty, tip_height)
    if expected is None:
        since_ts = int(first_ts or time.time())
        elapsed = max(0, int(time.time()) - since_ts)
        expected = (elapsed * pool_hs / need) if need > 0 and pool_hs > 0 else 0.0
    if since_ts:
        in_span = db("SELECT COUNT(*) AS n FROM found_blocks WHERE ts >= ?", (since_ts,), one=True)
        luck_found = int(in_span["n"]) if in_span else nfound
    else:
        luck_found = nfound
    luck = (luck_found / expected * 100.0) if expected and expected > 0.01 else None
    interval = (need / net_hs) if net_hs > 0 and need > 0 else None
    return share, ttf_s, nfound, expected, luck, interval, luck_found, since_ts


OWN_GATEWAY_UA_PREFIX = "lazarus-gateway/"


def _gateway_row(c):
    ua = str(c.get("user_agent") or "")
    tag = str(c.get("secondary_tag") or c.get("name") or "").strip()[:40]
    return {
        "id": c.get("id"),
        "gateway": c.get("gateway"),
        "user_agent": ua,
        "generation": c.get("generation"),
        # The pool's own public stratum connects to Prime like anyone else's gateway.
        "own": ua.startswith(OWN_GATEWAY_UA_PREFIX),
        "fee_path": str(c.get("fee_path") or "").lower(),
        "identity": c.get("identity") or "",
        "secondary_tag": tag,
        "name": tag,
        "connected_s": int(c.get("connected_s") or 0),
        "accepted": int(c.get("accepted") or 0),
        "rejected": int(c.get("rejected") or 0),
        "last_reject": c.get("last_reject") or "",
        "last_share_s": c.get("last_share_s"),
        "work": int(c.get("work") or 0),
        "coinbasers": int(c.get("coinbasers") or 0),
        "block_candidates": int(c.get("block_candidates") or 0),
        "offline": bool(c.get("offline")),
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
    return apply_owed_settlement({
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
    })


def prime_summary():
    """The Prime as the UI shows it: identity, health, gateways, window, blocks, owed."""
    _by, meta = fetch_prime_window()
    pool = meta.get("pool") or {}
    totals = meta.get("totals") or {}
    clients = [_gateway_row(c) for c in meta.get("clients") or []]
    clients, log_found = _merge_persistent_gateway_finds(clients)
    known_tags = {
        str(r["gateway"]): str(r["tag"] or "").strip()
        for r in db("SELECT gateway, tag FROM gateway_tags") or []
        if r["tag"]
    }
    for g in clients:
        if g.get("own") or g.get("secondary_tag"):
            continue
        tag = known_tags.get(str(g.get("gateway") or ""))
        if tag:
            g["secondary_tag"] = tag
            g["name"] = tag
    blocks = [_block_row(b) for b in meta.get("blocks") or []]
    blocks.sort(key=lambda b: -(b["height"] or 0))
    pool_addr = pool.get("address") or ""
    for b in blocks:
        miner_to_pool = sum(int(o.get("sats") or 0) for o in b["split"] if o.get("address") == pool_addr)
        b["miner_to_pool_sats"] = miner_to_pool
        b["fee_sats"] = max(0, int(b.get("pool_sats") or 0) - miner_to_pool)
    try:
        fee_bps = _bps_or(pool.get("fee_bps"), POOL_FEE * 100)
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
        "stratum_fee_bps": _bps_or(meta.get("stratum_fee_bps"), fee_bps),
        "datum_rebate_bps": int(meta.get("datum_rebate_bps") or 0),
        "solo_rebate_bps": int(meta.get("solo_rebate_bps") or 0),
        "rebate_owed_sats": int(meta.get("rebate_owed_sats") or 0),
        "sample_rebate_sats": int(meta.get("sample_rebate_sats") or 0),
        "datum_work_percent": float(meta.get("datum_work_percent") or 0),
        "stratum_work_percent": float(meta.get("stratum_work_percent") or 0),
        "datum_uplift_percent": float(meta.get("datum_uplift_percent") or 0),
        "datum_miners": int(meta.get("datum_miners") or 0),
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
            "block_candidates": int(log_found or totals.get("block_candidates") or 0),
            "blocks_submitted": int(totals.get("blocks_submitted") or 0),
        },
        "owed_sats": max(
            0,
            int(meta.get("owed_sats") or 0)
            - sum(int(b.get("owed_sats") or 0) for b in blocks if b.get("owed_resolved")),
        ),
        "gateways": clients,
        "gateways_online": sum(1 for g in clients if not g.get("offline")),
        "gateways_remote": sum(1 for g in clients if not g["own"] and not g.get("offline")),
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
                "window_shares": int(info.get("window_shares") or info.get("credits") or 0),
                "fee_path": info.get("fee_path") or "",
                "hr_ghs": float(info.get("hr_ghs") or 0),
                "last_share_s": info.get("last_share_s"),
            })
    miners.sort(key=lambda m: -m["sats"])
    miner_sats = sum(m["sats"] for m in miners)
    unpaid = [
        {
            "address": addr,
            "work": int(info.get("window_work") or 0),
            "share_percent": float(info.get("window_percent") or 0),
            "carry_sats": int(info.get("carry_sats") or 0),
            "reason": "under the payout floor · carried forward" if info.get("payable") else "address not payable",
        }
        for addr, info in by.items()
        if int(info.get("window_sats") or 0) <= 0 and (int(info.get("window_work") or 0) > 0 or int(info.get("carry_sats") or 0) > 0)
    ]
    carry_total = int(meta.get("carry_total_sats") or 0)
    carry_paid = int(meta.get("sample_carry_paid_sats") or 0)
    pool_sats = int(meta.get("sample_pool_sats") or max(0, value - miner_sats))
    fee_sats = int(meta.get("sample_fee_sats") or 0)
    pool_addr = (meta.get("pool") or {}).get("address") or ""
    # Who each output belongs to, for the donut's labels: an address on the DATUM path is
    # running its own gateway. The operator's secondary coinbase tag (pool_tag_secondary)
    # comes from a live share when Prime has seen one, otherwise from a block they found.
    names = gateway_names_by_address()
    outputs = []
    for m in miners:
        o = dict(m, to="miner")
        who = names.get(m["address"])
        if who and m.get("fee_path") == "datum":
            o["name"] = who.get("name") or ""
            o["gateway_name"] = who.get("name") or ""
            o["gateway"] = who.get("gateway") or ""
            o["gateway_connected"] = bool(who.get("connected"))
        outputs.append(o)
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
        # Carry from earlier blocks riding in these outputs (comes out of the pool's
        # remainder, which is why pool_sats can be under fee_sats), and what is still held.
        "carry_paid_sats": carry_paid,
        "carry_total_sats": carry_total,
        "carry_holders": int(meta.get("carry_holders") or 0),
        "deferred_sats": int(meta.get("sample_deferred_sats") or 0),
        # DATUM bonus this split credits to DATUM miners' balances. Not one of the outputs
        # above: it rides on their next output, so it comes out of the pool's remainder later.
        "rebate_sats": int(meta.get("sample_rebate_sats") or 0),
        "rebate_percent": int((meta.get("pool") or {}).get("datum_rebate_bps") or 0) / 100.0,
        "rebate_owed_sats": int(meta.get("rebate_owed_sats") or 0),
        "pool_address": pool_addr,
        "window_multiple": meta.get("window_multiple") or 8,
        "window_fill_percent": meta.get("fill_percent") or 0,
        "miners": outputs,
        "unpaid": unpaid,
    }


def _hasher_path(m):
    """Live arrival path for one miner row: own DATUM gateway vs public stratum."""
    via = str(m.get("via") or "").lower()
    if via in ("prime", "gateway"):
        return "datum"
    return "stratum"


def _path_hashrate(miners):
    """Credited live hashrate split by hasher path. One row per payout address.

    Same identity-level credited rate as the pool ticker (Prime ledger), classified by
    whether that address is hashing through its own DATUM gateway or the public stratum
    right now. Not TIDES window share, and not overflow.datum_hs (that meter is for relay).
    """
    seen = set()
    datum = stratum = 0.0
    n_datum = n_stratum = 0
    for m in miners or []:
        a = m.get("address") or ""
        if not a or a in seen:
            continue
        seen.add(a)
        hr = float(m.get("credited_hr_ghs") or 0)
        if hr < 1e-6:
            hr = float(m.get("hr_ghs") or 0)
        if _hasher_path(m) == "datum":
            datum += hr
            n_datum += 1
        else:
            stratum += hr
            n_stratum += 1
    return datum, stratum, n_datum, n_stratum


_DATUM_GW_MIN_PCT = 0.5
_DATUM_GW_HASHING_S = 180


def _datum_gateway_slices(gateways, datum_hr_ghs):
    """Live DATUM hashrate by named gateway. Percents are of DATUM hashrate, not the pool.

    House public stratum (`own` / fee_path stratum) is excluded. Slice weights are each
    hashing gateway's session work per connected second (average rate since connect), then
    scaled so they sum to the same credited `datum_hr_ghs` as the path pie. Every gateway
    at or above 0.5% of DATUM hashrate keeps its own slice; the rest fold into Other.
    """
    try:
        datum = float(datum_hr_ghs or 0)
    except (TypeError, ValueError):
        datum = 0.0
    if datum <= 1e-12:
        return [], 0
    buckets = {}
    for g in gateways or []:
        if g.get("own") or g.get("offline"):
            continue
        if str(g.get("fee_path") or "").lower() == "stratum":
            continue
        try:
            work = float(g.get("work") or 0)
            accepted = int(g.get("accepted") or 0)
            last = g.get("last_share_s")
            last_s = float(last) if last is not None else 1e9
            connected = max(float(g.get("connected_s") or 0), 30.0)
        except (TypeError, ValueError):
            continue
        if work <= 0 or accepted <= 0 or last_s >= _DATUM_GW_HASHING_S:
            continue
        tag = str(g.get("secondary_tag") or g.get("name") or "").strip()
        gw = str(g.get("gateway") or "")
        key = tag.lower() if tag else (gw or f"id:{g.get('id')}")
        b = buckets.get(key)
        if b is None:
            b = {"name": tag, "gateway": gw, "weight": 0.0, "sessions": 0}
            buckets[key] = b
        b["weight"] += work / connected
        b["sessions"] += 1
        if tag:
            b["name"] = tag
        if gw and not b["gateway"]:
            b["gateway"] = gw
    items = sorted(buckets.values(), key=lambda x: -x["weight"])
    total_w = sum(x["weight"] for x in items)
    if total_w <= 0:
        return [], 0
    n = len(items)
    head, tail = [], []
    for x in items:
        if 100.0 * x["weight"] / total_w >= _DATUM_GW_MIN_PCT:
            head.append(x)
        else:
            tail.append(x)
    if tail:
        head.append(
            {
                "name": "",
                "gateway": "",
                "weight": sum(x["weight"] for x in tail),
                "sessions": sum(int(x["sessions"]) for x in tail),
                "other": True,
            }
        )
    items = head
    out = []
    for x in items:
        frac = x["weight"] / total_w
        out.append(
            {
                "name": x["name"],
                "gateway": x.get("gateway") or "",
                "hr_ghs": datum * frac,
                "percent": 100.0 * frac,
                "sessions": int(x["sessions"]),
                "other": bool(x.get("other")),
            }
        )
    return out, n


def pool_payload():
    node = node_info()
    miners = online_miners()
    seen_addr = set()
    pool_hr = 0.0
    for m in miners:
        a = m.get("address") or ""
        if not a or a in seen_addr:
            continue
        seen_addr.add(a)
        credited = float(m.get("credited_hr_ghs") or 0)
        pool_hr += credited if credited > 1e-6 else float(m.get("hr_ghs") or 0)
    _lby, _lpool = _ledger_hashrate()
    if _lpool > 1e-9:
        pool_hr = _lpool
    elif pool_hr < 1e-9:
        pool_hr = state.get("pool_hr_ghs") or 0
    datum_hr, stratum_hr, datum_hr_miners, stratum_hr_miners = _path_hashrate(miners)
    split = datum_hr + stratum_hr
    if pool_hr > 1e-9 and split > 1e-9:
        scale = pool_hr / split
        datum_hr *= scale
        stratum_hr *= scale
    elif pool_hr > 1e-9 and split <= 1e-9:
        stratum_hr = pool_hr
        datum_hr = 0.0
    path_den = datum_hr + stratum_hr
    datum_hr_pct = (100.0 * datum_hr / path_den) if path_den > 1e-12 else 0.0
    stratum_hr_pct = (100.0 * stratum_hr / path_den) if path_den > 1e-12 else 0.0
    online = len(seen_addr)
    net = float(node.get("networkhashps") or 0)
    first = db("SELECT MIN(first_ts) AS t FROM miners", one=True)
    first_ts = first["t"] if first and first["t"] else state.get("ts")
    share, ttf_s, nfound, expected, luck, interval, luck_found, luck_since = luck_and_ttf(
        pool_hr, net, node.get("difficulty"), first_ts, node.get("height")
    )
    # Sessions (rigs) vs. distinct payout addresses: the ticker names both. Every public
    # stratum session is one worker; an address seen only through its own gateway is one
    # worker too, since Prime cannot see behind that gateway.
    # Sessions that have not authorized an address yet are not counted (the miners table
    # skips them too), so this agrees with the rows below it.
    named = [m for m in miners if m.get("address")]
    stratum_addrs = {m.get("address") for m in named if (m.get("via") or "stratum") == "stratum"}
    workers = sum(1 for m in named if (m.get("via") or "stratum") == "stratum") + len(
        {m.get("address") for m in named if (m.get("via") or "stratum") != "stratum"} - stratum_addrs
    )
    # Daily estimate at the current target (same work units as TTF), not 144 × share.
    # 144 assumes 10-minute blocks; this chain has been running much faster than that.
    need = hashes_per_block(node.get("difficulty"))
    blocks_per_day = (86400.0 / ttf_s) if ttf_s else 0.0
    est_btc_day = blocks_per_day * SUBSIDY * (1 - POOL_FEE / 100.0)
    ths_btc_day = ((1e12 * 86400.0 / need) * SUBSIDY) if need else 0.0
    known = db("SELECT COUNT(*) AS n FROM miners", one=True)
    since = int(time.time()) - 86400
    hist = db(
        "SELECT (ts / 60) * 60 AS ts, AVG(hr_ghs) AS hr_ghs, AVG(miners) AS miners "
        "FROM pool_samples WHERE ts > ? GROUP BY (ts / 60) ORDER BY 1",
        (since,),
    )
    win = tides_window_snapshot()
    prime = prime_summary()
    # /api/pool is polled every 10s. Full coinbase splits for ~100 blocks were ~900 KB of
    # that payload (and gzipped on every request). Found-by-Lazarus reads /api/payouts.
    prime_pub = {k: v for k, v in prime.items() if k != "blocks"}
    nblocks = int(win["window_multiple"] or 8)
    fill = win["window_fill_percent"]
    datum_fee = prime["fee_bps"] / 100.0 if prime.get("reachable") else POOL_FEE
    stratum_fee = prime["stratum_fee_bps"] / 100.0 if prime.get("reachable") else STRATUM_FEE
    rebate_pct = (prime.get("datum_rebate_bps") or 0) / 100.0
    uplift_pct = float(prime.get("datum_uplift_percent") or 0)
    if datum_fee == 0 and stratum_fee == 0:
        fee_clause = "100%, no fee"
    elif datum_fee == stratum_fee:
        fee_clause = f"{100-datum_fee:g}% to miners, {datum_fee:g}% fee"
    else:
        fee_clause = f"{datum_fee:g}% fee through your own DATUM gateway, {stratum_fee:g}% on the public stratum"
        if rebate_pct > 0:
            fee_clause += f"; {rebate_pct:g} point{'s' if rebate_pct != 1 else ''} of the stratum fee is credited to DATUM miners"
    payout = (
        f"A found block pays the TIDES window in its coinbase ({fee_clause}): "
        f"{nblocks} network-blocks of accepted work, currently {fill:.0f}% full. "
        f"Hashrate is not your cut — a new rig starts near 0% and ramps as its work enters and older work ages out."
    )
    datum_gateways, datum_gateway_count = _datum_gateway_slices(prime.get("gateways") or [], datum_hr)
    return {
        "name": "Lazarus",
        "tagline": "Proverbs 11:1",
        "fee_percent": POOL_FEE,
        "fees": {
            "datum_percent": datum_fee,
            "stratum_percent": stratum_fee,
            # Of the stratum fee, this many points are credited to DATUM miners' balances on
            # every found block (pro rata by DATUM work) and paid with their next output that
            # clears the floor; the pool keeps stratum_percent − this.
            "datum_rebate_percent": rebate_pct,
            "solo_rebate_percent": (prime.get("solo_rebate_bps") or 0) / 100.0,
            "rebate_owed_btc": (prime.get("rebate_owed_sats") or 0) / 1e8,
            "sample_rebate_btc": (prime.get("sample_rebate_sats") or 0) / 1e8,
            # What the pot means to a DATUM miner: percent above its proportional share that
            # DATUM work earns right now (rebate × stratum work ÷ DATUM work in the window).
            "datum_uplift_percent": uplift_pct,
            "datum_work_percent": float(prime.get("datum_work_percent") or 0),
            "stratum_work_percent": float(prime.get("stratum_work_percent") or 0),
            "datum_miners": int(prime.get("datum_miners") or 0),
            "note": "The fee is taken per miner from that miner's window share, by the path the work arrived on. Switching paths keeps the accepted work."
            + (
                f" {rebate_pct:g}% of stratum work's value is credited to DATUM miners on every block, pro rata by DATUM work, and paid with their next output."
                if rebate_pct > 0
                else ""
            ),
        },
        "stratum": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "stratum_asic": f"stratum+tcp://{STRATUM_HOST}:{STRATUM_PORT}",
        "host": STRATUM_HOST,
        "port": STRATUM_PORT,
        "pool_hr_ghs": pool_hr,
        # Live credited hashrate by hasher path (DATUM gateway vs public stratum). Percents
        # sum to 100 of pool_hr_ghs. Window work split is fees.datum_work_percent.
        "datum_hr_ghs": datum_hr,
        "stratum_hr_ghs": stratum_hr,
        "datum_hr_percent": datum_hr_pct,
        "stratum_hr_percent": stratum_hr_pct,
        "datum_hr_miners": datum_hr_miners,
        "stratum_hr_miners": stratum_hr_miners,
        # DATUM slice only: hashing remote gateways, percents of datum_hr_ghs.
        "datum_gateways": datum_gateways,
        "datum_gateway_count": datum_gateway_count,
        "miners_online": online,
        "workers_online": max(workers, online),
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
        "ths_btc_day": ths_btc_day,
        "ths_btc_day_datum": ths_btc_day * (1 - datum_fee / 100.0),
        "ths_btc_day_stratum": ths_btc_day * (1 - stratum_fee / 100.0),
        # DATUM including the rebate credit at today's work split (0 uplift when it is off).
        "ths_btc_day_datum_bonus": ths_btc_day * (1 - datum_fee / 100.0) * (1 + uplift_pct / 100.0),
        "ttf_seconds": ttf_s,
        "block_interval_seconds": interval,
        "blocks_found": nfound,
        "blocks_expected": expected,
        # Luck is found / expected over the span the hashrate samples cover (they are kept
        # for a week), at the difficulty in force when each hash was done.
        "luck_percent": luck,
        "luck_blocks_found": luck_found,
        "luck_since_ts": luck_since,
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
        "prime": prime_pub,
        # Network-share valve on the house stratum: over the line, new miners are relayed
        # to other BLAKE2b pools and paid there. Absent when the gateway lacks the feature.
        "overflow": overflow_doc().get("overflow"),
        "payouts_onchain": True,
        "explorer": EXPLORER,
        "updated": state.get("ts") or int(time.time()),
        "history": [
            {
                "ts": int(r["ts"]),
                "hr_ghs": round(float(r["hr_ghs"] or 0), 3),
                "miners": int(round(float(r["miners"] or 0))),
            }
            for r in (hist or [])
        ],
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
    # One sample row per worker per scrape: the address's rate at a scrape is the SUM over
    # its workers; the minute bucket then averages the scrapes that fell in it. (A plain
    # AVG(hr_ghs) here would read as the average worker, not the address.)
    hist = db(
        "SELECT (ts / 60) * 60 AS ts, AVG(tot) AS hr FROM "
        "(SELECT ts, SUM(hr_ghs) AS tot FROM samples WHERE address=? AND ts > ? GROUP BY ts) "
        "GROUP BY (ts / 60) ORDER BY 1",
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
    net_hs = float(node.get("networkhashps") or 0)
    net_ghs = net_hs / 1e9
    share = (hr / net_ghs) if net_ghs else 0
    miner_need = hashes_per_block(node.get("difficulty"))
    miner_hs = float(hr or 0) * 1e9
    # Billed at the rate for the path this address's window work is on (2% public
    # stratum, 0.5% own gateway), not the DATUM rate for everyone.
    path_fee = _fee_percent_for_path(
        pinfo.get("fee_path") or ("stratum" if any((m.get("via") or "stratum") == "stratum" for m in recs) else "datum")
    )
    gross_day = ((miner_hs * 86400.0 / miner_need) * SUBSIDY) if miner_need and miner_hs else 0.0
    est = gross_day * (1 - path_fee / 100.0)
    # The DATUM case for this address, at today's window split: 0% fee plus the rebate
    # uplift. For a stratum miner this is what switching gains; for a DATUM miner it is the
    # bonus already accruing. Zero when the rebate is off.
    _pm = state.get("prime_meta") or {}
    uplift_pct = float(_pm.get("datum_uplift_percent") or 0)
    datum_fee_pct = _fee_percent_for_path("datum")
    est_datum_day = gross_day * (1 - datum_fee_pct / 100.0) * (1 + uplift_pct / 100.0)
    est_bonus_day = gross_day * (1 - datum_fee_pct / 100.0) * (uplift_pct / 100.0)
    # Same denominator as the headline hashrate (Prime's pool_ghs), so "% of pool"
    # agrees with the ticker instead of a second sum over online sessions.
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
    _lby2, _lpool2 = _ledger_hashrate()
    if _lpool2 > 1e-9:
        pool_hr = _lpool2
    pool_hr = pool_hr or 1e-9
    contrib = min(1.0, hr / pool_hr) if pool_hr else 0
    tip = rpc("getblockcount") or 0
    # Every block the pool has found: Paid / Immature are lifetime totals. The list
    # itself is trimmed to the most recent 50 below.
    fb_rows = db("SELECT height, hash, ts FROM found_blocks ORDER BY height DESC") or []
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
        confs = max(0, int(tip) - int(fb["height"]) + 1) if tip and fb["height"] else 0
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
                # Coinbase outputs spend after 100 confirmations; the block itself is one.
                "confirmations": confs,
                # Spendable once the chain reaches height + 100 (status flips to paid then).
                "blocks_to_mature": max(0, int(fb["height"]) + MATURITY_CONFS - int(tip)) if tip and fb["height"] else MATURITY_CONFS,
            }
        )
        if st == "immature":
            immature_btc += amt
        else:
            paid_btc += amt
    immature_blocks = sum(1 for p in (payouts or []) if p.get("status") == "immature")
    if used_chain and payouts is not None:
        payouts = payouts[:50]
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
        payouts = [dict(r) for r in (payouts or [])]
        immature_blocks = sum(1 for p in payouts if p.get("status") == "immature")
        for p in payouts:
            confs = max(0, int(tip) - int(p["height"]) + 1) if tip and p.get("height") else 0
            p["confirmations"] = confs
            p["blocks_to_mature"] = max(0, int(p["height"]) + MATURITY_CONFS - int(tip)) if tip and p.get("height") else MATURITY_CONFS
    # Average hashrate over the last hour / day from the per-minute samples, so the
    # miner page can show a steadier figure than the instantaneous one.
    now_ts = int(time.time())
    _h1 = [float(r["hr"] or 0) for r in (hist or []) if int(r["ts"]) > now_ts - 3600]
    _h24 = [float(r["hr"] or 0) for r in (hist or [])]
    hr_1h = (sum(_h1) / len(_h1)) if _h1 else hr
    hr_24h = (sum(_h24) / len(_h24)) if _h24 else hr
    rw = db("SELECT work FROM round_work WHERE address=?", (address,), one=True)
    tw = db("SELECT COALESCE(SUM(work),0) AS s FROM round_work", one=True)
    my_work = float(rw["work"]) if rw else 0.0
    tot_work = float(tw["s"]) if tw else 0.0
    round_share = (my_work / tot_work) if tot_work else 0.0
    ttf_s = (miner_need / miner_hs) if miner_need and miner_hs else None
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
            "window_shares": int(pinfo.get("window_shares") or pinfo.get("credits") or 0),
        }]
    else:
        via = ""
    solo = _solo_for(address)
    if solo:
        shs = float(solo.get("hashrate_ghs") or 0) * 1e9
        solo["ttf_seconds"] = (miner_need / shs) if miner_need and shs else None
    # Sessions the gateway is relaying to another pool under this address. Not ours to
    # credit, but the miner looking itself up here deserves to see where its work went.
    relayed = [p for p in overflow_doc().get("proxied") or [] if _addr_key(p.get("address")) == _addr_key(address)]
    known = bool(stored or recs or pinfo.get("window_work") or life_a or solo or relayed)
    last_s = min((_share_age_s(m.get("last_share_s")) for m in recs), default=1e9)
    if pinfo.get("last_share_s") is not None:
        last_s = min(last_s, _share_age_s(pinfo.get("last_share_s"), missing=0.0))
    is_online = (credited > 1e-6 and last_s < 180) or any(
        float(m.get("hr_ghs") or 0) > 1e-6 and _share_age_s(m.get("last_share_s"), missing=0.0) < 180
        for m in recs
        if (m.get("via") or "") in ("stratum", "both", "prime", "gateway")
    ) or bool(solo and float(solo.get("hashrate_ghs") or 0) > 1e-6)
    best = float(stored["best_hr_ghs"] if stored and stored["best_hr_ghs"] is not None else (hr or 0))
    if best >= _PRIME_HR_CAP_GHS:
        best = hr
    win = tides_window_snapshot()
    out = {
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
        "window_shares": int(pinfo.get("window_shares") or pinfo.get("credits") or 0),
        "diff_acc": (recs[0].get("diff_acc", 0) if recs else 0) or (stored["diff_acc"] if stored and "diff_acc" in stored.keys() else 0),
        "first_seen": stored["first_ts"] if stored else None,
        "last_seen": stored["last_ts"] if stored else None,
        "best_hr_ghs": best if stored or recs else hr,
        "pool_contribution": contrib,
        "hashrate_pool_percent": contrib * 100.0,
        "est_btc_day": est,
        "est_btc_week": est * 7,
        "ttf_seconds": ttf_s,
        # The exact output primed would put in the next coinbase for this address. When
        # Prime knows the address at all, this is its figure even if that is zero (under
        # the payout floor: the earnings then accrue as carry instead of being paid). The
        # proportional estimate is only for an address Prime has not seen.
        "block_payout_btc": (int(pinfo.get("window_sats") or 0) / 1e8) if pinfo else SUBSIDY * (1 - path_fee / 100.0) * round_share,
        "next_block_exact": bool(pinfo),
        # Earned in earlier blocks, not yet placed; paid on top of the next output that
        # clears the floor. Zero once it has been paid.
        "carry_btc": int(pinfo.get("carry_sats") or 0) / 1e8,
        # DATUM rebate the next found block credits to this address's balance (0 for stratum
        # work, or when the rebate is off). Not inside block_payout_btc.
        "rebate_btc": int(pinfo.get("rebate_sats") or 0) / 1e8,
        # This hashrate through a DATUM gateway at today's split: 0% fee plus the rebate
        # uplift. `est_bonus_btc_day` is the uplift alone; `datum_uplift_percent` is the pool-wide
        # percent above proportional share that DATUM work earns right now.
        "est_datum_btc_day": est_datum_day,
        "est_bonus_btc_day": est_bonus_day,
        "datum_uplift_percent": uplift_pct,
        "datum_rebate_percent": int(_pm.get("datum_rebate_bps") or 0) / 100.0,
        "min_payout_btc": int(((state.get("prime_meta") or {}).get("pool") or {}).get("min_payout") or 0) / 1e8,
        "fee_path": pinfo.get("fee_path") or "",
        "fee_percent_path": path_fee,
        "est_fee_percent": path_fee,
        "gateway_name": "",
        "paid_btc": paid_btc,
        "unpaid_btc": 0.0,
        "immature_btc": immature_btc,
        "immature_blocks": immature_blocks,
        "tip_height": int(tip or 0),
        "maturity_confs": MATURITY_CONFS,
        "hr_1h_ghs": hr_1h,
        "hr_24h_ghs": hr_24h,
        "round_work": my_work,
        "round_share": round_share,
        "window_multiple": win["window_multiple"],
        "window_fill_percent": win["window_fill_percent"],
        "blocks_found": [dict(r) for r in (payouts or [])],
        "fee_percent": POOL_FEE,
        # Solo is a separate book: none of it is in `window_work` above, and none of it is
        # owed. Present so one address that mines both ways sees both on one page.
        "solo": solo,
        "relayed": relayed,
        "overflow": overflow_doc().get("overflow"),
        "history": [{"ts": int(r["ts"]), "hr_ghs": round(float(r["hr"] or 0), 3)} for r in (hist or [])],
    }
    names = gateway_names_by_address()
    stamp_gateway_names([out], names)
    stamp_gateway_names(out.get("workers") or [], names)
    return out


def _solo_for(address):
    """This address's solo standing, or None if it has never mined solo here."""
    try:
        doc = cached("solo", 3.0, solo_payload)
    except Exception:
        return None
    me = _solo_row_for(doc["miners"], address)
    key = _addr_key(address)
    blocks = [b for b in doc["blocks"] if _addr_key(b.get("finder")) == key]
    if not me and not blocks:
        return None
    row = dict(me or _solo_row(address))
    row["blocks_onchain"] = len(blocks)
    row["blocks_list"] = blocks
    return row


class PoolHTTPServer(ThreadingHTTPServer):
    # Default backlog is 5; a few open dashboards each open several API calls at once, and
    # the proxy in front holds a pool of connections open. When this queue overflows the
    # kernel leaves the proxy's SYNs unanswered and it reports 502.
    request_queue_size = 512
    daemon_threads = True

    def server_bind(self):
        """SO_REUSEPORT: several workers listen on one port and the kernel deals new
        connections out between them.

        The proxy sends every request to a single port, so one process was carrying the
        whole dashboard while its siblings on the other ports sat idle. Sharing the port
        needs no proxy-side change. Every listener on the port must set this, so an old
        instance has to exit before a new one binds -- which is the order the ensure
        script already uses."""
        with contextlib.suppress(OSError, AttributeError):
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
        super().server_bind()


def _collapse_found_payouts(rows):
    """One object per block. Per-output lists are served from `/api/found/<hash>` when a
    row is expanded — putting them in /api/payouts was ~7 MB JSON and froze the public page."""
    order = []
    by = {}
    for row in rows or []:
        key = row.get("hash") or row.get("height")
        if key not in by:
            order.append(key)
            by[key] = {
                "height": row.get("height"),
                "hash": row.get("hash"),
                "ts": row.get("ts"),
                "status": row.get("status"),
                "kind": row.get("kind") or "",
                "block_status": row.get("block_status") or "",
                "owed_sats": row.get("owed_sats") or 0,
                "owed_txid": row.get("owed_txid") or "",
                "owed_resolved": bool(row.get("owed_resolved")),
                "found_by": row.get("found_by") or "",
                "gateway": row.get("gateway") or "",
                "reward_btc": row.get("reward_btc"),
                "confirmations": row.get("confirmations") or 0,
                "miner_btc": 0.0,
                "pool_btc": 0.0,
                "outputs": [],
            }
        b = by[key]
        to = row.get("to") or "miner"
        amt = float(row.get("miner_btc") or 0)
        b["outputs"].append(
            {
                "address": row.get("finder") or "",
                "btc": amt,
                "share": row.get("share") or 0,
                "to": to,
            }
        )
        if to == "pool":
            b["pool_btc"] += amt
        else:
            b["miner_btc"] += amt
    public = []
    for k in order:
        b = by[k]
        pub = dict(b)
        pub["output_count"] = len(b["outputs"])
        pub.pop("outputs", None)
        public.append(pub)
    return public


def _public_prime_blocks(blocks):
    """Keep splits only for blocks not yet in chain (pending/orphan). In-chain splits
    already live on the collapsed payouts.outputs list."""
    slim = []
    for b in blocks or []:
        d = dict(b)
        if str(d.get("status") or "") == "in chain":
            d["split"] = []
        slim.append(d)
    return slim


# Public origin for canonical / sitemap / OG. Override in config.json if the UI is mirrored.
_PUBLIC_SITE = str(CONF.get("public_url") or "https://pool.awokenlazarus.xyz").rstrip("/")
_SEO_PAGES = {
    "/": {
        "en": {
            "title": "Lazarus Pool — BLAKE2b Bitcoin (XBT / BTCB2) mining pool for Siacoin ASICs",
            "description": (
                "Mine Bitcoin (XBT / BTCB2) with any Siacoin BLAKE2b ASIC — Goldshell SC, iBeLink BM-S3, "
                "Antminer A3. The first pool to pay TIDES as a split coinbase on this BIP-110 Bitcoin fork's "
                "mainnet, and the first to subsidize DATUM miners with its own stratum hashers. 0% via DATUM, "
                "10% public stratum (stratum+tcp://stratum.awokenlazarus.xyz:23334)."
            ),
            "scroll": "",
        },
        "zh": {
            "title": "Lazarus Pool — 用 Siacoin BLAKE2b 矿机挖比特币（XBT / BTCB2）的矿池",
            "description": (
                "用任何能挖 Siacoin 的 BLAKE2b ASIC 挖比特币（XBT / BTCB2）——金贝 SC、iBeLink BM-S3、蚂蚁 A3。"
                "本矿池是这条 BIP-110 比特币分叉主网上第一家用 TIDES 拆分 coinbase 支付的矿池，也是第一家"
                "用自有 stratum 算力补贴 DATUM 矿工的矿池。自建 DATUM 0%，公共 stratum 10%"
                "（stratum+tcp://stratum.awokenlazarus.xyz:23334）。"
            ),
            "scroll": "",
        },
    },
    "/hardware": {
        "en": {
            "title": "Siacoin ASICs that mine Bitcoin XBT (BTCB2) — Lazarus Pool",
            "description": (
                "Any ASIC that can mine Siacoin can mine Bitcoin XBT on BLAKE2b. "
                "Goldshell SC, iBeLink BM-S3, Antminer A3 — prices and estimated XBT per day on Lazarus Pool."
            ),
            "scroll": "hardware",
        },
        "zh": {
            "title": "能挖 Siacoin 的 ASIC 都能挖比特币 XBT（BTCB2）— Lazarus Pool",
            "description": "能挖 Siacoin 的 BLAKE2b 矿机都能在 Lazarus Pool 挖比特币 XBT。金贝 SC、iBeLink BM-S3、蚂蚁 A3，含价格与日收益估算。",
            "scroll": "hardware",
        },
    },
    "/connect": {
        "en": {
            "title": "Connect a miner to Lazarus Pool — XBT / BTCB2 stratum and DATUM",
            "description": (
                "Point a BLAKE2b ASIC at stratum+tcp://stratum.awokenlazarus.xyz:23334. "
                "Username is your Bitcoin (XBT) payout address. Or run a DATUM gateway at 0% fee."
            ),
            "scroll": "connect",
        },
        "zh": {
            "title": "接入 Lazarus Pool — XBT / BTCB2 的 stratum 与 DATUM",
            "description": "把 BLAKE2b 矿机指向 stratum+tcp://stratum.awokenlazarus.xyz:23334。用户名是你的比特币（XBT）收款地址。或自建 DATUM 网关，手续费 0%。",
            "scroll": "connect",
        },
    },
    "/mine-xbt": {
        "en": {
            "title": "How to mine XBT (BTCB2) — Lazarus Pool",
            "description": (
                "Mine Bitcoin XBT / BTCB2 with a Siacoin ASIC. Algorithm BLAKE2b, not SHA-256. "
                "Stratum: stratum+tcp://stratum.awokenlazarus.xyz:23334 — user = payout address, pass = x."
            ),
            "scroll": "connect",
        },
        "zh": {
            "title": "如何挖 XBT（BTCB2）— Lazarus Pool",
            "description": "用 Siacoin ASIC 挖比特币 XBT / BTCB2。算法是 BLAKE2b，不是 SHA-256。Stratum：stratum+tcp://stratum.awokenlazarus.xyz:23334，用户名=收款地址，密码 x。",
            "scroll": "connect",
        },
    },
    "/how": {
        "en": {
            "title": "How TIDES payouts work — Lazarus Pool (Bitcoin XBT / BTCB2)",
            "description": (
                "TIDES window share, the DATUM subsidy, and coinbase payouts on the BLAKE2b Bitcoin "
                "(XBT / BTCB2) chain. No pool balance, no withdrawals — the block pays your address directly."
            ),
            "scroll": "how",
        },
        "zh": {
            "title": "TIDES 如何支付 — Lazarus Pool（比特币 XBT / BTCB2）",
            "description": "BLAKE2b 比特币（XBT / BTCB2）链上的 TIDES 窗口份额、DATUM 补贴与 coinbase 支付。没有矿池余额，不用提现。",
            "scroll": "how",
        },
    },
    "/bip110": {
        "en": {
            "title": "BIP-110 Bitcoin fork mining — BLAKE2b (XBT / BTCB2) | Lazarus Pool",
            "description": (
                "BIP-110 split Bitcoin at block 961,632 in August 2026; the resulting chain then moved its "
                "proof-of-work to BLAKE2b at block 961,640. Mine this Bitcoin fork (XBT / BTCB2) with Siacoin "
                "ASICs on Lazarus Pool — TIDES, DATUM, and payouts inside the coinbase."
            ),
            "scroll": "how",
        },
        "zh": {
            "title": "BIP-110 比特币分叉挖矿 — BLAKE2b（XBT / BTCB2）| Lazarus Pool",
            "description": (
                "BIP-110 于 2026 年 8 月在 961,632 高度让比特币分链，这条链随后在 961,640 高度把工作量证明"
                "从 SHA-256d 换成 BLAKE2b。在 Lazarus Pool 用 Siacoin 矿机挖这条比特币分叉（XBT / BTCB2）："
                "TIDES、DATUM，并在 coinbase 内直接支付。"
            ),
            "scroll": "how",
        },
    },
    "/datum-subsidy": {
        "en": {
            "title": "DATUM subsidy — the first pool to pay for decentralization | Lazarus Pool",
            "description": (
                "Lazarus Pool was the first pool anywhere to use its stratum hashers to subsidize DATUM miners. "
                "Run your own DATUM gateway and Bitcoin Knots node: 0% fee plus a share of the public stratum's "
                "fee on every block. Decentralization that pays instead of costing."
            ),
            "scroll": "connect",
        },
        "zh": {
            "title": "DATUM 补贴 — 第一家为去中心化付钱的矿池 | Lazarus Pool",
            "description": (
                "Lazarus Pool 是第一家用自有 stratum 算力补贴 DATUM 矿工的矿池。自建 DATUM 网关和 "
                "Bitcoin Knots 节点：手续费 0%，每个区块还把公共 stratum 手续费的一部分记给你。"
                "去中心化不再是成本，而是收益。"
            ),
            "scroll": "connect",
        },
    },
    "/profitability": {
        "en": {
            "title": "Is mining XBT profitable? BLAKE2b ASIC earnings | Lazarus Pool",
            "description": (
                "Profitable crypto mining for idle Siacoin hardware: live XBT / BTCB2 per TH/s per day at current "
                "difficulty, per-machine estimates in XBT and dollars, and the DATUM subsidy on top."
            ),
            "scroll": "hardware",
        },
        "zh": {
            "title": "挖 XBT 划算吗？BLAKE2b 矿机收益 | Lazarus Pool",
            "description": (
                "让闲置的 Siacoin 矿机重新赚钱：按当前难度的每 TH/s 每日 XBT / BTCB2 收益、逐台机器的 XBT 与美元"
                "估算，另加 DATUM 补贴。"
            ),
            "scroll": "hardware",
        },
    },
    "/tides": {
        "en": {
            "title": "TIDES split coinbase — first on BLAKE2b Bitcoin mainnet | Lazarus Pool",
            "description": (
                "Lazarus Pool is the first pool confirmed to pay TIDES as a split coinbase on BLAKE2b Bitcoin "
                "(XBT / BTCB2) mainnet. Every address in a window worth eight times difficulty is an output of "
                "the block found — non-custodial by construction, with no pool balance to trust."
            ),
            "scroll": "how",
        },
        "zh": {
            "title": "TIDES 拆分 coinbase — BLAKE2b 比特币主网首家 | Lazarus Pool",
            "description": (
                "Lazarus Pool 是 BLAKE2b 比特币（XBT / BTCB2）主网上第一家用 TIDES 拆分 coinbase 支付的矿池。"
                "相当于全网难度八倍的滚动窗口内，每个地址都是所出区块的一个输出——天然非托管，没有需要信任的矿池余额。"
            ),
            "scroll": "how",
        },
    },
    "/pools": {
        "en": {
            "title": "BTCB2 / XBT mining pools compared — fees, custody, transaction fees",
            "description": (
                "Every BLAKE2b Bitcoin (XBT / BTCB2) pool worth pointing hashrate at, compared on what "
                "actually differs: the fee, whether the pool holds a balance for you, whether the block's "
                "transaction fees reach miners, and whether it limits its own share of the network."
            ),
            "scroll": "pools",
        },
        "zh": {
            "title": "BTCB2 / XBT 矿池对比 — 手续费、是否托管、交易费归谁",
            "description": (
                "值得投入算力的 BLAKE2b 比特币（XBT / BTCB2）矿池对比，只比真正有差别的地方：手续费、矿池是否"
                "替你保管余额、区块里的交易费是否分给矿工，以及它是否限制自己在全网中的占比。"
            ),
            "scroll": "pools",
        },
    },
    "/self-cap": {
        "en": {
            "title": "The 25% line: a mining pool that turns hashrate away — Lazarus Pool",
            "description": (
                "No pool should hold a third of a chain. Past 25% of network hashrate Lazarus relays new "
                "stratum miners to another BTCB2 / XBT pool and lets that pool pay them. Enforced in the "
                "software on every connection, not promised in a blog post."
            ),
            "scroll": "pools",
        },
        "zh": {
            "title": "25% 这条线：会把算力拒之门外的矿池 — Lazarus Pool",
            "description": (
                "没有哪个矿池该占一条链的三分之一。算力超过全网 25% 后，Lazarus 会把新接入的 stratum 矿工中继到"
                "另一家 BTCB2 / XBT 矿池，由那家矿池付款。这是每一次连接都由软件强制执行的，不是博客里的承诺。"
            ),
            "scroll": "pools",
        },
    },
    "/non-custodial": {
        "en": {
            "title": "Non-custodial XBT mining: no pool balance, no withdrawal — Lazarus Pool",
            "description": (
                "Lazarus Pool never holds your coins. Every payout is an output of the block itself, paid to "
                "your address by the coinbase — no balance, no minimum, no withdrawal, nothing owed to anyone "
                "if the pool disappeared tonight."
            ),
            "scroll": "how",
        },
        "zh": {
            "title": "非托管挖矿：没有矿池余额，不用提现 — Lazarus Pool",
            "description": (
                "Lazarus Pool 从不持有你的币。每一笔支付都是区块本身的一个输出，由 coinbase 直接付到你的地址——"
                "没有余额、没有起付线、不用提现；哪怕矿池今晚消失，也不欠任何人。"
            ),
            "scroll": "how",
        },
    },
    "/calculator": {
        "en": {
            "title": "XBT / BTCB2 mining calculator — earnings per TH/s at live difficulty",
            "description": (
                "Type in your hashrate and get estimated XBT and dollars per day on the BLAKE2b Bitcoin chain, "
                "from live difficulty, price and the DATUM subsidy. Per-machine estimates for every Siacoin "
                "ASIC are listed below it."
            ),
            "scroll": "calc",
        },
        "zh": {
            "title": "XBT / BTCB2 挖矿收益计算器 — 按实时难度算每 TH/s 收益",
            "description": (
                "输入你的算力，按实时难度、价格与 DATUM 补贴，算出在 BLAKE2b 比特币链上每天大约能拿多少 XBT 和"
                "多少美元。下方还有每一款 Siacoin 矿机的逐台估算。"
            ),
            "scroll": "calc",
        },
    },
    "/blocks": {
        "en": {
            "title": "Blocks found by Lazarus Pool — XBT / BTCB2 coinbase payouts you can open",
            "description": (
                "Every block Lazarus Pool has found on the BLAKE2b Bitcoin (XBT / BTCB2) chain, with the "
                "coinbase outputs it paid. Open any of them in the explorer and check your own address "
                "against the TIDES window."
            ),
            "scroll": "blocks",
        },
        "zh": {
            "title": "Lazarus Pool 出的区块 — 可逐条核对的 XBT / BTCB2 coinbase 支付",
            "description": (
                "Lazarus Pool 在 BLAKE2b 比特币（XBT / BTCB2）链上找到的每一个区块，以及它支付的 coinbase 输出。"
                "任意打开一个到浏览器里，就能拿自己的地址对着 TIDES 窗口核对。"
            ),
            "scroll": "blocks",
        },
    },
    "/api": {
        "en": {
            "title": "Lazarus Pool API — public JSON for the BLAKE2b Bitcoin (XBT / BTCB2) pool",
            "description": (
                "Public, unauthenticated JSON endpoints for pool hashrate, the TIDES window, blocks found, "
                "coinbase outputs, gateways, payouts, price and hardware estimates. No key, no rate limit "
                "worth worrying about, CORS open."
            ),
            "scroll": "",
        },
        "zh": {
            "title": "Lazarus Pool API — BLAKE2b 比特币（XBT / BTCB2）矿池的公开 JSON",
            "description": (
                "公开、免鉴权的 JSON 接口：矿池算力、TIDES 窗口、已出区块、coinbase 输出、网关、支付、价格与"
                "硬件收益估算。不需要 key，也没有值得担心的频率限制，CORS 全开。"
            ),
            "scroll": "",
        },
    },
}
# Trailing-slash and legacy spellings of the same page, so every variant answers 200 and
# canonicalises to one URL rather than splitting the same content across near-duplicates.
# The pool comparison. Each figure is what that pool publishes on its own site, so the claim is
# checkable and stays fair when they change it; our own most expensive number is in there too,
# because a comparison that only flatters the host is worth nothing to the person reading it.
_POOL_TABLE_EN = """<div class="seo-table"><table>
        <caption>BLAKE2b Bitcoin (XBT / BTCB2) pools, as each publishes its own terms, September 2026</caption>
        <thead><tr><th scope="col">Pool</th><th scope="col">Fee</th><th scope="col">Reward scheme</th><th scope="col">Who holds your coins</th><th scope="col">The block's transaction fees</th><th scope="col">Limit on its own share</th></tr></thead>
        <tbody>
        <tr><th scope="row">Lazarus Pool</th><td>0% own DATUM gateway · 10% public stratum, five of those points paid back to DATUM miners</td><td>TIDES, 8&times; difficulty</td><td>Nobody. The block's coinbase pays your address</td><td>Scale every miner's payout up</td><td>25%, enforced by relaying new miners elsewhere</td></tr>
        <tr><th scope="row">B2Pool</th><td>0% own DATUM gateway · 1% stratum</td><td>TIDES, 8&times; difficulty</td><td>Coinbase where it fits, otherwise the pool until your balance passes 10,000 sat</td><td>Stay with the pool</td><td>None published</td></tr>
        <tr><th scope="row">AlphaPool</th><td>2.5%</td><td>PPLNS</td><td>The pool, until a block reaches 100-confirmation maturity and a batch cycle pays out</td><td>Not published</td><td>30%, pledged after it passed 50% of the network</td></tr>
        </tbody>
      </table></div>"""

_POOL_TABLE_ZH = """<div class="seo-table"><table>
        <caption>BLAKE2b 比特币（XBT / BTCB2）矿池对比，均按各家自行公布的口径，2026 年 9 月</caption>
        <thead><tr><th scope="col">矿池</th><th scope="col">手续费</th><th scope="col">奖励方式</th><th scope="col">谁替你拿着币</th><th scope="col">区块里的交易费</th><th scope="col">自身占比上限</th></tr></thead>
        <tbody>
        <tr><th scope="row">Lazarus Pool</th><td>自建 DATUM 网关 0% · 公共 stratum 10%，其中五个点返还给 DATUM 矿工</td><td>TIDES，难度 8 倍</td><td>没有人。由区块的 coinbase 直接付到你的地址</td><td>等比例抬高每位矿工的收益</td><td>25%，超过即把新矿工中继到别家</td></tr>
        <tr><th scope="row">B2Pool</th><td>自建 DATUM 网关 0% · stratum 1%</td><td>TIDES，难度 8 倍</td><td>能进 coinbase 就进，否则由矿池代持至余额超过 10,000 sat</td><td>留给矿池</td><td>未公布</td></tr>
        <tr><th scope="row">AlphaPool</th><td>2.5%</td><td>PPLNS</td><td>矿池代持，直到区块达到 100 确认成熟并由批量周期支付</td><td>未公布</td><td>30%，在占到全网一半以上之后承诺</td></tr>
        </tbody>
      </table></div>"""

_API_TABLE_EN = """<div class="seo-table"><table>
        <caption>Public endpoints. GET, JSON, no authentication.</caption>
        <thead><tr><th scope="col">Endpoint</th><th scope="col">What it returns</th></tr></thead>
        <tbody>
        <tr><th scope="row"><code>/api/pool</code></th><td>Hashrate by path, network difficulty and share, the TIDES window, the fee split and the live XBT per TH/s per day</td></tr>
        <tr><th scope="row"><code>/api/coinbaser</code></th><td>The coinbase output list Prime is handing out right now — the next block's payout, before it is found</td></tr>
        <tr><th scope="row"><code>/api/blocks</code></th><td>Every block the pool has found, with height, time and reward</td></tr>
        <tr><th scope="row"><code>/api/found/&lt;blockhash&gt;</code></th><td>The coinbase outputs a found block actually paid</td></tr>
        <tr><th scope="row"><code>/api/miners</code></th><td>Addresses with accepted work in the window, their share of it and their hashrate</td></tr>
        <tr><th scope="row"><code>/api/gateways</code></th><td>Connected DATUM gateways and what each is contributing</td></tr>
        <tr><th scope="row"><code>/api/payouts</code></th><td>Coinbase payouts already made, per address</td></tr>
        <tr><th scope="row"><code>/api/solo</code></th><td>Solo miners and blocks found solo</td></tr>
        <tr><th scope="row"><code>/api/price</code></th><td>The XBT price the site converts with</td></tr>
        <tr><th scope="row"><code>/api/hardware</code></th><td>The Siacoin ASIC list with prices and estimated XBT and dollars per day</td></tr>
        </tbody>
      </table></div>"""

_API_TABLE_ZH = """<div class="seo-table"><table>
        <caption>公开接口。GET、JSON、免鉴权。</caption>
        <thead><tr><th scope="col">接口</th><th scope="col">返回内容</th></tr></thead>
        <tbody>
        <tr><th scope="row"><code>/api/pool</code></th><td>各路径算力、全网难度与占比、TIDES 窗口、手续费拆分，以及实时的每 TH/s 每日 XBT</td></tr>
        <tr><th scope="row"><code>/api/coinbaser</code></th><td>Prime 此刻正在下发的 coinbase 输出列表——也就是下一个区块在出块之前就定好的支付</td></tr>
        <tr><th scope="row"><code>/api/blocks</code></th><td>矿池找到的每一个区块，含高度、时间与奖励</td></tr>
        <tr><th scope="row"><code>/api/found/&lt;区块哈希&gt;</code></th><td>某个已出区块实际支付的 coinbase 输出</td></tr>
        <tr><th scope="row"><code>/api/miners</code></th><td>窗口内有有效工作量的地址、各自占比与算力</td></tr>
        <tr><th scope="row"><code>/api/gateways</code></th><td>已连接的 DATUM 网关及各自的贡献</td></tr>
        <tr><th scope="row"><code>/api/payouts</code></th><td>已完成的 coinbase 支付，按地址列出</td></tr>
        <tr><th scope="row"><code>/api/solo</code></th><td>单挖矿工与单挖出的区块</td></tr>
        <tr><th scope="row"><code>/api/price</code></th><td>本站折算所用的 XBT 价格</td></tr>
        <tr><th scope="row"><code>/api/hardware</code></th><td>Siacoin 矿机列表，含价格与每日 XBT / 美元估算</td></tr>
        </tbody>
      </table></div>"""

# Page-specific opening copy. Every pretty URL serves the same dashboard below, so without this
# each one is a near-duplicate and Google collapses them into the homepage.
_SEO_INTRO = {
    # Chinese only. The English homepage keeps the hero as its <h1>; /zh/ needs Chinese copy in the
    # markup itself rather than relying on the browser to translate it after load.
    "/": {
        "zh": ("用 Siacoin BLAKE2b 矿机挖比特币（XBT / BTCB2）", [
            "任何能挖 Siacoin 的 ASIC 都能挖这条链——同为 BLAKE2b，原厂固件即可，不用换硬件。把矿机指向 <b>stratum+tcp://stratum.awokenlazarus.xyz:23334</b>，用户名填你的收款地址，密码填 <code>x</code>。没有账户，不用注册。",
            "支付走 TIDES 拆分 coinbase：矿池找到区块时，窗口内每个地址都成为该区块的一个输出，直接付到你的地址。没有矿池余额、没有起付线、不用提现，矿池也从不持有你的币。自建 DATUM 网关 0% 手续费，并从公共 stratum 的手续费里分得补贴。",
            "<a href=\"/zh/mine-xbt\">如何开始挖</a> · <a href=\"/zh/hardware\">哪些矿机能用</a> · <a href=\"/zh/calculator\">收益计算器</a> · <a href=\"/zh/pools\">各矿池对比</a> · <a href=\"/zh/self-cap\">为什么我们把自己限制在 25%</a>",
        ]),
    },
    "/bip110": {
        "en": ("BIP-110 and BLAKE2b: what actually forked", [
            "BIP-110 is a Bitcoin proposal to restrict non-financial data in transactions for a year. It asked for 55% of hashrate to signal support and peaked near 2.6%, and nodes running it began rejecting blocks that did not signal at height 961,632 on 8 August 2026. That is the moment the chain split. This chain is the BIP-110 branch; the majority chain carried on under SHA-256d, unaffected.",
            "The proof-of-work change is a separate, later decision on this branch. On 30 August 2026 it moved from SHA-256d to BLAKE2b with a Sia-style header: block 961,639 was the last SHA-256d block and 961,640 the first BLAKE2b one. Everything else is still Bitcoin — the 21 million cap, the halving schedule, script, addresses, and every block of history before the split.",
            "So \u201cBIP-110\u201d is what people call this chain, but BLAKE2b is what decides your hardware. Any ASIC built for Siacoin mines here on stock firmware; no SHA-256 machine can produce a valid share at all. Because the pre-split history is shared with Bitcoin, generate a fresh address for this chain and keep SHA-256 keys well clear. Lazarus Pool mines it with <a href=\"/tides\">TIDES payouts in the coinbase</a> and pays a <a href=\"/datum-subsidy\">subsidy to DATUM miners</a>.",
        ]),
        "zh": ("BIP-110 与 BLAKE2b：到底分叉了什么", [
            "BIP-110 是一项比特币提案，主张在一年内限制交易中的非金融数据。它需要 55% 的算力表态支持，最高只到约 2.6%；运行它的节点从 2026 年 8 月 8 日、961,632 高度起开始拒绝不表态的区块——那一刻链就分开了。本链是 BIP-110 这一支，多数链继续用 SHA-256d，未受影响。",
            "改工作量证明是这一支后来的另一个决定。2026 年 8 月 30 日，它从 SHA-256d 换成了 BLAKE2b（Sia 风格区块头）：961,639 是最后一个 SHA-256d 区块，961,640 是第一个 BLAKE2b 区块。其余部分仍是比特币——2100 万上限、减半周期、脚本、地址，以及分叉前的全部历史。",
            "所以「BIP-110」是大家对这条链的称呼，但真正决定你用什么硬件的是 BLAKE2b。任何为 Siacoin 而造的 ASIC 用原厂固件就能在这里挖矿；SHA-256 机器根本产不出有效份额。由于分叉前的历史与比特币共享，请为本链另生成新地址，SHA-256 的私钥务必远离。Lazarus Pool 用 <a href=\"/tides\">TIDES 在 coinbase 内支付</a>，并向 <a href=\"/datum-subsidy\">DATUM 矿工发放补贴</a>。",
        ]),
    },
    "/tides": {
        "en": ("TIDES: paid inside the block, not from a pool balance", [
            "TIDES keeps a rolling window of accepted shares worth roughly eight times network difficulty — days of pool work, not the last hour. When the pool finds a block, every address in that window becomes an output of that block's coinbase, in proportion to its share of the window and less that miner's fee. A new rig starts near zero and ramps up as its work enters the window and older work ages out.",
            "Lazarus Pool is the first pool confirmed to run a TIDES split coinbase on BLAKE2b Bitcoin mainnet — not on a testnet and not as a proposal, but in blocks you can open in the explorer and read the outputs of. The consequence matters more than the mechanism: there is no pool balance, no minimum, and no withdrawal, because the pool never holds your coins in the first place. If it vanished tonight, nobody would be owed anything.",
        ]),
        "zh": ("TIDES：在区块里直接支付，而不是从矿池余额里提现", [
            "TIDES 维护一个滚动窗口，容量约为全网难度的八倍——那是好几天的矿池工作量，而不是最近一小时。矿池找到区块时，窗口内每个地址都会成为该区块 coinbase 的一个输出，按其窗口占比支付并扣除该矿工的手续费。新机器从接近零开始，随着新工作进入、旧工作老化而逐步爬升。",
            "Lazarus Pool 是 BLAKE2b 比特币主网上第一家被确认运行 TIDES 拆分 coinbase 的矿池——不是测试网，也不是提案，而是你可以在浏览器里打开并逐条查看输出的真实区块。比机制更重要的是后果：没有矿池余额、没有起付线、不用提现，因为矿池从一开始就不持有你的币。哪怕它今晚消失，也不欠任何人。",
        ]),
    },
    "/datum-subsidy": {
        "en": ("The DATUM subsidy: getting paid to decentralize", [
            "Running your own DATUM gateway against your own Bitcoin Knots node means you build the block template and choose the transactions in it. The pool only supplies the coinbase split and verifies your shares. That work costs you nothing in fees here — DATUM miners pay 0% — and it moves template construction out of the pool's hands, which is the part of mining centralization that actually matters.",
            "Lazarus Pool is the first pool anywhere to fund a subsidy for that from its own stratum hashers. The public stratum charges 10%, and five of those points are not kept: on every block found they are credited pro rata to every DATUM miner holding work in the window, whether or not that miner's template produced the block. Decentralizing the network pays better than using the pool's own stratum, which is the incentive the right way round. <a href=\"/connect\">Gateway setup and the config to copy.</a>",
        ]),
        "zh": ("DATUM 补贴：为去中心化拿钱", [
            "用自己的 Bitcoin Knots 节点跑自己的 DATUM 网关，意味着区块模板由你构建、交易由你挑选，矿池只提供 coinbase 拆分并校验你的份额。在这里这件事不收你一分手续费——DATUM 矿工 0%——而且它把模板构建权从矿池手里移走，那才是挖矿中心化真正要紧的一环。",
            "Lazarus Pool 是全网第一家用自有 stratum 算力为此出资补贴的矿池。公共 stratum 收 10%，其中五个点并不留下：每找到一个区块，就按比例记给窗口内每一位 DATUM 矿工，无论那个区块是不是由他的模板产出。让网络去中心化比用矿池的 stratum 更赚钱——激励方向本该如此。<a href=\"/connect\">网关配置与可直接复制的 config。</a>",
        ]),
    },
    "/profitability": {
        "en": ("Is mining Bitcoin XBT profitable?", [
            "Difficulty on this BLAKE2b chain is still low relative to a full block subsidy, which is the whole reason a Siacoin ASIC that stopped paying for itself on Sia can earn again here. The live figure for XBT per TH/s per day sits in the pool summary and moves with difficulty and price; the machine list below turns it into an estimate in XBT and in dollars for each specific box at today's numbers.",
            "Read those estimates honestly. They assume the base subsidy with no transaction fees, they use the current difficulty rather than a forecast, they route through your own DATUM gateway with the subsidy included, and they do not subtract electricity — that number is yours and it decides whether any of this works for you. Transaction fees in a found block scale every payout up proportionally. <a href=\"/hardware\">See the machines and their estimates below.</a>",
        ]),
        "zh": ("挖比特币 XBT 划算吗？", [
            "本 BLAKE2b 链的难度相对于完整区块奖励仍然偏低，这正是一台在 Sia 上已经赚不回电费的 Siacoin 矿机能在这里重新赚钱的原因。每 TH/s 每日 XBT 的实时数字就在矿池概览里，随难度与价格变化；下面的机器列表把它换算成每台机器在当前数字下的 XBT 与美元估算。",
            "请如实看待这些估算：它们按基础奖励计算、不含交易费，用的是当前难度而非预测，走你自己的 DATUM 网关并已计入补贴，而且没有扣除电费——那个数字只有你知道，也正是它决定这件事对你是否成立。区块里的交易费会等比例抬高每一笔支付。<a href=\"/hardware\">机器与估算见下方。</a>",
        ]),
    },
    "/hardware": {
        "en": ("Siacoin BLAKE2b ASICs that mine Bitcoin XBT", [
            "Any ASIC that can mine Siacoin can mine Bitcoin XBT, because both are BLAKE2b — no firmware change, no new hardware. Goldshell's SC series, iBeLink's BM-S3, BM-S3+ and BM-N3, and the Antminer A3 all connect and start hashing. SHA-256 machines cannot: an S19, S21 or Whatsminer will never produce a valid share on this chain.",
            "The list below is BT-Miners' BTCB2 collection with their prices, turned into estimated XBT and dollars per day at current difficulty and price, through your own DATUM gateway with the subsidy included. Electricity is not in those numbers. Click any machine to open its page on BT-Miners. Lazarus Pool is not responsible for BT-Miners' customer support or quality of service.",
        ]),
        "zh": ("能挖比特币 XBT 的 Siacoin BLAKE2b 矿机", [
            "任何能挖 Siacoin 的 ASIC 都能挖比特币 XBT，因为两者同为 BLAKE2b——不用换固件，也不用换硬件。金贝 SC 系列、iBeLink BM-S3 / BM-S3+ / BM-N3、蚂蚁 A3 都能直接连上开始工作。SHA-256 机器不行：S19、S21 或神马在本链永远产不出有效份额。",
            "下面的列表来自 BT-Miners 的 BTCB2 系列，价格是他们的，并按当前难度与价格换算成每日 XBT 与美元估算，走你自己的 DATUM 网关并计入补贴。电费不在其中。点击任意机器可打开其 BT-Miners 页面。Lazarus Pool 不对 BT-Miners 的客户支持或服务质量负责。",
        ]),
    },
    "/connect": {
        "en": ("Connect a miner to Lazarus Pool", [
            "Point the miner at <b>stratum+tcp://stratum.awokenlazarus.xyz:23334</b>, set the username to the address you want paid — optionally <code>address.worker</code> — and the password to <code>x</code>. The algorithm is BLAKE2b with a Sia-style header, not SHA-256d. New sessions start at difficulty 4096 and vardiff steps up toward your hashrate from there. There is no account and no registration; the username is the payout instruction.",
            "There are two ways in. The public stratum charges 10% and our node builds the templates, which is the one-line setup. Running your own Bitcoin Knots node and DATUM gateway costs 0% and pays you a <a href=\"/datum-subsidy\">share of that stratum fee</a> on every block, because you are building the templates yourself. Both land in the same TIDES window under your address, so switching later keeps the accepted work you already have.",
        ]),
        "zh": ("把矿机接入 Lazarus Pool", [
            "把矿机指向 <b>stratum+tcp://stratum.awokenlazarus.xyz:23334</b>，用户名填你要收款的地址（也可以写成 <code>地址.worker</code>），密码填 <code>x</code>。算法是 BLAKE2b（Sia 风格区块头），不是 SHA-256d。新会话从难度 4096 起步，之后 vardiff 会朝你的算力逐步调整。没有账户，也不用注册——用户名就是收款指令。",
            "有两条路。公共 stratum 收 10%，模板由我们的节点构建，配置只有一行。自己跑 Bitcoin Knots 节点和 DATUM 网关则是 0%，而且因为模板是你自己构建的，每个区块还会把<a href=\"/datum-subsidy\">那笔 stratum 手续费的一部分</a>付给你。两条路都记入同一个 TIDES 窗口、同一个地址，所以以后切换不会丢掉已积累的工作量。",
        ]),
    },
    "/mine-xbt": {
        "en": ("How to mine Bitcoin XBT (BTCB2)", [
            "You need three things: an ASIC that hashes BLAKE2b, an address on this chain to be paid to, and the pool endpoint. If you already own a Siacoin miner you have the first one — this is the same algorithm, so stock firmware works. Point it at <b>stratum+tcp://stratum.awokenlazarus.xyz:23334</b> with your address as the username and <code>x</code> as the password, and it will start hashing immediately.",
            "One warning worth reading twice: this chain shares every block of history with SHA-256 Bitcoin up to block 961,640, so an address holding real BTC should never be used here. Generate a fresh address for this chain. Payouts arrive as outputs in the coinbase of blocks the pool finds, spendable after 100 confirmations, with no balance to withdraw. <a href=\"/hardware\">Which machines work</a> · <a href=\"/tides\">how the payout is calculated</a>.",
        ]),
        "zh": ("如何挖比特币 XBT（BTCB2）", [
            "你需要三样东西：一台能算 BLAKE2b 的 ASIC、一个本链的收款地址，以及矿池地址。如果你已经有 Siacoin 矿机，第一样就有了——算法相同，原厂固件即可。把它指向 <b>stratum+tcp://stratum.awokenlazarus.xyz:23334</b>，用户名填你的地址，密码填 <code>x</code>，立刻就能开始工作。",
            "有一条提醒值得看两遍：本链与 SHA-256 比特币共享 961,640 高度之前的全部历史，因此持有真实 BTC 的地址绝不可在此使用，请为本链另生成一个新地址。收益以矿池所出区块 coinbase 中的输出形式到账，100 个确认后可动用，没有余额需要提现。<a href=\"/hardware\">哪些机器能用</a> · <a href=\"/tides\">支付如何计算</a>。",
        ]),
    },
    "/how": {
        "en": ("How your hashrate becomes a payout", [
            "Your miner hashes a plain BLAKE2b header and knows nothing about any of this. Every share it sends is rebuilt into a full header and hashed again by Prime, the pool server; accepted work is credited to the address in your username and enters the TIDES window. The window holds the last eight times network difficulty of accepted work, which is days of pool work rather than a recent average.",
            "When a block is found, Prime has already handed every gateway the same coinbase output list — the one shown under Next payout — and a share whose coinbase pays anything else is refused. So whoever's machine finds the block, that block pays the whole window. There is no pool balance to withdraw and no operator holding your coins between blocks. <a href=\"/tides\">More on the TIDES window</a> · <a href=\"/connect\">connect a miner</a>.",
        ]),
        "zh": ("你的算力如何变成收益", [
            "你的矿机只是在算一个普通的 BLAKE2b 区块头，对这一切一无所知。它发出的每个份额都会被矿池服务端 Prime 重建成完整区块头并重新哈希；被接受的工作量记到你用户名里的地址上，并进入 TIDES 窗口。窗口容纳最近相当于全网难度八倍的工作量，那是好几天的矿池工作量，而不是一个近期平均值。",
            "找到区块时，Prime 早已把同一份 coinbase 输出列表发给了每个网关——就是「下一次支付」里显示的那份——凡是 coinbase 支付其他内容的份额都会被拒绝。所以无论谁的机器出块，那个区块都支付给整个窗口。没有矿池余额需要提现，区块之间也没有谁替你保管币。<a href=\"/tides\">了解 TIDES 窗口</a> · <a href=\"/connect\">接入矿机</a>。",
        ]),
    },
    "/pools": {
        "en": ("Which XBT (BTCB2) pool should you point hashrate at?", [
            "The fee is the number everyone compares first and the least interesting of the four things that actually differ between pools on this chain. The others: whether the pool ever holds your coins, whether the transaction fees in a found block reach the miners or stay with the operator, and whether the pool does anything at all to limit its own share of the network.",
            _POOL_TABLE_EN,
            "Read that honestly and our public stratum is the expensive one. If you have no intention of running a node, 1% elsewhere beats 10% here and we would rather say so than pretend otherwise. What that 10% buys is the other column: five of those points are handed back to DATUM miners on every block found, which is why the path we actually recommend costs 0% and gets paid a bonus on top of a full window share.",
            "The rest of the table is where nothing else on this chain matches. Transaction fees in a block scale every payout up here instead of staying with the pool. Nothing is ever held — the block itself pays your address, so there is no balance, threshold or withdrawal. And past 25% of network hashrate this pool <a href=\"/self-cap\">turns new miners away</a> and hands them to someone else. Figures are as each pool published them in September 2026; check their sites before you commit a fleet, and see the pools we relay to below.",
        ]),
        "zh": ("XBT（BTCB2）该挖哪个矿池？", [
            "手续费是所有人第一个拿来比的数字，也是本链各矿池之间真正有差别的四件事里最不重要的一件。另外三件是：矿池会不会替你保管币、所出区块里的交易费是分给矿工还是留给运营者，以及这家矿池有没有采取任何措施限制自己在全网中的占比。",
            _POOL_TABLE_ZH,
            "如实来看，我们的公共 stratum 是贵的那一个。如果你完全不打算自己跑节点，别家 1% 就是比这里 10% 划算，我们宁愿直说，也不想装作不是。这 10% 换来的是隔壁那一列：其中五个点在每次出块时都会返还给 DATUM 矿工——这也正是我们真正推荐的那条路为什么是 0%，而且在拿到完整窗口份额之外还额外拿补贴。",
            "表格剩下的部分，本链目前没有别家能对上。这里区块中的交易费会等比例抬高每一笔支付，而不是留在矿池。任何时候都不代持——由区块本身付到你的地址，因此没有余额、没有起付线、不用提现。而且一旦超过全网 25% 的算力，本矿池会<a href=\"/self-cap\">把新矿工拒之门外</a>并转交给别家。表中数字为各矿池 2026 年 9 月自行公布的口径；投入整批机器前请先到各家网站核对，也可以看下方我们中继过去的矿池。",
        ]),
    },
    "/self-cap": {
        "en": ("A pool that turns hashrate away at 25%", [
            "On a chain this size one pool can pass a third of the network in an afternoon, and in September 2026 one did — past half of all BTCB2 hashrate, followed by a patch proposed in earnest to blacklist that pool's payout address at the consensus level, which the Knots maintainer publicly told people not to run. The hashrate came back down voluntarily. Nothing about the episode was fixed by it.",
            "Lazarus holds itself to 25%. Over that line a <em>new</em> stratum connection is not accepted and mined on our behalf: it is relayed to one of four other pools and paid by that pool, under the same address, with nothing credited to our window. Miners already here keep mining here. Own-gateway DATUM miners are never relayed, because a miner building their own block templates is not the thing that centralizes a chain. New miners come back automatically once we are under the line.",
            "This is in the connection path rather than in a pledge — the live network share and the count of connections being relayed right now are both on this page, and a relayed worker's own stats page names the pool that has it. It is the same reasoning as the <a href=\"/datum-subsidy\">DATUM subsidy</a>: the pool pays miners to take template construction away from it, and hands away hashrate it is not entitled to. <a href=\"/pools\">How the other pools compare.</a>",
        ]),
        "zh": ("超过 25% 就把算力拒之门外的矿池", [
            "在这个规模的链上，一家矿池一个下午就能超过全网三分之一——2026 年 9 月真的发生了：某矿池占到 BTCB2 全网算力一半以上，随后有人正经提交补丁，要在共识层把该矿池的收款地址拉黑，Knots 维护者公开表示不要运行那段代码。最后算力是自愿降下来的，这件事本身什么也没解决。",
            "Lazarus 给自己划的线是 25%。越过这条线后，<em>新</em>的 stratum 连接不会被我们接下来自己挖：它会被中继到另外四家矿池之一，由那家矿池按同一个地址付款，我们的窗口里不记入任何东西。已经在这里的矿机继续留在这里。自建网关的 DATUM 矿工永远不会被中继，因为自己构建区块模板的矿工并不是让一条链中心化的那个因素。等我们回到线下，新矿工会自动回来。",
            "这件事写在连接路径里，而不是写在承诺里——实时的全网占比、以及此刻正被中继的连接数都在本页上，被中继的矿机在自己的统计页里也会看到接手它的矿池名字。这和<a href=\"/datum-subsidy\">DATUM 补贴</a>是同一个道理：矿池花钱请矿工把模板构建权从自己手里拿走，也把本不该属于自己的算力让出去。<a href=\"/pools\">其他矿池怎么比</a>。",
        ]),
    },
    "/non-custodial": {
        "en": ("No pool balance, no withdrawal, nothing to trust", [
            "Almost every mining pool credits your work to a balance and pays that balance out later — when it passes a threshold, when a block matures, when a batch cycle runs. That gap between earning and holding is where mining money has always gone missing: exits, hacks, thresholds you never reach, a dust balance stranded when you unplug. Lazarus does not have the gap, because it never takes custody in the first place.",
            "Your payout is an output of the block itself. Prime hands every gateway the same coinbase output list before any work goes out, and a share whose coinbase pays anything other than that list is refused — so whichever machine finds the block, that block's coinbase pays every address in the <a href=\"/tides\">TIDES window</a> directly. The pool never receives your coins, which means it cannot hold, batch, freeze or lose them.",
            "In practice: no account, no registration, no KYC, no minimum payout, no withdrawal button, no pending balance, and nothing owed to anyone if this pool disappeared tonight. Coinbase outputs are spendable after 100 confirmations like any other. A pool that pays you \u201conce your balance passes a threshold\u201d or \u201cat maturity\u201d is holding your coins in between; that is a real difference in what you are trusting, and it is worth knowing which kind you are on. <a href=\"/pools\">Compare the pools on this chain.</a>",
        ]),
        "zh": ("没有矿池余额，不用提现，没有需要信任的对象", [
            "几乎所有矿池都会把你的工作量记成一笔余额，之后再支付出去——攒够起付线时、区块成熟时、批量支付周期跑到时。赚到和拿到之间这段空隙，正是挖矿的钱历来消失的地方：跑路、被盗、永远攒不到的起付线、拔机后卡住的零星余额。Lazarus 没有这段空隙，因为它从一开始就不接管你的币。",
            "你的收益是区块本身的一个输出。在任何工作下发之前，Prime 就把同一份 coinbase 输出列表交给了每个网关，凡是 coinbase 支付了这份列表以外内容的份额都会被拒绝——所以无论哪台机器出块，那个区块的 coinbase 都直接支付给 <a href=\"/tides\">TIDES 窗口</a>里的每一个地址。矿池从不收到你的币，也就无从代持、批量、冻结或弄丢。",
            "具体就是：没有账户、不用注册、没有 KYC、没有起付线、没有提现按钮、没有待发余额；哪怕这家矿池今晚消失，也不欠任何人。coinbase 输出和其他输出一样，100 个确认后即可动用。一家说「余额攒够起付线才付」或「成熟后再付」的矿池，在这中间是替你拿着币的；你信任的东西因此不同，值得先弄清自己在哪一种上。<a href=\"/pools\">对比本链各矿池</a>。",
        ]),
    },
    "/calculator": {
        "en": ("XBT / BTCB2 mining calculator", [
            "Type your hashrate into <a href=\"#calc\">the calculator further down this page</a> and it works out what that hashrate is worth per day on the BLAKE2b Bitcoin chain right now. The arithmetic is deliberately dull: the pool publishes a live XBT-per-TH/s-per-day figure from current network difficulty and the block subsidy, and your estimate is that figure multiplied by your hashrate, converted at the current price.",
            "What is in the number and what is not, because this is where calculators lie. It uses the difficulty right now, not a forecast, and difficulty is what will actually change your earnings. It assumes the base subsidy with no transaction fees, so a block with fees in it pays more than this says. It assumes the DATUM path with the subsidy included, which is the 0% route. It does not subtract electricity — that figure is yours, and it is the one that decides whether any of this works. <a href=\"/hardware\">Per-machine estimates for every Siacoin ASIC</a> are listed below, and <a href=\"/profitability\">whether mining XBT pays at all</a> goes into it further.",
        ]),
        "zh": ("XBT / BTCB2 挖矿收益计算器", [
            "把你机器的总算力填进<a href=\"#calc\">本页下方的计算器</a>，它会算出这份算力此刻在 BLAKE2b 比特币链上每天值多少。算法故意做得很直白：矿池会按当前全网难度和区块奖励公布一个实时的「每 TH/s 每日 XBT」数字，你的估算就是这个数字乘上你的算力，再按当前价格折算成美元。",
            "这个数字包含什么、不包含什么——计算器就是在这里骗人的。它用的是此刻的难度，不是预测，而难度才是真正会改变你收益的东西。它按基础奖励计算、不含交易费，所以带交易费的区块会比这里显示的多。它按含补贴的 DATUM 路径计算，也就是 0% 那条路。它没有扣电费——那个数字只有你知道，也正是它决定这件事是否成立。下方有<a href=\"/hardware\">每一款 Siacoin 矿机的逐台估算</a>，<a href=\"/profitability\">挖 XBT 到底划不划算</a>另有更细的说明。",
        ]),
    },
    "/blocks": {
        "en": ("Blocks found by Lazarus Pool", [
            "Every block this pool has found on the BLAKE2b Bitcoin (XBT / BTCB2) chain is listed below with its height, when it landed, and what it paid. Because payouts are a split coinbase rather than a pool balance, each of these blocks <em>is</em> a payout run: opening one shows the outputs it made and the addresses they went to.",
            "That makes the whole payout history checkable by anyone without asking us for anything. Find a block from a period you were mining, open its coinbase, and your address should appear with the share of the <a href=\"/tides\">TIDES window</a> it held at the time, less its fee. Nothing is reconstructed from our database for that check — it is on chain. <a href=\"/non-custodial\">Why there is no balance to reconcile.</a>",
        ]),
        "zh": ("Lazarus Pool 找到的区块", [
            "本矿池在 BLAKE2b 比特币（XBT / BTCB2）链上找到的每一个区块都列在下方，含高度、出块时间和支付金额。由于支付走的是拆分 coinbase 而不是矿池余额，这里每一个区块<em>本身</em>就是一次发工资：打开它就能看到它产生的输出以及收款地址。",
            "这让整段支付历史任何人都能自行核对，不必向我们索取任何东西。找一个你当时在挖的区块，打开它的 coinbase，你的地址应当出现在其中，金额对应你当时在 <a href=\"/tides\">TIDES 窗口</a>里的占比并扣除手续费。这项核对完全不依赖我们的数据库重算——它就在链上。<a href=\"/non-custodial\">为什么这里没有余额需要对账</a>。",
        ]),
    },
    "/api": {
        "en": ("Lazarus Pool public API", [
            "Everything the site draws itself with is a public JSON endpoint. No key, no account, no signature, CORS open, and the same numbers the pages show. Responses carry short cache headers; honour them and you can poll as often as you like.",
            _API_TABLE_EN,
            "Two of these take an address and are deliberately excluded from search indexing, since one page per address is infinite and identical: <code>/api/miner/&lt;address&gt;</code> and <code>/api/solo/&lt;address&gt;</code>. If you are building something on this and need a field that is not there, the pool server is one Python file and the endpoint you want is probably a few lines — say so and it can be added.",
        ]),
        "zh": ("Lazarus Pool 公开 API", [
            "本站自己画图用的一切都是公开 JSON 接口。不需要 key、不需要账户、不需要签名，CORS 全开，数字与页面上显示的完全一致。响应带有较短的缓存头；遵守它，你想多频繁拉取都可以。",
            _API_TABLE_ZH,
            "其中两个要带地址，并且故意不做搜索收录，因为按地址生成的页面是无限多且结构相同的：<code>/api/miner/&lt;地址&gt;</code> 和 <code>/api/solo/&lt;地址&gt;</code>。如果你在这之上做东西、需要某个还没有的字段，矿池服务端就是一个 Python 文件，你要的接口大概只是几行——说一声就能加。",
        ]),
    },
}

_SEO_ALIASES = {"/index.html": "/"}
for _p in _SEO_PAGES:
    if _p != "/":
        _SEO_ALIASES[_p + "/"] = _p
_SEO_ALIASES["/bip-110"] = "/bip110"
_SEO_ALIASES["/mine-btcb2"] = "/mine-xbt"

# A plain-markdown protocol reference. Nobody else documents how a BLAKE2b share on this chain is
# actually derived, so this is the page a developer or a model looking for it should land on, and
# markdown is the form both read most reliably.
_STRATUM_DOC_PATH = "/stratum-protocol.md"

_STRATUM_DOC = """# Stratum on the BLAKE2b Bitcoin chain (XBT / BTCB2)

Endpoint: `stratum+tcp://stratum.awokenlazarus.xyz:23334`
Username: the address to be paid, optionally `address.worker`. Password: `x`.
There is no account and no registration; the username is the payout instruction.

This is stratum v1 over a newline-delimited JSON socket, but the work is a Sia-style
80-byte header hashed with BLAKE2b-256 rather than a Bitcoin header hashed with
SHA-256d. A stock Siacoin ASIC already does exactly this, which is why it needs no
firmware change to mine here. An SHA-256 miner cannot produce a valid share at all.

## Messages

    -> {"id":1,"method":"mining.subscribe","params":["your-miner/0.1"]}
    <- {"id":1,"result":[[notifications],"<extranonce1 hex>",<extranonce2 size>]}
    -> {"id":2,"method":"mining.authorize","params":["<payout address>","x"]}
    -> {"id":3,"method":"mining.suggest_difficulty","params":[<n>]}
    <- {"method":"mining.set_difficulty","params":[<n>]}
    <- {"method":"mining.notify","params":[job_id, prevhash, coinb1, ..., ntime, clean_jobs]}
    -> {"id":n,"method":"mining.submit",
        "params":["<payout address>", job_id, "<extranonce2 hex>", "<ntime hex>", "<nonce hex>"]}

`ntime` is `params[7]` of the notify and is 8 bytes (16 hex chars), not Bitcoin's 4.
A 4-byte value is accepted and zero-padded on the right. `clean_jobs` is `params[8]`.
New sessions start at difficulty 4096 and vardiff moves toward your hashrate from there.

## Building the work

`coinb1` is exactly 39 bytes. `extranonce1 + extranonce2` is exactly 12 bytes.

    sia_prev = tagged_sha256("Bitcoin prevblock header, hashed", reverse(prevhash))
    sia_prev[0:6] = 00 00 00 00 00 00
    root     = blake2b256(0x00 || coinb1[39] || extranonce[12])
    header   = sia_prev[32] || nonce[8] || ntime[8] || root[32]      # 80 bytes
    pow      = reverse(blake2b256(header))

where `tagged_sha256(tag, data) = sha256(sha256(tag) || sha256(tag) || data)`.

If the pool already sends a Sia prevhash in `mining.notify` — its first six bytes are
zero — use it as it stands rather than hashing it again. DATUM gateways do this.

## Share target

The target is a floor-power-of-two ladder, as on Sia, not Bitcoin's compact encoding:
take `bits = difficulty.bit_length() - 1`, start from `ff * 28 || 00 * 4`, and shift
right by `bits`. Above `2**224` it falls back to the Bitcoin-style target. A share is
valid when `pow <= target`.

## What makes a share acceptable here

Payouts are a split coinbase, so the pool hands every gateway the same coinbase output
list before any work goes out, and a share whose coinbase pays anything other than that
list is refused. The current list is public at `/api/coinbaser` — it is the next block's
payout, published before the block exists. See SITEPLACEHOLDER/tides.

## Running your own templates

A Bitcoin Knots node plus a DATUM gateway pointed here means you build the block
template and choose its transactions; the pool only supplies the coinbase split and
verifies shares. That path pays no fee and is credited a share of the public stratum's
fee on every block found. See SITEPLACEHOLDER/connect and SITEPLACEHOLDER/datum-subsidy.

## Machine-readable pool data

`/api/pool`, `/api/coinbaser`, `/api/blocks`, `/api/found/<blockhash>`, `/api/miners`,
`/api/gateways`, `/api/payouts`, `/api/hardware` — public JSON, no key.
Full reference: SITEPLACEHOLDER/api
"""


_LLMS_TXT = """# Lazarus Pool

> A mining pool for Bitcoin (XBT / BTCB2), the BIP-110 Bitcoin fork that mines with BLAKE2b instead
> of SHA-256d. Siacoin ASICs work unchanged. TIDES pays every miner inside the coinbase of each
> block found, so the pool never holds anyone's coins.

## Firsts

- First pool confirmed to pay TIDES as a split coinbase on BLAKE2b Bitcoin (XBT / BTCB2) mainnet.
  Not a testnet demo and not a proposal — the payouts are outputs of real blocks, checkable in the
  explorer at https://mempool.awokenlazarus.xyz/mining/pool/lazarus
- First pool anywhere to use its stratum hashers to subsidize DATUM miners: part of the public
  stratum's fee is credited to miners running their own DATUM gateway, on every block.
- Long-standing position on decentralization: Lazarus Pool pays miners for securing the network and
  building their own block templates rather than charging them for the privilege.

## Facts

- Site: {site}/
- How to mine: {site}/mine-xbt
- Hardware (Siacoin ASICs, with earnings estimates): {site}/hardware
- Earnings calculator: {site}/calculator
- Is it profitable: {site}/profitability
- The BIP-110 fork: {site}/bip110
- TIDES payouts: {site}/tides
- DATUM subsidy: {site}/datum-subsidy
- Pools on this chain, compared: {site}/pools
- The 25% self-cap: {site}/self-cap
- Why there is no pool balance: {site}/non-custodial
- Stratum protocol reference (markdown): {site}/stratum-protocol.md
- Public API reference: {site}/api
- Explorer: https://mempool.awokenlazarus.xyz
- GitHub: https://github.com/AwokenLazarus/Bitcoin
- Discord: https://discord.gg/fD33dJXnzz
- Chinese: any URL under /zh/, e.g. {site}/zh/hardware

## What XBT / BTCB2 is

Two separate events, often run together. BIP-110 is a proposal to restrict non-financial data in
transactions; it needed 55% of hashrate, peaked near 2.6%, and its nodes began rejecting blocks that
did not signal at height 961,632 on 8 August 2026 — that is the split, and this chain is the BIP-110
branch. Then on 30 August 2026 this branch changed proof-of-work from SHA-256d to BLAKE2b with a
Sia-style header: block 961,639 was the last SHA-256d block, 961,640 the first BLAKE2b one.

Everything else — 21 million cap, halving schedule, script, addresses, and all history before the
split — is Bitcoin. The chain is called Bitcoin, the ticker is XBT, and exchanges list it as BTCB2.
It is not Bitcoin Cash, not a token, and not an altcoin on a new codebase; it is the same code with a
different hash function, which is why the ASICs that mine Siacoin mine it too. Pre-split history is
shared with Bitcoin, so use a fresh address here.

## Connect

- Public stratum: stratum+tcp://stratum.awokenlazarus.xyz:23334
- Username: your XBT payout address (bc1…)
- Password: x
- Algorithm: BLAKE2b (Sia-style header). Not SHA-256d.
- DATUM Prime: stratum.awokenlazarus.xyz:28915 (0% fee; run your own gateway)

## Fees

- Own DATUM gateway: 0%, plus the DATUM subsidy taken from the public-stratum fee
- Public stratum: 10% (5 points of that credited back to DATUM miners)
- Solo: 5%

## Hardware

Any ASIC that can mine Siacoin can mine Bitcoin XBT / BTCB2 — same BLAKE2b algorithm, no firmware
change. Examples: Goldshell SC-series (SC6-SE, SC-BOX), iBeLink BM-S3 / BM-S3+ / BM-N3, Antminer A3.
SHA-256 miners (Antminer S19/S21, Whatsminer M30/M50) will not work. Idle Siacoin rigs are the
cheapest route into profitable crypto mining on this chain because difficulty is still low relative
to the block subsidy.

## Payouts and decentralization

TIDES pays out a share of a rolling window worth about eight times network difficulty, inside the
coinbase of the block that is found — the split coinbase Lazarus Pool was first to run on this
mainnet. There is no pool balance, no withdrawal, no minimum, and no custody. Miners running their
own DATUM gateway build their own block templates against their own Bitcoin Knots node, so template
construction is decentralized rather than delegated to the pool, and the DATUM subsidy — funded by
the pool's own stratum hashers, another first — pays them extra for doing it. Coins trade as BTCB2
on Neoxa and NonKYC.

## The 25% self-cap

Lazarus Pool holds itself to 25% of network hashrate. Over that line a new stratum connection is
not mined on the pool's behalf: it is relayed to one of four other pools and paid by that pool,
under the same address, with nothing credited to the Lazarus window. Miners already connected keep
mining here, and own-gateway DATUM miners are never relayed because a miner building their own
templates is not what centralizes a chain. This is enforced per connection in the software, and
both the live network share and the number of connections currently relayed are published on the
site. In September 2026 a different BTCB2 pool passed 50% of network hashrate and a patch was
proposed to blacklist its payout address at the consensus level; no other pool on this chain
publishes a self-imposed limit. See {site}/self-cap.

## How the pools on this chain differ

Fees are the least of it. What differs is custody, what happens to the transaction fees in a found
block, and whether a pool limits its own share. As each pool published its own terms in September
2026:

- Lazarus Pool: 0% with your own DATUM gateway, 10% on the public stratum with five of those points
  paid back to DATUM miners. TIDES, window of 8x difficulty. No custody at all — the block's
  coinbase pays your address, so there is no balance, threshold or withdrawal. The block's
  transaction fees scale every miner's payout up. Self-capped at 25%.
- B2Pool: 0% with your own DATUM gateway, 1% on the stratum. TIDES, window of 8x difficulty. Paid
  from the coinbase where it fits, otherwise by the pool once your balance passes 10,000 sat. The
  block's transaction fees stay with the pool. No self-limit published.
- AlphaPool: 2.5%, PPLNS. The pool holds a balance until a block reaches 100-confirmation maturity
  and a batch cycle pays out. Treatment of transaction fees not published. Pledged a 30% cap after
  passing 50% of the network.

Honest summary: if you will never run a node, Lazarus's public stratum is the most expensive of the
three, and the 1% elsewhere is cheaper. The path Lazarus recommends and is cheapest on is your own
DATUM gateway at 0% plus the subsidy, and it is the only one of the three that never takes custody,
shares the block's transaction fees with miners, or turns hashrate away. See {site}/pools.
""".replace("{site}", "SITEPLACEHOLDER")


def _seo_lang(query):
    q = parse_qs(query or "")
    n = (q.get("lang") or [""])[0].replace("_", "-")
    if n == "zh" or n.lower().startswith("zh-"):
        return "zh"
    return "en"


def split_lang_prefix(path):
    """Peel a /zh path prefix off the URL: "/zh/hardware" -> ("zh", "/hardware")."""
    if path in ("/zh", "/zh/"):
        return "zh", "/"
    if path.startswith("/zh/"):
        return "zh", path[len("/zh") :]
    return "", path


def _seo_url(path, lang):
    """The one canonical URL for a page in a language: English at /x, Chinese at /zh/x."""
    prefix = "/zh" if lang == "zh" else ""
    return _PUBLIC_SITE + prefix + ("/" if path == "/" else path)


def _seo_page(path, query):
    prefix_lang, path = split_lang_prefix(path)
    path = _SEO_ALIASES.get(path, path)
    pack = _SEO_PAGES.get(path) or _SEO_PAGES["/"]
    # A /zh URL is an explicit choice and outranks ?lang=, which stays supported for old links.
    lang = prefix_lang or _seo_lang(query)
    meta = dict(pack.get(lang) or pack["en"])
    meta["alt_en"] = _seo_url(path, "en")
    meta["alt_zh"] = _seo_url(path, "zh")
    # Chinese served under ?lang= points its canonical at the /zh path so the two do not compete.
    meta["canonical"] = meta["alt_zh"] if lang == "zh" else meta["alt_en"]
    meta["lang"] = "zh-CN" if lang == "zh" else "en"
    meta["path"] = path
    # A page with its own opening copy should not jump straight past it; those pages link into the
    # relevant section themselves instead.
    if path in _SEO_INTRO:
        meta["scroll"] = ""
    return meta


def _xml_attr(s):
    return html.escape(str(s or ""), quote=True)


def render_pool_index(path, query=""):
    """Homepage HTML with path/lang-specific title, description, canonical, and scroll target."""
    raw = (STATIC / "index.html").read_text(encoding="utf-8")
    meta = _seo_page(path, query)
    title = meta["title"]
    desc = meta["description"]
    canon = meta["canonical"]
    # Only the homepage keeps the data-i18n hooks; on the keyword pages the client-side dictionary
    # would otherwise overwrite the page-specific title with the generic one.
    home = meta["path"] == "/"
    hook_t = ' data-i18n="meta.title"' if home else ""
    hook_d = ' data-i18n="meta.description"' if home else ""
    raw = re.sub(r"<title[^>]*>.*?</title>", f"<title{hook_t}>{html.escape(title)}</title>", raw, count=1, flags=re.S)
    raw = re.sub(
        r'<meta name="description"[^>]*>',
        f'<meta name="description"{hook_d} content="{_xml_attr(desc)}">',
        raw,
        count=1,
    )
    alts = (
        f'<link rel="canonical" href="{_xml_attr(canon)}">\n'
        f'<link rel="alternate" hreflang="en" href="{_xml_attr(meta["alt_en"])}">\n'
        f'<link rel="alternate" hreflang="zh-CN" href="{_xml_attr(meta["alt_zh"])}">\n'
        f'<link rel="alternate" hreflang="x-default" href="{_xml_attr(meta["alt_en"])}">'
    )
    raw = re.sub(r'<link rel="alternate" hreflang="[^"]*"[^>]*>\n?', "", raw)
    raw = re.sub(r'<link rel="canonical"[^>]*>', alts, raw, count=1)
    raw = re.sub(r'<meta property="og:title"[^>]*>', f'<meta property="og:title" content="{_xml_attr(title)}">', raw, count=1)
    raw = re.sub(r'<meta property="og:description"[^>]*>', f'<meta property="og:description" content="{_xml_attr(desc)}">', raw, count=1)
    raw = re.sub(r'<meta property="og:url"[^>]*>', f'<meta property="og:url" content="{_xml_attr(canon)}">', raw, count=1)
    raw = re.sub(r'<meta name="twitter:title"[^>]*>', f'<meta name="twitter:title" content="{_xml_attr(title)}">', raw, count=1)
    raw = re.sub(r'<meta name="twitter:description"[^>]*>', f'<meta name="twitter:description" content="{_xml_attr(desc)}">', raw, count=1)
    raw = raw.replace('<html lang="en" data-scroll="">', f'<html lang="{meta["lang"]}" data-scroll="{_xml_attr(meta.get("scroll") or "")}">', 1)
    return _inject_intro(raw, meta)


def _inject_intro(raw, meta):
    """Put this URL's own heading and prose at the top of the page.

    Without it every pretty URL is the same dashboard with a different <title>, which search
    engines treat as duplicates of the homepage and drop. The page-specific copy takes over the
    <h1>, so the hero heading is demoted to <h2> to keep one <h1> per page.
    """
    pack = _SEO_INTRO.get(meta["path"])
    if not pack:
        return raw
    lang = "zh" if meta["lang"] == "zh-CN" else "en"
    # The homepage has copy for Chinese only: /zh/ would otherwise ship an English <h1> and English
    # prose to any crawler that does not run the client-side dictionary.
    entry = pack.get(lang)
    if not entry:
        return raw
    title, paras = entry
    # A block already written as markup (a table, a list) is emitted as it stands; anything else is
    # prose and gets wrapped in a paragraph.
    body = "\n".join(f"      {t}" if t.startswith("<") else f"      <p>{t}</p>" for t in paras)
    block = (
        '\n  <section class="seo-intro" aria-labelledby="seo-intro-title">\n'
        '    <div class="wrap">\n'
        f'      <h1 id="seo-intro-title">{html.escape(title)}</h1>\n'
        f"{body}\n"
        "    </div>\n"
        "  </section>\n"
    )
    raw = re.sub(r"<h1 id=\"hero-title\"(.*?)</h1>", r'<h2 id="hero-title"\1</h2>', raw, count=1, flags=re.S)
    return raw.replace("<main>", "<main>" + block, 1)


# Ownership proofs for the search consoles. Served verbatim from the site root because that is the
# only place the verifiers look. These are public tokens, not secrets.
_SITE_VERIFY = {
    "/googleda1d0aa98080ef94.html": (
        "google-site-verification: googleda1d0aa98080ef94.html",
        "text/html; charset=utf-8",
    ),
    # Bing fetches this path exactly as spelled, so it is matched case-sensitively first and the
    # lowercase spelling is aliased below for anything that normalises the URL.
    "/BingSiteAuth.xml": (
        '<?xml version="1.0"?>\n<users>\n\t<user>24AA86BB5477F9AB2254252ADF5A1770</user>\n</users>',
        "text/xml; charset=utf-8",
    ),
}
_SITE_VERIFY["/bingsiteauth.xml"] = _SITE_VERIFY["/BingSiteAuth.xml"]


# IndexNow lets us tell Bing, Yandex and friends to recrawl within hours instead of waiting for
# them to come round. The key is public by design: they fetch it back from the path below to prove
# whoever submitted the URLs controls the host.
_INDEXNOW_KEY = "c812dd6a130b16b2f5e881a8f8865809"
_INDEXNOW_PATH = f"/{_INDEXNOW_KEY}.txt"


# Crawl order, refresh rate and priority per page. Highest-intent pages first: a searcher asking
# how to mine, what it earns, or which pool to use is worth more than one browsing the API docs.
_SITEMAP_ORDER = (
    ("/", "hourly", "1.0"),
    ("/mine-xbt", "weekly", "0.9"),
    ("/hardware", "daily", "0.9"),
    ("/profitability", "daily", "0.9"),
    ("/calculator", "daily", "0.9"),
    ("/pools", "weekly", "0.9"),
    ("/connect", "weekly", "0.8"),
    ("/datum-subsidy", "weekly", "0.8"),
    ("/self-cap", "weekly", "0.8"),
    ("/non-custodial", "weekly", "0.8"),
    ("/tides", "weekly", "0.8"),
    ("/bip110", "weekly", "0.8"),
    ("/blocks", "hourly", "0.7"),
    ("/how", "weekly", "0.7"),
    ("/api", "monthly", "0.6"),
)


def indexnow_urls():
    """Every URL worth submitting, English and Chinese."""
    out = []
    for p, _freq, _prio in _SITEMAP_ORDER:
        out.append(_seo_url(p, "en"))
        out.append(_seo_url(p, "zh"))
    return out


def robots_txt():
    return (
        "User-agent: *\n"
        "Allow: /\n"
        "\n"
        "# The API reference is a page a developer should be able to find; the JSON endpoints under\n"
        "# it have nothing to read and would only burn crawl budget. Longest match wins, so /api\n"
        "# stays allowed while everything below it is blocked.\n"
        "Allow: /api\n"
        "Disallow: /api/\n"
        "\n"
        "# One page per miner address, instantiable without limit and identical in structure for\n"
        "# every address. Nothing to index; still linked and reachable for the miner who wants it.\n"
        "Disallow: /miner\n"
        "Disallow: /zh/miner\n"
        "\n"
        f"Sitemap: {_PUBLIC_SITE}/sitemap.xml\n"
    )


def _lastmod(freq):
    """When this URL last changed, as a date.

    Pages built from live pool data genuinely change every day, so they carry today. The rest
    change when the site is deployed, which is the mtime of the file their copy lives in.
    """
    if freq in ("hourly", "daily"):
        return time.strftime("%Y-%m-%d", time.gmtime())
    try:
        newest = max(Path(__file__).stat().st_mtime, (STATIC / "index.html").stat().st_mtime)
    except OSError:
        newest = time.time()
    return time.strftime("%Y-%m-%d", time.gmtime(newest))


def sitemap_xml():
    """Every crawlable page in both languages, each declaring the other as its alternate."""
    urls = []
    for p, freq, prio in _SITEMAP_ORDER:
        en, zh = _seo_url(p, "en"), _seo_url(p, "zh")
        alts = (
            f'<xhtml:link rel="alternate" hreflang="en" href="{_xml_attr(en)}"/>'
            f'<xhtml:link rel="alternate" hreflang="zh-CN" href="{_xml_attr(zh)}"/>'
            f'<xhtml:link rel="alternate" hreflang="x-default" href="{_xml_attr(en)}"/>'
        )
        for loc in (en, zh):
            urls.append(
                f"  <url><loc>{_xml_attr(loc)}</loc><lastmod>{_lastmod(freq)}</lastmod>"
                f"{alts}<changefreq>{freq}</changefreq><priority>{prio}</priority></url>"
            )
    # The protocol reference is one English document with no Chinese twin, so it declares no
    # alternates rather than pointing hreflang at a page that does not exist.
    urls.append(
        f"  <url><loc>{_xml_attr(_PUBLIC_SITE + _STRATUM_DOC_PATH)}</loc>"
        f"<lastmod>{_lastmod('weekly')}</lastmod><changefreq>monthly</changefreq><priority>0.7</priority></url>"
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">\n'
        + "\n".join(urls)
        + "\n</urlset>\n"
    )


def llms_txt():
    return _LLMS_TXT.replace("SITEPLACEHOLDER", _PUBLIC_SITE)


def stratum_doc():
    return _STRATUM_DOC.replace("SITEPLACEHOLDER", _PUBLIC_SITE)


# Paths cheap enough to answer while the slot table is full.
_CHEAP_PATHS = frozenset(
    (
        "/",
        "/index.html",
        "/hardware",
        "/connect",
        "/mine-xbt",
        "/how",
        "/bip110",
        "/datum-subsidy",
        "/profitability",
        "/tides",
        "/pools",
        "/self-cap",
        "/non-custodial",
        "/calculator",
        "/blocks",
        "/api",
        "/robots.txt",
        "/sitemap.xml",
        "/llms.txt",
        "/.well-known/llms.txt",
        _STRATUM_DOC_PATH,
        _INDEXNOW_PATH,
        *_SITE_VERIFY,
        "/api/pool",
        "/api/miners",
        "/api/coinbaser",
        "/api/solo",
        "/api/price",
        "/api/v1/prices",
        "/api/payouts",
        "/api/hardware",
    )
)


class Handler(BaseHTTPRequestHandler):
    # Socket timeout per request: a client that opens a connection and trickles bytes
    # (slowloris) otherwise holds a thread forever on this thread-per-connection server.
    timeout = 20

    def log_message(self, fmt, *args):
        return

    def handle_one_request(self):
        """One request, with the expensive-work slot held only while it is being answered.

        The throttle used to wrap `handle()`, which spans the whole connection: the proxy
        keeps a pool of connections open, so a slot was held from accept until the socket
        timed out, idle or not. Roughly 24 upstream connections then pinned every slot and
        `/api/miner` answered 503 while the box was doing nothing. Reading the request line
        is the part that blocks on an idle peer, so it stays outside the slot."""
        try:
            self.raw_requestline = self.rfile.readline(65537)
            if len(self.raw_requestline) > 65536:
                self.requestline = ""
                self.request_version = ""
                self.command = ""
                self.send_error(414)
                return
            if not self.raw_requestline:
                self.close_connection = True
                return
            if not self.parse_request():
                return
            method = getattr(self, "do_" + self.command, None)
            if method is None:
                self.send_error(501, f"Unsupported method ({self.command!r})")
                return
            path = unquote(urlparse(self.path).path)
            if split_lang_prefix(path)[1] in _CHEAP_PATHS or path.startswith("/static/"):
                method()
            elif _inflight.acquire(blocking=False):
                try:
                    method()
                finally:
                    _inflight.release()
            else:
                self._shed(path)
            self.wfile.flush()
        except (TimeoutError, socket.timeout):
            self.close_connection = True

    def _shed(self, path):
        """Slot table full. An address page is what a miner came here for: if one has
        already been built for it, hand that over rather than a 503."""
        stale = None
        for prefix, bucket in (("/api/miner/", "miner"), ("/api/solo/", "solo")):
            if path.startswith(prefix):
                addr = path.split(prefix, 1)[1].strip("/")
                if _ADDRESS_RE.match(addr):
                    stale = cache_peek((bucket, addr))
                break
        if stale is not None:
            self.send_json(stale, cache_s=5)
        else:
            self.send_json({"error": "busy"}, 503)

    def send_json(self, obj, code=200, cache_s=0):
        body = json.dumps(obj, separators=(",", ":")).encode()
        enc = (self.headers.get("Accept-Encoding") or "").lower()
        use_gzip = code == 200 and "gzip" in enc and len(body) >= 400
        if use_gzip:
            body = gzip.compress(body, compresslevel=1)
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        if use_gzip:
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Vary", "Accept-Encoding")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("X-Content-Type-Options", "nosniff")
        if cache_s > 0:
            self.send_header(
                "Cache-Control",
                f"public, max-age=0, s-maxage={int(cache_s)}, stale-while-revalidate={int(cache_s) * 6}",
            )
        else:
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self._write_body(body)

    def send_text(self, text, ctype, cache_s=0):
        body = text.encode("utf-8")
        enc = (self.headers.get("Accept-Encoding") or "").lower()
        use_gzip = "gzip" in enc and len(body) >= 400
        if use_gzip:
            body = gzip.compress(body, compresslevel=1)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        if use_gzip:
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Vary", "Accept-Encoding")
        self.send_header("X-Content-Type-Options", "nosniff")
        if cache_s > 0:
            self.send_header("Cache-Control", f"public, max-age=0, s-maxage={int(cache_s)}")
        else:
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self._write_body(body)

    def send_file(self, path, ctype):
        data = Path(path).read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Content-Type-Options", "nosniff")
        if Path(path).suffix.lower() in {".js", ".css", ".svg", ".png", ".ico", ".wasm", ".woff2"}:
            self.send_header("Cache-Control", "public, max-age=600")
        else:
            self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self._write_body(data)

    def _write_body(self, body):
        """HEAD gets the headers and nothing else, so every send_* helper goes through here."""
        if getattr(self, "_head_only", False):
            return
        self.wfile.write(body)

    def do_HEAD(self):
        # Crawlers, link checkers and sitemap fetchers often HEAD before GET. Without this the
        # stdlib answers 501, which reads as "couldn't fetch" in Search Console.
        self._head_only = True
        try:
            self.do_GET()
        finally:
            self._head_only = False

    def do_GET(self):
        u = urlparse(self.path)
        path = unquote(u.path)
        if path in ("/api/price", "/api/v1/prices"):
            self.send_json(price_payload() if path == "/api/price" else mempool_prices_payload(), cache_s=15)
            return
        if path == "/api/pool":
            self.send_json(cached("pool", 5.0, pool_payload), cache_s=5)
            return
        if path == "/api/miners":
            self.send_json(cached("miners", 5.0, self._miners_payload), cache_s=5)
            return
        if path.startswith("/api/miner/"):
            addr = path.split("/api/miner/", 1)[1].strip("/")
            if not _ADDRESS_RE.match(addr):
                self.send_json({"error": "not found"}, 404)
                return
            self.send_json(cached(("miner", addr), 5.0, lambda: miner_payload(addr)), cache_s=5)
            return
        if path == "/api/blocks":
            self.send_json(cached("blocks", 15.0, lambda: {"blocks": mempool_blocks()}), cache_s=15)
            return
        if path == "/api/coinbaser":
            self.send_json(cached("coinbaser", 5.0, prime_coinbaser_preview), cache_s=5)
            return
        if path == "/api/solo":
            self.send_json(cached("solo", 5.0, solo_payload), cache_s=5)
            return
        if path.startswith("/api/solo/"):
            addr = path.split("/api/solo/", 1)[1].strip("/")
            if not _ADDRESS_RE.match(addr):
                self.send_json({"error": "not found"}, 404)
                return
            self.send_json(cached(("solo", addr), 5.0, lambda: solo_miner_payload(addr)), cache_s=5)
            return
        if path == "/api/gateways":
            self.send_json(cached("gateways", 5.0, self._gateways_payload), cache_s=5)
            return
        if path == "/api/payouts":
            self.send_json(cached("payouts", 30.0, self._payouts_payload), cache_s=30)
            return
        if path == "/api/hardware":
            self.send_json(cached("hardware", 15.0, hardware_payload), cache_s=15)
            return
        if path.startswith("/api/found/"):
            hx = path.split("/api/found/", 1)[1].strip("/")
            if not _BLOCKHASH_RE.match(hx):
                self.send_json({"error": "not found"}, 404)
                return
            doc = cached(("found", hx), 30.0, lambda: found_outputs_payload(hx))
            if not doc:
                self.send_json({"error": "not found"}, 404)
                return
            self.send_json(doc, cache_s=30)
            return
        if path == "/robots.txt":
            self.send_text(robots_txt(), "text/plain; charset=utf-8", cache_s=3600)
            return
        if path == "/sitemap.xml":
            self.send_text(sitemap_xml(), "application/xml; charset=utf-8", cache_s=3600)
            return
        if path in _SITE_VERIFY:
            proof, ctype = _SITE_VERIFY[path]
            self.send_text(proof, ctype, cache_s=86400)
            return
        if path == _INDEXNOW_PATH:
            self.send_text(_INDEXNOW_KEY, "text/plain; charset=utf-8", cache_s=86400)
            return
        if path in ("/llms.txt", "/.well-known/llms.txt"):
            self.send_text(llms_txt(), "text/plain; charset=utf-8", cache_s=3600)
            return
        if path == _STRATUM_DOC_PATH:
            self.send_text(stratum_doc(), "text/markdown; charset=utf-8", cache_s=3600)
            return
        bare = split_lang_prefix(path)[1]
        if _SEO_ALIASES.get(bare, bare) in _SEO_PAGES:
            # One page, many crawlable URLs: each gets its own title, description and canonical.
            self.send_text(render_pool_index(path, u.query), "text/html; charset=utf-8", cache_s=300)
            return
        if bare.startswith("/miner/") or bare in ("/miner", "/miner.html"):
            # Dedicated miner page; the address is read from the URL client-side.
            self.send_file(STATIC / "miner.html", "text/html; charset=utf-8")
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

    @staticmethod
    def _gateways_payload():
        pr = prime_summary()
        return {"reachable": pr["reachable"], "gateways": pr["gateways"], "totals": pr["totals"]}

    @staticmethod
    def _miners_payload():
        online = online_miners()
        seen = db("SELECT * FROM miners ORDER BY last_ts DESC LIMIT 200")
        addrs = [r["address"] for r in (seen or []) if r["address"]]
        shares_by = {}
        if addrs:
            ph = ",".join("?" * len(addrs))
            for row in (
                db(
                    f"SELECT address, COALESCE(SUM(lifetime_acc),0) AS a, COALESCE(SUM(lifetime_rej),0) AS r, "
                    f"COALESCE(SUM(last_shares_acc),0) AS s FROM worker_shares WHERE address IN ({ph}) GROUP BY address",
                    tuple(addrs),
                )
                or []
            ):
                shares_by[row["address"]] = (int(row["a"]), int(row["r"]), int(row["s"]))
        stratum_online = {o.get("address") for o in online if o.get("via") == "stratum"}
        prime_ids = state.get("prime") or {}
        seen_out = []
        for r in seen or []:
            d = dict(r)
            addr = d.get("address") or ""
            life_a, life_r, sess = shares_by.get(addr, (0, 0, 0))
            info = prime_info_for(addr)
            d["shares_lifetime"] = life_a or int(info.get("window_work") or d.get("shares_lifetime") or d.get("shares_acc") or 0)
            d["shares_session"] = int(sess or d.get("shares_session") or 0)
            d["shares_acc"] = d["shares_lifetime"]
            d["shares_rej"] = life_r or int(d.get("shares_rej") or 0)
            d["window_work"] = int(info.get("window_work") or 0)
            d["window_percent"] = float(info.get("window_percent") or 0)
            d["window_sats"] = int(info.get("window_sats") or 0)
            d["window_shares"] = int(info.get("window_shares") or info.get("credits") or 0)
            d["fee_path"] = info.get("fee_path") or ""
            d["via"] = "prime" if (addr in prime_ids and addr not in stratum_online) else d.get("via")
            try:
                if float(d.get("best_hr_ghs") or 0) > _PRIME_HR_CAP_GHS:
                    d["best_hr_ghs"] = _PRIME_HR_CAP_GHS
            except (TypeError, ValueError):
                pass
            d["hr_ghs"] = float(info.get("hr_ghs") or 0)
            seen_out.append(d)
        names = gateway_names_by_address()
        online = stamp_gateway_names(rollup_online_by_address(online), names)
        stamp_gateway_names(seen_out, names)
        ov = overflow_doc()
        return {"online": online, "seen": seen_out, "relayed": ov.get("proxied") or [], "overflow": ov.get("overflow")}

    @staticmethod
    def _payouts_payload():
        if True:
            tip = rpc("getblockcount") or 0
            # Enough history that coinbases past 100 confs show as spendable, not a window
            # of only-immature recent blocks (the pool finds ~20–40/day).
            fbs = db("SELECT height, hash, ts, reward_btc FROM found_blocks ORDER BY height DESC LIMIT 250") or []
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
                confs = (int(tip) - int(fb["height"]) + 1) if tip and fb["height"] else 0
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
                            "confirmations": confs,
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
                row["owed_txid"] = (pb.get("owed_txid") if pb else "") or ""
                row["owed_resolved"] = bool(pb.get("owed_resolved")) if pb else False
                row["gateway"] = pb["gateway"] if pb else ""
                row["found_by"] = pb["finder"] if pb else ""
                if not row.get("owed_txid"):
                    apply_owed_settlement(row)
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
            payouts = _collapse_found_payouts(tagged)
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
            return {
                "scheme": "TIDES",
                "fee_percent": POOL_FEE,
                "maturity_blocks": MATURITY_CONFS,
                "tip": int(tip or 0),
                "current_round": current,
                "payouts": payouts,
                "prime_blocks": _public_prime_blocks(prime_blocks.values()),
            }


def main():
    if not NO_WRITE:
        ensure_open_round()
        init_share_accounting()
    threading.Thread(target=loop, daemon=True).start()
    host = CONF.get("listen_host", "0.0.0.0")
    port = int(os.environ.get("POOL_LISTEN_PORT") or CONF.get("listen_port", 8888))
    print(f"lazarus-pool http://{host}:{port}", flush=True)
    PoolHTTPServer((host, port), Handler).serve_forever()


if __name__ == "__main__":
    main()
