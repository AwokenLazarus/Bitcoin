"""Pure ConnectBlock for node-template consensus (soft-fork rules).

Validity is a function of (chain, block) only. IL transaction bodies live in
the attestation vector (and in the last W blocks' vectors). A node that never
saw a tx in its mempool still validates the same way as one that did.

`block.attestations` is the in-memory `vtxa`. A real node would also check
the coinbase NTA OP_RETURN commitment specified in the BIP draft.
"""

from __future__ import annotations

import hashlib
from typing import Dict, List, Sequence, Set, Tuple

from spec import (
    ANYONE,
    MAX_ATTESTATIONS,
    MAX_EXTRA_WEIGHT,
    MAX_IL_TXIDS,
    MAX_IL_WEIGHT,
    OP_RETURN,
    Attestation,
    Block,
    Chain,
    OutPoint,
    Tx,
    Verdict,
)


def share_target(chain: Chain, bits_target: int) -> int:
    return min((1 << 256) - 1, bits_target * chain.k)


def coinbase_weight(block: Block) -> int:
    """Must not trust Block.weight — a finder would set it to max_weight and
    make every IL tx look like it does not fit.
    """
    return 4000 + 40 * len(block.coinbase_outs)


def tx_weight(t: Tx) -> int:
    """Must not trust Tx.weight — a finder would inflate it to evict an IL tx."""
    return 100 + 50 * (len(t.spends) + len(t.pays))


def tx_key(t: Tx) -> tuple:
    """Identity that consensus actually uses. Weight is not part of it."""
    return (t.txid, t.spends, t.pays)


def compute_txid(spends: Sequence, pays: Sequence) -> bytes:
    """txid is a hash of the body, not an attacker-chosen label."""
    return hashlib.sha256(repr((tuple(spends), tuple(pays))).encode()).digest()


def tx_id_ok(t: Tx) -> bool:
    return t.txid == compute_txid(t.spends, t.pays)


def att_digest(a: Attestation) -> bytes:
    payload = (
        a.script,
        a.height,
        a.bits_target,
        tx_key(a.extra),
        tuple(tx_key(t) for t in a.il),
        a.wtxid_tree,
    )
    return hashlib.sha256(repr(payload).encode()).digest()


def att_pow_tag(a: Attestation) -> int:
    """Must match Attestation.pow_int so a copied tag cannot ride a different IL."""
    return int.from_bytes(att_digest(a)[:8], "big")


def block_pow_tag(block: Block) -> int:
    payload = (
        block.height,
        block.bits_target,
        tuple(block.coinbase_outs),
        tuple(tx_key(t) for t in block.txs),
        tuple(att_digest(a) for a in block.attestations),
    )
    return int.from_bytes(hashlib.sha256(repr(payload).encode()).digest()[:8], "big")


def _value_scripts(block: Block) -> List[bytes]:
    out: List[bytes] = []
    for script, value in block.coinbase_outs:
        if script == OP_RETURN:
            continue
        if value > 0:
            out.append(script)
    return out


def _apply_tx(
    utxo: Dict[OutPoint, Tuple[bytes, int]], tx: Tx
) -> Tuple[bool, str]:
    if not tx.spends:
        return False, "tx_no_inputs"
    spent_here: Set[OutPoint] = set()
    in_value = 0
    for op in tx.spends:
        if op in spent_here:
            return False, "tx_internal_double_spend"
        spent_here.add(op)
        if op not in utxo:
            return False, "tx_missing_input"
        _script, val = utxo[op]
        in_value += val
    out_value = sum(v for _s, v in tx.pays)
    if any(v < 0 for _s, v in tx.pays):
        return False, "tx_negative_value"
    if out_value > in_value:
        return False, "tx_overspend"
    for op in tx.spends:
        del utxo[op]
    for i, (script, val) in enumerate(tx.pays):
        utxo[(tx.txid, i)] = (script, val)
    return True, ""


def _tx_valid_against(utxo: Dict[OutPoint, Tuple[bytes, int]], tx: Tx) -> bool:
    probe = dict(utxo)
    ok, _ = _apply_tx(probe, tx)
    return ok


def _il_catalog(chain: Chain, block: Block) -> Tuple[Dict[bytes, Tx], List[str]]:
    """Bodies committed on chain + in this block. Not the local mempool."""
    out: Dict[bytes, Tx] = {}
    errors: List[str] = []

    def add(t: Tx) -> None:
        prev = out.get(t.txid)
        if prev is not None and tx_key(prev) != tx_key(t):
            errors.append("il_txid_conflict")
            return
        out.setdefault(t.txid, t)

    for h, txs in chain.window_ils:
        if chain.height - chain.W < h <= chain.height:
            for t in txs:
                add(t)
    for a in block.attestations:
        for t in a.il:
            add(t)
    return out, errors


