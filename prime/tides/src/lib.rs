//! TIDES accounting for a DATUM Prime.
//!
//! **TIDES** (Transparent Index of Distinct Extended Shares) pays each block to the
//! miners whose shares fall inside a sliding window of recent work. The window is sized
//! in *work*, not time or count: it holds the most recent shares whose summed difficulty
//! reaches `window_multiple × network_difficulty`. Every block's reward, minus the pool
//! fee, is split in proportion to each identity's work inside that window at the moment
//! the block's coinbase is issued.
//!
//! This crate is the pure accounting side: the [`Window`], the [`split`](Window::split),
//! and a small durable [`Ledger`] that survives restarts. It knows nothing about the wire
//! protocol or the node.
//!
//! Design notes, since "faster and leaner" was the brief:
//!
//! * Shares are **coalesced**: consecutive credits for the same identity in the same
//!   second and at the same height merge into one record. A GPU farm submitting
//!   difficulty-1 shares at 10 kH/s no longer produces ten thousand rows a second.
//! * Records are fixed 24-byte binary rows in an append-only file; identities are interned
//!   once into a side file. Loading replays the file, then trims to the window.
//! * Compaction rewrites the file with only the window's rows when it has grown to twice
//!   the window, so disk and startup stay proportional to the window.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod grace;
pub mod split;
pub use grace::{GraceBook, GraceParams};
pub use split::{Payee, Split, SplitParams, Unpaid, UnpaidReason};

/// Work that arrived before dual-fee tagging. Split as DATUM (the lower fee).
pub const SOURCE_UNKNOWN: u8 = 0;
/// Public house stratum (our gateway, our templates).
pub const SOURCE_STRATUM: u8 = 1;
/// External DATUM / Prime gateway.
pub const SOURCE_DATUM: u8 = 2;
/// Public house stratum inside the address's grace (see [`grace`]): charged the grace fee.
/// A build from before the tag reads such a row as untagged, that is as DATUM work.
pub const SOURCE_STRATUM_GRACE: u8 = 3;

/// One accepted unit of work, possibly several coalesced shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credit {
    /// Unix seconds.
    pub ts: u32,
    /// Index into the identity table.
    pub ident: u32,
    /// Difficulty-1 share units.
    pub work: u64,
    /// Height the work was for.
    pub height: u32,
    /// `SOURCE_*` — stored in the last 4 bytes of the on-disk row (was padding).
    pub source: u8,
}

impl Credit {
    pub const SIZE: usize = 24;

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.ts.to_le_bytes());
        out.extend_from_slice(&self.ident.to_le_bytes());
        out.extend_from_slice(&self.work.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.extend_from_slice(&[self.source, 0, 0, 0]);
    }

    fn read(b: &[u8; Self::SIZE]) -> Self {
        Credit {
            ts: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            ident: u32::from_le_bytes(b[4..8].try_into().unwrap()),
            work: u64::from_le_bytes(b[8..16].try_into().unwrap()),
            height: u32::from_le_bytes(b[16..20].try_into().unwrap()),
            source: b[20],
        }
    }
}

/// Per-identity view of the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MinerStat {
    pub identity: String,
    pub work: u64,
    /// Work done on the house stratum (`SOURCE_STRATUM` and `SOURCE_STRATUM_GRACE`). The rest
    /// of `work` is DATUM (or untagged).
    pub stratum_work: u64,
    /// The part of `stratum_work` tagged `SOURCE_STRATUM_GRACE`: charged the grace fee.
    #[serde(default)]
    pub grace_work: u64,
    pub credits: u64,
    pub last_ts: u32,
    /// Sats earned in earlier blocks that no coinbase has placed yet (under the payout
    /// floor or over the size budget). Paid on top of the earned share once it fits.
    #[serde(default)]
    pub carry: u64,
}

/// The sliding share window and identity table. Pure in-memory state.
#[derive(Debug, Default)]
pub struct Window {
    idents: Vec<String>,
    ident_index: HashMap<String, u32>,
    credits: VecDeque<Credit>,
    totals: HashMap<u32, (u64, u64, u32)>, // work, credit rows, last ts
    total_work: u64,
    target_work: u64,
    /// Unplaced earnings per identity; see [`MinerStat::carry`]. Only non-zero entries.
    carry: HashMap<u32, u64>,
    /// DATUM rebate the pool owes the window from outside it (the rebate share of solo-block
    /// fees, and stratum rebate from blocks with no DATUM work). Paid down by each split out
    /// of the fee the pool keeps; see `split::compute`.
    rebate_owed: u64,
    /// When each identity last had work credited, unix seconds. The window forgets an identity
    /// once its rows age out; carry does not, and whether a balance is *stale* (its owner has
    /// stopped mining) is a question about exactly the identities the window has forgotten.
    last_seen: HashMap<u32, u32>,
    /// Carry set aside for a payout made outside the coinbase; see [`Hold`]. By batch id.
    holds: std::collections::BTreeMap<String, Hold>,
    /// Carry a coinbase paid that was not on the books: a block mined on a coinbaser issued
    /// before that carry was paid. Collected from this identity's later earnings.
    debt: HashMap<u32, u64>,
    pub lifetime_shares: u64,
    pub lifetime_work: u64,
}

impl Window {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, identity: &str) -> u32 {
        if let Some(&i) = self.ident_index.get(identity) {
            return i;
        }
        let i = self.idents.len() as u32;
        self.idents.push(identity.to_owned());
        self.ident_index.insert(identity.to_owned(), i);
        i
    }

    pub fn identity(&self, ident: u32) -> Option<&str> {
        self.idents.get(ident as usize).map(String::as_str)
    }

    pub fn identities(&self) -> &[String] {
        &self.idents
    }

    /// Index of an interned identity, without scanning [`Window::identities`]. The table
    /// grows monotonically, so a linear scan per lookup gets slower for the life of the
    /// process; this is the same `HashMap` [`Window::intern`] already maintains.
    pub fn index_of(&self, identity: &str) -> Option<u32> {
        self.ident_index.get(identity).copied()
    }

    pub fn target_work(&self) -> u64 {
        self.target_work
    }

    pub fn total_work(&self) -> u64 {
        self.total_work
    }

    pub fn len(&self) -> usize {
        self.credits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.credits.is_empty()
    }

    pub fn credits(&self) -> impl DoubleEndedIterator<Item = &Credit> + ExactSizeIterator {
        self.credits.iter()
    }

    /// Work in the window for one identity.
    pub fn work_of(&self, identity: &str) -> u64 {
        self.ident_index.get(identity).and_then(|i| self.totals.get(i)).map_or(0, |t| t.0)
    }

    /// Resize the window (e.g. the network difficulty changed) and trim.
    pub fn set_target(&mut self, target_work: u64) {
        self.target_work = target_work;
        self.trim();
    }

    /// Add work. Returns the credit as stored (it may have been merged into the tail row),
    /// and whether a new row was appended.
    pub fn credit(&mut self, identity: &str, work: u64, height: u32, ts: u32, source: u8) -> (Credit, bool) {
        let ident = self.intern(identity);
        self.lifetime_shares += 1;
        self.lifetime_work = self.lifetime_work.saturating_add(work);
        self.total_work = self.total_work.saturating_add(work);
        self.note_seen(ident, ts);
        let t = self.totals.entry(ident).or_insert((0, 0, 0));
        t.0 = t.0.saturating_add(work);
        t.2 = ts;
        let appended = match self.credits.back_mut() {
            Some(tail) if tail.ident == ident && tail.ts == ts && tail.height == height && tail.source == source => {
                tail.work = tail.work.saturating_add(work);
                false
            }
            _ => {
                t.1 += 1;
                self.credits.push_back(Credit { ts, ident, work, height, source });
                true
            }
        };
        let stored = *self.credits.back().unwrap();
        self.trim();
        (stored, appended)
    }

    /// Add work as its own row, never merging into the tail. Used by [`Ledger`] so that
    /// on-disk rows stay 1:1 with window rows and a replay lands on the same trim boundary.
    pub fn credit_row(&mut self, identity: &str, work: u64, height: u32, ts: u32, source: u8) -> Credit {
        let ident = self.intern(identity);
        self.lifetime_shares += 1;
        self.lifetime_work = self.lifetime_work.saturating_add(work);
        self.total_work = self.total_work.saturating_add(work);
        self.note_seen(ident, ts);
        let t = self.totals.entry(ident).or_insert((0, 0, 0));
        t.0 = t.0.saturating_add(work);
        t.1 += 1;
        t.2 = ts;
        let c = Credit { ts, ident, work, height, source };
        self.credits.push_back(c);
        self.trim();
        c
    }

    /// Replay a stored row without coalescing or lifetime accounting.
    fn push_raw(&mut self, c: Credit) {
        self.note_seen(c.ident, c.ts);
        self.total_work = self.total_work.saturating_add(c.work);
        let t = self.totals.entry(c.ident).or_insert((0, 0, 0));
        t.0 = t.0.saturating_add(c.work);
        t.1 += 1;
        t.2 = t.2.max(c.ts);
        self.credits.push_back(c);
    }

    /// Drop the oldest rows while the window still holds at least the target.
    fn trim(&mut self) {
        if self.target_work == 0 {
            return;
        }
        while let Some(front) = self.credits.front() {
            // Both operands derive from the same rows, so this cannot underflow unless the
            // totals have drifted; if they have, stop trimming rather than wrap the window.
            let Some(rest) = self.total_work.checked_sub(front.work) else { break };
            if rest < self.target_work {
                break;
            }
            let c = self.credits.pop_front().unwrap();
            self.total_work = rest;
            if let Some(t) = self.totals.get_mut(&c.ident) {
                t.0 = t.0.saturating_sub(c.work);
                t.1 = t.1.saturating_sub(1);
                if t.1 == 0 {
                    self.totals.remove(&c.ident);
                }
            }
        }
    }

    /// Carry (unplaced earnings from earlier blocks) held for one identity.
    pub fn carry_of(&self, identity: &str) -> u64 {
        self.ident_index.get(identity).and_then(|i| self.carry.get(i)).copied().unwrap_or(0)
    }

    /// Outstanding DATUM rebate the pool owes the window; see [`Window::adjust_rebate_owed`].
    pub fn rebate_owed(&self) -> u64 {
        self.rebate_owed
    }

    /// Set the owed DATUM rebate outright (seeding, or an operator correction).
    pub fn set_rebate_owed(&mut self, sats: u64) {
        self.rebate_owed = sats;
    }

    /// Move the owed DATUM rebate by `delta` sats (a found block's [`Split::rebate_delta`],
    /// or a solo block's rebate share), saturating at zero. Returns the new balance.
    pub fn adjust_rebate_owed(&mut self, delta: i64) -> u64 {
        self.rebate_owed = if delta < 0 {
            self.rebate_owed.saturating_sub(delta.unsigned_abs())
        } else {
            self.rebate_owed.saturating_add(delta as u64)
        };
        self.rebate_owed
    }

    /// Sum of all carry the pool is holding for miners.
    pub fn total_carry(&self) -> u64 {
        self.carry.values().fold(0u64, |a, &b| a.saturating_add(b))
    }

    /// Every identity with carry, largest first.
    pub fn carries(&self) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> =
            self.carry.iter().map(|(&i, &s)| (self.idents[i as usize].clone(), s)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Set an identity's carry outright (seeding, or an operator correction).
    pub fn set_carry(&mut self, identity: &str, sats: u64) {
        let i = self.intern(identity);
        if sats == 0 {
            self.carry.remove(&i);
        } else {
            self.carry.insert(i, sats);
        }
    }

    /// Carry a coinbase paid twice, still to be collected from later earnings.
    pub fn debt_of(&self, identity: &str) -> u64 {
        self.ident_index.get(identity).and_then(|i| self.debt.get(i)).copied().unwrap_or(0)
    }

    /// Sum of [`Window::debt_of`].
    pub fn total_debt(&self) -> u64 {
        self.debt.values().fold(0u64, |a, &b| a.saturating_add(b))
    }

    /// Every identity with a double-pay debt, largest first.
    pub fn debts(&self) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.debt.iter().map(|(&i, &s)| (self.idents[i as usize].clone(), s)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// Record that `identity`'s coinbase was paid `sats` of carry the books did not hold.
    fn accrue_debt(&mut self, identity: &str, sats: u64) {
        if sats == 0 {
            return;
        }
        let i = self.intern(identity);
        let new = self.debt.get(&i).copied().unwrap_or(0).saturating_add(sats);
        self.debt.insert(i, new);
    }

    /// Collect up to `sats` of debt. Returns what was collected.
    fn reduce_debt(&mut self, identity: &str, sats: u64) -> u64 {
        let Some(&i) = self.ident_index.get(identity) else { return 0 };
        let cur = self.debt.get(&i).copied().unwrap_or(0);
        let take = cur.min(sats);
        let left = cur - take;
        if left == 0 {
            self.debt.remove(&i);
        } else {
            self.debt.insert(i, left);
        }
        take
    }

    /// Put a debt back after the block that paid it down is orphaned. Taken from carry
    /// first, so a reorg cannot leave the same sats as both a debt and a balance.
    fn restore_debt(&mut self, identity: &str, sats: u64) {
        if sats == 0 {
            return;
        }
        let have = self.carry_of(identity);
        let from_carry = have.min(sats);
        if from_carry > 0 {
            self.adjust_carry(identity, -(from_carry.min(i64::MAX as u64) as i64));
        }
        self.accrue_debt(identity, sats - from_carry);
    }

    /// The coinbase payment this debt was for is undone. Debt still outstanding comes off;
    /// debt already collected from later earnings goes back on as carry.
    fn forgive_debt(&mut self, identity: &str, sats: u64) {
        let from_debt = self.reduce_debt(identity, sats);
        let back = sats - from_debt;
        if back > 0 {
            self.adjust_carry(identity, back.min(i64::MAX as u64) as i64);
        }
    }

    /// Put `sats` back on an identity's books: a double-pay debt is paid down first and only the
    /// rest is carry. Otherwise the same sats stand as a debt and as a balance at once, and the
    /// next coinbaser pays the balance out while the debt waits. Returns how much of `sats`
    /// paid a debt down rather than becoming carry.
    fn restore_carry(&mut self, identity: &str, sats: u64) -> u64 {
        let paid = self.reduce_debt(identity, sats);
        let rest = sats - paid;
        if rest > 0 {
            self.adjust_carry(identity, rest.min(i64::MAX as u64) as i64);
        }
        paid
    }

    /// Move an identity's carry by `delta` sats, saturating at zero. Returns the new carry.
    /// Deltas (not assignments) are what a found block applies, so two blocks found off
    /// snapshots that both predate the other's settlement still add up correctly.
    pub fn adjust_carry(&mut self, identity: &str, delta: i64) -> u64 {
        let i = self.intern(identity);
        let cur = self.carry.get(&i).copied().unwrap_or(0);
        let new = if delta >= 0 { cur.saturating_add(delta as u64) } else { cur.saturating_sub(delta.unsigned_abs()) };
        if new == 0 {
            self.carry.remove(&i);
        } else {
            self.carry.insert(i, new);
        }
        new
    }

    /// Per-identity totals, largest first. Identities with carry but no work left in the
    /// window are included (work 0) so their carry can still be paid.
    pub fn miners(&self) -> Vec<MinerStat> {
        let mut stratum: HashMap<u32, (u64, u64)> = HashMap::new();
        for c in &self.credits {
            if c.source == SOURCE_STRATUM || c.source == SOURCE_STRATUM_GRACE {
                let s = stratum.entry(c.ident).or_insert((0, 0));
                s.0 += c.work;
                if c.source == SOURCE_STRATUM_GRACE {
                    s.1 += c.work;
                }
            }
        }
        let mut v: Vec<MinerStat> = self
            .totals
            .iter()
            .map(|(&i, &(work, credits, last_ts))| MinerStat {
                identity: self.idents[i as usize].clone(),
                work,
                stratum_work: stratum.get(&i).map_or(0, |s| s.0).min(work),
                grace_work: stratum.get(&i).map_or(0, |s| s.1).min(work),
                credits,
                last_ts,
                carry: self.carry.get(&i).copied().unwrap_or(0),
            })
            .collect();
        for (&i, &carry) in &self.carry {
            if carry > 0 && !self.totals.contains_key(&i) {
                v.push(MinerStat {
                    identity: self.idents[i as usize].clone(),
                    work: 0,
                    stratum_work: 0,
                    grace_work: 0,
                    credits: 0,
                    last_ts: self.last_seen.get(&i).copied().unwrap_or(0),
                    carry,
                });
            }
        }
        v.sort_by(|a, b| {
            b.work.cmp(&a.work).then_with(|| b.carry.cmp(&a.carry)).then_with(|| a.identity.cmp(&b.identity))
        });
        v
    }

    /// Compute the coinbase split for a block worth `value` sats.
    pub fn split(
        &self,
        value: u64,
        params: &SplitParams,
        now: u32,
        script_for: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Split {
        split::compute(self.miners(), self.total_work, value, params, self.rebate_owed, now, script_for)
    }

    fn note_seen(&mut self, ident: u32, ts: u32) {
        let e = self.last_seen.entry(ident).or_insert(0);
        *e = (*e).max(ts);
    }

    /// When this identity last had work credited (0: never, or not known).
    pub fn last_seen_of(&self, identity: &str) -> u32 {
        self.ident_index.get(identity).and_then(|i| self.last_seen.get(i)).copied().unwrap_or(0)
    }

    /// Record when an identity was last seen, if that is later than what is known. For
    /// balances that predate `last_seen` (the operator backfills them from the block log).
    pub fn set_last_seen(&mut self, identity: &str, ts: u32) {
        let i = self.intern(identity);
        self.note_seen(i, ts);
    }

    /// Balances whose owners have stopped mining: no work left in the window, last credited
    /// at least `after` seconds before `now`, and holding at least `min` sats. Largest first.
    /// An identity never seen (`last_seen` 0) is not stale: nothing says how long it has been.
    pub fn stale_carries(&self, now: u32, after: u32, min: u64) -> Vec<StaleCarry> {
        let mut v: Vec<StaleCarry> = self
            .carry
            .iter()
            .filter(|(i, &sats)| sats >= min.max(1) && !self.totals.contains_key(i))
            .filter_map(|(i, &sats)| {
                let last_seen = self.last_seen.get(i).copied().unwrap_or(0);
                (last_seen > 0 && now.saturating_sub(last_seen) >= after).then(|| StaleCarry {
                    identity: self.idents[*i as usize].clone(),
                    sats,
                    last_seen,
                })
            })
            .collect();
        v.sort_by(|a, b| b.sats.cmp(&a.sats).then_with(|| a.identity.cmp(&b.identity)));
        v
    }

    /// Take up to `sats` of one identity's money out of the holds, oldest batch first.
    /// Returns what was taken. A hold left with nothing in it is dropped.
    fn draw_from_holds(&mut self, identity: &str, sats: u64) -> u64 {
        let mut left = sats;
        let mut order: Vec<(u64, String)> = self.holds.iter().map(|(k, h)| (h.created_ts, k.clone())).collect();
        order.sort();
        for (_, batch) in order {
            if left == 0 {
                break;
            }
            let Some(hold) = self.holds.get_mut(&batch) else { continue };
            for e in hold.entries.iter_mut().filter(|e| e.0 == identity) {
                let take = e.1.min(left);
                e.1 -= take;
                left -= take;
            }
            hold.entries.retain(|e| e.1 > 0);
            if hold.entries.is_empty() {
                self.holds.remove(&batch);
            }
        }
        sats - left
    }

    pub fn holds(&self) -> &std::collections::BTreeMap<String, Hold> {
        &self.holds
    }

    /// Sats set aside in holds, all batches.
    pub fn total_held(&self) -> u64 {
        self.holds.values().flat_map(|h| h.entries.iter()).fold(0u64, |a, e| a.saturating_add(e.1))
    }
}

/// A balance whose owner has stopped mining; see [`Window::stale_carries`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StaleCarry {
    pub identity: String,
    pub sats: u64,
    pub last_seen: u32,
}

/// Carry set aside to be paid by an ordinary transaction from the pool's wallet, not by a
/// coinbase. Money in a hold is off the books as far as any split is concerned, which is the
/// point: between the operator deciding to pay a balance by hand and that payment confirming,
/// no coinbase may pay it too. A hold ends one of two ways: the payment confirmed and the
/// money is gone ([`Ledger::finish_hold`]), or it was abandoned and the balances go back to
/// carry ([`Ledger::release_hold`]).
///
/// One thing can still reach into a hold: a coinbaser handed out *before* the hold was placed
/// names the balance as it then stood, and a block mined on it pays that balance. Such a block
/// takes the amount out of the hold (see [`Ledger::book_debits`]), so the hand-made payment
/// must not be built until those coinbasers can no longer be mined on: `ready_height`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub created_ts: u64,
    /// Tip height when the hold was placed.
    pub height: u32,
    /// From this tip height on, no coinbaser older than the hold can be mined on, and
    /// `entries` are final.
    pub ready_height: u32,
    pub entries: Vec<(String, u64)>,
}

