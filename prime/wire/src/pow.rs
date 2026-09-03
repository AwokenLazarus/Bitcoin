//! BLAKE2b header-v2 proof of work, as Bitcoin Knots defines it for this chain.
//!
//! The 164-byte header is never hashed directly. Consensus builds it in stages so that
//! unmodified Sia-layout hardware can grind it:
//!
//! 1. **H1** — tagged SHA256 ("Bitcoin block header 1") over the template-fixed fields:
//!    version (with the v2 bit), previous block (display order), height, merkle root,
//!    serialized time, a zero byte, nBits, tx count, flags, XOR-mask clear bits, and the
//!    tagged hash of the XOR key.
//! 2. **H2** — tagged SHA256 ("Merge-mining hook") over `H1 || 32 zero bytes || rhs`.
//!    This is the job's *commitment*; the Sia `coinb1` is `00 00 00 || H2 || 00 00 00 00`.
//! 3. **root** — `BLAKE2b-256(0x00 || coinb1 || extranonce(12))`, a 52-byte preimage.
//!    This is what the hardware sees as its merkle root.
//! 4. **work** — the 80 bytes the hardware hashes: `hidden_prev(32) || nonce(8) ||
//!    ntime(8) || root(32)`, where `hidden_prev` is a tagged SHA256 of the previous block
//!    hash with its first six bytes cleared.
//! 5. **hash** — `BLAKE2b-256(work) XOR mask`, byte-reversed into internal order so it
//!    compares against targets like a SHA256d hash would.
//!
//! Everything a Prime needs to redo this is in a DATUM share plus its job section; the
//! transactions themselves are never required.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use sha2::Sha256;

pub const HEADER_V2_LEN: usize = 164;
/// Version bit that marks a v2 header. H1 always includes it.
pub const V2_BIT: u32 = 0x8000_0000;
/// Header flag: block time is `time_on_wire + time_offset`.
pub const FLAG_USE_TIME_OFFSET: u8 = 0x04;
/// Flags Knots rejects (`bad-flags-highbits`).
pub const FLAGS_RESERVED_HIGH: u8 = 0xC0;
pub const SIA_COINB1_LEN: usize = 39;
pub const WORK_LEN: usize = 80;

pub type Hash = [u8; 32];

#[inline]
pub fn sha256(data: &[u8]) -> Hash {
    Sha256::digest(data).into()
}

#[inline]
pub fn sha256d(data: &[u8]) -> Hash {
    sha256(&sha256(data))
}

/// BIP340-style tagged hash: `SHA256(SHA256(tag) || SHA256(tag) || data)`.
pub fn tagged_sha256(tag: &str, data: &[u8]) -> Hash {
    let t = sha256(tag.as_bytes());
    let mut h = Sha256::new();
    h.update(t);
    h.update(t);
    h.update(data);
    h.finalize().into()
}

#[inline]
pub fn blake2b256(data: &[u8]) -> Hash {
    Blake2b::<U32>::digest(data).into()
}

/// Fold a coinbase txid through stratum-style merkle branches to the block's merkle root.
pub fn merkle_root(txid: Hash, branches: &[Hash]) -> Hash {
    let mut acc = txid;
    let mut buf = [0u8; 64];
    for b in branches {
        buf[..32].copy_from_slice(&acc);
        buf[32..].copy_from_slice(b);
        acc = sha256d(&buf);
    }
    acc
}

/// Stratum merkle branches for a coinbase given the other txids, in template order.
/// Used to build test fixtures the way a gateway would.
pub fn merkle_branches_for_coinbase(txids: &[Hash]) -> Vec<Hash> {
    let mut branches = Vec::new();
    // level 0 has the coinbase placeholder at index 0 followed by the txids
    let mut level: Vec<Option<Hash>> = std::iter::once(None).chain(txids.iter().copied().map(Some)).collect();
    while level.len() > 1 {
        branches.push(level[1].expect("sibling of the coinbase path is always a real hash"));
        if level.len() % 2 == 1 {
            let last = *level.last().unwrap();
            level.push(last);
        }
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(match (pair[0], pair[1]) {
                (Some(a), Some(b)) => {
                    let mut buf = [0u8; 64];
                    buf[..32].copy_from_slice(&a);
                    buf[32..].copy_from_slice(&b);
                    Some(sha256d(&buf))
                }
                _ => None,
            });
        }
        level = next;
    }
    branches
}

