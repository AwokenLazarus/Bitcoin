//! Work that can never become a valid block earns nothing.
//!
//! A share proves hashes were done; it says nothing about whether the block they were done on
//! could ever be accepted. After the long coinbase maturity soft fork (block 973,440), gateways
//! whose nodes had not upgraded went on mining in two ways that cannot pay anyone (22 Sep 2026):
//!
//! - **on a dead parent.** Their nodes accepted blocks ours rejects (973,543, 973,545, 973,548,
//!   973,553 all spent a coinbase about a hundred blocks deep, where 6,480 are now required) and
//!   built on them. Our node has the parent and calls it invalid, or has never seen it: a child
//!   of an invalid block is refused before it is stored, so it never reaches an upgraded node.
//!   See [`ParentBook`].
//! - **on a dead template.** Its transactions break the rules at our tip: the gateway's mempool
//!   holds a spend our node will not mine, and every block it builds carries it. 973,553 was
//!   found this way on a Lazarus gateway. See [`Faults`].
//!
//! Neither is taken on a guess. A parent is judged by our node, after a grace period long enough
//! for any real block to reach it; a template by our node's own validation of it
//! (`getblocktemplate` proposal mode) or of a block built from it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use datum_wire::pow::Hash;

use crate::rpc::RpcError;
use crate::state::{now, Shared};

/// How long a parent our node has not seen is given to arrive before work on it is refused.
/// A real block reaches a well-connected node in seconds; one our node rejected never does.
pub const PARENT_GRACE: Duration = Duration::from_secs(30);
/// How often the node is asked again about a parent it has already answered for. A block can be
/// found invalid after its header arrived (the node rejects it once the whole block is in).
const PARENT_RECHECK: Duration = Duration::from_secs(10);
/// Parents remembered at most, and for how long; plenty for the handful a fork produces.
const PARENT_MAX: usize = 4096;
const PARENT_KEEP: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParentState {
    /// Not asked yet, or the node could not answer.
    Unknown,
    /// Our node has the header and does not call it invalid.
    Known,
    /// Our node has never seen it.
    Unseen,
    /// Our node has it and rejected it (or a block beneath it).
    Invalid,
}

#[derive(Debug)]
struct ParentEntry {
    state: ParentState,
    first_seen: Instant,
    /// When the node was last asked; set before asking, so a burst of shares asks once.
    asked: Option<Instant>,
}

/// What primed makes of a parent that is not our tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentVerdict {
    Take,
    /// Our node rejected it: nothing built on it can be a block.
    Invalid,
    /// Our node has not seen it in [`PARENT_GRACE`].
    Unseen,
}

impl ParentVerdict {
    pub fn why(self) -> &'static str {
        match self {
            ParentVerdict::Take => "taken",
            ParentVerdict::Invalid => "our node rejected that block as invalid",
            ParentVerdict::Unseen => "our node has never seen that block",
        }
    }
}

/// Parents gateways have built on that are not our node's tip, and what our node says of them.
#[derive(Debug, Default)]
pub struct ParentBook {
    entries: Mutex<HashMap<Hash, ParentEntry>>,
}

impl ParentBook {
    /// The verdict from what is already known, and whether the node should be asked now.
    fn lookup(&self, prev: &Hash, at: Instant) -> (ParentVerdict, bool) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= PARENT_MAX && !entries.contains_key(prev) {
            entries.retain(|_, e| at.duration_since(e.first_seen) < PARENT_KEEP);
            if entries.len() >= PARENT_MAX {
                // Only a gateway inventing parents gets here; forgetting is safe, because
                // a parent starts over as unseen and is refused again after the grace.
                entries.clear();
            }
        }
        let e =
            entries.entry(*prev).or_insert(ParentEntry { state: ParentState::Unknown, first_seen: at, asked: None });
        let ask = e.state != ParentState::Invalid && e.asked.is_none_or(|t| at.duration_since(t) >= PARENT_RECHECK);
        if ask {
            e.asked = Some(at);
        }
        (verdict(e, at), ask)
    }

    fn record(&self, prev: &Hash, state: ParentState, at: Instant) -> ParentVerdict {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let e = entries.entry(*prev).or_insert(ParentEntry { state, first_seen: at, asked: Some(at) });
        // invalid stays invalid; an answer the node could not give changes nothing
        if e.state != ParentState::Invalid && state != ParentState::Unknown {
            e.state = state;
        }
        verdict(e, at)
    }
}

