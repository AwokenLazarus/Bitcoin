//! State shared by every session, the node poller, and the stats server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use datum_wire::crypto::Identity;
use datum_wire::pow::Hash;
use tides::{BlockLog, BlockRecord, Ledger, MinerStat, SplitParams};
use tokio::sync::{broadcast, watch};

use crate::address::{self, Network};
use crate::config::Config;
use crate::rpc::Rpc;

/// How long a [`CoinbaserBase`] is reused before the window is snapshotted again.
///
/// A gateway asks for a coinbaser once per template — every ten seconds or so — but at a tip
/// change every one of them asks at once. One second collapses that burst into a single
/// snapshot while keeping the window effectively live: a share credited inside the last
/// second lands in the next coinbaser instead of this one, and the window spans hours.
pub const COINBASER_BASE_TTL: Duration = Duration::from_secs(1);

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
    /// Accepted shares whose coinbase paid only the pool script. Work the window earns
    /// nothing from if it finds a block, so a gateway sitting above zero here is publishing
    /// jobs without the split and wants looking at before it gets lucky.
    pub pool_only_shares: u64,
    /// Of those, the ones on a job that carried transactions. Stock DATUM's per-height
    /// subsidy-only job is expected and unavoidable; this is the count that should be zero.
    pub pool_only_full_jobs: u64,
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
    /// Coinbaser replies repeated verbatim because the session was over its bucket and had
    /// already been answered for that exact value.
    pub coinbasers_repeated: AtomicU64,
    /// Coinbaser replies computed while the session was over its bucket. Answered anyway:
    /// silence costs a pool-only coinbase.
    pub coinbasers_over_rate: AtomicU64,
    /// Coinbaser replies slow enough to be worth a warning; see `session::COINBASER_SLOW`.
    pub coinbasers_slow: AtomicU64,
    /// Longest request-to-reply time for a coinbaser, in microseconds.
    pub coinbaser_max_us: AtomicU64,
    /// Times the shared window snapshot behind coinbaser replies was actually rebuilt. Far
    /// below `coinbasers` is the point: it means replies are being served from one snapshot
    /// instead of each taking the ledger lock and recomputing the same split.
    pub coinbaser_base_builds: AtomicU64,
    /// Accepted shares whose coinbase paid only the pool script.
    pub pool_only_shares: AtomicU64,
    /// Of those, the ones on a job that carried transactions — the kind that should be zero.
    pub pool_only_full_jobs: AtomicU64,
}

impl Totals {
    pub fn add(&self, c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }

    pub fn raise(&self, c: &AtomicU64, n: u64) {
        c.fetch_max(n, Ordering::Relaxed);
    }
}

/// Everything a coinbaser reply is computed from, snapshotted out of the window with the
/// identity → payout script conversion already done.
///
/// Stock DATUM's coinbaser thread sends its request and then blocks for five seconds; if the
/// reply has not arrived it publishes the job anyway with a coinbase that pays only the pool
/// script, and a block found on that work owes the window everything (block 968440). A reply
/// therefore must not queue behind the ledger mutex, and at a tip change every gateway asks
/// at once — so the expensive half is done once per [`COINBASER_BASE_TTL`] and shared.
pub struct CoinbaserBase {
    pub miners: Vec<MinerStat>,
    pub total_work: u64,
    pub target_work: u64,
    /// Payout script per identity in `miners`, so a reply never redoes bech32.
    pub scripts: HashMap<String, Vec<u8>>,
    built_at: Instant,
}

impl CoinbaserBase {
    pub fn script_for(&self, identity: &str) -> Option<Vec<u8>> {
        self.scripts.get(identity).cloned()
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
    /// Shared window snapshot behind coinbaser replies; see [`Shared::coinbaser_base`].
    pub coinbaser_base: Mutex<Option<Arc<CoinbaserBase>>>,
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

    /// The window snapshot a coinbaser reply is computed from, rebuilt at most once per
    /// [`COINBASER_BASE_TTL`].
    ///
    /// The rebuild holds this mutex across the ledger lock, so the gateways that arrive
    /// during one wait for it and then all read the same snapshot instead of each taking the
    /// ledger lock and redoing the same work. Nothing takes the ledger lock and then this
    /// one, so the order cannot deadlock.
    pub fn coinbaser_base(&self) -> Arc<CoinbaserBase> {
        let mut slot = self.coinbaser_base.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(base) = slot.as_ref() {
            if base.built_at.elapsed() < COINBASER_BASE_TTL {
                return base.clone();
            }
        }
        let (miners, total_work, target_work) = {
            let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            (ledger.window.miners(), ledger.window.total_work(), ledger.window.target_work())
        };
        let scripts = miners
            .iter()
            .filter_map(|m| address::to_script(&m.identity, self.network).map(|s| (m.identity.clone(), s)))
            .collect();
        let base = Arc::new(CoinbaserBase { miners, total_work, target_work, scripts, built_at: Instant::now() });
        *slot = Some(base.clone());
        self.totals.add(&self.totals.coinbaser_base_builds, 1);
        base
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

    /// A coinbaser reply is computed off a shared snapshot rather than the live window, so the
    /// snapshot must pay exactly what the window would have. This is the money path: if the two
    /// ever disagree, gateways are handed a split the pool did not intend.
    #[test]
    fn a_snapshot_split_pays_what_the_live_window_would() {
        use tides::{Window, SOURCE_DATUM, SOURCE_STRATUM};

        let net = Network::Mainnet;
        let mut w = Window::new();
        w.set_target(1_000_000);
        // real payout addresses, so the bech32 path is the one under test
        w.credit("bc1qk3kxstl02hqnhynwtx0zws7merw6ynut52vtzs", 400_000, 1, 100, SOURCE_DATUM);
        w.credit("bc1qpxcy2pgedcfccfpw0p9xpzm3edkgajmjl5xe02", 250_000, 1, 100, SOURCE_DATUM);
        w.credit("bc1q4kar3d2l33utmscncmhc923gg8xy459qp2e554", 100_000, 1, 100, SOURCE_STRATUM);
        w.credit("38vJdhcMNudZSNHPQdmfCAL1VnZRjK4ouk", 40_000, 1, 100, SOURCE_DATUM);
        // an identity with no payable script must be dropped by both paths alike
        w.credit("not-an-address", 10_000, 1, 100, SOURCE_DATUM);

        let params = SplitParams { fee_bps: 50, stratum_fee_bps: 250, ..SplitParams::default() };
        let scripts: HashMap<String, Vec<u8>> = w
            .miners()
            .iter()
            .filter_map(|m| address::to_script(&m.identity, net).map(|s| (m.identity.clone(), s)))
            .collect();

        // the subsidy alone, and a value carrying fees, as a real template would
        for value in [312_500_000u64, 312_644_067] {
            let live = w.split(value, &params, |i| address::to_script(i, net));
            let snap =
                tides::split::compute(w.miners(), w.total_work(), value, &params, |i| scripts.get(i).cloned());
            assert_eq!(snap.fee_sats, live.fee_sats, "fee at value={value}");
            assert_eq!(snap.pool_sats, live.pool_sats, "pool remainder at value={value}");
            assert_eq!(snap.payees.len(), live.payees.len(), "payee count at value={value}");
            for (a, b) in snap.payees.iter().zip(live.payees.iter()) {
                assert_eq!((&a.identity, a.sats, &a.script), (&b.identity, b.sats, &b.script));
            }
            assert_eq!(snap.pool_sats + snap.paid_sats(), value, "outputs must sum to the template value");
            assert!(snap.payees.iter().all(|p| p.identity != "not-an-address"));
        }
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
