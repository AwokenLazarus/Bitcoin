#!/usr/bin/env python3
"""The custody fee on make-goods (Mike, 2026-10-17), as the site shows it.

    python3 pool/test_custody_fee.py

The fee wallet decides who pays when it signs a make-good and writes that into the queue job
(`custody_charged`, `custody_bps`); the site shows the job's numbers with the wallet's arithmetic
(`custody_cut`: s * bps // 10000, nothing when the rest would be under the address's dust floor)
and never recomputes who pays. The miner page's warning uses the wallet's offender rule
(`offenders_from`). With custody_fee_bps 0 nothing visible changes.

Imports server.py the way test_earned_24h.py does (a throwaway DB, and a config.json only if
none exists). Make-goods run the real code over a blocks.jsonl and a makegood-queue in a temp
directory; only the node is stubbed.
"""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

POOL = Path(__file__).resolve().parent
_cfg = POOL / "config.json"
_made_cfg = not _cfg.exists()
if _made_cfg:
    _cfg.write_text(json.dumps({"public_url": "https://pool.lazarus-xbt.xyz"}))
TMP = Path(tempfile.mkdtemp(prefix="custody-"))
if "server" not in sys.modules:
    os.environ["POOL_DB"] = str(TMP / "pool.sqlite")
    os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server
finally:
    if _made_cfg:
        _cfg.unlink()

A = "bc1q" + "a" * 38  # 22-byte witness v0 keyhash: dust floor 294
B = "bc1p" + "b" * 58  # taproot: 330
L = "1" + "L" * 33  # legacy P2PKH: 546
S = "3" + "S" * 33  # P2SH: 540
P = "bc1qpool"
H = 990_000
NOW = 1_800_000_000
REWARD = 312_500_000
DAY = 86400


def h(n):
    return f"{n:064x}"


def custody(bps=1000, height=H):
    """Turn the fee on (or off) for one test."""
    return mock.patch.multiple(server, CUSTODY_FEE_BPS=bps, CUSTODY_FROM_HEIGHT=height)


class Book:
    """primed's log, the coinbases and the fee wallet's queue that one test sees."""

    def __init__(self, case):
        self.dir = Path(tempfile.mkdtemp(prefix="book-", dir=TMP))
        self.log = []
        self.fb_rows = []
        for folder in ("pending", "sent", "failed"):
            (self.dir / "queue" / folder).mkdir(parents=True)
        patches = {
            "BLOCKS_LOG": self.dir / "blocks.jsonl",
            "MAKEGOOD_QUEUE": self.dir / "queue",
            "coinbase_splits": lambda blockhash: {P: REWARD / 1e8},
            "prime_doc": lambda max_age=4.0: {"pool": {"address": P}, "blocks": list(self.log)},
            "owed_settlements": dict,
            "tx_confirmations": lambda txid: None,
            "rpc": lambda method, params=None: 1000 if method == "getblockcount" else None,
            "_block_log_latest_cache": {"sig": None, "latest": {}},
            "_makegood_owed_cache": {"sig": None, "rows": []},
            "_makegood_jobs_cache": {"sig": None, "jobs": {}},
            "_offender_blocks_cache": {"key": None, "latest": {}},
        }
        for name, value in patches.items():
            p = mock.patch.object(server, name, value)
            p.start()
            case.addCleanup(p.stop)

    def pool_only(self, height, split, ts=NOW - 600, gateway="", charged=None, bps=1000, kind="pool-only"):
        """A pool-only block paying `split` [(addr, sats)] by make-good. `charged` set: the fee
        wallet has signed it (queue/pending) and named those addresses as paying the fee."""
        items = list(split) + [(P, REWARD - sum(s for _, s in split))]
        self.log.append({"ts": ts, "height": height, "hash": h(height), "kind": kind, "owed_sats": REWARD,
                         "split": [list(x) for x in items], "gateway": gateway})
        self.fb_rows.append({"height": height, "hash": h(height), "ts": ts})
        if charged is not None:
            job = {"id": f"makegood-{height}", "txid": h(height)[::-1], "custody_charged": list(charged),
                   "custody_bps": bps if charged else 0, "custody_fee": 0}
            (self.dir / "queue" / "pending" / f"makegood-{height}.json").write_text(json.dumps(job))
        return self

    def write(self):
        with (self.dir / "blocks.jsonl").open("w") as f:
            f.writelines(json.dumps(rec) + "\n" for rec in self.log)
        # The jobs cache is keyed on folder mtimes, which can tie within one test.
        server._makegood_jobs_cache.update(sig=None, jobs={})
        return self


