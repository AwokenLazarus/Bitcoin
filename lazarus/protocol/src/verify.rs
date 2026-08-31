//! Pool-side share verification.
//!
//! A DATUM gateway builds its own template, so Prime never sees the transaction list and
//! must not need it. What Prime *can* check from a share is that the work was really done
//! (rebuild the header the miner solved and hash it) and that the coinbase pays the split
//! Prime issued. Everything about which transactions the miner chose stays theirs.

use crate::coinbaser::{parse_coinbase, split_satisfied, CoinbaserV2, ParsedCoinbase};
use crate::mining::{self, PowSubmit};
use crate::pow::{self, HeaderV2};

/// Slack for `scale_to` rounding when a gateway rescales a split to a new template value.
pub const SPLIT_TOLERANCE_SATS: u64 = 2;
/// How far a share's height may lead Prime's view of the tip.
pub const HEIGHT_LEAD: u64 = 2;
/// How many blocks behind the tip a share's job may be and still be credited.
///
/// A miner works the job it was handed, and blocks on this chain arrive about once a
/// minute, so a share landing a block or two late is normal and the work behind it is
/// real. Refusing it throws away hashing the miner genuinely did through no fault of its
/// own. Replaying old work to farm credit is not possible: a repeat is the same share and
/// is caught by deduplication.
pub const HEIGHT_LAG: u64 = 3;
/// Allowed clock skew on the header time, in seconds.
pub const MAX_TIME_SKEW: u64 = 7200;

pub struct ShareContext<'a> {
    /// The split Prime issued under this share's coinbaser id, if still cached.
    pub issued: Option<&'a CoinbaserV2>,
    /// Prime's view of the tip height; 0 skips the height check.
    pub tip_height: u64,
    /// Unix seconds; 0 skips the clock check.
    pub now: u64,
    pub min_diff: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedShare {
    pub work: u64,
    pub hash: [u8; 32],
    pub height: u32,
    /// The rebuilt header also meets the network target: whoever sent this found a block.
    pub is_block_candidate: bool,
}

/// Rebuild the header a miner hashed. `merkle_root` comes from the share's own coinbase
/// folded through its branches.
pub fn header_from_share(s: &PowSubmit, merkle_root: [u8; 32]) -> Option<HeaderV2> {
    let job = s.job.as_ref()?;
    let b2 = s.blake2b.as_ref()?;
    let mut h = HeaderV2::default();
    h.version = s.version as i32;
    h.prev_block = job.prev_hash;
    h.merkle_root = merkle_root;
    h.bits = u32::from_le_bytes(job.nbits);
    h.height = job.height as i32;
    h.txcount = job.txn_count as u16;
    h.flags = if s.use_time_offset { pow::FLAG_USE_TIME_OFFSET } else { 0 };
    h.time = b2.time_on_wire;
    h.nonce = u32::from_le_bytes(b2.sia_nonce[0..4].try_into().ok()?);
    h.nonce2 = u32::from_le_bytes(b2.sia_nonce[4..8].try_into().ok()?);
    h.extranonce = pow::header_extranonce(&s.extranonce);
    // The ASIC hashes its own 8-byte ntime inside the 80-byte pass, so those bytes are
    // nonce space we have to reproduce rather than assume are zero.
    h.time_offset = u32::from_le_bytes(b2.sia_ntime[0..4].try_into().ok()?);
    h.nonce3 = u32::from_le_bytes(b2.sia_ntime[4..8].try_into().ok()?);
    Some(h)
}

/// Parse the share's coinbase and confirm it pays the issued split.
pub fn check_coinbase(s: &PowSubmit, issued: Option<&CoinbaserV2>) -> Result<ParsedCoinbase, u16> {
    let job = s.job.as_ref().ok_or(mining::REJECT_BAD_JOB_ID)?;
    let cbs = s.coinbase.as_ref().ok_or(mining::REJECT_BAD_COINBASE_ID)?;
    if cbs.coinbase_id != s.coinbase_id || job.coinbaser_id != cbs.coinbase_id {
        return Err(mining::REJECT_BAD_COINBASE_ID);
    }
    let mut full = cbs.coinb1.clone();
    full.extend_from_slice(&cbs.coinb2);
    let parsed = parse_coinbase(&full).ok_or(mining::REJECT_BAD_COINBASE_ID)?;
    let issued = issued.ok_or(mining::REJECT_BAD_COINBASE_ID)?;
    if cbs.coinbase_id != issued.id {
        return Err(mining::REJECT_BAD_COINBASE_ID);
    }
    if !split_satisfied(issued, &parsed.outputs, SPLIT_TOLERANCE_SATS) {
        return Err(mining::REJECT_BAD_COINBASE_ID);
    }
    if let Some(h) = parsed.height {
        if h != job.height {
            return Err(mining::REJECT_BAD_JOB_ID);
        }
    }
    Ok(parsed)
}

