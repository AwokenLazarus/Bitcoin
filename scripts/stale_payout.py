#!/usr/bin/env python3
"""Pay balances of miners who have stopped mining, by hand, from the pool's fee wallet.

Prime lists *stale* balances in stats.json: no work in the window, no share for
`stale-after-days`, at least `stale-min-payout` sats. With `stale-coinbase` it also pays them
itself in the coinbases it issues, as room allows. This tool is the other way: one ordinary
transaction, now.

    stale_payout.py list                      what is stale, what is held
    stale_payout.py hold  [--id ID] [--only ADDR ...] [--max-sats N]
    stale_payout.py pay   --id ID             dry run: prints the transaction it would send
    stale_payout.py pay   --id ID --send      sign, testmempoolaccept, broadcast, tell Prime
    stale_payout.py release --id ID           give up: the balances are carry again
    stale_payout.py status [--id ID]

The order matters and the tool enforces it:

  1. `hold` asks Prime to set the balances aside. From then on no coinbaser pays them. Prime
     answers with `ready_height`: coinbasers handed out before the hold can still be mined on
     until then, and a block found on one pays the balance and takes it out of the hold.
  2. `pay` refuses before the tip reaches `ready_height`, then pays exactly what is still held.
  3. After broadcasting it leaves a `paid` request. Prime reads the transaction from its own
     node and takes the balances off the books once it has 3 confirmations and pays every held
     address in full. Until then the hold stands; `status` shows it.

Hard rules (the fee wallet's): RPC 127.0.0.1:9332 only; inputs are mature `sweepable-fee`
UTXOs only, never a reserved pool-only/partial coinbase; testmempoolaccept before send; the
pool pays the network fee; one change output back to the pool address.
"""
from __future__ import annotations

import argparse
import fcntl
import json
import os
import sys
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fee_wallet as fw  # noqa: E402  (same directory on the node: ~/blake2b/bin)

STATS_URL = os.environ.get("PRIME_STATS", "http://127.0.0.1:28916/stats.json")
PAYOUTS = Path(os.environ.get("PRIME_PAYOUTS", "/home/umbrel/blake2b/lazarus-prime/payouts"))
# a change output smaller than this is not worth a UTXO; it goes to the miners' fee instead
MIN_CHANGE = int(os.environ.get("STALE_MIN_CHANGE", "50000"))
MAX_OUTPUTS = int(os.environ.get("STALE_MAX_OUTPUTS", "400"))


def stats() -> dict:
    with urllib.request.urlopen(STATS_URL, timeout=10) as r:
        return json.loads(r.read())


def stale_doc(st: dict) -> dict:
    doc = (st.get("window") or {}).get("stale")
    if doc is None:
        raise SystemExit("this Prime does not report stale balances (older build?)")
    return doc


def request(batch: str, body: dict) -> None:
    """Hand Prime a request: written whole under another name, then renamed into place."""
    PAYOUTS.mkdir(parents=True, exist_ok=True)
    answer = PAYOUTS / f"{batch}.json"
    if answer.exists():
        answer.rename(PAYOUTS / f"{batch}.{int(time.time())}.prev.json")
    tmp = PAYOUTS / f"{batch}.tmp"
    tmp.write_text(json.dumps(body) + "\n")
    tmp.rename(PAYOUTS / f"{batch}.request.json")


def answer(batch: str, wait: float = 30.0, accept=lambda a: True) -> dict:
    path = PAYOUTS / f"{batch}.json"
    deadline = time.time() + wait
    while time.time() < deadline:
        try:
            a = json.loads(path.read_text())
            if accept(a):
                return a
        except (OSError, ValueError):
            pass
        time.sleep(0.5)
    raise SystemExit(f"Prime has not answered {batch} in {wait:.0f}s (is primed running this build?)")


def fmt(sats: int) -> str:
    return f"{sats / 1e8:.8f}"


def cmd_list(_args) -> int:
    doc = stale_doc(stats())
    print(f"stale after {doc['after_days']} days, {doc['min_payout']} sats or more; paid in coinbases too: {doc['coinbase']}")
    print(f"{doc['count']} balances, {fmt(doc['sats'])} XBT")
    for b in doc["balances"]:
        flag = "" if b["payable"] else "   NOT AN ADDRESS: cannot be paid"
        print(f"  {b['sats']:>10}  idle {b['idle_days']:>3}d  {b['identity']}{flag}")
    if doc.get("holds"):
        print(f"held for payments by hand: {fmt(doc['held_sats'])} XBT")
        for batch, h in doc["holds"].items():
            print(f"  {batch}: {len(h['entries'])} balances, {sum(e[1] for e in h['entries'])} sats, ready at height {h['ready_height']}")
    return 0