fn verdict(e: &ParentEntry, at: Instant) -> ParentVerdict {
    match e.state {
        ParentState::Invalid => ParentVerdict::Invalid,
        ParentState::Unseen if at.duration_since(e.first_seen) >= PARENT_GRACE => ParentVerdict::Unseen,
        _ => ParentVerdict::Take,
    }
}

/// Judge a parent that is not our tip. Asks the node at most once per [`PARENT_RECHECK`] per
/// parent across every session; in between, and whenever the node cannot answer, the last
/// verdict stands, and a parent with no verdict yet is taken.
pub async fn parent_verdict(shared: &Shared, prev_le: &Hash) -> ParentVerdict {
    let at = Instant::now();
    let (known, ask) = shared.parents.lookup(prev_le, at);
    if !ask {
        return known;
    }
    let mut be = *prev_le;
    be.reverse();
    let hash = hex::encode(be);
    let state = match shared.rpc.getblockheader(&hash).await {
        Ok(_) => match shared.rpc.call("getchaintips", serde_json::json!([])).await {
            Ok(tips) => {
                let invalid = tips.as_array().is_some_and(|tips| {
                    tips.iter().any(|t| {
                        t.get("hash").and_then(|h| h.as_str()) == Some(hash.as_str())
                            && t.get("status").and_then(|s| s.as_str()) == Some("invalid")
                    })
                });
                if invalid {
                    ParentState::Invalid
                } else {
                    ParentState::Known
                }
            }
            Err(_) => ParentState::Known,
        },
        // "Block not found"
        Err(RpcError::Node { code: -5, .. }) => ParentState::Unseen,
        Err(e) => {
            log::debug!("getblockheader {hash}: {e}; parent keeps its last verdict");
            ParentState::Unknown
        }
    };
    shared.parents.record(prev_le, state, at)
}

/// Why a block built from a gateway's template was refused, when the reason lies in the
/// template's own transactions. Only these mark a gateway: a reason about the header, the
/// parent, the coinbase or the merkle root can come from how Prime put the block together, or
/// from a race, and must not cost an honest gateway its credit.
pub fn template_fault(reason: &str) -> bool {
    let reason = reason.trim();
    reason.starts_with("bad-txns-") || reason == "bad-blk-sigops"
}

/// The one template reason that cannot be a Prime assembly slip. A node that knows Knots #419
/// will not put a premature coinbase spend in a template, so seeing it is enough to fault the
/// gateway at once. Any other [`template_fault`] needs two consecutive failing checks on
/// different jobs — `assemble_block` can report `bad-txns-inputs-missingorspent` from a
/// partial or mis-ordered job-validation reply, and that must not refuse an honest gateway.
pub fn faults_at_once(reason: &str) -> bool {
    reason.trim().starts_with("bad-txns-premature-spend-of-coinbase")
}

/// Distinguishes one job from another for [`Faults::note_fail`]. Same slot at the same height
/// is the same job (a recheck of a Prime assembly slip must not count as the second strike).
pub fn job_token(job_id: u8, height: u32) -> u32 {
    ((u32::from(job_id)) << 24) | (height & 0x00ff_ffff)
}

/// A gateway whose template our node found invalid; its shares are refused until a later
/// template of its passes.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Fault {
    pub reason: String,
    pub height: u32,
    pub since_ts: u64,
    /// "block" (our node refused a block it found) or "proposal" (a template check).
    pub found_by: &'static str,
}

/// A first failing check that is not enough to fault the gateway on its own.
#[derive(Clone, Debug)]
struct PendingFail {
    /// Opaque job token from the caller (height and slot); the confirming check must differ.
    job: u32,
}

/// Faulted gateways by whole signing key (hex). Kept across reconnects; a restart forgets them,
/// and the next template check finds them again. A live fault also expires after
/// `quarantine-max-hours` (see [`Faults::with_ttl`]) so an upgraded gateway mining empty
/// templates, or one that never answers job validation, is not stuck until Prime restarts.
#[derive(Debug)]
pub struct Faults {
    by_key: Mutex<HashMap<String, Fault>>,
    pending: Mutex<HashMap<String, PendingFail>>,
    /// 0 means no expiry (tests). Production sets `quarantine-max-hours` in seconds.
    ttl_secs: AtomicU64,
}

impl Default for Faults {
    fn default() -> Self {
        Self {
            by_key: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            ttl_secs: AtomicU64::new(0),
        }
    }
}

impl Faults {
    pub fn with_ttl(ttl_secs: u64) -> Self {
        Self { ttl_secs: AtomicU64::new(ttl_secs), ..Self::default() }
    }

