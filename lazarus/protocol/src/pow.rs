//! BLAKE2b header v2 (164 bytes) as this chain's Knots hardfork hashes it.
use blake2::{Blake2b, digest::consts::U32, Digest};
use sha2::Sha256;

pub const HEADER_V2_SIZE: usize = 164;
pub const V2_FLAG: u32 = 0x8000_0000;
pub const FLAG_USE_TIME_OFFSET: u8 = 4;

pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    let mut h = Blake2b::<U32>::new();
    h.update(data);
    h.finalize().into()
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn tagged_sha256(tag: &str, data: &[u8]) -> [u8; 32] {
    let t = sha256(tag.as_bytes());
    let mut h = Sha256::new();
    h.update(t);
    h.update(t);
    h.update(data);
    h.finalize().into()
}

pub fn prevblock_hidden(prev_block: &[u8; 32]) -> [u8; 32] {
    let mut display = *prev_block;
    display.reverse();
    let mut out = tagged_sha256("Bitcoin prevblock header, hashed", &display);
    out[..6].fill(0);
    out
}

pub fn xor_mask(xor_key: &[u8; 16], clear_bits: u8) -> [u8; 32] {
    if xor_key.iter().all(|&b| b == 0) {
        return [0u8; 32];
    }
    let mut m = tagged_sha256("Bitcoin block hash PoW XOR mask", xor_key);
    let clear_bytes = (clear_bits / 8) as usize;
    for b in m.iter_mut().take(clear_bytes.min(32)) {
        *b = 0;
    }
    if clear_bytes < 32 {
        m[clear_bytes] &= 0xffu8 >> (clear_bits % 8);
    }
    m
}

/// Compact nBits to a 32-byte target, most significant byte first: the same order as a
/// block id and as `pow_hash`.
pub fn bits_to_target(bits: u32) -> Option<[u8; 32]> {
    let exp = (bits >> 24) as usize;
    let mant = bits & 0x00ff_ffff;
    if mant == 0 || exp > 32 {
        return None;
    }
    let mut be = [0u8; 32];
    let mb = [(mant >> 16) as u8, (mant >> 8) as u8, mant as u8];
    if exp >= 3 {
        let start = 32usize.saturating_sub(exp);
        if start + 3 > 32 {
            return None;
        }
        be[start] = mb[0];
        be[start + 1] = mb[1];
        be[start + 2] = mb[2];
    } else {
        let shift = 8 * (3 - exp);
        let w = mant >> shift;
        be[29] = (w >> 16) as u8;
        be[30] = (w >> 8) as u8;
        be[31] = w as u8;
    }
    Some(be)
}

/// Share target for a power-of-two difficulty, most significant byte first.
/// Difficulty 2^pot means one share per 2^(32+pot) hashes.
pub fn target_for_pot(pot: u8) -> [u8; 32] {
    let bits = 32u32 + u32::from(pot);
    if bits >= 256 {
        return [0u8; 32];
    }
    let mut t = [0xffu8; 32];
    let byte_shift = (bits / 8) as usize;
    let bit_shift = bits % 8;
    if byte_shift > 0 {
        t.copy_within(0..32 - byte_shift, byte_shift);
        t[..byte_shift].fill(0);
    }
    if bit_shift > 0 {
        for i in (1..32).rev() {
            t[i] = (t[i] >> bit_shift) | (t[i - 1] << (8 - bit_shift));
        }
        t[0] >>= bit_shift;
    }
    t
}

/// True if `hash` is at or below `target`. Both are most significant byte first, so this is
/// a plain big-endian comparison.
pub fn meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    hash[..] <= target[..]
}

#[derive(Clone, Debug, Default)]
pub struct HeaderV2 {
    pub version: i32,
    pub prev_block: [u8; 32],
    pub merkle_root: [u8; 32],
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
    pub nonce2: u32,
    pub nonce3: u32,
    pub extranonce: [u8; 16],
    pub time_offset: u32,
    pub txcount: u16,
    pub flags: u8,
    pub xor_key_mask_clear_bits: u8,
    pub xor_key: [u8; 16],
    pub height: i32,
    pub mm_rhs: [u8; 32],
}

