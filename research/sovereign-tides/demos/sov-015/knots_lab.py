# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""Knots 29.4.2 regtest helpers shared by race_probe.py and foreign_knots.py (SOV-015).

Nodes run on loopback: rpc BASE+2i, p2p BASE+2i+1 (i = the node's index in `Lab.order`), datadirs
under the lab's run dir. Funds are P2WSH(OP_TRUE) outputs, so no wallet signs anything.
"""
import base64
import hashlib
import json
import os
import shutil
import struct
import subprocess
import time
import urllib.error
import urllib.request

RDTS = "-rdtsexpiry=4102444800"
P2WSH_TRUE = b"\x00\x20" + hashlib.sha256(b"\x51").digest()
DESC = "raw(" + P2WSH_TRUE.hex() + ")"
RPC_AUTH = "Basic " + base64.b64encode(b"x:sov015").decode()


def varint(n):
    return bytes([n]) if n < 0xFD else b"\xfd" + struct.pack("<H", n)


def op_true_tx(inputs, outputs):
    """Spend P2WSH(OP_TRUE) inputs; returns (hex, txid)."""
    ins = varint(len(inputs)) + b"".join(bytes.fromhex(t)[::-1] + struct.pack("<I", n) + b"\x00" + b"\xfd\xff\xff\xff"
                                         for t, n in inputs)
    outs = varint(len(outputs)) + b"".join(struct.pack("<Q", v) + varint(len(s)) + s for v, s in outputs)
    ver, lock = struct.pack("<I", 2), b"\x00" * 4
    txid = hashlib.sha256(hashlib.sha256(ver + ins + outs + lock).digest()).digest()[::-1].hex()
    wit = b"".join(b"\x01\x01\x51" for _ in inputs)
    return (ver + b"\x00\x01" + ins + outs + wit + lock).hex(), txid


def tx_inputs(raw_hex):
    """Outpoints a raw tx spends."""
    b = bytes.fromhex(raw_hex)
    pos = 6 if b[4:6] == b"\x00\x01" else 4
    n = b[pos]
    pos += 1
    out = []
    for _ in range(n):
        out.append((b[pos:pos + 32][::-1].hex(), struct.unpack("<I", b[pos + 32:pos + 36])[0]))
        pos += 36
        sl = b[pos]
        pos += 1 + sl + 4
    return out


B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def wif_regtest(secret: bytes) -> str:
    payload = b"\xef" + secret + b"\x01"
    data = payload + hashlib.sha256(hashlib.sha256(payload).digest()).digest()[:4]
    n = int.from_bytes(data, "big")
    s = ""
    while n:
        n, r = divmod(n, 58)
        s = B58[r] + s
    return "1" * (len(data) - len(data.lstrip(b"\x00"))) + s


class Lab:
    def __init__(self, bitcoind: str, base: int, run: str, order: list[str]):
        self.bitcoind, self.base, self.run, self.order = bitcoind, base, run, order
        self.nodes = {}

    def p2p(self, name) -> int:
        return self.base + 2 * self.order.index(name) + 1

    def addr(self, name) -> str:
        return f"127.0.0.1:{self.p2p(name)}"

    def start(self, name, extra=(), connect=(), ports=()):
        """`connect`: nodes this one opens outbound connections to. `ports`: extra p2p ports it
        also listens on (one node, several addresses)."""
        i = self.order.index(name)
        d = os.path.join(self.run, f"knots-{name}")
        shutil.rmtree(d, ignore_errors=True)
        os.makedirs(d, exist_ok=True)
        rpcport, p2p = self.base + 2 * i, self.base + 2 * i + 1
        extra = list(extra)
        listen = [] if "-listen=0" in extra else [f"-port={p2p}", f"-bind=127.0.0.1:{p2p}"] + [f"-bind=127.0.0.1:{p}" for p in ports]
        args = [f"-datadir={d}", "-regtest", "-server", f"-rpcport={rpcport}", "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1", "-rpcuser=x", "-rpcpassword=sov015", "-testactivationheight=blake2b@101",
                "-blake2b_headline=Lazarus", RDTS, "-dnsseed=0", "-fixedseeds=0", "-listenonion=0", "-v2transport=0",
                "-maxconnections=64"] + listen + [f"-addnode={self.addr(j)}" for j in connect] + extra
        p = subprocess.Popen(["nice", "-n", "19", self.bitcoind] + args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.nodes[name] = {"p": p, "d": d, "port": rpcport, "p2p": p2p}

    def rpc(self, name, method, *params, wallet=None):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
        url = f"http://127.0.0.1:{self.nodes[name]['port']}/" + (f"wallet/{wallet}" if wallet else "")
        req = urllib.request.Request(url, body, {"Authorization": RPC_AUTH})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return json.load(r)["result"]
        except urllib.error.HTTPError as e:
            raise RuntimeError(json.load(e).get("error")) from None

    def wait_up(self):
        for name in self.nodes:
            for _ in range(240):
                try:
                    self.rpc(name, "getblockcount")
                    break
                except Exception:  # noqa: BLE001
                    time.sleep(0.5)
            else:
                raise SystemExit(f"{name} did not start")

    def wait_sync(self, names, tip):
        for _ in range(240):
            if all(self.rpc(n, "getbestblockhash") == tip for n in names):
                return
            time.sleep(0.5)
        raise SystemExit("nodes did not sync")

    def stop_all(self, keep=False):
        for n in self.nodes.values():
            if n["p"].poll() is None:
                n["p"].terminate()
        for n in self.nodes.values():
            try:
                n["p"].wait(timeout=60)
            except Exception:  # noqa: BLE001
                n["p"].kill()
        if not keep:
            for n in self.nodes.values():
                shutil.rmtree(n["d"], ignore_errors=True)

    def fund(self, miner, n_cb, per_cb, value_each=None):
        """Mine `n_cb` spendable coinbases (+100 to mature) to OP_TRUE and split each into
        `per_cb` OP_TRUE outputs. Returns [(txid, vout, value)] (unconfirmed until the next block)."""
        self.rpc(miner, "generatetodescriptor", 101 + n_cb, DESC)
        utxos = []
        for h in range(1, 1 + n_cb):
            cb = self.rpc(miner, "getblock", self.rpc(miner, "getblockhash", h), 2)["tx"][0]
            v = round(cb["vout"][0]["value"] * 1e8)
            each = value_each or (v - 50_000) // per_cb
            raw, txid = op_true_tx([(cb["txid"], 0)], [(each, P2WSH_TRUE)] * per_cb)
            assert self.rpc(miner, "sendrawtransaction", raw, 0) == txid
            utxos.extend((txid, i, each) for i in range(per_cb))
        return utxos
