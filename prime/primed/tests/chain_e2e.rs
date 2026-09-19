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

/// A found block's books, end to end. The carry its coinbase paid comes off the moment the
/// candidate is seen (the next coinbaser must not hand it out again); if the node then says
/// the block is not in the chain, exactly that goes back; and when the node does have it, it
/// is settled.
#[test]
#[ignore]
fn a_found_block_is_booked_when_seen_and_settled_when_the_node_has_it() {
    const REGTEST_BITS: u32 = 0x207f_ffff; // every share is a block, honestly: the node says so
    const OWED: u64 = 1_000_000;
    let miner = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
    let tip = [0x42; 32];
    let mut share = pool_only_share(1, HEIGHT, tip, REGTEST_BITS, 0, unix_now());
    grind_diff1(&mut share);

    let pool = Identity::generate();
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&tip),
        parent: node_hex(&[0x41; 32]),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    });
    let cfg = format!("{}\nhouse-loopback = false", node.config(0.2));
    let (primed, port) = start_primed_seeded(&pool, &cfg, |dir| {
        let meta = serde_json::json!({"target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "carry": {miner: OWED}, "rebate_owed": 0});
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
    });
    wait_for_tip(&primed);
    let carry = |p: &Primed| settled_stats(p)["window"]["carry_total_sats"].as_u64().unwrap();
    assert_eq!(carry(&primed), OWED);

    // the split for this template hands the miner its carry; the gateway then mines a coinbase
    // that pays only the pool, so the block owes that carry through the make-good instead
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    share.job.as_mut().unwrap().coinbaser_id = gw.request_coinbaser(VALUE, &tip);
    let (status, code) = gw.submit(&share);
    assert_eq!(status, mining::ACCEPTED_TENTATIVELY, "code {code}");

    let st = settled_stats(&primed);
    let block = st["blocks"].as_array().unwrap().last().expect("the candidate is recorded").clone();
    let hash = block["hash"].as_str().unwrap().to_string();
    assert_eq!(block["kind"], "pool-only", "{block}");
    assert_eq!(block["owed_sats"], OWED, "{block}");
    assert_eq!(block["settled"], false);
    assert_eq!(block["books"]["debits_live"], true, "{block}");
    assert_eq!(block["books"]["credits_live"], false, "{block}");
    assert_eq!(st["window"]["carry_total_sats"], 0, "off the books at once: {st}");

    let wait_for = |what: &str, done: &dyn Fn(&serde_json::Value) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let st = settled_stats(&primed);
            let b = st["blocks"].as_array().unwrap().iter().rev().find(|b| b["hash"] == hash.as_str()).unwrap().clone();
            if done(&b) {
                return st;
            }
            assert!(Instant::now() < deadline, "never {what}: {b}");
        }
    };
    // the node: not in the main chain
    node.confirmations.lock().unwrap().insert(hash.clone(), -1);
    let st = wait_for("orphaned", &|b| b["kind"] == "orphan:pool-only");
    assert_eq!(st["window"]["carry_total_sats"], OWED, "exactly what it took comes back: {st}");

    // ...and then it is after all (the branch it was on won)
    node.confirmations.lock().unwrap().insert(hash.clone(), 2);
    let st = wait_for("settled", &|b| b["settled"] == true);
    let b = st["blocks"].as_array().unwrap().iter().rev().find(|b| b["hash"] == hash.as_str()).unwrap().clone();
    assert_eq!(b["kind"], "pool-only", "{b}");
    assert_eq!(b["books"]["debits_live"], true, "{b}");
    assert_eq!(b["books"]["credits_live"], true, "{b}");
    assert_eq!(st["window"]["carry_total_sats"], 0, "{st}");
}


