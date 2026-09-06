//! The coinbase transaction as a gateway ships it: `coinb1 || extranonce slot || coinb2`,
//! legacy (non-witness) serialization.
//!
//! On a BLAKE2b job the extranonce lives in the header, so the 12-byte slot in the
//! coinbase is all zeros, and one byte in `coinb1` (the *target byte*) carries the
//! share's power-of-two difficulty so a Prime can tell what target the gateway checked.

use crate::{Cursor, Error, Result};

pub const EXTRANONCE_SLOT: usize = 12;
/// Witness the gateway attaches to the coinbase: one 32-byte reserved value of zeros,
/// matching the `default_witness_commitment` a node hands out with the template.
pub const COINBASE_WITNESS: [u8; 34] = {
    let mut w = [0u8; 34];
    w[0] = 0x01;
    w[1] = 0x20;
    w
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOut {
    pub value: u64,
    pub script: Vec<u8>,
}

impl TxOut {
    pub fn is_op_return(&self) -> bool {
        self.script.first() == Some(&0x6a)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coinbase {
    pub version: u32,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
    pub outputs: Vec<TxOut>,
    pub lock_time: u32,
    /// BIP34 height from the first scriptSig push, if it decodes.
    pub height: Option<u32>,
    /// Byte offset of the outputs section, so callers can splice the witness in.
    outputs_at: usize,
    lock_time_at: usize,
}

impl Coinbase {
    /// Saturating: the bytes are the gateway's, and a sum past `u64::MAX` must classify as
    /// nonsense, not panic the session.
    pub fn total_output_value(&self) -> u64 {
        self.outputs.iter().fold(0u64, |a, o| a.saturating_add(o.value))
    }

    /// Sum paid to a specific scriptPubKey (saturating, as above).
    pub fn paid_to(&self, script: &[u8]) -> u64 {
        self.outputs.iter().filter(|o| o.script == script).fold(0u64, |a, o| a.saturating_add(o.value))
    }
}

/// Put the pieces of a BLAKE2b share's coinbase back together with its target byte.
///
/// Stock gateways split the coinbase around a 12-byte extranonce slot in the scriptSig and
/// name the byte that carries the share's target. `lazarus-gateway` (the pool's own public
/// stratum) instead sends the whole legacy coinbase as `coinb1`, nothing in `coinb2`, and
/// `target_byte_index` 0 — a shape a stock gateway can never produce (its slot always has
/// outputs after it, and byte 0 is the version). That form is taken as is.
pub fn assemble(coinb1: &[u8], coinb2: &[u8], target_byte_index: usize, target_pot: u8) -> Vec<u8> {
    if coinb2.is_empty() && target_byte_index == 0 {
        return coinb1.to_vec();
    }
    let mut v = Vec::with_capacity(coinb1.len() + EXTRANONCE_SLOT + coinb2.len());
    v.extend_from_slice(coinb1);
    v.resize(coinb1.len() + EXTRANONCE_SLOT, 0);
    v.extend_from_slice(coinb2);
    if target_byte_index < v.len() {
        v[target_byte_index] = target_pot;
    }
    v
}

pub fn read_varint(c: &mut Cursor) -> Result<u64> {
    Ok(match c.u8()? {
        0xfd => u64::from(c.u16()?),
        0xfe => u64::from(c.u32()?),
        0xff => c.u64()?,
        n => u64::from(n),
    })
}

pub fn write_varint(out: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        out.push(n as u8);
    } else if n <= 0xffff {
        out.push(0xfd);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        out.push(0xfe);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(0xff);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

fn bip34_height(script_sig: &[u8]) -> Option<u32> {
    let (&n, rest) = script_sig.split_first()?;
    match n {
        0x51..=0x60 => Some(u32::from(n - 0x50)),
        1..=4 => {
            let b = rest.get(..n as usize)?;
            let mut v = 0u32;
            for (i, &x) in b.iter().enumerate() {
                v |= u32::from(x) << (8 * i);
            }
            Some(v)
        }
        _ => None,
    }
}

/// Parse a legacy-serialized coinbase. Must be exactly one input spending the null
/// outpoint and consume the whole buffer.
pub fn parse(bytes: &[u8]) -> Result<Coinbase> {
    let mut c = Cursor::new(bytes);
    let version = c.u32()?;
    if read_varint(&mut c)? != 1 {
        return Err(Error::Malformed("coinbase input count"));
    }
    let prevout = c.take(36)?;
    if prevout[..32] != [0u8; 32] || prevout[32..] != [0xff; 4] {
        return Err(Error::Malformed("coinbase outpoint"));
    }
    let n = read_varint(&mut c)? as usize;
    if !(2..=100).contains(&n) {
        return Err(Error::Malformed("coinbase scriptSig length"));
    }
    let script_sig = c.take(n)?.to_vec();
    let sequence = c.u32()?;
    let outputs_at = c.pos;
    let nout = read_varint(&mut c)? as usize;
    if nout == 0 || nout > 10_000 {
        return Err(Error::Malformed("coinbase output count"));
    }
    let mut outputs = Vec::with_capacity(nout.min(1024));
    for _ in 0..nout {
        let value = c.u64()?;
        let sl = read_varint(&mut c)? as usize;
        if sl > 10_000 {
            return Err(Error::Malformed("coinbase output script length"));
        }
        outputs.push(TxOut { value, script: c.take(sl)?.to_vec() });
    }
    let lock_time_at = c.pos;
    let lock_time = c.u32()?;
    if c.remaining() != 0 {
        return Err(Error::Malformed("coinbase trailing bytes"));
    }
    let height = bip34_height(&script_sig);
    Ok(Coinbase { version, script_sig, sequence, outputs, lock_time, height, outputs_at, lock_time_at })
}

/// Re-serialize a parsed legacy coinbase in segwit form with the standard reserved-value
/// witness, for assembling a full block.
pub fn with_witness(legacy: &[u8], parsed: &Coinbase) -> Vec<u8> {
    let mut v = Vec::with_capacity(legacy.len() + 2 + COINBASE_WITNESS.len());
    v.extend_from_slice(&legacy[..4]);
    v.push(0x00);
    v.push(0x01);
    v.extend_from_slice(&legacy[4..parsed.lock_time_at]);
    v.extend_from_slice(&COINBASE_WITNESS);
    v.extend_from_slice(&legacy[parsed.lock_time_at..]);
    let _ = parsed.outputs_at;
    v
}

/// Build a legacy coinbase for tests and tools: BIP34 height, tag, a 12-byte zero
/// extranonce slot, then the outputs. Returns `(bytes, target_byte_index)` where the target
/// byte is the last byte of the tag push (a gateway puts it inside its scriptSig too).
pub fn build(height: u32, tag: &[u8], outputs: &[TxOut], lock_time: u32) -> (Vec<u8>, usize) {
    let mut sig = Vec::new();
    let hb = height.to_le_bytes();
    let n = 4 - hb.iter().rev().take_while(|&&b| b == 0).count();
    let n = n.max(1);
    sig.push(n as u8);
    sig.extend_from_slice(&hb[..n]);
    // tag push: tag || 1 target byte || 12 extranonce bytes
    let push_len = tag.len() + 1 + EXTRANONCE_SLOT;
    sig.push(push_len as u8);
    sig.extend_from_slice(tag);
    let target_in_sig = sig.len();
    sig.push(0xff);
    sig.extend_from_slice(&[0u8; EXTRANONCE_SLOT]);

    let mut v = Vec::new();
    v.extend_from_slice(&1u32.to_le_bytes());
    v.push(1);
    v.extend_from_slice(&[0u8; 32]);
    v.extend_from_slice(&[0xff; 4]);
    write_varint(&mut v, sig.len() as u64);
    let sig_at = v.len();
    v.extend_from_slice(&sig);
    v.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    write_varint(&mut v, outputs.len() as u64);
    for o in outputs {
        v.extend_from_slice(&o.value.to_le_bytes());
        write_varint(&mut v, o.script.len() as u64);
        v.extend_from_slice(&o.script);
    }
    v.extend_from_slice(&lock_time.to_le_bytes());
    (v, sig_at + target_in_sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_handles_both_gateway_shapes() {
        let outs = vec![TxOut { value: 5, script: vec![0x00, 0x14, 0x11] }];
        let (full, tidx) = build(7, b"t", &outs, 0);
        // stock: coinb1 ends with the target byte, the 12-byte slot follows, coinb2 is the rest
        let c1 = &full[..tidx + 1];
        let c2 = &full[tidx + 1 + EXTRANONCE_SLOT..];
        let a = assemble(c1, c2, tidx, 9);
        assert_eq!(a.len(), full.len());
        assert_eq!(a[tidx], 9);
        assert_eq!(&a[tidx + 1..tidx + 1 + EXTRANONCE_SLOT], &[0u8; EXTRANONCE_SLOT]);
        assert_eq!(&a[tidx + 1 + EXTRANONCE_SLOT..], c2);
        // lazarus-gateway: the whole coinbase in coinb1, nothing else
        assert_eq!(assemble(&full, &[], 0, 9), full);
        assert_eq!(full[0], 1, "version byte untouched");
    }

    #[test]
    fn build_parse_and_split_round_trip() {
        let outs = vec![
            TxOut { value: 100, script: vec![0x00, 0x14, 1, 2, 3] },
            TxOut { value: 0, script: vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed] },
        ];
        let (cb, tidx) = build(966_267, b"Lazarus", &outs, 0);
        assert_eq!(cb[tidx], 0xff);
        let p = parse(&cb).unwrap();
        assert_eq!(p.height, Some(966_267));
        assert_eq!(p.outputs, outs);
        assert!(p.outputs[1].is_op_return());
        assert_eq!(p.paid_to(&[0x00, 0x14, 1, 2, 3]), 100);

        // split at the extranonce slot as a gateway would and reassemble with a target byte
        let slot = tidx + 1;
        let coinb1 = &cb[..slot];
        let coinb2 = &cb[slot + EXTRANONCE_SLOT..];
        let again = assemble(coinb1, coinb2, tidx, 9);
        let mut expect = cb.clone();
        expect[tidx] = 9;
        assert_eq!(again, expect);

        let sw = with_witness(&cb, &p);
        assert_eq!(&sw[..4], &cb[..4]);
        assert_eq!(&sw[4..6], &[0, 1]);
        assert_eq!(sw.len(), cb.len() + 2 + 34);
        assert_eq!(&sw[sw.len() - 4..], &cb[cb.len() - 4..]);
    }

    #[test]
    fn rejects_non_coinbase_shapes() {
        let (mut cb, _) = build(1, b"x", &[TxOut { value: 1, script: vec![0x51] }], 0);
        cb[5] = 1; // prevout hash non-zero
        assert!(parse(&cb).is_err());
        let (cb, _) = build(1, b"x", &[TxOut { value: 1, script: vec![0x51] }], 0);
        assert!(parse(&cb[..cb.len() - 1]).is_err());
        let mut more = cb.clone();
        more.push(0);
        assert!(parse(&more).is_err());
    }

    #[test]
    fn bip34_small_heights() {
        assert_eq!(bip34_height(&[0x51]), Some(1));
        assert_eq!(bip34_height(&[0x03, 0x7b, 0xbe, 0x0e]), Some(966_267));
        assert_eq!(bip34_height(&[]), None);
    }
}