impl HeaderV2 {
    pub fn time_on_wire(&self) -> u32 {
        if self.flags & FLAG_USE_TIME_OFFSET == 0 {
            self.time
        } else {
            self.time.wrapping_sub(self.time_offset)
        }
    }

    /// The merge-mining hook. Commits to every header field above the coinbase, and is
    /// what a Sia-style miner receives in place of coinb1.
    pub fn h2(&self) -> [u8; 32] {
        let xor_key_hash = tagged_sha256("Bitcoin block hash PoW XOR key", &self.xor_key);
        let mut prev_display = self.prev_block;
        prev_display.reverse();
        let mut h1d = Vec::with_capacity(119);
        h1d.extend_from_slice(&(self.version as u32 | V2_FLAG).to_le_bytes());
        h1d.extend_from_slice(&prev_display);
        h1d.extend_from_slice(&self.height.to_le_bytes());
        h1d.extend_from_slice(&self.merkle_root);
        h1d.extend_from_slice(&self.time_on_wire().to_le_bytes());
        h1d.push(0);
        h1d.extend_from_slice(&self.bits.to_le_bytes());
        h1d.extend_from_slice(&(self.txcount as u32).to_le_bytes());
        h1d.push(self.flags);
        h1d.push(self.xor_key_mask_clear_bits);
        h1d.extend_from_slice(&xor_key_hash);
        let h1 = tagged_sha256("Bitcoin block header 1", &h1d);
        let mut h2d = [0u8; 96];
        h2d[..32].copy_from_slice(&h1);
        h2d[64..].copy_from_slice(&self.mm_rhs);
        tagged_sha256("Merge-mining hook", &h2d)
    }

    /// blake2b over everything the miner cannot change, its extranonce included. Fixed for
    /// a job, so a miner only iterates the 80-byte ASIC pass below.
    pub fn hash1(&self) -> [u8; 32] {
        let mut ss = [0u8; 52];
        ss[4..36].copy_from_slice(&self.h2());
        ss[36..].copy_from_slice(&self.extranonce);
        blake2b_256(&ss)
    }

    pub fn pow_hash(&self) -> [u8; 32] {
        let hash2 = asic_hash(
            &prevblock_hidden(&self.prev_block),
            &self.hash1(),
            self.nonce,
            self.nonce2,
            self.time_offset,
            self.nonce3,
        );
        let mask = xor_mask(&self.xor_key, self.xor_key_mask_clear_bits);
        let mut result = [0u8; 32];
        for i in 0..32 {
            result[i] = hash2[i] ^ mask[i];
        }
        result
    }

    /// The 164-byte v2 header as it goes into a block.
    pub fn serialize(&self) -> [u8; HEADER_V2_SIZE] {
        let mut b = [0u8; HEADER_V2_SIZE];
        b[0..4].copy_from_slice(&(self.version as u32 | V2_FLAG).to_le_bytes());
        b[4..36].copy_from_slice(&self.prev_block);
        b[36..68].copy_from_slice(&self.merkle_root);
        b[68..72].copy_from_slice(&self.time_on_wire().to_le_bytes());
        b[72..76].copy_from_slice(&self.bits.to_le_bytes());
        b[76..80].copy_from_slice(&self.nonce.to_le_bytes());
        b[80..84].copy_from_slice(&self.nonce2.to_le_bytes());
        b[84..88].copy_from_slice(&self.nonce3.to_le_bytes());
        b[88..104].copy_from_slice(&self.extranonce);
        b[104..108].copy_from_slice(&self.time_offset.to_le_bytes());
        b[108..110].copy_from_slice(&self.txcount.to_le_bytes());
        b[110] = self.flags;
        b[111] = self.xor_key_mask_clear_bits;
        b[112..128].copy_from_slice(&self.xor_key);
        b[128..132].copy_from_slice(&self.height.to_le_bytes());
        b[132..164].copy_from_slice(&self.mm_rhs);
        b
    }

