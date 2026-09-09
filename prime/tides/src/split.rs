//! Turning a window into coinbase outputs.
//!
//! `distributable = value − fee`. Each identity gets `distributable × work / total_work`,
//! rounded down. Identities that cannot be paid — no valid payout script, under the dust
//! floor, or past the size budget a gateway's coinbase can hold — are dropped and their
//! amount stays with the pool, which the gateway pays automatically as the remainder.
//!
//! **Carry.** TIDES does not forfeit what a block could not place. An identity dropped for
//! being under `min_payout` (or over the size budget) has what it earned in that block
//! added to its *carry*; the carry rides on top of its earned share in every later split,
//! and is paid — out of the pool's remainder, which is where those sats went — the first
//! time earned + carry clears the floor and fits. An identity whose work has aged out of
//! the window entirely is still a payee while its carry alone clears the floor.
//!
//! **DATUM rebate.** `datum_rebate_bps` of the house-stratum fee is not kept by the pool but
//! *credited* to the window's DATUM miners, pro rata by DATUM work, as carry — the same
//! balance under-floor earnings use. The coinbase itself is unchanged in shape: stratum work
//! is charged the full fee and the pool output receives it; when the block is found, each
//! DATUM miner's slice of the rebate joins its carry and is paid, out of the pool's remainder,
//! with its next output that clears the floor. A miner too small to make a given coinbase is
//! therefore credited exactly like one that did. The rebate share of solo-block fees is
//! credited the same way when such a block lands on chain. `rebate_owed` holds rebate with
//! nobody to credit yet (a window with no payable DATUM work); it is handed out with the next
//! block that has one.

use serde::Serialize;

use crate::MinerStat;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitParams {
    /// DATUM / Prime-gateway fee in basis points (50 = 0.5%).
    pub fee_bps: u32,
    /// Public house-stratum fee in basis points (500 = 5%). 0 means same as `fee_bps`.
    pub stratum_fee_bps: u32,
    /// Share of the house-stratum fee (basis points of stratum work's value) redistributed
    /// to DATUM work. 0 disables the rebate. Clamped to the stratum fee.
    pub datum_rebate_bps: u32,
    /// Smallest output the split will emit, in sats.
    pub min_payout: u64,
    /// Cap on the number of outputs (the protocol allows 512).
    pub max_outputs: usize,
    /// Byte budget for the emitted outputs inside the coinbase. Type-4 ("huge") coinbases
    /// hold 16 KiB total; leave room for the scriptSig, the pool output and the witness
    /// commitment.
    pub output_budget_bytes: usize,
}

