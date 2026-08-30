#!/usr/bin/env python3
import json, re, pymysql
from pathlib import Path

conn=pymysql.connect(host=__import__("os").environ.get("MEMPOOL_DB_HOST","127.0.0.1"),user=__import__("os").environ["MEMPOOL_DB_USER"],password=__import__("os").environ["MEMPOOL_DB_PASSWORD"],database="mempool")
cur=conn.cursor()
cur.execute("SELECT id, name, regexes, slug, unique_id FROM pools WHERE slug<>'unknown'")
compiled=[]
for pid, name, regexes, slug, uid in cur.fetchall():
    try:
        regs=json.loads(regexes)
    except Exception:
        continue
    for r in regs:
        try:
            compiled.append((pid, name, slug, uid, re.compile(r)))
        except re.error:
            pass
compiled.sort(key=lambda x: len(x[4].pattern), reverse=True)
cur.execute("SELECT id FROM pools WHERE slug='unknown'")
unknown_id=cur.fetchone()[0]
cur.execute("SELECT height, hash, pool_id, coinbase_signature_ascii, coinbase_raw FROM blocks WHERE height>=961640")
updated=0
by_height={}
for height, hsh, pool_id, ascii, raw in cur.fetchall():
    text = ascii or ""
    if raw:
        try:
            text += " " + bytes.fromhex(raw).decode("latin1", errors="replace")
        except Exception:
            pass
    matched=None
    for pid, name, slug, uid, cre in compiled:
        if cre.search(text):
            matched=(pid,name,slug,uid)
            break
    new_id = matched[0] if matched else unknown_id
    pool = {"id": matched[3] if matched else 0, "name": matched[1] if matched else "Unknown", "slug": matched[2] if matched else "unknown", "minerNames": None}
    by_height[height]=pool
    if new_id != pool_id:
        cur.execute("UPDATE blocks SET pool_id=%s WHERE height=%s AND hash=%s", (new_id, height, hsh))
        updated += 1
conn.commit()
conn.close()

# Patch mempool API disk cache so the UI shows names without a full reindex.
cache=Path("/home/umbrel/umbrel/app-data/mempool/data/cache.json")
patched=0
if cache.exists():
    data=json.loads(cache.read_text())
    for b in data.get("blocks") or []:
        h=b.get("height")
        if h in by_height and (b.get("extras") or {}).get("pool",{}).get("name") != by_height[h]["name"]:
            b.setdefault("extras", {})["pool"]=by_height[h]
            patched += 1
    if patched:
        cache.write_text(json.dumps(data, separators=(",", ":")))
print(f"db_updated={updated} cache_patched={patched}")