/// Template-fixed header fields that make up the job commitment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commitment {
    pub version: u32,
    /// Internal byte order (as it sits in the header).
    pub prev_hash: Hash,
    pub height: u32,
    pub merkle_root: Hash,
    pub time_on_wire: u32,
    pub nbits: u32,
    pub txcount: u32,
    pub flags: u8,
    pub xor_clear_bits: u8,
    pub xor_key: [u8; 16],
    pub rhs: Hash,
}

impl Commitment {
    /// H1 preimage, 119 bytes.
    fn h1_preimage(&self) -> [u8; 119] {
        let mut p = [0u8; 119];
        p[0..4].copy_from_slice(&(self.version | V2_BIT).to_le_bytes());
        for i in 0..32 {
            p[4 + i] = self.prev_hash[31 - i];
        }
        p[36..40].copy_from_slice(&self.height.to_le_bytes());
        p[40..72].copy_from_slice(&self.merkle_root);
        p[72..76].copy_from_slice(&self.time_on_wire.to_le_bytes());
        p[76] = 0;
        p[77..81].copy_from_slice(&self.nbits.to_le_bytes());
        p[81..85].copy_from_slice(&self.txcount.to_le_bytes());
        p[85] = self.flags;
        p[86] = self.xor_clear_bits;
        p[87..119].copy_from_slice(&xor_key_hash(&self.xor_key));
        p
    }

    /// The commitment (H2) the Sia coinb1 carries.
    pub fn h2(&self) -> Hash {
        let h1 = tagged_sha256("Bitcoin block header 1", &self.h1_preimage());
        let mut p = [0u8; 96];
        p[..32].copy_from_slice(&h1);
        p[64..].copy_from_slice(&self.rhs);
        tagged_sha256("Merge-mining hook", &p)
    }
}

pub fn xor_key_hash(key: &[u8; 16]) -> Hash {
    tagged_sha256("Bitcoin block hash PoW XOR key", key)
}

/// The mask XORed onto the BLAKE2b output. All zero when the key is all zero.
pub fn xor_mask(key: &[u8; 16], clear_bits: u8) -> Hash {
    if key.iter().all(|&b| b == 0) {
        return [0u8; 32];
    }
    let mut m = tagged_sha256("Bitcoin block hash PoW XOR mask", key);
    let bytes = usize::from(clear_bits / 8);
    let rem = clear_bits % 8;
    for b in m.iter_mut().take(bytes) {
        *b = 0;
    }
    if bytes < 32 {
        m[bytes] &= 0xff >> rem;
    }
    m
}

/// Sia `coinb1` for a job: three zero bytes, H2, four zero bytes.
pub fn sia_coinb1(h2: &Hash) -> [u8; SIA_COINB1_LEN] {
    let mut c = [0u8; SIA_COINB1_LEN];
    c[3..35].copy_from_slice(h2);
    c
}

/// What the hardware treats as the merkle root: `BLAKE2b(0x00 || coinb1 || extranonce)`.
pub fn work_root(h2: &Hash, extranonce: &[u8; 12]) -> Hash {
    let mut leaf = [0u8; 52];
    leaf[1..40].copy_from_slice(&sia_coinb1(h2));
    leaf[40..].copy_from_slice(extranonce);
    blake2b256(&leaf)
}

/// The 32 bytes sent in the previous-hash slot: tagged hash of the display-order previous
/// block hash with the first six bytes cleared.
pub fn sia_prevhash(prev_internal: &Hash) -> Hash {
    let mut display = [0u8; 32];
    for i in 0..32 {
        display[i] = prev_internal[31 - i];
    }
    let mut h = tagged_sha256("Bitcoin prevblock header, hashed", &display);
    h[..6].fill(0);
    h
}

/// The 80 bytes the hardware hashes.
pub fn work_header(sia_prev: &Hash, nonce8: &[u8; 8], ntime8: &[u8; 8], root: &Hash) -> [u8; WORK_LEN] {
    let mut w = [0u8; WORK_LEN];
    w[..32].copy_from_slice(sia_prev);
    w[32..40].copy_from_slice(nonce8);
    w[40..48].copy_from_slice(ntime8);
    w[48..].copy_from_slice(root);
    w
}

/// Final block hash in internal (little-endian) byte order.
pub fn pow_hash_le(work: &[u8; WORK_LEN], xor_key: &[u8; 16], clear_bits: u8) -> Hash {
    let h = blake2b256(work);
    let m = xor_mask(xor_key, clear_bits);
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[31 - i] = h[i] ^ m[i];
    }
    out
}

/// Precomputed per-job pieces so a share costs one BLAKE2b to check.
#[derive(Clone, Debug)]
pub struct JobWork {
    pub sia_prev: Hash,
    pub root: Hash,
    pub mask: Hash,
}

