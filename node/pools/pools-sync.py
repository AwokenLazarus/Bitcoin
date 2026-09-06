#!/usr/bin/env python3
"""Keep the mempool explorer's mining-pool list and block attribution current.

Sources, merged by slug (name with non-alphanumerics stripped, lower-cased, the same rule
the mempool backend uses):

  1. Kilombino's BLAKE2b pools-v2.json -- community list, mostly payout addresses, a few tags.
  2. mempool.guide's pool definitions   -- their tag regexes, fetched per slug from their
     weekly pool list and cached (refreshed slowly; their API is not quick).
  3. pools-overrides.json (ours)        -- Lazarus with the pool payout address, extra tags,
     entries to drop, and which tag-only entries are "generic" software identifiers.

Outputs:

  * pools-v2.json for the backend (served on :8765, see serve-pools.sh), ordered by match
    priority so a fresh backend import tags blocks the same way this script does.
  * the `pools` table upserted by slug (unique_id kept stable for existing rows).
  * blocks from START_HEIGHT re-attributed where the priority match differs from the stored
    pool_id, and the API disk cache patched so the UI shows the new names without a restart.

Match priority (a block can carry both a pool address and a tag, so order matters):

  0  Lazarus                         pool payout address or tag
  1  other pools ("pool": true)      PyBLOCK: the pool tag beats a member's payout address
  2  named tag pools                 a coinbase tag someone chose (AlphaPool, Titus 1:15, ...)
  3  named address pools             Kilombino entries with a real name
  4  "Solo <addr>" address pools     Kilombino auto-names, just an address
  5  generic tag pools               DATUM / Knots / blake2b-mainnet software strings

The backend matches a new block against `SELECT ... FROM pools`, i.e. in row-id order, so the
priority above is encoded in the ids themselves: each priority owns an id band (Lazarus 1-99,
shared pools 100-999, tag pools 1000-9999, address pools 10000-59999, generic 60000+, Unknown
65500). New pools are inserted at the next free id in their band, and when a pool sits outside
its band (first run, or an override promoted it) the table is renumbered in one transaction
with blocks.pool_id / hashrates.pool_id remapped. That way the backend tags blocks correctly
on arrival, including the in-memory recent-blocks window that no DB fix-up can reach; the
re-attribution pass below stays as a safety net for blocks tagged before a list change. Pools
that are in the DB but no longer in the merged list (dropped via overrides) get their matchers
blanked so they cannot claim blocks.

Runs from a systemd user timer; network fetches are cached and rate-limited so a run is
normally a DB pass only. Env: POOLS_DIR, MEMPOOL_DB_HOST/USER/PASS/NAME, MEMPOOL_CACHE,
START_HEIGHT. `--dry-run` prints what the DB pass would change without writing.
"""
import json
import os
import re
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

POOLS_DIR = Path(os.environ.get("POOLS_DIR", os.path.expanduser("~/blake2b/pools")))
OUT_JSON = POOLS_DIR / "pools-v2.json"
OVERRIDES = POOLS_DIR / "pools-overrides.json"
STATE = POOLS_DIR / "pools-state.json"
KILOMBINO_URL = "https://raw.githubusercontent.com/Kilombino/mempool-bip110/main/pools-v2.json"
GUIDE = "https://mempool.guide/api/v1/mining"
KILOMBINO_TTL = 6 * 3600
GUIDE_LIST_TTL = 24 * 3600
GUIDE_POOL_TTL = 7 * 24 * 3600
GUIDE_FETCH_BUDGET = 25          # per run; their API answers in ~1-3 s per pool
START_HEIGHT = int(os.environ.get("START_HEIGHT", "961640"))
CACHE_JSON = Path(os.environ.get("MEMPOOL_CACHE", "/home/umbrel/umbrel/app-data/mempool/data/cache.json"))
DB = dict(host=os.environ.get("MEMPOOL_DB_HOST", "10.21.21.28"),
          user=os.environ.get("MEMPOOL_DB_USER", "mempool"),
          password=os.environ.get("MEMPOOL_DB_PASS", "mempool"),
          database=os.environ.get("MEMPOOL_DB_NAME", "mempool"))
DRY_RUN = "--dry-run" in sys.argv[1:]

