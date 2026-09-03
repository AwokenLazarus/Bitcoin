//! Pool-side share verification.
//!
//! A DATUM gateway builds its own block template. Prime never sees the transaction list
//! and does not need it: a share carries (once per job slot) the previous block hash,
//! nBits, height, transaction count and the coinbase's merkle branches, plus (once per
//! coinbase variant) the coinbase split into `coinb1`/`coinb2`. From those and the
//! share's own grinding fields Prime rebuilds the exact 164-byte header the miner hashed,
//! and can therefore check two things independently:
//!
//! * the **work** is real — the rebuilt header hashes under the share's target; and
//! * the **coinbase** pays what Prime told the gateway to pay.
//!
//! Everything about *which* transactions the miner chose stays with the miner.

use std::collections::HashMap;

use crate::coinbase::{self, Coinbase};
use crate::coinbaser::Output;
use crate::mining::{self, CoinbaseSection, JobSection, PowSubmit};
use crate::pow::{self, Commitment, Hash, JobWork};

/// Allowed clock skew on the header time, in seconds (Bitcoin's future-time rule).
pub const MAX_TIME_AHEAD: u32 = 7200;
/// A share's time may not be older than this many seconds behind Prime's clock. Templates
/// are refreshed every few seconds and a job lives about a minute; anything an hour old is
/// not real-time mining.
pub const MAX_TIME_BEHIND: u32 = 3600;

/// One of the gateway's eight job slots as Prime remembers it for a session.
#[derive(Clone, Debug, Default)]
pub struct JobSlot {
    pub job: Option<JobSection>,
    pub coinbases: HashMap<u8, CoinbaseSection>,
    /// H2 per (coinbase id, target byte, txcount convention), so repeat shares on a job cost
    /// one BLAKE2b instead of a full coinbase hash + merkle fold + two tagged SHA256s.
    h2_cache: HashMap<(u8, u8, u32, u32), Hash>,
    /// The parsed coinbase per (coinbase id, target byte).
    cb_cache: HashMap<(u8, u8), (Vec<u8>, Coinbase)>,
}

impl JobSlot {
    /// Fold a share's optional sections into the slot. A job section always starts a fresh
    /// slot: the gateway only resends it when the slot moved to a new job.
    pub fn absorb(&mut self, s: &PowSubmit) {
        if let Some(j) = &s.job {
            let same = self.job.as_ref() == Some(j);
            if !same {
                self.job = Some(j.clone());
                self.coinbases.clear();
                self.h2_cache.clear();
                self.cb_cache.clear();
            }
        }
        if let Some(c) = &s.coinbase {
            if self.coinbases.get(&c.coinbase_id) != Some(c) {
                self.coinbases.insert(c.coinbase_id, c.clone());
                self.h2_cache.retain(|k, _| k.0 != c.coinbase_id);
                self.cb_cache.retain(|k, _| k.0 != c.coinbase_id);
            }
        }
    }

    pub fn coinbase_id_for(s: &PowSubmit) -> u8 {
        if s.subsidy_only() {
            0xff
        } else {
            s.coinbase_id
        }
    }
}

/// Where a share's coinbase sends the block reward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinbaseKind {
    /// Every output Prime issued under the referenced coinbaser id is present and paid in full.
    Split,
    /// Some, not all, of the issued outputs are present, each paid in full, and nothing
    /// else is paid. The gateway builds several coinbase sizes and hands small miners the
    /// ones with room for only the first few outputs; the pool holds the rest and owes it
    /// to the window if this finds a block. The count is how many outputs were paid.
    Partial(u16),
    /// Only the pool's own script (plus OP_RETURNs) is paid: the stock gateway's "empty"
    /// coinbase used until a coinbaser reply arrives, or its smallest size class. The pool
    /// keeps 100% and owes the window if this finds a block.
    PoolOnly,
    /// Pays somewhere Prime did not sanction.
    Foreign,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedShare {
    pub hash: Hash,
    /// Work in difficulty-1 units (`2^target_pot`).
    pub work: u64,
    pub target_pot: u8,
    pub height: u32,
    pub ntime: u32,
    pub is_block_candidate: bool,
    pub coinbase_kind: CoinbaseKind,
    /// Total value of the coinbase outputs, i.e. subsidy plus fees.
    pub coinbase_value: u64,
    pub paid_to_pool: u64,
    /// The header commitment, kept so a block candidate can be serialized.
    pub commitment: Commitment,
    /// Legacy-serialized coinbase, kept for block assembly.
    pub coinbase_legacy: Vec<u8>,
    pub coinbase: Coinbase,
}