def cmd_hold(args) -> int:
    doc = stale_doc(stats())
    rows = [b for b in doc["balances"] if b["payable"]]
    if args.only:
        want = set(args.only)
        missing = want - {b["identity"] for b in rows}
        if missing:
            raise SystemExit(f"not stale (or not payable): {sorted(missing)}")
        rows = [b for b in rows if b["identity"] in want]
    rows = rows[:MAX_OUTPUTS]
    if args.max_sats:
        kept, total = [], 0
        for b in rows:  # largest first, as listed
            if total + b["sats"] <= args.max_sats:
                kept.append(b)
                total += b["sats"]
        rows = kept
    if not rows:
        print("nothing to hold")
        return 0
    batch = args.id or datetime.now(timezone.utc).strftime("stale-%Y%m%dT%H%M%SZ")
    if (PAYOUTS / f"{batch}.json").exists() or batch in (doc.get("holds") or {}):
        raise SystemExit(f"{batch} has been used; pick another --id")
    request(batch, {"action": "hold", "entries": [[b["identity"], b["sats"]] for b in rows]})
    a = answer(batch)
    if not a.get("ok"):
        raise SystemExit(f"Prime refused: {a.get('error')}")
    print(f"{batch}: held {len(a['entries'])} balances, {fmt(a['held_sats'])} XBT, at height {a['height']}")
    for s in a.get("skipped") or []:
        print(f"  skipped {s['identity']}: {s['reason']}")
    print(f"pay when the tip reaches {a['ready_height']}:  stale_payout.py pay --id {batch}")
    return 0


def mature_fee_utxos(pool_addr: str) -> list[tuple[int, dict]]:
    by_hash = fw.load_block_index()
    reserved = fw.reserved_outpoints_from_queue()
    out = []
    for u in fw.rpc("listunspent", [1, 9999999, [pool_addr], True], wallet=fw.WALLET) or []:
        if fw.classify_utxo(u, by_hash, pool_addr, reserved).startswith("reserved"):
            continue
        conf = int(u.get("confirmations") or 0)
        tx = fw.rpc("getrawtransaction", [u["txid"], True])
        if "coinbase" in (tx.get("vin") or [{}])[0] and conf < 100:
            continue
        out.append((int(round(float(u["amount"]) * 1e8)), u))
    out.sort(key=lambda x: -x[0])
    return out


def cmd_pay(args) -> int:
    st = stats()
    hold = (stale_doc(st).get("holds") or {}).get(args.id)
    if not hold:
        raise SystemExit(f"no hold {args.id} (see `list`)")
    record = PAYOUTS / f"{args.id}.tx.json"
    if record.exists():
        raise SystemExit(f"{args.id} was already sent: {json.loads(record.read_text())['txid']} (see `status`)")
    tip = int((st.get("node") or {}).get("height") or fw.rpc("getblockcount"))
    if tip < hold["ready_height"]:
        raise SystemExit(f"tip is {tip}; the held amounts are final at {hold['ready_height']} ({hold['ready_height'] - tip} blocks to go)")
    pairs = [(e[0], int(e[1])) for e in hold["entries"]]
    total = sum(s for _, s in pairs)
    fw.ensure_wallet()
    fw.check_dust(pairs)
    pool_addr = fw.pool_address()

    # largest first until the payment, the fee and a change output are covered
    picked, have = [], 0
    for sats, u in mature_fee_utxos(pool_addr):
        picked.append(u)
        have += sats
        if have >= total + fw.tx_fee_sats(len(picked), len(pairs) + 1) + MIN_CHANGE:
            break
    fee = fw.tx_fee_sats(len(picked), len(pairs) + 1)
    if have < total + fee:
        raise SystemExit(f"the fee wallet has {have} mature fee sats; this payment needs {total + fee}. Hold less (--max-sats) or wait for more fee outputs to mature.")
    change = have - total - fee
    outs = [{a: fw.btc(s)} for a, s in pairs]
    if change >= MIN_CHANGE:
        outs.append({pool_addr: fw.btc(change)})
    else:
        fee, change = fee + change, 0
    ins = [{"txid": u["txid"], "vout": int(u["vout"])} for u in picked]
    print(f"{args.id}: {len(pairs)} payments, {fmt(total)} XBT; {len(ins)} inputs {fmt(have)}; fee {fee} sats; change {fmt(change)} to {pool_addr}")
    if not args.send:
        for a, s in pairs:
            print(f"  {s:>10}  {a}")
        print("dry run. Add --send to sign and broadcast.")
        return 0

    hexs, txid = fw.sign_and_hex(ins, outs)
    dec = fw.rpc("decoderawtransaction", [hexs])
    paid = {}
    for v in dec["vout"]:
        paid[fw.addr_of(v)] = paid.get(fw.addr_of(v), 0) + int(round(float(v["value"]) * 1e8))
    wrong = [(a, s, paid.get(a, 0)) for a, s in pairs if paid.get(a, 0) != s]
    if wrong:
        raise SystemExit(f"signed transaction does not pay what is held: {wrong[:5]}")
    tma = fw.rpc("testmempoolaccept", [[hexs]])[0]
    if not tma.get("allowed"):
        raise SystemExit(f"node would not accept it: {tma.get('reject-reason')}")
    # on disk before it is on the wire: a crash after sending must not lose the txid
    record.write_text(json.dumps({"id": args.id, "txid": txid, "hex": hexs, "total": total, "fee": fee, "entries": pairs, "ts": int(time.time())}, indent=2) + "\n")
    sent = fw.rpc("sendrawtransaction", [hexs])
    fw.log(f"stale payout {args.id} sent {sent}: {len(pairs)} payments {total} sats fee {fee}")
    fw.notify("Lazarus stale payout", f"{sent[:16]}… {len(pairs)} miners, {fmt(total)} XBT")
    request(args.id, {"action": "paid", "txid": sent})
    print(f"sent {sent}")
    print("Prime takes the balances off the books at 3 confirmations:  stale_payout.py status --id " + args.id)
    return 0