    fn ttl(&self) -> u64 {
        match self.ttl_secs.load(Ordering::Relaxed) {
            0 => u64::MAX,
            s => s,
        }
    }

    pub fn get(&self, key: &str) -> Option<Fault> {
        self.get_at(key, now(), self.ttl())
    }

    pub fn get_at(&self, key: &str, at: u64, ttl_secs: u64) -> Option<Fault> {
        let mut m = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        if m.get(key).is_some_and(|f| ttl_secs < u64::MAX && at.saturating_sub(f.since_ts) >= ttl_secs) {
            m.remove(key);
            return None;
        }
        m.get(key).cloned()
    }

    /// Mark `key`; true if it was not marked before.
    pub fn set(&self, key: &str, reason: &str, height: u32, found_by: &'static str) -> bool {
        self.set_at(key, reason, height, found_by, now())
    }

    fn set_at(&self, key: &str, reason: &str, height: u32, found_by: &'static str, at: u64) -> bool {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        let mut m = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        let new = !m.contains_key(key);
        m.insert(key.to_string(), Fault { reason: reason.to_string(), height, since_ts: at, found_by });
        new
    }

    /// Record a failed template check or found-block verdict.
    ///
    /// [`faults_at_once`] reasons mark the gateway immediately. Any other [`template_fault`]
    /// waits for a second consecutive failure on a different `job`. Returns true if this call
    /// newly faults the gateway.
    pub fn note_fail(&self, key: &str, reason: &str, job: u32, height: u32, found_by: &'static str) -> bool {
        if self.get(key).is_some() || faults_at_once(reason) {
            return self.set(key, reason, height, found_by);
        }
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        match pending.get(key) {
            Some(prev) if prev.job != job => {
                pending.remove(key);
                drop(pending);
                self.set(key, reason, height, found_by)
            }
            Some(_) => false,
            None => {
                pending.insert(key.to_string(), PendingFail { job });
                false
            }
        }
    }

