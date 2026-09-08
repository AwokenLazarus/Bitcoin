#!/usr/bin/env python3
"""Exhaustive ConnectBlock scenarios. Run: python3 test_consensus.py"""

from __future__ import annotations

import copy
import random
import unittest

from helpers import att, block, fund, tx
from spec import ANYONE, OP_RETURN, Attestation, Chain
from validate import connect, feasible_il, validate


def fresh() -> Chain:
    return Chain(height=100, bits_target=1 << 200, k=1000, W=144)


class ConsensusTests(unittest.TestCase):
    def _miner(self, chain: Chain, name: bytes, il=()):
        coin = fund(chain, name, 50_000, b"fundx00" + name)
        extra = tx(b"e-" + name, (coin,), ((name, 49_000),))
        a = att(name, chain.height + 1, chain.bits_target, extra, il)
        return a, extra

    def test_01_solo_attested_accepts(self):
        c = fresh()
        a, extra = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertTrue(connect(c, b).ok)

    def test_02_split_all_attested_accepts(self):
        c = fresh()
        a1, e1 = self._miner(c, b"alice")
        a2, e2 = self._miner(c, b"bob")
        b = block(101, c.bits_target, [(b"alice", 30), (b"bob", 20)], [a1, a2], [])
        self.assertTrue(connect(c, b).ok)

    def test_03_unattested_payee_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 30), (b"stratum-user", 20)], [a], [])
        v = validate(c, b)
        self.assertFalse(v.ok)
        self.assertTrue(any("unattested_payee" in x for x in v.errors))

    def test_04_piggyback_tides_list_does_not_attest_others(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 30), (b"carol", 20)], [a], [])
        self.assertFalse(validate(c, b).ok)

    def test_05_empty_extra_headers_only_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        empty = tx(b"empty", tuple(), ((b"alice", 1),))
        a2 = att(b"alice", 101, c.bits_target, empty, ())
        b = block(101, c.bits_target, [(b"alice", 50)], [a2], [])
        v = validate(c, b)
        self.assertFalse(v.ok)
        self.assertTrue(any(x in v.errors for x in ("att_empty_extra", "att_extra_invalid")))

    def test_06_ancient_bits_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        a = att(b"alice", 101, 1 << 250, a.extra, ())
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertIn("att_bits", validate(c, b).errors)

    def test_07_invalid_extra_tx_rejects(self):
        c = fresh()
        bogus = tx(b"bogus", ((b"nope", 0),), ((b"alice", 1),))
        a = att(b"alice", 101, c.bits_target, bogus, ())
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertIn("att_extra_invalid", validate(c, b).errors)

    def test_08_anyone_can_spend_coinbase_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(ANYONE, 50)], [a], [])
        self.assertIn("anyone_can_spend", validate(c, b).errors)

    def test_09_opreturn_zero_allowed(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [], extra_outs=[(OP_RETURN, 0)])
        self.assertTrue(connect(c, b).ok)

    def test_10_opreturn_with_value_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [], extra_outs=[(OP_RETURN, 10)])
        self.assertFalse(validate(c, b).ok)

    def test_11_same_extra_two_scripts_rejects(self):
        """One GBT (same extra tx) rewritten to two coinbases."""
        c = fresh()
        a1, e1 = self._miner(c, b"alice")
        a2 = att(b"bob", 101, c.bits_target, e1, ())
        # bob still needs a valid extra; using alice's extra is the multiplex.
        # bob's extra must also be valid — e1 spends alice's coin, still valid on prev.
        b = block(101, c.bits_target, [(b"alice", 30), (b"bob", 20)], [a1, a2], [])
        self.assertIn("att_multiplex_tree", validate(c, b).errors)

    def test_12_distinct_extras_two_identities_accepts(self):
        c = fresh()
        a1, e1 = self._miner(c, b"alice")
        a2, e2 = self._miner(c, b"bob")
        b = block(101, c.bits_target, [(b"alice", 30), (b"bob", 20)], [a1, a2], [])
        self.assertTrue(validate(c, b).ok)

    def test_13_attestation_wrong_payee_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"bob", 50)], [a], [])
        self.assertFalse(validate(c, b).ok)

    def test_14_attestation_pow_too_high_rejects(self):
        c = fresh()
        coin = fund(c, b"alice", 50_000, b"u")
        extra = tx(b"e", (coin,), ((b"alice", 1),))
        a = att(b"alice", 101, c.bits_target, extra, (), pow_int=(1 << 200) * 1000)
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertIn("att_pow_unbind", validate(c, b).errors)

    def test_15_block_pow_too_high_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [], pow_int=1 << 200)
        self.assertIn("block_pow_unbind", validate(c, b).errors)

    def test_16_bad_height_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(107, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertFalse(validate(c, b).ok)

    def test_17_determinism_two_copies_agree(self):
        c1, c2 = fresh(), fresh()
        for ch in (c1, c2):
            fund(ch, b"alice", 50_000, b"u")
        extra = tx(b"e", ((b"u", 0),), ((b"alice", 1),))
        a = att(b"alice", 101, c1.bits_target, extra, ())
        b = block(101, c1.bits_target, [(b"alice", 50)], [a], [])
        self.assertEqual(validate(c1, b), validate(c2, b))

    def test_18_missing_sidecar_payee_fails_closed(self):
        c = fresh()
        b = block(101, c.bits_target, [(b"alice", 50)], [], [])
        self.assertTrue(any("unattested_payee" in x for x in validate(c, b).errors))

    def test_19_paying_datum_miner_forces_their_il_tx(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        v = validate(c, b)
        self.assertFalse(v.ok)
        self.assertTrue(any("il_unsatisfied" in x for x in v.errors))
        b2 = block(101, c.bits_target, [(b"alice", 50)], [a], [T])
        self.assertTrue(validate(c, b2).ok)

    def test_20_self_paying_pool_can_omit_t_without_alice(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        pool_a, pool_e = self._miner(c, b"pool", il=())
        b = block(101, c.bits_target, [(b"pool", 50)], [pool_a], [])
        self.assertTrue(validate(c, b).ok)

    def test_21_confirmed_il_tx_does_not_bind_later_blocks(self):
        c = fresh()
        u_coin = fund(c, b"user", 8_000, b"ufund")
        U = tx(b"TX-U", (u_coin,), ((b"user", 7_000),))
        a, e = self._miner(c, b"alice", il=(U,))
        b1 = block(101, c.bits_target, [(b"alice", 50)], [a], [U])
        self.assertTrue(connect(c, b1).ok)
        pool_a, pool_e = self._miner(c, b"pool")
        b2 = block(102, c.bits_target, [(b"pool", 50)], [pool_a], [])
        self.assertTrue(validate(c, b2).ok)

    def test_22_carry_forward_unconfirmed_il_binds_pool(self):
        c = fresh()
        V_body = tx(b"TX-V", ((b"v", 0),), ((b"user", 1),))
        a, e = self._miner(c, b"alice", il=(V_body,))
        b1 = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertTrue(connect(c, b1).ok)
        fund(c, b"user", 5_000, b"v")
        pool_a, pool_e = self._miner(c, b"pool")
        b2 = block(102, c.bits_target, [(b"pool", 50)], [pool_a], [])
        v = validate(c, b2)
        self.assertFalse(v.ok)
        self.assertTrue(any("il_unsatisfied" in x for x in v.errors))
        b2ok = block(102, c.bits_target, [(b"pool", 50)], [pool_a], [V_body])
        self.assertTrue(validate(c, b2ok).ok)

    def test_23_il_skipped_if_already_in_block(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [T])
        self.assertTrue(validate(c, b).ok)

    def test_24_conflict_cannot_replace_il_tx(self):
        """Stricter than FOCIL: spending the IL input with another tx does not
        excuse omitting the IL tx. That would be a veto.
        """
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        S = tx(b"TX-S", (t_coin,), ((b"other", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [S])
        self.assertFalse(validate(c, b).ok)
        b_ok = block(101, c.bits_target, [(b"alice", 50)], [a], [T])
        self.assertTrue(validate(c, b_ok).ok)

    def test_25_stuffing_cannot_evict_il(self):
        c = fresh()
        c.max_weight = 8000
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),), weight=2000)
        a, e = self._miner(c, b"alice", il=(T,))
        filler = fund(c, b"x", 1000, b"fill")
        F = tx(b"F", (filler,), ((b"x", 1),), weight=3000)
        stuffed = block(101, c.bits_target, [(b"alice", 50)], [a], [F])
        self.assertFalse(validate(c, stuffed).ok)
        honest = block(101, c.bits_target, [(b"alice", 50)], [a], [T])
        self.assertTrue(validate(c, honest).ok)

    def test_26_duplicate_tx_rejects(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [T, T])
        self.assertFalse(validate(c, b).ok)

    def test_27_overspend_tx_rejects(self):
        c = fresh()
        t_coin = fund(c, b"user", 10, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 99_000),))
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [T])
        self.assertFalse(validate(c, b).ok)

    def test_28_connect_does_not_mutate_on_reject(self):
        c = fresh()
        snap = copy.deepcopy(c)
        b = block(101, c.bits_target, [(b"alice", 50)], [], [])
        v = connect(c, b)
        self.assertFalse(v.ok)
        self.assertEqual(c.height, snap.height)
        self.assertEqual(c.utxo, snap.utxo)

    def test_29_connect_advances_and_pays(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertTrue(connect(c, b).ok)
        self.assertEqual(c.height, 101)
        self.assertTrue(any(sc == b"alice" for sc, _v in c.utxo.values()))

    def test_30_gifted_attestation_accepts_residual(self):
        c = fresh()
        a, e = self._miner(c, b"customer")
        b = block(101, c.bits_target, [(b"customer", 50)], [a], [])
        self.assertTrue(validate(c, b).ok)

    def test_31_il_size_cap_rejects(self):
        c = fresh()
        huge = tuple(tx(bytes([i % 256]) + b"-x" * 15, ((b"z", 0),), ((b"u", 1),)) for i in range(300))
        a, e = self._miner(c, b"alice", il=huge)
        self.assertIn("att_il_size", validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [])).errors)

    def test_32_duplicate_attestation_script_rejects(self):
        c = fresh()
        a1, e1 = self._miner(c, b"alice")
        a2, e2 = self._miner(c, b"alice")
        # second extra is a different utxo so tree differs; still duplicate script
        b = block(101, c.bits_target, [(b"alice", 50)], [a1, a2], [])
        self.assertFalse(validate(c, b).ok)

    def test_33_unpaid_extra_attestation_still_forces_il(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        pool_a, pool_e = self._miner(c, b"pool")
        ind_a, ind_e = self._miner(c, b"indie", il=(T,))
        b = block(101, c.bits_target, [(b"pool", 50)], [pool_a, ind_a], [])
        v = validate(c, b)
        self.assertFalse(v.ok)
        self.assertTrue(any("il_unsatisfied" in x for x in v.errors))

    def test_34_random_blocks_determinism(self):
        rng = random.Random(0)
        for i in range(50):
            c1, c2 = fresh(), fresh()
            name = bytes([rng.randint(1, 255)]) + b"m"
            for ch in (c1, c2):
                fund(ch, name, 50_000, b"u" + name)
            extra = tx(b"e" + name, ((b"u" + name, 0),), ((name, 1),))
            a = att(name, 101, c1.bits_target, extra, ())
            pay = [(name, 50)] if rng.random() > 0.2 else [(name, 50), (b"x", 1)]
            b = block(101, c1.bits_target, pay, [a], [])
            self.assertEqual(validate(c1, b), validate(c2, b))

    def test_35_invalid_il_body_is_infeasible_not_a_fork(self):
        c = fresh()
        ghost = tx(b"ghost-tx-id-32-bytes-long....", ((b"missing", 0),), ((b"u", 1),))
        a, e = self._miner(c, b"alice", il=(ghost,))
        self.assertTrue(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [])).ok)

    def test_36_window_il_expires(self):
        c = fresh()
        c.W = 2
        V_body = tx(b"TX-V", ((b"v", 0),), ((b"user", 1),))
        a, e = self._miner(c, b"alice", il=(V_body,))
        self.assertTrue(connect(c, block(101, c.bits_target, [(b"alice", 50)], [a], [])).ok)
        for h, name in ((102, b"b"), (103, b"c")):
            ax, ex = self._miner(c, name)
            self.assertTrue(connect(c, block(h, c.bits_target, [(name, 50)], [ax], [])).ok)
        fund(c, b"user", 5_000, b"v")
        d, de = self._miner(c, b"d")
        self.assertTrue(validate(c, block(104, c.bits_target, [(b"d", 50)], [d], [])).ok)

    def test_37_rewind_undoes_window(self):
        c = fresh()
        V_body = tx(b"TX-V", ((b"v", 0),), ((b"user", 1),))
        a, e = self._miner(c, b"alice", il=(V_body,))
        snap = copy.deepcopy(c)
        self.assertTrue(connect(c, block(101, c.bits_target, [(b"alice", 50)], [a], [])).ok)
        fund(c, b"user", 5_000, b"v")
        pool_a, pool_e = self._miner(c, b"pool")
        self.assertFalse(validate(c, block(102, c.bits_target, [(b"pool", 50)], [pool_a], [])).ok)
        c.height, c.utxo, c.window_ils, c.confirmed = snap.height, snap.utxo, snap.window_ils, snap.confirmed
        pool_a, pool_e = self._miner(c, b"pool")
        self.assertTrue(validate(c, block(101, c.bits_target, [(b"pool", 50)], [pool_a], [])).ok)

    def test_38_datum_split_cannot_veto_one_member(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        alice, ae = self._miner(c, b"alice", il=(T,))
        bob, be = self._miner(c, b"bob", il=())
        b = block(101, c.bits_target, [(b"alice", 25), (b"bob", 25)], [alice, bob], [])
        self.assertFalse(validate(c, b).ok)
        b_ok = block(101, c.bits_target, [(b"alice", 25), (b"bob", 25)], [alice, bob], [T])
        self.assertTrue(validate(c, b_ok).ok)

    def test_39_legacy_stratum_payee_never_valid(self):
        c = fresh()
        a, e = self._miner(c, b"pool")
        for user in (b"worker1", b"1pool.com", b"bc1qstratum"):
            b = block(101, c.bits_target, [(b"pool", 40), (user, 10)], [a], [])
            self.assertFalse(validate(c, b).ok)

    def test_40_il_bodies_are_capped_not_full_templates(self):
        c = fresh()
        ids = tuple(
            tx(bytes([i]) * 32, ((b"no", 0),), ((b"u", 1),), weight=200) for i in range(20)
        )
        a, e = self._miner(c, b"alice", il=ids)
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertTrue(validate(c, b).ok)
        self.assertEqual(sum(t.weight for t in a.il), 4000)
        self.assertLess(sum(t.weight for t in a.il), 32_000)

    def test_41_without_il_rule_pool_can_pay_and_veto(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertTrue(validate(c, b, enforce_il=False).ok)
        self.assertFalse(validate(c, b, enforce_il=True).ok)

    def test_42_two_nodes_different_local_knowledge_same_verdict(self):
        """THE fork check. Node A 'knows' T, node B does not. Same block."""
        c_a, c_b = fresh(), fresh()
        for ch in (c_a, c_b):
            fund(ch, b"alice", 50_000, b"u-alice")
            fund(ch, b"user", 10_000, b"tfund")
        extra = tx(b"e-alice", ((b"u-alice", 0),), ((b"alice", 1),))
        T = tx(b"TX-T", ((b"tfund", 0),), ((b"user", 9_000),))
        a = att(b"alice", 101, c_a.bits_target, extra, (T,))
        omitted = block(101, c_a.bits_target, [(b"alice", 50)], [a], [])
        included = block(101, c_a.bits_target, [(b"alice", 50)], [a], [T])
        self.assertEqual(validate(c_a, omitted), validate(c_b, omitted))
        self.assertFalse(validate(c_a, omitted).ok)
        self.assertEqual(validate(c_a, included), validate(c_b, included))
        self.assertTrue(validate(c_a, included).ok)

    def test_43_tree_mismatch_rejects(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        bad = Attestation(
            script=b"alice",
            height=101,
            bits_target=c.bits_target,
            pow_int=0,
            extra=e,
            wtxid_tree=b"not-the-extra",
            il=(),
        )
        b = block(101, c.bits_target, [(b"alice", 50)], [bad], [])
        self.assertIn("att_tree_mismatch", validate(c, b).errors)

    def test_44_feasible_il_is_pure(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertEqual([t.txid for t in feasible_il(c, b)], [T.txid])
        stuffed = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        self.assertEqual(feasible_il(c, b), feasible_il(c, stuffed))

    def test_45_claimed_txid_must_hash_the_body(self):
        """A copied txid on a different body is invalid (txid is a hash now)."""
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        other = fund(c, b"user", 10_000, b"other")
        from spec import Tx

        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        fake = Tx(txid=T.txid, spends=(other,), pays=((b"censor", 9_000),), weight=400)
        a, e = self._miner(c, b"alice", il=(T,))
        self.assertFalse(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [fake])).ok)
        self.assertTrue(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [T])).ok)

    def test_46_claimed_block_weight_cannot_evict_il(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, e = self._miner(c, b"alice", il=(T,))
        b = block(101, c.bits_target, [(b"alice", 50)], [a], [])
        b.weight = c.max_weight
        self.assertFalse(validate(c, b).ok)
        must = feasible_il(c, b)
        self.assertEqual([t.txid for t in must], [T.txid])

    def test_47_claimed_tx_weight_cannot_evict_il(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),), weight=c.max_weight)
        a, e = self._miner(c, b"alice", il=(T,))
        self.assertFalse(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [])).ok)
        self.assertTrue(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [T])).ok)


    def test_48_forged_txid_collision_rejected(self):
        c = fresh()
        dummy_in = fund(c, b"user", 1_000, b"dummyin")
        dummy = tx(b"dummy", (dummy_in,), ((b"user", 500),))
        a1, e1 = self._miner(c, b"alice")
        self.assertTrue(connect(c, block(101, c.bits_target, [(b"alice", 50)], [a1], [dummy])).ok)
        real_in = fund(c, b"user", 10_000, b"realin")
        real = tx(b"real", (real_in,), ((b"user", 9_000),))
        from spec import Tx
        forged = Tx(txid=dummy.txid, spends=real.spends, pays=real.pays, weight=400)
        a2, e2 = self._miner(c, b"bob", il=(forged,))
        self.assertIn(
            "bad_txid",
            validate(c, block(102, c.bits_target, [(b"bob", 50)], [a2], [forged])).errors,
        )

    def test_49_negative_values_reject(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", -50)], [a], [])
        self.assertIn("coinbase_negative", validate(c, b).errors)
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", -1),))
        a2, e2 = self._miner(c, b"bob")
        b2 = block(101, c.bits_target, [(b"bob", 50)], [a2], [T])
        self.assertFalse(validate(c, b2).ok)

    def test_50_weight_field_is_not_part_of_il_identity(self):
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),), weight=400)
        copy_t = tx(b"TX-T", (t_coin,), ((b"user", 9_000),), weight=1)
        a, e = self._miner(c, b"alice", il=(T,))
        self.assertTrue(validate(c, block(101, c.bits_target, [(b"alice", 50)], [a], [copy_t])).ok)

    def test_51_il_txid_body_conflict_rejects(self):
        from spec import Tx

        c = fresh()
        t1 = fund(c, b"user", 10_000, b"a")
        t2 = fund(c, b"user", 10_000, b"b")
        A = tx(b"A", (t1,), ((b"user", 1),))
        B = tx(b"B", (t2,), ((b"user", 1),))
        fakeB = Tx(txid=A.txid, spends=B.spends, pays=B.pays, weight=400)
        alice, e1 = self._miner(c, b"alice", il=(A,))
        bob, e2 = self._miner(c, b"bob", il=(fakeB,))
        blk = block(101, c.bits_target, [(b"alice", 25), (b"bob", 25)], [alice, bob], [A])
        self.assertTrue(any(x in validate(c, blk).errors for x in ("bad_txid", "il_txid_conflict")))

    def test_52_too_many_attestations(self):
        from spec import MAX_ATTESTATIONS

        c = fresh()
        atts = []
        payees = []
        for i in range(MAX_ATTESTATIONS + 1):
            name = i.to_bytes(2, "big")
            a, _e = self._miner(c, name)
            atts.append(a)
            if i == 0:
                payees.append((name, 50))
        b = block(101, c.bits_target, payees, atts, [])
        self.assertIn("too_many_attestations", validate(c, b).errors)

    def test_53_stolen_attestation_pow_tag_rejects(self):
        from spec import Attestation

        c = fresh()
        alice, _ae = self._miner(c, b"alice")
        bob, _be = self._miner(c, b"bob")
        stolen = Attestation(
            script=bob.script,
            height=bob.height,
            bits_target=bob.bits_target,
            pow_int=alice.pow_int,
            extra=bob.extra,
            wtxid_tree=bob.wtxid_tree,
            il=bob.il,
        )
        b = block(101, c.bits_target, [(b"bob", 50)], [stolen], [])
        self.assertIn("att_pow_unbind", validate(c, b).errors)

    def test_54_two_outputs_same_attested_script_ok(self):
        c = fresh()
        a, e = self._miner(c, b"alice")
        b = block(101, c.bits_target, [(b"alice", 25), (b"alice", 25)], [a], [])
        self.assertTrue(validate(c, b).ok)

    def test_55_oversized_extra_rejects(self):
        c = fresh()
        name = b"alice"
        coin = fund(c, name, 50_000, b"big")
        pays = ((name, 1),) + tuple((name, 0) for _ in range(700))
        extra = tx(b"big", (coin,), pays)
        a = att(name, 101, c.bits_target, extra, ())
        b = block(101, c.bits_target, [(name, 50)], [a], [])
        self.assertIn("att_extra_weight", validate(c, b).errors)


if __name__ == "__main__":
    unittest.main(verbosity=2)
