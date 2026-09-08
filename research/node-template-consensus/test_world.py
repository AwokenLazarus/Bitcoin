#!/usr/bin/env python3
"""World-sim assertions: qualitative properties of the rule set under hashrate mixes."""

from __future__ import annotations

import unittest

from world import Agent as WAgent
from world import run_world


class WorldTests(unittest.TestCase):
    def test_pps_self_vetoes_t(self):
        r = run_world(
            "pps",
            [WAgent(b"foundry", 1.0, "pps", True)],
            blocks=120,
            seed=1,
            t_arrives_at=5,
        )
        self.assertGreater(r.accepted, 100)
        self.assertFalse(r.t_confirmed)

    def test_unattested_usernames_never_connect(self):
        r = run_world(
            "usernames",
            [WAgent(b"foundry", 1.0, "pps", True)],
            blocks=40,
            seed=2,
            pps_pay_unattested_customers=True,
        )
        self.assertEqual(r.accepted, 0)
        self.assertEqual(r.rejected_unattested, 40)

    def test_gifted_names_still_veto(self):
        r = run_world(
            "gift",
            [WAgent(b"foundry", 1.0, "pps", True)],
            blocks=40,
            seed=3,
            pps_gift_customers=3,
            t_arrives_at=2,
        )
        self.assertEqual(r.accepted, 40)
        self.assertFalse(r.t_confirmed)

    def test_datum_includes_t_immediately(self):
        agents = [WAgent(bytes([ord("a") + i]), 0.25, "datum", False) for i in range(4)]
        r = run_world("datum", agents, blocks=80, seed=4, t_arrives_at=3)
        self.assertTrue(r.t_confirmed)
        self.assertIsNotNone(r.t_delay)
        self.assertLessEqual(r.t_delay, 1)

    def test_paying_a_node_miner_strips_pool_veto(self):
        agents = [
            WAgent(b"foundry", 0.85, "datum", True),
            WAgent(b"node", 0.15, "datum", False),
        ]
        r = run_world("hybrid", agents, blocks=80, seed=5, t_arrives_at=4)
        self.assertTrue(r.t_confirmed)
        self.assertLessEqual(r.t_delay, 1)

    def test_unpaid_minority_node_only_lands_t_when_they_find(self):
        agents = [
            WAgent(b"foundry", 0.9, "pps", True),
            WAgent(b"node", 0.1, "datum", False),
        ]
        r = run_world("minority", agents, blocks=300, seed=6, t_arrives_at=5)
        # With 10% hashrate, P(no DATUM block in 295 pending slots) is 0.9**295 ~ 10^-14.
        self.assertTrue(r.t_confirmed)


if __name__ == "__main__":
    unittest.main(verbosity=2)
