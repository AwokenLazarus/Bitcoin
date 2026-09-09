//! Solo-block observer: the DATUM rebate's second source.
//!
//! The dedicated solo gateways (`lazarus-gateway --mode solo`, `prime_port 0`) never talk to
//! Prime; their blocks pay the finder and a fee output to the pool script, stamped with
//! `solo-coinbase-tag`. When `solo-rebate-bps` is set, that share of each such block's
//! reward is credited to the DATUM miners in the window at that moment, pro rata by payable
//! DATUM work, as carry — the same way the split credits the stratum rebate (see
//! `tides::split`). It is then paid out of the pool's remainder in following coinbases. If
//! no DATUM miner can take it, it waits in `rebate_owed` for the next block that has one.
//!
//! Scans the chain one block behind the tip, so a block is only booked once something has
//! been built on it, and never backfills: a fresh data dir starts at the current tip. Each
//! booked block is appended to `solo-rebates.jsonl`; the scan cursor lives in
//! `solo-scan.json`. Blocks Prime already recorded (found through a Prime gateway) are not
//! solo blocks in this sense and are skipped.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::{now, Shared};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cursor {
    /// Last height fully scanned.
    height: u32,
}

/// One solo block whose rebate share was booked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoloRebate {
    pub ts: u64,
    pub height: u32,
    pub hash: String,
    /// Sum of the coinbase outputs, sats.
    pub coinbase_value: u64,
    /// What the coinbase paid the pool script (the solo fee), sats.
    pub pool_sats: u64,
    /// `coinbase_value × solo_rebate_bps / 10 000`.
    pub rebate_sats: u64,
    /// Credited to DATUM identities' carry, identity → sats. Empty when nobody could take it
    /// (then `rebate_sats` went to `rebate_owed` instead).
    #[serde(default)]
    pub credits: Vec<(String, u64)>,
    /// `rebate_owed` after this block.
    pub rebate_owed_after: u64,
}

fn cursor_path(dir: &Path) -> PathBuf {
    dir.join("solo-scan.json")
}

fn log_path(dir: &Path) -> PathBuf {
    dir.join("solo-rebates.jsonl")
}

fn read_cursor(dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(cursor_path(dir)).ok()?;
    serde_json::from_str::<Cursor>(&text).ok().map(|c| c.height)
}

fn write_cursor(dir: &Path, height: u32) {
    let tmp = cursor_path(dir).with_extension("tmp");
    let body = serde_json::to_vec(&Cursor { height }).unwrap_or_default();
    if std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, cursor_path(dir))).is_err() {
        log::warn!("solo-scan.json write failed");
    }
}

/// What a coinbase tells us, decoded from `getrawtransaction … true`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CoinbaseFacts {
    pub tagged: bool,
    pub value_sats: u64,
    pub pool_sats: u64,
}

fn btc_to_sats(v: &Value) -> u64 {
    // Knots reports BTC as a JSON number; round to the sat to undo float formatting
    (v.as_f64().unwrap_or(0.0) * 1e8).round().max(0.0) as u64
}

/// Read tag, total value and the pool's cut out of a decoded coinbase transaction.
pub fn coinbase_facts(tx: &Value, tag: &str, pool_script_hex: &str) -> CoinbaseFacts {
    let script_sig = tx
        .get("vin")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.get("coinbase"))
        .and_then(|v| v.as_str())
        .and_then(|h| hex::decode(h).ok())
        .unwrap_or_default();
    let tagged = !tag.is_empty() && script_sig.windows(tag.len()).any(|w| w == tag.as_bytes());
    let mut value_sats = 0u64;
    let mut pool_sats = 0u64;
    if let Some(vouts) = tx.get("vout").and_then(|v| v.as_array()) {
        for o in vouts {
            let sats = o.get("value").map(btc_to_sats).unwrap_or(0);
            value_sats = value_sats.saturating_add(sats);
            let spk = o.get("scriptPubKey").and_then(|s| s.get("hex")).and_then(|h| h.as_str()).unwrap_or("");
            if spk.eq_ignore_ascii_case(pool_script_hex) {
                pool_sats = pool_sats.saturating_add(sats);
            }
        }
    }
    CoinbaseFacts { tagged, value_sats, pool_sats }
}

/// The rebate a solo block of `value_sats` books.
pub fn rebate_for(value_sats: u64, bps: u32) -> u64 {
    ((u128::from(value_sats) * u128::from(bps)) / 10_000) as u64
}