/// Why a requested entry was not put in a hold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HoldSkip {
    pub identity: String,
    pub requested: u64,
    pub carry: u64,
    pub reason: &'static str,
}

/// Persisted window state.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Meta {
    target_work: u64,
    lifetime_shares: u64,
    lifetime_work: u64,
    /// Carry per identity (see [`MinerStat::carry`]). Money the pool holds for miners, so
    /// it lives in the atomically-written meta file rather than the append-only rows.
    #[serde(default)]
    carry: std::collections::BTreeMap<String, u64>,
    /// Outstanding DATUM rebate (see [`Window::rebate_owed`]). Pool money owed to miners.
    #[serde(default)]
    rebate_owed: u64,
    /// When each identity that is owed money was last credited work ([`Window::last_seen_of`]).
    /// Only identities with carry: for anyone else the window's own rows say it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    last_seen: std::collections::BTreeMap<String, u32>,
    /// Carry set aside for payouts made outside the coinbase. Money, like `carry`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    holds: std::collections::BTreeMap<String, Hold>,
    /// Double-paid carry still to collect. See [`Window::debt_of`].
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    debt: std::collections::BTreeMap<String, u64>,
    /// Block hashes whose debits are already in this window. Absent on a ledger written
    /// before that was recorded: those debits are already in the balances, and are not applied again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    applied_debits: Vec<String>,
    /// True once this file records [`Self::applied_debits`]. A file from before that field
    /// deserialises this as false.
    #[serde(default)]
    applied_debits_tracked: bool,
}

/// Durable [`Window`]: identities, credit rows, and a small meta file on disk.
pub struct Ledger {
    dir: PathBuf,
    pub window: Window,
    credits_out: BufWriter<File>,
    idents_out: BufWriter<File>,
    rows_on_disk: u64,
    dirty: bool,
    /// Block hashes whose debits this window already includes.
    applied_debits: std::collections::HashSet<String>,
    /// False on a ledger written before applied debits were recorded.
    debits_tracked: bool,
    /// Stratum grace clocks ([`grace`]). Kept in its own file: it is not money, and a build
    /// from before it must still be able to read `window.json`.
    pub grace: GraceBook,
    /// Whether `grace` was read from disk. A book that was not is built from the window.
    grace_loaded: bool,
}

impl Ledger {
    const CREDITS: &'static str = "credits.bin";
    const IDENTS: &'static str = "identities.txt";
    const META: &'static str = "window.json";
    const GRACE: &'static str = "grace.json";

    /// Open (or create) the ledger in `dir` and replay it.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut window = Window::new();

        let meta = load_meta(&dir.join(Self::META))?;
        window.lifetime_shares = meta.lifetime_shares;
        window.lifetime_work = meta.lifetime_work;
        window.rebate_owed = meta.rebate_owed;
        window.holds = meta.holds.clone();

        let idents_path = dir.join(Self::IDENTS);
        // A crash can leave half an identity at the end of the file. Appending to it would
        // glue the next identity onto that line and shift every index after it by one, so
        // that credits load against the wrong addresses; cut back to the last whole line.
        if let Ok(bytes) = fs::read(&idents_path) {
            let whole = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            if whole != bytes.len() {
                log::warn!("{}: dropping a torn last line ({} bytes)", idents_path.display(), bytes.len() - whole);
                OpenOptions::new().write(true).open(&idents_path)?.set_len(whole as u64)?;
            }
        }
        if let Ok(f) = File::open(&idents_path) {
            for line in BufReader::new(f).lines() {
                let line = line?;
                if !line.is_empty() {
                    window.intern(&line);
                }
            }
        }
        // after the identity table so a carried identity already on file keeps its index;
        // one not on file (seeded by hand) is interned and written on the next flush
        let mut new_idents = Vec::new();
        for (identity, sats) in &meta.carry {
            if !window.ident_index.contains_key(identity) {
                new_idents.push(identity.clone());
            }
            window.set_carry(identity, *sats);
        }
        for (identity, sats) in &meta.debt {
            if !window.ident_index.contains_key(identity) {
                new_idents.push(identity.clone());
            }
            window.accrue_debt(identity, *sats);
        }
        for (identity, ts) in &meta.last_seen {
            if window.ident_index.contains_key(identity) {
                window.set_last_seen(identity, *ts);
            }
        }

        let credits_path = dir.join(Self::CREDITS);
        let mut rows_on_disk = 0u64;
        let mut rows_skipped = 0u64;
        if let Ok(f) = File::open(&credits_path) {
            let len = f.metadata()?.len();
            let usable = len - len % Credit::SIZE as u64;
            if usable != len {
                // the file is appended to: a partial row left in place would put every later
                // row out of step with the row size
                log::warn!("{}: dropping a torn last row ({} bytes)", credits_path.display(), len - usable);
                OpenOptions::new().write(true).open(&credits_path)?.set_len(usable)?;
            }
            let mut buf = Vec::with_capacity(usable as usize);
            f.take(usable).read_to_end(&mut buf)?;
            for chunk in buf.as_chunks::<{ Credit::SIZE }>().0 {
                let c = Credit::read(chunk);
                if (c.ident as usize) < window.idents.len() {
                    window.push_raw(c);
                    rows_on_disk += 1;
                } else {
                    rows_skipped += 1;
                }
            }
        }
        window.set_target(meta.target_work);

