from pathlib import Path

p = Path("/home/mike/lazarus-pool/server.py")
t = p.read_text()

old = '''    CREATE INDEX IF NOT EXISTS idx_samples_addr_ts ON samples(address, ts);
    """
)
'''
new = '''    CREATE INDEX IF NOT EXISTS idx_samples_addr_ts ON samples(address, ts);
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
    """
)
'''
assert old in t, "schema"
t = t.replace(old, new, 1)

old = "db_conn.commit()\n\n\ndef db("
new = '''db_conn.commit()


def ensure_open_round():
    openr = db("SELECT id FROM rounds WHERE status='open' ORDER BY id DESC", one=True)
    if not openr:
        db(
            "INSERT INTO rounds(started_ts,status,total_work) VALUES(?, 'open', 0)",
            (int(time.time()),),
            write=True,
        )


def db('''
assert old in t, "commit"
t = t.replace(old, new, 1)

old = '''                (rec["address"], ts, ts, rec["hr_ghs"], rec["shares_acc"], rec["shares_rej"], rec["diff_acc"]),
                write=True,
            )
    live_hr'''
new = '''                (rec["address"], ts, ts, rec["hr_ghs"], rec["shares_acc"], rec["shares_rej"], rec["diff_acc"]),
                write=True,
            )
        credit_round_work(rec["address"], rec["diff_acc"])
    live_hr'''
assert old in t, "scrape"
t = t.replace(old, new, 1)

fn = r'''
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


'''
assert "def credit_round_work" not in t
t = t.replace("def scan_found_blocks():", fn + "def scan_found_blocks():", 1)

old = '''        db(
            "INSERT OR REPLACE INTO found_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase) VALUES(?,?,?,?,?,?,?,?)",
            (height, h, blk.get("time"), reward, finder, fee_btc, miner_btc, text[:200]),
            write=True,
        )
'''
new = '''        existed = db("SELECT height FROM found_blocks WHERE height=?", (height,), one=True)
        db(
            "INSERT OR REPLACE INTO found_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase) VALUES(?,?,?,?,?,?,?,?)",
            (height, h, blk.get("time"), reward, finder, fee_btc, miner_btc, text[:200]),
            write=True,
        )
        if not existed:
            close_round_for_block(height, h, reward, fee_btc, miner_btc)
'''
assert old in t, "insert"
t = t.replace(old, new, 1)

t = t.replace(
    "            scan_found_blocks()\n        except Exception as e:\n            print(\"scan\", e, flush=True)",
    "            scan_found_blocks()\n            mature_rounds()\n        except Exception as e:\n            print(\"scan\", e, flush=True)",
)

t = t.replace(
    "Finder-takes-block: {100-POOL_FEE:g}% to the miner who finds it, {POOL_FEE:g}% pool fee",
    "Proportional: {100-POOL_FEE:g}% of each found block is split by accepted work this round; {POOL_FEE:g}% pool fee",
)
t = t.replace(
    'f"FPPS-style estimates; actual payout is {100-POOL_FEE:g}% of the found block to that miner"',
    '"PROP"',
)
old = '''    payouts = db("SELECT * FROM found_blocks WHERE finder=? ORDER BY height DESC LIMIT 50", (address,))
    earned = db("SELECT COALESCE(SUM(miner_btc),0) AS s FROM found_blocks WHERE finder=?", (address,), one=True)
'''
new = '''    payouts = db(
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
'''
assert old in t, "miner pay"
t = t.replace(old, new, 1)

t = t.replace(
    '''        "block_payout_btc": SUBSIDY * (1 - POOL_FEE / 100.0),
        "paid_btc": float(earned["s"]) if earned else 0,
        "blocks_found": [dict(r) for r in payouts],
''',
    '''        "block_payout_btc": SUBSIDY * (1 - POOL_FEE / 100.0) * (round_share or contrib),
        "paid_btc": float(earned["s"]) if earned else 0,
        "unpaid_btc": float(unpaid["s"]) if unpaid else 0,
        "immature_btc": float(immature["s"]) if immature else 0,
        "round_work": my_work,
        "round_share": round_share,
        "blocks_found": [dict(r) for r in payouts],
''',
)

old = '''        if path == "/api/payouts":
            rows = db("SELECT * FROM found_blocks ORDER BY height DESC LIMIT 100")
            self.send_json({"payouts": [dict(r) for r in rows], "scheme": "finder-takes-block", "fee_percent": POOL_FEE})
            return
'''
new = '''        if path == "/api/payouts":
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
'''
assert old in t, "api"
t = t.replace(old, new, 1)

t = t.replace(
    "def main():\n    threading.Thread(target=loop, daemon=True).start()",
    "def main():\n    ensure_open_round()\n    threading.Thread(target=loop, daemon=True).start()",
)

p.write_text(t)
print("ok")