    /// Inverse of `serialize`, so headers the node already accepted can be re-hashed and
    /// checked against our own PoW function.
    pub fn deserialize(b: &[u8; HEADER_V2_SIZE]) -> Self {
        let mut h = HeaderV2::default();
        let v = u32::from_le_bytes(b[0..4].try_into().unwrap());
        h.version = (v & !V2_FLAG) as i32;
        h.prev_block.copy_from_slice(&b[4..36]);
        h.merkle_root.copy_from_slice(&b[36..68]);
        let wire_time = u32::from_le_bytes(b[68..72].try_into().unwrap());
        h.bits = u32::from_le_bytes(b[72..76].try_into().unwrap());
        h.nonce = u32::from_le_bytes(b[76..80].try_into().unwrap());
        h.nonce2 = u32::from_le_bytes(b[80..84].try_into().unwrap());
        h.nonce3 = u32::from_le_bytes(b[84..88].try_into().unwrap());
        h.extranonce.copy_from_slice(&b[88..104]);
        h.time_offset = u32::from_le_bytes(b[104..108].try_into().unwrap());
        h.txcount = u16::from_le_bytes(b[108..110].try_into().unwrap());
        h.flags = b[110];
        h.xor_key_mask_clear_bits = b[111];
        h.xor_key.copy_from_slice(&b[112..128]);
        h.height = i32::from_le_bytes(b[128..132].try_into().unwrap());
        h.mm_rhs.copy_from_slice(&b[132..164]);
        h.time = if h.flags & FLAG_USE_TIME_OFFSET == 0 {
            wire_time
        } else {
            wire_time.wrapping_add(h.time_offset)
        };
        h
    }

    /// coinb1 (39 bytes) for Sia-style stratum: 3 zeros + h2 + 4 zero pad.
    pub fn coinb1_sia(&self) -> [u8; 39] {
        let mut c = [0u8; 39];
        c[3..35].copy_from_slice(&self.h2());
        c
    }
}

/// The 16 bytes `hash1` commits to, from a miner's 12-byte stratum extranonce.
///
/// A Sia-style miner builds its merkle-root field by hashing the coinbase as a merkle
/// *leaf*, which prefixes a 0x00 tag byte: blake2b(0x00 || coinb1 || en1 || en2). Our
/// coinb1 is 3 zero bytes || h2 || 4 zero bytes, so that leaf preimage is
///
/// ```text
/// 0x00 | 000000 | h2(32) | 00000000 | en1(4) | en2(8)
///   = 4 zero bytes | h2(32) | 4 zero bytes | en1(4) | en2(8)   (52 bytes)
/// ```
///
/// `hash1` reads bytes 36..52 of that same preimage as the extranonce, which lands on the
/// 4-byte pad followed by the miner's own nonces. Getting this wrong costs nothing on a
/// share but silently makes every block we assemble unsolved, so the gateway and the
/// verifier both derive it here instead of laying the bytes out by hand.
pub fn header_extranonce(en12: &[u8]) -> [u8; 16] {
    let mut x = [0u8; 16];
    for (i, b) in en12.iter().take(12).enumerate() {
        x[4 + i] = *b;
    }
    x
}

/// The second blake2b pass: the 80 bytes an ASIC iterates while it rolls nonces.
pub fn asic_hash(
    prev_hidden: &[u8; 32],
    hash1: &[u8; 32],
    nonce: u32,
    nonce2: u32,
    time_offset: u32,
    nonce3: u32,
) -> [u8; 32] {
    let mut asic = [0u8; 80];
    asic[..32].copy_from_slice(prev_hidden);
    asic[32..36].copy_from_slice(&nonce.to_le_bytes());
    asic[36..40].copy_from_slice(&nonce2.to_le_bytes());
    asic[40..44].copy_from_slice(&time_offset.to_le_bytes());
    asic[44..48].copy_from_slice(&nonce3.to_le_bytes());
    asic[48..].copy_from_slice(hash1);
    blake2b_256(&asic)
}

/// SHA256d of a serialized tx (Bitcoin txid, internal byte order).
pub fn sha256d(data: &[u8]) -> [u8; 32] {
    sha256(&sha256(data))
}

/// Bitcoin merkle root over txids (internal order). Odd levels duplicate the last hash.
pub fn merkle_root_from_txids(txids: &[[u8; 32]]) -> [u8; 32] {
    if txids.is_empty() {
        return [0u8; 32];
    }
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            let mut cat = [0u8; 64];
            cat[..32].copy_from_slice(&pair[0]);
            cat[32..].copy_from_slice(&pair[1]);
            next.push(sha256d(&cat));
        }
        level = next;
    }
    level[0]
}

