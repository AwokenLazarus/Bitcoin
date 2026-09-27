# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Mike Moore (AwokenLazarus)
"""secp256k1 arithmetic, BIP340-style Schnorr and an ECVRF, in pure Python.

This is a model, not production crypto: it is not constant time and has had no review.
It exists so the v3 validity rules can be tested end to end.

ECVRF follows the shape of RFC 9381 (ECVRF-*-TAI: try-and-increment hash to curve,
Fiat-Shamir proof Gamma || c || s, 81 bytes) on secp256k1 with our own suite byte. The
property A1 depends on is **uniqueness**: for a key and an input there is exactly one
output beta, however the proof nonce is chosen. A plain Schnorr signature does not have
it (the signer can re-sign with fresh randomness), which is why phase 2 must use a VRF.
"""

import hashlib
import secrets

P = 2**256 - 2**32 - 977
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
G = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)

SUITE = b"\xfe"  # private suite byte: ECVRF-SECP256K1-SHA256-TAI (model)
VRF_PROOF_LEN = 33 + 16 + 32


def sha256(b: bytes) -> bytes:
    return hashlib.sha256(b).digest()


def tagged_hash(tag: str, data: bytes) -> bytes:
    t = sha256(tag.encode())
    return sha256(t + t + data)


# --- point arithmetic (Jacobian internally, affine tuples at the API) -------------------

def _to_jac(p):
    return None if p is None else (p[0], p[1], 1)


def _from_jac(p):
    if p is None or p[2] == 0:
        return None
    zi = pow(p[2], -1, P)
    zi2 = zi * zi % P
    return (p[0] * zi2 % P, p[1] * zi2 * zi % P)


def _jdouble(p):
    if p is None:
        return None
    x, y, z = p
    if y == 0:
        return None
    s = 4 * x * y * y % P
    m = 3 * x * x % P
    x3 = (m * m - 2 * s) % P
    y3 = (m * (s - x3) - 8 * pow(y, 4, P)) % P
    z3 = 2 * y * z % P
    return (x3, y3, z3)


def _jadd(p, q):
    if p is None:
        return q
    if q is None:
        return p
    x1, y1, z1 = p
    x2, y2, z2 = q
    z1s, z2s = z1 * z1 % P, z2 * z2 % P
    u1, u2 = x1 * z2s % P, x2 * z1s % P
    s1, s2 = y1 * z2s * z2 % P, y2 * z1s * z1 % P
    if u1 == u2:
        return _jdouble(p) if s1 == s2 else None
    h, r = (u2 - u1) % P, (s2 - s1) % P
    h2 = h * h % P
    h3 = h2 * h % P
    x3 = (r * r - h3 - 2 * u1 * h2) % P
    y3 = (r * (u1 * h2 - x3) - s1 * h3) % P
    return (x3, y3, h * z1 * z2 % P)


def point_add(p, q):
    return _from_jac(_jadd(_to_jac(p), _to_jac(q)))


def point_neg(p):
    return None if p is None else (p[0], (-p[1]) % P)


def point_mul(p, k: int):
    k %= N
    acc = None
    base = _to_jac(p)
    while k:
        if k & 1:
            acc = _jadd(acc, base)
        base = _jdouble(base)
        k >>= 1
    return _from_jac(acc)


def is_on_curve(p) -> bool:
    return p is not None and (p[1] * p[1] - p[0] ** 3 - 7) % P == 0


