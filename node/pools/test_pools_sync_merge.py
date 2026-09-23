#!/usr/bin/env python3
"""Match-priority checks for pools-overrides.json (no DB, no network)."""
import importlib.util
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("pools_sync", HERE / "pools-sync.py")
PS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PS)


def _merged():
    ov = json.loads((HERE / "pools-overrides.json").read_text())
    guide = [
        {"name": "DATUM miners", "link": "https://ocean.xyz/docs/datum",
         "addresses": [], "regexes": ["DATUM", "datum", "Datum"]},
        {"name": "CONVOY", "link": "", "addresses": [], "regexes": ["CONVOY"]},
        {"name": "RIPTIDE", "link": "https://tides.maveth.ca",
         "addresses": [], "regexes": ["RIPTIDE", "RIPTIDES"]},
    ]
    return PS.merge([], guide, ov)


def _matchers(merged):
    counters = {p: lo for p, (lo, hi) in PS.BANDS.items()}
    ids = {}
    for slug, _, prio in merged:
        p = min(prio, 5)
        ids[slug] = (counters[p], 0)
        counters[p] += 1
    return PS.compile_matchers(merged, ids)


def _hit(matchers, text):
    for slug, _aset, regs, _ids in matchers:
        if any(r.search(text) for r in regs):
            return slug
    return None


class DatumMinersGeneric(unittest.TestCase):
    def test_named_pools_outrank_datumminers(self):
        prio = {s: p for s, _, p in _merged()}
        self.assertEqual(prio["convoy"], 2)
        self.assertEqual(prio["riptide"], 2)
        self.assertEqual(prio["datumminers"], 5)
        self.assertEqual(prio["datum"], 5)
        self.assertEqual(prio["lazarus"], 0)

    def test_datum_user_coinbases_stay_with_named_pools(self):
        matchers = _matchers(_merged())
        self.assertEqual(_hit(matchers, "CONVOY\x0fDATUM User\x00"), "convoy")
        self.assertEqual(_hit(matchers, "RIPTIDE\x0fDATUM User\x00"), "riptide")
        self.assertEqual(_hit(matchers, "Lazarus\x0fMeteking\x00"), "lazarus")
        self.assertEqual(_hit(matchers, "DATUM"), "datumminers")
        self.assertEqual(_hit(matchers, "DATUM solo mined"), "datumminers")

# Real coinbases from this chain, so the gateway-band rule is tested against what miners send.
CB_POOL_OWN = "039adb0e14416c706861506f6f6c0f416c706861506f6f6c00030e92100e5cdb000000000000000000000000"
CB_POOL_ALIAS = "03b2d70e1a444154554d2d416c706861506f6f6c0f416c706861506f6f6c00070e92100150de710e48cc000000000000000000000000"
CB_GATEWAY = "039cdb0e19416c706861506f6f6c0f546865204d6f746865727368697000070e92100150de710eb124000000000000000000000000"


class SecondaryTagTests(unittest.TestCase):
    """A band means someone else's gateway built the block on the pool's coinbase."""

    def test_a_third_party_gateway_is_a_band(self):
        self.assertEqual(PS.secondary_tag(CB_GATEWAY, "AlphaPool", ["AlphaPool"]), "The Mothership")

    def test_the_pools_own_tag_is_not_a_band(self):
        self.assertIsNone(PS.secondary_tag(CB_POOL_OWN, "AlphaPool", ["AlphaPool"]))

    def test_a_pool_naming_itself_under_an_alias_is_not_a_band(self):
        # primary DATUM-AlphaPool, secondary AlphaPool: one operator, not a gateway of its own
        self.assertIsNone(PS.secondary_tag(CB_POOL_ALIAS, "AlphaPool", ["AlphaPool", "DATUM-AP(?![A-Za-z0-9])"]))

    def test_punctuation_and_case_do_not_hide_the_pools_own_name(self):
        self.assertTrue(PS._same_party("Pow.re", "buy hashrate @ pow.re"))
        self.assertTrue(PS._same_party("/mined on B2Pool.io/", "b2pool.io"))
        self.assertFalse(PS._same_party("AlphaPool", "The Mothership"))
        self.assertFalse(PS._same_party("Lazarus", ""))


if __name__ == "__main__":
    unittest.main()