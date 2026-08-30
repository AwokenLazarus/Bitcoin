#!/usr/bin/env python3
"""DATUM header-v2 BLAKE2b GPU miner (Sia-style 80-byte work) for the local Umbrel node."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import socket
import struct
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
KERNEL = (HERE / "blake2b.cl").read_text()

HOST_DEFAULT = "127.0.0.1"
PORT_DEFAULT = 23334
USER_DEFAULT = "bc1q...youraddress.worker"


def tagged_sha256(tag: str, data: bytes) -> bytes:
    th = hashlib.sha256(tag.encode()).digest()
    return hashlib.sha256(th + th + data).digest()


def sia_prevhash(prev_bin: bytes) -> bytes:
    ordered = prev_bin[::-1]
    out = bytearray(tagged_sha256("Bitcoin prevblock header, hashed", ordered))
    out[:6] = b"\x00" * 6
    return bytes(out)


def blake2b256(data: bytes) -> bytes:
    return hashlib.blake2b(data, digest_size=32).digest()


def work_root(coinb1_39: bytes, extranonce_12: bytes) -> bytes:
    return blake2b256(b"\x00" + coinb1_39 + extranonce_12)


def build_work(sia_prev: bytes, nonce8: bytes, ntime8: bytes, root: bytes) -> bytes:
    return sia_prev + nonce8 + ntime8 + root


def pow_hash_le(work: bytes) -> bytes:
    return blake2b256(work)[::-1]


def share_target(diff: int) -> bytes:
    bits = (diff.bit_length() - 1) if diff else 0  # floorPoT
    if bits >= 224:
        return bitcoin_target(diff)
    t = bytearray(b"\xff" * 28 + b"\x00" * 4)
    byte_shift, bit_shift = bits // 8, bits % 8
    if byte_shift:
        t = bytearray(t[byte_shift:] + b"\x00" * byte_shift)
    if bit_shift:
        for i in range(31):
            t[i] = ((t[i] >> bit_shift) | (t[i + 1] << (8 - bit_shift))) & 0xFF
        t[31] >>= bit_shift
    return bytes(t)


def bitcoin_target(diff: int) -> bytes:
    if diff < 1:
        diff = 1
    parts = [0, 0, 0, 0x00000000FFFF0000]
    rem = 0
    result = bytearray(32)
    for i in range(3, -1, -1):
        temp = (rem << 64) | parts[i]
        quot, rem = divmod(temp, diff)
        for j in range(8):
            result[(i << 3) + j] = (quot >> (j << 3)) & 0xFF
    return bytes(result)


def meets(hash_le: bytes, target: bytes) -> bool:
    for i in range(31, -1, -1):
        if hash_le[i] < target[i]:
            return True
        if hash_le[i] > target[i]:
            return False
    return True


def hexb(b: bytes) -> str:
    return b.hex()


class Stratum:
    def __init__(self, host: str, port: int, user: str, password: str):
        self.host, self.port, self.user, self.password = host, port, user, password
        self.sock = None
        self.buf = b""
        self.lock = threading.Lock()
        self.en1 = b""
        self.job = None
        self.diff = 1
        self.accepted = 0
        self.rejected = 0
        self.submitted = 0
        self._id = 10

    def connect(self):
        self.sock = socket.create_connection((self.host, self.port), 10)
        self.sock.settimeout(30)
        self.send({"id": 1, "method": "mining.subscribe", "params": ["rtx2080-opencl/0.1"]})
        self.send({"id": 2, "method": "mining.authorize", "params": [self.user, self.password]})
        self.send({"id": 3, "method": "mining.suggest_difficulty", "params": [1]})

    def send(self, obj):
        with self.lock:
            self.sock.sendall((json.dumps(obj) + "\n").encode())

    def next_id(self) -> int:
        with self.lock:
            self._id += 1
            return self._id

    def submit(self, job_id: str, en2: bytes, ntime: bytes, nonce: bytes):
        self.submitted += 1
        self.send(
            {
                "id": self.next_id(),
                "method": "mining.submit",
                "params": [self.user, job_id, hexb(en2), hexb(ntime), hexb(nonce)],
            }
        )

    def read_loop(self):
        while True:
            try:
                chunk = self.sock.recv(16384)
            except TimeoutError:
                continue
            if not chunk:
                raise SystemExit("stratum closed")
            self.buf += chunk
            while b"\n" in self.buf:
                line, self.buf = self.buf.split(b"\n", 1)
                if not line:
                    continue
                msg = json.loads(line.decode())
                self.handle(msg)

    def handle(self, msg: dict):
        method = msg.get("method")
        if method == "mining.set_difficulty":
            self.diff = int(float(msg["params"][0]))
            print(f"difficulty {self.diff}", flush=True)
        elif method == "mining.notify":
            p = msg["params"]
            self.job = {
                "id": p[0],
                "prev": bytes.fromhex(p[1]),
                "coinb1": bytes.fromhex(p[2]),
                "ntime": bytes.fromhex(p[7]) if len(p[7]) == 16 else int(p[7], 16).to_bytes(4, "big") + b"\x00" * 4,
                "clean": bool(p[8]) if len(p) > 8 else True,
            }
            print(f"job {p[0]} ntime={p[7]} coinb1={len(self.job['coinb1'])}b", flush=True)
        elif msg.get("id") == 1 and "result" in msg:
            res = msg["result"]
            # [[notifications], extranonce1, extranonce2_size]
            self.en1 = bytes.fromhex(res[1])
            print(f"subscribed en1={self.en1.hex()} en2sz={res[2]}", flush=True)
        elif msg.get("id") and msg.get("id") >= 10:
            if msg.get("error"):
                self.rejected += 1
                print(f"reject {msg['error']}", flush=True)
            elif msg.get("result") is True:
                self.accepted += 1
                print(f"ACCEPT total={self.accepted}", flush=True)
            elif msg.get("result") is False:
                self.rejected += 1
                print(f"reject result=false {msg}", flush=True)


def run_gpu(st: Stratum, intensity: int):
    import pyopencl as cl
    import numpy as np

    plat = cl.get_platforms()[0]
    dev = plat.get_devices()[0]
    ctx = cl.Context([dev])
    queue = cl.CommandQueue(ctx, properties=cl.command_queue_properties.PROFILING_ENABLE)
    prg = cl.Program(ctx, KERNEL).build(options=["-cl-fast-relaxed-math"])
    print(f"opencl {dev.name} {dev.global_mem_size/1024/1024:.0f}MB", flush=True)

    gs = 1 << intensity
    mf = cl.mem_flags
    work_buf = cl.Buffer(ctx, mf.READ_ONLY, 80)
    tgt_buf = cl.Buffer(ctx, mf.READ_ONLY, 32)
    hits = np.zeros(1 + 64, dtype=np.uint64)
    hits_buf = cl.Buffer(ctx, mf.READ_WRITE, hits.nbytes)
    done_buf = cl.Buffer(ctx, mf.WRITE_ONLY, 8)

    nonce_base = int.from_bytes(os.urandom(6), "little")
    hashes = 0
    t0 = time.time()
    last = t0
    en2 = os.urandom(8)
    last_job = None
    sia_prev = root = ntime = job_id = None
    target = share_target(1)

    while True:
        job = st.job
        if not job or not st.en1:
            time.sleep(0.05)
            continue
        if job is not last_job:
            last_job = job
            if len(job["coinb1"]) != 39:
                print(f"bad coinb1 len {len(job['coinb1'])}", flush=True)
                time.sleep(0.2)
                continue
            en2 = os.urandom(8)
            extranonce = st.en1 + en2
            if len(extranonce) != 12:
                extranonce = (st.en1 + en2)[:12].ljust(12, b"\x00")
            # DATUM already puts sia_prevhash in mining.notify prevhash (first 6 bytes are 0).
            sia_prev = job["prev"]
            if sia_prev[:6] != bytes(6):
                sia_prev = sia_prevhash(job["prev"])
            root = work_root(job["coinb1"], extranonce)
            ntime = job["ntime"] if len(job["ntime"]) == 8 else job["ntime"][:8]
            job_id = job["id"]
            target = share_target(st.diff)
            nonce_base = int.from_bytes(os.urandom(6), "little")
            print(f"work ready diff={st.diff} target_hi={target[-4:].hex()}", flush=True)

        work = build_work(sia_prev, b"\x00" * 8, ntime, root)
        cl.enqueue_copy(queue, work_buf, work)
        cl.enqueue_copy(queue, tgt_buf, target)
        hits[:] = 0
        cl.enqueue_copy(queue, hits_buf, hits)
        prg.mine(queue, (gs,), None, work_buf, tgt_buf, np.uint64(nonce_base), np.uint32(64), hits_buf, done_buf)
        cl.enqueue_copy(queue, hits, hits_buf)
        queue.finish()
        n_hits = int(hits[0])
        for i in range(min(n_hits, 64)):
            nonce = int(hits[1 + i]).to_bytes(8, "little")
            # CPU verify before submit
            w = build_work(sia_prev, nonce, ntime, root)
            if meets(pow_hash_le(w), target):
                st.submit(job_id, en2, ntime, nonce)
                print(f"submit nonce={nonce.hex()} job={job_id}", flush=True)
        nonce_base = (nonce_base + gs) & 0xFFFFFFFFFFFFFFFF
        hashes += gs
        now = time.time()
        if now - last >= 5:
            dt = now - t0
            hs = hashes / dt if dt else 0
            print(
                f"{hs/1e6:.1f} MH/s  hashes={hashes}  acc={st.accepted} rej={st.rejected} sub={st.submitted} diff={st.diff}",
                flush=True,
            )
            last = now


def selftest():
    msg = bytes(range(80))
    ref = blake2b256(msg)
    print("cpu blake2b", ref.hex())
    import pyopencl as cl
    import numpy as np

    ctx = cl.Context([cl.get_platforms()[0].get_devices()[0]])
    q = cl.CommandQueue(ctx)
    prg = cl.Program(ctx, KERNEL).build()
    easy = bytes([0xFF] * 32)  # always hit
    hits = np.zeros(1 + 64, dtype=np.uint64)
    mf = cl.mem_flags
    wb = cl.Buffer(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, hostbuf=msg)
    tb = cl.Buffer(ctx, mf.READ_ONLY | mf.COPY_HOST_PTR, hostbuf=easy)
    hb = cl.Buffer(ctx, mf.READ_WRITE | mf.COPY_HOST_PTR, hostbuf=hits)
    db = cl.Buffer(ctx, mf.WRITE_ONLY, 8)
    prg.mine(q, (1,), None, wb, tb, np.uint64(0), np.uint32(64), hb, db)
    cl.enqueue_copy(q, hits, hb)
    q.finish()
    print("gpu hits", int(hits[0]), "nonce0", int(hits[1]) if hits[0] else None)
    # compare CPU hash of same work
    w = bytearray(msg)
    print("cpu meets easy", meets(pow_hash_le(bytes(w)), easy))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default=HOST_DEFAULT)
    ap.add_argument("--port", type=int, default=PORT_DEFAULT)
    ap.add_argument("--user", default=USER_DEFAULT)
    ap.add_argument("--pass", dest="password", default="x")
    ap.add_argument("--intensity", type=int, default=22, help="global size = 2**N")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        selftest()
        return
    st = Stratum(args.host, args.port, args.user, args.password)
    st.connect()
    threading.Thread(target=st.read_loop, daemon=True).start()
    run_gpu(st, args.intensity)


if __name__ == "__main__":
    main()
