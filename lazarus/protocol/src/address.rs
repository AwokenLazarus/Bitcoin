/// Bitcoin address → output script. Identity is the username up to the first `.`.
use sha2::{Digest, Sha256};

const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn polymod(values: &[u8]) -> u32 {
    let gens = [0x3b6a57b2u32, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut chk = 1u32;
    for v in values {
        let b = chk >> 25;
        chk = ((chk & 0x1ffffff) << 5) ^ u32::from(*v);
        for (i, g) in gens.iter().enumerate() {
            if (b >> i) & 1 == 1 {
                chk ^= *g;
            }
        }
    }
    chk
}

fn hrp_expand(hrp: &str) -> Vec<u8> {
    let mut v: Vec<u8> = hrp.bytes().map(|x| x >> 5).collect();
    v.push(0);
    v.extend(hrp.bytes().map(|x| x & 31));
    v
}

fn bech32_decode(addr: &str) -> Option<(String, Vec<u8>, bool)> {
    let lower = addr.to_ascii_lowercase();
    if addr.chars().any(|c| c.is_ascii_uppercase()) && addr.chars().any(|c| c.is_ascii_lowercase()) {
        return None;
    }
    let pos = lower.rfind('1')?;
    if pos < 1 || pos + 7 > lower.len() {
        return None;
    }
    let hrp = &lower[..pos];
    let data_part = &lower[pos + 1..];
    let mut data = Vec::new();
    for c in data_part.bytes() {
        let idx = CHARSET.iter().position(|&x| x == c)?;
        data.push(idx as u8);
    }
    if data.len() < 6 {
        return None;
    }
    let mut values = hrp_expand(hrp);
    values.extend_from_slice(&data);
    let m = polymod(&values);
    let is_m = m == 0x2bc830a3;
    if m != 1 && !is_m {
        return None;
    }
    Some((hrp.to_string(), data[..data.len() - 6].to_vec(), is_m))
}

fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Option<Vec<u8>> {
    let mut acc = 0u32;
    let mut bits = 0u32;
    let mut ret = Vec::new();
    let maxv = (1u32 << to) - 1;
    for &v in data {
        if u32::from(v) >> from != 0 {
            return None;
        }
        acc = (acc << from) | u32::from(v);
        bits += from;
        while bits >= to {
            bits -= to;
            ret.push(((acc >> bits) & maxv) as u8);
        }
    }
    if pad {
        if bits != 0 {
            ret.push(((acc << (to - bits)) & maxv) as u8);
        }
    } else if bits >= from || ((acc << (to - bits)) & maxv) != 0 {
        return None;
    }
    Some(ret)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn hash256(data: &[u8]) -> [u8; 32] {
    sha256(&sha256(data))
}

fn b58_decode(s: &str) -> Option<Vec<u8>> {
    const ALPH: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut n = vec![0u8];
    for c in s.bytes() {
        let d = ALPH.iter().position(|&x| x == c)? as u32;
        let mut carry = d;
        for b in n.iter_mut().rev() {
            let v = u32::from(*b) * 58 + carry;
            *b = (v & 0xff) as u8;
            carry = v >> 8;
        }
        while carry > 0 {
            n.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }
    let lead = s.bytes().take_while(|&c| c == b'1').count();
    let mut out = vec![0u8; lead];
    let start = n.iter().position(|&b| b != 0).unwrap_or(n.len());
    out.extend_from_slice(&n[start..]);
    Some(out)
}

/// Identity string (username up to first `.`) → standard output script.
pub fn identity_script(user: &str) -> Option<Vec<u8>> {
    let ident = user.split('.').next().unwrap_or("").trim();
    if ident.is_empty() {
        return None;
    }
    if ident.to_ascii_lowercase().starts_with("bc1") {
        let (hrp, data, is_m) = bech32_decode(ident)?;
        if hrp != "bc" {
            return None;
        }
        if data.is_empty() {
            return None;
        }
        let ver = data[0];
        let prog = convert_bits(&data[1..], 5, 8, false)?;
        if is_m {
            // bech32m: taproot v1
            if ver != 1 || prog.len() != 32 {
                return None;
            }
            let mut s = vec![0x51, 0x20];
            s.extend_from_slice(&prog);
            return Some(s);
        }
        // bech32: witness v0
        if ver != 0 {
            return None;
        }
        if prog.len() == 20 {
            let mut s = vec![0x00, 0x14];
            s.extend_from_slice(&prog);
            return Some(s);
        }
        if prog.len() == 32 {
            let mut s = vec![0x00, 0x20];
            s.extend_from_slice(&prog);
            return Some(s);
        }
        return None;
    }
    let raw = b58_decode(ident)?;
    if raw.len() < 5 {
        return None;
    }
    let (payload, checksum) = raw.split_at(raw.len() - 4);
    let expect = hash256(payload);
    if checksum != &expect[..4] {
        return None;
    }
    if payload.len() != 21 {
        return None;
    }
    match payload[0] {
        0x00 => {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(&payload[1..]);
            s.extend_from_slice(&[0x88, 0xac]);
            Some(s)
        }
        0x05 => {
            let mut s = vec![0xa9, 0x14];
            s.extend_from_slice(&payload[1..]);
            s.push(0x87);
            Some(s)
        }
        _ => None,
    }
}

/// Username identity (up to first `.`).
pub fn identity_of(user: &str) -> String {
    user.split('.').next().unwrap_or("").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn p2wpkh() {
        // well-known: bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4
        let s = identity_script("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker").unwrap();
        assert_eq!(s[0], 0x00);
        assert_eq!(s[1], 0x14);
        assert_eq!(s.len(), 22);
    }
    #[test]
    fn bad() {
        assert!(identity_script("not-an-address").is_none());
        assert!(identity_script("").is_none());
    }
}