# pools.id is smallint unsigned; the backend walks pools in id order (see module docstring).
BANDS = {0: (1, 99), 1: (100, 999), 2: (1000, 9999), 3: (10000, 29999), 4: (30000, 59999), 5: (60000, 61999)}
UNKNOWN_ID = 65500
SCRATCH_BASE = 62000            # 62000-65499: transient ids while renumbering, never final


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def slugify(name):
    return re.sub(r"[^a-z0-9]", "", name.lower())


def fetch_json(url, timeout=20):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return json.load(r)


def load_state():
    try:
        return json.loads(STATE.read_text())
    except Exception:
        return {}


def save_state(st):
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(st, indent=1))
    tmp.replace(STATE)


# --- sources ---------------------------------------------------------------------------------

def source_kilombino(st):
    cache = st.setdefault("kilombino", {})
    if time.time() - cache.get("fetched", 0) > KILOMBINO_TTL:
        try:
            data = fetch_json(KILOMBINO_URL)
            if isinstance(data, list) and len(data) > 50:
                cache.update(fetched=time.time(), data=data)
                log(f"kilombino: {len(data)} entries")
        except Exception as e:
            log("kilombino fetch failed, using cache:", e)
            cache["fetched"] = time.time() - KILOMBINO_TTL + 900   # retry in 15 min
    return cache.get("data", [])


def source_guide(st):
    """mempool.guide pool definitions: refresh their 1w slug list daily, then fill/refresh
    per-pool definitions a few at a time."""
    cache = st.setdefault("guide", {"slugs": [], "pools": {}})
    now = time.time()
    if now - cache.get("list_fetched", 0) > GUIDE_LIST_TTL:
        try:
            lst = fetch_json(f"{GUIDE}/pools/1w", timeout=30)
            slugs = [p["slug"] for p in lst.get("pools", []) if p.get("slug")]
            if slugs:
                cache["slugs"] = sorted(set(cache.get("slugs", [])) | set(slugs))
                cache["list_fetched"] = now
                log(f"guide: {len(slugs)} pools this week, {len(cache['slugs'])} known")
        except Exception as e:
            log("guide list failed:", e)
            cache["list_fetched"] = now - GUIDE_LIST_TTL + 1800
    budget = GUIDE_FETCH_BUDGET
    pools = cache["pools"]
    due = [s for s in cache.get("slugs", []) if now - pools.get(s, {}).get("fetched", 0) > GUIDE_POOL_TTL]
    due.sort(key=lambda s: pools.get(s, {}).get("fetched", 0))
    for s in due[:budget]:
        try:
            p = fetch_json(f"{GUIDE}/pool/{urllib.parse.quote(s)}", timeout=10)["pool"]
            pools[s] = {"fetched": now, "name": p.get("name", ""), "link": p.get("link", ""),
                        "addresses": p.get("addresses") or [], "regexes": p.get("regexes") or []}
        except Exception as e:
            pools.setdefault(s, {})["fetched"] = now - GUIDE_POOL_TTL + 6 * 3600
            log(f"guide pool {s} failed:", e)
    if due:
        log(f"guide: refreshed {min(budget, len(due))} definitions, {max(0, len(due) - budget)} pending")
    return [p for p in pools.values() if p.get("name")]


def load_overrides():
    try:
        return json.loads(OVERRIDES.read_text())
    except Exception as e:
        log("overrides unreadable:", e)
        return {"pools": [], "drop": [], "generic": []}


# --- merge -------------------------------------------------------------------------------------