        let credits_out = BufWriter::new(OpenOptions::new().create(true).append(true).open(&credits_path)?);
        let idents_out = BufWriter::new(OpenOptions::new().create(true).append(true).open(&idents_path)?);
        let (grace, grace_loaded) = load_grace(&dir.join(Self::GRACE));
        let mut l = Ledger {
            dir,
            window,
            credits_out,
            idents_out,
            rows_on_disk,
            dirty: false,
            applied_debits: meta.applied_debits.into_iter().collect(),
            debits_tracked: meta.applied_debits_tracked,
            grace,
            grace_loaded,
        };
        for identity in new_idents {
            l.idents_out.write_all(identity.as_bytes())?;
            l.idents_out.write_all(b"\n")?;
            l.dirty = true;
        }
        // A row whose identity never reached disk stays out of the window, and must leave the
        // file too: the next new identity takes that index, and the row would load as its work.
        if rows_skipped > 0 {
            log::warn!(
                "{}: {rows_skipped} rows name an identity that was never written; removing them",
                credits_path.display()
            );
        }
        if rows_skipped > 0 || l.rows_on_disk > l.window.len() as u64 + 8192 {
            l.compact()?;
        }
        Ok(l)
    }

    /// Apply a found block's carry adjustments (see [`Split::carry_delta`]) and schedule a
    /// flush. A credit pays the identity's double-pay debt down before it is carry
    /// ([`Window::debt_of`]). Returns the identities touched with their new carry.
    pub fn settle_carry(&mut self, delta: &[(String, i64)]) -> Vec<(String, u64)> {
        let mut out = Vec::with_capacity(delta.len());
        for (identity, d) in delta {
            if *d == 0 {
                continue;
            }
            let known = self.window.ident_index.contains_key(identity);
            let new = if *d > 0 {
                // restore_carry pays a double-pay debt before the rest is a balance.
                // Callers are told the new carry, not how much of the credit went to the debt.
                self.window.restore_carry(identity, d.unsigned_abs());
                self.window.carry_of(identity)
            } else {
                self.window.adjust_carry(identity, *d)
            };
            if !known {
                // adjust_carry interned it; keep the identity file in step
                let _ = self.idents_out.write_all(format!("{identity}\n").as_bytes());
            }
            out.push((identity.clone(), new));
            self.dirty = true;
        }
        out
    }

    /// Move the owed DATUM rebate (see [`Window::adjust_rebate_owed`]) and schedule a flush.
    /// Returns the new balance.
    pub fn settle_rebate(&mut self, delta: i64) -> u64 {
        if delta == 0 {
            return self.window.rebate_owed;
        }
        self.dirty = true;
        self.window.adjust_rebate_owed(delta)
    }

    /// First step of booking a found block, when the candidate is seen: take off what its
    /// coinbase paid out. Does nothing if that is already on the ledger. See [`Books`].
    pub fn book_debits(&mut self, carry_delta: &[(String, i64)], books: &mut Books) {
        if books.debits_live {
            return;
        }
        books.debited.clear();
        books.shortfall.clear();
        for (identity, d) in carry_delta.iter().filter(|d| d.1 < 0) {
            let before = self.window.carry_of(identity);
            let after = self.window.adjust_carry(identity, *d);
            let mut moved = before - after;
            // A balance short of what this coinbase paid, with the rest sitting in a hold: the
            // coinbaser was handed out before the hold was placed. The block has paid it, so
            // it comes out of the hold, or the hand-made payment would pay it a second time.
            // Booked as an ordinary debit: an orphan puts it back as carry.
            let short = d.unsigned_abs().saturating_sub(moved);
            if short > 0 {
                moved += self.window.draw_from_holds(identity, short);
            }
            if moved > 0 {
                books.debited.push((identity.clone(), moved));
            }
            // The coinbase paid carry this window no longer holds: a coinbaser issued before
            // the last find. The payee has the sats already; they come back out of later earnings.
            let overpaid = d.unsigned_abs().saturating_sub(moved);
            if overpaid > 0 {
                self.window.accrue_debt(identity, overpaid);
                books.shortfall.push((identity.clone(), overpaid));
            }
        }
        let before = self.window.rebate_owed();
        let after = self.window.adjust_rebate_owed(-(books.rebate_owed_credited.min(i64::MAX as u64) as i64));
        books.rebate_debited = before - after;
        books.debits_live = true;
        self.dirty = true;
    }

    /// Second step, once the node has the block in its main chain: put on what the block
    /// earned people. Does nothing if that is already on the ledger.
    pub fn book_credits(&mut self, carry_delta: &[(String, i64)], books: &mut Books) {
        if books.credits_live {
            return;
        }
        books.credited.clear();
        books.debt_paid.clear();
        for (identity, d) in carry_delta.iter().filter(|d| d.1 > 0) {
            let earn = d.unsigned_abs();
            let known = self.window.ident_index.contains_key(identity);
            // A double-paid carry is collected before any of this earning becomes a new balance.
            let paid_down = self.window.reduce_debt(identity, earn);
            if paid_down > 0 {
                books.debt_paid.push((identity.clone(), paid_down));
            }
            let onto = earn - paid_down;
            let before = self.window.carry_of(identity);
            let after =
                if onto > 0 { self.window.adjust_carry(identity, onto.min(i64::MAX as u64) as i64) } else { before };
            if !known {
                let _ = self.idents_out.write_all(format!("{identity}\n").as_bytes());
            }
            if after > before {
                books.credited.push((identity.clone(), after - before));
            }
        }
        let before = self.window.rebate_owed();
        let after = self.window.adjust_rebate_owed(books.rebate_deferred.min(i64::MAX as u64) as i64);
        books.rebate_added = after - before;
        books.credits_live = true;
        self.dirty = true;
    }

    /// The block is not in the chain: undo exactly what it has on the ledger. Returns what a
    /// credit could not take back because it had already been paid out (only possible for a
    /// block the node confirmed and then reorganised away).
    pub fn unbook(&mut self, books: &mut Books) -> u64 {
        self.unbook_detail(books).0
    }

    /// [`Ledger::unbook`], plus the debt each restored debit paid down. That debt is a later
    /// coinbase that paid carry this block had already taken off the books. When this block is
    /// the losing sibling, that later record's shortfall was not a second payment.
    pub fn unbook_detail(&mut self, books: &mut Books) -> (u64, Vec<(String, u64)>) {
        let mut short = 0u64;
        if books.credits_live {
            for (identity, sats) in std::mem::take(&mut books.credited) {
                let before = self.window.carry_of(&identity);
                let after = self.window.adjust_carry(&identity, -(sats.min(i64::MAX as u64) as i64));
                short = short.saturating_add(sats - (before - after));
            }
            // Earnings that had paid a double-pay debt down: that payment is undone with them.
            for (identity, sats) in std::mem::take(&mut books.debt_paid) {
                self.window.restore_debt(&identity, sats);
            }
            self.window.adjust_rebate_owed(-(books.rebate_added.min(i64::MAX as u64) as i64));
            books.rebate_added = 0;
            books.credits_live = false;
        }
        let mut debt_settled = Vec::new();
        if books.debits_live {
            // A later block on a coinbaser from before this one may have paid the same carry
            // again and left it as a debt. That payment stands, so what comes back settles the
            // debt before any of it is a balance to pay out a third time.
            for (identity, sats) in std::mem::take(&mut books.debited) {
                let paid = self.window.restore_carry(&identity, sats);
                if paid > 0 {
                    debt_settled.push((identity, paid));
                }
            }
            for (identity, sats) in std::mem::take(&mut books.shortfall) {
                self.window.forgive_debt(&identity, sats);
            }
            self.window.adjust_rebate_owed(books.rebate_debited.min(i64::MAX as u64) as i64);
            books.rebate_debited = 0;
            books.debits_live = false;
        }
        self.dirty = true;
        (short, debt_settled)
    }

    /// Set stale balances aside for a payment made by hand. Each entry is taken only if the
    /// identity is stale by the given rule *now* and holds exactly the sats named: the list
    /// was drawn up from a snapshot, and a balance that has moved since (its owner came back,
    /// or a block paid it) is not the balance the operator looked at. Returns what was held
    /// and what was not; with nothing to hold, no hold is made.
    #[allow(clippy::too_many_arguments)]
    pub fn hold_carry(
        &mut self,
        batch: &str,
        entries: &[(String, u64)],
        now: u32,
        after: u32,
        min: u64,
        tip_height: u32,
        ready_height: u32,
    ) -> Result<(Vec<(String, u64)>, Vec<HoldSkip>), &'static str> {
        if self.window.holds.contains_key(batch) {
            return Err("a hold with this id exists");
        }
        let stale: HashMap<String, u64> =
            self.window.stale_carries(now, after, min).into_iter().map(|s| (s.identity, s.sats)).collect();
        let mut held: Vec<(String, u64)> = Vec::new();
        let mut skipped = Vec::new();
        for (identity, sats) in entries {
            let carry = self.window.carry_of(identity);
            let reason = if held.iter().any(|h| &h.0 == identity) {
                Some("listed twice")
            } else if *sats == 0 {
                Some("nothing requested")
            } else {
                match stale.get(identity) {
                    None => Some("not stale"),
                    Some(have) if have != sats => Some("balance changed"),
                    Some(_) => None,
                }
            };
            match reason {
                Some(reason) => skipped.push(HoldSkip { identity: identity.clone(), requested: *sats, carry, reason }),
                None => {
                    self.window.adjust_carry(identity, -(*sats as i64));
                    held.push((identity.clone(), *sats));
                }
            }
        }
        if !held.is_empty() {
            self.window.holds.insert(
                batch.to_owned(),
                Hold { created_ts: u64::from(now), height: tip_height, ready_height, entries: held.clone() },
            );
            self.dirty = true;
        }
        Ok((held, skipped))
    }

    /// The payment was abandoned: the held balances are carry again, less any double-pay debt
    /// their owner has run up since ([`Window::debt_of`]). Returns what was held.
    pub fn release_hold(&mut self, batch: &str) -> Option<Vec<(String, u64)>> {
        let hold = self.window.holds.remove(batch)?;
        for (identity, sats) in &hold.entries {
            self.window.restore_carry(identity, *sats);
        }
        self.dirty = true;
        Some(hold.entries)
    }

    /// The payment is in the chain: the held balances are paid and leave the books. Returns them.
    pub fn finish_hold(&mut self, batch: &str) -> Option<Vec<(String, u64)>> {
        let hold = self.window.holds.remove(batch)?;
        self.dirty = true;
        Some(hold.entries)
    }

    /// Date a balance that has no last-seen (see [`Window::set_last_seen`]) and schedule a flush.
    pub fn set_last_seen(&mut self, identity: &str, ts: u32) {
        if self.window.ident_index.contains_key(identity) {
            self.window.set_last_seen(identity, ts);
            self.dirty = true;
        }
    }

    /// Set the owed DATUM rebate outright and schedule a flush.
    pub fn set_rebate_owed(&mut self, sats: u64) {
        self.window.set_rebate_owed(sats);
        self.dirty = true;
    }

    /// Set one identity's carry outright and schedule a flush.
    pub fn set_carry(&mut self, identity: &str, sats: u64) {
        let known = self.window.ident_index.contains_key(identity);
        self.window.set_carry(identity, sats);
        if !known {
            let _ = self.idents_out.write_all(identity.as_bytes()).and_then(|_| self.idents_out.write_all(b"\n"));
        }
        self.dirty = true;
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Record work as one appended row.
    ///
    /// `credits.bin` is opened O_APPEND, which on Linux ignores the file offset on write,
    /// so a row can never be patched in place: a seek-then-write silently appends a
    /// duplicate instead. The window therefore does not coalesce on this path, keeping
    /// disk rows 1:1 with window rows so a replay reproduces the window exactly.
    pub fn credit(&mut self, identity: &str, work: u64, height: u32, ts: u32, source: u8) -> io::Result<()> {
        let known = self.window.ident_index.contains_key(identity);
        let row = self.window.credit_row(identity, work, height, ts, source);
        if !known {
            // one write, so the line and its newline are not split across a buffer spill
            self.idents_out.write_all(format!("{identity}\n").as_bytes())?;
        }
        let mut b = Vec::with_capacity(Credit::SIZE);
        row.write(&mut b);
        self.credits_out.write_all(&b)?;
        self.rows_on_disk += 1;
        self.dirty = true;
        Ok(())
    }

    /// The tag for work about to be credited to `identity`: DATUM, house stratum, or house
    /// stratum inside the address's grace. Moves the grace clocks. With grace off this is the
    /// two-way choice it always was and the book is not touched.
    pub fn source_for(&mut self, identity: &str, ts: u32, house_stratum: bool, p: &GraceParams) -> u8 {
        if !p.enabled() {
            return if house_stratum { SOURCE_STRATUM } else { SOURCE_DATUM };
        }
        if !house_stratum {
            self.grace.note_datum(identity, ts);
            return SOURCE_DATUM;
        }
        if self.grace.note_stratum(identity, ts, p) {
            SOURCE_STRATUM_GRACE
        } else {
            SOURCE_STRATUM
        }
    }

    /// Build the grace book from the window if there was none on disk. Call once after
    /// opening, before any work is credited. Returns how many addresses were given a clock.
    pub fn seed_grace(&mut self, p: &GraceParams) -> Option<usize> {
        if !p.enabled() || self.grace_loaded {
            return None;
        }
        self.grace_loaded = true;
        let w = &self.window;
        let rows = w.credits.iter().map(|c| {
            let class = match c.source {
                SOURCE_STRATUM => grace::Seen::Stratum,
                SOURCE_STRATUM_GRACE => grace::Seen::StratumGrace,
                SOURCE_DATUM => grace::Seen::Datum,
                _ => grace::Seen::Other,
            };
            (c.ts, w.idents[c.ident as usize].as_str(), class)
        });
        self.grace.seed(rows, p);
        self.dirty = true;
        Some(self.grace.stratum.len())
    }

    /// Drop grace records that can no longer change an answer. Call now and then.
    pub fn prune_grace(&mut self, now: u32, p: &GraceParams) {
        if p.enabled() {
            self.grace.prune(now, p);
        }
    }

    pub fn set_target(&mut self, target_work: u64) {
        if self.window.target_work() != target_work {
            self.window.set_target(target_work);
            self.dirty = true;
        }
    }

    /// Push buffered rows to the OS and write the meta file. Call periodically.
    pub fn flush(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if self.grace.is_dirty() {
            // not money and not worth failing a flush of what is: a write that fails is
            // retried by the next flush
            match write_atomic(&self.dir.join(Self::GRACE), &serde_json::to_vec(&self.grace)?) {
                Ok(()) => self.grace.clear_dirty(),
                Err(e) => log::error!("{}: {e}", self.dir.join(Self::GRACE).display()),
            }
        }
        self.idents_out.flush()?;
        self.credits_out.flush()?;
        let meta = Meta {
            target_work: self.window.target_work(),
            lifetime_shares: self.window.lifetime_shares,
            lifetime_work: self.window.lifetime_work,
            carry: self.window.carries().into_iter().collect(),
            rebate_owed: self.window.rebate_owed,
            // for everyone owed money, on the books or in a hold (a released hold is carry again)
            last_seen: self
                .window
                .carry
                .keys()
                .map(|i| self.window.idents[*i as usize].as_str())
                .chain(self.window.holds.values().flat_map(|h| h.entries.iter().map(|e| e.0.as_str())))
                .filter_map(|identity| {
                    let ts = self.window.last_seen_of(identity);
                    (ts > 0).then(|| (identity.to_owned(), ts))
                })
                .collect(),
            holds: self.window.holds.clone(),
            debt: self.window.debts().into_iter().collect(),
            applied_debits: {
                let mut v: Vec<String> = self.applied_debits.iter().cloned().collect();
                v.sort();
                v
            },
            applied_debits_tracked: self.debits_tracked,
        };
        write_atomic(&self.dir.join(Self::META), &serde_json::to_vec_pretty(&meta)?)?;
        self.dirty = false;
        // Keep the file close to the live window. The old 2× threshold meant a full
        // window almost never compacted, so a restart replayed a different set of rows
        // than the process had been paying from.
        if self.rows_on_disk > self.window.len() as u64 + 8192 {
            self.compact()?;
        }
        Ok(())
    }

    /// Durable flush: also fsync the credit file.
    pub fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        self.credits_out.get_ref().sync_data()?;
        self.idents_out.get_ref().sync_data()
    }

    /// Whether `window.json` already records which block debits it includes.
    pub fn debits_tracked(&self) -> bool {
        self.debits_tracked
    }

    /// The debits of `hash` are in this window. Written with the next flush.
    pub fn note_debit_applied(&mut self, hash: &str) {
        if self.applied_debits.insert(hash.to_string()) || !self.debits_tracked {
            self.debits_tracked = true;
            self.dirty = true;
        }
    }

    /// A ledger from before debits were tracked: every debit already on the block log is
    /// already in the balances. Mark them applied and do not take them off again.
    pub fn adopt_legacy_debits(&mut self, blocks: &[BlockRecord]) {
        if self.debits_tracked {
            return;
        }
        for b in blocks {
            if b.books.as_ref().is_some_and(|k| k.debits_live) {
                self.applied_debits.insert(b.hash.clone());
            }
        }
        self.debits_tracked = true;
        self.dirty = true;
    }

    /// Re-apply debits that are on the block log but not in this window: a kill in the
    /// moment after the block record was appended and before `window.json` was flushed.
    /// No-op until [`Ledger::note_debit_applied`] has been used, so a first start of this
    /// code does not debit history that housekeeping already flushed.
    pub fn reconcile_debits(&mut self, blocks: &[BlockRecord]) {
        if !self.debits_tracked {
            return;
        }
        for b in blocks {
            let Some(books) = b.books.as_ref() else { continue };
            if !books.debits_live || self.applied_debits.contains(&b.hash) {
                continue;
            }
            log::error!(
                "block {} paid carry that window.json does not show as paid; taking {} debit(s) and {} shortfall(s) off the books now",
                b.hash,
                books.debited.len(),
                books.shortfall.len()
            );
            for (identity, sats) in &books.debited {
                self.window.adjust_carry(identity, -(*sats.min(&(i64::MAX as u64)) as i64));
            }
            self.window.adjust_rebate_owed(-(books.rebate_debited.min(i64::MAX as u64) as i64));
            for (identity, sats) in &books.shortfall {
                self.window.accrue_debt(identity, *sats);
            }
            self.applied_debits.insert(b.hash.clone());
            self.dirty = true;
        }
    }

    /// Make the on-disk ledger identical to the in-memory window, then fsync.
    /// Call on a timer and on graceful shutdown so a restart reloads the same shares.
    pub fn persist_window(&mut self) -> io::Result<()> {
        self.sync()?;
        if self.rows_on_disk != self.window.len() as u64 {
            self.compact()?;
        }
        Ok(())
    }

    /// Rewrite the credits file with only the rows still in the window.
    pub fn compact(&mut self) -> io::Result<()> {
        self.credits_out.flush()?;
        let tmp = self.dir.join("credits.bin.tmp");
        {
            let mut w = BufWriter::new(File::create(&tmp)?);
            let mut buf = Vec::with_capacity(Credit::SIZE * 1024);
            for c in self.window.credits() {
                c.write(&mut buf);
                if buf.len() >= Credit::SIZE * 1024 {
                    w.write_all(&buf)?;
                    buf.clear();
                }
            }
            w.write_all(&buf)?;
            w.flush()?;
            w.get_ref().sync_data()?;
        }
        fs::rename(&tmp, self.dir.join(Self::CREDITS))?;
        self.credits_out = BufWriter::new(OpenOptions::new().append(true).open(self.dir.join(Self::CREDITS))?);
        self.rows_on_disk = self.window.len() as u64;
        Ok(())
    }

    /// Import rows from another pool's JSON ledger of the form
    /// `{"credits":[{"ts":..,"identity":"..","work":..}, ...]}` so a window carries over
    /// when this Prime replaces it. Rows are appended in file order.
    pub fn import_json_credits(&mut self, path: impl AsRef<Path>) -> io::Result<usize> {
        #[derive(Deserialize)]
        struct Row {
            ts: u32,
            identity: String,
            work: u64,
            #[serde(default)]
            height: u32,
            /// `SOURCE_STRATUM` / `SOURCE_DATUM`; absent (or anything else) imports as untagged,
            /// which the split treats as DATUM. Set it when making good hosted-stratum work, so
            /// the imported work is charged the stratum fee like the rest of that miner's work.
            #[serde(default)]
            source: u8,
        }
        #[derive(Deserialize)]
        struct Doc {
            credits: Vec<Row>,
        }
        let doc: Doc =
            serde_json::from_slice(&fs::read(path)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let n = doc.credits.len();
        for r in doc.credits {
            let source = match r.source {
                SOURCE_STRATUM => SOURCE_STRATUM,
                SOURCE_DATUM => SOURCE_DATUM,
                _ => SOURCE_UNKNOWN,
            };
            self.credit(&r.identity, r.work, r.height, r.ts, source)?;
        }
        self.flush()?;
        Ok(n)
    }
}