class Arithmetic(unittest.TestCase):
    def test_cut_is_floor_of_bps(self):
        self.assertEqual(server.custody_cut_sats(A, 1_000_000, 1000), 100_000)
        self.assertEqual(server.custody_cut_sats(B, 12_345, 1000), 1_234)
        self.assertEqual(server.custody_cut_sats(A, 1_000_000, 0), 0)

    def test_dust_floors_match_the_fee_wallet(self):
        self.assertEqual([server.dust_floor(x) for x in (A, B, L, S)], [294, 330, 546, 540])

    def test_rest_under_the_dust_floor_is_paid_in_full(self):
        # 326 - 32 = 294 is at the bc1q floor: cut. 325 - 32 = 293 is under it: no cut.
        self.assertEqual(server.custody_cut_sats(A, 326, 1000), 32)
        self.assertEqual(server.custody_cut_sats(A, 325, 1000), 0)
        self.assertEqual(server.custody_cut_sats(L, 607, 1000), 60)  # 607 - 60 = 547
        self.assertEqual(server.custody_cut_sats(L, 600, 1000), 0)  # 600 - 60 = 540 < 546
        self.assertEqual(server.custody_cut_sats(S, 600, 1000), 60)  # 540 is the P2SH floor
        self.assertEqual(server.custody_cut_sats(B, 366, 1000), 36)  # 366 - 36 = 330, the bc1p floor
        self.assertEqual(server.custody_cut_sats(B, 365, 1000), 0)  # 365 - 36 = 329

    def test_only_addresses_the_job_names_are_charged(self):
        with custody():
            job = {"custody_charged": [A], "custody_bps": 1000}
            a = server.stamp_custody({"height": H, "address": A, "sats": 1_000_000, "status": "queued"}, job)
            b = server.stamp_custody({"height": H, "address": B, "sats": 1_000_000, "status": "queued"}, job)
        self.assertEqual((a["custody_fee_sats"], a["net_sats"], a["custody"]), (100_000, 900_000, "charged"))
        self.assertEqual((b["custody_fee_sats"], b["net_sats"], b["custody"]), (0, 1_000_000, ""))

    def test_job_bps_is_the_one_used(self):
        # The job records the rate it signed with; a later config change does not rewrite history.
        with custody(bps=500):
            r = server.stamp_custody({"height": H, "address": A, "sats": 1_000_000, "status": "paid"},
                                     {"custody_charged": [A], "custody_bps": 1000})
        self.assertEqual(r["custody_fee_sats"], 100_000)

    def test_start_height_decides_the_caution(self):
        with custody():
            before = server.stamp_custody({"height": H - 1, "address": A, "sats": 1_000_000, "status": "owed"})
            at = server.stamp_custody({"height": H, "address": A, "sats": 1_000_000, "status": "owed"})
            signed = server.stamp_custody({"height": H, "address": A, "sats": 1_000_000, "status": "queued"}, {"custody_charged": []})
        self.assertEqual((before["custody"], before["custody_fee_sats"]), ("", 0))
        self.assertEqual((at["custody"], at["custody_fee_sats"], at["net_sats"]), ("pending", 0, 1_000_000), "unsigned: nothing shown as charged")
        self.assertEqual((signed["custody"], signed["custody_fee_sats"]), ("", 0), "signed and not named: paid in full")

    def test_a_fee_without_a_start_height_is_off(self):
        with custody(bps=1000, height=0):
            self.assertFalse(server.custody_on())
            self.assertEqual(server.custody_view(), {"bps": 0, "percent": 0.0, "from_height": 0})


