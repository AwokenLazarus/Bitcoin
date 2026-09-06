//! State shared by every session, the node poller, and the stats server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use datum_wire::crypto::Identity;
use datum_wire::pow::Hash;
use tides::{BlockLog, BlockRecord, Ledger, SplitParams};
use tokio::sync::{broadcast, watch};

use crate::address::Network;
use crate::config::Config;
use crate::rpc::Rpc;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    pub height: u32,
    pub hash: String,
    pub difficulty: f64,
    /// When this Prime first saw the tip; shares for the previous height are accepted for
    /// a grace period after it.
    pub seen_at: Instant,
    pub seen_ts: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ClientInfo {
    pub id: u64,
    pub remote: String,
    pub user_agent: String,
    pub generation: &'static str,
    /// Hex prefix of the gateway's long-term signing key.
    pub gateway: String,
    /// `stratum` = house public gateway (higher fee); `datum` = external Prime client.
    pub fee_path: String,
    pub connected_ts: u64,
    pub identity: String,
    pub accepted: u64,
    pub rejected: u64,
    pub work: u64,
    pub last_share_ts: u64,
    pub coinbasers: u64,
    pub block_candidates: u64,
    pub last_reject: Option<&'static str>,
}

#[derive(Default)]
pub struct Totals {
    pub connections: AtomicU64,
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
    pub work: AtomicU64,
    pub coinbasers: AtomicU64,
    pub block_candidates: AtomicU64,
    pub blocks_submitted: AtomicU64,
    pub handshake_failures: AtomicU64,
    /// Accepts turned away at the connection limits.
    pub connections_refused: AtomicU64,
}