pub struct Policy<'a> {
    /// The pool's payout scriptPubKey (what the gateway was configured with).
    pub pool_script: &'a [u8],
    /// The outputs Prime issued under the share's coinbaser id, if Prime still has them.
    pub issued: Option<&'a [Output]>,
    /// Slack per issued output, in sats.
    pub tolerance: u64,
    /// Unix seconds now; 0 disables the time check.
    pub now: u32,
    /// Smallest power-of-two difficulty the pool accepts.
    pub min_pot: u8,
}

pub fn classify_coinbase(cb: &Coinbase, pool_script: &[u8], issued: Option<&[Output]>, tolerance: u64) -> CoinbaseKind {
    let mut pool_paid = false;
    let mut foreign = 0u64;
    for o in &cb.outputs {
        if o.is_op_return() {
            continue;
        }
        if o.script == pool_script {
            pool_paid = true;
            continue;
        }
        let sanctioned = issued.is_some_and(|iss| iss.iter().any(|i| i.script == o.script));
        if !sanctioned {
            foreign += 1;
        }
    }
    if foreign > 0 {
        return CoinbaseKind::Foreign;
    }
    if let Some(iss) = issued {
        // The pool's own output, if the list names one, takes whatever is left and is never
        // "short"; only miner outputs are held to their amounts.
        let miners: Vec<&Output> = iss.iter().filter(|i| i.script != pool_script).collect();
        // A gateway whose template is worth less than the value the split was computed
        // for may scale every output down by the same ratio (lazarus-gateway does; a
        // stock gateway drops outputs that no longer fit instead). Both keep the split
        // proportional, so the amount owed per output is the issued amount scaled by
        // actual/issued value, never more than issued.
        let issued_value: u64 = iss.iter().map(|i| i.sats).sum();
        let actual_value: u64 = cb.outputs.iter().map(|o| o.value).sum();
        let owed = |sats: u64| -> u64 {
            if issued_value == 0 || actual_value >= issued_value {
                sats
            } else {
                (u128::from(sats) * u128::from(actual_value) / u128::from(issued_value)) as u64
            }
        };
        let mut present = 0u16;
        let mut shorted = false;
        for i in &miners {
            let paid = cb.paid_to(&i.script);
            if paid > 0 {
                present += 1;
                if paid + tolerance < owed(i.sats) {
                    shorted = true;
                }
            }
        }
        if !shorted {
            if usize::from(present) == miners.len() && (pool_paid || !miners.is_empty()) {
                return CoinbaseKind::Split;
            }
            if present > 0 {
                return CoinbaseKind::Partial(present);
            }
        }
    }
    let only_pool = cb.outputs.iter().all(|o| o.is_op_return() || o.script == pool_script);
    if pool_paid && only_pool {
        return CoinbaseKind::PoolOnly;
    }
    CoinbaseKind::Foreign
}

/// Rebuild the header from the share and check the work and the coinbase.
///
/// `slot` must already have absorbed the share. On `Err` the code is a DATUM reject reason.
pub fn verify(slot: &mut JobSlot, s: &PowSubmit, p: &Policy) -> Result<VerifiedShare, u16> {
    if s.target_pot < p.min_pot {
        return Err(mining::REJECT_BAD_TARGET);
    }
    let share_target = pow::share_target_le(s.target_pot).ok_or(mining::REJECT_BAD_TARGET)?;
    verify_with_target(slot, s, p, &share_target)
}

