//! Turning a window into coinbase outputs.
//!
//! `distributable = value − fee`. Each identity gets `distributable × work / total_work`,
//! rounded down. Identities that cannot be paid — no valid payout script, under the dust
//! floor, or past the size budget a gateway's coinbase can hold — are dropped and their
//! amount stays with the pool, which the gateway pays automatically as the remainder.

use serde::Serialize;

use crate::MinerStat;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitParams {
    /// Pool fee in basis points (50 = 0.5%).
    pub fee_bps: u32,
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
        SplitParams { fee_bps: 0, min_payout: 546, max_outputs: 512, output_budget_bytes: 14_000 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Payee {
    pub identity: String,
    pub work: u64,
    pub sats: u64,
    #[serde(with = "hex_bytes")]
    pub script: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Split {
    pub value: u64,
    pub fee_sats: u64,
    pub total_work: u64,
    /// Paid outputs, largest first.
    pub payees: Vec<Payee>,
    /// Identities in the window that will not get an output, with the sats they would have.
    pub unpaid: Vec<(String, u64, UnpaidReason)>,
    /// What the pool script receives: fee plus rounding plus everything unpaid.
    pub pool_sats: u64,
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
    let fee_sats = fee_for(value, p.fee_bps);
    let distributable = value - fee_sats;
    let mut payees = Vec::new();
    let mut unpaid = Vec::new();
    let mut paid = 0u64;
    let mut bytes = 0usize;
    if total_work > 0 {
        for m in miners {
            let sats = ((u128::from(distributable) * u128::from(m.work)) / u128::from(total_work)) as u64;
            if sats == 0 {
                continue;
            }
            if sats < p.min_payout {
                unpaid.push((m.identity, sats, UnpaidReason::BelowMinimum));
                continue;
            }
            let Some(script) = script_for(&m.identity) else {
                unpaid.push((m.identity, sats, UnpaidReason::NoScript));
                continue;
            };
            let need = 8 + 1 + script.len();
            if payees.len() >= p.max_outputs || bytes + need > p.output_budget_bytes {
                unpaid.push((m.identity, sats, UnpaidReason::OverBudget));
                continue;
            }
            bytes += need;
            paid += sats;
            payees.push(Payee { identity: m.identity, work: m.work, sats, script });
        }
    }
    Split { value, fee_sats, total_work, payees, unpaid, pool_sats: value - paid }
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
        MinerStat { identity: id.into(), work, credits: 1, last_ts: 0 }
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
        let p = SplitParams { fee_bps: 50, min_payout: 400_000, max_outputs: 512, output_budget_bytes: 14_000 };
        let s = compute(miners, 1001, 312_538_966, &p, script);
        assert_eq!(s.fee_sats, 1_562_694);
        let dist = 312_538_966 - 1_562_694;
        assert_eq!(s.payees.len(), 3);
        assert_eq!(s.payees[0].sats, dist * 600 / 1001);
        assert_eq!(s.payees[1].sats, dist * 300 / 1001);
        assert_eq!(s.payees[2].sats, dist * 100 / 1001);
        assert_eq!(s.unpaid.len(), 1);
        assert_eq!(s.unpaid[0].2, UnpaidReason::BelowMinimum);
        assert_eq!(s.pool_sats + s.paid_sats(), 312_538_966);
        assert!(s.pool_sats >= s.fee_sats + s.unpaid[0].1);
    }

    #[test]
    fn unpayable_and_budget() {
        let miners: Vec<MinerStat> =
            (0..20).map(|i| miner(&format!("{}{}", if i == 3 { "bad" } else { "m" }, i), 100)).collect();
        let p = SplitParams { fee_bps: 0, min_payout: 1, max_outputs: 5, output_budget_bytes: 14_000 };
        let s = compute(miners, 2000, 1_000_000, &p, script);
        assert_eq!(s.payees.len(), 5);
        assert!(s.unpaid.iter().any(|u| u.2 == UnpaidReason::NoScript));
        assert_eq!(s.unpaid.iter().filter(|u| u.2 == UnpaidReason::OverBudget).count(), 14);
        let p = SplitParams { fee_bps: 0, min_payout: 1, max_outputs: 512, output_budget_bytes: 12 * 2 };
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
}
