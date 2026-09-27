#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""SOV-015 relay-race probe on Knots 29.4.2 regtest: how often an honest node ends up holding a
foreign canary C (relayed from G's node) instead of the twin C′ the sidecar pushed to it directly,
as a function of how long after C the twin lands.

Nodes: A (G's registered node), BO (A's outbound peer: A connects to it), BI (A's inbound peer:
it connects to A). BO and BI are not connected to each other, so C can reach them only straight
from A. Per trial: C (10 sat/vB) over one P2P connection to A, then after `lag` seconds (from C
being sent) C′ (1 sat more) to BO and BI over their own connections, the way the sidecar sends
them. 20 s after the last trial, BO's and BI's mempools say which of C/C′ each took.

usage: race_probe.py <bitcoind> <base_port> <run_dir> <sidecar_dir> [trials_per_lag] [lags,...]
"""
import json
import os
import random
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
BITCOIND, BASE, RUN, SIDECAR_DIR = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
N = int(sys.argv[5]) if len(sys.argv) > 5 else 40
LAGS = [float(x) for x in sys.argv[6].split(",")] if len(sys.argv) > 6 else [0.0, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 4.0]
sys.path.insert(0, SIDECAR_DIR)
import knots_lab as kl  # noqa: E402
import xbt_p2p  # noqa: E402

FEE = 1_100


def main():
    lab = kl.Lab(BITCOIND, BASE, RUN, ["A", "BO", "BI"])
    try:
        big = ["-maxconnections=256"]
        lab.start("BO", big)
        lab.start("A", big, connect=["BO"])
        lab.start("BI", big, connect=["A"])
        lab.wait_up()
        per = 120
        trials = [(lag, k) for lag in LAGS for k in range(N)]
        utxos = lab.fund("A", (len(trials) + per - 1) // per, per)
        tip = lab.rpc("A", "generatetodescriptor", 1, kl.DESC)[0]
        lab.wait_sync(["A", "BO", "BI"], tip)
        time.sleep(2)
        print("peers:", {n: lab.rpc(n, "getconnectioncount") for n in lab.nodes}, "trials:", len(trials), flush=True)
        rng = random.Random(15)
        rng.shuffle(trials)
        results = []
        lock = threading.Lock()
        window = max(60.0, len(trials) * 0.35)

        def trial(i, lag):
            txid, vout, v = utxos[i]
            c_raw, c = kl.op_true_tx([(txid, vout)], [(v - FEE, kl.P2WSH_TRUE)])
            t_raw, tw = kl.op_true_tx([(txid, vout)], [(v - FEE - 1, kl.P2WSH_TRUE)])
            time.sleep(rng.uniform(0, window))
            try:
                a = xbt_p2p.Peer(lab.addr("A"), network="regtest", timeout=15)
                bs = {n: xbt_p2p.Peer(lab.addr(n), network="regtest", timeout=15) for n in ("BO", "BI")}
                c_sent = time.time()
                a.wait_pong(a.send_tx(bytes.fromhex(c_raw)))
                wait = c_sent + lag - time.time()
                if wait > 0:
                    time.sleep(wait)
                nonces = {n: p.send_tx(bytes.fromhex(t_raw)) for n, p in bs.items()}
                lags = {n: round(p.wait_pong(nonces[n]) - c_sent, 4) for n, p in bs.items()}
                for p in [a, *bs.values()]:
                    p.close()
            except xbt_p2p.DeliveryError as e:
                print("trial failed:", e, file=sys.stderr)
                return
            with lock:
                results.append({"lag": lag, "c": c, "twin": tw, "acked": lags})

        ths = [threading.Thread(target=trial, args=(i, lag)) for i, (lag, _) in enumerate(trials)]
        for th in ths:
            th.start()
        for th in ths:
            th.join()
        time.sleep(20)
        mp = {n: set(lab.rpc(n, "getrawmempool")) for n in ("A", "BO", "BI")}
        table = {}
        for r in results:
            row = table.setdefault(r["lag"], {"n": 0, "a_has_c": 0, "BO": 0, "BI": 0, "neither": 0, "ack": []})
            row["n"] += 1
            row["a_has_c"] += r["c"] in mp["A"]
            for n in ("BO", "BI"):
                if r["c"] in mp[n]:
                    row[n] += 1  # race lost: the honest node holds G's canary
                elif r["twin"] not in mp[n]:
                    row["neither"] += 1
                row["ack"].append(r["acked"][n])
        print("| twin lag (target) | trials | A holds C | BO (A's outbound) holds C | BI (A's inbound) holds C | measured ack lag median / max |")
        print("|---:|---:|---:|---:|---:|---:|")
        out = []
        for lag in sorted(table):
            row = table[lag]
            ack = sorted(row["ack"])
            med = ack[len(ack) // 2] if ack else None
            print(f"| {lag:g} s | {row['n']} | {row['a_has_c']}/{row['n']} | {row['BO']}/{row['n']} | {row['BI']}/{row['n']} | "
                  f"{med} / {ack[-1] if ack else None} s |")
            out.append({"lag": lag, **{k: v for k, v in row.items() if k != "ack"}, "ack_median": med, "ack_max": ack[-1] if ack else None})
        os.makedirs(os.path.join(RUN, "results"), exist_ok=True)
        path = os.path.join(RUN, "results", f"race-{int(time.time())}.json")
        json.dump(out, open(path, "w"), indent=1)
        print("result", path)
    finally:
        lab.stop_all()


if __name__ == "__main__":
    main()