/// Rebuild the header from the share and hash it. Does not judge the result.
pub fn share_hash(s: &PowSubmit, coinbase: &ParsedCoinbase) -> Result<(HeaderV2, [u8; 32]), u16> {
    let job = s.job.as_ref().ok_or(mining::REJECT_BAD_JOB_ID)?;
    let txid = pow::sha256d(&coinbase.legacy);
    let root = pow::fold_branches(txid, &job.merkle_branches);
    let hdr = header_from_share(s, root).ok_or(mining::REJECT_OTHER)?;
    let hash = hdr.pow_hash();
    Ok((hdr, hash))
}

/// Full check against an explicit share target.
pub fn verify_share_against(
    s: &PowSubmit,
    ctx: &ShareContext,
    share_target: &[u8; 32],
) -> Result<VerifiedShare, u16> {
    if s.job.is_none() {
        return Err(mining::REJECT_BAD_JOB_ID);
    }
    if s.blake2b.is_none() {
        return Err(mining::REJECT_OTHER);
    }
    if s.extranonce.len() != 12 {
        return Err(mining::REJECT_BAD_EXTRANONCE_SIZE);
    }
    let coinbase = check_coinbase(s, ctx.issued)?;
    let job = s.job.as_ref().ok_or(mining::REJECT_BAD_JOB_ID)?;
    let b2 = s.blake2b.as_ref().ok_or(mining::REJECT_OTHER)?;

    if ctx.tip_height > 0 {
        let h = u64::from(job.height);
        if h + HEIGHT_LAG < ctx.tip_height || h > ctx.tip_height + HEIGHT_LEAD {
            return Err(mining::REJECT_STALE);
        }
    }
    if ctx.now > 0 {
        let t = u64::from(b2.time_on_wire);
        let skew = if t > ctx.now { t - ctx.now } else { ctx.now - t };
        if skew > MAX_TIME_SKEW {
            return Err(mining::REJECT_STALE);
        }
    }

    let (hdr, hash) = share_hash(s, &coinbase)?;
    if !pow::meets_target(&hash, share_target) {
        return Err(mining::REJECT_HIGH_HASH);
    }
    let is_block_candidate = pow::bits_to_target(hdr.bits)
        .map(|t| pow::meets_target(&hash, &t))
        .unwrap_or(false);
    Ok(VerifiedShare {
        work: s.difficulty().max(ctx.min_diff),
        hash,
        height: job.height,
        is_block_candidate,
    })
}

/// Full check against the target the share claims (`target_byte`).
pub fn verify_share(s: &PowSubmit, ctx: &ShareContext) -> Result<VerifiedShare, u16> {
    let target = pow::target_for_pot(s.target_byte);
    verify_share_against(s, ctx, &target)
}