/// [`verify`] against an explicit share target instead of the one the share claims.
pub fn verify_with_target(
    slot: &mut JobSlot,
    s: &PowSubmit,
    p: &Policy,
    share_target: &Hash,
) -> Result<VerifiedShare, u16> {
    if !s.is_blake2b() {
        return Err(mining::REJECT_BAD_VERSION);
    }
    let b2 = s.blake2b.as_ref().ok_or(mining::REJECT_BAD_VERSION)?;
    let time_on_wire = s.time_on_wire.ok_or(mining::REJECT_BAD_NTIME)?;
    let job = slot.job.as_ref().ok_or(mining::REJECT_BAD_JOB_ID)?;

    let cb_id = JobSlot::coinbase_id_for(s);
    let cb_key = (cb_id, s.target_pot);
    if !slot.cb_cache.contains_key(&cb_key) {
        let sect = slot.coinbases.get(&cb_id).ok_or(mining::REJECT_COINBASE_MISSING)?;
        let legacy = coinbase::assemble(&sect.coinb1, &sect.coinb2, usize::from(job.target_byte_index), s.target_pot);
        let parsed = coinbase::parse(&legacy).map_err(|_| mining::REJECT_BAD_COINBASE)?;
        if let Some(h) = parsed.height {
            if h != job.height {
                return Err(mining::REJECT_BAD_JOB_ID);
            }
        }
        slot.cb_cache.insert(cb_key, (legacy, parsed));
    }
    let (legacy, parsed) = slot.cb_cache.get(&cb_key).unwrap();
    let coinbase_kind = classify_coinbase(parsed, p.pool_script, p.issued, p.tolerance);
    if coinbase_kind == CoinbaseKind::Foreign {
        return Err(mining::REJECT_BAD_COINBASE_OUTPUTS);
    }

    let flags = if s.use_time_offset() { pow::FLAG_USE_TIME_OFFSET } else { 0 };
    let ntime = pow::share_ntime(time_on_wire, &b2.ntime, flags);
    if p.now != 0 && (ntime > p.now.saturating_add(MAX_TIME_AHEAD) || ntime.saturating_add(MAX_TIME_BEHIND) < p.now) {
        return Err(mining::REJECT_BAD_NTIME);
    }

    // The header's transaction count includes the coinbase; the job section's does not.
    // Some gateways count it already, so try the spec'd form first and fall back once.
    let mut chosen: Option<(Commitment, Hash)> = None;
    for txcount in [job.txn_count.wrapping_add(1), job.txn_count] {
        let key = (cb_id, s.target_pot, txcount, s.version);
        let commitment_of = |h2_hint: Option<Hash>| -> (Commitment, Hash) {
            let c = Commitment {
                version: s.version,
                prev_hash: job.prev_hash,
                height: job.height,
                merkle_root: match h2_hint {
                    Some(_) => [0u8; 32],
                    None => pow::merkle_root(pow::sha256d(legacy), &job.merkle_branches),
                },
                time_on_wire,
                nbits: job.nbits_u32(),
                txcount,
                flags,
                xor_clear_bits: 0,
                xor_key: [0u8; 16],
                rhs: [0u8; 32],
            };
            let h2 = h2_hint.unwrap_or_else(|| c.h2());
            (c, h2)
        };
        let (c, h2) = match slot.h2_cache.get(&key) {
            Some(h) => commitment_of(Some(*h)),
            None => commitment_of(None),
        };
        let root = pow::work_root(&h2, &s.extranonce);
        let sia_prev = pow::sia_prevhash(&job.prev_hash);
        let work = pow::work_header(&sia_prev, &b2.nonce, &b2.ntime, &root);
        let hash = pow::pow_hash_le(&work, &[0u8; 16], 0);
        if pow::meets_target(&hash, share_target) {
            slot.h2_cache.insert(key, h2);
            chosen = Some((c, hash));
            break;
        }
    }
    let (mut commitment, hash) = chosen.ok_or(mining::REJECT_HIGH_HASH)?;
    if commitment.merkle_root == [0u8; 32] {
        // came from the cache; the full commitment is only needed for block assembly
        commitment.merkle_root = pow::merkle_root(pow::sha256d(legacy), &job.merkle_branches);
    }

    let is_block_candidate =
        pow::nbits_to_target_le(commitment.nbits).map(|t| pow::meets_target(&hash, &t)).unwrap_or(false);

    Ok(VerifiedShare {
        hash,
        work: s.claimed_work(),
        target_pot: s.target_pot,
        height: job.height,
        ntime,
        is_block_candidate,
        coinbase_kind,
        coinbase_value: parsed.total_output_value(),
        paid_to_pool: parsed.paid_to(p.pool_script),
        commitment,
        coinbase_legacy: legacy.clone(),
        coinbase: parsed.clone(),
    })
}