impl JobWork {
    pub fn new(c: &Commitment, extranonce: &[u8; 12]) -> Self {
        JobWork {
            sia_prev: sia_prevhash(&c.prev_hash),
            root: work_root(&c.h2(), extranonce),
            mask: xor_mask(&c.xor_key, c.xor_clear_bits),
        }
    }

    #[inline]
    pub fn hash(&self, nonce8: &[u8; 8], ntime8: &[u8; 8]) -> Hash {
        let w = work_header(&self.sia_prev, nonce8, ntime8, &self.root);
        let h = blake2b256(&w);
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[31 - i] = h[i] ^ self.mask[i];
        }
        out
    }
}

/// Share target for a power-of-two difficulty, internal byte order:
/// `(2^224 - 1) >> pot`. `None` when `pot >= 224`.
pub fn share_target_le(pot: u8) -> Option<Hash> {
    if pot >= 224 {
        return None;
    }
    let mut t = [0xffu8; 32];
    t[28..].fill(0);
    shr_le(&mut t, u32::from(pot));
    Some(t)
}

fn shr_le(n: &mut [u8; 32], bits: u32) {
    let bytes = (bits / 8) as usize;
    let rem = bits % 8;
    if bytes > 0 {
        n.copy_within(bytes.., 0);
        n[32 - bytes..].fill(0);
    }
    if rem > 0 {
        for i in 0..31 {
            n[i] = (n[i] >> rem) | (n[i + 1] << (8 - rem));
        }
        n[31] >>= rem;
    }
}

/// Decode a compact `nBits` target into internal byte order. `None` if it does not fit or
/// is negative.
pub fn nbits_to_target_le(nbits: u32) -> Option<Hash> {
    let exp = (nbits >> 24) as usize;
    let mant = nbits & 0x007f_ffff;
    if nbits & 0x0080_0000 != 0 || mant == 0 {
        return None;
    }
    let mut t = [0u8; 32];
    if exp <= 3 {
        let v = mant >> (8 * (3 - exp));
        t[..4].copy_from_slice(&v.to_le_bytes());
    } else {
        if exp > 32 {
            return None;
        }
        let b = mant.to_le_bytes();
        for (i, &byte) in b.iter().take(3).enumerate() {
            let idx = exp - 3 + i;
            if idx < 32 {
                t[idx] = byte;
            } else if byte != 0 {
                return None;
            }
        }
    }
    Some(t)
}

/// `hash <= target`, both internal (little-endian) byte order.
#[inline]
pub fn meets_target(hash_le: &Hash, target_le: &Hash) -> bool {
    for i in (0..32).rev() {
        if hash_le[i] != target_le[i] {
            return hash_le[i] < target_le[i];
        }
    }
    true
}

/// Approximate difficulty of a hash relative to a difficulty-1 share (`2^224 - 1`), as the
/// number of leading zero bits beyond the base 32. Useful for logs.
pub fn hash_pot(hash_le: &Hash) -> u32 {
    let mut lz = 0u32;
    for i in (0..32).rev() {
        if hash_le[i] == 0 {
            lz += 8;
        } else {
            lz += hash_le[i].leading_zeros();
            break;
        }
    }
    lz.saturating_sub(32)
}

/// The nTime a node reads from a header built from this share.
pub fn share_ntime(time_on_wire: u32, ntime8: &[u8; 8], flags: u8) -> u32 {
    if flags & FLAG_USE_TIME_OFFSET == 0 {
        return time_on_wire;
    }
    time_on_wire.wrapping_add(u32::from_le_bytes(ntime8[..4].try_into().unwrap()))
}

