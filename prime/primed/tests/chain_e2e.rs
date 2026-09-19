//! End-to-end: a job's target is held to the node's chain, a difficulty that was not hashed
//! earns only what its hash does, no honest gateway is refused for where its node is, and
//! asking the node about work "ahead" of it cannot be turned into an RPC flood.
//!
//! Against a real `primed` and a stand-in node. One test grinds a diff-1 share (~4.3 GH of
//! BLAKE2b, about a minute on a desktop), so they are ignored by default:
//!
//!     cargo test --release -p primed --test chain_e2e -- --ignored --nocapture

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::crypto::Identity;
use datum_wire::mining;

const TIP_BITS: u32 = 0x1903_a30c;

fn chain(next_bits: Option<u32>) -> (Chain, [u8; 32]) {
    let tip_wire = [0x42; 32];
    let c = Chain {
        height: HEIGHT - 1,
        tip: node_hex(&tip_wire),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits,
    };
    (c, tip_wire)
}

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

/// Wait for the Prime to have read a tip from the node.
fn wait_for_tip(p: &Primed) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(p)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip: {}", stats(p));
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Turn a stock-shaped share into the whole-coinbase form: everything in `coinb1`, no target
/// byte named. Its difficulty is then outside what is hashed.
fn make_whole(s: &mut datum_wire::mining::PowSubmit) {
    let cb = s.coinbase.as_mut().unwrap();
    cb.coinb1.extend_from_slice(&[0u8; 12]);
    let mut rest = std::mem::take(&mut cb.coinb2);
    cb.coinb1.append(&mut rest);
    s.job.as_mut().unwrap().target_byte_index = 0;
}

#[test]
#[ignore]
fn a_job_is_held_to_the_target_the_chain_sets_and_nobody_honest_is_refused() {
    // Ground before anything connects: a session with nothing to say is dropped after five
    // minutes, and on a busy machine a diff-1 share can take longer than that to find.
    let (_, tip) = chain(None);
    let now = unix_now();
    let mut good = pool_only_share(3, HEIGHT, tip, TIP_BITS, 0, now);
    grind_diff1(&mut good);
    // a gateway whose node is on a competing tip at our height
    let mut other_branch = pool_only_share(7, HEIGHT, [0x99; 32], TIP_BITS, 0, now);
    grind_diff1(&mut other_branch);
    // a gateway (an older lazarus-gateway away from the pool host, say) whose shares do not
    // carry their difficulty in the hash
    let mut whole = pool_only_share(6, HEIGHT, tip, TIP_BITS, 0, now);
    make_whole(&mut whole);
    grind_diff1(&mut whole);

    for next_bits in [Some(TIP_BITS), None] {
        let pool = Identity::generate();
        let (c, _) = chain(next_bits);
        let node = MockNode::start(c);
        // not the house gateway: the loopback these tests connect over earns no trust
        let (primed, port) = start_primed(&pool, &format!("{}\nhouse-loopback = false", node.config(0.2)));
        wait_for_tip(&primed);
        let mut gw = Gateway::connect(port, &pool, &Identity::generate());
        let taken = |r: (u8, u16)| r.0 == mining::ACCEPTED || r.0 == mining::ACCEPTED_TENTATIVELY;

        // regtest's target, under which every share would be a block: refused before any
        // work is looked at, on our tip, on another one, and a block ahead
        for (slot, height, prev) in [(1, HEIGHT, tip), (2, HEIGHT, [0x99; 32]), (4, HEIGHT + 1, [0x43; 32])] {
            let easy = pool_only_share(slot, height, prev, 0x207f_ffff, 0, now);
            assert_eq!(gw.submit(&easy), (mining::REJECTED, mining::REJECT_TARGET_MISMATCH), "next_bits {next_bits:?}");
        }
        // two blocks past our tip is our node well behind, or made up
        let far = pool_only_share(5, HEIGHT + 2, [0x44; 32], TIP_BITS, 0, now);
        assert_eq!(gw.submit(&far), (mining::REJECTED, mining::REJECT_STALE_BLOCK));

        // real work is taken: on our tip, and on a tip our node does not have
        let r = gw.submit(&good);
        assert!(taken(r), "on the tip: {r:?}");
        let r = gw.submit(&other_branch);
        assert!(taken(r), "on a competing tip: {r:?}");
        let st = settled_stats(&primed);
        assert_eq!(st["totals"]["shares_accepted"], 2, "{st}");
        assert_eq!(st["window"]["work"], 2, "{st}");

        // A share whose difficulty is outside its hash is taken too, and is worth what its
        // hash earns at the pool's threshold (2^20: for a diff-1 hash, as good as never),
        // whatever it claims. Accepted, no reject, nothing to gain.
        let r = gw.submit(&whole);
        assert!(taken(r), "difficulty not in the hash: {r:?}");
        let st = settled_stats(&primed);
        assert_eq!(st["totals"]["shares_accepted"], 3, "{st}");
        assert_eq!(st["totals"]["uncommitted_shares"], 1, "{st}");
        assert_eq!(st["window"]["work"], 2, "credited by hash alone: {st}");
        assert_eq!(st["totals"]["shares_rejected"], 4, "only the made-up jobs: {st}");
        assert_eq!(st["blocks"].as_array().map_or(0, Vec::len), 0, "no block was recorded: {st}");
    }
}

