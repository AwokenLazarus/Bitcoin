//! Coinbaser v2: the list of outputs Prime tells a gateway to put in its coinbase.
//!
//! ```text
//! coinbaser id (u8) | repeated: value sats (u64 LE) | script len (u8, 2..=64) | script
//! ```
//!
//! The gateway walks the list in order, keeps each output that still fits in the coinbase
//! it is building and whose value does not push the running total past the block's
//! coinbase value, and pays whatever is left to the pool address it was configured with.
//! The gateway accepts at most 512 entries and one script may be at most 64 bytes.

use crate::{Cursor, Error, Result};

pub const MAX_OUTPUTS: usize = 512;
pub const MAX_SCRIPT: usize = 64;
pub const MIN_SCRIPT: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub sats: u64,
    pub script: Vec<u8>,
}

impl Output {
    /// Bytes this output occupies inside a serialized coinbase transaction.
    pub fn serialized_len(&self) -> usize {
        8 + 1 + self.script.len()
    }
}

pub fn encode_v2(id: u8, outputs: &[Output]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + outputs.iter().map(|o| o.serialized_len()).sum::<usize>());
    v.push(id);
    for o in outputs.iter().take(MAX_OUTPUTS) {
        debug_assert!((MIN_SCRIPT..=MAX_SCRIPT).contains(&o.script.len()));
        v.extend_from_slice(&o.sats.to_le_bytes());
        v.push(o.script.len() as u8);
        v.extend_from_slice(&o.script);
    }
    v
}

pub fn decode_v2(bytes: &[u8]) -> Result<(u8, Vec<Output>)> {
    let mut c = Cursor::new(bytes);
    let id = c.u8()?;
    let mut out = Vec::new();
    while c.remaining() > 0 {
        let sats = c.u64()?;
        let n = c.u8()? as usize;
        if !(MIN_SCRIPT..=MAX_SCRIPT).contains(&n) {
            return Err(Error::Malformed("coinbaser script length"));
        }
        out.push(Output { sats, script: c.take(n)?.to_vec() });
        if out.len() > MAX_OUTPUTS {
            return Err(Error::Malformed("coinbaser output count"));
        }
    }
    Ok((id, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let outs = vec![
            Output { sats: 1_000, script: vec![0x00, 0x14, 1, 2, 3] },
            Output { sats: u64::MAX, script: vec![0x51; 64] },
        ];
        let enc = encode_v2(7, &outs);
        assert_eq!(enc[0], 7);
        assert_eq!(enc.len(), 1 + (9 + 5) + (9 + 64));
        assert_eq!(decode_v2(&enc).unwrap(), (7, outs));
        assert_eq!(decode_v2(&[3]).unwrap(), (3, vec![]));
    }

    #[test]
    fn rejects_bad_script_lengths() {
        let mut enc = encode_v2(1, &[Output { sats: 1, script: vec![0; 2] }]);
        enc[9] = 1;
        assert!(decode_v2(&enc).is_err());
        enc[9] = 65;
        assert!(decode_v2(&enc).is_err());
    }
}