/// Called by the node poller when the tip moves to `tip_height`. Scans up to `tip_height − 1`.
pub async fn scan(shared: &Shared, tip_height: u32) {
    let bps = shared.cfg.solo_rebate_bps;
    if bps == 0 || tip_height < 2 {
        return;
    }
    let dir = shared.cfg.data_dir.as_path();
    let upto = tip_height - 1;
    let start = match read_cursor(dir) {
        Some(h) => h + 1,
        None => {
            // first run: begin at the tip, never backfill history
            write_cursor(dir, upto);
            return;
        }
    };
    if start > upto {
        return;
    }
    // bounded per pass so a long outage catches up over a few polls without hammering the node
    let end = upto.min(start + 19);
    let pool_hex = hex::encode(&shared.pool_script);
    for height in start..=end {
        let hash = match shared.rpc.call("getblockhash", json!([height])).await {
            Ok(Value::String(h)) => h,
            Ok(other) => {
                log::warn!("solo scan: getblockhash {height} returned {other}");
                return;
            }
            Err(e) => {
                log::debug!("solo scan: getblockhash {height}: {e}");
                return;
            }
        };
        let known = shared.blocks.lock().unwrap().iter().any(|b| b.hash == hash);
        if !known {
            let block = match shared.rpc.call("getblock", json!([hash, 1])).await {
                Ok(b) => b,
                Err(e) => {
                    log::debug!("solo scan: getblock {hash}: {e}");
                    return;
                }
            };
            let Some(txid) =
                block.get("tx").and_then(|t| t.as_array()).and_then(|a| a.first()).and_then(|t| t.as_str())
            else {
                log::warn!("solo scan: block {hash} has no coinbase txid");
                return;
            };
            let tx = match shared.rpc.call("getrawtransaction", json!([txid, true, hash])).await {
                Ok(t) => t,
                Err(e) => {
                    log::debug!("solo scan: getrawtransaction {txid}: {e}");
                    return;
                }
            };
            let facts = coinbase_facts(&tx, &shared.cfg.solo_coinbase_tag, &pool_hex);
            if facts.tagged && facts.pool_sats > 0 {
                let rebate = rebate_for(facts.value_sats, bps);
                let net = shared.network;
                let (credits, after) = {
                    let mut ledger = shared.ledger.lock().unwrap();
                    // whatever was waiting for a DATUM miner goes out with this one
                    let pot = rebate.saturating_add(ledger.window.rebate_owed());
                    let (credits, _dust) = tides::split::rebate_credits(&ledger.window.miners(), pot, |i| {
                        crate::address::to_script(i, net)
                    });
                    let after = if credits.is_empty() {
                        ledger.settle_rebate(rebate.min(i64::MAX as u64) as i64)
                    } else {
                        let delta: Vec<(String, i64)> = credits
                            .iter()
                            .map(|(i, s)| (i.clone(), s.min(&(i64::MAX as u64)).to_owned() as i64))
                            .collect();
                        ledger.settle_carry(&delta);
                        ledger.set_rebate_owed(0);
                        0
                    };
                    (credits, after)
                };
                let rec = SoloRebate {
                    ts: now(),
                    height,
                    hash: hash.clone(),
                    coinbase_value: facts.value_sats,
                    pool_sats: facts.pool_sats,
                    rebate_sats: rebate,
                    credits: credits.clone(),
                    rebate_owed_after: after,
                };
                log::info!(
                    "solo block {hash} at {height}: value={} pool={} -> DATUM rebate {rebate} sats credited to {} miners' carry, owed now {after}",
                    facts.value_sats,
                    facts.pool_sats,
                    credits.len()
                );
                if let Ok(mut line) = serde_json::to_string(&rec) {
                    line.push('\n');
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log_path(dir))
                        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
                }
            } else if facts.tagged {
                log::info!("solo block {hash} at {height} pays no fee to the pool script; no rebate booked");
            }
        }
        write_cursor(dir, height);
    }
}

/// Every booked solo rebate, oldest first.
pub fn read_log(dir: &Path) -> Vec<SoloRebate> {
    std::fs::read_to_string(log_path(dir))
        .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cb(tag: &str, outs: &[(f64, &str)]) -> Value {
        let mut sig = vec![0x03, 0x40, 0xe1, 0x0e];
        sig.extend_from_slice(tag.as_bytes());
        json!({
            "vin": [{ "coinbase": hex::encode(sig) }],
            "vout": outs.iter().map(|(v, spk)| json!({ "value": v, "scriptPubKey": { "hex": spk } })).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn a_solo_coinbase_is_recognised_by_tag_and_pool_output() {
        let pool = "0014aabbccddeeff00112233445566778899aabbccdd";
        let f = coinbase_facts(&cb("Lazarus/solo", &[(3.03125, "0014ff"), (0.09375, pool)]), "Lazarus/solo", pool);
        assert_eq!(f, CoinbaseFacts { tagged: true, value_sats: 312_500_000, pool_sats: 9_375_000 });
        // 1% of a 3.125 BTC block
        assert_eq!(rebate_for(f.value_sats, 100), 3_125_000);
        // the pooled tag is not the solo tag
        let f = coinbase_facts(&cb("Lazarus", &[(3.125, pool)]), "Lazarus/solo", pool);
        assert!(!f.tagged);
        // a solo block with no fee output to us books nothing
        let f = coinbase_facts(&cb("Lazarus/solo", &[(3.125, "0014ff")]), "Lazarus/solo", pool);
        assert_eq!((f.tagged, f.pool_sats), (true, 0));
        // upper-case hex from a different node build still matches
        let f = coinbase_facts(&cb("Lazarus/solo", &[(0.1, &pool.to_uppercase())]), "Lazarus/solo", pool);
        assert_eq!(f.pool_sats, 10_000_000);
    }

    #[test]
    fn btc_amounts_round_to_the_sat() {
        assert_eq!(btc_to_sats(&json!(3.12499999)), 312_499_999);
        assert_eq!(btc_to_sats(&json!(0.00000001)), 1);
        assert_eq!(btc_to_sats(&json!(3.125)), 312_500_000);
    }

    #[test]
    fn cursor_and_log_round_trip() {
        let dir = std::env::temp_dir().join(format!("primed-solo-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_cursor(&dir), None);
        write_cursor(&dir, 970_000);
        assert_eq!(read_cursor(&dir), Some(970_000));
        assert!(read_log(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