def cmd_release(args) -> int:
    if (PAYOUTS / f"{args.id}.tx.json").exists():
        raise SystemExit(f"{args.id} has been sent; releasing it would pay these miners twice")
    request(args.id, {"action": "release"})
    a = answer(args.id)
    if not a.get("ok"):
        raise SystemExit(f"Prime refused: {a.get('error')}")
    print(f"{args.id}: released {len(a['entries'])} balances, {fmt(a['released_sats'])} XBT; they are carry again")
    return 0


def cmd_status(args) -> int:
    holds = stale_doc(stats()).get("holds") or {}
    ids = [args.id] if args.id else sorted({p.name.split(".")[0] for p in PAYOUTS.glob("*.json")} | set(holds))
    for batch in ids:
        line = [batch]
        if batch in holds:
            line.append(f"HELD {sum(e[1] for e in holds[batch]['entries'])} sats, ready at {holds[batch]['ready_height']}")
        tx = PAYOUTS / f"{batch}.tx.json"
        if tx.exists():
            txid = json.loads(tx.read_text())["txid"]
            try:
                conf = fw.rpc("getrawtransaction", [txid, True]).get("confirmations", 0)
            except fw.RpcError:
                conf = "unknown to the node"
            line.append(f"tx {txid} confirmations={conf}")
        try:
            a = json.loads((PAYOUTS / f"{batch}.json").read_text())
            line.append(f"Prime: {a.get('status') or a.get('error')}")
        except (OSError, ValueError):
            pass
        if (PAYOUTS / f"{batch}.request.json").exists():
            line.append("(request waiting)")
        print("  ".join(line))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")
    h = sub.add_parser("hold")
    h.add_argument("--id")
    h.add_argument("--only", nargs="+", metavar="ADDR")
    h.add_argument("--max-sats", type=int)
    p = sub.add_parser("pay")
    p.add_argument("--id", required=True)
    p.add_argument("--send", action="store_true")
    r = sub.add_parser("release")
    r.add_argument("--id", required=True)
    s = sub.add_parser("status")
    s.add_argument("--id")
    args = ap.parse_args()
    if args.cmd in ("list", "status"):
        return {"list": cmd_list, "status": cmd_status}[args.cmd](args)
    # one at a time, and never while the fee wallet's own timer is spending the same UTXOs
    fw.QUEUE.mkdir(parents=True, exist_ok=True)
    lock = open(fw.QUEUE / ".fee-wallet.lock", "w")
    try:
        fcntl.flock(lock.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        raise SystemExit("the fee wallet is running; try again in a minute")
    return {"hold": cmd_hold, "pay": cmd_pay, "release": cmd_release}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
