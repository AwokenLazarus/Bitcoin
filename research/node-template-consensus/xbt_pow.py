# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
# Ports CBlockHeader::GetHash from Bitcoin Knots src/primitives/block.cpp,
# Copyright (c) The Bitcoin Core developers, MIT.
"""BLAKE2b header v2 proof of work, ported line by line from Knots
`src/primitives/block.cpp` (CBlockHeader::GetHash, v29.4.2.knots20260508).

The tests check this port against every vector in Knots'
`src/test/data/block_header_v2.json`, so the v3 model below is built on the real
consensus function, not a sketch of it.
"""

import hashlib
import struct
from dataclasses import dataclass, field, replace

from xbt_secp import sha256, tagged_hash

HEADER_V2_LEN = 164
V2_FLAG = 0x80000000
FLAG_USE_TIME_OFFSET = 0x04


def sha256d(b: bytes) -> bytes:
    return sha256(sha256(b))


def blake2b256(b: bytes) -> bytes:
    return hashlib.blake2b(b, digest_size=32).digest()


def nbits_to_target(nbits: int) -> int:
    exp, mant = nbits >> 24, nbits & 0x007FFFFF
    if nbits & 0x00800000 or mant == 0:
        raise ValueError("negative or zero compact target")
    return mant >> (8 * (3 - exp)) if exp <= 3 else mant << (8 * (exp - 3))


def target_to_nbits(target: int) -> int:
    size = (target.bit_length() + 7) // 8
    mant = target >> (8 * (size - 3)) if size > 3 else target << (8 * (3 - size))
    if mant & 0x00800000:
        mant >>= 8
        size += 1
    return (size << 24) | mant


@dataclass
class HeaderV2:
    version: int = 0x20000000
    prev: bytes = bytes(32)          # internal byte order, as serialized
    merkle_root: bytes = bytes(32)   # internal byte order
    time_on_wire: int = 0
    nbits: int = 0x207FFFFF
    nonce: int = 0
    nonce2: int = 0
    nonce3: int = 0
    extranonce: bytes = bytes(16)
    time_offset: int = 0
    txcount: int = 1
    flags: int = 0
    clear_bits: int = 0
    xor_key: bytes = bytes(16)
    height: int = 0
    rhs: bytes = bytes(32)           # m_mm_rhs: the reserved merge-mining hook slot

    def serialize(self) -> bytes:
        b = struct.pack("<I", self.version | V2_FLAG) + self.prev + self.merkle_root
        b += struct.pack("<IIIII", self.time_on_wire, self.nbits, self.nonce, self.nonce2, self.nonce3)
        b += self.extranonce + struct.pack("<IHBB", self.time_offset, self.txcount, self.flags, self.clear_bits)
        b += self.xor_key + struct.pack("<I", self.height) + self.rhs
        assert len(b) == HEADER_V2_LEN
        return b

    @classmethod
    def parse(cls, b: bytes) -> "HeaderV2":
        if len(b) != HEADER_V2_LEN:
            raise ValueError("header must be 164 bytes")
        (v,) = struct.unpack_from("<I", b, 0)
        if not v & V2_FLAG:
            raise ValueError("not a v2 header")
        t, bits, n1, n2, n3 = struct.unpack_from("<IIIII", b, 68)
        toff, txc, flags, cb = struct.unpack_from("<IHBB", b, 104)
        (height,) = struct.unpack_from("<I", b, 128)
        return cls(v & ~V2_FLAG, b[4:36], b[36:68], t, bits, n1, n2, n3, b[88:104], toff, txc,
                   flags, cb, b[112:128], height, b[132:164])

    def copy(self, **kw) -> "HeaderV2":
        return replace(self, **kw)

    @property
    def ntime(self) -> int:
        return self.time_on_wire + self.time_offset if self.flags & FLAG_USE_TIME_OFFSET else self.time_on_wire

    # --- the staged hash ---------------------------------------------------------------

    def xor_key_hash(self) -> bytes:
        return tagged_hash("Bitcoin block hash PoW XOR key", self.xor_key)

    def mask(self) -> bytes:
        if not any(self.xor_key):
            return bytes(32)
        m = bytearray(tagged_hash("Bitcoin block hash PoW XOR mask", self.xor_key))
        nbytes, rem = self.clear_bits // 8, self.clear_bits % 8
        for i in range(min(nbytes, 32)):
            m[i] = 0
        if nbytes < 32:
            m[nbytes] &= 0xFF >> rem
        return bytes(m)

    def h1(self) -> bytes:
        pre = struct.pack("<I", self.version | V2_FLAG) + self.prev[::-1] + struct.pack("<I", self.height)
        pre += self.merkle_root + struct.pack("<IBIIBB", self.time_on_wire, 0, self.nbits, self.txcount,
                                              self.flags, self.clear_bits)
        pre += self.xor_key_hash()
        assert len(pre) == 119
        return tagged_hash("Bitcoin block header 1", pre)

    def h2(self) -> bytes:
        """The job commitment. `rhs` enters here, so anything in the reserved slot is
        bound into every hash the ASIC computes for this job."""
        return tagged_hash("Merge-mining hook", self.h1() + bytes(32) + self.rhs)

    def root(self) -> bytes:
        """blake2b_1: what the hardware sees as the merkle root (host-side, once per job)."""
        return blake2b256(bytes(4) + self.h2() + self.extranonce)

    def asic_input(self, h2: bytes = None, root: bytes = None) -> bytes:
        h2 = h2 or self.h2()
        root = root or self.root()
        grind = struct.pack("<II", self.nonce, self.nonce2)
        profile = self.flags & 3
        if profile == 0:
            hidden = bytearray(tagged_hash("Bitcoin prevblock header, hashed", self.prev[::-1]))
            hidden[:6] = bytes(6)
            return bytes(hidden) + grind + struct.pack("<II", self.time_offset, self.nonce3) + root
        if profile == 1:
            return grind + struct.pack("<II", self.nonce3, self.time_offset) + root + h2
        pad = bytes(48) if profile == 2 else bytes(80)
        return pad + h2 + grind + struct.pack("<II", self.time_offset, self.nonce3) + root

    def pow_hash(self) -> bytes:
        """Block hash in display order (big-endian), i.e. BLAKE2b(asic_input) XOR mask."""
        h = blake2b256(self.asic_input())
        return bytes(a ^ b for a, b in zip(h, self.mask()))

    def pow_int(self) -> int:
        return int.from_bytes(self.pow_hash(), "big")


def check_pow_v2(h: HeaderV2) -> bool:
    """What an unmodified Knots node checks (CheckBlockHeader): flags and hash <= target."""
    if h.flags & 0xC0:
        return False
    return h.pow_int() <= nbits_to_target(h.nbits)


def merkle_root(leaves) -> bytes:
    level = list(leaves)
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        level = [sha256d(level[i] + level[i + 1]) for i in range(0, len(level), 2)]
    return level[0]
