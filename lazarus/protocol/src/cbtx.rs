//! Coinbase serialization shared by the gateway (block assembly) and the verifier
//! (share checking), so both sides agree byte for byte.

use crate::coinbaser::{CoinbaserOutput, CoinbaserV2};

pub fn compact(n: u64) -> Vec<u8> {
    if n < 0xfd {
        vec![n as u8]
    } else if n <= 0xffff {
        let mut v = vec![0xfd];
        v.extend_from_slice(&(n as u16).to_le_bytes());
        v
    } else if n <= 0xffff_ffff {
        let mut v = vec![0xfe];
        v.extend_from_slice(&(n as u32).to_le_bytes());
        v
    } else {
        let mut v = vec![0xff];
        v.extend_from_slice(&n.to_le_bytes());
        v
    }
}

/// BIP34 height push, then the pool tag, then extranonce bytes.
pub fn scriptsig(height: u32, tag: &str, extra: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    if height < 0x100 {
        s.push(1);
        s.push(height as u8);
    } else if height < 0x10000 {
        s.push(2);
        s.extend_from_slice(&(height as u16).to_le_bytes());
    } else {
        s.push(3);
        s.extend_from_slice(&height.to_le_bytes()[..3]);
    }
    s.push(tag.len() as u8);
    s.extend_from_slice(tag.as_bytes());
    s.extend_from_slice(extra);
    s
}

fn push_outs(t: &mut Vec<u8>, cb: &CoinbaserV2, wit: Option<&[u8]>) {
    let mut outs = cb.outputs.clone();
    if let Some(w) = wit {
        outs.push(CoinbaserOutput { sats: 0, script: w.to_vec() });
    }
    t.extend_from_slice(&compact(outs.len() as u64));
    for o in &outs {
        t.extend_from_slice(&o.sats.to_le_bytes());
        t.extend_from_slice(&compact(o.script.len() as u64));
        t.extend_from_slice(&o.script);
    }
}

fn prefix(t: &mut Vec<u8>, height: u32, tag: &str, extra: &[u8]) {
    let ss = scriptsig(height, tag, extra);
    t.extend_from_slice(&[0u8; 32]);
    t.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    t.extend_from_slice(&compact(ss.len() as u64));
    t.extend_from_slice(&ss);
    t.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
}

/// Non-witness serialization: the txid preimage and the merkle leaf.
pub fn coinbase_legacy(
    height: u32,
    tag: &str,
    extra: &[u8],
    cb: &CoinbaserV2,
    wit: Option<&[u8]>,
) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes());
    t.push(1);
    prefix(&mut t, height, tag, extra);
    push_outs(&mut t, cb, wit);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

/// BIP141 serialization for the block: marker/flag, and witness before locktime.
pub fn coinbase_witness(
    height: u32,
    tag: &str,
    extra: &[u8],
    cb: &CoinbaserV2,
    wit: Option<&[u8]>,
) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes());
    t.push(0);
    t.push(1);
    t.push(1);
    prefix(&mut t, height, tag, extra);
    push_outs(&mut t, cb, wit);
    t.push(1);
    t.push(32);
    t.extend_from_slice(&[0u8; 32]);
    t.extend_from_slice(&0u32.to_le_bytes());
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coinbaser::parse_coinbase;

    fn p2wpkh(fill: u8) -> Vec<u8> {
        let mut s = vec![0x00, 0x14];
        s.extend_from_slice(&[fill; 20]);
        s
    }

    fn split() -> CoinbaserV2 {
        CoinbaserV2 {
            id: 3,
            outputs: vec![
                CoinbaserOutput { sats: 700, script: p2wpkh(7) },
                CoinbaserOutput { sats: 300, script: p2wpkh(9) },
            ],
        }
    }

    #[test]
    fn legacy_and_witness_share_outputs_and_txid_preimage() {
        let cb = split();
        let extra = [1u8; 12];
        let wit = [2u8; 38];
        let leg = coinbase_legacy(962_049, "Lazarus", &extra, &cb, Some(&wit));
        let wt = coinbase_witness(962_049, "Lazarus", &extra, &cb, Some(&wit));
        let a = parse_coinbase(&leg).unwrap();
        let b = parse_coinbase(&wt).unwrap();
        assert_eq!(a.legacy, leg);
        assert_eq!(b.legacy, leg, "witness serialization must strip to the legacy txid preimage");
        assert_eq!(a.outputs.len(), 3);
        assert_eq!(a.height, Some(962_049));
        assert_eq!(b.height, Some(962_049));
    }
}
