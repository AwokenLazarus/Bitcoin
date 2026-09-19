"""Builders for the reference chain."""

from __future__ import annotations

from typing import Optional, Sequence, Tuple

from spec import Attestation, Block, Chain, OutPoint, Tx
from validate import att_pow_tag, block_pow_tag, compute_txid


def fund(chain: Chain, script: bytes, value: int, tag: bytes) -> OutPoint:
    op: OutPoint = (tag, 0)
    chain.utxo[op] = (script, value)
    return op


def tx(
    _label: bytes,
    spends: Sequence[OutPoint],
    pays: Sequence[Tuple[bytes, int]],
    weight: int = 400,
) -> Tx:
    spends_t = tuple(spends)
    pays_t = tuple(pays)
    return Tx(
        txid=compute_txid(spends_t, pays_t),
        spends=spends_t,
        pays=pays_t,
        weight=weight,
    )


def att(
    script: bytes,
    height: int,
    bits_target: int,
    extra: Tx,
    il: Sequence[Tx] = (),
    pow_int: Optional[int] = None,
) -> Attestation:
    draft = Attestation(
        script=script,
        height=height,
        bits_target=bits_target,
        pow_int=0,
        extra=extra,
        wtxid_tree=extra.txid,
        il=tuple(il),
    )
    tag = att_pow_tag(draft)
    return Attestation(
        script=script,
        height=height,
        bits_target=bits_target,
        pow_int=tag if pow_int is None else pow_int,
        extra=extra,
        wtxid_tree=extra.txid,
        il=tuple(il),
    )


def block(
    height: int,
    bits_target: int,
    payees: Sequence[Tuple[bytes, int]],
    attestations: Sequence[Attestation],
    txs: Sequence[Tx],
    pow_int: int = 0,
    extra_outs: Sequence[Tuple[bytes, int]] = (),
) -> Block:
    outs = list(payees) + list(extra_outs)
    draft = Block(
        height=height,
        bits_target=bits_target,
        pow_int=0,
        coinbase_outs=outs,
        txs=list(txs),
        attestations=list(attestations),
        weight=4000,
    )
    tag = block_pow_tag(draft)
    return Block(
        height=height,
        bits_target=bits_target,
        pow_int=tag if pow_int == 0 else pow_int,
        coinbase_outs=outs,
        txs=list(txs),
        attestations=list(attestations),
        weight=4000,
    )
