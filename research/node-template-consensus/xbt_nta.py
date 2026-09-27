# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""XBT-NTA profile: the NTA rules plus what Bitcoin BLAKE2b (XBT) adds.

1. **Payee-key-signed attestations.** Every attestation names an x-only key K, its
   script must be the key-path taproot output `OP_1 <K>`, and it carries a BIP340
   signature by K over the attestation digest (script, height, nBits, extra, IL).
   A pool with a node can still grind an attestation for a customer's script, but
   it can no longer sign it. The base draft's "gifted attestation" residual shrinks
   to custody: whoever signs holds the key that spends that coinbase output.
2. **Commitment placement.** The NTA commitment goes either
   - in the **reserved header slot** (`m_mm_rhs`, bytes 132-163) as one TLV-tagged
     leaf of a small slot tree, so merge-mining and other uses can share the 32 bytes.
     The slot enters H2, so every share of the job commits to it. Knots 29.4.x
     accepts any slot value today (source read and isolated-regtest submitblock,
     XBT-050), so requiring content there is a soft fork; or
   - in a coinbase `OP_RETURN` `"NTA" 0x01 || commit`, as in the base draft.

Validity here is `validate()` from the base model plus the checks below.
"""

from __future__ import annotations

from dataclasses import replace
from typing import Dict, List, Sequence, Tuple

from spec import OP_RETURN, Attestation, Block, Chain, Verdict
from validate import att_digest, block_pow_tag, validate
from xbt_secp import lift_x, schnorr_sign, schnorr_verify, sha256, tagged_hash

NTA_LEAF = b"NTA1"
NTA_OPRETURN_PREFIX = OP_RETURN + b"NTA\x01"
MAX_SLOT_LEAVES = 16


def p2tr(key: bytes) -> bytes:
    """Key-path taproot scriptPubKey for an x-only key."""
    return b"\x51\x20" + key


def att_sighash(a: Attestation) -> bytes:
    """What the payee signs. att_digest covers script (= p2tr(K)), height, nBits,
    extra and IL, so the signature can't move to another template or payee."""
    return tagged_hash("XBT-NTA/attestation", att_digest(a))


def sign_attestation(a: Attestation, sk: int, key: bytes) -> Attestation:
    return replace(a, payee_key=key, sig=schnorr_sign(sk, att_sighash(a)))


def _merkle(leaves: Sequence[bytes]) -> bytes:
    level = list(leaves) or [sha256(b"")]
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        level = [sha256(sha256(level[i] + level[i + 1])) for i in range(0, len(level), 2)]
    return level[0]


def nta_commit(atts: Sequence[Attestation]) -> bytes:
    """Commitment to vtxa, signatures included (so a stripped signature is visible)."""
    leaves = [sha256(sha256(att_digest(a) + a.payee_key + a.sig)) for a in atts]
    return sha256(sha256(_merkle(leaves) + bytes(32)))


def slot_root(leaves: Sequence[Tuple[bytes, bytes]]) -> bytes:
    """Root of the reserved-slot tree: one (4-byte tag, 32-byte value) leaf per use,
    sorted by tag, so uses don't fight over the 32 bytes."""
    return tagged_hash("XBT slot", _merkle([tagged_hash("XBT slot leaf", t + v)
                                           for t, v in sorted(leaves)]))


def with_commitment(block: Block, placement: str = "slot",
                    other_leaves: Sequence[Tuple[bytes, bytes]] = ()) -> Block:
    """Builder: place the NTA commitment for block.attestations."""
    c = nta_commit(block.attestations)
    if placement == "slot":
        leaves = tuple(sorted(list(other_leaves) + [(NTA_LEAF, c)]))
        return replace(block, slot=slot_root(leaves), slot_leaves=leaves)
    outs = [o for o in block.coinbase_outs if not o[0].startswith(NTA_OPRETURN_PREFIX)]
    b = replace(block, coinbase_outs=outs + [(NTA_OPRETURN_PREFIX + c, 0)])
    return replace(b, pow_int=block_pow_tag(b))  # the coinbase changed: re-grind (model tag)


def _check_payee_sig(a: Attestation) -> List[str]:
    if len(a.payee_key) != 32 or lift_x(int.from_bytes(a.payee_key, "big")) is None:
        return ["att_no_payee_key"]
    if a.script != p2tr(a.payee_key):
        return ["att_key_script_mismatch"]
    if not schnorr_verify(a.payee_key, att_sighash(a), a.sig):
        return ["att_bad_sig"]
    return []


def _check_commitment(block: Block, placement: str) -> List[str]:
    c = nta_commit(block.attestations)
    if placement == "slot":
        tags = [t for t, _ in block.slot_leaves]
        if len(tags) != len(set(tags)) or len(tags) > MAX_SLOT_LEAVES:
            return ["slot_bad_leaves"]
        if any(len(t) != 4 or len(v) != 32 for t, v in block.slot_leaves):
            return ["slot_bad_leaves"]
        if block.slot != slot_root(block.slot_leaves):
            return ["slot_root_mismatch"]
        if dict(block.slot_leaves).get(NTA_LEAF) != c:
            return ["nta_commit_mismatch"]
        return []
    outs = [(s, v) for s, v in block.coinbase_outs if s.startswith(NTA_OPRETURN_PREFIX)]
    if len(outs) != 1:
        return ["nta_commit_count"]
    s, v = outs[0]
    if v != 0:
        return ["nta_commit_value"]
    if s[len(NTA_OPRETURN_PREFIX):] != c:
        return ["nta_commit_mismatch"]
    return []


def validate_xbt(chain: Chain, block: Block, *, placement: str = "slot",
                 enforce_il: bool = True) -> Verdict:
    base = validate(chain, block, enforce_il=enforce_il)
    errors = list(base.errors)
    for a in block.attestations:
        errors.extend(_check_payee_sig(a))
    errors.extend(_check_commitment(block, placement))
    return Verdict.reject(*errors) if errors else Verdict.accept()