impl Default for SplitParams {
    fn default() -> Self {
        SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 0,
            datum_rebate_bps: 0,
            min_payout: 546,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Payee {
    pub identity: String,
    pub work: u64,
    /// The output: this block's earned share plus `carry`.
    pub sats: u64,
    /// Sats of earlier blocks' unplaced earnings included in `sats`. Cleared from the
    /// identity's carry when the block that pays this output is found.
    #[serde(default)]
    pub carry: u64,
    #[serde(with = "hex_bytes")]
    pub script: Vec<u8>,
}

/// An identity in the window (or holding carry) that this split does not pay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Unpaid {
    pub identity: String,
    /// What it would have received: earned this block plus its existing carry.
    pub sats: u64,
    /// Earned by work in this block's window alone. For `BelowMinimum` and `OverBudget`
    /// this is what joins the identity's carry when the block is found.
    pub earned: u64,
    pub reason: UnpaidReason,
}

impl Unpaid {
    /// Whether the earned share rolls forward as carry. Work under an address that cannot
    /// be paid at all (`NoScript`) does not: it has nowhere to go.
    pub fn defers(&self) -> bool {
        matches!(self.reason, UnpaidReason::BelowMinimum | UnpaidReason::OverBudget) && self.earned > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Split {
    pub value: u64,
    pub fee_sats: u64,
    pub total_work: u64,
    /// Paid outputs, largest first.
    pub payees: Vec<Payee>,
    /// Identities that will not get an output, with the sats they would have.
    pub unpaid: Vec<Unpaid>,
    /// What the pool script receives: fee plus rounding plus everything unpaid, less any
    /// carry paid out of it.
    pub pool_sats: u64,
    /// Carry from earlier blocks included in `payees` (comes out of the pool's remainder).
    pub carry_paid: u64,
    /// DATUM rebate this block credits, identity → sats: its share of the stratum fee plus any
    /// `rebate_owed`, pro rata by payable DATUM work. Joins each identity's carry when the
    /// block is found (see [`Split::carry_delta`]).
    #[serde(default)]
    pub rebate_credits: Vec<(String, u64)>,
    /// Sum of `rebate_credits`.
    #[serde(default)]
    pub rebate_sats: u64,
    /// `rebate_owed` handed out inside `rebate_credits`. Cleared from the balance when the
    /// block is found.
    #[serde(default)]
    pub rebate_owed_credited: u64,
    /// This block's stratum-fee rebate with no payable DATUM work to credit. Joins
    /// `rebate_owed` when the block is found.
    #[serde(default)]
    pub rebate_deferred: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum UnpaidReason {
    NoScript,
    BelowMinimum,
    OverBudget,
}

impl Split {
    pub fn paid_sats(&self) -> u64 {
        self.payees.iter().map(|p| p.sats).sum()
    }

    /// Carry adjustments to apply when a block paying this split is found: minus the carry
    /// each payee was handed, plus what each deferred identity earned. Identities untouched
    /// by this block do not appear.
    pub fn carry_delta(&self, was_paid: impl Fn(&Payee) -> bool) -> Vec<(String, i64)> {
        carry_delta(&self.payees, &self.unpaid, &self.rebate_credits, was_paid)
    }

    /// Adjustment to the pool's `rebate_owed` balance when a block paying this split is
    /// found: minus the owed rebate this block credited out, plus the stratum rebate it had
    /// nobody to credit. Reversed if the block is orphaned.
    pub fn rebate_delta(&self) -> i64 {
        rebate_delta(self.rebate_owed_credited, self.rebate_deferred)
    }
}

/// [`Split::rebate_delta`] for a split kept as its parts (an issued coinbaser).
pub fn rebate_delta(rebate_owed_credited: u64, rebate_deferred: u64) -> i64 {
    (rebate_deferred.min(i64::MAX as u64) as i64).saturating_sub(rebate_owed_credited.min(i64::MAX as u64) as i64)
}

/// [`Split::carry_delta`] for a split kept as its parts (an issued coinbaser). Carry paid in
/// this coinbase comes off; earnings it could not place and the DATUM rebate it credits go on.
pub fn carry_delta(
    payees: &[Payee],
    unpaid: &[Unpaid],
    rebate_credits: &[(String, u64)],
    was_paid: impl Fn(&Payee) -> bool,
) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for p in payees {
        if p.carry > 0 && was_paid(p) {
            out.push((p.identity.clone(), -(p.carry.min(i64::MAX as u64) as i64)));
        }
    }
    for u in unpaid {
        if u.defers() {
            out.push((u.identity.clone(), u.earned.min(i64::MAX as u64) as i64));
        }
    }
    for (identity, sats) in rebate_credits {
        if *sats > 0 {
            out.push((identity.clone(), sats.min(&(i64::MAX as u64)).to_owned() as i64));
        }
    }
    out
}

/// Share `pot` sats over the payable DATUM work in `miners`, pro rata, floored. Returns the
/// credits and what could not be placed (rounding, or all of it when there is no payable
/// DATUM work). Used by the split for the stratum rebate and by the solo-block observer.
pub fn rebate_credits(
    miners: &[MinerStat],
    pot: u64,
    mut script_for: impl FnMut(&str) -> Option<Vec<u8>>,
) -> (Vec<(String, u64)>, u64) {
    if pot == 0 {
        return (Vec::new(), 0);
    }
    let eligible: Vec<(usize, u64)> = miners
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let dw = m.work - m.stratum_work.min(m.work);
            (dw > 0 && script_for(&m.identity).is_some()).then_some((i, dw))
        })
        .collect();
    let total: u64 = eligible.iter().map(|e| e.1).sum();
    if total == 0 {
        return (Vec::new(), pot);
    }
    let mut credited = 0u64;
    let credits: Vec<(String, u64)> = eligible
        .into_iter()
        .filter_map(|(i, dw)| {
            let c = (u128::from(pot) * u128::from(dw) / u128::from(total)) as u64;
            credited += c;
            (c > 0).then(|| (miners[i].identity.clone(), c))
        })
        .collect();
    (credits, pot - credited)
}

pub fn fee_for(value: u64, fee_bps: u32) -> u64 {
    ((u128::from(value) * u128::from(fee_bps)) / 10_000) as u64
}

/// This block's DATUM rebate out of the stratum fee: `datum_rebate_bps` of stratum work's
/// share of `value`. Closed-form on the window totals.
fn stratum_rebate(value: u64, total_work: u64, total_sw: u64, rebate_bps: u32) -> u64 {
    if total_work == 0 || rebate_bps == 0 {
        return 0;
    }
    (u128::from(value) * u128::from(total_sw) * u128::from(rebate_bps) / u128::from(total_work) / 10_000) as u64
}

pub fn compute(
    miners: Vec<MinerStat>,
    total_work: u64,
    value: u64,
    p: &SplitParams,
    rebate_owed: u64,
    mut script_for: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Split {
    let stratum_bps = if p.stratum_fee_bps == 0 { p.fee_bps } else { p.stratum_fee_bps };
    let rebate_bps = p.datum_rebate_bps.min(stratum_bps);

    let mut total_sw = 0u64;
    let mut scripts: Vec<Option<Vec<u8>>> = Vec::with_capacity(miners.len());
    for m in &miners {
        total_sw = total_sw.saturating_add(m.stratum_work.min(m.work));
        scripts.push(if m.work > 0 || m.carry > 0 { script_for(&m.identity) } else { None });
    }

    // The rebate is not an output of this coinbase: it is what the pool will owe the DATUM
    // miners in the window once the block is found, credited as carry. Any `rebate_owed`
    // from earlier blocks with nobody to credit rides along.
    let from_fee = stratum_rebate(value, total_work, total_sw, rebate_bps);
    let (rebate_credits, undistributed) = if rebate_bps > 0 {
        let by_index: std::collections::HashMap<&str, usize> =
            miners.iter().enumerate().map(|(i, m)| (m.identity.as_str(), i)).collect();
        rebate_credits(&miners, from_fee.saturating_add(rebate_owed), |id| {
            by_index.get(id).and_then(|&i| scripts[i].clone())
        })
    } else {
        (Vec::new(), 0)
    };
    let rebate_sats: u64 = rebate_credits.iter().map(|c| c.1).sum();
    let (rebate_owed_credited, rebate_deferred) = if rebate_credits.is_empty() {
        // nobody to credit this block: the fee share waits for the next DATUM miner
        (0, from_fee)
    } else {
        // rounding dust (`undistributed`) simply stays with the pool
        let _ = undistributed;
        (rebate_owed, 0)
    };

    // Each miner's share of this block: the fee-adjusted proportional part.
    let mut fee_sats = 0u64;
    let mut earned_payable = 0u64;
    let mut shares: Vec<u64> = Vec::with_capacity(miners.len());
    for (m, script) in miners.iter().zip(&scripts) {
        let (earned, fee) = if total_work > 0 && m.work > 0 {
            let sw = m.stratum_work.min(m.work);
            let dw = m.work - sw;
            let keep =
                u128::from(sw) * u128::from(10_000 - stratum_bps) + u128::from(dw) * u128::from(10_000 - p.fee_bps);
            let earned = (u128::from(value) * keep / u128::from(total_work) / 10_000) as u64;
            let fee = (u128::from(value)
                * (u128::from(sw) * u128::from(stratum_bps) + u128::from(dw) * u128::from(p.fee_bps))
                / u128::from(total_work)
                / 10_000) as u64;
            (earned, fee)
        } else {
            (0, 0)
        };
        fee_sats = fee_sats.saturating_add(fee);
        if script.is_some() {
            earned_payable = earned_payable.saturating_add(earned);
        }
        shares.push(earned);
    }
    // Carry is paid out of the pool's remainder — the fee the pool keeps plus whatever this
    // block cannot place — so the outputs can never sum past `value`. The room is budgeted
    // against every payable miner's earned share up front, not just the ones already
    // placed: a payee's carry must not eat what a later payee has earned. A backlog larger
    // than the room is paid down over the following blocks; what does not fit stays as carry.
    let mut carry_room = value.saturating_sub(earned_payable);

    let mut payees = Vec::new();
    let mut unpaid = Vec::new();
    let mut paid = 0u64;
    let mut carry_paid = 0u64;
    let mut bytes = 0usize;
    for ((m, script), earned) in miners.into_iter().zip(scripts).zip(shares) {
        let total = earned.saturating_add(m.carry);
        if total == 0 {
            continue;
        }
        // an identity this block does not place leaves its earned share with the pool, which
        // is then room to pay someone else's carry from
        if total < p.min_payout {
            if script.is_some() {
                carry_room = carry_room.saturating_add(earned);
            }
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::BelowMinimum });
            continue;
        }
        let Some(script) = script else {
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::NoScript });
            continue;
        };
        let need = 8 + 1 + script.len();
        if payees.len() >= p.max_outputs || bytes + need > p.output_budget_bytes {
            carry_room = carry_room.saturating_add(earned);
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::OverBudget });
            continue;
        }
        let carry = m.carry.min(carry_room);
        let sats = earned + carry;
        if sats < p.min_payout {
            carry_room = carry_room.saturating_add(earned);
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::OverBudget });
            continue;
        }
        carry_room -= carry;
        bytes += need;
        paid += sats;
        carry_paid += carry;
        payees.push(Payee { identity: m.identity, work: m.work, sats, carry, script });
    }
    // `paid` cannot exceed `value` (every payee's earned share is budgeted, and carry only
    // ever fills the room left beside them), but the pool's remainder must never wrap to a
    // 2^64 output if that invariant is ever broken upstream.
    Split {
        value,
        fee_sats,
        total_work,
        payees,
        unpaid,
        pool_sats: value.saturating_sub(paid),
        carry_paid,
        rebate_credits,
        rebate_sats,
        rebate_owed_credited,
        rebate_deferred,
    }
}

