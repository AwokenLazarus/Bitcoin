"""Read-only tools for the Lazarus Bitcoin stack. No wallet, send, or admin calls."""
from __future__ import annotations

import json
import socket
import ssl
import urllib.error
import urllib.request
from typing import Any

POOL = "https://pool.awokenlazarus.xyz"
MEMPOOL = "https://mempool.awokenlazarus.xyz"
ELECTRUM_HOST = "27.69.0.75"
ELECTRUM_PORT = 50002
ELECTRUM_NAME = "electrum.awokenlazarus.xyz"

# Stamped on every tool payload so the model cannot treat this as SHA-256d mainnet.
CHAIN = {
    "pow": "BLAKE2b",
    "not": "SHA-256d Bitcoin mainnet",
    "header_bytes": 164,
    "fork_height": 961640,
    "rpc": 9332,
    "p2p": 9333,
    "explorer": MEMPOOL,
    "pool": POOL,
}


def _hr(hs: float) -> str:
    if hs >= 1e18:
        return f"{hs / 1e18:.2f} EH/s BLAKE2b"
    if hs >= 1e15:
        return f"{hs / 1e15:.2f} PH/s BLAKE2b"
    if hs >= 1e12:
        return f"{hs / 1e12:.2f} TH/s BLAKE2b"
    if hs >= 1e9:
        return f"{hs / 1e9:.2f} GH/s BLAKE2b"
    return f"{hs:.0f} H/s BLAKE2b"


def _stamp(obj: dict) -> dict:
    out = dict(CHAIN)
    out.update(obj)
    return out

# bitcoin-cli / RPC method names we will never invoke, even if asked.
WRITE_RPC = frozenset(
    {
        "send",
        "sendtoaddress",
        "sendmany",
        "sendrawtransaction",
        "submitblock",
        "submitheader",
        "generatetoaddress",
        "generateblock",
        "stop",
        "settxfee",
        "setnetworkactive",
        "addnode",
        "disconnectnode",
        "importprivkey",
        "dumpprivkey",
        "dumpwallet",
        "backupwallet",
        "encryptwallet",
        "walletpassphrase",
        "signrawtransactionwithkey",
        "signmessage",
        "invalidateblock",
        "reconsiderblock",
        "preciousblock",
        "pruneblockchain",
    }
)


