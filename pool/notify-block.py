#!/usr/bin/env python3
"""Email the pool operator when a block is found, with coinbase payouts."""
from __future__ import annotations

import json
import subprocess
import time
import urllib.request
from pathlib import Path

POOL = "http://127.0.0.1:8888"
TO = "operator@example.com"
FROM_ADDR = "pool@example.com"
STATE = Path(__file__).resolve().parent / "notify-state.json"
PROTONCTL = Path("/home/mike/proton-mail/protonctl.py")
EXPLORER = "https://mempool.example.com"
INTERVAL = 15


def get(path):
    with urllib.request.urlopen(POOL + path, timeout=12) as r:
        return json.loads(r.read().decode())


def load_state():
    if STATE.exists():
        try:
            return json.loads(STATE.read_text())
        except Exception:
            pass
    return {"notified": []}


def save_state(st):
    STATE.write_text(json.dumps(st, indent=2) + "\n")


def group_payouts(rows):
    by = {}
    for r in rows or []:
        h = r.get("height")
        if not h or not r.get("hash"):
            continue
        rec = by.setdefault(
            int(h),
            {
                "height": int(h),
                "hash": r.get("hash"),
                "ts": r.get("ts"),
                "reward_btc": r.get("reward_btc"),
                "pool_fee_btc": r.get("pool_fee_btc"),
                "status": r.get("status"),
                "miners": [],
            },
        )
        if r.get("finder") and r.get("miner_btc") is not None:
            rec["miners"].append(
                {
                    "address": r["finder"],
                    "btc": float(r["miner_btc"] or 0),
                    "share": float(r.get("share") or 0),
                }
            )
    return by


def fmt_btc(n):
    if n is None:
        return "—"
    n = float(n)
    if abs(n) >= 0.01:
        return f"{n:.8f}"
    return f"{n:.8f}"


def compose(block):
    h = block["height"]
    hx = block["hash"]
    when = ""
    if block.get("ts"):
        when = time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime(int(block["ts"])))
    miners = block.get("miners") or []
    miner_total = sum(m["btc"] for m in miners)
    lines = [
        f"Lazarus found block {h}.",
        "",
        f"Height:  {h}",
        f"Hash:    {hx}",
        f"When:    {when or '—'}",
        f"Explorer: {EXPLORER}/block/{hx}",
        f"Reward:  {fmt_btc(block.get('reward_btc'))} BTC",
        f"Fee: {fmt_btc(block.get('pool_fee_btc'))} BTC",
        f"Miners:  {fmt_btc(miner_total)} BTC (PROP by this-round accepted work)",
        f"Status:  {block.get('status') or 'immature'} (coinbase matures after 100 confirmations)",
        "",
        "Payouts in this block's coinbase:",
    ]
    if not miners:
        lines.append("  (no miner outputs recorded — check DATUM coinbaser)")
    else:
        miners = sorted(miners, key=lambda m: -m["btc"])
        for m in miners:
            pct = (m["share"] * 100.0) if m["share"] else 0.0
            lines.append(f"  {m['address']}")
            lines.append(f"    {fmt_btc(m['btc'])} BTC  ({pct:.2f}% of miner share)")
    lines += ["", "— Lazarus pool"]
    return f"Lazarus found block {h}", "\n".join(lines)


def send(subject, body):
    cmd = [
        "python3",
        str(PROTONCTL),
        "send",
        "--to",
        TO,
        "--subject",
        subject,
        "--body",
        body,
        "--from-addr",
        FROM_ADDR,
    ]
    try:
        subprocess.check_call(cmd, timeout=60)
        return
    except subprocess.CalledProcessError:
        print("pool@ from rejected; Proton must own that address", flush=True)
        raise


def tick(st):
    data = get("/api/payouts")
    by = group_payouts(data.get("payouts"))
    known = set(int(x) for x in st.get("notified") or [])
    if st.get("seeded") is not True:
        # first run: remember existing blocks so we do not replay history
        st["notified"] = sorted(by.keys())
        st["seeded"] = True
        save_state(st)
        print(f"seeded {len(st['notified'])} existing block(s)", flush=True)
        return
    new = [h for h in sorted(by) if h not in known]
    for h in new:
        subj, body = compose(by[h])
        send(subj, body)
        known.add(h)
        st["notified"] = sorted(known)
        st["last_sent"] = h
        save_state(st)
        print(f"emailed block {h}", flush=True)


def main():
    print(f"watching {POOL} → {TO}", flush=True)
    while True:
        st = load_state()
        try:
            tick(st)
        except Exception as e:
            print("tick", type(e).__name__, e, flush=True)
        time.sleep(INTERVAL)


if __name__ == "__main__":
    main()