mod hex_bytes {
    use serde::Serializer;
    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b.iter().map(|x| format!("{x:02x}")).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn miner(id: &str, work: u64) -> MinerStat {
        MinerStat { identity: id.into(), work, stratum_work: 0, credits: 1, last_ts: 0, carry: 0 }
    }

    fn miner_stratum(id: &str, work: u64) -> MinerStat {
        MinerStat { identity: id.into(), work, stratum_work: work, credits: 1, last_ts: 0, carry: 0 }
    }

    fn miner_carry(id: &str, work: u64, carry: u64) -> MinerStat {
        MinerStat { identity: id.into(), work, stratum_work: 0, credits: 1, last_ts: 0, carry }
    }

    fn script(id: &str) -> Option<Vec<u8>> {
        if id.starts_with("bad") {
            None
        } else {
            Some(vec![0x00, 0x14, id.as_bytes()[0]])
        }
    }

    #[test]
    fn proportional_with_fee_and_floor() {
        let miners = vec![miner("a", 600), miner("b", 300), miner("c", 100), miner("d", 1)];
        // one unit of work is worth ~310k sats here; a 400k floor drops only `d`
        let p = SplitParams {
            fee_bps: 50,
            stratum_fee_bps: 50,
            datum_rebate_bps: 0,
            min_payout: 400_000,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(miners, 1001, 312_538_966, &p, 0, script);
        assert_eq!(s.fee_sats, 1_562_694);
        let dist = 312_538_966 - 1_562_694;
        assert_eq!(s.payees.len(), 3);
        assert_eq!(s.payees[0].sats, dist * 600 / 1001);
        assert_eq!(s.payees[1].sats, dist * 300 / 1001);
        assert_eq!(s.payees[2].sats, dist * 100 / 1001);
        assert_eq!(s.unpaid.len(), 1);
        assert_eq!(s.unpaid[0].reason, UnpaidReason::BelowMinimum);
        assert_eq!(s.pool_sats + s.paid_sats(), 312_538_966);
        assert!(s.pool_sats >= s.fee_sats + s.unpaid[0].sats);
        // what `d` earned is not forfeit: it is the carry delta for when this block is found
        assert_eq!(s.unpaid[0].earned, dist / 1001);
        assert_eq!(s.carry_delta(|_| true), vec![("d".to_string(), (dist / 1001) as i64)]);
        assert_eq!(s.carry_paid, 0);
    }

    /// The whole point of carry: work under the floor accumulates and is paid once it
    /// clears the floor, out of the pool's remainder, with outputs still summing to value.
    #[test]
    fn carry_accrues_then_pays_out_of_the_pool_remainder() {
        let p = SplitParams {
            fee_bps: 100,
            stratum_fee_bps: 100,
            datum_rebate_bps: 0,
            min_payout: 4_000,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        // block 1: `small` earns 3 960 (< floor) and is deferred
        let s1 = compute(vec![miner("big", 9_600), miner("small", 400)], 10_000, 100_000, &p, 0, script);
        assert_eq!(s1.payees.len(), 1);
        assert_eq!(s1.unpaid[0].identity, "small");
        assert_eq!(s1.unpaid[0].sats, 3_960);
        assert_eq!(s1.fee_sats, 1_000);
        assert_eq!(s1.pool_sats, 4_960, "fee plus the deferred sats sit with the pool for now");
        assert_eq!(s1.carry_delta(|_| true), vec![("small".to_string(), 3_960)]);
        // block 2: same work, now carrying 3 960 → 7 920 total clears the floor. The pool's
        // remainder this block is only its 1 000 fee, so that is how much carry it can pay
        // down now; the rest stays owed.
        let s2 = compute(vec![miner("big", 9_600), miner_carry("small", 400, 3_960)], 10_000, 100_000, &p, 0, script);
        assert_eq!(s2.payees.len(), 2);
        let small = s2.payees.iter().find(|x| x.identity == "small").unwrap();
        assert_eq!((small.sats, small.carry), (4_960, 1_000));
        assert_eq!(s2.carry_paid, 1_000);
        assert_eq!(s2.pool_sats, 0, "the carry came out of the pool's remainder");
        assert_eq!(s2.pool_sats + s2.paid_sats(), 100_000);
        assert_eq!(s2.carry_delta(|_| true), vec![("small".to_string(), -1_000)]);
        // a payee whose output the gateway did not actually place keeps its carry
        assert!(s2.carry_delta(|_| false).is_empty());
        // block 3: a bigger remainder (fee 1 000 + an unpayable miner's 990) pays more down
        let s3 = compute(
            vec![miner("big", 9_500), miner_carry("small", 400, 2_960), miner("bad", 100)],
            10_000,
            100_000,
            &p,
            0,
            script,
        );
        let small = s3.payees.iter().find(|x| x.identity == "small").unwrap();
        assert_eq!((small.sats, small.carry), (3_960 + 1_990, 1_990));
        assert_eq!(s3.pool_sats + s3.paid_sats(), 100_000);
        assert_eq!(s3.pool_sats, 0);
    }

    /// Carry alone can make an identity a payee after its work has left the window.
    #[test]
    fn carry_only_identity_is_paid_once_it_clears_the_floor() {
        let p = SplitParams {
            fee_bps: 100,
            stratum_fee_bps: 100,
            datum_rebate_bps: 0,
            min_payout: 5_000,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(vec![miner("a", 100), miner_carry("gone", 0, 4_999)], 100, 1_000_000, &p, 0, script);
        assert_eq!(s.payees.len(), 1, "4 999 is still under the floor");
        assert_eq!(s.unpaid[0].identity, "gone");
        assert_eq!((s.unpaid[0].sats, s.unpaid[0].earned), (4_999, 0));
        assert!(s.carry_delta(|_| true).is_empty(), "nothing earned, nothing to add");
        let s = compute(vec![miner("a", 100), miner_carry("gone", 0, 5_000)], 100, 1_000_000, &p, 0, script);
        assert_eq!(s.payees.len(), 2);
        let gone = s.payees.iter().find(|x| x.identity == "gone").unwrap();
        assert_eq!((gone.sats, gone.carry, gone.work), (5_000, 5_000, 0));
        assert_eq!(s.pool_sats, 10_000 - 5_000, "fee less the carry paid");
        assert_eq!(s.fee_sats, 10_000);
    }

    /// A carry backlog bigger than the pool's remainder is paid down over blocks, never by
    /// minting outputs past the template value.
    #[test]
    fn carry_never_pushes_outputs_past_value() {
        let p = SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 0,
            datum_rebate_bps: 0,
            min_payout: 1,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(vec![miner("a", 50), miner_carry("b", 50, 1_000_000)], 100, 1_000, &p, 0, script);
        assert_eq!(s.pool_sats + s.paid_sats(), 1_000);
        assert_eq!(s.pool_sats, 0);
        let b = s.payees.iter().find(|x| x.identity == "b").unwrap();
        assert_eq!(b.sats, 500, "earned 500, no room for any carry");
        assert_eq!(b.carry, 0);
        assert!(s.carry_delta(|_| true).is_empty(), "carry untouched, still owed in full");
        // unpayable work does not accrue: there is no address to ever pay it to
        let s = compute(vec![miner("a", 50), miner("bad", 50)], 100, 1_000, &p, 0, script);
        assert_eq!(s.unpaid[0].reason, UnpaidReason::NoScript);
        assert!(!s.unpaid[0].defers());
    }

    /// Carry is budgeted against every payee's earned share, not just the ones placed so
    /// far: a big carry on an early payee must leave later payees their full earnings.
    #[test]
    fn carry_on_an_early_payee_cannot_eat_a_later_payees_share() {
        let p = SplitParams { fee_bps: 100, stratum_fee_bps: 100, min_payout: 1, ..SplitParams::default() };
        let s = compute(vec![miner_carry("a", 60, 1_000_000), miner("b", 40)], 100, 100_000, &p, 0, script);
        let a = s.payees.iter().find(|x| x.identity == "a").unwrap();
        let b = s.payees.iter().find(|x| x.identity == "b").unwrap();
        assert_eq!(b.sats, 39_600, "b gets its full earned share");
        assert_eq!((a.sats, a.carry), (59_400 + 1_000, 1_000), "a's carry only fills the pool's 1% remainder");
        assert_eq!(s.pool_sats, 0);
        assert_eq!(s.pool_sats + s.paid_sats(), 100_000);
    }

    #[test]
    fn unpayable_and_budget() {
        let miners: Vec<MinerStat> =
            (0..20).map(|i| miner(&format!("{}{}", if i == 3 { "bad" } else { "m" }, i), 100)).collect();
        let p = SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 0,
            datum_rebate_bps: 0,
            min_payout: 1,
            max_outputs: 5,
            output_budget_bytes: 14_000,
        };
        let s = compute(miners, 2000, 1_000_000, &p, 0, script);
        assert_eq!(s.payees.len(), 5);
        assert!(s.unpaid.iter().any(|u| u.reason == UnpaidReason::NoScript));
        assert_eq!(s.unpaid.iter().filter(|u| u.reason == UnpaidReason::OverBudget).count(), 14);
        // over-budget work rolls forward too: it was earned, the coinbase just had no room
        assert_eq!(s.carry_delta(|_| true).len(), 14);
        let p = SplitParams {
            fee_bps: 0,
            stratum_fee_bps: 0,
            datum_rebate_bps: 0,
            min_payout: 1,
            max_outputs: 512,
            output_budget_bytes: 12 * 2,
        };
        let s = compute(vec![miner("a", 1), miner("b", 1), miner("c", 1)], 3, 300, &p, 0, script);
        assert_eq!(s.payees.len(), 2);
        assert_eq!(s.pool_sats, 100);
    }

    #[test]
    fn empty_window_pays_the_pool() {
        let s = compute(vec![], 0, 100, &SplitParams::default(), 0, script);
        assert!(s.payees.is_empty());
        assert_eq!(s.pool_sats, 100);
    }

    #[test]
    fn stratum_pays_a_higher_fee_than_datum() {
        let miners = vec![miner("datum", 600), miner_stratum("house", 400)];
        let p = SplitParams {
            fee_bps: 50,
            stratum_fee_bps: 500,
            datum_rebate_bps: 0,
            min_payout: 1,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(miners, 1000, 10_000_000, &p, 0, script);
        assert_eq!(s.payees[0].identity, "datum");
        assert_eq!(s.payees[0].sats, 5_970_000);
        assert_eq!(s.payees[1].identity, "house");
        assert_eq!(s.payees[1].sats, 3_800_000);
        assert_eq!(s.fee_sats, 230_000);
        assert_eq!(s.pool_sats + s.paid_sats(), 10_000_000);
        assert_eq!((s.rebate_sats, s.rebate_owed_credited, s.rebate_deferred), (0, 0, 0), "no rebate configured");
    }

    /// The production shape: DATUM free, stratum 3%, one point of which goes to DATUM work.
    fn rebate_params() -> SplitParams {
        SplitParams { fee_bps: 0, stratum_fee_bps: 300, datum_rebate_bps: 100, min_payout: 1, ..SplitParams::default() }
    }

    #[test]
    fn stratum_rebate_is_credited_to_datum_work_not_paid_in_the_coinbase() {
        // 60% of the window is stratum, 40% DATUM (two miners, 3:1)
        let miners = vec![miner_stratum("house", 600), miner("d1", 300), miner("d2", 100)];
        let value = 100_000_000u64;
        let s = compute(miners, 1000, value, &rebate_params(), 0, script);
        // the coinbase is the plain 3% split: stratum charged 1.8M, DATUM paid its share
        assert_eq!(s.fee_sats, 1_800_000);
        let by = |id: &str| s.payees.iter().find(|p| p.identity == id).unwrap();
        assert_eq!(by("house").sats, 58_200_000);
        assert_eq!(by("d1").sats, 30_000_000);
        assert_eq!(by("d2").sats, 10_000_000);
        assert_eq!(s.pool_sats, 1_800_000, "the pool output carries the whole fee for now");
        assert_eq!(s.pool_sats + s.paid_sats(), value);
        // one point of stratum work's value (600k) is credited 3:1 to the DATUM miners
        assert_eq!(s.rebate_sats, 600_000);
        assert_eq!(s.rebate_credits, vec![("d1".to_string(), 450_000), ("d2".to_string(), 150_000)]);
        assert_eq!((s.rebate_owed_credited, s.rebate_deferred, s.rebate_delta()), (0, 0, 0));
        // …as carry, when the block is found
        assert_eq!(s.carry_delta(|_| true), vec![("d1".to_string(), 450_000), ("d2".to_string(), 150_000)]);
        // the next block pays that carry out of the pool's remainder (1.8M ≥ 600k)
        let next = compute(
            vec![miner_stratum("house", 600), miner_carry("d1", 300, 450_000), miner_carry("d2", 100, 150_000)],
            1000,
            value,
            &rebate_params(),
            0,
            script,
        );
        let by = |id: &str| next.payees.iter().find(|p| p.identity == id).unwrap();
        assert_eq!((by("d1").sats, by("d1").carry), (30_450_000, 450_000));
        assert_eq!((by("d2").sats, by("d2").carry), (10_150_000, 150_000));
        assert_eq!(next.pool_sats, 1_200_000, "3% charged, 1% paid back out: the pool nets 2%");
        assert_eq!(next.pool_sats + next.paid_sats(), value);
    }

    /// The point of crediting rather than paying: a DATUM miner too small for this coinbase
    /// is credited its slice all the same, and it accumulates until it clears the floor.
    #[test]
    fn a_datum_miner_under_the_floor_still_earns_its_rebate() {
        let p = SplitParams { min_payout: 50_000, ..rebate_params() };
        let value = 100_000_000u64;
        // tiny earns 10k (< 50k floor) — but 1% of 99.99% of the value is 999,900 of rebate
        let s = compute(vec![miner_stratum("house", 9_999), miner("tiny", 1)], 10_000, value, &p, 0, script);
        assert_eq!(s.payees.len(), 1);
        assert_eq!(s.unpaid[0].identity, "tiny");
        assert_eq!(s.unpaid[0].earned, 10_000);
        assert_eq!(s.rebate_credits, vec![("tiny".to_string(), 999_900)]);
        // both the deferred earnings and the rebate join its carry
        let delta = s.carry_delta(|_| true);
        assert_eq!(delta, vec![("tiny".to_string(), 10_000), ("tiny".to_string(), 999_900)]);
        // next block: 10k earned + 1,009,900 carry clears the floor and is paid
        let s = compute(
            vec![miner_stratum("house", 9_999), miner_carry("tiny", 1, 1_009_900)],
            10_000,
            value,
            &p,
            0,
            script,
        );
        let tiny = s.payees.iter().find(|x| x.identity == "tiny").unwrap();
        assert_eq!((tiny.sats, tiny.carry), (1_019_900, 1_009_900));
        assert_eq!(s.pool_sats + s.paid_sats(), value);
    }

    #[test]
    fn rebate_is_clamped_to_the_stratum_fee_and_off_when_zero() {
        let p = SplitParams { datum_rebate_bps: 900, ..rebate_params() };
        let s = compute(vec![miner_stratum("house", 500), miner("d", 500)], 1000, 1_000_000, &p, 0, script);
        // 3% of 500k charged; the credit is capped at that same 3%
        assert_eq!((s.fee_sats, s.rebate_sats, s.pool_sats), (15_000, 15_000, 15_000));
        let p = SplitParams { datum_rebate_bps: 0, ..rebate_params() };
        let s = compute(vec![miner_stratum("house", 500), miner("d", 500)], 1000, 1_000_000, &p, 0, script);
        assert!(s.rebate_credits.is_empty());
        assert_eq!((s.fee_sats, s.rebate_sats, s.rebate_deferred), (15_000, 0, 0));
    }

    #[test]
    fn rebate_with_no_datum_work_is_deferred_and_handed_out_later() {
        let p = rebate_params();
        let s = compute(vec![miner_stratum("a", 700), miner_stratum("b", 300)], 1000, 1_000_000, &p, 0, script);
        assert_eq!(s.fee_sats, 30_000);
        assert!(s.rebate_credits.is_empty());
        assert_eq!(s.rebate_deferred, 10_000, "1% of an all-stratum block waits for a DATUM miner");
        assert_eq!(s.rebate_delta(), 10_000);
        assert!(s.carry_delta(|_| true).is_empty());
        // DATUM work under an unpayable address is not somewhere to send it either
        let s = compute(vec![miner_stratum("a", 900), miner("bad", 100)], 1000, 1_000_000, &p, 0, script);
        assert_eq!((s.rebate_sats, s.rebate_deferred), (0, 9_000));
        // the next block with a DATUM miner hands the balance out on top of its own point
        let s = compute(vec![miner_stratum("a", 900), miner("d", 100)], 1000, 1_000_000, &p, 19_000, script);
        assert_eq!(s.rebate_credits, vec![("d".to_string(), 9_000 + 19_000)]);
        assert_eq!((s.rebate_owed_credited, s.rebate_deferred, s.rebate_delta()), (19_000, 0, -19_000));
    }

    /// Outputs never exceed `value`, and the credits always equal the pot less rounding dust.
    #[test]
    fn rebate_conservation_fuzz() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..2_000 {
            let n = 1 + rnd(40) as usize;
            let mut miners = Vec::new();
            for i in 0..n {
                let work = 1 + rnd(1_000_000);
                let sw = match rnd(3) {
                    0 => 0,
                    1 => work,
                    _ => rnd(work + 1),
                };
                let id = if rnd(10) == 0 { format!("bad{i}") } else { format!("m{i}") };
                miners.push(MinerStat {
                    identity: id,
                    work,
                    stratum_work: sw,
                    credits: 1,
                    last_ts: 0,
                    carry: rnd(50_000),
                });
            }
            let total: u64 = miners.iter().map(|m| m.work).sum();
            let value = 100_000 + rnd(400_000_000);
            let stratum = rnd(1_000) as u32;
            let p = SplitParams {
                fee_bps: rnd(u64::from(stratum) + 1) as u32,
                stratum_fee_bps: stratum,
                datum_rebate_bps: rnd(1_200) as u32,
                min_payout: 1 + rnd(20_000),
                max_outputs: 1 + rnd(40) as usize,
                output_budget_bytes: 14_000,
            };
            let owed = rnd(50_000_000);
            let dbg = format!("{miners:?} total={total} value={value} p={p:?} owed={owed}");
            let s = compute(miners.clone(), total, value, &p, owed, script);
            assert_eq!(s.pool_sats + s.paid_sats(), value, "{dbg}\n{s:?}");
            // the summed per-miner fee floors can trail the closed-form fee by a sat each
            assert!(s.rebate_sats <= s.fee_sats + owed + n as u64, "{dbg}\n{s:?}");
            assert!(s.rebate_deferred == 0 || s.rebate_sats == 0, "a block either credits the rebate or defers it");
            if !s.rebate_credits.is_empty() {
                assert_eq!(s.rebate_owed_credited, owed, "owed goes out whole once someone can take it");
                assert!(s.rebate_sats + n as u64 >= s.rebate_deferred + s.rebate_owed_credited);
                assert!(s.rebate_credits.iter().all(|(id, c)| {
                    let m = miners.iter().find(|m| &m.identity == id).unwrap();
                    *c > 0 && m.work > m.stratum_work.min(m.work) && !id.starts_with("bad")
                }));
            }
            assert!(s.payees.iter().all(|x| x.sats >= x.carry));
            assert!(s.payees.iter().all(|x| x.sats >= p.min_payout));
        }
    }
}
