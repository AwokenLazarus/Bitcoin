/// Coinbaser v2: uint8 id, then repeating (uint64le sats + uint8 script_len + script).
#[derive(Clone, Debug)]
pub struct CoinbaserOutput {
    pub sats: u64,
    pub script: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct CoinbaserV2 {
    pub id: u8,
    pub outputs: Vec<CoinbaserOutput>,
}

impl CoinbaserV2 {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![self.id];
        for out in &self.outputs {
            o.extend_from_slice(&out.sats.to_le_bytes());
            o.push(out.script.len() as u8);
            o.extend_from_slice(&out.script);
        }
        o
    }

    pub fn value_sum(&self) -> u64 {
        self.outputs.iter().map(|o| o.sats).sum()
    }

    /// Re-apply the same payout ratios to a new template `coinbasevalue`.
    /// The last output absorbs rounding so the sum matches `new_value`.
    pub fn scale_to(&self, new_value: u64) -> CoinbaserV2 {
        let old = self.value_sum();
        if old == 0 || old == new_value || self.outputs.is_empty() {
            return self.clone();
        }
        let mut outputs = self.outputs.clone();
        let n = outputs.len();
        let mut paid = 0u64;
        for o in outputs.iter_mut().take(n.saturating_sub(1)) {
            o.sats = ((o.sats as u128) * (new_value as u128) / (old as u128)) as u64;
            paid = paid.saturating_add(o.sats);
        }
        if let Some(last) = outputs.last_mut() {
            last.sats = new_value.saturating_sub(paid);
        }
        CoinbaserV2 { id: self.id, outputs }
    }
}

pub fn parse_coinbaser_v2(buf: &[u8]) -> Option<CoinbaserV2> {
    if buf.is_empty() {
        return None;
    }
    let id = buf[0];
    let mut i = 1;
    let mut outputs = Vec::new();
    while i + 9 <= buf.len() {
        let sats = u64::from_le_bytes(buf[i..i + 8].try_into().ok()?);
        i += 8;
        let sl = buf[i] as usize;
        i += 1;
        if sl < 2 || sl > 64 || i + sl > buf.len() {
            return None;
        }
        outputs.push(CoinbaserOutput {
            sats,
            script: buf[i..i + sl].to_vec(),
        });
        i += sl;
        if outputs.len() >= 512 {
            break;
        }
    }
    Some(CoinbaserV2 { id, outputs })
}

/// Outputs plus the non-witness serialization of a coinbase transaction.
#[derive(Clone, Debug)]
pub struct ParsedCoinbase {
    pub outputs: Vec<CoinbaserOutput>,
    /// Witness marker, flag and witness data removed: the txid preimage.
    pub legacy: Vec<u8>,
    /// BIP34 height from the scriptsig push, when it looks like one.
    pub height: Option<u32>,
}

fn read_varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let first = *b.get(*i)?;
    *i += 1;
    let v = match first {
        0xfd => {
            let x = u16::from_le_bytes(b.get(*i..*i + 2)?.try_into().ok()?) as u64;
            *i += 2;
            x
        }
        0xfe => {
            let x = u32::from_le_bytes(b.get(*i..*i + 4)?.try_into().ok()?) as u64;
            *i += 4;
            x
        }
        0xff => {
            let x = u64::from_le_bytes(b.get(*i..*i + 8)?.try_into().ok()?);
            *i += 8;
            x
        }
        n => n as u64,
    };
    Some(v)
}

