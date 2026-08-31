/// Header obfuscation from OCEAN datum_protocol.c (`datum_header_xor_feedback`).
pub fn xor_u32(header: &mut [u8], key: u32) {
    let v = u32::from_le_bytes(header[0..4].try_into().unwrap()) ^ key;
    header[0..4].copy_from_slice(&v.to_le_bytes());
}

pub fn header_xor_feedback(i: u32) -> u32 {
    let s = 0xb10cfeed_u32;
    let mut h = s;
    let mut k = i;
    k = k.wrapping_mul(0xcc9e2d51);
    k = k.rotate_left(15);
    k = k.wrapping_mul(0x1b873593);
    h ^= k;
    h = h.rotate_left(13);
    h = h.wrapping_mul(5).wrapping_add(0xe6546b64);
    h ^= 4;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^= h >> 16;
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feedback_stable() {
        assert_ne!(header_xor_feedback(0xDC871829), 0xDC871829);
        assert_eq!(header_xor_feedback(1), header_xor_feedback(1));
        // vectors from live DATUM interop (nk 0x9abcdef0)
        assert_eq!(header_xor_feedback(0x9abc_def0), 0x62aa_f25c);
        assert_eq!(header_xor_feedback(!0x9abc_def0), 0x0cef_5178);
    }
}