/// Replace `path` with `data` so that a crash at any point leaves a whole file: the new one,
/// the old one, or the old one under `.bak`. The data is on disk before the rename makes it
/// the file, or a power cut can leave the name pointing at nothing.
fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    let bak = path.with_extension("json.bak");
    let _ = fs::remove_file(&bak);
    let _ = fs::hard_link(path, &bak);
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Read `grace.json`. Returns the book and whether one was read. A file that does not parse is
/// logged and treated as absent: the book is rebuilt from the window ([`GraceBook::seed`]),
/// which stops nothing. An address whose grace had run out and which has no work left in the
/// window is then unknown, and gets a new clock with its next stratum share.
fn load_grace(path: &Path) -> (GraceBook, bool) {
    match fs::read(path) {
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(book) => (book, true),
            Err(e) => {
                log::error!("{}: {e}; rebuilding the grace clocks from the window", path.display());
                (GraceBook::default(), false)
            }
        },
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                log::error!("{}: {e}; rebuilding the grace clocks from the window", path.display());
            }
            (GraceBook::default(), false)
        }
    }
}

/// Read `window.json`, which holds what the pool owes: every miner's carry and the rebate
/// balance. A file that is not there is a new ledger. One that is there and does not parse is
/// not: loading it as empty zeroes every balance, and the next flush writes the zeros over the
/// evidence. The previous flush's copy is used if it is whole; failing that, refuse to start.
fn load_meta(path: &Path) -> io::Result<Meta> {
    fn read(p: &Path) -> io::Result<Option<Meta>> {
        match fs::read(p) {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", p.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io::Error::new(e.kind(), format!("{}: {e}", p.display()))),
        }
    }
    let bak = path.with_extension("json.bak");
    match read(path) {
        Ok(Some(meta)) => Ok(meta),
        Ok(None) => match read(&bak)? {
            Some(meta) => {
                log::warn!("{} is missing; using {}", path.display(), bak.display());
                Ok(meta)
            }
            None => Ok(Meta::default()),
        },
        Err(e) => match read(&bak) {
            Ok(Some(meta)) => {
                log::error!("{e}; using the previous flush, {}", bak.display());
                Ok(meta)
            }
            _ => Err(io::Error::new(
                e.kind(),
                format!("{e}. This file holds the carry owed to miners, so it is not replaced with an empty one: restore it from a backup, or move it aside to start with no balances"),
            )),
        },
    }
}

/// A block the pool's coinbase paid (found by a gateway on this pool), for the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    pub ts: u64,
    pub height: u32,
    /// Display-order hex.
    pub hash: String,
    /// Identity that submitted the winning share, if it came through this Prime.
    pub finder: Option<String>,
    pub coinbase_value: u64,
    /// `split` (paid the TIDES split), `pool-only` (stock gateway's empty coinbase: the pool
    /// holds the whole reward and owes the window), or `unknown`.
    pub kind: String,
    /// Sats the pool owes the window for a pool-only block, after the fee.
    pub owed_sats: u64,
    /// The split that should have been (or was) paid, identity → sats.
    pub split: Vec<(String, u64)>,
    pub pool_sats: u64,
    /// Carry from earlier blocks included in this block's outputs (out of the pool's share).
    #[serde(default)]
    pub carry_paid: u64,
    /// Carry adjustments this block applied: identity → signed sats. Negative for carry paid
    /// out in this coinbase, positive for earnings it could not place. Reversed if the block
    /// is orphaned, re-applied if it comes back.
    #[serde(default)]
    pub carry_delta: Vec<(String, i64)>,
    /// DATUM rebate this block credited to DATUM identities' carry (the credits themselves
    /// are the positive rebate entries inside `carry_delta`).
    #[serde(default)]
    pub rebate_credited: u64,
    /// Adjustment this block applied to the owed DATUM rebate (see `Split::rebate_delta`):
    /// negative for owed rebate credited out by this block, positive for stratum rebate it
    /// had no DATUM miner to credit. Reversed if the block is orphaned, re-applied if it returns.
    #[serde(default)]
    pub rebate_delta: i64,
    /// Confirmed in the node's main chain.
    pub settled: bool,
    /// Outcome of this Prime's own `submitblock` (the gateway submits too): `pending`,
    /// `accepted`, `duplicate`, `no-transactions`, or `rejected: <reason>`.
    #[serde(default)]
    pub submit: String,
    /// Which gateway (identity key, hex prefix) sent the winning share.
    #[serde(default)]
    pub gateway: String,
    /// What this block has on the ledger right now, for blocks booked in two steps (see
    /// [`Books`]). `None` on records written before that: their whole `carry_delta` and
    /// `rebate_delta` went on when the candidate was seen and come off as one on an orphan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub books: Option<Books>,
    /// Carry the coinbase paid that was not on the books (`Books::shortfall`). 0 when the
    /// coinbase paid only carry the window held.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub carry_shortfall_sats: u64,
    /// Earnings this block's coinbase left in the pool's output because its coinbaser was held
    /// to the gateway's class budget (primed `class-budget`): what every identity the budget
    /// had no room for earned in this block, which `carry_delta` also credits to them. A block
    /// the gateway cut short records what it cut in `owed_sats`, and the fee wallet reserves
    /// that; this is the same money kept in carry instead, and the fee wallet must hold it back
    /// from its sweep the same way until carry has paid it out. 0, and absent from the record,
    /// for a coinbaser no class budget applied to, which is every block without the key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub carry_reserved_sats: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Move a false shortfall onto `carry_paid`.
///
/// `settled` is the debt an orphaned same-height sibling's debit paid down, and it is consumed
/// as it is applied. The shortfall was that sibling having already taken the carry off the
/// books; the chain paid it once. Returns the sats moved. Per-identity amounts come off
/// `books.shortfall` and onto `books.debited`, so a later orphan of this block puts the carry
/// back.
pub fn move_shortfall_to_paid(record: &mut BlockRecord, settled: &mut [(String, u64)]) -> u64 {
    if record.carry_shortfall_sats == 0 || settled.iter().all(|(_, sats)| *sats == 0) {
        return 0;
    }
    let mut moved = 0u64;
    if let Some(books) = record.books.as_mut() {
        let mut still = Vec::new();
        for (identity, sats) in std::mem::take(&mut books.shortfall) {
            let take = take_settled(settled, &identity, sats);
            if take > 0 {
                books.debited.push((identity.clone(), take));
                moved += take;
            }
            if sats > take {
                still.push((identity, sats - take));
            }
        }
        books.shortfall = still;
        if moved > 0 {
            books.debits_live = true;
        }
    } else {
        let mut room = record.carry_shortfall_sats;
        for (_, sats) in settled.iter_mut() {
            let take = room.min(*sats);
            *sats -= take;
            room -= take;
            moved += take;
            if room == 0 {
                break;
            }
        }
    }
    record.carry_paid = record.carry_paid.saturating_add(moved);
    record.carry_shortfall_sats = record.carry_shortfall_sats.saturating_sub(moved);
    moved
}

fn take_settled(settled: &mut [(String, u64)], identity: &str, want: u64) -> u64 {
    let mut got = 0u64;
    for (id, sats) in settled.iter_mut() {
        if id == identity && *sats > 0 && got < want {
            let take = (want - got).min(*sats);
            *sats -= take;
            got += take;
        }
    }
    got
}