def feasible_il(chain: Chain, block: Block) -> List[Tx]:
    """Mandatory txs: valid on the previous UTXO, first-wins by txid on inputs,
    then a weight prefix that fits after coinbase. Independent of block.txs,
    so stuffing cannot evict them.
    """
    cat, cat_err = _il_catalog(chain, block)
    if cat_err:
        return []
    bodies = sorted(cat.values(), key=lambda t: t.txid)
    used_inputs: Set[OutPoint] = set()
    taken: List[Tx] = []
    weight = coinbase_weight(block)
    for t in bodies:
        if tx_key(t) in chain.confirmed:
            continue
        if not _tx_valid_against(chain.utxo, t):
            continue
        if t.spent_set() & used_inputs:
            continue
        w = tx_weight(t)
        if weight + w > chain.max_weight:
            continue
        taken.append(t)
        used_inputs |= set(t.spends)
        weight += w
    return taken


def validate(chain: Chain, block: Block, *, enforce_il: bool = True) -> Verdict:
    errors: List[str] = []

    if block.height != chain.height + 1:
        errors.append("bad_height")
    if block.bits_target != chain.bits_target:
        errors.append("bad_bits")
    if block.pow_int != block_pow_tag(block):
        errors.append("block_pow_unbind")
    if block.pow_int >= chain.bits_target:
        errors.append("block_pow")

    for script, value in block.coinbase_outs:
        if value < 0:
            errors.append("coinbase_negative")
        if script == OP_RETURN and value != 0:
            errors.append("opreturn_value")
        if script == ANYONE and value > 0:
            errors.append("anyone_can_spend")

    payees = _value_scripts(block)
    att_by_script = {a.script: a for a in block.attestations}
    if len(att_by_script) != len(block.attestations):
        errors.append("duplicate_attestation_script")

    for s in payees:
        if s not in att_by_script:
            errors.append(f"unattested_payee:{s!r}")

    if len(block.attestations) > MAX_ATTESTATIONS:
        errors.append("too_many_attestations")

    trees: Dict[Tuple[int, bytes], bytes] = {}
    st = share_target(chain, chain.bits_target)
    for a in block.attestations:
        errors.extend(_check_attestation(chain, block, a, st, trees))

    utxo = dict(chain.utxo)
    used = coinbase_weight(block)
    seen_tx: Dict[bytes, Tx] = {}
    for tx in block.txs:
        if not tx_id_ok(tx):
            errors.append("bad_txid")
            break
        if tx.txid in seen_tx:
            errors.append("duplicate_tx")
            break
        seen_tx[tx.txid] = tx
        used += tx_weight(tx)
        if used > chain.max_weight:
            errors.append("block_weight")
            break
        ok, err = _apply_tx(utxo, tx)
        if not ok:
            errors.append(err)

    if errors:
        return Verdict.reject(*errors)

    if not enforce_il:
        return Verdict.accept()

    _cat, cat_err = _il_catalog(chain, block)
    if cat_err:
        return Verdict.reject(*cat_err)

    must = feasible_il(chain, block)
    have = {tx_key(t) for t in block.txs}
    for t in must:
        if tx_key(t) not in have:
            return Verdict.reject(f"il_unsatisfied:{t.txid.hex()}")

    return Verdict.accept()


def _check_attestation(
    chain: Chain,
    block: Block,
    a: Attestation,
    st: int,
    trees: Dict[Tuple[int, bytes], bytes],
) -> List[str]:
    err: List[str] = []
    if a.height != block.height:
        err.append("att_height")
    if a.bits_target != chain.bits_target:
        err.append("att_bits")
    if a.pow_int != att_pow_tag(a):
        err.append("att_pow_unbind")
    if a.pow_int >= st:
        err.append("att_pow")
    if a.script == ANYONE:
        err.append("att_anyone")
    if len(a.il) > MAX_IL_TXIDS:
        err.append("att_il_size")
    if sum(tx_weight(t) for t in a.il) > MAX_IL_WEIGHT:
        err.append("att_il_weight")
    if tx_weight(a.extra) > MAX_EXTRA_WEIGHT:
        err.append("att_extra_weight")
    if not tx_id_ok(a.extra):
        err.append("bad_txid")
    for t in a.il:
        if not tx_id_ok(t):
            err.append("bad_txid")
            break
    if not a.extra.spends:
        err.append("att_empty_extra")
    if not _tx_valid_against(chain.utxo, a.extra):
        err.append("att_extra_invalid")
    if a.wtxid_tree != a.extra.txid:
        err.append("att_tree_mismatch")
    key = (a.height, a.wtxid_tree)
    if key in trees and trees[key] != a.script:
        err.append("att_multiplex_tree")
    else:
        trees[key] = a.script
    if a.height < block.height - chain.W:
        err.append("att_window")
    return err


def connect(chain: Chain, block: Block, *, enforce_il: bool = True) -> Verdict:
    v = validate(chain, block, enforce_il=enforce_il)
    if not v.ok:
        return v
    for tx in block.txs:
        ok, err = _apply_tx(chain.utxo, tx)
        assert ok, err
    chain.confirmed = chain.confirmed | frozenset(tx_key(tx) for tx in block.txs)
    cb = f"cb{block.height}".encode()
    for i, (script, val) in enumerate(block.coinbase_outs):
        if val > 0 and script != OP_RETURN:
            chain.utxo[(cb, i)] = (script, val)
    for a in block.attestations:
        chain.window_ils.append((block.height, a.il))
    chain.height = block.height
    chain.prune_window()
    return v
