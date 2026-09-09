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

use serde::Serialize;

use crate::MinerStat;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitParams {
    /// DATUM / Prime-gateway fee in basis points (50 = 0.5%).
    pub fee_bps: u32,
    /// Public house-stratum fee in basis points (500 = 5%). 0 means same as `fee_bps`.
    pub stratum_fee_bps: u32,
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
        carry_delta(&self.payees, &self.unpaid, was_paid)
    }
}

/// [`Split::carry_delta`] for a split kept as its parts (an issued coinbaser).
pub fn carry_delta(payees: &[Payee], unpaid: &[Unpaid], was_paid: impl Fn(&Payee) -> bool) -> Vec<(String, i64)> {
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
    out
}

pub fn fee_for(value: u64, fee_bps: u32) -> u64 {
    ((u128::from(value) * u128::from(fee_bps)) / 10_000) as u64
}

pub fn compute(
    miners: Vec<MinerStat>,
    total_work: u64,
    value: u64,
    p: &SplitParams,
    mut script_for: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Split {
    let stratum_bps = if p.stratum_fee_bps == 0 { p.fee_bps } else { p.stratum_fee_bps };
    let mut fee_sats = 0u64;
    let mut payees = Vec::new();
    let mut unpaid = Vec::new();
    let mut paid = 0u64;
    let mut carry_paid = 0u64;
    let mut bytes = 0usize;
    for m in miners {
        let (earned, fee) = if total_work > 0 && m.work > 0 {
            let sw = m.stratum_work.min(m.work);
            let dw = m.work - sw;
            let keep = u128::from(sw) * u128::from(10_000 - stratum_bps)
                + u128::from(dw) * u128::from(10_000 - p.fee_bps);
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
        let total = earned.saturating_add(m.carry);
        if total == 0 {
            continue;
        }
        if total < p.min_payout {
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::BelowMinimum });
            continue;
        }
        let Some(script) = script_for(&m.identity) else {
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::NoScript });
            continue;
        };
        let need = 8 + 1 + script.len();
        if payees.len() >= p.max_outputs || bytes + need > p.output_budget_bytes {
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::OverBudget });
            continue;
        }
        // Carry is paid out of the pool's remainder — the fee plus whatever this block could
        // not place — so the outputs can never sum past `value`. A backlog larger than that
        // room is paid down over the following blocks; what does not fit stays as carry.
        let room = value.saturating_sub(paid.saturating_add(earned));
        let carry = m.carry.min(room);
        let sats = earned + carry;
        if sats < p.min_payout {
            unpaid.push(Unpaid { identity: m.identity, sats: total, earned, reason: UnpaidReason::OverBudget });
            continue;
        }
        bytes += need;
        paid += sats;
        carry_paid += carry;
        payees.push(Payee { identity: m.identity, work: m.work, sats, carry, script });
    }
    // `paid` cannot exceed `value` (each payee is a proper fraction of it plus carry capped
    // to the room left), but the pool's remainder must never wrap to a 2^64 output if that
    // invariant is ever broken upstream.
    Split { value, fee_sats, total_work, payees, unpaid, pool_sats: value.saturating_sub(paid), carry_paid }
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
            min_payout: 400_000,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(miners, 1001, 312_538_966, &p, script);
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
        let p = SplitParams { fee_bps: 100, stratum_fee_bps: 100, min_payout: 4_000, max_outputs: 512, output_budget_bytes: 14_000 };
        // block 1: `small` earns 3 960 (< floor) and is deferred
        let s1 = compute(vec![miner("big", 9_600), miner("small", 400)], 10_000, 100_000, &p, script);
        assert_eq!(s1.payees.len(), 1);
        assert_eq!(s1.unpaid[0].identity, "small");
        assert_eq!(s1.unpaid[0].sats, 3_960);
        assert_eq!(s1.fee_sats, 1_000);
        assert_eq!(s1.pool_sats, 4_960, "fee plus the deferred sats sit with the pool for now");
        assert_eq!(s1.carry_delta(|_| true), vec![("small".to_string(), 3_960)]);
        // block 2: same work, now carrying 3 960 → 7 920 total clears the floor. The pool's
        // remainder this block is only its 1 000 fee, so that is how much carry it can pay
        // down now; the rest stays owed.
        let s2 = compute(vec![miner("big", 9_600), miner_carry("small", 400, 3_960)], 10_000, 100_000, &p, script);
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
        let p = SplitParams { fee_bps: 100, stratum_fee_bps: 100, min_payout: 5_000, max_outputs: 512, output_budget_bytes: 14_000 };
        let s = compute(vec![miner("a", 100), miner_carry("gone", 0, 4_999)], 100, 1_000_000, &p, script);
        assert_eq!(s.payees.len(), 1, "4 999 is still under the floor");
        assert_eq!(s.unpaid[0].identity, "gone");
        assert_eq!((s.unpaid[0].sats, s.unpaid[0].earned), (4_999, 0));
        assert!(s.carry_delta(|_| true).is_empty(), "nothing earned, nothing to add");
        let s = compute(vec![miner("a", 100), miner_carry("gone", 0, 5_000)], 100, 1_000_000, &p, script);
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
        let p = SplitParams { fee_bps: 0, stratum_fee_bps: 0, min_payout: 1, max_outputs: 512, output_budget_bytes: 14_000 };
        let s = compute(vec![miner("a", 50), miner_carry("b", 50, 1_000_000)], 100, 1_000, &p, script);
        assert_eq!(s.pool_sats + s.paid_sats(), 1_000);
        assert_eq!(s.pool_sats, 0);
        let b = s.payees.iter().find(|x| x.identity == "b").unwrap();
        assert_eq!(b.sats, 500, "earned 500, no room for any carry");
        assert_eq!(b.carry, 0);
        assert!(s.carry_delta(|_| true).is_empty(), "carry untouched, still owed in full");
        // unpayable work does not accrue: there is no address to ever pay it to
        let s = compute(vec![miner("a", 50), miner("bad", 50)], 100, 1_000, &p, script);
        assert_eq!(s.unpaid[0].reason, UnpaidReason::NoScript);
        assert!(!s.unpaid[0].defers());
    }

    #[test]
    fn unpayable_and_budget() {
        let miners: Vec<MinerStat> =
            (0..20).map(|i| miner(&format!("{}{}", if i == 3 { "bad" } else { "m" }, i), 100)).collect();
        let p = SplitParams { fee_bps: 0, stratum_fee_bps: 0, min_payout: 1, max_outputs: 5, output_budget_bytes: 14_000 };
        let s = compute(miners, 2000, 1_000_000, &p, script);
        assert_eq!(s.payees.len(), 5);
        assert!(s.unpaid.iter().any(|u| u.reason == UnpaidReason::NoScript));
        assert_eq!(s.unpaid.iter().filter(|u| u.reason == UnpaidReason::OverBudget).count(), 14);
        // over-budget work rolls forward too: it was earned, the coinbase just had no room
        assert_eq!(s.carry_delta(|_| true).len(), 14);
        let p = SplitParams { fee_bps: 0, stratum_fee_bps: 0, min_payout: 1, max_outputs: 512, output_budget_bytes: 12 * 2 };
        let s = compute(vec![miner("a", 1), miner("b", 1), miner("c", 1)], 3, 300, &p, script);
        assert_eq!(s.payees.len(), 2);
        assert_eq!(s.pool_sats, 100);
    }

    #[test]
    fn empty_window_pays_the_pool() {
        let s = compute(vec![], 0, 100, &SplitParams::default(), script);
        assert!(s.payees.is_empty());
        assert_eq!(s.pool_sats, 100);
    }

    #[test]
    fn stratum_pays_a_higher_fee_than_datum() {
        let miners = vec![miner("datum", 600), miner_stratum("house", 400)];
        let p = SplitParams {
            fee_bps: 50,
            stratum_fee_bps: 500,
            min_payout: 1,
            max_outputs: 512,
            output_budget_bytes: 14_000,
        };
        let s = compute(miners, 1000, 10_000_000, &p, script);
        assert_eq!(s.payees[0].identity, "datum");
        assert_eq!(s.payees[0].sats, 5_970_000);
        assert_eq!(s.payees[1].identity, "house");
        assert_eq!(s.payees[1].sats, 3_800_000);
        assert_eq!(s.fee_sats, 230_000);
        assert_eq!(s.pool_sats + s.paid_sats(), 10_000_000);
    }
}