def merge(kilombino, guide, ov):
    pools = {}      # slug -> entry
    order = []

    def add(name, link, addresses, tags, replace=False, rename=False):
        slug = slugify(name)
        if not slug:
            return
        e = pools.get(slug)
        if e is None or replace:
            if e is None:
                order.append(slug)
            e = pools[slug] = {"name": name.strip(), "link": link or "", "addresses": [], "tags": []}
        elif rename:
            e["name"] = name.strip()     # mempool.guide's display names are the curated ones
        if link and not e["link"]:
            e["link"] = link
        for a in addresses or []:
            if a and a not in e["addresses"]:
                e["addresses"].append(a)
        for t in tags or []:
            if t and t not in e["tags"]:
                e["tags"].append(t)

    for p in kilombino:
        add(p.get("name", ""), p.get("link", ""), p.get("addresses"), p.get("tags"))
    for p in guide:
        link = p.get("link", "")
        if link == "https://example.com":
            link = ""
        add(p["name"], link, p.get("addresses"), p.get("regexes"), rename=True)
    for p in ov.get("pools", []):
        add(p["name"], p.get("link", ""), p.get("addresses"), p.get("tags"), replace=p.get("replace", False))

    for slug in ov.get("drop", []):
        pools.pop(slug, None)

    generic = set(ov.get("generic", []))
    shared = {slugify(p["name"]) for p in ov.get("pools", []) if p.get("pool")}

    def prio(slug):
        e = pools[slug]
        if slug in ("lazarussolo", "lazarus"):
            return 0
        if slug in shared:          # a pool's blocks also pay its members' addresses
            return 1
        if slug in generic:
            return 5
        if e["tags"]:
            return 2
        if e["name"].lower().startswith("solo "):
            return 4
        return 3

    ordered = [s for s in order if s in pools]
    pos = {s: i for i, s in enumerate(ordered)}
    # Within a band the more specific matcher has to come first. "Lazarus/solo" contains
    # "Lazarus", so without this a block found by a solo miner would be credited to the
    # pool, whose window earned nothing from it.
    within = {"lazarussolo": 0, "lazarus": 1}
    ordered.sort(key=lambda s: (prio(s), within.get(s, 2), pos[s]))
    return [(s, pools[s], prio(s)) for s in ordered]


# --- database ----------------------------------------------------------------------------------

def db_connect():
    import pymysql
    return pymysql.connect(**DB, autocommit=False)


def band_of(prio):
    return BANDS[min(prio, 5)]


def in_band(pool_id, prio):
    lo, hi = band_of(prio)
    return lo <= pool_id <= hi


def upsert_pools(cur, merged):
    """Bring the pools table in line with the merged list. Returns slug -> (db id, unique_id)."""
    # slug has no unique key; where a slug has several rows the lowest id is the pool, the rest
    # are treated like dropped pools (matchers blanked, parked at the end).
    cur.execute("SELECT id, name, link, addresses, regexes, slug, unique_id FROM pools ORDER BY id DESC")
    all_rows = cur.fetchall()
    rows = {r[5]: r for r in all_rows}
    dupes = [r for r in all_rows if rows[r[5]][0] != r[0]]
    next_uid = max([r[6] for r in all_rows] + [0]) + 1
    used = {r[0] for r in all_rows}
    ids = {}
    changed = inserted = 0
    for slug, e, prio in merged:
        addrs = json.dumps(e["addresses"])
        regs = json.dumps(e["tags"])
        r = rows.get(slug)
        if r:
            if (r[1], r[2], r[3], r[4]) != (e["name"], e["link"], addrs, regs):
                if not DRY_RUN:
                    cur.execute("UPDATE pools SET name=%s, link=%s, addresses=%s, regexes=%s WHERE id=%s",
                                (e["name"], e["link"], addrs, regs, r[0]))
                changed += 1
            ids[slug] = (r[0], r[6])
        else:
            lo, hi = band_of(prio)
            new_id = max([i for i in used if lo <= i <= hi] + [lo - 1]) + 1
            if new_id > hi:
                log(f"band {lo}-{hi} full; {slug} gets an auto id")
                new_id = None
            if DRY_RUN:
                log(f"would insert {slug} at id {new_id}")
                ids[slug] = (new_id or 0, next_uid)
            elif new_id is None:
                cur.execute("INSERT INTO pools (name, link, addresses, regexes, slug, unique_id) VALUES (%s,%s,%s,%s,%s,%s)",
                            (e["name"], e["link"], addrs, regs, slug, next_uid))
                ids[slug] = (cur.lastrowid, next_uid)
            else:
                cur.execute("INSERT INTO pools (id, name, link, addresses, regexes, slug, unique_id) VALUES (%s,%s,%s,%s,%s,%s,%s)",
                            (new_id, e["name"], e["link"], addrs, regs, slug, next_uid))
                ids[slug] = (new_id, next_uid)
            used.add(ids[slug][0])
            next_uid += 1
            inserted += 1
    if "unknown" not in ids:
        r = rows.get("unknown")
        ids["unknown"] = (r[0], r[6]) if r else None

    # Pools still in the DB but gone from the merged list (and duplicate rows) must not claim blocks.
    listed = set(ids)
    for r in [r for s, r in rows.items() if s not in listed] + dupes:
        if r[3] not in ("[]", None) or r[4] not in ("[]", None):
            log(f"blanking matchers of dropped pool {r[5]} (id {r[0]})")
            if not DRY_RUN:
                cur.execute("UPDATE pools SET addresses='[]', regexes='[]' WHERE id=%s", (r[0],))
            changed += 1
    return ids, changed, inserted


