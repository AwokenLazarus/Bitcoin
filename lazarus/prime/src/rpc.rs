use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Default)]
pub struct ChainTip {
    pub height: u64,
    pub hash: String,
    pub difficulty: f64,
    pub chain: String,
}

#[derive(Clone, Debug)]
pub struct CoinbaseInfo {
    pub is_ours: bool,
    pub value_outputs: u32,
    /// The coinbase's value outputs as `(scriptPubKey hex, sats)`, so a confirmed block can be
    /// matched to the exact split we issued and a make-good settled by the real amount paid.
    pub outputs: Vec<(String, u64)>,
}

pub fn cookie_basic(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let pair = raw.trim();
    if pair.is_empty() {
        return None;
    }
    Some(format!(
        "Basic {}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pair.as_bytes())
    ))
}

pub fn call(url: &str, auth: &str, method: &str, params: Value) -> Option<Value> {
    let body = json!({"jsonrpc":"1.0","id":"lz","method":method,"params":params});
    let resp = minreq::post(url)
        .with_header("Authorization", auth)
        .with_header("Content-Type", "application/json")
        .with_body(body.to_string())
        .send()
        .ok()?;
    let v: Value = serde_json::from_str(resp.as_str().ok()?).ok()?;
    v.get("result").cloned()
}

pub fn tip(url: &str, auth: &str) -> Option<ChainTip> {
    let info = call(url, auth, "getblockchaininfo", json!([]))?;
    Some(ChainTip {
        height: info.get("blocks").and_then(|x| x.as_u64()).unwrap_or(0),
        hash: info.get("bestblockhash").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        difficulty: info.get("difficulty").and_then(|x| x.as_f64()).unwrap_or(0.0),
        chain: info.get("chain").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

/// Count value outputs on the coinbase; `is_ours` if the scriptsig ascii contains `tag`.
pub fn coinbase_info(url: &str, auth: &str, blockhash: &str, tag: &str) -> Option<CoinbaseInfo> {
    let blk = call(url, auth, "getblock", json!([blockhash, 2]))?;
    let tx0 = blk.get("tx")?.as_array()?.first()?;
    let vin = tx0.get("vin")?.as_array()?.first()?;
    let hex = vin.get("coinbase")?.as_str().unwrap_or("");
    let raw = hex::decode(hex).ok()?;
    let text: String = raw
        .iter()
        .map(|b| if (32..127).contains(b) { *b as char } else { '.' })
        .collect();
    let is_ours = !tag.is_empty() && text.contains(tag);
    let mut value_outputs = 0u32;
    let mut outputs: Vec<(String, u64)> = Vec::new();
    if let Some(vouts) = tx0.get("vout").and_then(|v| v.as_array()) {
        for o in vouts {
            let v = o.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0);
            if v > 0.0 {
                value_outputs += 1;
                let sats = (v * 1e8).round() as u64;
                let spk = o
                    .get("scriptPubKey")
                    .and_then(|s| s.get("hex"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                outputs.push((spk, sats));
            }
        }
    }
    Some(CoinbaseInfo {
        is_ours,
        value_outputs,
        outputs,
    })
}