def lift_x(x: int):
    """The point with x coordinate `x` and even y, or None (BIP340)."""
    if x >= P:
        return None
    c = (pow(x, 3, P) + 7) % P
    y = pow(c, (P + 1) // 4, P)
    if y * y % P != c:
        return None
    return (x, y if y % 2 == 0 else P - y)


def compress(p) -> bytes:
    return bytes([2 + (p[1] & 1)]) + p[0].to_bytes(32, "big")


def decompress(b: bytes):
    if len(b) != 33 or b[0] not in (2, 3):
        return None
    pt = lift_x(int.from_bytes(b[1:], "big"))
    if pt is None:
        return None
    return pt if (pt[1] & 1) == (b[0] & 1) else point_neg(pt)


# --- keys ------------------------------------------------------------------------------

def keypair(seed: bytes = None):
    """(secret scalar with even-y public point, 32-byte x-only public key)."""
    sk = int.from_bytes(sha256(seed), "big") % N if seed is not None else secrets.randbelow(N - 1) + 1
    if sk == 0:
        sk = 1
    pub = point_mul(G, sk)
    if pub[1] & 1:
        sk = N - sk
        pub = point_neg(pub)
    return sk, pub[0].to_bytes(32, "big")


# --- BIP340-style Schnorr (used for coinbase authorisation and the grinding contrast) ---

def schnorr_sign(sk: int, msg: bytes, aux: bytes = b"\x00" * 32) -> bytes:
    pub = point_mul(G, sk)
    d = sk if pub[1] % 2 == 0 else N - sk
    px = pub[0].to_bytes(32, "big")
    t = (d ^ int.from_bytes(tagged_hash("BIP0340/aux", aux), "big")).to_bytes(32, "big")
    k0 = int.from_bytes(tagged_hash("BIP0340/nonce", t + px + msg), "big") % N
    r = point_mul(G, k0)
    k = k0 if r[1] % 2 == 0 else N - k0
    rx = r[0].to_bytes(32, "big")
    e = int.from_bytes(tagged_hash("BIP0340/challenge", rx + px + msg), "big") % N
    return rx + ((k + e * d) % N).to_bytes(32, "big")


def schnorr_verify(px: bytes, msg: bytes, sig: bytes) -> bool:
    if len(px) != 32 or len(sig) != 64:
        return False
    pub = lift_x(int.from_bytes(px, "big"))
    r, s = int.from_bytes(sig[:32], "big"), int.from_bytes(sig[32:], "big")
    if pub is None or r >= P or s >= N:
        return False
    e = int.from_bytes(tagged_hash("BIP0340/challenge", sig[:32] + px + msg), "big") % N
    rp = point_add(point_mul(G, s), point_neg(point_mul(pub, e)))
    return rp is not None and rp[1] % 2 == 0 and rp[0] == r


# --- ECVRF (unique output per key and input) -------------------------------------------

def _hash_to_curve(pk33: bytes, alpha: bytes):
    for ctr in range(256):
        h = sha256(SUITE + b"\x01" + pk33 + alpha + bytes([ctr]) + b"\x00")
        pt = decompress(b"\x02" + h)
        if pt is not None:
            return pt
    raise ValueError("hash_to_curve failed")  # probability 2^-256


def _challenge(*points) -> int:
    data = SUITE + b"\x02" + b"".join(compress(p) for p in points) + b"\x00"
    return int.from_bytes(sha256(data)[:16], "big")


def vrf_prove(sk: int, alpha: bytes, nonce: int = None) -> bytes:
    """81-byte proof. `nonce` overrides the deterministic nonce (tests use it to show
    that a different proof for the same input still yields the same beta)."""
    y = point_mul(G, sk)
    pk33 = compress(y)
    h = _hash_to_curve(pk33, alpha)
    gamma = point_mul(h, sk)
    if nonce is None:
        nonce = int.from_bytes(sha256(sk.to_bytes(32, "big") + compress(h)), "big") % N
    c = _challenge(y, h, gamma, point_mul(G, nonce), point_mul(h, nonce))
    s = (nonce + c * sk) % N
    return compress(gamma) + c.to_bytes(16, "big") + s.to_bytes(32, "big")


def vrf_proof_to_hash(proof: bytes) -> bytes:
    gamma = decompress(proof[:33])
    return sha256(SUITE + b"\x03" + compress(gamma) + b"\x00")


def vrf_verify(px: bytes, alpha: bytes, proof: bytes):
    """beta (32 bytes) if `proof` is valid for x-only key `px` and `alpha`, else None."""
    if len(proof) != VRF_PROOF_LEN or len(px) != 32:
        return None
    y = lift_x(int.from_bytes(px, "big"))
    gamma = decompress(proof[:33])
    c = int.from_bytes(proof[33:49], "big")
    s = int.from_bytes(proof[49:], "big")
    if y is None or gamma is None or s >= N:
        return None
    h = _hash_to_curve(compress(y), alpha)
    u = point_add(point_mul(G, s), point_neg(point_mul(y, c)))
    v = point_add(point_mul(h, s), point_neg(point_mul(gamma, c)))
    if u is None or v is None or _challenge(y, h, gamma, u, v) != c:
        return None
    return vrf_proof_to_hash(proof)
