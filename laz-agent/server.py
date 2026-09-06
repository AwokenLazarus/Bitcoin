#!/usr/bin/env python3
"""Read-only Laz Bitcoin agent. Talks to FreeToken :1919 with a small tool list."""
from __future__ import annotations

import json
import os
import re
import threading
import time
import urllib.error
import urllib.request
from collections import deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from tools import live_briefing, openai_tools, run_tool

HOST = os.environ.get("LAZ_BTC_BIND", "0.0.0.0")
PORT = int(os.environ.get("LAZ_BTC_PORT", "1921"))
LAZ_URL = os.environ.get("LAZ_URL", "http://127.0.0.1:1919/v1/chat/completions")
LAZ_KEY = os.environ.get("LAZ_API_KEY", "laz")
MAX_ROUNDS = 4
MAX_TOKENS = 192
MAX_QUESTION = 400
RATE = 20
RATE_WINDOW = 60
STATIC = Path(__file__).resolve().parent / "static"

SYSTEM = """You are Laz on the Lazarus chain only.

This is Bitcoin after a PoW change. SHA-256d ended at height 961640. From 961641 the PoW is BLAKE2b. Headers are 164 bytes. Node ports are RPC 9332 and P2P 9333 — not 8332/8333.

Never treat this as Bitcoin mainnet. Never quote mainnet hashrate (EH/s), difficulty (tens of T), price, or SHA-256d ASICs (S19, S21, Antminer). Those are the other chain. Miners here are BLAKE2b (e.g. Innosilicon S11 / header-v2).

A LIVE briefing from the Lazarus pool and Knots explorer is attached. Use those numbers. Call tools for extra detail (miners, tx, address, Electrum, a block). If a figure is not in the briefing or a tool result, say you do not have it — do not invent mainnet stats.

Wallet, miner, DATUM, Electrum, or pool-connection how-tos: do NOT give step-by-step setup, seed/key handling, or config to copy. Point them at official docs only:
- Pool Connect: https://pool.awokenlazarus.xyz/#connect
- Pool How it works: https://pool.awokenlazarus.xyz/#how
- GitHub docs: https://github.com/AwokenLazarus/Bitcoin/blob/main/README.md
- Run a node: https://github.com/AwokenLazarus/Bitcoin/tree/main/node
Say clearly that this chat is unofficial, not advice, and must not be trusted for anything that could lose coins or make anyone financially responsible. The website and GitHub are the source of truth.

Read-only: refuse send, submit, keys. Be brief. Say BLAKE2b when talking about mining or hashrate.
"""

_HOWTO = re.compile(
    r"\b("
    r"wallet|electrum|seed|mnemonic|xpub|xprv|descriptor|"
    r"stratum|datum|gateway|payout|"
    r"how (do i|to)|set ?up|setting up|connect(ing)? (my|a|the|to)|"
    r"configur|install|point (my|a) miner"
    r")\b",
    re.I,
)

DOCS_FOOTER = (
    "\n\n—\n"
    "Official setup (do not follow this chat for wallets, keys, or miner config):\n"
    "https://pool.awokenlazarus.xyz/#connect\n"
    "https://pool.awokenlazarus.xyz/#how\n"
    "https://github.com/AwokenLazarus/Bitcoin/blob/main/README.md\n"
    "https://github.com/AwokenLazarus/Bitcoin/tree/main/node\n"
    "Laz is an unofficial read-only helper. Do not trust it for anything that could "
    "lose coins or create financial liability. The pool site and GitHub are authoritative."
)

def _with_docs(question: str, text: str) -> str:
    if not _HOWTO.search(question or ""):
        return text
    low = (text or "").lower()
    if "github.com/awokenlazarus/bitcoin" in low and (
        "not be trusted" in low or "do not trust" in low or "not advice" in low
    ):
        return text
    if DOCS_FOOTER.strip() in (text or ""):
        return text
    return (text or "").rstrip() + DOCS_FOOTER


_hits: dict[str, deque] = {}
_lock = threading.Lock()


def _client_ok(ip: str) -> bool:
    now = time.time()
    with _lock:
        q = _hits.setdefault(ip, deque())
        while q and now - q[0] > RATE_WINDOW:
            q.popleft()
        if len(q) >= RATE:
            return False
        q.append(now)
        return True


