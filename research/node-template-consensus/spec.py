"""Node-template consensus — constants and types.

Abstract ConnectBlock model of the Node Template Attestation *soft fork*.
The production packaging (not executed here) is a SegWit-style coinbase
OP_RETURN commitment to an attestation vector `vtxa` that old peers strip.
This file only models the new validity rules.

Problem
-------
Stratum V1 lets a pool be the only template author. Hashers are usernames.
The pool can omit transactions (veto / OFAC filter) and the chain cannot tell
a 50% pool from a 50% solo farm.

What consensus CAN enforce
--------------------------
1. Coinbase value may only pay scripts that attached a compact self-pay
   attestation in THIS block (deterministic; no gossip-set).
2. Each attestation commits to a compact inclusion list (IL) of full txs.
3. Each attestation carries IL **transaction bodies** (capped), not txids
   that nodes look up in their mempool. ConnectBlock is a pure function of
   (chain, block). Two nodes with different mempools cannot disagree.
4. Feasible IL txs are those valid against the **previous** UTXO set, with
   input conflicts broken by txid order, fitting in the block weight budget
   before any optional txs. All of them must appear in the block. Filling
   the block with junk cannot evict them.
   Paying a node-miner therefore surrenders veto over that miner's IL.
   A DATUM split cannot take someone's hashrate and throw away their template.

What consensus CANNOT enforce
-----------------------------
A gifted attestation (pool node, customer script). A self-paying PPS pool
that settles hashers off-chain. Off-chain IOUs and later spends.

Identity in this model: txid is SHA256 of (spends, pays). Attestation and
block pow tags are SHA256 of committed fields so a stolen tag cannot ride a
different IL or tx set. Sidecar size is capped (512 attestations, 32k extra,
32k IL). Bitcoin serialization uses SHA256D / wtxid; see the BIP draft.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, FrozenSet, List, Optional, Sequence, Tuple

# Bitcoin-like block weight cap (WU).
MAX_BLOCK_WEIGHT = 4_000_000
# FOCIL uses 8 KiB ILs; 32-byte txids => 256 entries.
MAX_IL_TXIDS = 256
# Serialized-size stand-in (WU). FOCIL's 8 KiB ≈ this order of magnitude.
MAX_IL_WEIGHT = 32_000
# Sidecar DoS caps (DATUM coinbase already caps ~512 outputs).
MAX_ATTESTATIONS = 512
MAX_EXTRA_WEIGHT = 32_000
# Attestation extra tx must exist so headers-only SPV shares are illegal.
MIN_ATTESTATION_TXS = 2  # coinbase + extra
# Eligibility / IL carry-forward window, in blocks (~1 day at 10 min).
WINDOW_BLOCKS = 144
# Share target is k× easier than the block target at that height.
SHARE_K = 1000
# OP_RETURN marker in this model.
OP_RETURN = b"OP_RETURN"
# Anyone-can-spend marker.
ANYONE = b""


OutPoint = Tuple[bytes, int]  # (txid, vout)


@dataclass(frozen=True)
class Tx:
    txid: bytes
    spends: Tuple[OutPoint, ...]
    pays: Tuple[Tuple[bytes, int], ...]  # (script, value)
    weight: int

    def spent_set(self) -> FrozenSet[OutPoint]:
        return frozenset(self.spends)


@dataclass(frozen=True)
class Attestation:
    """Compact weak-template proof + IL. Carried in the block sidecar.

    Size is header + coinbase + extra tx + merkle + IL, not a full mempool.
    """

    script: bytes
    # Height of the block this attestation is spent in (must equal block.height).
    height: int
    bits_target: int  # must equal the chain's block target at this height
    pow_int: int
    extra: Tx
    # Must equal extra.txid in this model (one extra tx). Same extra => one script.
    wtxid_tree: bytes
    il: Tuple[Tx, ...]


@dataclass
class Block:
    height: int
    bits_target: int
    pow_int: int
    coinbase_outs: List[Tuple[bytes, int]]  # (script, value); 0-value OP_RETURN ok
    txs: List[Tx]  # not including coinbase
    attestations: List[Attestation]
    weight: int = 4000  # coinbase overhead; plus txs in validate


@dataclass
class Chain:
    height: int = 0
    bits_target: int = 1 << 240
    k: int = SHARE_K
    W: int = WINDOW_BLOCKS
    max_weight: int = MAX_BLOCK_WEIGHT
    utxo: Dict[OutPoint, Tuple[bytes, int]] = field(default_factory=dict)
    # Confirmed transaction *bodies* (txid, spends, pays) — not txid alone.
    confirmed: FrozenSet[tuple] = field(default_factory=frozenset)
    # (height, il txs) from attestations in recent blocks — bodies, not ids.
    window_ils: List[Tuple[int, Tuple[Tx, ...]]] = field(default_factory=list)

    def share_target(self) -> int:
        return min((1 << 256) - 1, self.bits_target * self.k)

    def prune_window(self) -> None:
        lo = self.height - self.W + 1
        self.window_ils = [(h, ils) for h, ils in self.window_ils if h >= lo]


@dataclass
class Verdict:
    ok: bool
    errors: Tuple[str, ...]

    @staticmethod
    def accept() -> "Verdict":
        return Verdict(True, ())

    @staticmethod
    def reject(*errors: str) -> "Verdict":
        return Verdict(False, tuple(errors))
