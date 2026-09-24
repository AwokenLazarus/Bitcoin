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
# 973948 AlphaPool/Tofu Toes (an 11-byte unique id), 973661 DATUM-AP/DATUM-AP (its Prime's tag, 18-22 Sep)
CB_GATEWAY_LONG_ID = "037cdc0e14416c706861506f6f6c0f546f667520546f6573000b0e92100150de71000000000eafa3000000000000000000000000"
CB_DATUM_AP = "035ddb0e12444154554d2d41500f444154554d2d415000070e92100150de710ef694000000000000000000000000"
ALPHAPOOL_TAGS = ["AlphaPool", "DATUM-AP(?![A-Za-z0-9])"]
# 972998 Lazarus/💰Pirate Lounge 🏴‍☠️: a gateway named in UTF-8
CB_UTF8_GATEWAY = "03c6d80e284c617a617275730ff09f92b0506972617465204c6f756e676520f09f8fb4e2808de298a0efb88f000b0e921001000000000000000eb619000000000000000000000000"


class SecondaryTagTests(unittest.TestCase):
    """A band means someone else's gateway built the block on the pool's coinbase."""

    def test_a_third_party_gateway_is_a_band(self):
        self.assertEqual(PS.secondary_tag(CB_GATEWAY, "AlphaPool", ["AlphaPool"]), "The Mothership")

    def test_the_pools_own_tag_is_not_a_band(self):
        self.assertIsNone(PS.secondary_tag(CB_POOL_OWN, "AlphaPool", ["AlphaPool"]))

    def test_a_pool_naming_itself_under_an_alias_is_not_a_band(self):
        # primary DATUM-AlphaPool, secondary AlphaPool: one operator, not a gateway of its own
        self.assertIsNone(PS.secondary_tag(CB_POOL_ALIAS, "AlphaPool", ["AlphaPool", "DATUM-AP(?![A-Za-z0-9])"]))

    def test_alphapool_gateways_with_a_long_unique_id_are_bands(self):
        self.assertEqual(PS.secondary_tag(CB_GATEWAY_LONG_ID, "AlphaPool", ALPHAPOOL_TAGS), "Tofu Toes")

    def test_datum_ap_naming_itself_is_the_pools_own_block(self):
        # mempool.guide lists these as a miner "DATUMAP"; here they are built by the pool
        self.assertIsNone(PS.secondary_tag(CB_DATUM_AP, "AlphaPool", ALPHAPOOL_TAGS))

    def test_every_alphapool_tag_era_is_matched_to_alphapool(self):
        matchers = _matchers(_merged())
        for text in ("AlphaPool\x0fTofu Toes\x00", "AlphaPool\x0fAlphaPool\x00",
                     "DATUM-AP\x0fDATUM-AP\x00", "DATUM-AlphaPool\x0fAlphaPool\x00"):
            self.assertEqual(_hit(matchers, text), "alphapool", text)
        self.assertNotEqual(_hit(matchers, "DATUM-APEX\x0fx\x00"), "alphapool")

    def test_a_gateway_named_with_emoji_is_a_band(self):
        # was rejected as non-ASCII and counted as built by the pool; mempool.guide names it too
        self.assertEqual(PS.secondary_tag(CB_UTF8_GATEWAY, "Lazarus", ["Lazarus"]), "Pirate Lounge")

    def test_control_bytes_still_are_not_a_tag(self):
        self.assertFalse(PS.is_datum_coinbase("03ccc10e08ff0102030f0a0b00" + "0400000000"))

    def test_punctuation_and_case_do_not_hide_the_pools_own_name(self):
        self.assertTrue(PS._same_party("Pow.re", "buy hashrate @ pow.re"))
        self.assertTrue(PS._same_party("/mined on B2Pool.io/", "b2pool.io"))
        self.assertFalse(PS._same_party("AlphaPool", "The Mothership"))
        self.assertFalse(PS._same_party("Lazarus", ""))


if __name__ == "__main__":
    unittest.main()