def renumber_pools(cur, merged, ids):
    """Move every pool into its priority band (see module docstring). Returns the number of
    pools whose id changed, or 0 when the table already follows the bands."""
    # canonical row per slug is the one upsert_pools chose; other rows with the same slug are
    # duplicates and are parked at the end like dropped pools
    canonical = {dbid: slug for slug, (dbid, _) in ids.items() if dbid}
    prio = {slug: p for slug, _, p in merged}
    cur.execute("SELECT id, slug FROM pools ORDER BY id")
    rows = cur.fetchall()
    stray = [(i, s) for i, s in rows if i in canonical and s != "unknown" and s in prio and not in_band(i, prio[s])]
    stray += [(i, s) for i, s in rows if s == "unknown" and i in canonical and i != UNKNOWN_ID]
    if not stray:
        return 0
    log(f"{len(stray)} pools outside their id band (e.g. {stray[0][1]}={stray[0][0]}); renumbering")

    # desired ids: merged order within each band; dropped pools go to the end of the generic band
    mapping = {}
    counters = {p: lo for p, (lo, hi) in BANDS.items()}
    for slug, _, p in merged:
        old = ids.get(slug, (0, 0))[0]
        if not old:
            continue
        p = min(p, 5)
        lo, hi = BANDS[p]
        if counters[p] > hi:
            raise RuntimeError(f"id band {lo}-{hi} overflow")
        mapping[old] = counters[p]
        counters[p] += 1
    for i, s in rows:
        if s == "unknown" and i in canonical:
            mapping[i] = UNKNOWN_ID
        elif i not in mapping:
            mapping[i] = counters[5]
            counters[5] += 1
    if len(mapping) > UNKNOWN_ID - SCRATCH_BASE:
        raise RuntimeError("too many pools for the scratch range")
    moves = {o: n for o, n in mapping.items() if o != n}
    if DRY_RUN:
        for o, n in sorted(moves.items(), key=lambda x: x[1])[:12]:
            log(f"  would move id {o} -> {n} ({dict(rows)[o]})")
        log(f"  ... {len(moves)} moves in total")
        return len(moves)

    # Two phases through the scratch range: unique keys are checked per row, so a direct
    # old->new CASE could collide half-way. blocks.pool_id has no unique key: one pass.
    scratch = {o: SCRATCH_BASE + k for k, o in enumerate(moves)}

    def case(col, m):
        return f"{col} = CASE {col} " + " ".join(f"WHEN {a} THEN {b}" for a, b in m.items()) + " END"

    def where(col, keys):
        return f"{col} IN ({','.join(str(k) for k in keys)})"

    cur.execute("SET FOREIGN_KEY_CHECKS=0")
    try:
        cur.execute(f"UPDATE pools SET {case('id', scratch)} WHERE {where('id', scratch)}")
        cur.execute(f"UPDATE pools SET {case('id', {scratch[o]: n for o, n in moves.items()})} "
                    f"WHERE {where('id', scratch.values())}")
        cur.execute(f"UPDATE blocks SET {case('pool_id', moves)} WHERE {where('pool_id', moves)}")
        # hashrate_timestamp is ON UPDATE current_timestamp(): pin it or every row moves to "now"
        keep = "hashrate_timestamp = hashrate_timestamp"
        cur.execute(f"UPDATE hashrates SET {keep}, {case('pool_id', scratch)} WHERE {where('pool_id', scratch)}")
        cur.execute(f"UPDATE hashrates SET {keep}, {case('pool_id', {scratch[o]: n for o, n in moves.items()})} "
                    f"WHERE {where('pool_id', scratch.values())}")
    finally:
        cur.execute("SET FOREIGN_KEY_CHECKS=1")
    for slug, (old, uid) in list(ids.items()):
        if old in moves:
            ids[slug] = (moves[old], uid)
    return len(moves)