/// Serialize a full block from a verified block candidate and the job's transactions
/// (raw, in template order, without the coinbase).
pub fn assemble_block(v: &VerifiedShare, s: &PowSubmit, txns: &[Vec<u8>]) -> Vec<u8> {
    let b2 = s.blake2b.as_ref().expect("verified shares are BLAKE2b");
    let header = pow::serialize_header_v2(&v.commitment, &b2.nonce, &b2.ntime, &s.extranonce);
    let cb = coinbase::with_witness(&v.coinbase_legacy, &v.coinbase);
    let mut out = Vec::with_capacity(header.len() + 9 + cb.len() + txns.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(&header);
    coinbase::write_varint(&mut out, txns.len() as u64 + 1);
    out.extend_from_slice(&cb);
    for t in txns {
        out.extend_from_slice(t);
    }
    out
}

/// Precompute the per-job hashing state for a share so a grinder (tests, the bundled
/// client) can search nonces the way hardware does.
pub fn job_work_for(slot: &JobSlot, s: &PowSubmit, txcount_includes_coinbase: bool) -> Option<JobWork> {
    let job = slot.job.as_ref()?;
    let sect = slot.coinbases.get(&JobSlot::coinbase_id_for(s))?;
    let legacy = coinbase::assemble(&sect.coinb1, &sect.coinb2, usize::from(job.target_byte_index), s.target_pot);
    let c = Commitment {
        version: s.version,
        prev_hash: job.prev_hash,
        height: job.height,
        merkle_root: pow::merkle_root(pow::sha256d(&legacy), &job.merkle_branches),
        time_on_wire: s.time_on_wire?,
        nbits: job.nbits_u32(),
        txcount: if txcount_includes_coinbase { job.txn_count } else { job.txn_count + 1 },
        flags: if s.use_time_offset() { pow::FLAG_USE_TIME_OFFSET } else { 0 },
        xor_clear_bits: 0,
        xor_key: [0u8; 16],
        rhs: [0u8; 32],
    };
    Some(JobWork::new(&c, &s.extranonce))
}

#[cfg(test)]
pub mod fixtures {
    //! Build shares the way a gateway does, for tests here and in dependents.
    use super::*;
    use crate::coinbase::TxOut;
    use crate::mining::Blake2bSection;

    pub const HEIGHT: u32 = 966_267;
    pub const VALUE: u64 = 312_538_966;
    pub const NOW: u32 = 1_788_408_528;

    pub fn p2wpkh(fill: u8) -> Vec<u8> {
        let mut s = vec![0x00, 0x14];
        s.extend_from_slice(&[fill; 20]);
        s
    }

    pub fn pool_script() -> Vec<u8> {
        let mut s = vec![0x00, 0x20];
        s.extend_from_slice(&[0x5d; 32]);
        s
    }

    pub fn split() -> Vec<Output> {
        vec![
            Output { sats: 200_000_000, script: p2wpkh(0x11) },
            Output { sats: 100_000_000, script: p2wpkh(0x22) },
            Output { sats: 10_000_000, script: p2wpkh(0x33) },
        ]
    }

    pub fn txids(n: usize) -> Vec<Hash> {
        (0..n)
            .map(|i| {
                let mut a = [0u8; 32];
                a[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
                a
            })
            .collect()
    }

    /// Outputs as the gateway lays them out: issued split, then pool remainder, then witness commitment.
    pub fn gateway_outputs(issued: &[Output], pool: &[u8], value: u64) -> Vec<TxOut> {
        let mut outs: Vec<TxOut> = issued.iter().map(|o| TxOut { value: o.sats, script: o.script.clone() }).collect();
        let paid: u64 = issued.iter().map(|o| o.sats).sum();
        outs.push(TxOut { value: value - paid, script: pool.to_vec() });
        let mut wc = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
        wc.extend_from_slice(&[0x76; 32]);
        outs.push(TxOut { value: 0, script: wc });
        outs
    }

    /// A share for job slot `slot` with the given outputs and difficulty, plus the extranonce
    /// and grinding fields. `pot` is the target byte the gateway wrote into the coinbase.
    #[allow(clippy::too_many_arguments)]
    pub fn share(
        slot: u8,
        cb_id: u8,
        coinbaser_id: u8,
        outs: &[TxOut],
        txs: &[Hash],
        pot: u8,
        nonce: [u8; 8],
        ntime: [u8; 8],
    ) -> PowSubmit {
        let (cb, tidx) = coinbase::build(HEIGHT, b"Lazarus", outs, 0);
        let split_at = tidx + 1;
        let coinb1 = cb[..split_at].to_vec();
        let coinb2 = cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec();
        PowSubmit {
            job_id: slot,
            coinbase_id: cb_id,
            flags: mining::FLAG_BLAKE2B,
            target_pot: pot,
            ntime32: u32::from_le_bytes(ntime[..4].try_into().unwrap()),
            nonce32: u32::from_le_bytes(nonce[..4].try_into().unwrap()),
            version: 0xa000_0000,
            extranonce: [0x0b, 0x10, 0xc0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8],
            username: "bc1qminer.rig".into(),
            reserved: [0; 4],
            blake2b: Some(Blake2bSection { ntime, nonce }),
            time_on_wire: Some(NOW),
            job: Some(JobSection {
                prev_hash: [0x77; 32],
                target_byte_index: tidx as u16,
                nbits: 0x193c_2d40u32.to_le_bytes(),
                coinbaser_id,
                height: HEIGHT,
                coinbase_value: VALUE,
                txn_count: txs.len() as u32,
                txn_total_weight: 0,
                txn_total_size: 0,
                txn_total_sigops: 0,
                merkle_branches: pow::merkle_branches_for_coinbase(txs),
            }),
            coinbase: Some(CoinbaseSection { coinbase_id: cb_id, coinb1, coinb2 }),
        }
    }

    /// A target one in 2^12 hashes meets, so tests can grind real shares in milliseconds.
    /// (A real difficulty-1 share is 2^32 hashes.)
    pub fn easy_target() -> Hash {
        let mut t = [0xffu8; 32];
        t[31] = 0;
        t[30] = 0x0f;
        t
    }

    /// Grind `s` until it meets `target`, using the txcount convention `includes_coinbase`.
    pub fn grind_to(slot: &mut JobSlot, s: &mut PowSubmit, target: &Hash, includes_coinbase: bool) {
        slot.absorb(s);
        let jw = job_work_for(slot, s, includes_coinbase).unwrap();
        let b = s.blake2b.as_mut().unwrap();
        let mut n = u32::from_le_bytes(b.nonce[..4].try_into().unwrap());
        loop {
            b.nonce[..4].copy_from_slice(&n.to_le_bytes());
            if pow::meets_target(&jw.hash(&b.nonce, &b.ntime), target) {
                s.nonce32 = n;
                return;
            }
            n = n.wrapping_add(1);
        }
    }

    pub fn grind(slot: &mut JobSlot, s: &mut PowSubmit) {
        grind_to(slot, s, &easy_target(), false)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::coinbase::TxOut;

    fn policy<'a>(issued: &'a [Output], pool: &'a [u8]) -> Policy<'a> {
        Policy { pool_script: pool, issued: Some(issued), tolerance: 2, now: NOW, min_pot: 0 }
    }

    fn check(slot: &mut JobSlot, s: &PowSubmit, p: &Policy) -> Result<VerifiedShare, u16> {
        verify_with_target(slot, s, p, &easy_target())
    }

    /// Throughput of the two paths a Prime sees: the first share on a job (parse coinbase,
    /// merkle fold, two tagged SHA256s, one BLAKE2b) and every later share on it (one
    /// BLAKE2b plus the coinbase policy check). Run with
    /// `cargo test --release -p datum-wire -- --ignored --nocapture verify_throughput`.
    #[test]
    #[ignore]
    fn verify_throughput() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let txs = txids(2000);
        let mut slot = JobSlot::default();
        let mut s = share(2, 4, 9, &outs, &txs, 3, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        let p = policy(&iss, &pool);
        // any hash passes: we time the rebuild, not the luck
        let all = [0xff; 32];

        let n = 20_000u64;
        let t = std::time::Instant::now();
        for _ in 0..n {
            let mut fresh = JobSlot::default();
            fresh.absorb(&s);
            verify_with_target(&mut fresh, &s, &p, &all).unwrap();
        }
        let cold = t.elapsed();

        let mut s2 = s.clone();
        s2.job = None;
        s2.coinbase = None;
        let t = std::time::Instant::now();
        for i in 0..n {
            s2.blake2b.as_mut().unwrap().nonce[..8].copy_from_slice(&i.to_le_bytes());
            verify_with_target(&mut slot, &s2, &p, &all).unwrap();
        }
        let warm = t.elapsed();
        println!(
            "cold (job + coinbase sections, 2000-tx template): {:.0} shares/s; warm (same job): {:.0} shares/s",
            n as f64 / cold.as_secs_f64(),
            n as f64 / warm.as_secs_f64()
        );
    }

    #[test]
    fn a_ground_share_verifies_and_repeat_shares_hit_the_cache() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let txs = txids(58);
        let mut slot = JobSlot::default();
        let mut s = share(2, 4, 9, &outs, &txs, 3, [0; 8], [0, 0, 0, 0, 0xef, 0xbe, 0xad, 0xde]);
        grind(&mut slot, &mut s);
        let v = check(&mut slot, &s, &policy(&iss, &pool)).expect("verifies");
        assert_eq!(v.work, 8);
        assert_eq!(v.height, HEIGHT);
        assert_eq!(v.coinbase_kind, CoinbaseKind::Split);
        assert_eq!(v.coinbase_value, VALUE);
        assert_eq!(v.paid_to_pool, VALUE - 310_000_000);
        assert!(!v.is_block_candidate);
        assert_eq!(v.commitment.txcount, 59);
        assert_eq!(v.ntime, NOW);

        // second share on the same job: no job/coinbase sections, different nonce
        let mut s2 = s.clone();
        s2.job = None;
        s2.coinbase = None;
        s2.blake2b.as_mut().unwrap().nonce[4..].copy_from_slice(&[9, 9, 9, 9]);
        grind(&mut slot, &mut s2);
        let v2 = check(&mut slot, &s2, &policy(&iss, &pool)).expect("cached job still verifies");
        assert_ne!(v2.hash, v.hash);
        assert_eq!(v2.commitment, v.commitment);
        assert_eq!(slot.h2_cache.len(), 1, "one commitment cached for (cb, pot, txcount, version)");
    }

    #[test]
    fn work_must_actually_meet_the_claimed_target() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(3), 2, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        // the real target for pot 2 needs 2^34 hashes; the easy grind will not have met it
        assert_eq!(verify(&mut slot, &s, &policy(&iss, &pool)), Err(mining::REJECT_HIGH_HASH));
        // tampering with any grinding field breaks even the easy target
        let mut t = s.clone();
        t.extranonce[11] ^= 1;
        assert_eq!(check(&mut slot, &t, &policy(&iss, &pool)), Err(mining::REJECT_HIGH_HASH));
        let mut t = s.clone();
        t.blake2b.as_mut().unwrap().ntime[0] ^= 1;
        assert_eq!(check(&mut slot, &t, &policy(&iss, &pool)), Err(mining::REJECT_HIGH_HASH));
        let mut t = s.clone();
        t.version ^= 1;
        assert_eq!(check(&mut slot, &t, &policy(&iss, &pool)), Err(mining::REJECT_HIGH_HASH));
    }

    #[test]
    fn coinbase_that_underpays_or_diverts_is_rejected() {
        let pool = pool_script();
        let iss = split();
        let mut robbed = iss.clone();
        robbed[1].sats = 1;
        let outs = gateway_outputs(&robbed, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)), Err(mining::REJECT_BAD_COINBASE_OUTPUTS));

        let thief = vec![Output { sats: VALUE, script: p2wpkh(0xee) }];
        let outs = gateway_outputs(&thief, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)), Err(mining::REJECT_BAD_COINBASE_OUTPUTS));

        // a smaller coinbase size class carries only the first issued outputs: Partial
        let outs = gateway_outputs(&iss[..2], &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 2, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        let v = check(&mut slot, &s, &policy(&iss, &pool)).unwrap();
        assert_eq!(v.coinbase_kind, CoinbaseKind::Partial(2));
        assert_eq!(v.paid_to_pool, VALUE - 300_000_000);
        // ...but a partial coinbase that shorts one of the outputs it does carry is not
        let mut short = iss[..2].to_vec();
        short[1].sats -= 1_000;
        let outs = gateway_outputs(&short, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 2, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)), Err(mining::REJECT_BAD_COINBASE_OUTPUTS));

        // a gateway may rescale slightly (dust rounding), within tolerance
        let mut near = iss.clone();
        near[0].sats -= 2;
        let outs = gateway_outputs(&near, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)).unwrap().coinbase_kind, CoinbaseKind::Split);
    }

    /// Prime's list is complete — the pool's own output comes last — and a gateway whose
    /// template is worth less than the value the split was computed for may scale the whole
    /// list down proportionally (lazarus-gateway) or drop the pool output (stock). Either
    /// verifies; a miner output scaled below its share does not.
    #[test]
    fn complete_list_and_proportional_scaling() {
        let pool = pool_script();
        let mut iss = split();
        let miners_paid: u64 = iss.iter().map(|o| o.sats).sum();
        iss.push(Output { sats: VALUE - miners_paid, script: pool.clone() });
        assert_eq!(iss.iter().map(|o| o.sats).sum::<u64>(), VALUE);

        // exact template value: paid verbatim
        let outs: Vec<TxOut> = iss.iter().map(|o| TxOut { value: o.sats, script: o.script.clone() }).collect();
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        let v = check(&mut slot, &s, &policy(&iss, &pool)).unwrap();
        assert_eq!(v.coinbase_kind, CoinbaseKind::Split);
        assert_eq!(v.paid_to_pool, VALUE - miners_paid);

        // template worth 1% less: every output scaled by the same ratio (lazarus-gateway)
        let smaller = VALUE - VALUE / 100;
        let mut scaled: Vec<TxOut> = iss
            .iter()
            .map(|o| TxOut {
                value: (u128::from(o.sats) * u128::from(smaller) / u128::from(VALUE)) as u64,
                script: o.script.clone(),
            })
            .collect();
        let paid: u64 = scaled.iter().map(|o| o.value).sum();
        scaled.last_mut().unwrap().value += smaller - paid;
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &scaled, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)).unwrap().coinbase_kind, CoinbaseKind::Split);

        // template worth a little less: stock gateway keeps the miner outputs whole, our
        // pool output no longer fits, and its own leftover output replaces it
        let stock = gateway_outputs(&iss[..3], &pool, VALUE - 1_000_000);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &stock, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)).unwrap().coinbase_kind, CoinbaseKind::Split);

        // scaling one miner output harder than the ratio is still a short payment
        let mut cheat = scaled.clone();
        cheat[0].value -= 1_000;
        cheat[3].value += 1_000;
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &cheat, &txids(3), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        assert_eq!(check(&mut slot, &s, &policy(&iss, &pool)), Err(mining::REJECT_BAD_COINBASE_OUTPUTS));
    }

    #[test]
    fn pool_only_coinbase_is_credited_but_flagged() {
        let pool = pool_script();
        let outs = gateway_outputs(&[], &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 0, 0, &outs, &txids(0), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        let p = Policy { pool_script: &pool, issued: None, tolerance: 0, now: NOW, min_pot: 0 };
        let v = check(&mut slot, &s, &p).expect("pool-only is valid work");
        assert_eq!(v.coinbase_kind, CoinbaseKind::PoolOnly);
        assert_eq!(v.paid_to_pool, VALUE);
        assert_eq!(v.commitment.txcount, 1);
        // with an issued split that this coinbase ignores, it is still pool-only, not foreign
        let iss = split();
        let v = check(&mut slot, &s, &policy(&iss, &pool)).unwrap();
        assert_eq!(v.coinbase_kind, CoinbaseKind::PoolOnly);
    }

    #[test]
    fn missing_sections_and_bad_times_are_rejected() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(1, 4, 1, &outs, &txids(2), 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);

        let mut fresh = JobSlot::default();
        let mut no_job = s.clone();
        no_job.job = None;
        fresh.absorb(&no_job);
        assert_eq!(check(&mut fresh, &no_job, &policy(&iss, &pool)), Err(mining::REJECT_BAD_JOB_ID));

        let mut fresh = JobSlot::default();
        let mut no_cb = s.clone();
        no_cb.coinbase = None;
        fresh.absorb(&no_cb);
        assert_eq!(check(&mut fresh, &no_cb, &policy(&iss, &pool)), Err(mining::REJECT_COINBASE_MISSING));

        // a SHA256d share carries neither the flag nor the 0x03 section
        let mut sha = s.clone();
        sha.flags = 0;
        sha.blake2b = None;
        assert_eq!(check(&mut slot, &sha, &policy(&iss, &pool)), Err(mining::REJECT_BAD_VERSION));
        // flag clear but section present (iohzrd lineage) still verifies
        let mut flagless = s.clone();
        flagless.flags = 0;
        assert!(check(&mut slot, &flagless, &policy(&iss, &pool)).is_ok());

        let mut p = policy(&iss, &pool);
        p.now = NOW + MAX_TIME_BEHIND + 10;
        assert_eq!(check(&mut slot, &s, &p), Err(mining::REJECT_BAD_NTIME));
        p.now = NOW - MAX_TIME_AHEAD - 10;
        assert_eq!(check(&mut slot, &s, &p), Err(mining::REJECT_BAD_NTIME));

        let mut p = policy(&iss, &pool);
        p.min_pot = 5;
        assert_eq!(verify(&mut slot, &s, &p), Err(mining::REJECT_BAD_TARGET));

        // a height mismatch between coinbase and job section
        let mut wrong_h = s.clone();
        wrong_h.job.as_mut().unwrap().height += 1;
        let mut fresh = JobSlot::default();
        fresh.absorb(&wrong_h);
        assert_eq!(check(&mut fresh, &wrong_h, &policy(&iss, &pool)), Err(mining::REJECT_BAD_JOB_ID));
    }

    #[test]
    fn a_gateway_that_counts_the_coinbase_itself_still_verifies() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txids(5), 1, [0; 8], [0; 8]);
        grind_to(&mut slot, &mut s, &easy_target(), true);
        let v = check(&mut slot, &s, &policy(&iss, &pool)).expect("alternate txcount convention");
        assert_eq!(v.commitment.txcount, 5);
    }

    #[test]
    fn slot_replaces_job_and_coinbases_when_the_job_changes() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let mut slot = JobSlot::default();
        let a = share(0, 4, 1, &outs, &txids(5), 1, [0; 8], [0; 8]);
        slot.absorb(&a);
        let mut b = share(0, 4, 2, &outs, &txids(6), 1, [0; 8], [0; 8]);
        b.coinbase = None; // a real gateway resends it, but the slot must not keep stale ones
        slot.absorb(&b);
        assert_eq!(slot.job.as_ref().unwrap().coinbaser_id, 2);
        assert!(slot.coinbases.is_empty());
    }

    #[test]
    fn block_assembly_has_header_count_coinbase_and_transactions() {
        let pool = pool_script();
        let iss = split();
        let outs = gateway_outputs(&iss, &pool, VALUE);
        let txs = txids(2);
        let mut slot = JobSlot::default();
        let mut s = share(0, 4, 1, &outs, &txs, 1, [0; 8], [0; 8]);
        grind(&mut slot, &mut s);
        let v = check(&mut slot, &s, &policy(&iss, &pool)).unwrap();
        let raw = vec![vec![0xaa; 100], vec![0xbb; 60]];
        let block = assemble_block(&v, &s, &raw);
        assert_eq!(block.len(), 164 + 1 + (v.coinbase_legacy.len() + 2 + 34) + 160);
        assert_eq!(block[164], 3);
        assert_eq!(&block[..4], &0xa000_0000u32.to_le_bytes());
        assert_eq!(&block[4..36], &[0x77; 32]);
        assert_eq!(&block[36..68], &v.commitment.merkle_root);
        assert_eq!(&block[block.len() - 60..], &raw[1][..]);
    }
}
