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
    reason.starts_with("bad-txns-") || reason == "bad-blk-sigops"
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

/// Faulted gateways by whole signing key (hex). Kept across reconnects; a restart forgets them,
/// and the next template check finds them again.
#[derive(Debug, Default)]
pub struct Faults {
    by_key: Mutex<HashMap<String, Fault>>,
}

impl Faults {
    pub fn get(&self, key: &str) -> Option<Fault> {
        self.by_key.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    /// Mark `key`; true if it was not marked before.
    pub fn set(&self, key: &str, reason: &str, height: u32, found_by: &'static str) -> bool {
        let mut m = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        let new = !m.contains_key(key);
        m.insert(key.to_string(), Fault { reason: reason.to_string(), height, since_ts: now(), found_by });
        new
    }

    pub fn clear(&self, key: &str) -> Option<Fault> {
        self.by_key.lock().unwrap_or_else(|e| e.into_inner()).remove(key)
    }

    pub fn all(&self) -> Vec<(String, Fault)> {
        let m = self.by_key.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<_> = m.iter().map(|(k, f)| (k.clone(), f.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
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
}