def compile_matchers(merged, ids):
    out = []
    for slug, e, _ in merged:
        regs = []
        for t in e["tags"]:
            try:
                regs.append(re.compile(t, re.I))
            except re.error:
                regs.append(re.compile(re.escape(t), re.I))
        out.append((slug, set(e["addresses"]), regs, ids[slug]))
    return out


def retag(cur, matchers, unknown, ids_by_slug, names):
    cur.execute("SELECT height, hash, pool_id, coinbase_addresses, coinbase_raw FROM blocks WHERE height >= %s",
                (START_HEIGHT,))
    updated = 0
    by_height = {}
    for height, hsh, pool_id, addrs_json, raw in cur.fetchall():
        try:
            addrs = set(json.loads(addrs_json or "[]"))
        except Exception:
            addrs = set()
        try:
            text = bytes.fromhex(raw or "").decode("latin1")
        except Exception:
            text = ""
        hit = None
        for slug, aset, regs, dbids in matchers:
            if (addrs and aset & addrs) or any(r.search(text) for r in regs):
                hit = (slug, dbids)
                break
        new_id, uid, slug, name = (hit[1][0], hit[1][1], hit[0], names[hit[0]]) if hit else (unknown[0], unknown[1], "unknown", "Unknown")
        by_height[height] = {"id": uid, "name": name, "slug": slug}
        if new_id != pool_id:
            if DRY_RUN:
                log(f"  would retag block {height}: pool_id {pool_id} -> {new_id} ({slug})")
            else:
                cur.execute("UPDATE blocks SET pool_id=%s WHERE height=%s AND hash=%s", (new_id, height, hsh))
            updated += 1
    return updated, by_height


def patch_cache(by_height):
    if not CACHE_JSON.exists():
        return 0
    try:
        data = json.loads(CACHE_JSON.read_text())
    except Exception:
        return 0
    patched = 0
    for b in data.get("blocks") or []:
        h = b.get("height")
        want = by_height.get(h)
        have = (b.get("extras") or {}).get("pool") or {}
        if want and have.get("name") != want["name"]:
            # keep the backend's template-creator names (DATUM secondary tag); only the pool changes
            b.setdefault("extras", {})["pool"] = dict(want, minerNames=have.get("minerNames"))
            patched += 1
    if patched and not DRY_RUN:
        tmp = CACHE_JSON.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(data, separators=(",", ":")))
        tmp.replace(CACHE_JSON)
    return patched


# --- main -------------------------------------------------------------------------------------

def main():
    POOLS_DIR.mkdir(parents=True, exist_ok=True)
    st = load_state()
    ov = load_overrides()
    kil = source_kilombino(st)
    guide = source_guide(st)
    save_state(st)
    merged = merge(kil, guide, ov)
    if len(merged) < 50:
        log("merged list suspiciously small; not touching the DB")
        return 1

    conn = db_connect()
    try:
        cur = conn.cursor()
        ids, changed, inserted = upsert_pools(cur, merged)
        moved = renumber_pools(cur, merged, ids)
        if DRY_RUN:
            conn.rollback()
        else:
            conn.commit()
        names = {slug: e["name"] for slug, e, _ in merged}
        matchers = compile_matchers(merged, ids)
        updated, by_height = retag(cur, matchers, ids["unknown"], ids, names)
        if DRY_RUN:
            conn.rollback()
        else:
            conn.commit()
    finally:
        conn.close()

    out = [{"id": ids[slug][1], "name": e["name"], "addresses": e["addresses"], "tags": e["tags"], "link": e["link"]}
           for slug, e, _ in merged]
    new_text = json.dumps(out, indent=1)
    if not DRY_RUN and (not OUT_JSON.exists() or OUT_JSON.read_text() != new_text):
        tmp = OUT_JSON.with_suffix(".tmp")
        tmp.write_text(new_text)
        tmp.replace(OUT_JSON)
        log(f"pools-v2.json: {len(out)} pools written")

    patched = patch_cache(by_height) if updated else 0
    tagged = sum(1 for v in by_height.values() if v["slug"] != "unknown")
    log(f"{'DRY RUN: ' if DRY_RUN else ''}pools: {len(merged)} merged, {changed} updated, {inserted} inserted, "
        f"{moved} renumbered; blocks: {len(by_height)} scanned, {tagged} attributed, {updated} retagged, "
        f"cache {patched} patched")
    return 0


if __name__ == "__main__":
    sys.exit(main())