class MakegoodTotals(unittest.TestCase):
    def test_rows_payload_and_earned_use_the_net(self):
        b = Book(self)
        b.pool_only(H, [(A, 1_000_000), (B, 500_000)], charged=[A])
        b.pool_only(H - 5, [(A, 2_000_000)], charged=[])  # before H: the wallet charged nobody
        b.write()
        with custody():
            rows = server.makegood_rows_for(A, tip=1000)
            self.assertEqual([(r["height"], r["sats"], r["custody_fee_sats"], r["net_sats"]) for r in rows],
                             [(H, 1_000_000, 100_000, 900_000), (H - 5, 2_000_000, 0, 2_000_000)])
            e = server.earned_window(A, b.fb_rows, rows, now=NOW)
            self.assertEqual((e["makegood_sats"], e["makegood_custody_fee_sats"], e["total_sats"]), (2_900_000, 100_000, 2_900_000))
            doc = server.makegoods_payload()
        blk = {x["height"]: x for x in doc["blocks"]}
        self.assertEqual((blk[H]["owed_sats"], blk[H]["custody_fee_sats"], blk[H]["net_sats"]), (1_500_000, 100_000, 1_400_000))
        self.assertEqual(doc["pending_sats"], 1_400_000 + 2_000_000)
        self.assertEqual(doc["custody_fee_sats"], 100_000)
        self.assertEqual(doc["custody"], {"bps": 1000, "percent": 10.0, "from_height": H})

    def test_unsigned_block_after_h_shows_nothing_charged(self):
        b = Book(self).pool_only(H + 1, [(A, 1_000_000)]).write()
        with custody():
            (r,) = server.makegood_rows_for(A, tip=1000)
        self.assertEqual((r["status"], r["custody"], r["custody_fee_sats"], r["net_sats"]), ("owed", "pending", 0, 1_000_000))

    def test_fee_off_changes_no_figure(self):
        b = Book(self)
        b.pool_only(H, [(A, 1_000_000), (B, 500_000)], charged=[])
        b.pool_only(H + 1, [(A, 300_000)])
        b.write()
        with custody(bps=0, height=0):
            rows = server.makegood_rows_for(A, tip=1000)
            e = server.earned_window(A, b.fb_rows, rows, now=NOW)
            doc = server.makegoods_payload()
        self.assertTrue(all(r["net_sats"] == r["sats"] and r["custody_fee_sats"] == 0 and r["custody"] == "" for r in rows))
        self.assertEqual((e["makegood_sats"], e["makegood_custody_fee_sats"]), (1_300_000, 0))
        self.assertEqual(doc["pending_sats"], sum(x["owed_sats"] for x in doc["blocks"]))
        self.assertEqual(doc["custody_fee_sats"], 0)


def client(ident, gw, accepted=0, pof=0, offline=False):
    return {"identity": ident, "gateway": gw, "accepted": accepted, "pool_only_full_jobs": pof,
            "offline": offline, "fee_path": "datum", "user_agent": "v0.4.1-beta/abc"}