fn write_varint(o: &mut Vec<u8>, n: u64) {
    if n < 0xfd {
        o.push(n as u8);
    } else if n <= 0xffff {
        o.push(0xfd);
        o.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xffff_ffff {
        o.push(0xfe);
        o.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        o.push(0xff);
        o.extend_from_slice(&n.to_le_bytes());
    }
}

fn bip34_height(ss: &[u8]) -> Option<u32> {
    let n = *ss.first()? as usize;
    if n == 0 || n > 4 || ss.len() < 1 + n {
        return None;
    }
    let mut h = 0u32;
    for (k, b) in ss[1..1 + n].iter().enumerate() {
        h |= u32::from(*b) << (8 * k);
    }
    Some(h)
}

/// Parse a coinbase in either serialization. Rebuilds the legacy bytes so the caller
/// can take the txid without trusting the sender's framing.
pub fn parse_coinbase(tx: &[u8]) -> Option<ParsedCoinbase> {
    let version = tx.get(0..4)?.to_vec();
    let segwit = tx.get(4) == Some(&0x00) && tx.get(5) == Some(&0x01);
    let mut i = if segwit { 6 } else { 4 };
    let n_in = read_varint(tx, &mut i)?;
    if n_in == 0 || n_in > 8 {
        return None;
    }
    let ins_start = i;
    let mut scriptsig = Vec::new();
    for k in 0..n_in {
        i = i.checked_add(36)?;
        if i > tx.len() {
            return None;
        }
        let sl = read_varint(tx, &mut i)? as usize;
        let s = tx.get(i..i.checked_add(sl)?)?;
        if k == 0 {
            scriptsig = s.to_vec();
        }
        i += sl;
        i = i.checked_add(4)?;
        if i > tx.len() {
            return None;
        }
    }
    let ins_end = i;
    let n_out = read_varint(tx, &mut i)?;
    if n_out == 0 || n_out > 512 {
        return None;
    }
    let outs_start = i;
    let mut outputs = Vec::with_capacity(n_out as usize);
    for _ in 0..n_out {
        let sats = u64::from_le_bytes(tx.get(i..i + 8)?.try_into().ok()?);
        i += 8;
        let sl = read_varint(tx, &mut i)? as usize;
        let script = tx.get(i..i.checked_add(sl)?)?.to_vec();
        i += sl;
        outputs.push(CoinbaserOutput { sats, script });
    }
    let outs_end = i;
    if segwit {
        for _ in 0..n_in {
            let items = read_varint(tx, &mut i)?;
            if items > 16 {
                return None;
            }
            for _ in 0..items {
                let l = read_varint(tx, &mut i)? as usize;
                i = i.checked_add(l)?;
                if i > tx.len() {
                    return None;
                }
            }
        }
    }
    let locktime = tx.get(i..i + 4)?.to_vec();
    let mut legacy = Vec::with_capacity(tx.len());
    legacy.extend_from_slice(&version);
    write_varint(&mut legacy, n_in);
    legacy.extend_from_slice(tx.get(ins_start..ins_end)?);
    write_varint(&mut legacy, n_out);
    legacy.extend_from_slice(tx.get(outs_start..outs_end)?);
    legacy.extend_from_slice(&locktime);
    Some(ParsedCoinbase {
        outputs,
        legacy,
        height: bip34_height(&scriptsig),
    })
}

/// Does this coinbase honour the split we issued? Each issued script must be paid at least
/// its proportional share of whatever the coinbase actually pays, so a gateway may rescale
/// for a new template value but cannot redirect anyone's payout to itself.
pub fn split_satisfied(
    issued: &CoinbaserV2,
    outputs: &[CoinbaserOutput],
    tolerance_sats: u64,
) -> bool {
    let issued_total = issued.value_sum();
    if issued_total == 0 || issued.outputs.is_empty() {
        return false;
    }
    let actual_total: u64 = outputs.iter().map(|o| o.sats).sum();
    if actual_total == 0 {
        return false;
    }
    let mut want: Vec<(&[u8], u64)> = Vec::new();
    for o in &issued.outputs {
        match want.iter_mut().find(|(s, _)| *s == o.script.as_slice()) {
            Some((_, v)) => *v = v.saturating_add(o.sats),
            None => want.push((o.script.as_slice(), o.sats)),
        }
    }
    for (script, sats) in &want {
        let scaled = ((*sats as u128) * (actual_total as u128) / (issued_total as u128)) as u64;
        let need = scaled.saturating_sub(tolerance_sats);
        let got: u64 = outputs
            .iter()
            .filter(|o| o.script.as_slice() == *script)
            .map(|o| o.sats)
            .sum();
        if got < need {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cb(parts: &[u64]) -> CoinbaserV2 {
        CoinbaserV2 {
            id: 7,
            outputs: parts
                .iter()
                .map(|s| CoinbaserOutput {
                    sats: *s,
                    script: vec![0x00, 0x14, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19],
                })
                .collect(),
        }
    }

    /// Distinct script per output, so aggregation by script does not mask a redirect.
    fn cb_distinct(parts: &[u64]) -> CoinbaserV2 {
        CoinbaserV2 {
            id: 7,
            outputs: parts
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut script = vec![0x00, 0x14];
                    script.extend_from_slice(&[0x40 + i as u8; 20]);
                    CoinbaserOutput { sats: *s, script }
                })
                .collect(),
        }
    }

    #[test]
    fn scale_preserves_ratios_and_sum() {
        let src = cb(&[750_000, 250_000]);
        let out = src.scale_to(1_000_004);
        assert_eq!(out.id, 7);
        assert_eq!(out.value_sum(), 1_000_004);
        assert_eq!(out.outputs[0].sats, 750_003);
        assert_eq!(out.outputs[1].sats, 250_001);
    }

    #[test]
    fn scale_same_value_is_identity() {
        let src = cb(&[100, 200, 300]);
        let out = src.scale_to(600);
        assert_eq!(out.outputs.iter().map(|o| o.sats).collect::<Vec<_>>(), vec![100, 200, 300]);
    }

    #[test]
    fn split_satisfied_accepts_exact_and_scaled() {
        let issued = cb_distinct(&[750_000, 250_000]);
        let outs = issued.outputs.clone();
        assert!(split_satisfied(&issued, &outs, 2));
        let scaled = issued.scale_to(2_000_000).outputs;
        assert!(split_satisfied(&issued, &scaled, 2));
    }

    #[test]
    fn split_satisfied_rejects_theft() {
        let issued = cb_distinct(&[750_000, 250_000]);
        // pay the second script far less than its share
        let mut robbed = issued.outputs.clone();
        robbed[1].sats = 1;
        assert!(!split_satisfied(&issued, &robbed, 2));
        // drop an output entirely
        let dropped = vec![issued.outputs[0].clone()];
        assert!(!split_satisfied(&issued, &dropped, 2));
        // redirect everything to a script we never issued
        let mut hijack = issued.outputs.clone();
        hijack[0].script = vec![0x51; 22];
        hijack[1].script = vec![0x51; 22];
        assert!(!split_satisfied(&issued, &hijack, 2));
    }

    #[test]
    fn parse_coinbase_rejects_garbage() {
        assert!(parse_coinbase(&[]).is_none());
        assert!(parse_coinbase(&[1, 0, 0, 0, 1]).is_none());
    }
}