/// Carry class budgets put in the pool's output that has not been paid out.
///
/// Payments take carry that was not made by a class budget first (dust deferrals, rebate), and
/// only then class carry. A confirmed block adds its `carry_reserved_sats` once those credits
/// are on the books. An unsettled block's reserve is still in its pool output, so it counts
/// too. Orphans are skipped: their booking was undone. The fee wallet holds this back, not all
/// of `carry_total`, which also holds the dust.
pub fn class_carry_unpaid(blocks: &[BlockRecord]) -> u64 {
    let mut order: Vec<&BlockRecord> = blocks.iter().filter(|b| !b.kind.starts_with("orphan")).collect();
    order.sort_by(|a, b| (a.ts, a.height, a.hash.as_str()).cmp(&(b.ts, b.height, b.hash.as_str())));
    let mut class_on_books = 0u64;
    let mut other = 0u64;
    let mut unsettled = 0u64;
    for block in order {
        let debited = match &block.books {
            Some(books) if books.debits_live => books.debited.iter().map(|d| d.1).sum(),
            Some(_) => 0,
            None => block.carry_paid,
        };
        let from_other = debited.min(other);
        other -= from_other;
        class_on_books = class_on_books.saturating_sub(debited - from_other);
        let credits_on = match &block.books {
            Some(books) => books.credits_live,
            None => true,
        };
        if credits_on {
            let landed = match &block.books {
                Some(books) => books.credited.iter().map(|c| c.1).sum(),
                None => block.carry_delta.iter().map(|d| d.1.max(0) as u64).sum(),
            };
            let class_part = block.carry_reserved_sats.min(landed);
            class_on_books = class_on_books.saturating_add(class_part);
            other = other.saturating_add(landed.saturating_sub(class_part));
        } else {
            unsettled = unsettled.saturating_add(block.carry_reserved_sats);
        }
    }
    class_on_books.saturating_add(unsettled)
}

/// A found block's effect on the ledger, booked in two steps and undone exactly.
///
/// What a block takes *off* the books goes at once, when the candidate is seen: the carry its
/// coinbase paid out and the owed DATUM rebate it credited out. The next coinbaser is computed
/// seconds later and must not hand the same carry out again, and nobody gains from a balance
/// going down, so there is nothing to wait for.
///
/// What it puts *on* the books waits until the node has the block in its main chain: earnings
/// it could not place, DATUM rebate credits, rebate with nobody to credit. A candidate is a
/// gateway's share that met its job's target; balances granted on that alone are paid out by
/// the very next block, and if the candidate then turns out to be no block at all (refused by
/// the node, or orphaned) the money has been paid for a block that never was, and taking it
/// back stops at zero.
///
/// Every amount here is what actually moved, not what was asked for (a balance can be short of
/// a debit), so [`Ledger::unbook`] puts back precisely that and no more.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Books {
    /// The two halves of `rebate_delta`: owed rebate this block credits out, and stratum
    /// rebate it had no DATUM miner to credit.
    pub rebate_owed_credited: u64,
    pub rebate_deferred: u64,
    #[serde(default)]
    pub debits_live: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debited: Vec<(String, u64)>,
    #[serde(default)]
    pub rebate_debited: u64,
    #[serde(default)]
    pub credits_live: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credited: Vec<(String, u64)>,
    #[serde(default)]
    pub rebate_added: u64,
    /// Carry the coinbase paid that was not on the books. A debt against those payees until
    /// later earnings collect it. Undone with the debits if the block is orphaned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shortfall: Vec<(String, u64)>,
    /// Of this block's credits, what paid a double-pay debt down instead of becoming carry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debt_paid: Vec<(String, u64)>,
}

impl Books {
    pub fn new(rebate_owed_credited: u64, rebate_deferred: u64) -> Books {
        Books { rebate_owed_credited, rebate_deferred, ..Books::default() }
    }
}

/// Append-only JSON-lines block log.
pub struct BlockLog {
    path: PathBuf,
}

impl BlockLog {
    pub fn open(dir: impl AsRef<Path>) -> Self {
        BlockLog { path: dir.as_ref().join("blocks.jsonl") }
    }

    pub fn append(&self, r: &BlockRecord) -> io::Result<()> {
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        let mut line = serde_json::to_vec(r)?;
        line.push(b'\n');
        f.write_all(&line)?;
        f.sync_data()
    }

    /// All records in first-seen order. A hash appearing more than once (status updates are
    /// appended, never rewritten) yields only its latest line.
    pub fn read_all(&self) -> io::Result<Vec<BlockRecord>> {
        // Bytes, not `BufRead::lines`: a line torn inside a multi-byte character (a kill or a
        // power cut mid-append; a finder's username is whatever UTF-8 the miner sent) is not
        // valid UTF-8, and that must be one bad line, not a file that cannot be read.
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e),
        };
        let mut out: Vec<BlockRecord> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut bad = 0u64;
        let mut n = 0u64;
        for line in bytes.split(|b| *b == b'\n') {
            n += 1;
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match serde_json::from_slice::<BlockRecord>(line) {
                Ok(r) => match index.get(&r.hash) {
                    Some(&i) => out[i] = r,
                    None => {
                        index.insert(r.hash.clone(), out.len());
                        out.push(r);
                    }
                },
                Err(e) => {
                    bad += 1;
                    log::error!("blocks.jsonl: line {n} does not parse and is not a block record: {e}");
                }
            }
        }
        if bad > 0 {
            log::error!("blocks.jsonl: {bad} line(s) skipped; they are not in the block log");
        }
        Ok(out)
    }
}

/// Hold the rebate credits inside `delta` to `allowed` sats.
///
/// [`split::carry_delta`] appends those credits after everything else, so they are matched
/// from the end. Returns the rebate credit still in `delta`.
pub fn cap_rebate_credits(delta: &mut Vec<(String, i64)>, planned: &[(String, u64)], allowed: u64) -> u64 {
    let planned_sum: u64 = planned.iter().map(|(_, s)| *s).sum();
    if planned_sum <= allowed {
        return planned_sum;
    }
    let mut cut = planned_sum - allowed;
    for (identity, sats) in planned.iter().rev() {
        if cut == 0 {
            break;
        }
        let Some(entry) = delta.iter_mut().rev().find(|e| e.0 == *identity && e.1 > 0) else { continue };
        let drop_n = (*sats).min(cut).min(entry.1 as u64);
        entry.1 -= drop_n as i64;
        cut -= drop_n;
    }
    delta.retain(|e| e.1 != 0);
    planned_sum - (planned_sum - allowed - cut)
}

/// Take out of the rebate credits inside `delta` the owed DATUM rebate an earlier block drew.
///
/// A coinbaser's rebate credits (`planned`) are its own block's stratum-fee rebate plus the
/// whole owed balance of the moment it was issued, `owed_credited`. A block found on it after
/// another block drew that balance finds only `owed_drawn` of it still there. The difference
/// has been credited once and comes off; the rebate this block's own fee paid for stays.
/// Returns the rebate credit still in `delta`.
pub fn cap_rebate_to_owed_drawn(
    delta: &mut Vec<(String, i64)>,
    planned: &[(String, u64)],
    owed_credited: u64,
    owed_drawn: u64,
) -> u64 {
    let planned_sum: u64 = planned.iter().map(|(_, s)| *s).sum();
    cap_rebate_credits(delta, planned, planned_sum.saturating_sub(owed_credited.saturating_sub(owed_drawn)))
}

/// Lifetime non-orphan finds per gateway signing-key prefix, recovered from the block log.
///
/// Session counters reset when primed restarts; this is what the UI should show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GatewayFinds {
    pub found: u64,
    pub last_ts: u64,
    pub last_finder: String,
}

