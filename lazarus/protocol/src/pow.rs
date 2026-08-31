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

/// Compact nBits → 32-byte target with the most-significant byte at index 31
/// (same layout `meets_target` compares). Bitcoin compact is big-endian mantissa.
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
    let mut t = [0u8; 32];
    for i in 0..32 {
        t[i] = be[31 - i];
    }
    Some(t)
}

/// Share target for a power-of-two difficulty (POT byte).
pub fn target_for_pot(pot: u8) -> [u8; 32] {
    // difficulty 2^pot → target = (2^256 / (2^32 * 2^pot)) in hash-compared (LE high-byte-last) form.
    // Browser miner compares hashLe[31] first (most significant).
    // A pot of 0 is difficulty 1: top 4 bytes zero-ish via 2^32 hashes.
    let bits = 32u32 + u32::from(pot);
    let mut t = [0xffu8; 32];
    let byte_shift = (bits / 8) as usize;
    let bit_shift = (bits % 8) as u32;
    if byte_shift >= 32 {
        return [0u8; 32];
    }
    if byte_shift > 0 {
        t.copy_within(byte_shift.., 0);
        t[32 - byte_shift..].fill(0);
    }
    if bit_shift > 0 {
        for i in 0..31 {
            t[i] = (t[i] >> bit_shift) | (t[i + 1] << (8 - bit_shift));
        }
        t[31] >>= bit_shift;
    }
    t
}

/// Compare hash (LE, high byte at [31]) against target (same layout). True if hash <= target.
pub fn meets_target(hash_le: &[u8; 32], target: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        if hash_le[i] < target[i] {
            return true;
        }
        if hash_le[i] > target[i] {
            return false;
        }
    }
    true
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

    pub fn pow_hash_le(&self) -> [u8; 32] {
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
        let h2 = tagged_sha256("Merge-mining hook", &h2d);
        let mut ss = [0u8; 52];
        ss[4..36].copy_from_slice(&h2);
        ss[36..].copy_from_slice(&self.extranonce);
        let hash1 = blake2b_256(&ss);
        // Profile 0 ASIC input (80 bytes)
        let mut asic = Vec::with_capacity(80);
        asic.extend_from_slice(&prevblock_hidden(&self.prev_block));
        asic.extend_from_slice(&self.nonce.to_le_bytes());
        asic.extend_from_slice(&self.nonce2.to_le_bytes());
        asic.extend_from_slice(&self.time_offset.to_le_bytes());
        asic.extend_from_slice(&self.nonce3.to_le_bytes());
        asic.extend_from_slice(&hash1);
        let hash2 = blake2b_256(&asic);
        let mask = xor_mask(&self.xor_key, self.xor_key_mask_clear_bits);
        let mut result = [0u8; 32];
        for i in 0..32 {
            result[i] = hash2[i] ^ mask[i];
        }
        result
    }

    /// coinb1 (39 bytes) for Sia-style stratum: 3 zeros + h2 + 4 zero pad.
    pub fn coinb1_sia(&self) -> [u8; 39] {
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
        let h2 = tagged_sha256("Merge-mining hook", &h2d);
        let mut c = [0u8; 39];
        c[3..35].copy_from_slice(&h2);
        c
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pot_target_diff1_has_room() {
        let t = target_for_pot(0);
        assert!(t.iter().any(|&b| b != 0));
        let z = [0u8; 32];
        assert!(meets_target(&z, &t));
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
        assert_eq!(t[31], 0x7f);
        assert_eq!(t[30], 0xff);
        assert_eq!(t[29], 0xff);
        let mut easy = [0xffu8; 32];
        easy[31] = 0x00; // MSB clear — below 0x7fffff...
        assert!(meets_target(&easy, &t));
        let hard = [0xffu8; 32];
        assert!(!meets_target(&hard, &t));
    }
}
