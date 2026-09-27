#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""XBT-NTA profile: payee-key-signed attestations and the reserved-slot commitment.
Run: python3 test_xbt.py"""

from __future__ import annotations

import json
import os
import unittest
from dataclasses import replace

from helpers import att, block, fund, tx
from spec import Chain
from validate import validate
from xbt_nta import (NTA_LEAF, NTA_OPRETURN_PREFIX, nta_commit, p2tr, sign_attestation,
                     slot_root, validate_xbt, with_commitment)
from xbt_pow import HeaderV2, check_pow_v2, nbits_to_target
from xbt_secp import keypair

HERE = os.path.dirname(os.path.abspath(__file__))


def fresh() -> Chain:
    return Chain(height=100, bits_target=1 << 200, k=1000, W=144)


class XbtNtaTests(unittest.TestCase):
    def _payee(self, chain: Chain, seed: bytes, il=(), sign_with=None):
        """Attestation for the taproot script of `seed`'s key, signed by `sign_with`
        (default: the payee itself)."""
        sk, key = keypair(seed)
        script = p2tr(key)
        coin = fund(chain, script, 50_000, b"fund-" + seed)
        extra = tx(b"e-" + seed, (coin,), ((script, 49_000),))
        a = att(script, chain.height + 1, chain.bits_target, extra, il)
        signer_sk, signer_key = sign_with if sign_with else (sk, key)
        return sign_attestation(a, signer_sk, signer_key), script

    def test_x01_signed_payee_accepts_slot_and_opreturn(self):
        for placement in ("slot", "opreturn"):
            with self.subTest(placement):
                c = fresh()
                a, s = self._payee(c, b"alice")
                b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []), placement)
                self.assertTrue(validate_xbt(c, b, placement=placement).ok, validate_xbt(c, b, placement=placement))

    def test_x02_split_each_payee_signs(self):
        c = fresh()
        a1, s1 = self._payee(c, b"alice")
        a2, s2 = self._payee(c, b"bob")
        b = with_commitment(block(101, c.bits_target, [(s1, 30), (s2, 20)], [a1, a2], []))
        self.assertTrue(validate_xbt(c, b).ok)

    def test_x03_gifted_attestation_now_fails(self):
        """Base draft test 30 accepts a pool-ground attestation for a customer's
        script. With payee signatures the pool can't produce one without the key."""
        c = fresh()
        pool = keypair(b"pool")
        a, s = self._payee(c, b"customer", sign_with=pool)
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []))
        self.assertTrue(validate(c, b).ok)  # the base draft accepts it
        self.assertIn("att_key_script_mismatch", validate_xbt(c, b).errors)
        # claiming the customer's key without its secret: signature fails
        _, cust_key = keypair(b"customer")
        forged = replace(a, payee_key=cust_key)
        b2 = with_commitment(block(101, c.bits_target, [(s, 50)], [forged], []))
        self.assertIn("att_bad_sig", validate_xbt(c, b2).errors)

    def test_x04_unsigned_or_non_taproot_payee_rejects(self):
        c = fresh()
        coin = fund(c, b"legacy-script", 50_000, b"fund-l")
        extra = tx(b"e-l", (coin,), ((b"legacy-script", 49_000),))
        a = att(b"legacy-script", 101, c.bits_target, extra, ())
        b = with_commitment(block(101, c.bits_target, [(b"legacy-script", 50)], [a], []))
        self.assertIn("att_no_payee_key", validate_xbt(c, b).errors)

    def test_x05_signature_does_not_survive_il_swap(self):
        """The pool takes a payee's signed attestation and swaps in its own IL."""
        c = fresh()
        t_coin = fund(c, b"user", 10_000, b"tfund")
        T = tx(b"TX-T", (t_coin,), ((b"user", 9_000),))
        a, s = self._payee(c, b"alice", il=(T,))
        stripped = att(a.script, a.height, a.bits_target, a.extra, ())
        stripped = replace(stripped, payee_key=a.payee_key, sig=a.sig)
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [stripped], []))
        self.assertIn("att_bad_sig", validate_xbt(c, b).errors)
        # the honest block must include T (base IL rule still applies)
        b_ok = with_commitment(block(101, c.bits_target, [(s, 50)], [a], [T]))
        self.assertTrue(validate_xbt(c, b_ok).ok)
        b_veto = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []))
        self.assertTrue(any("il_unsatisfied" in e for e in validate_xbt(c, b_veto).errors))

    def test_x06_custody_residual(self):
        """Residual: a pool that holds the farm's key can sign for it. Then the pool can
        also spend that coinbase output: custody of farm keys, or self-pay PPS."""
        c = fresh()
        farm = keypair(b"farm")  # the pool holds this secret
        a, s = self._payee(c, b"farm", sign_with=farm)
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []))
        self.assertTrue(validate_xbt(c, b).ok)

    def test_x07_slot_commitment_errors(self):
        c = fresh()
        a, s = self._payee(c, b"alice")
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []))
        self.assertIn("slot_root_mismatch", validate_xbt(c, replace(b, slot=bytes(32))).errors)
        no_leaf = replace(b, slot_leaves=(), slot=slot_root(()))
        self.assertIn("nta_commit_mismatch", validate_xbt(c, no_leaf).errors)
        wrong = ((NTA_LEAF, bytes(32)),)
        self.assertIn("nta_commit_mismatch", validate_xbt(c, replace(b, slot_leaves=wrong, slot=slot_root(wrong))).errors)
        dup = ((b"MM01", bytes(32)), (b"MM01", b"\x01" * 32), (NTA_LEAF, nta_commit([a])))
        self.assertIn("slot_bad_leaves", validate_xbt(c, replace(b, slot_leaves=dup, slot=slot_root(dup))).errors)

    def test_x08_slot_shared_with_other_uses(self):
        c = fresh()
        a, s = self._payee(c, b"alice")
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []), "slot",
                            other_leaves=[(b"MM01", b"\x11" * 32), (b"ANCH", b"\x22" * 32)])
        self.assertTrue(validate_xbt(c, b).ok)

    def test_x09_opreturn_commitment_errors(self):
        c = fresh()
        a, s = self._payee(c, b"alice")
        base = block(101, c.bits_target, [(s, 50)], [a], [])
        self.assertIn("nta_commit_count", validate_xbt(c, base, placement="opreturn").errors)
        b = with_commitment(base, "opreturn")
        two = replace(b, coinbase_outs=b.coinbase_outs + [(NTA_OPRETURN_PREFIX + bytes(32), 0)])
        self.assertIn("nta_commit_count", validate_xbt(c, two, placement="opreturn").errors)

    def test_x10_slot_is_bound_into_the_job_and_old_nodes_accept(self):
        """On the real v2 header: the slot enters H2 (every share of the job commits to
        the attestation root), and a header carrying it still passes the unmodified
        v2 PoW check, so the rule only tightens validity (soft fork). Knots 29.4.1rc4
        and 29.4.2rc2 accepted non-zero slots via submitblock on isolated regtest."""
        with open(os.path.join(HERE, "block_header_v2_vector.json")) as f:
            vec = json.load(f)
        h = HeaderV2.parse(bytes.fromhex(vec["serialized"]))
        self.assertEqual(h.pow_hash().hex(), vec["block_hash"])  # port matches Knots
        c = fresh()
        a, s = self._payee(c, b"alice")
        b = with_commitment(block(101, c.bits_target, [(s, 50)], [a], []))
        h2 = h.copy(rhs=b.slot, nbits=0x207FFFFF)
        self.assertNotEqual(h2.root(), h.copy(rhs=bytes(32), nbits=0x207FFFFF).root())
        while not check_pow_v2(h2):
            h2.nonce += 1
        self.assertLessEqual(h2.pow_int(), nbits_to_target(h2.nbits))


if __name__ == "__main__":
    unittest.main()