class OutdatedGateways(unittest.TestCase):
    def test_pool_only_work_rate(self):
        Book(self).write()
        clients = [client(A, "g1", 1000, 5), client(B, "g2", 1000, 4), client(L, "g3", 49, 49), client(S, "g4", 1000, 900, offline=True)]
        bad = server.gateway_offenders(clients, now=NOW)
        self.assertEqual(set(bad), {A}, "0.5% of 50+ accepted, connected only")
        self.assertIn("pool-only work", bad[A])

    def test_latest_block_in_30_days(self):
        b = Book(self)
        b.pool_only(H, [(A, 1)], ts=NOW - 10 * DAY, gateway="g1")  # g1: latest is pool-only
        b.pool_only(H + 1, [(A, 1)], ts=NOW - 10 * DAY, gateway="g2")
        b.log.append({"ts": NOW - DAY, "height": H + 2, "hash": h(H + 2), "kind": "split", "gateway": "g2"})  # g2 fixed since
        b.pool_only(H + 3, [(A, 1)], ts=NOW - 31 * DAY, gateway="g3")  # too old
        b.pool_only(H + 4, [(A, 1)], ts=NOW - DAY, gateway="g4", kind="partial")
        b.log.append({"ts": NOW - 60, "height": H + 5, "hash": h(H + 5), "kind": "orphan-pool-only", "gateway": "g5"})
        b.write()
        clients = [client("i1", "g1", 10), client("i2", "g2", 10), client("i3", "g3", 10), client("i4", "g4", 10), client("i5", "g5", 10)]
        bad = server.gateway_offenders(clients, now=NOW)
        self.assertEqual(sorted(bad), ["i1", "i4"])
        self.assertIn("partial", bad["i4"])

    def test_miner_flags(self):
        Book(self).write()
        clients = [client(A, "g1", 1000, 50)]
        self.assertEqual(server.gateway_outdated_for(A, [], clients, now=NOW)[:2], (True, server.gateway_offenders(clients, NOW)[A]))
        self.assertEqual(server.gateway_outdated_for(B, [], clients, now=NOW), (False, "", False))
        charged = [{"custody_fee_sats": 10, "ts": NOW - 6 * DAY}]
        old = [{"custody_fee_sats": 10, "ts": NOW - 8 * DAY}]
        out, reason, recent = server.gateway_outdated_for(B, charged, clients, now=NOW)
        self.assertEqual((out, recent), (False, True))
        self.assertIn("custody fee", reason)
        self.assertEqual(server.gateway_outdated_for(B, old, clients, now=NOW), (False, "", False))


class Pages(unittest.TestCase):
    def test_fee_off_pages_and_llms_say_nothing_new(self):
        with custody(bps=0, height=0):
            llms = server.llms_txt()
            home = server.render_pool_index("/")
            zh = server.render_pool_index("/zh/")
            pool = server.pool_payload()
        self.assertNotIn("Custody fee", llms)
        self.assertNotIn("CUSTODYPLACEHOLDER", llms)
        for page in (home, zh):
            self.assertNotIn("data-custody", page)
            self.assertNotIn('id="fee-custody"', page)
        self.assertIn('data-i18n-html="gatewaynotice.text"', home)
        self.assertNotIn("Custody fee", pool["fees"]["note"])
        self.assertEqual(pool["fees"]["custody"]["percent"], 0.0)

    def test_fee_on_pages_carry_the_line_and_notice(self):
        with custody():
            llms = server.llms_txt()
            home = server.render_pool_index("/")
            zh = server.render_pool_index("/zh/")
            how = server.render_pool_index("/how")
            pool = server.pool_payload()
        self.assertIn("- Custody fee: from block 990,000", llms)
        self.assertIn("paid in full", llms)
        self.assertIn('id="gatewaynotice" data-custody', home)
        self.assertIn("From block 990,000, a miner whose gateway produces these coinbases is paid 10% less", home)
        self.assertIn('id="fee-custody"', home)
        self.assertIn("自区块 990,000 起", zh)
        self.assertIn("早已被反复提醒", zh)
        self.assertIn("<p>Custody fee: from block 990,000", how)
        self.assertEqual(pool["fees"]["custody"], {"bps": 1000, "percent": 10.0, "from_height": H})
        self.assertIn("Custody fee", pool["fees"]["note"])

    def test_i18n_keys_exist_in_both_languages(self):
        for locale in ("en", "zh-CN"):
            d = server._i18n_dict(locale)
            self.assertIn("{height}", d["gatewaynotice"]["custodyText"])
            self.assertIn("{pct}", d["hero"]["custodyFee"])
            for k in ("kicker", "title", "text", "textFee", "setup", "discord"):
                self.assertTrue(d["minerwarn"][k], (locale, k))
            for k in ("mgCustody", "mgCustodyMay", "mgCustodyTotal", "earned24Custody"):
                self.assertTrue(d["miner"][k], (locale, k))


if __name__ == "__main__":
    unittest.main()
