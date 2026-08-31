use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use lazarus_protocol::coinbaser::{CoinbaserOutput, CoinbaserV2};
use lazarus_protocol::identity_script;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Credit {
    ts: u64,
    identity: String,
    work: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Persist {
    credits: Vec<Credit>,
    carry: HashMap<String, u64>,
    shares: u64,
    accepted_work: u64,
}

#[derive(Default)]
pub struct Ledger {
    credits: Vec<Credit>,
    /// Unpaid effort from a block that used the wrong (unsplit) template.
    /// Not trimmed. Cleared only after a Lazarus block with 2+ value outputs.
    carry: HashMap<String, u64>,
    pub shares: u64,
    pub accepted_work: u64,
    seen: HashMap<[u8; 8], ()>,
}

impl Ledger {
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(p) = serde_json::from_str::<Persist>(&raw) else {
            log::warn!("ledger file unreadable; starting empty persist");
            return Self::default();
        };
        log::info!(
            "ledger loaded credits={} carry_ids={} shares={}",
            p.credits.len(),
            p.carry.len(),
            p.shares
        );
        Self {
            credits: p.credits,
            carry: p.carry,
            shares: p.shares,
            accepted_work: p.accepted_work,
            seen: HashMap::new(),
        }
    }

    pub fn save(&self, path: &Path) {
        let p = Persist {
            credits: self.credits.clone(),
            carry: self.carry.clone(),
            shares: self.shares,
            accepted_work: self.accepted_work,
        };
        if let Ok(raw) = serde_json::to_string(&p) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }

    pub fn merge_carry(&mut self, ident: String, work: u64) {
        if ident.is_empty() || work == 0 {
            return;
        }
        *self.carry.entry(ident).or_insert(0) = self.carry.get(&ident).copied().unwrap_or(0).saturating_add(work);
    }

    pub fn load_carry_file(&mut self, path: &Path) -> usize {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return 0;
        };
        let Ok(map) = serde_json::from_str::<HashMap<String, u64>>(&raw) else {
            return 0;
        };
        let n = map.len();
        for (ident, work) in map {
            self.merge_carry(ident, work);
        }
        n
    }

    pub fn clear_carry(&mut self) {
        self.carry.clear();
    }

    pub fn carry_len(&self) -> usize {
        self.carry.len()
    }

    pub fn credit(&mut self, identity: String, work: u64, nonce: u32, job: u8) {
        let mut k = [0u8; 8];
        k[..4].copy_from_slice(&nonce.to_le_bytes());
        k[4] = job;
        if self.seen.contains_key(&k) {
            return;
        }
        self.seen.insert(k, ());
        if self.seen.len() > 100_000 {
            self.seen.clear();
        }
        self.credits.push(Credit {
            ts: Self::now(),
            identity,
            work,
        });
        self.shares += 1;
        self.accepted_work += work;
    }

    pub fn trim(&mut self, target_work: u64) {
        if target_work == 0 {
            return;
        }
        let mut total: u64 = self.credits.iter().map(|c| c.work).sum();
        while total > target_work && !self.credits.is_empty() {
            let drop = self.credits.remove(0);
            total = total.saturating_sub(drop.work);
        }
    }

    pub fn window_work(&self) -> u64 {
        self.credits.iter().map(|c| c.work).sum::<u64>() + self.carry.values().sum::<u64>()
    }

    pub fn tides_work(&self) -> u64 {
        self.credits.iter().map(|c| c.work).sum()
    }

    pub fn by_identity(&self) -> HashMap<String, u64> {
        let mut m = HashMap::new();
        for c in &self.credits {
            *m.entry(c.identity.clone()).or_insert(0) += c.work;
        }
        for (ident, work) in &self.carry {
            *m.entry(ident.clone()).or_insert(0) += *work;
        }
        m
    }

    /// Window + unpaid carry split the block; fee_bps=0 pays 100% (dust/unpayable remainder to pool script).
    pub fn coinbaser(
        &self,
        value: u64,
        fee_bps: u64,
        min_payout: u64,
        pool_script: &[u8],
        id: u8,
    ) -> CoinbaserV2 {
        let fee = value.saturating_mul(fee_bps) / 10_000;
        let miners = value.saturating_sub(fee);
        let total = self.window_work().max(1);
        let mut outputs = Vec::new();
        let mut paid = 0u64;
        let mut rows: Vec<(String, u64)> = self.by_identity().into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (ident, work) in rows {
            let Some(script) = identity_script(&ident) else {
                continue;
            };
            let sats = miners.saturating_mul(work) / total;
            if sats < min_payout {
                continue;
            }
            paid = paid.saturating_add(sats);
            outputs.push(CoinbaserOutput { sats, script });
            if outputs.len() >= 32 {
                break;
            }
        }
        let rest = value.saturating_sub(paid);
        if rest > 0 || outputs.is_empty() {
            outputs.push(CoinbaserOutput {
                sats: rest,
                script: pool_script.to_vec(),
            });
        }
        CoinbaserV2 { id, outputs }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazarus_protocol::identity_script;

    fn pool() -> Vec<u8> {
        identity_script("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4").unwrap()
    }

    #[test]
    fn split_uses_carry_and_live() {
        let mut led = Ledger::default();
        led.merge_carry("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".into(), 75);
        led.credit("bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3".into(), 25, 1, 1);
        let blob = led.coinbaser(1_000_000, 0, 546, &pool(), 1);
        assert!(blob.outputs.len() >= 2);
        let miner_sats: u64 = blob.outputs.iter().map(|o| o.sats).sum();
        assert_eq!(miner_sats, 1_000_000);
    }

    #[test]
    fn trim_does_not_drop_carry() {
        let mut led = Ledger::default();
        led.merge_carry("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".into(), 9_000);
        led.credit("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4".into(), 100, 2, 1);
        led.trim(50);
        assert_eq!(led.tides_work(), 0);
        assert_eq!(led.window_work(), 9_000);
        assert_eq!(led.carry_len(), 1);
    }
}
