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
}
