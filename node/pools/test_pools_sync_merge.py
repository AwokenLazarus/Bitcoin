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


if __name__ == "__main__":
    unittest.main()