impl Totals {
    pub fn add(&self, c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

/// Every share hash the pool has credited, by block height, across all sessions.
///
/// The hash commits to prev/merkle/nbits/txcount/version and the miner's nonces, so it is
/// unique per (height, work) and a set keyed by height is a complete dedup. It lives here
/// rather than on a session so that neither reconnecting, nor re-sending a job section that
/// differs in a byte nobody reads, nor filling a per-job set can empty it: a share is
/// credited once, ever. Heights below the stale window are pruned by housekeeping, so the
/// set is bounded by real hashrate over two or three blocks — and by a hard cap, at which
/// point new work is refused rather than old work forgotten.
#[derive(Debug, Default)]
pub struct SeenShares {
    by_height: BTreeMap<u32, HashSet<Hash>>,
    total: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// New work; it has been recorded.
    Fresh,
    /// This exact hash was already credited.
    Duplicate,
    /// The set is at capacity; the share was not recorded and must not be credited.
    Full,
}

impl SeenShares {
    /// Distinct credited shares kept per height. Every entry cost someone 2^32 hashes at
    /// least (min-diff 1), so at the pool's hashrate this is far more than a block's worth;
    /// only a flood of genuine diff-1 work gets near it, and refusing that flood is the
    /// right answer.
    pub const MAX_PER_HEIGHT: usize = 1_000_000;
    /// Across all heights still retained.
    pub const MAX_TOTAL: usize = 2_500_000;

    pub fn insert(&mut self, height: u32, hash: Hash) -> Seen {
        let set = self.by_height.entry(height).or_default();
        if set.contains(&hash) {
            return Seen::Duplicate;
        }
        if set.len() >= Self::MAX_PER_HEIGHT || self.total >= Self::MAX_TOTAL {
            return Seen::Full;
        }
        set.insert(hash);
        self.total += 1;
        Seen::Fresh
    }

    /// Forget every height below `min_height`.
    pub fn prune_below(&mut self, min_height: u32) {
        while let Some((&h, _)) = self.by_height.first_key_value() {
            if h >= min_height {
                break;
            }
            if let Some(set) = self.by_height.remove(&h) {
                self.total -= set.len();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.total
    }

    pub fn heights(&self) -> usize {
        self.by_height.len()
    }
}

/// Live DATUM connections, total and per remote address, so one host cannot hold every
/// session slot (each session buffers coinbases and job state on the attacker's behalf).
#[derive(Debug, Default)]
pub struct Connections {
    per_ip: HashMap<IpAddr, u32>,
    total: u32,
}

impl Connections {
    /// Reserve a slot for `ip`, or say which limit it would break.
    pub fn admit(&mut self, ip: IpAddr, max_total: u32, max_per_ip: u32) -> Result<(), &'static str> {
        if self.total >= max_total {
            return Err("connection limit reached");
        }
        let n = self.per_ip.entry(ip).or_insert(0);
        if *n >= max_per_ip {
            return Err("per-address connection limit reached");
        }
        *n += 1;
        self.total += 1;
        Ok(())
    }

    pub fn release(&mut self, ip: IpAddr) {
        if let Some(n) = self.per_ip.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_ip.remove(&ip);
            }
        }
        self.total = self.total.saturating_sub(1);
    }

    pub fn total(&self) -> u32 {
        self.total
    }
}

pub struct Shared {
    pub cfg: Config,
    pub pool: Identity,
    pub pool_script: Vec<u8>,
    pub network: Network,
    pub split_params: SplitParams,
    pub ledger: Mutex<Ledger>,
    pub blocks: Mutex<Vec<BlockRecord>>,
    pub block_log: BlockLog,
    pub clients: Mutex<HashMap<u64, ClientInfo>>,
    pub seen: Mutex<SeenShares>,
    pub connections: Mutex<Connections>,
    pub tip_tx: watch::Sender<Option<Tip>>,
    pub tip: watch::Receiver<Option<Tip>>,
    /// Fired when a block candidate is found or the node tip moves; sessions relay a
    /// block-notify so gateways refresh their templates.
    pub notify: broadcast::Sender<u32>,
    pub rpc: Rpc,
    pub totals: Totals,
    pub started: Instant,
    pub started_ts: u64,
    pub next_client_id: AtomicU64,
}

impl Shared {
    pub fn client_update(&self, id: u64, f: impl FnOnce(&mut ClientInfo)) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&id) {
            f(c);
        }
    }

    pub fn tip_snapshot(&self) -> Option<Tip> {
        self.tip.borrow().clone()
    }

    /// Target work for the TIDES window from the current network difficulty.
    pub fn window_target(&self, difficulty: f64) -> u64 {
        let t = (difficulty * f64::from(self.cfg.window)).round().max(1.0) as u64;
        t.max(self.cfg.window_min_work)
    }

    pub fn record_block(&self, r: BlockRecord) {
        if let Err(e) = self.block_log.append(&r) {
            log::error!("block log append failed: {e}");
        }
        let mut b = self.blocks.lock().unwrap();
        b.push(r);
        if b.len() > 10_000 {
            let excess = b.len() - 10_000;
            b.drain(..excess);
        }
    }

    pub fn update_block(&self, hash: &str, f: impl FnOnce(&mut BlockRecord)) -> Option<BlockRecord> {
        let updated = {
            let mut b = self.blocks.lock().unwrap();
            let r = b.iter_mut().rev().find(|r| r.hash == hash)?;
            f(r);
            r.clone()
        };
        if let Err(e) = self.block_log.append(&updated) {
            log::error!("block log append failed: {e}");
        }
        Some(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u64) -> Hash {
        let mut a = [0u8; 32];
        a[..8].copy_from_slice(&n.to_le_bytes());
        a
    }

    #[test]
    fn a_share_is_credited_once_regardless_of_who_resubmits_it() {
        let mut s = SeenShares::default();
        assert_eq!(s.insert(100, h(1)), Seen::Fresh);
        // the finding's loop: alternate job sections, resubmit the same share forever
        for _ in 0..10 {
            assert_eq!(s.insert(100, h(1)), Seen::Duplicate);
        }
        // a reconnect is the same set
        assert_eq!(s.insert(100, h(1)), Seen::Duplicate);
        // different height is different work (the hash commits to the height anyway)
        assert_eq!(s.insert(101, h(1)), Seen::Fresh);
        assert_eq!(s.len(), 2);
        assert_eq!(s.heights(), 2);
    }

    #[test]
    fn pruning_forgets_only_old_heights() {
        let mut s = SeenShares::default();
        for height in 95..=101u32 {
            for i in 0..3 {
                s.insert(height, h(u64::from(height) * 10 + i));
            }
        }
        assert_eq!(s.len(), 21);
        s.prune_below(99);
        assert_eq!(s.heights(), 3);
        assert_eq!(s.len(), 9);
        assert_eq!(s.insert(99, h(990)), Seen::Duplicate);
        assert_eq!(s.insert(98, h(980)), Seen::Fresh, "an old height can be re-entered; the stale check gates it");
        s.prune_below(200);
        assert_eq!(s.len(), 0);
    }

    #[test]
    fn a_full_set_refuses_rather_than_forgets() {
        let mut s = SeenShares::default();
        for i in 0..SeenShares::MAX_PER_HEIGHT as u64 {
            assert_eq!(s.insert(7, h(i)), Seen::Fresh);
        }
        assert_eq!(s.insert(7, h(u64::MAX)), Seen::Full);
        // everything already there is still remembered
        assert_eq!(s.insert(7, h(0)), Seen::Duplicate);
        assert_eq!(s.insert(7, h(SeenShares::MAX_PER_HEIGHT as u64 - 1)), Seen::Duplicate);
        // another height still has room until the total cap
        assert_eq!(s.insert(8, h(1)), Seen::Fresh);
    }

    #[test]
    fn connection_limits_are_per_ip_and_total() {
        let mut c = Connections::default();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(c.admit(a, 3, 2).is_ok());
        assert!(c.admit(a, 3, 2).is_ok());
        assert_eq!(c.admit(a, 3, 2), Err("per-address connection limit reached"));
        assert!(c.admit(b, 3, 2).is_ok());
        assert_eq!(c.admit(b, 3, 2), Err("connection limit reached"));
        c.release(a);
        assert!(c.admit(b, 3, 2).is_ok());
        assert_eq!(c.total(), 3);
        c.release(a);
        c.release(b);
        c.release(b);
        assert_eq!(c.total(), 0);
        c.release(b); // over-release is harmless
        assert_eq!(c.total(), 0);
    }
}
