use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lazarus_protocol::coinbaser::{CoinbaserOutput, CoinbaserV2};
use lazarus_protocol::identity_script;
use serde::{Deserialize, Serialize};

/// Credits landing in the same bucket for the same identity are summed into one row, so a
/// busy window is a few thousand rows instead of one row per share. Payout shares are
/// unaffected: bucketing changes only the granularity at which old work leaves the window.
const BUCKET_SECS: u64 = 60;
/// Dedupe keys kept in memory before the set is dropped and rebuilt.
const SEEN_CAP: usize = 400_000;

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
    /// Running sum of `credits`, so trimming and reporting never walk the window.
    total: u64,
    seen: HashMap<[u8; 16], ()>,
    last_save: Option<Instant>,
    dirty: bool,
}

impl Ledger {
    pub fn now() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn bucket_of(ts: u64) -> u64 {
        ts - (ts % BUCKET_SECS)
    }

    /// Merge rows sharing a bucket and identity. Totals and per-identity sums are preserved.
    fn compacted(credits: Vec<Credit>) -> Vec<Credit> {
        let mut out: Vec<Credit> = Vec::new();
        for c in credits {
            let bucket = Self::bucket_of(c.ts);
            let mut merged = false;
            for o in out.iter_mut().rev() {
                if o.ts < bucket {
                    break;
                }
                if o.identity == c.identity {
                    o.work = o.work.saturating_add(c.work);
                    merged = true;
                    break;
                }
            }
            if !merged {
                out.push(Credit { ts: bucket, identity: c.identity, work: c.work });
            }
        }
        out
    }

    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(p) = serde_json::from_str::<Persist>(&raw) else {
            log::warn!("ledger file unreadable; starting empty persist");
            return Self::default();
        };
        let before = p.credits.len();
        let credits = Self::compacted(p.credits);
        let total = credits.iter().map(|c| c.work).sum();
        log::info!(
            "ledger loaded credits={} (compacted from {}) carry_ids={} shares={} window_work={}",
            credits.len(),
            before,
            p.carry.len(),
            p.shares,
            total
        );
        Self {
            credits,
            carry: p.carry,
            shares: p.shares,
            accepted_work: p.accepted_work,
            total,
            seen: HashMap::new(),
            last_save: None,
            dirty: before != 0,
        }
    }

    pub fn save(&mut self, path: &Path) {
        let p = Persist {
            credits: self.credits.clone(),
            carry: self.carry.clone(),
            shares: self.shares,
            accepted_work: self.accepted_work,
        };
        if let Ok(raw) = serde_json::to_string(&p) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                self.last_save = Some(Instant::now());
                self.dirty = false;
            }
        }
    }

    /// Write at most once per `min_interval`, and only if something changed. Share crediting
    /// is on the hot path; rewriting the whole window per share is not.
    pub fn save_if_due(&mut self, path: &Path, min_interval: Duration) {
        if !self.dirty {
            return;
        }
        let due = self.last_save.map(|t| t.elapsed() >= min_interval).unwrap_or(true);
        if due {
            self.save(path);
        }
    }

    pub fn merge_carry(&mut self, ident: String, work: u64) {
        if ident.is_empty() || work == 0 {
            return;
        }
        let cur = self.carry.get(&ident).copied().unwrap_or(0);
        self.carry.insert(ident, cur.saturating_add(work));
        self.dirty = true;
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
        self.dirty = true;
    }

    pub fn carry_len(&self) -> usize {
        self.carry.len()
    }

    pub fn credits_len(&self) -> usize {
        self.credits.len()
    }

    /// Credit work to an identity. `key` must be unique per submission; a repeat is ignored
    /// and reported as `false` so the caller can reject it.
    pub fn credit(&mut self, identity: String, work: u64, key: [u8; 16]) -> bool {
        if self.seen.contains_key(&key) {
            return false;
        }
        if self.seen.len() >= SEEN_CAP {
            self.seen.clear();
        }
        self.seen.insert(key, ());

        let bucket = Self::bucket_of(Self::now());
        let mut merged = false;
        for c in self.credits.iter_mut().rev() {
            if c.ts < bucket {
                break;
            }
            if c.identity == identity {
                c.work = c.work.saturating_add(work);
                merged = true;
                break;
            }
        }
        if !merged {
            self.credits.push(Credit { ts: bucket, identity, work });
        }
        self.total = self.total.saturating_add(work);
        self.shares += 1;
        self.accepted_work = self.accepted_work.saturating_add(work);
        self.dirty = true;
        true
    }

    pub fn trim(&mut self, target_work: u64) {
        if target_work == 0 {
            return;
        }
        let mut n = 0usize;
        let mut total = self.total;
        while total > target_work && n < self.credits.len() {
            total = total.saturating_sub(self.credits[n].work);
            n += 1;
        }
        if n > 0 {
            self.credits.drain(..n);
            self.total = total;
            self.dirty = true;
        }
    }

    pub fn window_work(&self) -> u64 {
        self.total + self.carry.values().sum::<u64>()
    }

    pub fn tides_work(&self) -> u64 {
        self.total
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

    const A: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    const B: &str = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";

    fn pool() -> Vec<u8> {
        identity_script(A).unwrap()
    }

    fn key(n: u64) -> [u8; 16] {
        let mut k = [0u8; 16];
        k[..8].copy_from_slice(&n.to_le_bytes());
        k
    }

    #[test]
    fn split_uses_carry_and_live() {
        let mut led = Ledger::default();
        led.merge_carry(A.into(), 75);
        assert!(led.credit(B.into(), 25, key(1)));
        let blob = led.coinbaser(1_000_000, 0, 546, &pool(), 1);
        assert!(blob.outputs.len() >= 2);
        let miner_sats: u64 = blob.outputs.iter().map(|o| o.sats).sum();
        assert_eq!(miner_sats, 1_000_000);
    }

    #[test]
    fn trim_does_not_drop_carry() {
        let mut led = Ledger::default();
        led.merge_carry(A.into(), 9_000);
        led.credit(A.into(), 100, key(2));
        led.trim(50);
        assert_eq!(led.tides_work(), 0);
        assert_eq!(led.window_work(), 9_000);
        assert_eq!(led.carry_len(), 1);
    }

    #[test]
    fn repeat_submissions_are_not_paid_twice() {
        let mut led = Ledger::default();
        assert!(led.credit(A.into(), 32, key(7)));
        assert!(!led.credit(A.into(), 32, key(7)));
        assert_eq!(led.tides_work(), 32);
        assert_eq!(led.shares, 1);
    }

    #[test]
    fn many_shares_collapse_into_bucket_rows() {
        let mut led = Ledger::default();
        for i in 0..5_000u64 {
            led.credit(if i % 2 == 0 { A.into() } else { B.into() }, 1, key(i));
        }
        assert_eq!(led.tides_work(), 5_000);
        // two identities in the current bucket, so at most two rows per bucket crossed
        assert!(led.credits_len() <= 4, "rows={}", led.credits_len());
        let by = led.by_identity();
        assert_eq!(by[A], 2_500);
        assert_eq!(by[B], 2_500);
    }

    #[test]
    fn trim_keeps_the_running_total_honest() {
        let mut led = Ledger::default();
        for i in 0..100u64 {
            led.credit(A.into(), 10, key(i));
        }
        assert_eq!(led.tides_work(), 1_000);
        led.trim(400);
        let walked: u64 = led.credits.iter().map(|c| c.work).sum();
        assert_eq!(led.tides_work(), walked);
    }

    #[test]
    fn compaction_preserves_totals() {
        // first three share the 960 bucket; the last one is far enough out to start its own
        let raw = vec![
            Credit { ts: 960, identity: A.into(), work: 5 },
            Credit { ts: 970, identity: B.into(), work: 7 },
            Credit { ts: 980, identity: A.into(), work: 3 },
            Credit { ts: 5_000, identity: A.into(), work: 11 },
        ];
        let before: u64 = raw.iter().map(|c| c.work).sum();
        let out = Ledger::compacted(raw);
        assert_eq!(out.iter().map(|c| c.work).sum::<u64>(), before);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].ts, 960);
        assert_eq!(out[0].work, 8);
        assert_eq!(out[1].work, 7);
        assert_eq!(out[2].ts, 4_980);
    }
}