    pub fn clear(&self, key: &str) -> Option<Fault> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        self.by_key.lock().unwrap_or_else(|e| e.into_inner()).remove(key)
    }

    pub fn all(&self) -> Vec<(String, Fault)> {
        self.all_at(now(), self.ttl())
    }

    pub fn all_at(&self, at: u64, ttl_secs: u64) -> Vec<(String, Fault)> {
        let mut m = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        if ttl_secs < u64::MAX {
            m.retain(|_, f| at.saturating_sub(f.since_ts) < ttl_secs);
        }
        let mut v: Vec<_> = m.iter().map(|(k, f)| (k.clone(), f.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// A stored quarantine whose `until` has passed: the gateway is admitted again, and any
/// leftover template fault from that incident should be dropped so an upgraded operator
/// mining empty templates is credited without waiting for a passing check or a restart.
pub fn quarantine_has_lapsed(until: u64, at: u64) -> bool {
    until <= at
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Hash = [7u8; 32];

    #[test]
    fn an_unseen_parent_is_taken_through_the_grace_and_refused_after() {
        let book = ParentBook::default();
        let t0 = Instant::now();
        let (v, ask) = book.lookup(&P, t0);
        assert_eq!((v, ask), (ParentVerdict::Take, true));
        assert_eq!(book.record(&P, ParentState::Unseen, t0), ParentVerdict::Take);
        // asked once per recheck period, whatever the traffic
        assert_eq!(book.lookup(&P, t0 + Duration::from_secs(1)), (ParentVerdict::Take, false));
        let late = t0 + PARENT_GRACE;
        assert_eq!(book.lookup(&P, late), (ParentVerdict::Unseen, true));
        // it turned up after all: taken again
        assert_eq!(book.record(&P, ParentState::Known, late), ParentVerdict::Take);
    }

    #[test]
    fn an_invalid_parent_stays_invalid_and_is_not_asked_about_again() {
        let book = ParentBook::default();
        let t0 = Instant::now();
        book.lookup(&P, t0);
        assert_eq!(book.record(&P, ParentState::Invalid, t0), ParentVerdict::Invalid);
        assert_eq!(book.lookup(&P, t0 + Duration::from_secs(60)), (ParentVerdict::Invalid, false));
        assert_eq!(book.record(&P, ParentState::Known, t0), ParentVerdict::Invalid);
    }

    #[test]
    fn a_node_that_cannot_answer_changes_no_verdict() {
        let book = ParentBook::default();
        let t0 = Instant::now();
        book.lookup(&P, t0);
        assert_eq!(book.record(&P, ParentState::Known, t0), ParentVerdict::Take);
        assert_eq!(book.record(&P, ParentState::Unknown, t0 + PARENT_GRACE * 2), ParentVerdict::Take);
    }

    #[test]
    fn only_a_transaction_reason_is_the_templates_fault() {
        for r in ["bad-txns-premature-spend-of-coinbase", "bad-txns-inputs-missingorspent", "bad-blk-sigops"] {
            assert!(template_fault(r), "{r}");
        }
        for r in [
            "bad-txnmrklroot",
            "bad-witness-merkle-match",
            "bad-cb-amount",
            "bad-prevblk",
            "high-hash",
            "duplicate",
            "inconclusive",
            "inconclusive-not-best-prevblk",
        ] {
            assert!(!template_fault(r), "{r}");
        }
    }

    #[test]
    fn a_fault_is_kept_until_cleared() {
        let f = Faults::default();
        assert!(f.set("k", "bad-txns-premature-spend-of-coinbase", 973553, "block"));
        assert!(!f.set("k", "bad-txns-premature-spend-of-coinbase", 973554, "proposal"));
        assert_eq!(f.get("k").unwrap().height, 973554);
        assert!(f.clear("k").is_some());
        assert!(f.get("k").is_none());
    }

    #[test]
    fn a_premature_coinbase_spend_faults_at_once() {
        let f = Faults::default();
        assert!(faults_at_once("bad-txns-premature-spend-of-coinbase"));
        assert!(faults_at_once("bad-txns-premature-spend-of-coinbase, tried to spend coinbase at depth 102"));
        assert!(!faults_at_once("bad-txns-inputs-missingorspent"));
        assert!(!faults_at_once("bad-blk-sigops"));
        assert!(f.note_fail("k", "bad-txns-premature-spend-of-coinbase", 1, 973553, "template check"));
        assert_eq!(f.get("k").unwrap().height, 973553);
    }

    #[test]
    fn a_prime_assembly_slip_needs_two_different_jobs() {
        let f = Faults::default();
        assert!(!f.note_fail("k", "bad-txns-inputs-missingorspent", 1, 100, "template check"));
        assert!(f.get("k").is_none(), "one slip must not refuse an honest gateway");
        assert!(!f.note_fail("k", "bad-txns-inputs-missingorspent", 1, 100, "template check"));
        assert!(f.get("k").is_none(), "the same job again is still one slip");
        assert!(f.note_fail("k", "bad-blk-sigops", 2, 101, "template check"));
        assert_eq!(f.get("k").unwrap().reason, "bad-blk-sigops");
    }

    #[test]
    fn a_passing_check_forgets_the_first_slip() {
        let f = Faults::default();
        assert!(!f.note_fail("k", "bad-txns-inputs-missingorspent", 1, 100, "template check"));
        assert!(f.clear("k").is_none(), "pending is not yet a fault");
        assert!(!f.note_fail("k", "bad-txns-inputs-missingorspent", 2, 101, "template check"));
        assert!(f.get("k").is_none(), "a cleared first slip starts the count over");
    }

    #[test]
    fn a_fault_expires_after_its_ttl() {
        let f = Faults::with_ttl(86_400);
        f.set_at("k", "bad-txns-premature-spend-of-coinbase", 973553, "block", 1_000);
        assert!(f.get_at("k", 1_000 + 86_400 - 1, 86_400).is_some());
        assert!(f.get_at("k", 1_000 + 86_400, 86_400).is_none());
        assert!(f.get("k").is_none(), "an expired fault is dropped, not just hidden");
        f.set_at("old", "bad-txns-premature-spend-of-coinbase", 1, "block", 1);
        f.set_at("new", "bad-txns-premature-spend-of-coinbase", 2, "block", 100_000);
        let live = f.all_at(100_000, 86_400);
        assert_eq!(live.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["new"]);
    }

    #[test]
    fn connecting_after_quarantine_lapses_clears_the_fault() {
        assert!(quarantine_has_lapsed(1_000, 1_000));
        assert!(quarantine_has_lapsed(1_000, 1_001));
        assert!(!quarantine_has_lapsed(1_001, 1_000));
        let f = Faults::default();
        f.set("k", "bad-txns-premature-spend-of-coinbase", 973553, "block");
        if quarantine_has_lapsed(1_000, 3_600) {
            assert!(f.clear("k").is_some());
        }
        assert!(f.get("k").is_none());
    }
}