/// Wait for the Prime to answer a payout request; returns the answer once `done` accepts it.
fn payout_answer(p: &Primed, id: &str, done: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let path = p.dir.join("payouts").join(format!("{id}.json"));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(v) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
            if done(&v) {
                return v;
            }
        }
        assert!(std::time::Instant::now() < deadline, "no answer for payout {id}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn payout_request(p: &Primed, id: &str, body: serde_json::Value) {
    let dir = p.dir.join("payouts");
    std::fs::create_dir_all(&dir).unwrap();
    let _ = std::fs::remove_file(dir.join(format!("{id}.json")));
    // written whole, then named: the Prime must never read half a request
    let tmp = dir.join(format!("{id}.tmp"));
    std::fs::write(&tmp, body.to_string()).unwrap();
    std::fs::rename(tmp, dir.join(format!("{id}.request.json"))).unwrap();
}

/// A miner who left with less than the floor on the books: listed once it has been gone long
/// enough, paid by the next coinbase, or set aside and paid by hand, and never both.
#[test]
fn a_stale_balance_is_paid_by_the_coinbase_or_by_hand_and_never_twice() {
    const BITS: u32 = 0x207f_ffff;
    let left = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
    let left_script = "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262";
    let recent = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
    let tip = [0x42; 32];
    let now = unix_now();
    let pool = Identity::generate();
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&tip),
        parent: node_hex(&[0x41; 32]),
        bits: BITS,
        next_bits: Some(BITS),
    });
    let cfg = format!(
        "{}\nhouse-loopback = false\nmin-payout = 500000\nstale-after-days = 7\nstale-min-payout = 10000",
        node.config(0.2)
    );
    let (primed, port) = start_primed_seeded(&pool, &cfg, |dir| {
        let meta = serde_json::json!({
            "target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "rebate_owed": 0,
            "carry": {left: 499_226, recent: 400_000, "not-an-address": 50_000},
            "last_seen": {left: now - 8 * 86_400, recent: now - 86_400, "not-an-address": now - 30 * 86_400},
        });
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("identities.txt"), format!("{left}\n{recent}\nnot-an-address\n")).unwrap();
    });
    wait_for_tip(&primed);

    // listed: the one gone eight days, not the one gone a day; the unpayable name is shown as such
    let st = settled_stats(&primed);
    let stale = &st["window"]["stale"];
    assert_eq!(stale["after_days"], 7, "{stale}");
    let ids: Vec<&str> = stale["balances"].as_array().unwrap().iter().map(|b| b["identity"].as_str().unwrap()).collect();
    assert_eq!(ids, [left, "not-an-address"], "{stale}");
    assert_eq!(stale["balances"][0]["payable"], true);
    assert_eq!(stale["balances"][1]["payable"], false);

    // the coinbase path: the next coinbaser pays it, whole, under the 500 000 floor
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (_, outputs) = gw.request_coinbaser_outputs(VALUE, &tip);
    let paid_left = |outs: &[datum_wire::coinbaser::Output]| {
        outs.iter().filter(|o| hex::encode(&o.script) == left_script).map(|o| o.sats).sum::<u64>()
    };
    assert_eq!(paid_left(&outputs), 499_226, "{outputs:?}");
    assert_eq!(outputs.iter().map(|o| o.sats).sum::<u64>(), VALUE);

    // by hand instead: a hold takes it off the books. A wrong amount, a miner who is not
    // stale and a name that is no address are refused one by one.
    payout_request(
        &primed,
        "batch-1",
        serde_json::json!({"action": "hold", "entries": [[left, 499_226], [recent, 400_000], ["not-an-address", 50_000]]}),
    );
    let held = payout_answer(&primed, "batch-1", |v| v["status"] == "held");
    assert_eq!(held["entries"], serde_json::json!([[left, 499_226]]), "{held}");
    assert_eq!(held["ready_height"], HEIGHT - 1 + 4, "{held}");
    let why: Vec<&str> = held["skipped"].as_array().unwrap().iter().map(|s| s["reason"].as_str().unwrap()).collect();
    assert_eq!(why, ["not stale", "not an address"], "{held}");
    assert!(!primed.dir.join("payouts/batch-1.request.json").exists(), "a request is handled once");
    let st = settled_stats(&primed);
    assert_eq!(st["window"]["stale"]["held_sats"], 499_226);
    assert_eq!(st["window"]["carry_total_sats"], 450_000);
    // and no coinbaser issued from now on pays it
    let (_, outputs) = gw.request_coinbaser_outputs(VALUE + 1, &tip);
    assert_eq!(paid_left(&outputs), 0, "{outputs:?}");

    // "paid" is checked against the node. Unknown to it: keep waiting. Confirmed but a sat
    // short: refused, and the hold stays.
    let txid = "ab".repeat(32);
    payout_request(&primed, "batch-1", serde_json::json!({"action": "paid", "txid": txid}));
    let waiting = payout_answer(&primed, "batch-1", |v| v["status"] == "waiting");
    assert_eq!(waiting["retry"], true, "{waiting}");
    assert!(primed.dir.join("payouts/batch-1.request.json").exists(), "it is looked at again");
    std::fs::remove_file(primed.dir.join("payouts/batch-1.request.json")).unwrap();

    let short = "cd".repeat(32);
    let tx = |sats: f64| serde_json::json!({"confirmations": 3, "vout": [{"value": sats, "scriptPubKey": {"hex": left_script}}]});
    node.txs.lock().unwrap().insert(short.clone(), tx(0.00499225));
    payout_request(&primed, "batch-1", serde_json::json!({"action": "paid", "txid": short}));
    let refused = payout_answer(&primed, "batch-1", |v| v["ok"] == false && v["retry"].is_null());
    assert!(refused["error"].as_str().unwrap().contains("paid 499225"), "{refused}");
    // an id that names no hold finishes nothing
    payout_request(&primed, "batch-9", serde_json::json!({"action": "paid", "txid": short}));
    assert_eq!(payout_answer(&primed, "batch-9", |v| v["ok"] == false)["error"], "no such hold");
    assert_eq!(settled_stats(&primed)["window"]["stale"]["held_sats"], 499_226, "still held");

    // abandoned: released, and it is carry (and stale, and in the next coinbaser) again
    payout_request(&primed, "batch-1", serde_json::json!({"action": "release"}));
    let released = payout_answer(&primed, "batch-1", |v| v["status"] == "released");
    assert_eq!(released["released_sats"], 499_226);
    let st = settled_stats(&primed);
    assert_eq!((st["window"]["stale"]["held_sats"].as_u64(), st["window"]["carry_total_sats"].as_u64()), (Some(0), Some(949_226)));

    // held again and really paid: off the books for good
    payout_request(&primed, "batch-2", serde_json::json!({"action": "hold", "entries": [[left, 499_226]]}));
    payout_answer(&primed, "batch-2", |v| v["status"] == "held");
    let good = "ef".repeat(32);
    node.txs.lock().unwrap().insert(good.clone(), tx(0.00499226));
    payout_request(&primed, "batch-2", serde_json::json!({"action": "paid", "txid": good}));
    let paid = payout_answer(&primed, "batch-2", |v| v["status"] == "paid");
    assert_eq!(paid["paid_sats"], 499_226, "{paid}");
    let st = settled_stats(&primed);
    assert_eq!((st["window"]["stale"]["held_sats"].as_u64(), st["window"]["carry_total_sats"].as_u64()), (Some(0), Some(450_000)));
    let log = std::fs::read_to_string(primed.dir.join("payouts/payouts.jsonl")).unwrap();
    assert!(log.lines().count() >= 4, "every change is on the record:\n{log}");
    // and it survives a restart: window.json has no carry for it and no hold
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(primed.dir.join("window.json")).unwrap()).unwrap();
    assert!(meta["carry"].get(left).is_none(), "{meta}");
    assert!(meta.get("holds").is_none(), "{meta}");
}