pub fn gateway_finds(blocks: &[BlockRecord]) -> HashMap<String, GatewayFinds> {
    let mut out: HashMap<String, GatewayFinds> = HashMap::new();
    for b in blocks {
        if b.gateway.is_empty() || b.kind.starts_with("orphan") {
            continue;
        }
        let e = out.entry(b.gateway.clone()).or_default();
        e.found += 1;
        if b.ts >= e.last_ts {
            e.last_ts = b.ts;
            if let Some(f) = &b.finder {
                if !f.is_empty() {
                    e.last_finder = f.clone();
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cost of resolving each window miner to its identity index, the way `stats::build`
    /// does on every `stats.json`. Run with
    /// `cargo test --release -p tides -- --ignored --nocapture bench_index_lookup`.
    #[test]
    #[ignore]
    fn bench_index_lookup() {
        use std::time::Instant;
        let mut w = Window::new();
        // the identity table as it looks after a long uptime, plus the miners actually in
        // the window: the table keeps every identity ever seen, the window does not
        for i in 0..60_000u32 {
            w.credit(&format!("bc1qidle{i:040}"), 1, 1, 1, SOURCE_DATUM);
        }
        w.set_target(0);
        let active: Vec<String> = (0..500).map(|i| format!("bc1qactive{i:038}")).collect();
        for a in &active {
            w.credit(a, 1000, 1, 2, SOURCE_DATUM);
        }
        let mut sink = 0u64;
        let t = Instant::now();
        for a in &active {
            sink += w.identities().iter().position(|i| i == a).unwrap_or(0) as u64;
        }
        let scan = t.elapsed();
        let t = Instant::now();
        for a in &active {
            sink += w.index_of(a).unwrap_or(0) as u64;
        }
        let map = t.elapsed();
        println!("identities={} miners={} sink={sink}", w.identities().len(), active.len());
        println!("  identities().position() : {scan:?}");
        println!("  index_of()              : {map:?}");
    }

    #[test]
    fn index_of_matches_a_linear_scan() {
        let mut w = Window::new();
        for id in ["a", "b", "c"] {
            w.credit(id, 1, 1, 1, SOURCE_DATUM);
        }
        for (i, id) in w.identities().to_vec().iter().enumerate() {
            assert_eq!(w.index_of(id), Some(i as u32));
        }
        assert_eq!(w.index_of("nope"), None);
        // survives trimming: the table keeps identities whose rows have aged out
        w.set_target(1);
        assert_eq!(w.index_of("a"), Some(0));
    }

    #[test]
    fn window_trims_to_target_and_tracks_totals() {
        let mut w = Window::new();
        w.set_target(100);
        for i in 0..10 {
            w.credit("a", 20, 1, 1000 + i, SOURCE_UNKNOWN);
        }
        // 10 × 20 = 200 in, window keeps the newest rows summing to ≥ 100 → 5 rows
        assert_eq!(w.total_work(), 100);
        assert_eq!(w.len(), 5);
        w.credit("b", 5, 1, 2000, SOURCE_UNKNOWN);
        assert_eq!(w.total_work(), 105);
        w.credit("b", 5, 1, 2000, SOURCE_UNKNOWN); // coalesces with the previous row
        assert_eq!(w.len(), 6);
        assert_eq!(w.total_work(), 110);
        assert_eq!(w.work_of("b"), 10);
        assert_eq!(w.lifetime_shares, 12);
        assert_eq!(w.lifetime_work, 210);
        let m = w.miners();
        assert_eq!(m[0].identity, "a");
        assert_eq!(m[0].work, 100);
        assert_eq!(m[1].work, 10);
        // shrinking the target trims from the front
        w.set_target(20);
        assert!(w.total_work() >= 20 && w.total_work() < 40);
    }

    #[test]
    fn stratum_and_datum_work_do_not_coalesce() {
        let mut w = Window::new();
        w.set_target(10_000);
        w.credit("a", 600, 1, 100, SOURCE_DATUM);
        w.credit("a", 400, 1, 100, SOURCE_STRATUM);
        assert_eq!(w.len(), 2);
        let m = &w.miners()[0];
        assert_eq!(m.work, 1000);
        assert_eq!(m.stratum_work, 400);
    }

    #[test]
    fn ledger_persists_replays_and_compacts() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let before = {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(50);
            for i in 0..100u32 {
                l.credit(if i % 3 == 0 { "x" } else { "y" }, 1, 7, i, SOURCE_UNKNOWN).unwrap();
            }
            // two credits for the same miner in the same second: separate rows, never merged
            l.credit("y", 3, 7, 99, SOURCE_UNKNOWN).unwrap();
            l.credit("y", 4, 7, 99, SOURCE_UNKNOWN).unwrap();
            l.sync().unwrap();
            assert!(l.window.total_work() >= 50);
            (l.window.total_work(), l.window.len(), l.window.credits().copied().collect::<Vec<_>>())
        };
        let l = Ledger::open(&dir).unwrap();
        assert!(l.window.total_work() >= 50);
        assert_eq!(l.window.target_work(), 50);
        assert_eq!(l.window.lifetime_shares, 102);
        assert_eq!(l.window.lifetime_work, 107);
        assert_eq!(l.window.identities(), &["x".to_string(), "y".to_string()]);
        // the whole point: a reload lands on the same rows the process was paying from
        assert_eq!(l.window.total_work(), before.0, "reloaded window work must match");
        assert_eq!(l.window.len(), before.1, "reloaded row count must match");
        assert_eq!(l.window.credits().copied().collect::<Vec<_>>(), before.2, "rows must match");
        let tail = *l.window.credits().last().unwrap();
        assert_eq!(tail.work, 4, "the last credit is its own row, not merged into 3+4");
        assert_eq!(tail.ts, 99);
        // the file holds only what the window holds after compaction
        let mut l = l;
        l.compact().unwrap();
        let len = fs::metadata(dir.join("credits.bin")).unwrap().len();
        assert_eq!(len as usize, l.window.len() * Credit::SIZE);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Carry is money the pool holds for miners: it must survive a restart exactly, and an
    /// identity whose rows have all aged out must still be a payee for its carry.
    #[test]
    fn carry_survives_restart_and_outlives_the_window() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(100);
            l.credit("bc1qsmall", 10, 1, 100, SOURCE_DATUM).unwrap();
            l.credit("bc1qbig", 90, 1, 101, SOURCE_DATUM).unwrap();
            // a found block that could not place `small`, and one that paid `big` some carry
            let touched = l.settle_carry(&[("bc1qsmall".into(), 700), ("bc1qbig".into(), -5)]);
            assert_eq!(touched, vec![("bc1qsmall".into(), 700), ("bc1qbig".into(), 0)]);
            assert_eq!(l.settle_carry(&[("bc1qsmall".into(), 300), ("bc1qnew".into(), 42)]).len(), 2);
            assert_eq!(l.window.carry_of("bc1qsmall"), 1_000);
            assert_eq!(l.window.total_carry(), 1_042);
            // `small`'s rows age out, its carry does not
            for i in 0..20 {
                l.credit("bc1qbig", 10, 2, 200 + i, SOURCE_DATUM).unwrap();
            }
            assert_eq!(l.window.work_of("bc1qsmall"), 0);
            let m = l.window.miners();
            let small = m.iter().find(|m| m.identity == "bc1qsmall").expect("carry-only identity is listed");
            assert_eq!((small.work, small.carry), (0, 1_000));
            l.persist_window().unwrap();
        }
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.window.carry_of("bc1qsmall"), 1_000);
        assert_eq!(l.window.carry_of("bc1qnew"), 42);
        assert_eq!(l.window.carry_of("bc1qbig"), 0);
        assert_eq!(l.window.carries(), vec![("bc1qsmall".to_string(), 1_000), ("bc1qnew".to_string(), 42)]);
        // the reloaded identity table is consistent: a seeded identity was appended once
        assert_eq!(l.window.identities().iter().filter(|i| *i == "bc1qnew").count(), 1);
        // and the split pays the carry-only identity once its carry clears the floor, out
        // of the pool's fee (with a 0% fee and nothing unplaced there is no remainder to
        // pay it from, and it would simply wait for a block that has one)
        let p = SplitParams { fee_bps: 100, stratum_fee_bps: 100, min_payout: 500, ..SplitParams::default() };
        let s = l.window.split(1_000_000, &p, 0, |i| Some(i.as_bytes().to_vec()));
        let small = s.payees.iter().find(|x| x.identity == "bc1qsmall").expect("paid from carry alone");
        assert_eq!((small.sats, small.carry), (1_000, 1_000));
        assert!(s.unpaid.iter().any(|u| u.identity == "bc1qnew" && u.sats == 42));
        assert_eq!(s.pool_sats + s.paid_sats(), 1_000_000);
        assert_eq!(s.pool_sats, s.fee_sats - 1_000);
        // an orphan reverses the delta and saturates at zero rather than going negative
        l.settle_carry(&[("bc1qnew".into(), -100)]);
        assert_eq!(l.window.carry_of("bc1qnew"), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The owed DATUM rebate is pool money owed to miners: it lives in the meta file, survives
    /// a restart, is paid down by the split, and an orphan's reversal saturates at zero.
    #[test]
    fn rebate_owed_survives_restart_and_is_paid_down_by_the_split() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(1_000_000);
            assert_eq!(l.window.rebate_owed(), 0);
            // a solo block's rebate share arrives, then a found block books more
            assert_eq!(l.settle_rebate(3_125_000), 3_125_000);
            assert_eq!(l.settle_rebate(500), 3_125_500);
            l.credit("bc1qhouse", 900, 1, 100, SOURCE_STRATUM).unwrap();
            l.credit("bc1qdatum", 100, 1, 100, SOURCE_DATUM).unwrap();
            l.persist_window().unwrap();
        }
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.window.rebate_owed(), 3_125_500, "the balance came back from window.json");
        let p = SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 300,
            datum_rebate_bps: 100,
            min_payout: 1,
            ..SplitParams::default()
        };
        let s = l.window.split(1_000_000, &p, 0, |i| Some(i.as_bytes().to_vec()));
        // stratum 900k → fee 27k in the pool output; 9k of it plus the whole owed balance is
        // credited to the one DATUM miner when the block is found
        assert_eq!((s.fee_sats, s.pool_sats, s.rebate_sats), (27_000, 27_000, 9_000 + 3_125_500));
        assert_eq!(s.rebate_credits, vec![("bc1qdatum".to_string(), 3_134_500)]);
        assert_eq!(s.rebate_owed_credited, 3_125_500);
        let d = s.payees.iter().find(|x| x.identity == "bc1qdatum").unwrap();
        assert_eq!(d.sats, 100_000, "the coinbase itself pays the plain share");
        assert_eq!(s.pool_sats + s.paid_sats(), 1_000_000);
        // the block is found: the balance is cleared and the credit sits in carry
        assert_eq!(l.settle_rebate(s.rebate_delta()), 0);
        l.settle_carry(&s.carry_delta(|_| true));
        assert_eq!(l.window.carry_of("bc1qdatum"), 3_134_500);
        // orphaned: both come back
        assert_eq!(l.settle_rebate(-s.rebate_delta()), 3_125_500);
        let reverse: Vec<(String, i64)> = s.carry_delta(|_| true).iter().map(|(i, d)| (i.clone(), -d)).collect();
        l.settle_carry(&reverse);
        assert_eq!(l.window.carry_of("bc1qdatum"), 0);
        // a reversal past zero saturates
        assert_eq!(l.settle_rebate(-10_000_000), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_window_makes_restart_a_noop() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let (work, rows, miners) = {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(10_000);
            for i in 0..500u32 {
                l.credit(if i % 5 == 0 { "slow" } else { "fast" }, 40, 1, 1000 + i, SOURCE_DATUM).unwrap();
            }
            l.persist_window().unwrap();
            let miners: Vec<(String, u64)> = l.window.miners().into_iter().map(|m| (m.identity, m.work)).collect();
            let file_rows = fs::metadata(dir.join("credits.bin")).unwrap().len() as usize / Credit::SIZE;
            assert_eq!(file_rows, l.window.len(), "credits.bin must be exactly the live window");
            (l.window.total_work(), l.window.len(), miners)
        };
        let l = Ledger::open(&dir).unwrap();
        assert_eq!(l.window.total_work(), work);
        assert_eq!(l.window.len(), rows);
        let after: Vec<(String, u64)> = l.window.miners().into_iter().map(|m| (m.identity, m.work)).collect();
        assert_eq!(after, miners);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Several miners submitting in the same second interleave, so an in-memory coalesce
    /// can target a row that is no longer physically last in the file. Per-miner totals
    /// must still survive a reload byte for byte.
    #[test]
    fn interleaved_same_second_credits_reload_exactly() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let names = ["gwA", "gwB", "gwC", "gwD"];
        let before: Vec<(String, u64)> = {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(400_000);
            // same ts for a whole burst, rotating owners, so tails churn constantly
            for ts in 0..60u32 {
                for i in 0..40usize {
                    let who = names[(i * 7 + ts as usize) % names.len()];
                    l.credit(who, 512, 1, 5000 + ts, SOURCE_DATUM).unwrap();
                    // same miner twice in a row -> exercises the coalesce path
                    l.credit(who, 512, 1, 5000 + ts, SOURCE_DATUM).unwrap();
                }
            }
            l.flush().unwrap();
            let mut m: Vec<(String, u64)> = l.window.miners().into_iter().map(|x| (x.identity, x.work)).collect();
            m.sort();
            assert_eq!(m.iter().map(|x| x.1).sum::<u64>(), l.window.total_work());
            m
        };
        let l = Ledger::open(&dir).unwrap();
        let mut after: Vec<(String, u64)> = l.window.miners().into_iter().map(|x| (x.identity, x.work)).collect();
        after.sort();
        assert_eq!(after, before, "per-miner work must be identical after reload");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_legacy_json() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let json = dir.join("old.json");
        fs::write(
            &json,
            r#"{"credits":[{"ts":1,"identity":"bc1qa","work":56},{"ts":2,"identity":"1Fw8","work":8192},{"ts":3,"identity":"bc1qs","work":100,"source":1},{"ts":3,"identity":"bc1qs","work":20,"source":2}],"carry":{},"shares":2}"#,
        )
        .unwrap();
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.import_json_credits(&json).unwrap(), 4);
        assert_eq!(l.window.work_of("1Fw8"), 8192);
        assert_eq!(l.window.total_work(), 8368);
        // an explicit source survives the import: the stratum part is charged as stratum
        let m = l.window.miners();
        let s = m.iter().find(|m| m.identity == "bc1qs").unwrap();
        assert_eq!((s.work, s.stratum_work), (120, 100));
        assert_eq!(m.iter().find(|m| m.identity == "1Fw8").unwrap().stratum_work, 0, "untagged is not stratum");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn block_log_round_trip() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = BlockLog::open(&dir);
        assert!(log.read_all().unwrap().is_empty());
        let r = BlockRecord {
            ts: 1,
            height: 2,
            hash: "00".into(),
            finder: Some("bc1q".into()),
            coinbase_value: 3,
            kind: "split".into(),
            owed_sats: 0,
            split: vec![("bc1q".into(), 3)],
            pool_sats: 0,
            carry_paid: 0,
            carry_delta: vec![],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: true,
            submit: "accepted".into(),
            gateway: "ab".into(),
            books: None,
            carry_shortfall_sats: 0,
            carry_reserved_sats: 0,
        };
        log.append(&r).unwrap();
        assert_eq!(log.read_all().unwrap(), vec![r.clone()]);
        let mut r2 = r.clone();
        r2.submit = "duplicate".into();
        log.append(&r2).unwrap();
        assert_eq!(log.read_all().unwrap(), vec![r2]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A coinbase that pays carry the window no longer holds books a debt, and later earnings
    /// collect it. Orphaning that block forgives whatever debt is still outstanding.
    #[test]
    fn carry_paid_twice_is_a_debt_collected_from_later_earnings() {
        let dir = std::env::temp_dir().join(format!("tides-debt-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let mut l = Ledger::open(&dir).unwrap();
        l.set_carry("m", 100);
        let mut first = Books::new(0, 0);
        l.book_debits(&[("m".into(), -100)], &mut first);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m"), first.debited.len()), (0, 0, 1));
        let mut second = Books::new(0, 0);
        l.book_debits(&[("m".into(), -100)], &mut second);
        assert_eq!(second.debited, Vec::<(String, u64)>::new());
        assert_eq!(second.shortfall, vec![("m".into(), 100)]);
        assert_eq!(l.window.debt_of("m"), 100);
        let mut earned = Books::new(0, 0);
        l.book_credits(&[("m".into(), 40)], &mut earned);
        assert_eq!((l.window.debt_of("m"), l.window.carry_of("m")), (60, 0));
        assert_eq!(earned.debt_paid, vec![("m".into(), 40)]);
        l.unbook(&mut second);
        assert_eq!((l.window.debt_of("m"), l.window.carry_of("m")), (0, 40));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Two block-target shares on one tip: the loser takes the carry, the winner is booked as a
    /// shortfall, and orphaning the loser settles that debt. The winner's record must then say
    /// the carry was paid once.
    #[test]
    fn orphaning_a_sibling_clears_the_winners_false_shortfall() {
        let dir = std::env::temp_dir().join(format!("tides-sibling-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let mut l = Ledger::open(&dir).unwrap();
        l.set_carry("m", 100);
        let mut loser = Books::new(0, 0);
        l.book_debits(&[("m".into(), -100)], &mut loser);
        let mut winner = Books::new(0, 0);
        l.book_debits(&[("m".into(), -100)], &mut winner);
        assert_eq!(winner.shortfall, vec![("m".into(), 100)]);
        let mut record = BlockRecord {
            ts: 1,
            height: 2,
            hash: "win".into(),
            finder: None,
            coinbase_value: 100,
            kind: "split".into(),
            owed_sats: 0,
            split: vec![],
            pool_sats: 0,
            carry_paid: 0,
            carry_delta: vec![("m".into(), -100)],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: false,
            submit: "pending".into(),
            gateway: "ab".into(),
            books: Some(winner),
            carry_shortfall_sats: 100,
            carry_reserved_sats: 0,
        };
        let (_short, mut settled) = l.unbook_detail(&mut loser);
        assert_eq!(settled, vec![("m".into(), 100)]);
        assert_eq!(l.window.debt_of("m"), 0);
        assert_eq!(move_shortfall_to_paid(&mut record, &mut settled), 100);
        assert_eq!((record.carry_paid, record.carry_shortfall_sats), (100, 0));
        let books = record.books.unwrap();
        assert!(books.shortfall.is_empty());
        assert_eq!(books.debited, vec![("m".into(), 100)]);
        assert!(settled.iter().all(|(_, sats)| *sats == 0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Dust carry is not class carry. A payment takes the dust first, and the class tail stays
    /// until that is gone.
    #[test]
    fn class_carry_unpaid_excludes_dust_deferrals() {
        let dust = BlockRecord {
            ts: 1,
            height: 1,
            hash: "dust".into(),
            finder: None,
            coinbase_value: 1,
            kind: "partial".into(),
            owed_sats: 50,
            split: vec![],
            pool_sats: 0,
            carry_paid: 0,
            carry_delta: vec![("d".into(), 100)],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: true,
            submit: "accepted".into(),
            gateway: "ab".into(),
            books: None,
            carry_shortfall_sats: 0,
            carry_reserved_sats: 0,
        };
        let class = BlockRecord {
            ts: 2,
            height: 2,
            hash: "class".into(),
            finder: None,
            coinbase_value: 1,
            kind: "split".into(),
            owed_sats: 0,
            split: vec![],
            pool_sats: 50,
            carry_paid: 0,
            carry_delta: vec![("c".into(), 50)],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: true,
            submit: "accepted".into(),
            gateway: "ab".into(),
            books: None,
            carry_shortfall_sats: 0,
            carry_reserved_sats: 50,
        };
        let mut paid = class.clone();
        paid.ts = 3;
        paid.height = 3;
        paid.hash = "pay".into();
        paid.carry_paid = 80;
        paid.carry_delta = vec![("d".into(), -80)];
        paid.carry_reserved_sats = 0;
        assert_eq!(class_carry_unpaid(&[dust.clone(), class.clone()]), 50);
        // 80 of the 100 dust is paid; the 50 of class carry is untouched
        assert_eq!(class_carry_unpaid(&[dust.clone(), class.clone(), paid.clone()]), 50);
        paid.carry_paid = 120;
        paid.carry_delta = vec![("d".into(), -100), ("c".into(), -20)];
        assert_eq!(class_carry_unpaid(&[dust, class, paid]), 30);
    }

    /// A coinbaser from before the last find credits the owed rebate that find already drew.
    /// Only that comes off: the rebate the block's own fee paid for is credited whatever the
    /// owed balance has become.
    #[test]
    fn a_rebate_credit_loses_only_the_owed_balance_an_earlier_block_drew() {
        // 30 of the block's own rebate and 20 that was owed; the 20 was drawn before this block
        let mut delta = vec![("m".into(), 50i64)];
        assert_eq!(cap_rebate_to_owed_drawn(&mut delta, &[("m".into(), 50)], 20, 0), 30);
        assert_eq!(delta, vec![("m".into(), 30i64)]);
        // half of it was still owed
        let mut delta = vec![("m".into(), 50i64)];
        assert_eq!(cap_rebate_to_owed_drawn(&mut delta, &[("m".into(), 50)], 20, 10), 40);
        assert_eq!(delta, vec![("m".into(), 40i64)]);
        // all of it was: nothing comes off
        let mut delta = vec![("m".into(), 50i64), ("n".into(), 50i64)];
        assert_eq!(cap_rebate_to_owed_drawn(&mut delta, &[("m".into(), 50), ("n".into(), 50)], 20, 20), 100);
        assert_eq!(delta.iter().map(|d| d.1).sum::<i64>(), 100);
        // nothing was owed when the coinbaser was issued: the credits are all the block's own
        let mut delta = vec![("m".into(), 50i64)];
        assert_eq!(cap_rebate_to_owed_drawn(&mut delta, &[("m".into(), 50)], 0, 0), 50);
        // a deferred earning of the same identity is not a rebate credit and is left alone
        let mut delta = vec![("m".into(), 7i64), ("m".into(), 50i64), ("n".into(), 50i64)];
        assert_eq!(cap_rebate_to_owed_drawn(&mut delta, &[("m".into(), 50), ("n".into(), 50)], 60, 0), 40);
        assert_eq!(delta, vec![("m".into(), 7i64), ("m".into(), 40i64)]);
    }

    /// The review's case with a block of its own on top: the orphaned block had paid part of
    /// the balance and booked the rest as a debt, and another block's debt is outstanding too.
    /// Undoing it takes exactly its payment back, debt first.
    #[test]
    fn an_orphan_s_debit_pays_down_debt_before_it_is_carry_again() {
        let dir = std::env::temp_dir().join(format!("tides-debt-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let mut l = Ledger::open(&dir).unwrap();
        l.set_carry("m", 100);
        let (mut first, mut second, mut third) = (Books::new(0, 0), Books::new(0, 0), Books::new(0, 0));
        l.book_debits(&[("m".into(), -100)], &mut first);
        l.book_debits(&[("m".into(), -30)], &mut second);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 30));
        // 60 more is earned and confirmed, then a third coinbase pays 100 of which 30 is there
        l.book_credits(&[("m".into(), 60)], &mut Books::new(0, 0));
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (30, 0));
        l.book_debits(&[("m".into(), -100)], &mut third);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 70));
        assert_eq!((third.debited.clone(), third.shortfall.clone()), (vec![("m".into(), 30)], vec![("m".into(), 70)]));
        // owed 100 + 60, paid 100 + 30 + 100: 70 over. Without the first block, 30 is still owed.
        l.unbook(&mut first);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (30, 0));
        // and without the third either, 130
        l.unbook(&mut third);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (130, 0));
        // the first block comes back: its payment is on the books again
        l.book_debits(&[("m".into(), -100)], &mut first);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (30, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A released hold and a credit made outside the two-step books (a solo block's rebate, a
    /// record from before `Books`, the operator's) meet a debt the same way.
    #[test]
    fn a_released_hold_and_a_settled_credit_pay_down_debt_first() {
        let dir = std::env::temp_dir().join(format!("tides-debt-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let week = 7 * 86_400u32;
        let now = 10 + week;
        let mut l = Ledger::open(&dir).unwrap();
        l.set_target(100);
        l.credit("m", 1, 1, 10, SOURCE_DATUM).unwrap();
        for i in 0..200 {
            l.credit("active", 1, 2, now - 5 + (i % 5), SOURCE_DATUM).unwrap();
        }
        l.set_carry("m", 100_000);
        l.book_debits(&[("m".into(), -100_000)], &mut Books::new(0, 0));
        l.book_debits(&[("m".into(), -100_000)], &mut Books::new(0, 0));
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 100_000));

        assert_eq!(l.settle_carry(&[("m".into(), 30_000)]), vec![("m".to_string(), 0)]);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 70_000));
        // a debit is a debit, as before
        l.settle_carry(&[("m".into(), -5)]);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 70_000));

        // a balance set by hand beside the debt, held for a payment that is then abandoned
        l.set_carry("m", 50_000);
        let (held, _) = l.hold_carry("b1", &[("m".into(), 50_000)], now, week, 10_000, 500, 504).unwrap();
        assert_eq!(held, vec![("m".to_string(), 50_000)]);
        assert_eq!(l.release_hold("b1").unwrap(), vec![("m".to_string(), 50_000)]);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m"), l.window.total_held()), (0, 20_000, 0));

        // more than the debt: the rest is carry
        assert_eq!(l.settle_carry(&[("m".into(), 25_000)]), vec![("m".to_string(), 5_000)]);
        assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (5_000, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebate_credits_are_cut_from_the_end_down_to_what_is_allowed() {
        let mut delta = vec![("m".into(), 50i64)];
        assert_eq!(cap_rebate_credits(&mut delta, &[("m".into(), 50)], 0), 0);
        assert!(delta.is_empty());
        let mut delta = vec![("m".into(), 50i64), ("n".into(), 50i64)];
        assert_eq!(cap_rebate_credits(&mut delta, &[("m".into(), 50), ("n".into(), 50)], 50), 50);
        assert_eq!(delta.iter().map(|d| d.1).sum::<i64>(), 50);
    }

    /// Whatever bytes a block log holds, reading it never fails and never panics, and every
    /// whole record in it is read: garbage between records costs only the lines it is on.
    #[test]
    fn a_block_log_reads_its_whole_records_whatever_else_is_in_it() {
        let dir = std::env::temp_dir().join(format!("tides-badbytes-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = BlockLog::open(&dir);
        let good = |h: &str| {
            let mut line = serde_json::to_vec(&rec(h, "gw", "split", 1, "min\u{e9}r \u{1f980}")).unwrap();
            line.push(b'\n');
            line
        };
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..500 {
            let mut bytes = Vec::new();
            let mut want = Vec::new();
            for i in 0..(next() % 6) {
                let h = format!("{round}-{i}");
                match next() % 4 {
                    // a whole record
                    0 | 1 => {
                        bytes.extend_from_slice(&good(&h));
                        want.push(h);
                    }
                    // a record cut anywhere, mid-character included, and the line ended
                    2 => {
                        let line = good(&h);
                        let cut = next() as usize % (line.len() - 1);
                        bytes.extend_from_slice(&line[..cut]);
                        bytes.push(b'\n');
                    }
                    // bytes that are nothing at all
                    _ => {
                        bytes.extend((0..next() % 40).map(|_| next() as u8).filter(|b| *b != b'\n'));
                        bytes.push(b'\n');
                    }
                }
            }
            // and a last line torn off without its newline
            if next() % 2 == 0 {
                let line = good("torn");
                bytes.extend_from_slice(&line[..next() as usize % (line.len() - 1)]);
            }
            fs::write(dir.join("blocks.jsonl"), &bytes).unwrap();
            let got: Vec<String> = log.read_all().unwrap().into_iter().map(|r| r.hash).collect();
            assert_eq!(got, want, "round {round}: {:?}", String::from_utf8_lossy(&bytes));
        }
        // CRLF line ends and blank lines are still not bad lines
        let mut bytes = good("a");
        bytes.pop();
        bytes.extend_from_slice(b"\r\n\n  \n");
        bytes.extend_from_slice(&good("b"));
        fs::write(dir.join("blocks.jsonl"), &bytes).unwrap();
        assert_eq!(log.read_all().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bad_block_log_line_is_logged_and_not_taken_as_a_block() {
        let dir = std::env::temp_dir().join(format!("tides-badline-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = BlockLog::open(&dir);
        fs::write(dir.join("blocks.jsonl"), "{\"hash\":\"ab\",\"nope\":true}\n").unwrap();
        assert!(log.read_all().unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// `carry_reserved_sats` is new. A line written before it loads with it at 0, and a record
    /// with it at 0 writes exactly the line it always did, so `blocks.jsonl` and everything that
    /// reads it (the fee wallet, the pool site) see no change from a Prime without the key.
    #[test]
    fn carry_reserved_is_absent_from_old_lines_and_from_records_without_it() {
        let old = r#"{"ts":1,"height":2,"hash":"00","finder":"bc1q","coinbase_value":3,"kind":"partial","owed_sats":5,"split":[["bc1q",3]],"pool_sats":0,"carry_paid":0,"carry_delta":[["bc1q",4]],"rebate_credited":0,"rebate_delta":0,"settled":false,"submit":"pending","gateway":"ab"}"#;
        let r: BlockRecord = serde_json::from_str(old).unwrap();
        assert_eq!((r.kind.as_str(), r.owed_sats, r.carry_reserved_sats), ("partial", 5, 0));
        assert_eq!(serde_json::to_string(&r).unwrap(), old, "written back byte for byte");

        let capped = BlockRecord { kind: "split".into(), owed_sats: 0, carry_reserved_sats: 4, ..r };
        let line = serde_json::to_string(&capped).unwrap();
        assert!(line.ends_with(r#","carry_reserved_sats":4}"#), "{line}");
        assert_eq!(serde_json::from_str::<BlockRecord>(&line).unwrap(), capped);
        // and the log reads either kind of line
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("blocks.jsonl"), format!("{old}\n{}\n", line.replace("\"00\"", "\"01\""))).unwrap();
        let read = BlockLog::open(&dir).read_all().unwrap();
        assert_eq!(read.iter().map(|r| r.carry_reserved_sats).collect::<Vec<_>>(), vec![0, 4]);
        let _ = fs::remove_dir_all(&dir);
    }

    fn rec(hash: &str, gw: &str, kind: &str, ts: u64, finder: &str) -> BlockRecord {
        BlockRecord {
            ts,
            height: 1,
            hash: hash.into(),
            finder: Some(finder.into()),
            coinbase_value: 1,
            kind: kind.into(),
            owed_sats: 0,
            split: vec![],
            pool_sats: 0,
            carry_paid: 0,
            carry_delta: vec![],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: kind == "split",
            submit: "accepted".into(),
            gateway: gw.into(),
            books: None,
            carry_shortfall_sats: 0,
            carry_reserved_sats: 0,
        }
    }

    #[test]
    fn gateway_finds_survive_as_the_log_not_the_session() {
        let blocks = vec![
            rec("aa", "gw-a", "split", 10, "bc1qa"),
            rec("bb", "gw-a", "split", 20, "bc1qb"),
            rec("cc", "gw-b", "split", 15, "bc1qc"),
            rec("dd", "gw-a", "orphan:split", 25, "bc1qd"),
            rec("ee", "", "split", 30, "bc1qe"),
        ];
        let m = gateway_finds(&blocks);
        assert_eq!(m["gw-a"].found, 2);
        assert_eq!(m["gw-a"].last_finder, "bc1qb");
        assert_eq!(m["gw-a"].last_ts, 20);
        assert_eq!(m["gw-b"].found, 1);
        assert!(!m.contains_key(""));
        assert_eq!(m.values().map(|f| f.found).sum::<u64>(), 3);
    }

    /// `window.json` is what the pool owes. Unreadable is not the same as empty.
    #[test]
    fn a_damaged_window_file_is_never_read_as_no_balances() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.credit("alice", 40, 1, 1000, SOURCE_DATUM).unwrap();
            l.settle_carry(&[("alice".into(), 5_000)]);
            l.flush().unwrap();
            l.settle_carry(&[("alice".into(), 2_000)]);
            l.sync().unwrap();
        }
        let meta = dir.join("window.json");
        let carry = |l: &Ledger| l.window.carries().into_iter().find(|c| c.0 == "alice").map(|c| c.1);
        assert_eq!(carry(&Ledger::open(&dir).unwrap()), Some(7_000));
        // what a power cut leaves: an empty file. The previous flush is still beside it.
        fs::write(&meta, b"").unwrap();
        assert_eq!(carry(&Ledger::open(&dir).unwrap()), Some(5_000), "the previous flush, not zero");
        // gone altogether (a crash between the two renames)
        fs::remove_file(&meta).unwrap();
        assert_eq!(carry(&Ledger::open(&dir).unwrap()), Some(5_000));
        // no whole copy anywhere: refuse, and leave the evidence alone
        fs::write(&meta, b"{\"carry\": {\"alice\": 70").unwrap();
        fs::write(dir.join("window.json.bak"), b"").unwrap();
        let err = Ledger::open(&dir).err().expect("must not open with balances zeroed").to_string();
        assert!(err.contains("window.json") && err.contains("carry"), "{err}");
        assert_eq!(fs::read(&meta).unwrap(), b"{\"carry\": {\"alice\": 70");
        // a ledger that never had the file is simply new
        let fresh = dir.join("fresh");
        assert_eq!(Ledger::open(&fresh).unwrap().window.carries().len(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Both data files are appended to, so a torn tail has to be cut off before the next
    /// write or everything after it is read out of step.
    #[test]
    fn torn_file_tails_are_cut_back_before_appending() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(1_000_000);
            l.credit("alice", 40, 1, 1000, SOURCE_DATUM).unwrap();
            l.credit("bob", 60, 1, 1001, SOURCE_DATUM).unwrap();
            l.sync().unwrap();
        }
        // a crash mid-write: half a row, and half an identity with no newline
        let mut f = OpenOptions::new().append(true).open(dir.join("credits.bin")).unwrap();
        f.write_all(&[0xab; Credit::SIZE / 2]).unwrap();
        let mut f = OpenOptions::new().append(true).open(dir.join("identities.txt")).unwrap();
        f.write_all(b"car").unwrap();
        {
            let mut l = Ledger::open(&dir).unwrap();
            assert_eq!(l.window.total_work(), 100);
            l.credit("dave", 25, 1, 1002, SOURCE_DATUM).unwrap();
            l.sync().unwrap();
        }
        let l = Ledger::open(&dir).unwrap();
        let work: Vec<(String, u64)> = l.window.miners().into_iter().map(|m| (m.identity, m.work)).collect();
        for who in [("alice", 40), ("bob", 60), ("dave", 25)] {
            assert!(work.contains(&(who.0.to_string(), who.1)), "{who:?} in {work:?}");
        }
        assert_eq!(work.len(), 3, "{work:?}");
        assert_eq!(fs::metadata(dir.join("credits.bin")).unwrap().len() as usize % Credit::SIZE, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A found block is booked in two steps: what it takes off the books at once, what it puts
    /// on them only when the node has it. A candidate that never becomes a block must leave the
    /// ledger exactly as it found it, whatever happened in between.
    #[test]
    fn a_block_that_never_was_costs_the_ledger_nothing() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let mut l = Ledger::open(&dir).unwrap();
        let carry = |l: &Ledger, who: &str| l.window.carry_of(who);
        l.settle_carry(&[("alice".into(), 5_000), ("bob".into(), 300)]);
        l.set_rebate_owed(1_000);
        // the block's coinbase paid alice her 5 000 of carry, could not place carol's 700 of
        // earnings, credits dave 400 of DATUM rebate, draws 250 from the owed rebate and defers 90
        let delta: Vec<(String, i64)> = vec![("alice".into(), -5_000), ("carol".into(), 700), ("dave".into(), 400)];
        let mut books = Books::new(250, 90);

        l.book_debits(&delta, &mut books);
        assert_eq!(
            (carry(&l, "alice"), carry(&l, "carol"), carry(&l, "dave")),
            (0, 0, 0),
            "paid carry is gone at once"
        );
        assert_eq!(l.window.rebate_owed(), 750);
        l.book_debits(&delta, &mut books);
        assert_eq!(l.window.rebate_owed(), 750, "booking twice is booking once");

        // The next block is found before the node has said anything about this one, and pays
        // out every balance there is. Today carol and dave would be among them.
        assert_eq!(carry(&l, "carol") + carry(&l, "dave"), 0, "nothing to pay out for a block not yet confirmed");
        l.settle_carry(&[("bob".into(), -300)]);

        // the node refuses it: everything goes back, and only that
        assert_eq!(l.unbook(&mut books), 0);
        assert_eq!((carry(&l, "alice"), carry(&l, "bob"), carry(&l, "carol"), carry(&l, "dave")), (5_000, 0, 0, 0));
        assert_eq!(l.window.rebate_owed(), 1_000);
        assert_eq!(books, Books::new(250, 90), "nothing of it left on the ledger");
        assert_eq!(l.unbook(&mut books), 0, "undoing twice is undoing once");
        assert_eq!(carry(&l, "alice"), 5_000);

        // and if the node takes it after all (it was on a competing tip that won)
        l.book_debits(&delta, &mut books);
        l.book_credits(&delta, &mut books);
        l.book_credits(&delta, &mut books);
        assert_eq!((carry(&l, "alice"), carry(&l, "carol"), carry(&l, "dave")), (0, 700, 400));
        assert_eq!(l.window.rebate_owed(), 1_000 - 250 + 90);

        // a confirmed block reorganised away after carol was paid: what cannot come back is said
        l.settle_carry(&[("carol".into(), -700)]);
        assert_eq!(l.unbook(&mut books), 700);
        assert_eq!((carry(&l, "alice"), carry(&l, "carol"), carry(&l, "dave")), (5_000, 0, 0));
        assert_eq!(l.window.rebate_owed(), 1_000);

        // a debit larger than the balance takes what is there, and gives back what it took
        let mut short = Books::new(5_000, 0);
        l.book_debits(&[("alice".into(), -9_000)], &mut short);
        assert_eq!((carry(&l, "alice"), l.window.rebate_owed()), (0, 0));
        l.unbook(&mut short);
        assert_eq!((carry(&l, "alice"), l.window.rebate_owed()), (5_000, 1_000));

        // old records carry no books and read back as they were written
        let old: BlockRecord = serde_json::from_str(
            r#"{"ts":1,"height":2,"hash":"ab","finder":null,"coinbase_value":3,"kind":"split","owed_sats":0,"split":[],"pool_sats":0,"settled":true}"#,
        )
        .unwrap();
        assert_eq!(old.books, None);
        assert!(!serde_json::to_string(&old).unwrap().contains("books"));
        let _ = fs::remove_dir_all(&dir);
    }
    /// Last-seen survives the window forgetting an identity, and a restart.
    #[test]
    fn last_seen_outlives_the_window_and_is_persisted_for_balances() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut l = Ledger::open(&dir).unwrap();
            l.set_target(100);
            l.credit("left", 50, 1, 1_000, SOURCE_DATUM).unwrap();
            l.set_carry("left", 40_000);
            for i in 0..10 {
                l.credit("stays", 50, 2, 2_000 + i, SOURCE_DATUM).unwrap();
            }
            assert_eq!(l.window.work_of("left"), 0, "aged out");
            assert_eq!(l.window.last_seen_of("left"), 1_000);
            let m = l.window.miners().into_iter().find(|m| m.identity == "left").unwrap();
            assert_eq!((m.work, m.last_ts, m.carry), (0, 1_000, 40_000));
            l.persist_window().unwrap();
        }
        let l = Ledger::open(&dir).unwrap();
        assert_eq!(l.window.last_seen_of("left"), 1_000);
        let week = 7 * 86_400;
        assert!(l.window.stale_carries(1_000 + week - 1, week, 10_000).is_empty());
        let stale = l.window.stale_carries(1_000 + week, week, 10_000);
        assert_eq!(stale, vec![StaleCarry { identity: "left".into(), sats: 40_000, last_seen: 1_000 }]);
        assert!(l.window.stale_carries(1_000 + week, week, 40_001).is_empty(), "under the minimum");
        // a window.json written before last_seen existed still loads
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hold_takes_stale_balances_off_the_books_until_paid_or_released() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let week = 7 * 86_400u32;
        let now = 10 + week;
        let mut l = Ledger::open(&dir).unwrap();
        l.set_target(100);
        for (id, carry) in [("s1", 300_000u64), ("s2", 20_000), ("small", 5_000)] {
            l.credit(id, 1, 1, 10, SOURCE_DATUM).unwrap();
            l.set_carry(id, carry);
        }
        for i in 0..200 {
            l.credit("active", 1, 2, now - 5 + (i % 5), SOURCE_DATUM).unwrap();
        }
        l.set_carry("active", 50_000);
        let ask: Vec<(String, u64)> =
            [("s1", 300_000), ("s2", 19_999), ("small", 5_000), ("active", 50_000), ("s1", 300_000)]
                .iter()
                .map(|(i, s)| (i.to_string(), *s as u64))
                .collect();
        let (held, skipped) = l.hold_carry("b1", &ask, now, week, 10_000, 500, 504).unwrap();
        assert_eq!(held, vec![("s1".to_string(), 300_000)]);
        let why: Vec<(&str, &str)> = skipped.iter().map(|s| (s.identity.as_str(), s.reason)).collect();
        assert_eq!(
            why,
            [("s2", "balance changed"), ("small", "not stale"), ("active", "not stale"), ("s1", "listed twice")]
        );
        assert_eq!((l.window.carry_of("s1"), l.window.total_held()), (0, 300_000));
        assert!(l.hold_carry("b1", &ask, now, week, 10_000, 500, 504).is_err(), "ids are used once");
        // held money is in no split
        let p =
            SplitParams { min_payout: 500_000, stale_after: week, stale_min_payout: 10_000, ..SplitParams::default() };
        let s = l.window.split(100_000_000, &p, now, |i| Some(i.as_bytes().to_vec()));
        assert!(s.payees.iter().all(|x| x.identity != "s1"));
        // and it survives a restart
        l.persist_window().unwrap();
        drop(l);
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.window.holds()["b1"].entries, vec![("s1".to_string(), 300_000)]);
        assert_eq!(l.window.holds()["b1"].ready_height, 504);
        // released: carry again
        assert_eq!(l.release_hold("b1").unwrap(), vec![("s1".to_string(), 300_000)]);
        assert_eq!((l.window.carry_of("s1"), l.window.total_held()), (300_000, 0));
        assert!(l.release_hold("b1").is_none());
        // held again and paid: gone
        l.hold_carry("b2", &[("s1".to_string(), 300_000)], now, week, 10_000, 500, 504).unwrap();
        assert_eq!(l.finish_hold("b2").unwrap(), vec![("s1".to_string(), 300_000)]);
        assert_eq!((l.window.carry_of("s1"), l.window.total_held()), (0, 0));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A coinbaser handed out before the hold names the balance as it stood. A block mined on
    /// it has paid that balance, so it comes out of the hold; an orphan puts it back as carry.
    #[test]
    fn a_block_mined_on_an_older_coinbaser_draws_from_the_hold() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let week = 7 * 86_400u32;
        let now = 10 + week;
        let mut l = Ledger::open(&dir).unwrap();
        l.set_target(100);
        for id in ["s1", "s2"] {
            l.credit(id, 1, 1, 10, SOURCE_DATUM).unwrap();
            l.set_carry(id, 100_000);
        }
        for _ in 0..200 {
            l.credit("active", 1, 2, now, SOURCE_DATUM).unwrap();
        }
        let ask = vec![("s1".to_string(), 100_000), ("s2".to_string(), 100_000)];
        l.hold_carry("b", &ask, now, week, 10_000, 500, 504).unwrap();
        let delta = vec![("s1".to_string(), -100_000i64)];
        let mut books = Books::new(0, 0);
        l.book_debits(&delta, &mut books);
        assert_eq!(books.debited, vec![("s1".to_string(), 100_000)]);
        assert_eq!(
            l.window.holds()["b"].entries,
            vec![("s2".to_string(), 100_000)],
            "s1 is paid; only s2 is still held"
        );
        assert_eq!(l.window.carry_of("s1"), 0);
        // orphaned: the block paid nobody after all, and s1 is owed again (as carry)
        l.unbook(&mut books);
        assert_eq!(l.window.carry_of("s1"), 100_000);
        assert_eq!(l.window.total_held(), 100_000);
        // a block that pays the last entry leaves no empty hold behind
        let mut books = Books::new(0, 0);
        l.book_debits(&[("s2".to_string(), -100_000i64)], &mut books);
        assert!(l.window.holds().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn grace_tags_are_stored_with_the_row_and_the_clocks_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("tides-test-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&dir);
        let p = GraceParams { secs: 24 * 3600, datum_secs: 96 * 3600, rearm_secs: 0, epoch: 0 };
        let off = GraceParams::default();
        let t0 = 1_800_000_000u32;
        {
            let mut l = Ledger::open(&dir).unwrap();
            // grace off: the two-way tag, and no book
            assert_eq!(l.source_for("old", t0, true, &off), SOURCE_STRATUM);
            assert_eq!(l.source_for("gw", t0, false, &off), SOURCE_DATUM);
            assert_eq!(l.seed_grace(&off), None);
            l.credit("old", 100, 1, t0, SOURCE_STRATUM).unwrap();
            l.credit("gw", 300, 1, t0, SOURCE_DATUM).unwrap();
            l.sync().unwrap();
            assert!(!dir.join("grace.json").exists());
        }
        {
            // grace switched on: those in the window are given their clocks
            let mut l = Ledger::open(&dir).unwrap();
            let seeded = GraceParams { epoch: t0 - 23 * 3600, ..p };
            assert_eq!(l.seed_grace(&seeded), Some(1));
            assert_eq!(l.seed_grace(&seeded), None, "once");
            let ts = t0 + 3600;
            for (who, house, want) in [
                ("old", true, SOURCE_STRATUM), // 24 h after the epoch
                ("new", true, SOURCE_STRATUM_GRACE),
                ("gw", true, SOURCE_STRATUM_GRACE),
                ("gw", false, SOURCE_DATUM),
            ] {
                let source = l.source_for(who, ts, house, &seeded);
                assert_eq!(source, want, "{who}");
                l.credit(who, 100, 1, ts, source).unwrap();
            }
            let m: HashMap<String, MinerStat> =
                l.window.miners().into_iter().map(|m| (m.identity.clone(), m)).collect();
            assert_eq!((m["old"].work, m["old"].stratum_work, m["old"].grace_work), (200, 200, 0));
            assert_eq!((m["new"].work, m["new"].stratum_work, m["new"].grace_work), (100, 100, 100));
            assert_eq!((m["gw"].work, m["gw"].stratum_work, m["gw"].grace_work), (500, 100, 100));
            l.persist_window().unwrap();
            assert!(dir.join("grace.json").exists());
        }
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.seed_grace(&p), None, "read from disk, not rebuilt");
        let m: HashMap<String, MinerStat> = l.window.miners().into_iter().map(|m| (m.identity.clone(), m)).collect();
        assert_eq!((m["gw"].stratum_work, m["gw"].grace_work), (100, 100), "the tag is in the row");
        assert_eq!(l.grace.until("new", &p), Some(t0 + 3600 + 24 * 3600));
        assert_eq!(l.grace.until("gw", &p), Some(t0 + 3600 + 96 * 3600), "seen on DATUM");
        assert_eq!(l.source_for("new", t0 + 25 * 3600, true, &p), SOURCE_STRATUM);
        assert_eq!(l.source_for("gw", t0 + 25 * 3600, true, &p), SOURCE_STRATUM_GRACE);

        // a book that does not parse is rebuilt from the window instead of stopping the pool
        drop(l);
        fs::write(dir.join("grace.json"), b"{ not json").unwrap();
        let mut l = Ledger::open(&dir).unwrap();
        assert_eq!(l.seed_grace(&p), Some(3));
        let _ = fs::remove_dir_all(&dir);
    }
}