/// SHA256d merkle root from coinbase serialization + other txids (internal order).
pub fn merkle_root_sha256d(coinbase_tx: &[u8], txids: &[[u8; 32]]) -> [u8; 32] {
    let mut ids = Vec::with_capacity(txids.len() + 1);
    ids.push(sha256d(coinbase_tx));
    ids.extend_from_slice(txids);
    merkle_root_from_txids(&ids)
}

/// Sibling path proving the coinbase (leaf index 0) is in the merkle tree, as stratum
/// sends it. Fold it with `fold_branches` to recover the root.
pub fn merkle_branches_for_coinbase(txids: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut level: Vec<[u8; 32]> = Vec::with_capacity(txids.len() + 1);
    level.push([0u8; 32]); // the coinbase leaf itself is never read from the path
    level.extend_from_slice(txids);
    let mut branches = Vec::new();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        // the coinbase index is 0 at every level, so its sibling is always index 1
        branches.push(level[1]);
        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            let mut cat = [0u8; 64];
            cat[..32].copy_from_slice(&pair[0]);
            cat[32..].copy_from_slice(&pair[1]);
            next.push(sha256d(&cat));
        }
        level = next;
    }
    branches
}

/// Fold a leaf through a coinbase sibling path (leaf always on the left).
pub fn fold_branches(leaf: [u8; 32], branches: &[[u8; 32]]) -> [u8; 32] {
    let mut h = leaf;
    for b in branches {
        let mut cat = [0u8; 64];
        cat[..32].copy_from_slice(&h);
        cat[32..].copy_from_slice(b);
        h = sha256d(&cat);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pot_target_diff1_has_room() {
        let t = target_for_pot(0);
        // Difficulty 1 is 2^32 hashes: the leading four bytes must be zero.
        assert_eq!(&t[..4], &[0u8; 4]);
        assert_eq!(t[4], 0xff);
        let z = [0u8; 32];
        assert!(meets_target(&z, &t));
        assert!(!meets_target(&[0xffu8; 32], &t));
    }

    /// A higher POT must always be a strictly harder target, with no wraparound at a byte
    /// boundary. Vardiff walks this whole range, so a single bad step would silently hand a
    /// miner an unreachable or a free target.
    #[test]
    fn pot_targets_are_monotonic() {
        let mut prev = target_for_pot(0);
        for pot in 1..=64u8 {
            let t = target_for_pot(pot);
            assert!(t[..] < prev[..], "pot {pot} is not harder than {}", pot - 1);
            prev = t;
        }
    }
    /// The node reports target 000000000000008d4f00... for nBits 0x1a008d4f at this epoch.
    #[test]
    fn mainnet_bits_match_the_node_target() {
        let t = bits_to_target(0x1a008d4f).unwrap();
        assert_eq!(
            hex::encode(t),
            "000000000000008d4f000000000000000000000000000000000000000000000000"[..64].to_string()
        );
    }

    /// The extranonce layout must reproduce, byte for byte, the merkle leaf a Sia-style
    /// miner hashes from the coinb1 we publish. This is the check that was missing when
    /// every share on the pool failed its proof of work.
    #[test]
    fn header_extranonce_matches_the_miner_leaf() {
        let mut h = HeaderV2::default();
        h.height = 962_101;
        h.bits = 0x1a008d4f;
        h.prev_block = [7u8; 32];
        h.merkle_root = [9u8; 32];
        let en1 = [0x49u8, 0x69, 0x01, 0x4d];
        let en2 = [2u8, 0, 0, 0, 0, 0, 0, 0];
        let mut en12 = Vec::new();
        en12.extend_from_slice(&en1);
        en12.extend_from_slice(&en2);
        h.extranonce = header_extranonce(&en12);

        // What the miner hashes: the 0x00 merkle-leaf tag, the 39-byte coinb1 we publish,
        // then its extranonce1 and extranonce2.
        let mut leaf = vec![0u8];
        leaf.extend_from_slice(&h.coinb1_sia());
        leaf.extend_from_slice(&en1);
        leaf.extend_from_slice(&en2);
        assert_eq!(leaf.len(), 52);
        assert_eq!(blake2b_256(&leaf), h.hash1(), "hash1 must equal the miner's leaf");
    }

    #[test]
    fn merkle_one_tx_is_txid() {
        let a = [1u8; 32];
        assert_eq!(merkle_root_from_txids(&[a]), a);
    }
    #[test]
    fn merkle_two_and_three() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        let mut ab = [0u8; 64];
        ab[..32].copy_from_slice(&a);
        ab[32..].copy_from_slice(&b);
        let two = sha256d(&ab);
        assert_eq!(merkle_root_from_txids(&[a, b]), two);
        let mut cc = [0u8; 64];
        cc[..32].copy_from_slice(&c);
        cc[32..].copy_from_slice(&c);
        let c2 = sha256d(&cc);
        let mut top = [0u8; 64];
        top[..32].copy_from_slice(&two);
        top[32..].copy_from_slice(&c2);
        assert_eq!(merkle_root_from_txids(&[a, b, c]), sha256d(&top));
    }
    #[test]
    fn regtest_bits_is_easy() {
        // 0x207fffff → target 0x7fffff << 232, MSB 0x7f
        let t = bits_to_target(0x207fffff).unwrap();
        assert_eq!(t[0], 0x7f);
        assert_eq!(t[1], 0xff);
        assert_eq!(t[2], 0xff);
        let mut easy = [0xffu8; 32];
        easy[0] = 0x00; // MSB clear, below 0x7fffff...
        assert!(meets_target(&easy, &t));
        let hard = [0xffu8; 32];
        assert!(!meets_target(&hard, &t));
    }

    #[test]
    fn branches_fold_back_to_the_root() {
        for n in [0usize, 1, 2, 3, 4, 7, 8, 9, 16, 17, 100, 913] {
            let txids: Vec<[u8; 32]> = (0..n)
                .map(|i| {
                    let mut a = [0u8; 32];
                    a[..8].copy_from_slice(&(i as u64).to_le_bytes());
                    a[31] = (i % 251) as u8;
                    a
                })
                .collect();
            let cb = format!("coinbase-{n}").into_bytes();
            let want = merkle_root_sha256d(&cb, &txids);
            let branches = merkle_branches_for_coinbase(&txids);
            let got = fold_branches(sha256d(&cb), &branches);
            assert_eq!(got, want, "n={n}");
            assert!(branches.len() <= 24, "n={n} branches={}", branches.len());
        }
    }

    #[test]
    fn wrong_branch_changes_the_root() {
        let txids: Vec<[u8; 32]> = (0..5u8).map(|i| [i + 1; 32]).collect();
        let cb = b"coinbase".to_vec();
        let mut branches = merkle_branches_for_coinbase(&txids);
        let good = fold_branches(sha256d(&cb), &branches);
        branches[0][0] ^= 0xff;
        assert_ne!(fold_branches(sha256d(&cb), &branches), good);
    }

    #[test]
    fn serialize_roundtrips_header_fields() {
        let mut h = HeaderV2::default();
        h.version = 0x2000_0000;
        h.prev_block = [3u8; 32];
        h.merkle_root = [4u8; 32];
        h.time = 1_700_000_000;
        h.bits = 0x1703_098d;
        h.nonce = 0xdead_beef;
        h.nonce2 = 0x0102_0304;
        h.txcount = 913;
        h.height = 962_049;
        let b = h.serialize();
        assert_eq!(b.len(), HEADER_V2_SIZE);
        assert_eq!(u32::from_le_bytes(b[72..76].try_into().unwrap()), h.bits);
        assert_eq!(u32::from_le_bytes(b[76..80].try_into().unwrap()), h.nonce);
        assert_eq!(i32::from_le_bytes(b[128..132].try_into().unwrap()), h.height);
        assert_eq!(u16::from_le_bytes(b[108..110].try_into().unwrap()), h.txcount);
    }

    fn golden_header() -> HeaderV2 {
        let mut h = HeaderV2::default();
        h.version = 0x2000_0000;
        h.prev_block = [0x5a; 32];
        h.merkle_root = [0xa5; 32];
        h.time = 1_800_000_000;
        h.bits = 0x1703_098d;
        h.height = 962_049;
        h.txcount = 913;
        h.nonce = 0x1234_5678;
        h.nonce2 = 0x9abc_def0;
        h.nonce3 = 0x0f0f_0f0f;
        h.time_offset = 0;
        h.extranonce = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
        ];
        h
    }

    /// Locks the hashing in place. If a refactor moves a byte, live miners would keep
    /// solving headers we can no longer reproduce, so this vector must never change.
    #[test]
    fn pow_hash_is_stable() {
        let h = golden_header();
        assert_eq!(
            hex::encode(h.pow_hash()),
            "6ee726ff19dc275c4024d488db4cc560338fe9723ea5fa5e5d46a0e54f00602f"
        );
        assert_eq!(
            hex::encode(h.coinb1_sia()),
            "000000f9ca68495589351ece0db5f2b73aca85626d01b0a93624f163115c2df5e57fbc00000000"
        );
    }

    #[test]
    fn coinb1_carries_h2_and_hash1_follows_it() {
        let h = golden_header();
        assert_eq!(&h.coinb1_sia()[3..35], &h.h2()[..]);
        let mut ss = [0u8; 52];
        ss[4..36].copy_from_slice(&h.h2());
        ss[36..].copy_from_slice(&h.extranonce);
        assert_eq!(h.hash1(), blake2b_256(&ss));
    }

    /// Real mainnet headers this node accepted, solved by other people's hardware. Our PoW
    /// function has to reproduce their block ids exactly; if it drifts from consensus, a
    /// share we accept would be worthless and a block we build would be rejected.
    #[test]
    fn mainnet_headers_hash_to_their_block_id() {
        const VECTORS: [(&str, &str); 4] = [
            // height 962094
            (
                "000000a08a5b7dbc0337b142a753a5866bd75425b5836645d8b7bbbd4a0000000000000037510619fdaa9e4789db4474492959bead42940083793308eda540d6f28cb38cb710956a4f8d001aff7b6153f35f0386b710956a00000000b10cf00d04000000000000000000000061030000000000000000000000000000000000002eae0e000000000000000000000000000000000000000000000000000000000000000000",
                "00000000000000752ea27e251cdfdfe7add3023d8da73bddb6483832841f5d4d",
            ),
            // height 962093
            (
                "000000a0feec7d41958832724007c3f9c5f92eeec9571abb4b67f7ef870000000000000025e7be2c7e2a940dfa98ead7149f7bf242afa19cf45c95b74771ddd654be9501760f956a4f8d001ad9a44074e169ae52760f956a00000000b14cf00e1200000000000000000000007c030000000000000000000000000000000000002dae0e000000000000000000000000000000000000000000000000000000000000000000",
                "000000000000004abdbbb7d8456683b52554d76b86a553a742b13703bc7d5b8a",
            ),
            // height 962092
            (
                "000000a0b6e1385dff49ee60384703630c718db81fa5023fde3a964e8500000000000000c5bb5d4e36e8d78ea11f21637f363dc59dca4e18d9e78360820bdb4e186c00fb3a0f956a4f8d001ac685dfa29acae44a3a0f956a00000000b10cf00c04000000000000000000000095000000000000000000000000000000000000002cae0e000000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000087eff7674bbb1a57c9ee2ef9c5f9c3074072328895417decfe",
            ),
            // height 962045
            (
                "000000a0e2dc0205107ac0d6dd56db39646d405ab6eb0f10e976a1da83000000000000001e42d9ddd07f36b18b58629c3745d550b268b6ff742cf036e440ef6fd56bf47fb4f4946a4f8d001aed47399af5f6203bb4f4946a00000000b10cf00d020000000000000000000000ea03040000000000000000000000000000000000fdad0e000000000000000000000000000000000000000000000000000000000000000000",
                "000000000000001a35e6b1647d02f7cdfa1e1b67d8bdd4c6ef1c2dbda524b9af",
            ),
        ];
        for (header_hex, block_id) in VECTORS {
            let raw = hex::decode(header_hex).unwrap();
            let b: [u8; HEADER_V2_SIZE] = raw.try_into().unwrap();
            let h = HeaderV2::deserialize(&b);
            assert_eq!(h.serialize(), b, "serialize must invert deserialize");
            assert_eq!(
                hex::encode(h.pow_hash()),
                block_id,
                "height {}",
                h.height
            );
            let target = bits_to_target(h.bits).expect("bits");
            assert!(meets_target(&h.pow_hash(), &target), "height {}", h.height);
        }
    }
}