pub fn reject_name(code: u16) -> &'static str {
    match code {
        mining::REJECT_BAD_JOB_ID => "BadJobId",
        mining::REJECT_BAD_COINBASE_ID => "BadCoinbase",
        mining::REJECT_BAD_EXTRANONCE_SIZE => "BadExtranonce",
        mining::REJECT_BAD_TARGET => "BadTarget",
        mining::REJECT_BAD_USERNAME => "BadUsername",
        mining::REJECT_STALE => "Stale",
        mining::REJECT_HIGH_HASH => "HighHash",
        mining::REJECT_DUPLICATE => "Duplicate",
        _ => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbtx;
    use crate::coinbaser::CoinbaserOutput;
    use crate::mining::{Blake2bSection, CoinbaseSection, JobSection};

    const HEIGHT: u32 = 962_049;
    const TAG: &str = "Lazarus";
    const EXTRA1: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];
    const EN2: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

    fn p2wsh(fill: u8) -> Vec<u8> {
        let mut s = vec![0x00, 0x20];
        s.extend_from_slice(&[fill; 32]);
        s
    }

    fn issued_split() -> CoinbaserV2 {
        CoinbaserV2 {
            id: 42,
            outputs: vec![
                CoinbaserOutput { sats: 200_000_000, script: p2wsh(0x11) },
                CoinbaserOutput { sats: 100_000_000, script: p2wsh(0x22) },
                CoinbaserOutput { sats: 13_156_010, script: p2wsh(0x33) },
            ],
        }
    }

    fn txids(n: usize) -> Vec<[u8; 32]> {
        (0..n)
            .map(|i| {
                let mut a = [0u8; 32];
                a[..8].copy_from_slice(&(i as u64 + 1).to_le_bytes());
                a
            })
            .collect()
    }

    /// Build exactly what the gateway builds: the job header, and the share a miner
    /// submits against it.
    fn gateway_share(cb: &CoinbaserV2, txs: &[[u8; 32]]) -> (HeaderV2, PowSubmit) {
        let wit = [0x6au8; 38];
        let mut extra = EXTRA1.to_vec();
        extra.extend_from_slice(&[0u8; 8]);
        let cbleg = cbtx::coinbase_legacy(HEIGHT, TAG, &extra, cb, Some(&wit));

        let mut hdr = HeaderV2::default();
        hdr.version = 0x2000_0000;
        hdr.prev_block = [0x77; 32];
        hdr.time = 1_800_000_000;
        hdr.bits = 0x1703_098d;
        hdr.height = HEIGHT as i32;
        hdr.txcount = (txs.len() + 1) as u16;
        hdr.merkle_root = pow::merkle_root_sha256d(&cbleg, txs);
        let mut en12 = EXTRA1.to_vec();
        en12.extend_from_slice(&EN2);
        hdr.extranonce = pow::header_extranonce(&en12);
        hdr.nonce = 0x1234_5678;
        hdr.nonce2 = 0x9abc_def0;

        let mut sia_nonce = [0u8; 8];
        sia_nonce[0..4].copy_from_slice(&hdr.nonce.to_le_bytes());
        sia_nonce[4..8].copy_from_slice(&hdr.nonce2.to_le_bytes());
        let mut en = EXTRA1.to_vec();
        en.extend_from_slice(&EN2);

        let s = PowSubmit {
            job_id: 7,
            coinbase_id: cb.id,
            is_block: false,
            subsidy_only: false,
            quickdiff: false,
            target_byte: 0,
            ntime: hdr.time,
            nonce: hdr.nonce,
            version: hdr.version as u32,
            extranonce: en,
            username: "bc1qexample.worker".into(),
            use_time_offset: false,
            job: Some(JobSection {
                prev_hash: hdr.prev_block,
                target_byte_index: 0,
                nbits: hdr.bits.to_le_bytes(),
                coinbaser_id: cb.id,
                height: HEIGHT,
                coinbase_value: cb.value_sum(),
                txn_count: txs.len() as u32 + 1,
                txn_total_weight: 0,
                txn_total_size: 0,
                txn_total_sigops: 0,
                merkle_branches: pow::merkle_branches_for_coinbase(txs),
            }),
            coinbase: Some(CoinbaseSection { coinbase_id: cb.id, coinb1: cbleg, coinb2: Vec::new() }),
            blake2b: Some(Blake2bSection { sia_ntime: [0u8; 8], sia_nonce, time_on_wire: hdr.time }),
        };
        (hdr, s)
    }

    fn ctx<'a>(issued: &'a CoinbaserV2) -> ShareContext<'a> {
        ShareContext { issued: Some(issued), tip_height: u64::from(HEIGHT) - 1, now: 1_800_000_000, min_diff: 1 }
    }

    fn easy() -> [u8; 32] {
        [0xffu8; 32]
    }

    #[test]
    fn rebuilds_the_header_the_gateway_published() {
        let cb = issued_split();
        for n in [0usize, 1, 2, 3, 891] {
            let (hdr, share) = gateway_share(&cb, &txids(n));
            // through the wire, so the test covers encode/decode too
            let wire = share.encode();
            let back = PowSubmit::decode(&wire).expect("decode");
            let parsed = check_coinbase(&back, Some(&cb)).expect("coinbase");
            let (rebuilt, hash) = share_hash(&back, &parsed).expect("hash");
            assert_eq!(rebuilt.merkle_root, hdr.merkle_root, "n={n}");
            assert_eq!(rebuilt.serialize(), hdr.serialize(), "n={n}");
            assert_eq!(hash, hdr.pow_hash(), "n={n}");
        }
    }

    /// A share for a job a block or two behind the tip is late, not invalid: the miner did
    /// the work. Anything older than the lag window is still refused.
    #[test]
    fn a_share_a_block_or_two_late_is_still_credited() {
        let cb = issued_split();
        let (_, share) = gateway_share(&cb, &txids(4));
        for behind in 0..=HEIGHT_LAG {
            let c = ShareContext {
                issued: Some(&cb),
                tip_height: u64::from(HEIGHT) + behind,
                now: 1_800_000_000,
                min_diff: 1,
            };
            assert!(
                verify_share_against(&share, &c, &easy()).is_ok(),
                "{behind} block(s) behind should be credited"
            );
        }
        let too_old = ShareContext {
            issued: Some(&cb),
            tip_height: u64::from(HEIGHT) + HEIGHT_LAG + 1,
            now: 1_800_000_000,
            min_diff: 1,
        };
        assert_eq!(
            verify_share_against(&share, &too_old, &easy()),
            Err(mining::REJECT_STALE),
            "past the lag window it is stale"
        );
    }

    #[test]
    fn accepts_a_good_share_and_counts_its_work() {
        let cb = issued_split();
        let (_, mut share) = gateway_share(&cb, &txids(20));
        share.target_byte = 5;
        let v = verify_share_against(&share, &ctx(&cb), &easy()).expect("accepted");
        assert_eq!(v.work, 32);
        assert_eq!(v.height, HEIGHT);
        assert!(!v.is_block_candidate);
    }

    #[test]
    fn claimed_difficulty_must_be_earned() {
        let cb = issued_split();
        let (_, share) = gateway_share(&cb, &txids(20));
        // real share target for difficulty 1 needs 2^32 work; a single nonce will not have it
        assert_eq!(verify_share(&share, &ctx(&cb)), Err(mining::REJECT_HIGH_HASH));
    }

    #[test]
    fn rejects_a_coinbase_that_underpays_a_contributor() {
        let cb = issued_split();
        let mut robbed = cb.clone();
        robbed.outputs[1].sats = 1;
        robbed.outputs[0].sats = 299_999_999;
        let (_, share) = gateway_share(&robbed, &txids(10));
        assert_eq!(
            verify_share_against(&share, &ctx(&cb), &easy()),
            Err(mining::REJECT_BAD_COINBASE_ID)
        );
    }

    #[test]
    fn rejects_a_coinbase_that_pays_only_the_sender() {
        let cb = issued_split();
        let selfish = CoinbaserV2 {
            id: cb.id,
            outputs: vec![CoinbaserOutput { sats: cb.value_sum(), script: p2wsh(0xee) }],
        };
        let (_, share) = gateway_share(&selfish, &txids(10));
        assert_eq!(
            verify_share_against(&share, &ctx(&cb), &easy()),
            Err(mining::REJECT_BAD_COINBASE_ID)
        );
    }

    #[test]
    fn accepts_a_gateway_rescaled_split() {
        let cb = issued_split();
        let rescaled = cb.scale_to(cb.value_sum() + 41_337);
        let (_, share) = gateway_share(&rescaled, &txids(10));
        assert!(verify_share_against(&share, &ctx(&cb), &easy()).is_ok());
    }

    #[test]
    fn rejects_missing_sections_and_bad_ids() {
        let cb = issued_split();
        let (_, base) = gateway_share(&cb, &txids(4));

        let mut no_cb = base.clone();
        no_cb.coinbase = None;
        assert_eq!(
            verify_share_against(&no_cb, &ctx(&cb), &easy()),
            Err(mining::REJECT_BAD_COINBASE_ID)
        );

        let mut no_job = base.clone();
        no_job.job = None;
        assert_eq!(
            verify_share_against(&no_job, &ctx(&cb), &easy()),
            Err(mining::REJECT_BAD_JOB_ID)
        );

        let mut wrong_id = base.clone();
        wrong_id.coinbase_id = cb.id.wrapping_add(1);
        assert_eq!(
            verify_share_against(&wrong_id, &ctx(&cb), &easy()),
            Err(mining::REJECT_BAD_COINBASE_ID)
        );

        let unknown_split = ShareContext { issued: None, tip_height: 0, now: 0, min_diff: 1 };
        assert_eq!(
            verify_share_against(&base, &unknown_split, &easy()),
            Err(mining::REJECT_BAD_COINBASE_ID)
        );
    }

    #[test]
    fn rejects_stale_height_and_clock() {
        let cb = issued_split();
        let (_, share) = gateway_share(&cb, &txids(4));

        let old = ShareContext { issued: Some(&cb), tip_height: u64::from(HEIGHT) + 10, now: 0, min_diff: 1 };
        assert_eq!(verify_share_against(&share, &old, &easy()), Err(mining::REJECT_STALE));

        let skewed = ShareContext { issued: Some(&cb), tip_height: 0, now: 1_800_000_000 + 8000, min_diff: 1 };
        assert_eq!(verify_share_against(&share, &skewed, &easy()), Err(mining::REJECT_STALE));
    }

    #[test]
    fn tampered_branches_change_the_hash() {
        let cb = issued_split();
        let (hdr, mut share) = gateway_share(&cb, &txids(9));
        if let Some(j) = share.job.as_mut() {
            j.merkle_branches[0][0] ^= 0xff;
        }
        let parsed = check_coinbase(&share, Some(&cb)).expect("coinbase");
        let (_, hash) = share_hash(&share, &parsed).expect("hash");
        assert_ne!(hash, hdr.pow_hash());
    }

    /// Grinds a real difficulty-1 share (about 2^32 blake2b passes) and puts it through the
    /// verifier, which is the only way to exercise the accept path against work that was
    /// actually done. Ignored by default; run with:
    ///   cargo test -p lazarus-protocol --release mined_share -- --ignored --nocapture
    #[test]
    #[ignore]
    fn a_mined_share_is_accepted() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        use std::sync::Arc;

        let cb = issued_split();
        let txs = txids(200);
        let (hdr, share) = gateway_share(&cb, &txs);
        let parsed = check_coinbase(&share, Some(&cb)).expect("coinbase");
        let (rebuilt, _) = share_hash(&share, &parsed).expect("hash");
        assert_eq!(rebuilt.serialize(), hdr.serialize());

        let prev_hidden = pow::prevblock_hidden(&rebuilt.prev_block);
        let hash1 = rebuilt.hash1();
        let target = pow::target_for_pot(0);

        let found = Arc::new(AtomicU64::new(u64::MAX));
        let stop = Arc::new(AtomicBool::new(false));
        let threads: u32 = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4);
        let mut handles = Vec::new();
        for t in 0..threads {
            let found = found.clone();
            let stop = stop.clone();
            handles.push(std::thread::spawn(move || {
                let mut n = t;
                while !stop.load(Ordering::Relaxed) {
                    if pow::meets_target(&pow::asic_hash(&prev_hidden, &hash1, n, 0, 0, 0), &target) {
                        found.store(u64::from(n), Ordering::SeqCst);
                        stop.store(true, Ordering::SeqCst);
                        return;
                    }
                    match n.checked_add(threads) {
                        Some(x) => n = x,
                        None => return,
                    }
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        let raw = found.load(Ordering::SeqCst);
        assert!(raw != u64::MAX, "nonce space exhausted without a difficulty-1 hit");
        let nonce = raw as u32;

        let mut mined = share.clone();
        let b = mined.blake2b.as_mut().unwrap();
        b.sia_nonce[0..4].copy_from_slice(&nonce.to_le_bytes());
        b.sia_nonce[4..8].copy_from_slice(&0u32.to_le_bytes());

        let v = verify_share(&mined, &ctx(&cb)).expect("a mined share must verify");
        assert_eq!(v.work, 1);
        assert!(!v.is_block_candidate);
        println!("mined nonce {:08x} hash {}", nonce, hex::encode(v.hash));

        // the same proof of work, with the coinbase rewritten to pay only the sender
        let selfish = CoinbaserV2 {
            id: cb.id,
            outputs: vec![CoinbaserOutput { sats: cb.value_sum(), script: p2wsh(0xee) }],
        };
        let (_, mut stolen) = gateway_share(&selfish, &txs);
        let sb = stolen.blake2b.as_mut().unwrap();
        sb.sia_nonce[0..4].copy_from_slice(&nonce.to_le_bytes());
        sb.sia_nonce[4..8].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(verify_share(&stolen, &ctx(&cb)), Err(mining::REJECT_BAD_COINBASE_ID));
    }
}
