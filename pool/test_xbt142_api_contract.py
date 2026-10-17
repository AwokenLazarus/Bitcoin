#!/usr/bin/env python3
"""Public API contract: stable fields, OpenAPI, and the blocks_found drift check.

    python3 -m pytest pool/test_api_contract.py -q
"""

import html
import json
import logging
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import jsonschema

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists() and "server" not in sys.modules
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
if "server" not in sys.modules:
    os.environ["POOL_DB"] = str(Path(tempfile.mkdtemp(prefix="api-contract-")) / "pool.sqlite")
    os.environ.pop("POOL_UI_NO_WRITE", None)
sys.path.insert(0, str(POOL))
try:
    import server
finally:
    if _made_cfg:
        _cfg.unlink()

ADDR = "bc1qminera"
BLOCK = "ab" * 32
OTHER = "cd" * 32
LOST = "11" * 32
MAKEGOOD = "ef" * 32
OAS_URL = "https://spec.openapis.org/oas/3.1/schema/2022-10-07"
OAS_CACHE = Path(tempfile.gettempdir()) / "openapi-3.1.schema.json"


def _present(doc, parts):
    if not parts:
        return True
    head, *rest = parts
    if head.endswith("[]"):
        key = head[:-2]
        if not isinstance(doc, dict) or key not in doc or not isinstance(doc[key], list) or not doc[key]:
            return False
        return all(_present(item, rest) for item in doc[key])
    if not isinstance(doc, dict) or head not in doc:
        return False
    if not rest:
        return True
    return _present(doc[head], rest)


def field_present(doc, name):
    return _present(doc, name.split("."))