def _get(url: str, timeout: float = 8.0, limit: int = 400_000) -> Any:
    req = urllib.request.Request(url, headers={"User-Agent": "laz-btc-agent/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        raw = resp.read(limit)
    text = raw.decode("utf-8", "replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text.strip()[:4000]


def _trim(obj: Any, n: int = 1400) -> str:
    text = json.dumps(obj, default=str)
    if len(text) > n:
        return text[: n - 20] + '…[truncated]"'
    return text


def node_status(_args: dict) -> str:
    tip_h = _get(f"{MEMPOOL}/api/blocks/tip/height")
    tip_hash = _get(f"{MEMPOOL}/api/blocks/tip/hash")
    mem = _get(f"{MEMPOOL}/api/mempool")
    fees = _get(f"{MEMPOOL}/api/v1/fees/recommended")
    da = _get(f"{MEMPOOL}/api/v1/difficulty-adjustment")
    if isinstance(mem, dict):
        mem = {k: mem.get(k) for k in ("count", "vsize", "total_fee") if k in mem}
    if isinstance(da, dict):
        da = {k: da.get(k) for k in ("progressPercent", "difficultyChange", "remainingBlocks", "remainingTime", "nextRetargetHeight") if k in da}
    return _trim(
        _stamp(
            {
                "tip_height": tip_h,
                "tip_hash": tip_hash,
                "mempool": mem,
                "fees_sat_vb": fees,
                "difficulty": da,
            }
        )
    )


def get_block(args: dict) -> str:
    ident = str(args.get("id") or args.get("hash") or args.get("height") or "").strip()
    if not ident:
        return '{"error":"id (height or hash) required"}'
    if ident.isdigit():
        hashes = _get(f"{MEMPOOL}/api/block-height/{ident}")
        ident = hashes if isinstance(hashes, str) else (hashes[0] if hashes else ident)
    block = _get(f"{MEMPOOL}/api/block/{ident}")
    if isinstance(block, dict):
        return _trim(_stamp({"block": block}))
    return _trim(_stamp({"block": block}))


def get_tx(args: dict) -> str:
    txid = str(args.get("txid") or args.get("id") or "").strip()
    if not txid or len(txid) < 32:
        return '{"error":"txid required"}'
    return _trim(_stamp({"tx": _get(f"{MEMPOOL}/api/tx/{txid}")}))


def get_address(args: dict) -> str:
    addr = str(args.get("address") or "").strip()
    if not addr or len(addr) < 20:
        return '{"error":"address required"}'
    info = _get(f"{MEMPOOL}/api/address/{addr}")
    txs = _get(f"{MEMPOOL}/api/address/{addr}/txs")
    if isinstance(txs, list):
        info = dict(info) if isinstance(info, dict) else {"address": addr}
        info["recent_txs"] = txs[:8]
    return _trim(_stamp(info if isinstance(info, dict) else {"address": addr, "info": info}))


def mempool_overview(_args: dict) -> str:
    mem = _get(f"{MEMPOOL}/api/mempool")
    if isinstance(mem, dict):
        mem = {k: mem.get(k) for k in ("count", "vsize", "total_fee") if k in mem}
    blocks = _get(f"{MEMPOOL}/api/v1/blocks")
    if isinstance(blocks, list):
        slim = []
        for b in blocks[:4]:
            if isinstance(b, dict):
                slim.append({k: b.get(k) for k in ("height", "id", "tx_count", "size", "extras") if k in b})
                if isinstance(slim[-1].get("extras"), dict):
                    slim[-1]["extras"] = {k: slim[-1]["extras"].get(k) for k in ("pool", "avgFeeRate") if k in slim[-1]["extras"]}
        blocks = slim
    return _trim(_stamp({"mempool": mem, "fees_sat_vb": _get(f"{MEMPOOL}/api/v1/fees/recommended"), "recent_blocks": blocks}))


def mining_overview(_args: dict) -> str:
    pools = _get(f"{MEMPOOL}/api/v1/mining/pools/3d")
    if isinstance(pools, dict) and isinstance(pools.get("pools"), list):
        pools = {"pools": pools["pools"][:6], "blockCount": pools.get("blockCount")}
    hr = _get(f"{MEMPOOL}/api/v1/mining/hashrate/3d")
    if isinstance(hr, dict):
        hr = {k: hr.get(k) for k in ("currentHashrate", "currentDifficulty") if k in hr}
    laz = _get(f"{MEMPOOL}/api/v1/mining/pool/lazarus")
    if isinstance(laz, dict):
        keep = ("name", "slug", "blockCount", "estimatedHashrate")
        laz = {k: laz.get(k) for k in keep if k in laz}
    if isinstance(hr, dict) and hr.get("currentHashrate"):
        hr = dict(hr)
        hr["currentHashrate_human"] = _hr(float(hr["currentHashrate"]))
    return _trim(_stamp({"pools_3d": pools, "hashrate": hr, "lazarus_pool": laz}))


def pool_status(_args: dict) -> str:
    doc = _get(f"{POOL}/api/pool", limit=800_000)
    if isinstance(doc, str):
        try:
            doc = json.loads(doc)
        except json.JSONDecodeError:
            return _trim(_stamp({"error": "pool api failed", "raw": doc[:400]}))
    if not isinstance(doc, dict):
        return _trim(_stamp({"error": "pool api failed", "raw": doc}))
    doc.pop("history", None)
    ghs = float(doc.get("pool_hr_ghs") or 0)
    net = float(doc.get("network_hr_hs") or 0)
    return _trim(
        _stamp(
            {
                "pool": "Lazarus",
                "tip_height": doc.get("height"),
                "difficulty": doc.get("difficulty"),
                "pool_hashrate": _hr(ghs * 1e9),
                "network_hashrate": _hr(net) if net else None,
                "pool_share_percent": round(float(doc.get("pool_share") or 0) * 100, 4),
                "miners_online": doc.get("miners_online"),
                "workers_online": doc.get("workers_online"),
                "fees": doc.get("fees") or {"datum_percent": 0.5, "stratum_percent": 2.5},
                "stratum_asic": doc.get("stratum_asic") or doc.get("stratum"),
                "stratum_prime": "stratum+tcp://stratum.awokenlazarus.xyz:28915",
                "connect": "BLAKE2b miners only (Innosilicon S11 / header-v2). Not SHA-256d ASICs.",
            }
        )
    )


def pool_miners(args: dict) -> str:
    addr = str(args.get("address") or "").strip()
    if addr:
        return _trim(_stamp({"miner": _get(f"{POOL}/api/miner/{addr}")}))
    doc = _get(f"{POOL}/api/miners")
    if isinstance(doc, dict):
        online = doc.get("online") or []
        seen = doc.get("seen") or []
        return _trim(_stamp({"online_count": len(online), "online": online[:12], "seen_sample": seen[:8]}))
    return _trim(doc)


def pool_payouts(_args: dict) -> str:
    return _trim(
        _stamp(
            {
                "next_coinbase": _get(f"{POOL}/api/coinbaser"),
                "found_payouts": _get(f"{POOL}/api/payouts"),
                "gateways": _get(f"{POOL}/api/gateways"),
            }
        )
    )


def _electrum(method: str, params: list | None = None) -> Any:
    # This chain refuses protocol < 1.8 (164-byte BLAKE2b headers).
    allowed = {"server.version", "server.features", "server.banner", "server.ping"}
    if method not in allowed:
        raise ValueError("electrum method not on the read-only list")
    req = json.dumps({"id": 1, "jsonrpc": "2.0", "method": method, "params": params or []}) + "\n"
    last_err = None
    # Prefer public TLS, then LAN electrs (plaintext, protocol 1.8).
    targets = [
        ("tls", ELECTRUM_NAME, ELECTRUM_PORT, True),
        ("tcp", "27.69.0.25", 50011, False),
    ]
    for kind, host, port, use_tls in targets:
        try:
            raw = socket.create_connection((host, port), 6)
            sock = raw
            if use_tls:
                ctx = ssl.create_default_context()
                try:
                    sock = ctx.wrap_socket(raw, server_hostname=ELECTRUM_NAME)
                except ssl.SSLError:
                    raw.close()
                    raw = socket.create_connection((host, port), 6)
                    sock = ssl._create_unverified_context().wrap_socket(raw, server_hostname=ELECTRUM_NAME)
            try:
                sock.settimeout(8)
                sock.sendall(req.encode())
                buf = b""
                while b"\n" not in buf:
                    chunk = sock.recv(4096)
                    if not chunk:
                        break
                    buf += chunk
            finally:
                sock.close()
            if not buf:
                last_err = RuntimeError(f"{kind} {host}:{port} empty")
                continue
            return json.loads(buf.decode())
        except Exception as e:
            last_err = e
            continue
    raise last_err or RuntimeError("electrum unreachable")


def electrum_status(_args: dict) -> str:
    try:
        ver = _electrum("server.version", ["Laz", "1.8"])
        feat = _electrum("server.features")
        banner = _electrum("server.banner")
        feat_r = (feat or {}).get("result") if isinstance(feat, dict) else feat
        if isinstance(feat_r, dict):
            feat_r = {
                "protocol_min": feat_r.get("protocol_min"),
                "protocol_max": feat_r.get("protocol_max"),
                "server_version": feat_r.get("server_version"),
                "blake2b_fork": feat_r.get("blake2b_fork"),
            }
        return _trim(
            _stamp(
                {
                    "endpoint": f"{ELECTRUM_NAME}:{ELECTRUM_PORT}",
                    "tls": True,
                    "version": (ver or {}).get("result") if isinstance(ver, dict) else ver,
                    "features": feat_r,
                    "banner": (banner or {}).get("result") if isinstance(banner, dict) else banner,
                }
            )
        )
    except Exception as e:
        return json.dumps({"error": f"electrum read failed: {e}", "endpoint": f"{ELECTRUM_NAME}:{ELECTRUM_PORT}"})


def electrum_address(args: dict) -> str:
    addr = str(args.get("address") or "").strip()
    if not addr:
        return '{"error":"address required"}'
    # Prefer the explorer; Electrum scripthash encoding varies by script type.
    return get_address({"address": addr})


TOOLS = {
    "node_status": {
        "fn": node_status,
        "spec": {
            "name": "node_status",
            "description": "This BLAKE2b node's tip, mempool, fees. Not SHA-256d mainnet.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    "get_block": {
        "fn": get_block,
        "spec": {
            "name": "get_block",
            "description": "Block by height or hash.",
            "parameters": {
                "type": "object",
                "properties": {"id": {"type": "string", "description": "Block height or hash"}},
                "required": ["id"],
            },
        },
    },
    "get_tx": {
        "fn": get_tx,
        "spec": {
            "name": "get_tx",
            "description": "Tx by txid.",
            "parameters": {
                "type": "object",
                "properties": {"txid": {"type": "string"}},
                "required": ["txid"],
            },
        },
    },
    "get_address": {
        "fn": get_address,
        "spec": {
            "name": "get_address",
            "description": "Address balance and recent txs.",
            "parameters": {
                "type": "object",
                "properties": {"address": {"type": "string"}},
                "required": ["address"],
            },
        },
    },
    "mempool_overview": {
        "fn": mempool_overview,
        "spec": {
            "name": "mempool_overview",
            "description": "Mempool count and fees.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    "mining_overview": {
        "fn": mining_overview,
        "spec": {
            "name": "mining_overview",
            "description": "This BLAKE2b network hashrate and Lazarus share.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    "pool_status": {
        "fn": pool_status,
        "spec": {
            "name": "pool_status",
            "description": "Lazarus BLAKE2b pool hashrate, fees, tip. Not SHA-256d.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    "pool_miners": {
        "fn": pool_miners,
        "spec": {
            "name": "pool_miners",
            "description": "Online miners or one address.",
            "parameters": {
                "type": "object",
                "properties": {"address": {"type": "string", "description": "Optional payout address"}},
            },
        },
    },
    "pool_payouts": {
        "fn": pool_payouts,
        "spec": {
            "name": "pool_payouts",
            "description": "Next coinbase split and payouts.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
    "electrum_status": {
        "fn": electrum_status,
        "spec": {
            "name": "electrum_status",
            "description": "Electrum TLS :50002 version.",
            "parameters": {"type": "object", "properties": {}},
        },
    },
}


def live_briefing() -> str:
    """Compact live card from the pool + explorer. Always attached to the user turn."""
    try:
        pool = json.loads(pool_status({}))
    except Exception as e:
        pool = {"error": str(e)}
    try:
        node = json.loads(node_status({}))
    except Exception as e:
        node = {"error": str(e)}
    return _trim(
        {
            "pow": "BLAKE2b",
            "not": "SHA-256d Bitcoin mainnet — do not quote its EH/s, difficulty, or S19/S21 ASICs",
            "header_bytes": 164,
            "fork_height": 961640,
            "ports": "RPC 9332 P2P 9333 (not 8332/8333)",
            "tip": node.get("tip_height"),
            "tip_hash": node.get("tip_hash"),
            "mempool_count": (node.get("mempool") or {}).get("count") if isinstance(node.get("mempool"), dict) else None,
            "fees_sat_vb": node.get("fees_sat_vb"),
            "pool_hashrate": pool.get("pool_hashrate"),
            "network_hashrate": pool.get("network_hashrate"),
            "difficulty": pool.get("difficulty") or (node.get("difficulty") or {}).get("difficultyChange"),
            "miners_online": pool.get("miners_online"),
            "fees": pool.get("fees"),
            "stratum_asic": pool.get("stratum_asic"),
            "stratum_prime": pool.get("stratum_prime"),
        },
        n=900,
    )


def openai_tools() -> list[dict]:
    return [{"type": "function", "function": t["spec"]} for t in TOOLS.values()]


def run_tool(name: str, arguments: Any) -> str:
    if name in WRITE_RPC or name.lower().startswith(("send", "submit", "generate", "import", "dump", "sign")):
        return json.dumps({"error": "write operations are disabled"})
    spec = TOOLS.get(name)
    if not spec:
        return json.dumps({"error": f"unknown tool {name}"})
    if isinstance(arguments, str):
        try:
            arguments = json.loads(arguments or "{}")
        except json.JSONDecodeError:
            arguments = {}
    if not isinstance(arguments, dict):
        arguments = {}
    try:
        return spec["fn"](arguments)
    except urllib.error.HTTPError as e:
        return json.dumps({"error": f"http {e.code} for {name}"})
    except Exception as e:
        return json.dumps({"error": f"{type(e).__name__}: {e}"})
