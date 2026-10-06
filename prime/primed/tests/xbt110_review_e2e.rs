//! XBT-110 review: two ways the carry a found block paid is still paid again after 23f7eeb.
//!
//! Both tests are written to FAIL on `prime/xbt-010-086` @ f4f0db3: each asserts what the books
//! should say and shows what they say instead. They are against a real `primed` and a stand-in
//! node, and grind real diff-1 shares (two between them, a minute or two across all cores), so
//! they are ignored by default:
//!
//!     cargo test --release -p primed --test xbt110_review_e2e -- --ignored --nocapture --test-threads=1

mod common;

use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::coinbaser::Output;
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};
use datum_wire::pow::Hash;
use datum_wire::verify::{self, JobSlot, Policy};

/// Every share is a block, and the node says so (`a_found_block_is_booked_...` in chain_e2e).
const REGTEST_BITS: u32 = 0x207f_ffff;
/// Carry the pool holds for one miner who has no work left in the window.
const OWED: u64 = 1_000_000;
const MINER: &str = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
const MINER_SCRIPT: &str = "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262";
const TIP_A: Hash = [0x42; 32];

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

fn wait_for_tip(p: &Primed) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(p)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip: {}", stats(p));
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The split of a window that holds nothing but `MINER`'s carry: the carry, then the pool.
fn the_split() -> Vec<Output> {
    vec![
        Output { sats: OWED, script: hex::decode(MINER_SCRIPT).unwrap() },
        Output { sats: VALUE - OWED, script: hex::decode(POOL_SCRIPT).unwrap() },
    ]
}

/// A share at `height` on `prev` whose coinbase pays [`the_split`] exactly.
fn split_share(slot: u8, height: u32, prev: Hash, now: u32) -> PowSubmit {
    let outs: Vec<TxOut> = the_split().into_iter().map(|o| TxOut { value: o.sats, script: o.script }).collect();
    let mut s = pool_only_share(slot, height, prev, REGTEST_BITS, 0, now);
    let (cb, tidx, split_at) = coinbase::build(height, b"Lazarus", &outs, 0);
    s.coinbase = Some(CoinbaseSection {
        coinbase_id: 0,
        coinb1: cb[..split_at].to_vec(),
        coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
    });
    s.job.as_mut().unwrap().target_byte_index = tidx as u16;
    s
}

/// The hash of a ground share, in the byte order a job's `prev_hash` carries: the block it is.
fn block_hash(s: &PowSubmit) -> Hash {
    let issued = the_split();
    let pool = hex::decode(POOL_SCRIPT).unwrap();
    let policy = Policy {
        pool_script: &pool,
        issued: Some(&issued),
        tolerance: 0,
        now: 0,
        min_pot: 0,
        gateway_script: None,
        empty_solo_fee_bps: 0,
        trusted_target: false,
        uncommitted_pot: 20,
        held_split: false,
    };
    let mut slot = JobSlot::default();
    slot.absorb(s).unwrap();
    verify::verify(&mut slot, s, &policy).expect("the ground share verifies").hash
}

/// The first block, ground once for both tests: a full split at `HEIGHT` on `TIP_A`.
fn first_block() -> &'static (PowSubmit, Hash) {
    static FIRST: OnceLock<(PowSubmit, Hash)> = OnceLock::new();
    FIRST.get_or_init(|| {
        let mut s = split_share(1, HEIGHT, TIP_A, unix_now());
        grind_diff1(&mut s);
        let h = block_hash(&s);
        (s, h)
    })
}

fn seed_carry(dir: &std::path::Path) {
    let meta = serde_json::json!({"target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "carry": {MINER: OWED}, "rebate_owed": 0});
    std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
}

fn node_at_a() -> MockNode {
    MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP_A),
        parent: node_hex(&[0x41; 32]),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    })
}

fn carry_total(p: &Primed) -> u64 {
    settled_stats(p)["window"]["carry_total_sats"].as_u64().unwrap()
}