/// Serialize the full 164-byte header from a commitment plus the share's grinding fields.
pub fn serialize_header_v2(
    c: &Commitment,
    nonce8: &[u8; 8],
    ntime8: &[u8; 8],
    extranonce: &[u8; 12],
) -> [u8; HEADER_V2_LEN] {
    let mut h = [0u8; HEADER_V2_LEN];
    h[0..4].copy_from_slice(&(c.version | V2_BIT).to_le_bytes());
    h[4..36].copy_from_slice(&c.prev_hash);
    h[36..68].copy_from_slice(&c.merkle_root);
    h[68..72].copy_from_slice(&c.time_on_wire.to_le_bytes());
    h[72..76].copy_from_slice(&c.nbits.to_le_bytes());
    h[76..84].copy_from_slice(nonce8);
    h[84..88].copy_from_slice(&ntime8[4..8]);
    // 88..92 zero: the header extranonce is 16 bytes, the wire carries the last 12
    h[92..104].copy_from_slice(extranonce);
    h[104..108].copy_from_slice(&ntime8[0..4]);
    h[108..110].copy_from_slice(&(c.txcount as u16).to_le_bytes());
    h[110] = c.flags;
    h[111] = c.xor_clear_bits;
    h[112..128].copy_from_slice(&c.xor_key);
    h[128..132].copy_from_slice(&c.height.to_le_bytes());
    h[132..164].copy_from_slice(&c.rhs);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(s: &str) -> Vec<u8> {
        hex::decode(s).unwrap()
    }
    fn hx32(s: &str) -> Hash {
        hx(s).try_into().unwrap()
    }

    /// Vector from the gateway's own test suite (`datum_pow_blake2b_vector_tests`).
    #[test]
    fn gateway_vector_with_xor_key_and_time_offset() {
        let mut prev = [0u8; 32];
        let mut merkle = [0u8; 32];
        let mut rhs = [0u8; 32];
        for i in 0..32u8 {
            prev[i as usize] = 0xc0 + i;
            merkle[i as usize] = i;
            rhs[i as usize] = 0x80 + i;
        }
        let mut xor_key = [0u8; 16];
        for i in 0..16u8 {
            xor_key[i as usize] = 0x10 + i;
        }
        let mut en = [0u8; 12];
        for i in 0..12u8 {
            en[i as usize] = 0xa0 + i;
        }
        let nonce: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut ntime = [0u8; 8];
        ntime[..4].copy_from_slice(&0x0102_0304u32.to_le_bytes());
        ntime[4..].copy_from_slice(&0x1817_1615u32.to_le_bytes());

        let c = Commitment {
            version: 0x2000_0000,
            prev_hash: prev,
            height: 12345,
            merkle_root: merkle,
            time_on_wire: 0x6553_412f,
            nbits: 0x207f_ffff,
            txcount: 3,
            flags: FLAG_USE_TIME_OFFSET,
            xor_clear_bits: 13,
            xor_key,
            rhs,
        };
        let h2 = c.h2();
        assert_eq!(h2, hx32("be3009118e9fbe8be787c9fef5ee1a34c95b92efe7c6f1d430c488e094ce94a8"));
        let root = work_root(&h2, &en);
        assert_eq!(root, hx32("2ae3e2ac5e7b16faeda5b13386d9b3fb0e5ddfa803deee88eb9a1f6ce65c9110"));
        let work = work_header(&sia_prevhash(&prev), &nonce, &ntime, &root);
        assert_eq!(
            hex::encode(work),
            "0000000000008a7f7054908ed879cc78d133dc6604fb0fd017552289799cabd6\
             01020304050607080403020115161718\
             2ae3e2ac5e7b16faeda5b13386d9b3fb0e5ddfa803deee88eb9a1f6ce65c9110"
        );
        let hash = pow_hash_le(&work, &xor_key, 13);
        assert_eq!(hash, hx32("15ed05ccf950c40f149ea623b77f7f3f58afb9ab3ab723d5ca5870338c42d935"));
        assert_eq!(JobWork::new(&c, &en).hash(&nonce, &ntime), hash);

        let hdr = serialize_header_v2(&c, &nonce, &ntime, &en);
        assert_eq!(
            hex::encode(hdr),
            "000000a0c0c1c2c3c4c5c6c7c8c9cacbcccdcecfd0d1d2d3d4d5d6d7d8d9dadbdcdddedf\
             000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f2f415365ffff7f20\
             01020304050607081516171800000000a0a1a2a3a4a5a6a7a8a9aaab040302010300040d\
             101112131415161718191a1b1c1d1e1f39300000\
             808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f"
        );
        assert_eq!(share_ntime(0x6553_412f, &ntime, FLAG_USE_TIME_OFFSET), 0x6553_412fu32.wrapping_add(0x0102_0304));
        assert_eq!(share_ntime(0x6553_412f, &ntime, 0), 0x6553_412f);
    }

    /// Canonical profile-0 vector published with Knots' header-v2 implementation
    /// (`src/test/data/block_header_v2.json`, `profile_0_time_offset`).
    #[test]
    fn knots_consensus_vector() {
        let prev = hx32("1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100");
        let merkle = hx32("00112233445566778899aabbccddeeff00102030405060708090a0b0c0d0e0f0");
        let rhs = hx32("8967452301efcdab8967452301efcdab8967452301efcdab8967452301efcdab");
        let c = Commitment {
            version: 0x2000_0000,
            prev_hash: prev,
            height: 840_000,
            merkle_root: merkle,
            time_on_wire: 2_000_000_000 - 600,
            nbits: 0x1d00_ffff,
            txcount: 3,
            flags: 0x1c,
            xor_clear_bits: 0,
            xor_key: [0u8; 16],
            rhs,
        };
        assert_eq!(c.h2(), hx32("ab5becb2336a3701557b0f6e33de39bd333072b8494c7c60952a8e8a636565e3"));
        let root = hx32("7e6326906eaa52fe59e03a14f1dfb8dd5d6e78497e56a8a6e4f4fb4d385e43db");
        let mut nonce = [0u8; 8];
        nonce[..4].copy_from_slice(&0x0bad_f00du32.to_le_bytes());
        nonce[4..].copy_from_slice(&0x1122_3344u32.to_le_bytes());
        let mut ntime = [0u8; 8];
        ntime[..4].copy_from_slice(&600u32.to_le_bytes());
        ntime[4..].copy_from_slice(&0x89ab_cdefu32.to_le_bytes());
        let work = work_header(&sia_prevhash(&prev), &nonce, &ntime, &root);
        assert_eq!(
            hex::encode(work),
            "000000000000943aff74219e1f45899abfdf536373c0f2fc92e6fe58335cd0ad\
             0df0ad0b4433221158020000efcdab89\
             7e6326906eaa52fe59e03a14f1dfb8dd5d6e78497e56a8a6e4f4fb4d385e43db"
        );
        assert_eq!(
            pow_hash_le(&work, &[0u8; 16], 0),
            hx32("04d78755b174467ec8537c230912ddd9bc4f28229b795b78490ad705cf5d494b")
        );
    }

    #[test]
    fn targets() {
        let t4 = share_target_le(4).unwrap();
        assert!(t4[..27].iter().all(|&b| b == 0xff));
        assert_eq!(t4[27], 0x0f);
        assert!(t4[28..].iter().all(|&b| b == 0));
        assert!(share_target_le(224).is_none());
        // difficulty-1 target in compact form: 0x1d00ffff => 0x00000000ffff0000...0000
        let t = nbits_to_target_le(0x1d00_ffff).unwrap();
        assert_eq!(hex::encode(t), "0000000000000000000000000000000000000000000000000000ffff00000000");
        // the live chain's nBits
        let t = nbits_to_target_le(0x193c_2d40).unwrap();
        let mut disp = t;
        disp.reverse();
        assert_eq!(hex::encode(disp), "000000000000003c2d4000000000000000000000000000000000000000000000");
        assert!(nbits_to_target_le(0x1d80_ffff).is_none());
        assert!(nbits_to_target_le(0x2200_ffff).is_none());
        let mut small = t;
        small[31] = 0;
        assert!(meets_target(&small, &t));
        assert!(meets_target(&t, &t));
        let mut big = t;
        big[31] = 1;
        assert!(!meets_target(&big, &t));
        assert_eq!(hash_pot(&share_target_le(0).unwrap()), 0);
        assert_eq!(hash_pot(&share_target_le(13).unwrap()), 13);
    }

    #[test]
    fn merkle_branches_and_root_agree() {
        let txids: Vec<Hash> = (1..=7u8).map(|i| [i; 32]).collect();
        let cb = [0xcc; 32];
        let branches = merkle_branches_for_coinbase(&txids);
        assert_eq!(branches.len(), 3);
        // full tree the long way
        let mut level: Vec<Hash> = std::iter::once(cb).chain(txids.iter().copied()).collect();
        while level.len() > 1 {
            if level.len() % 2 == 1 {
                level.push(*level.last().unwrap());
            }
            level = level
                .chunks(2)
                .map(|p| {
                    let mut b = [0u8; 64];
                    b[..32].copy_from_slice(&p[0]);
                    b[32..].copy_from_slice(&p[1]);
                    sha256d(&b)
                })
                .collect();
        }
        assert_eq!(merkle_root(cb, &branches), level[0]);
        assert_eq!(merkle_root(cb, &[]), cb);
        assert!(merkle_branches_for_coinbase(&[]).is_empty());
    }

    #[test]
    fn xor_mask_clear_bits() {
        let key = [1u8; 16];
        let full = xor_mask(&key, 0);
        assert_ne!(full, [0u8; 32]);
        let m = xor_mask(&key, 13);
        assert_eq!(m[0], 0);
        assert_eq!(m[1], full[1] & 0x07);
        assert_eq!(&m[2..], &full[2..]);
        assert_eq!(xor_mask(&[0u8; 16], 13), [0u8; 32]);
    }
}