class ApiContract(unittest.TestCase):
    def setUp(self):
        self._no_write = server.NO_WRITE
        server.NO_WRITE = False
        self.addCleanup(setattr, server, "NO_WRITE", self._no_write)
        self.tmp = Path(tempfile.mkdtemp(prefix="api-contract-"))
        self.log_path = self.tmp / "blocks.jsonl"
        # One pool-only block that owes ADDR a make-good, so the make-good fields have rows.
        self.log_path.write_text(
            json.dumps(
                {
                    "ts": int(time.time()),
                    "height": 150,
                    "hash": MAKEGOOD,
                    "kind": "pool-only",
                    "owed_sats": 312_500_000,
                    "split": [[ADDR, 1_000_000], ["bc1qpool", 311_500_000]],
                    "gateway": "bb" * 8,
                }
            )
            + "\n"
        )
        self._blocks_log = server.BLOCKS_LOG
        server.BLOCKS_LOG = self.log_path
        server._block_log_latest_cache["sig"] = None
        server._block_log_cache.update(sig=None, found={}, finder={}, n=0)
        server._makegood_owed_cache.update(sig=None, rows=[])
        now = time.time()
        server._prime_doc_cache.update(
            doc={
                "window": {
                    "miners": [],
                    "shares": 0,
                    "work": 0,
                    "target_work": 1,
                    "fill_percent": 0,
                    "sample_value": 312_500_000,
                    "sample_pool_sats": 0,
                    "sample_fee_sats": 0,
                },
                "pool": {
                    "min_diff": 1024,
                    "pubkey": "ab" * 32,
                    "fee_bps": 0,
                    "stratum_fee_bps": 10000,
                    "address": "bc1qpool",
                    "min_payout": 500_000,
                },
                "hashrate": {"pool_ghs": 1, "window_s": 300},
                "totals": {},
                "clients": [
                    {
                        "id": 1,
                        "gateway": "aa" * 8,
                        "user_agent": "v0.4.1-beta/abc",
                        "generation": "",
                        "fee_path": "datum",
                        "identity": "bc1qgw",
                        "secondary_tag": "rig",
                        "connected_s": 10,
                        "accepted": 1,
                        "rejected": 0,
                        "work": 1,
                        "coinbasers": 0,
                        "block_candidates": 0,
                        "offline": False,
                    }
                ],
                "blocks": [],
                "build": {"name": "primed", "version": "0"},
                "uptime_s": 1,
                "started_ts": 1,
                "node": {},
                "owed": 0,
            },
            ts=now,
            ok_ts=now,
        )
        server._price_cache.update(
            doc={"USD": 1.25, "time": int(now), "stale": False, "pair": "BTCB2", "EUR": 1.1},
            ts=now,
            refreshing=False,
        )
        server._hardware_cache.update(
            miners=[{"name": "Box", "ths": 2.0, "price_usd": 100}],
            ts=now,
            error="",
            refreshing=False,
        )
        server.state.update(
            {
                "pool_hr_ghs": 1.0,
                "shares_acc": 0,
                "shares_rej": 0,
                "miners": [
                    {
                        "address": ADDR,
                        "worker": "w",
                        "hr_ghs": 10.0,
                        "via": "stratum",
                        "host": "1.2.3.4",
                        "diff_acc": 1,
                        "shares_acc": 1,
                        "shares_rej": 0,
                        "last_share_s": 1,
                        "vdiff": 1024,
                    }
                ],
                "ts": int(now),
                "prime": {},
                "prime_meta": {},
                "gateway_hr": {},
            }
        )
        for table in ("found_blocks", "solo_blocks", "rounds", "round_payouts", "round_work", "scan_pending"):
            server.db(f"DELETE FROM {table}", write=True)
        server.db(
            "DELETE FROM meta WHERE key LIKE 'blocks_found%'",
            write=True,
        )
        server.init_share_accounting()
        server.db(
            "INSERT OR REPLACE INTO found_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase)"
            " VALUES(?,?,?,?,?,?,?,?)",
            (100, BLOCK, int(now), 3.125, ADDR, 0.0, 3.0, "Lazarus"),
            write=True,
        )
        self.explorer_count = 1
        self.explorer_page = [{"height": 100, "id": OTHER}]
        self.curl_patch = mock.patch.object(server, "curl", self._curl)
        self.rpc_patch = mock.patch.object(server, "rpc", self._rpc)
        self.splits_patch = mock.patch.object(server, "coinbase_splits", return_value={ADDR: 3.0, "bc1qpool": 0.125})
        self.curl_patch.start()
        self.rpc_patch.start()
        self.splits_patch.start()
        self.addCleanup(self.curl_patch.stop)
        self.addCleanup(self.rpc_patch.stop)
        self.addCleanup(self.splits_patch.stop)
        self.addCleanup(self._restore_log)

    def _restore_log(self):
        server.BLOCKS_LOG = self._blocks_log
        server._block_log_latest_cache["sig"] = None

    def _rpc(self, method, params=None):
        if method in ("getblockcount", "getmininginfo", "getblockchaininfo"):
            if method == "getmininginfo":
                return {"blocks": 200, "difficulty": 2.0, "networkhashps": 1e15}
            if method == "getblockchaininfo":
                return {"blocks": 200, "chain": "main", "difficulty": 2.0}
            return 200
        return None

    def _curl(self, url, digest=False, timeout=3, headers=None):
        u = str(url)
        if u.rstrip("/").endswith("/api/v1/blocks"):
            return json.dumps(
                [
                    {
                        "height": 10,
                        "id": BLOCK,
                        "timestamp": 1_700_000_000,
                        "tx_count": 1,
                        "extras": {"pool": {"name": "Lazarus"}},
                    }
                ]
            )
        if u.rstrip("/").endswith("/api/v1/mining/pool/lazarus"):
            return json.dumps({"blockCount": {"all": self.explorer_count}})
        if "/mining/pool/lazarus/blocks" in u:
            return json.dumps(self.explorer_page)
        return ""

    def samples(self):
        return {
            "/api/pool": server.pool_payload(),
            "/api/coinbaser": server.prime_coinbaser_preview(),
            "/api/blocks": {"blocks": server.mempool_blocks()},
            "/api/found/{blockhash}": server.found_outputs_payload(BLOCK),
            "/api/miners": server.public_view(server.Handler._miners_payload()),
            "/api/miner/{address}": server.public_view(server.miner_payload(ADDR)),
            "/api/gateways": server.Handler._gateways_payload(),
            "/api/payouts": server.Handler._payouts_payload(),
            "/api/solo": server.solo_payload(),
            "/api/solo/{address}": server.solo_miner_payload(ADDR),
            "/api/price": server.price_payload(),
            "/api/v1/prices": server.mempool_prices_payload(),
            "/api/hardware": server.hardware_payload(),
            "/api/rentals": server.rentals_payload(),
            "/api/history": server.history_payload("24h"),
            "/api/makegoods": server.makegoods_payload(),
            "/api/gateway-builds": server.gateway_builds(),
        }

    def test_stable_fields_are_in_sample_responses(self):
        got = self.samples()
        for ep in server.API_CATALOG:
            if not ep.fields:
                continue
            doc = got[ep.path]
            missing = [f.name for f in ep.fields if not field_present(doc, f.name)]
            self.assertEqual(missing, [], ep.path)

    def test_page_and_openapi_list_the_same_fields(self):
        en = server.api_reference_html("en")
        zh = server.api_reference_html("zh")
        spec = server.api_openapi()
        self.assertIn(en, server._SEO_INTRO["/api"]["en"][1])
        self.assertIn(zh, server._SEO_INTRO["/api"]["zh"][1])
        self.assertEqual(spec["openapi"], "3.1.0")
        for ep in server.API_CATALOG:
            self.assertIn(ep.path, spec["paths"])
            self.assertIn(f"<code>{ep.path}</code>", en)
            self.assertIn(f"<code>{ep.path}</code>", zh)
            for field in ep.fields:
                self.assertIn(f"<code>{html.escape(field.name)}</code>", en)
                self.assertIn(f"<code>{html.escape(field.name)}</code>", zh)
                self.assertIn(html.escape(field.en), en)
                self.assertIn(html.escape(field.zh), zh)

    def test_openapi_document_validates(self):
        schema = _openapi_schema()
        validator = jsonschema.Draft202012Validator(schema, resolver=_openapi_resolver(schema))
        errors = sorted((e.json_path + " " + e.message) for e in validator.iter_errors(server.api_openapi()))
        self.assertEqual(errors, [])

    def test_min_diff_comes_from_prime_and_is_left_out_when_absent(self):
        doc = server.pool_payload()
        self.assertEqual(doc["datum"]["min_diff"], 1024)
        pool = dict(server._prime_doc_cache["doc"]["pool"])
        del pool["min_diff"]
        server._prime_doc_cache["doc"]["pool"] = pool
        server._prime_doc_cache["ts"] = time.time()
        doc = server.pool_payload()
        self.assertNotIn("min_diff", doc["datum"])

    def test_drift_warns_after_more_than_two_checks_and_does_not_correct(self):
        self.explorer_count = 1
        self.explorer_page = [{"height": 100, "id": OTHER}]
        server.db(
            "INSERT OR REPLACE INTO found_blocks(height,hash,ts,reward_btc,finder,pool_fee_btc,miner_btc,coinbase)"
            " VALUES(?,?,?,?,?,?,?,?)",
            (101, LOST, int(time.time()), 3.125, ADDR, 0.0, 3.0, "Lazarus"),
            write=True,
        )
        log = server._blocks_found_log()
        quiet = _Capture()
        log.addHandler(quiet)
        try:
            server.check_blocks_found_drift(force=True)
            server.check_blocks_found_drift(force=True)
            self.assertEqual(quiet.messages, [])
            with self.assertLogs("pool.blocks_found", level="WARNING") as caught:
                server.check_blocks_found_drift(force=True)
        finally:
            log.removeHandler(quiet)
        text = "\n".join(caught.output)
        self.assertIn("ours=2", text)
        self.assertIn("explorer=1", text)
        self.assertIn("100", text)
        self.assertIn("101", text)
        self.assertEqual(server.db("SELECT COUNT(*) AS n FROM found_blocks", one=True)["n"], 2)
        n, ts = server.blocks_found_guard_view()
        self.assertEqual(n, 1)
        self.assertIsInstance(ts, int)

    def test_a_match_clears_the_streak(self):
        self.explorer_count = 1
        self.explorer_page = [{"height": 100, "id": BLOCK}]
        server.check_blocks_found_drift(force=True)
        self.assertEqual(server._meta_int("blocks_found_mismatch_streak"), 0)
        published, _ts = server.blocks_found_guard_view()
        self.assertEqual(published, 1)


class _Capture(logging.Handler):
    def __init__(self):
        super().__init__(level=logging.WARNING)
        self.messages = []

    def emit(self, record):
        self.messages.append(record.getMessage())


def _http_json(url, path):
    if path.exists() and path.stat().st_size > 1000:
        return json.loads(path.read_text())
    raw = subprocess.run(
        ["curl", "-fsS", "-A", "lazarus-pool-test", "--max-time", "30", url],
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    path.write_bytes(raw)
    return json.loads(raw)


def _openapi_schema():
    return _http_json(OAS_URL, OAS_CACHE)


def _openapi_resolver(schema):
    """jsonschema 4.10 resolves the OAS 3.1 schema's dialect $ref from this store."""
    store = {}
    ident = schema.get("$id")
    if ident:
        store[ident] = schema
    meta_url = schema.get("$schema")
    if meta_url:
        meta_path = Path(tempfile.gettempdir()) / "jsonschema-2020-12.json"
        meta = _http_json(meta_url, meta_path)
        store[meta_url] = meta
        if meta.get("$id"):
            store[meta["$id"]] = meta
    return jsonschema.RefResolver(base_uri=ident or OAS_URL, referrer=schema, store=store)


if __name__ == "__main__":
    unittest.main()