/// Finding 1. A coinbaser is honoured for the block it was asked for and the two after
/// (`COINBASER_GRACE_BLOCKS`), and it is a reading of carry from before any of them was found.
/// 23f7eeb stops the shared snapshot offering paid carry to *new* replies; a reply already in a
/// gateway's hands still offers it. The pool finds a block that pays a miner's carry; the same
/// (or any) gateway finds the next block on the coinbaser it held from before: the coinbase pays
/// that carry a second time, out of the pool's remainder, there is nothing left on the books to
/// take it from, and nothing says so.
#[test]
#[ignore]
fn a_block_on_a_coinbaser_from_before_the_last_find_does_not_pay_its_carry_again() {
    let (first, h1) = first_block().clone();
    // the next block, on top of the first, paying the very same list
    let mut second = split_share(2, HEIGHT + 1, h1, unix_now());
    grind_diff1(&mut second);

    let pool = Identity::generate();
    let node = node_at_a();
    let cfg = format!("{}\nhouse-loopback = false", node.config(0.2));
    let (primed, port) = start_primed_seeded(&pool, &cfg, seed_carry);
    wait_for_tip(&primed);
    assert_eq!(carry_total(&primed), OWED);

    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (id, outputs) = gw.request_coinbaser_outputs(VALUE, &TIP_A);
    assert_eq!(outputs, the_split(), "the one coinbaser this session is ever sent");

    // block 1: the split in full, and the carry it paid comes off the books at once
    let mut first = first;
    first.job.as_mut().unwrap().coinbaser_id = id;
    assert_eq!(gw.submit(&first).0, mining::ACCEPTED);
    let st = settled_stats(&primed);
    let b1 = st["blocks"].as_array().unwrap().last().expect("block 1 is recorded").clone();
    assert_eq!((b1["kind"].as_str(), b1["carry_paid"].as_u64()), (Some("split"), Some(OWED)), "{b1}");
    assert_eq!(st["window"]["carry_total_sats"], 0, "{st}");

    // the node takes it: block 1 is the tip, and settled
    *node.chain.lock().unwrap() = Chain {
        height: HEIGHT,
        tip: node_hex(&h1),
        parent: node_hex(&TIP_A),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let st = settled_stats(&primed);
        if st["node"]["height"] == HEIGHT && st["blocks"][0]["settled"] == true {
            break;
        }
        assert!(Instant::now() < deadline, "block 1 never settled: {st}");
    }

    // block 2, one height on, mined on the coinbaser from before block 1 was found. Within the
    // grace, so it is the split in full and accepted as such.
    second.job.as_mut().unwrap().coinbaser_id = id;
    assert_eq!(gw.submit(&second).0, mining::ACCEPTED, "a coinbaser one block old is still honoured");

    let st = settled_stats(&primed);
    let blocks: Vec<&serde_json::Value> = st["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| !b["kind"].as_str().unwrap().starts_with("orphan"))
        .collect();
    assert_eq!(blocks.len(), 2, "{st}");
    let paid_in_coinbases: u64 = blocks.iter().map(|b| b["carry_paid"].as_u64().unwrap()).sum();
    let taken_off_the_books: u64 = blocks
        .iter()
        .flat_map(|b| b["books"]["debited"].as_array().cloned().unwrap_or_default())
        .map(|d| d[1].as_u64().unwrap())
        .sum();
    eprintln!(
        "the pool owed {MINER} {OWED} sats of carry; two coinbases paid it {paid_in_coinbases}, and \
         {taken_off_the_books} came off the books"
    );
    assert_eq!(
        paid_in_coinbases, OWED,
        "the miner was owed {OWED} sats and two coinbases paid {paid_in_coinbases}: {taken_off_the_books} came off \
         the books, the rest out of the pool's remainder, and no record says it was overpaid"
    );
}

/// Finding 2. A found block's debits are booked in memory and its record is written (and
/// fsynced) to `blocks.jsonl` at once, but `window.json` only at the next housekeeping flush, up
/// to five seconds later. A Prime killed in between (OOM, power, `kill -9`, a panic) restarts
/// with the carry the block paid back on its books, next to a block record that says it came
/// off (`debits_live`), and nothing at startup reconciles the two: the next coinbaser pays it
/// again.
///
/// The data directory is copied the instant the share is answered, which is what a kill at that
/// instant leaves behind. (If a housekeeping tick lands in those few milliseconds the copy is
/// from after the flush and the test passes: about one run in a thousand.)
#[test]
#[ignore]
fn a_prime_killed_just_after_a_find_does_not_offer_the_paid_carry_again() {
    let (first, _) = first_block().clone();
    let pool = Identity::generate();
    let node = node_at_a();
    let cfg = format!("{}\nhouse-loopback = false", node.config(0.2));
    let (primed, port) = start_primed_seeded(&pool, &cfg, seed_carry);
    wait_for_tip(&primed);
    // let the flush for anything startup dirtied go by, so the only thing unflushed is the block
    std::thread::sleep(Duration::from_secs(6));

    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (id, outputs) = gw.request_coinbaser_outputs(VALUE, &TIP_A);
    assert_eq!(outputs, the_split());
    let mut first = first;
    first.job.as_mut().unwrap().coinbaser_id = id;
    assert_eq!(gw.submit(&first).0, mining::ACCEPTED);

    // what is on disk right now is what a kill right now leaves
    let crash = std::env::temp_dir().join(format!("primed-xbt110-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&crash);
    std::fs::create_dir_all(&crash).unwrap();
    for f in ["window.json", "blocks.jsonl", "identities.txt", "credits.bin"] {
        if primed.dir.join(f).exists() {
            std::fs::copy(primed.dir.join(f), crash.join(f)).unwrap();
        }
    }
    let record = std::fs::read_to_string(crash.join("blocks.jsonl")).expect("the block record is on disk already");
    let record: serde_json::Value = serde_json::from_str(record.lines().last().unwrap()).unwrap();
    assert_eq!(record["books"]["debits_live"], true, "{record}");
    assert_eq!(record["books"]["debited"], serde_json::json!([[MINER, OWED]]), "{record}");
    drop(gw);
    drop(primed);

    // the restart
    let (again, port) = start_primed_seeded(&pool, &cfg, |dir| {
        for f in ["window.json", "blocks.jsonl", "identities.txt", "credits.bin"] {
            if crash.join(f).exists() {
                std::fs::copy(crash.join(f), dir.join(f)).unwrap();
            }
        }
    });
    let _ = std::fs::remove_dir_all(&crash);
    wait_for_tip(&again);
    let st = settled_stats(&again);
    let on_the_books = st["window"]["carry_total_sats"].as_u64().unwrap();
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (_, outputs) = gw.request_coinbaser_outputs(VALUE, &TIP_A);
    let offered: u64 = outputs.iter().filter(|o| hex::encode(&o.script) == MINER_SCRIPT).map(|o| o.sats).sum();
    eprintln!(
        "after the restart the block record still says {OWED} sats of carry came off for the block, the books hold \
         {on_the_books}, and the next coinbaser offers the miner {offered}"
    );
    assert_eq!(
        (on_the_books, offered),
        (0, 0),
        "block {} paid this carry and its record says so, yet the restarted Prime holds it and offers it again",
        record["hash"]
    );
}