def _laz(messages: list) -> dict:
    body = {
        "model": "Laz",
        "messages": messages,
        "tools": openai_tools(),
        "tool_choice": "auto",
        "max_tokens": MAX_TOKENS,
        "chat_template_kwargs": {"enable_thinking": False},
    }
    last = None
    for attempt in range(8):
        req = urllib.request.Request(
            LAZ_URL,
            data=json.dumps(body).encode(),
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {LAZ_KEY}",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=150) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.HTTPError as e:
            last = e
            if e.code in (503, 429) and attempt < 7:
                time.sleep(2 + attempt)
                continue
            raise
    raise last


def answer(question: str, source: str = "web") -> dict:
    q = (question or "").strip()[:MAX_QUESTION]
    if not q:
        return {"error": "empty question"}
    briefing = live_briefing()
    messages = [
        {"role": "system", "content": SYSTEM},
        {
            "role": "user",
            "content": (
                f"LIVE from Lazarus pool + node (this chain only):\n{briefing}\n\n"
                f"[{source}] {q}"
            ),
        },
    ]
    used = []
    for _ in range(MAX_ROUNDS):
        data = _laz(messages)
        choice = (data.get("choices") or [{}])[0]
        msg = choice.get("message") or {}
        calls = msg.get("tool_calls") or []
        if not calls:
            text = (msg.get("content") or "").strip()
            return {"answer": _with_docs(q, text or "I could not form a reply."), "tools": used, "model": "Laz"}
        messages.append(
            {
                "role": "assistant",
                "content": msg.get("content") or "",
                "tool_calls": calls,
            }
        )
        for tc in calls:
            fn = (tc.get("function") or {})
            name = fn.get("name") or ""
            args = fn.get("arguments") or "{}"
            result = run_tool(name, args)
            used.append(name)
            messages.append(
                {
                    "role": "tool",
                    "tool_call_id": tc.get("id") or name,
                    "content": result,
                }
            )
    return {"answer": _with_docs(q, "Stopped after too many tool rounds."), "tools": used, "model": "Laz"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return

    def _cors(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")

    def _send(self, code: int, body: bytes, ctype: str):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self._cors()
        self.end_headers()
        self.wfile.write(body)

    def _json(self, obj, code=200):
        self._send(code, json.dumps(obj).encode(), "application/json")

    def do_OPTIONS(self):
        self.send_response(204)
        self._cors()
        self.end_headers()

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path in ("/health", "/api/laz/health"):
            self._json({"ok": True, "model": "Laz", "readonly": True})
            return
        if path.startswith("/static/"):
            name = Path(path).name
            fp = STATIC / name
            if fp.is_file() and fp.resolve().parent == STATIC.resolve():
                ctype = "application/javascript" if name.endswith(".js") else "text/css"
                self._send(200, fp.read_bytes(), ctype)
                return
        self._json({"error": "not found"}, 404)

    def do_POST(self):
        path = self.path.split("?", 1)[0]
        if path not in ("/chat", "/api/laz/chat"):
            self._json({"error": "not found"}, 404)
            return
        ip = self.headers.get("X-Forwarded-For", self.client_address[0]).split(",")[0].strip()
        if not _client_ok(ip):
            self._json({"error": "rate limited"}, 429)
            return
        n = int(self.headers.get("Content-Length") or 0)
        if n > 8000:
            self._json({"error": "payload too large"}, 413)
            return
        try:
            payload = json.loads(self.rfile.read(n) or b"{}")
        except json.JSONDecodeError:
            self._json({"error": "bad json"}, 400)
            return
        q = payload.get("q") or payload.get("question") or payload.get("message") or ""
        source = str(payload.get("source") or "web")[:32]
        try:
            self._json(answer(str(q), source))
        except urllib.error.URLError as e:
            self._json({"error": f"Laz unreachable: {e}"}, 502)
        except Exception as e:
            self._json({"error": f"{type(e).__name__}: {e}"}, 500)


def main():
    httpd = ThreadingHTTPServer((HOST, PORT), Handler)
    print(f"laz-btc-agent readonly on {HOST}:{PORT} -> {LAZ_URL}", flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
