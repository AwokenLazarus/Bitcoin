#!/usr/bin/env python3
"""Per-address public-stratum grace: the estimate uses grace_work, and the page rate follows the clock.

Imports server.py read-only (throwaway DB, config.json only if none exists).
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
os.environ.setdefault("POOL_DB", str(Path(tempfile.mkdtemp(prefix="grace-")) / "pool.sqlite"))
os.environ["POOL_UI_NO_WRITE"] = "1"
sys.path.insert(0, str(POOL))
try:
    import server  # noqa: E402
finally:
    if _made_cfg:
        _cfg.unlink()


def _doc(miners, pool):
    return {"window": {"miners": miners}, "pool": pool}


class StratumGraceFee(unittest.TestCase):
    def test_uplift_credits_grace_work_at_the_grace_rebate(self):
        # No grace work: the old formula, rebate_bps/100 * stratum/datum.
        self.assertEqual(server._datum_uplift_percent(100, 100, 5000, 0, None), 50.0)
        # All of the stratum work is inside grace.
        self.assertEqual(server._datum_uplift_percent(100, 100, 5000, 100, 1250), 12.5)
        # Grace work with no rebate knob is left out of the full-rebate pot.
        self.assertEqual(server._datum_uplift_percent(100, 100, 5000, 100, None), 0.0)
        # 60 full-fee, 40 grace.
        self.assertEqual(server._datum_uplift_percent(100, 100, 5000, 40, 1250), 35.0)

    def test_window_blend_uses_the_grace_rate_when_it_is_known(self):
        blended = server._window_fee_percent(100, 100, 40, 0, 100, 25)
        self.assertEqual(blended, 70.0)
        # No grace rate published: do not invent 25%.
        self.assertEqual(server._window_fee_percent(100, 100, 40, 0, 100, None), 100.0)

    def test_page_rate_follows_the_clock_and_the_estimate_keeps_the_window(self):
        in_grace = server._miner_stratum_fees(
            {
                "fee_path": "stratum",
                "window_work": 100,
                "stratum_work": 100,
                "grace_work": 100,
                "stratum_grace_until": 2_000,
            },
            0,
            100,
            25,
            1_000,
        )
        self.assertTrue(in_grace["in_stratum_grace"])
        self.assertEqual(in_grace["fee_percent_path"], 25)
        self.assertEqual(in_grace["est_fee_percent"], 25)
        self.assertEqual(in_grace["stratum_grace_until"], 2_000)

        ended = server._miner_stratum_fees(
            {
                "fee_path": "stratum",
                "window_work": 100,
                "stratum_work": 100,
                "grace_work": 40,
                "stratum_grace_until": 500,
            },
            0,
            100,
            25,
            1_000,
        )
        self.assertFalse(ended["in_stratum_grace"])
        self.assertEqual(ended["fee_percent_path"], 100)
        self.assertEqual(ended["est_fee_percent"], 70.0)

    def test_missing_grace_rate_does_not_invent_25(self):
        view = server._miner_stratum_fees(
            {
                "fee_path": "stratum",
                "window_work": 100,
                "stratum_work": 100,
                "grace_work": 100,
                "stratum_grace_until": 2_000,
            },
            0,
            100,
            None,
            1_000,
        )
        self.assertFalse(view["in_stratum_grace"])
        self.assertIsNone(view["grace_fee_percent"])
        self.assertEqual(view["fee_percent_path"], 100)
        self.assertEqual(view["est_fee_percent"], 100)

    def test_fetch_prime_window_passes_knobs_and_splits_the_pot(self):
        doc = _doc(
            [
                {
                    "identity": "stratum-addr",
                    "work": 100,
                    "stratum_work": 100,
                    "grace_work": 40,
                    "fee_path": "stratum",
                    "stratum_grace_until": 2_000,
                },
                {"identity": "datum-addr", "work": 100, "stratum_work": 0, "fee_path": "datum"},
            ],
            {
                "stratum_fee_bps": 10000,
                "fee_bps": 0,
                "datum_rebate_bps": 5000,
                "stratum_grace_fee_bps": 2500,
                "stratum_grace_rebate_bps": 1250,
                "stratum_grace_hours": 24,
                "stratum_grace_datum_hours": 96,
                "stratum_grace_rearm_hours": 168,
                "stratum_grace_epoch": 1791262800,
            },
        )
        with mock.patch.object(server, "prime_doc", return_value=doc), mock.patch.object(
            server, "prime_reachable", return_value=True
        ):
            by, meta = server.fetch_prime_window()
        self.assertEqual(by["stratum-addr"]["grace_work"], 40)
        self.assertEqual(by["stratum-addr"]["stratum_grace_until"], 2_000)
        self.assertEqual(meta["datum_uplift_percent"], 35.0)
        self.assertEqual(meta["stratum_grace_fee_bps"], 2500)
        self.assertEqual(meta["stratum_grace_hours"], 24)
        self.assertEqual(meta["stratum_grace_datum_hours"], 96)
        self.assertEqual(meta["stratum_grace_rebate_bps"], 1250)
        self.assertEqual(meta["stratum_grace_rearm_hours"], 168)
        self.assertEqual(meta["stratum_grace_epoch"], 1791262800)
        clause = server._append_stratum_grace("100% on the public stratum", 100, meta)
        self.assertIn("24 h grace at 25%", clause)
        self.assertIn("96 h if the address had DATUM work", clause)
        self.assertTrue(clause.endswith(", then 100%"))

    def test_absent_knobs_stay_absent(self):
        doc = _doc(
            [
                {"identity": "a", "work": 50, "stratum_work": 50, "fee_path": "stratum"},
                {"identity": "b", "work": 50, "stratum_work": 0, "fee_path": "datum"},
            ],
            {"stratum_fee_bps": 10000, "fee_bps": 0, "datum_rebate_bps": 5000},
        )
        with mock.patch.object(server, "prime_doc", return_value=doc), mock.patch.object(
            server, "prime_reachable", return_value=True
        ):
            by, meta = server.fetch_prime_window()
        self.assertEqual(by["a"]["grace_work"], 0)
        self.assertIsNone(by["a"]["stratum_grace_until"])
        self.assertIsNone(meta["stratum_grace_fee_bps"])
        self.assertIsNone(meta["stratum_grace_hours"])
        self.assertEqual(meta["datum_uplift_percent"], 50.0)
        self.assertEqual(server._append_stratum_grace("100% on the public stratum", 100, meta), "100% on the public stratum")


if __name__ == "__main__":
    unittest.main()
