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
