//! End-to-end: a hostile gateway against a real `primed`, replaying one genuine share.
//!
//! This is the loop from the September 2026 review (finding #1): submit a valid share, then
//! resubmit it with a job section that differs in a field nothing reads, then again from a
//! fresh connection. Before the fix each resubmission emptied the per-job dedup set and was
//! credited again. Now the pool credits it once and rejects every replay as duplicate work.
//!
//! The share is real diff-1 work — about 2^32 BLAKE2b hashes, a minute or two on a desktop
//! across all cores — so the test is ignored by default:
//!
//!     cargo test --release -p primed --test replay_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::crypto::Identity;
use datum_wire::mining;

#[test]
#[ignore]
fn one_share_is_credited_once_no_matter_how_it_is_resubmitted() {
    let pool = Identity::generate();
    let gw_identity = Identity::generate();
    // nothing listens here: shares are taken without a chain to hold them to
    let (primed, port) = start_primed(&pool, "rpc = \"http://127.0.0.1:9\"\npoll = 5.0");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32;

    let mut share = pool_only_share(3, HEIGHT, [0x77; 32], 0x193c_2d40, 0, now);
    grind_diff1(&mut share);

    let mut gw = Gateway::connect(port, &pool, &gw_identity);
    let (status, code) = gw.submit(&share);
    assert!(
        status == mining::ACCEPTED || status == mining::ACCEPTED_TENTATIVELY,
        "genuine share accepted (status 0x{status:02x}, code {code})"
    );

    // The finding's loop: alternate two job sections differing only in txn_total_weight,
    // resend the same share each time. Every one must be a duplicate now.
    for i in 0..10u32 {
        let mut again = share.clone();
        again.job.as_mut().unwrap().txn_total_weight = if i % 2 == 0 { 4_000_000 } else { 0 };
        let (status, code) = gw.submit(&again);
        assert_eq!(
            (status, code),
            (mining::REJECTED, mining::REJECT_DUPLICATE_WORK),
            "replay #{i} via job-section flip"
        );
    }
    // Same share, no sections at all (cached job)
    let mut bare = share.clone();
    bare.job = None;
    bare.coinbase = None;
    assert_eq!(gw.submit(&bare), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK));
    // Same share on a different job slot
    let mut other_slot = share.clone();
    other_slot.job_id = 7;
    assert_eq!(gw.submit(&other_slot), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK));
    drop(gw);

    // A reconnect (fresh session, same or different gateway key) is still a duplicate.
    std::thread::sleep(Duration::from_millis(200));
    let mut gw2 = Gateway::connect(port, &pool, &gw_identity);
    assert_eq!(gw2.submit(&share), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK), "replay after reconnect");
    let mut gw3 = Gateway::connect(port, &pool, &Identity::generate());
    assert_eq!(gw3.submit(&share), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK), "replay from another key");

    let st = settled_stats(&primed);
    assert_eq!(st["totals"]["shares_accepted"], 1, "{st}");
    assert_eq!(st["totals"]["work_accepted"], 1);
    assert_eq!(st["totals"]["shares_rejected"], 14);
    assert_eq!(st["totals"]["seen_shares"], 1);
    assert_eq!(st["window"]["work"], 1);
    eprintln!("credited work = {} after 1 genuine share and 14 replays", st["window"]["work"]);
}