#[test]
#[ignore]
fn work_ahead_of_the_node_cannot_flood_it_with_rpcs() {
    let pool = Identity::generate();
    let (c, _) = chain(Some(TIP_BITS));
    let node = MockNode::start(c);
    // a slow poller, so the calls counted are the sessions'
    let (primed, port) = start_primed(&pool, &node.config(30.0));
    wait_for_tip(&primed);
    std::thread::sleep(Duration::from_millis(500));
    let before = node.calls.load(Ordering::Relaxed);
    let started = Instant::now();
    let now = unix_now();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let pool_pk = Identity::from_secret_bytes(&pool.secret_bytes());
            std::thread::spawn(move || {
                let mut gw = Gateway::connect(port, &pool_pk, &Identity::generate());
                let ahead = pool_only_share(1, HEIGHT + 3, [0x55; 32], TIP_BITS, 0, now);
                // under the reject-flood limit (2000 in 10 s), for three seconds
                while started.elapsed() < Duration::from_secs(3) {
                    assert_eq!(gw.submit(&ahead), (mining::REJECTED, mining::REJECT_STALE_BLOCK));
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let calls = node.calls.load(Ordering::Relaxed) - before;
    let allowed = started.elapsed().as_millis() as u64 / 250 + 2;
    eprintln!(
        "8 sessions submitting work ahead of the tip for {:?}: {calls} tip reads (at most {allowed})",
        started.elapsed()
    );
    assert!(calls >= 2, "sessions do get the node asked: {calls}");
    assert!(calls <= allowed, "{calls} > {allowed}");
}

/// The pool's node being slow to start is not the gateways' fault: until it has given a tip
/// their work is taken and credited. What is not taken on a gateway's word is a block.
#[test]
#[ignore]
fn before_the_node_gives_a_tip_work_is_taken_but_a_block_is_not() {
    // regtest's target: this share "meets nbits", as every share would
    let mut share = pool_only_share(1, HEIGHT, [0x42; 32], 0x207f_ffff, 0, unix_now());
    grind_diff1(&mut share);
    let pool = Identity::generate();
    let (primed, port) = start_primed(&pool, "rpc = \"http://127.0.0.1:9\"\npoll = 5.0");
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (status, code) = gw.submit(&share);
    assert!(status == mining::ACCEPTED || status == mining::ACCEPTED_TENTATIVELY, "0x{status:02x} {code}");
    let st = settled_stats(&primed);
    assert_eq!(st["totals"]["shares_accepted"], 1, "{st}");
    assert_eq!(st["window"]["work"], 1, "{st}");
    assert_eq!(st["blocks"].as_array().map_or(0, Vec::len), 0, "not recorded as a block: {st}");
    assert_eq!(st["totals"]["block_candidates"], 0, "{st}");
}

/// A split is a reading of the window at one moment. The job names the coinbaser it used, and
/// that name is honoured for the block the coinbaser was asked for and two after (a gateway
/// racing a new tip is late, not lying), and no further: a gateway cannot ask once, when its
/// share of the window is at its best, and mine on it for ever.
#[test]
#[ignore]
fn a_coinbaser_is_good_for_a_couple_of_blocks_and_no_longer() {
    let pool = Identity::generate();
    let (c, tip_a) = chain(Some(TIP_BITS));
    let (tip_b, tip_c, tip_d) = ([0x43; 32], [0x44; 32], [0x45; 32]);
    // All four shares are ground before anything connects (see above). The coinbaser id a
    // job names is not part of the header, so it is filled in once the Prime has issued one.
    let now = unix_now();
    let mut shares = [
        pool_only_share(1, HEIGHT, tip_a, TIP_BITS, 0, now),
        pool_only_share(2, HEIGHT + 1, tip_b, TIP_BITS, 0, now),
        pool_only_share(3, HEIGHT + 2, tip_c, TIP_BITS, 0, now),
        pool_only_share(4, HEIGHT + 3, tip_d, TIP_BITS, 0, now),
    ];
    for s in &mut shares {
        grind_diff1(s);
    }

    let node = MockNode::start(c);
    let (primed, port) = start_primed(&pool, &format!("{}\nhouse-loopback = false", node.config(0.2)));
    wait_for_tip(&primed);
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());

    // With nobody in the window the split is one output, the pool's, which is exactly what
    // these coinbases pay: held to the coinbaser they name, that is the split, in full
    // (accepted). With no coinbaser to hold them to, it is a pool-only coinbase (tentative).
    let id = gw.request_coinbaser(VALUE, &tip_a);
    let tips = [tip_a, tip_b, tip_c, tip_d];
    let expect = [mining::ACCEPTED, mining::ACCEPTED, mining::ACCEPTED, mining::ACCEPTED_TENTATIVELY];
    for (i, share) in shares.iter_mut().enumerate() {
        if i > 0 {
            let height = HEIGHT - 1 + i as u32;
            *node.chain.lock().unwrap() = Chain {
                height,
                tip: node_hex(&tips[i]),
                parent: node_hex(&tips[i - 1]),
                bits: TIP_BITS,
                next_bits: Some(TIP_BITS),
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while stats(&primed)["node"]["height"] != height {
                assert!(Instant::now() < deadline, "primed never saw the tip at {height}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        share.job.as_mut().unwrap().coinbaser_id = id;
        assert_eq!(gw.submit(share).0, expect[i], "share {i}: a coinbaser issued {i} block(s) back");
    }
}
