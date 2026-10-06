//! XBT-119 review of the staged primed release (`prime/release-010-086-111-grace` @ c949b3c),
//! against a real `primed` and a stand-in node.
//!
//! * The rebate twin of XBT-110's finding 1 (Q2): owed DATUM rebate credited out by block N,
//!   then block N+1 mined on the coinbaser from before N. Grinds two diff-1 shares. Written to
//!   FAIL on c949b3c: it asserts what block N+1 should credit and shows what it credits.
//! * A data directory moved between `primed-grace-214bb201` (what the hub runs) and this build,
//!   both ways (Q5). Needs that binary: `XBT119_OLD_PRIMED=/path/to/primed`; skipped without.
//!
//! ```text
//! XBT119_OLD_PRIMED=~/Bitcoin.worktrees/stratum-grace/prime/target/release/primed \
//!   cargo test --release -p primed --test xbt119_review_e2e -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::coinbaser::Output;
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};
use datum_wire::pow::Hash;
use datum_wire::verify::{self, JobSlot, Policy};
use tides::{BlockLog, BlockRecord, Books, Ledger, SplitParams, SOURCE_DATUM, SOURCE_STRATUM};

const REGTEST_BITS: u32 = 0x207f_ffff;
const TIP_A: Hash = [0x42; 32];
/// A DATUM miner and a house-stratum miner, with their output scripts.
const DATUM: &str = "bc1qk3kxstl02hqnhynwtx0zws7merw6ynut52vtzs";
const DATUM_SCRIPT: &str = "0014b46c682fef55c13b926e599e2743dbc8dda24f8b";
const STRATUM: &str = "bc1qpxcy2pgedcfccfpw0p9xpzm3edkgajmjl5xe02";
const STRATUM_SCRIPT: &str = "001409b04505196e138c242e784a608b71cb6c8ecb72";
const DATUM_WORK: u64 = 600_000;
const STRATUM_WORK: u64 = 400_000;
/// DATUM rebate the pool owes from before (a solo block's share, say), waiting to be credited.
const OWED_REBATE: u64 = 1_000_000;
/// The hub's shape in small: DATUM at the harness's 0.5%, stratum 25%, half of that to DATUM.
const STRATUM_FEE_BPS: u32 = 2_500;
const REBATE_BPS: u32 = 1_250;

/// `PRIMED_BIN` is read by the harness when a Prime starts, and is process-wide.
static BIN: Mutex<()> = Mutex::new(());

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

fn rebate_config(node: &MockNode) -> String {
    format!(
        "{}\nhouse-loopback = false\nmin-payout = 546\nwindow-min-work = 100000000\n\
         stratum-fee-bps = {STRATUM_FEE_BPS}\ndatum-rebate-bps = {REBATE_BPS}",
        node.config(0.2)
    )
}

fn seed_window(dir: &std::path::Path) {
    let t = unix_now() - 600;
    let mut l = Ledger::open(dir).unwrap();
    l.credit(DATUM, DATUM_WORK, HEIGHT, t, SOURCE_DATUM).unwrap();
    l.credit(STRATUM, STRATUM_WORK, HEIGHT, t, SOURCE_STRATUM).unwrap();
    l.set_rebate_owed(OWED_REBATE);
    l.persist_window().unwrap();
}

/// This block's own DATUM rebate: `REBATE_BPS` of the stratum work's share of the reward.
fn fee_rebate() -> u64 {
    (u128::from(VALUE) * u128::from(STRATUM_WORK) * u128::from(REBATE_BPS)
        / u128::from(DATUM_WORK + STRATUM_WORK)
        / 10_000) as u64
}

/// A share at `height` on `prev` whose coinbase pays `outputs` exactly.
fn split_share(outputs: &[Output], slot: u8, height: u32, prev: Hash, now: u32) -> PowSubmit {
    let outs: Vec<TxOut> = outputs.iter().map(|o| TxOut { value: o.sats, script: o.script.clone() }).collect();
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

fn block_hash(issued: &[Output], s: &PowSubmit) -> Hash {
    let pool = hex::decode(POOL_SCRIPT).unwrap();
    let policy = Policy {
        pool_script: &pool,
        issued: Some(issued),
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

fn block_log(p: &Primed) -> Vec<BlockRecord> {
    BlockLog::open(&p.dir).read_all().unwrap()
}

fn wait_settled(p: &Primed, n: usize) -> Vec<BlockRecord> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let b = block_log(p);
        if b.len() >= n && b.iter().take(n).all(|r| r.settled && r.books.as_ref().is_some_and(|k| k.credits_live)) {
            return b;
        }
        assert!(Instant::now() < deadline, "{n} block(s) never settled: {b:?}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn carry_of(p: &Primed, who: &str) -> u64 {
    settled_stats(p)["window"]["miners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["identity"] == who)
        .map_or(0, |m| m["carry_sats"].as_u64().unwrap())
}

/// Q2. The pool owes the window `OWED_REBATE` of DATUM rebate. Coinbaser C is issued: a block on
/// it credits the DATUM miner that, plus the rebate share of the block's own stratum fee. Block N
/// is found on C and does so. Block N+1 is found on C too (a coinbaser one block old is
/// honoured). The owed rebate is gone and must not be credited again; block N+1's own stratum
/// fee was paid to the pool again, and its rebate share is owed to the DATUM miner again.
#[test]
fn a_block_on_a_coinbaser_from_before_the_last_find_credits_its_own_rebate_and_not_the_owed_one_again() {
    let pool = Identity::generate();
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP_A),
        parent: node_hex(&[0x41; 32]),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    });
    let cfg = rebate_config(&node);

    // What the coinbaser will be, asked of a Prime that is then thrown away: the list is a
    // function of the seeded window, and the shares must be ground on it before the session
    // that submits them opens.
    let issued = {
        let _g = BIN.lock().unwrap_or_else(|e| e.into_inner());
        let (probe, port) = start_primed_seeded(&pool, &cfg, seed_window);
        wait_for_tip(&probe);
        let mut gw = Gateway::connect(port, &pool, &Identity::generate());
        gw.request_coinbaser_outputs(VALUE, &TIP_A).1
    };
    let f = fee_rebate();
    // the split itself, from the same window through the crate, as a check on the figures below
    {
        let d = std::env::temp_dir().join(format!("primed-xbt119-model-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        seed_window(&d);
        let l = Ledger::open(&d).unwrap();
        let p = SplitParams {
            fee_bps: 50,
            stratum_fee_bps: STRATUM_FEE_BPS,
            datum_rebate_bps: REBATE_BPS,
            ..SplitParams::default()
        };
        let s = l.window.split(VALUE, &p, unix_now(), |i| match i {
            DATUM => hex::decode(DATUM_SCRIPT).ok(),
            STRATUM => hex::decode(STRATUM_SCRIPT).ok(),
            _ => None,
        });
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(s.rebate_credits, vec![(DATUM.to_string(), f + OWED_REBATE)]);
        assert_eq!((s.rebate_owed_credited, s.rebate_deferred), (OWED_REBATE, 0));
        let mut want: Vec<Output> =
            s.payees.iter().map(|p| Output { sats: p.sats, script: p.script.clone() }).collect();
        want.push(Output { sats: s.pool_sats, script: hex::decode(POOL_SCRIPT).unwrap() });
        assert_eq!(issued, want, "the Prime issues the split of the seeded window");
    }

    let mut first = split_share(&issued, 1, HEIGHT, TIP_A, unix_now());
    grind_diff1(&mut first);
    let h1 = block_hash(&issued, &first);
    let mut second = split_share(&issued, 2, HEIGHT + 1, h1, unix_now());
    grind_diff1(&mut second);
    let h2 = block_hash(&issued, &second);

    let _g = BIN.lock().unwrap_or_else(|e| e.into_inner());
    let (primed, port) = start_primed_seeded(&pool, &cfg, seed_window);
    wait_for_tip(&primed);
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    let (id, outputs) = gw.request_coinbaser_outputs(VALUE, &TIP_A);
    assert_eq!(outputs, issued, "the one coinbaser this session is ever sent");

    // block N
    first.job.as_mut().unwrap().coinbaser_id = id;
    assert_eq!(gw.submit(&first).0, mining::ACCEPTED);
    *node.chain.lock().unwrap() = Chain {
        height: HEIGHT,
        tip: node_hex(&h1),
        parent: node_hex(&TIP_A),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    };
    let b = wait_settled(&primed, 1);
    let k = b[0].books.as_ref().unwrap();
    assert_eq!((b[0].kind.as_str(), b[0].rebate_credited, k.rebate_debited), ("split", f + OWED_REBATE, OWED_REBATE));
    assert_eq!(carry_of(&primed, DATUM), f + OWED_REBATE, "block N credits its own rebate and the owed one");
    assert_eq!(settled_stats(&primed)["window"]["rebate_owed_sats"], 0);

    // block N+1, on the coinbaser from before block N
    second.job.as_mut().unwrap().coinbaser_id = id;
    assert_eq!(gw.submit(&second).0, mining::ACCEPTED, "a coinbaser one block old is still honoured");
    node.confirmations.lock().unwrap().insert(node_hex(&h1), 2);
    *node.chain.lock().unwrap() = Chain {
        height: HEIGHT + 1,
        tip: node_hex(&h2),
        parent: node_hex(&h1),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    };
    let b = wait_settled(&primed, 2);
    let credit: i64 = b[1].carry_delta.iter().filter(|d| d.0 == DATUM && d.1 > 0).map(|d| d.1).sum();
    let carry = carry_of(&primed, DATUM);
    eprintln!(
        "each block's stratum fee holds {f} sats of DATUM rebate and the pool owed {OWED_REBATE} more. Block N \
         credited {DATUM} {}; block N+1, on the coinbaser from before N, credited it {credit} (rebate_credited {}, \
         carry_delta {:?}); its carry is now {carry}",
        b[0].rebate_credited, b[1].rebate_credited, b[1].carry_delta
    );
    assert!(credit as u64 <= f, "the owed rebate was credited a second time: {credit}");
    assert_eq!(
        (credit as u64, b[1].rebate_credited, carry),
        (f, f, 2 * f + OWED_REBATE),
        "block N+1 paid the pool its stratum fee, {f} sats of which is DATUM rebate, and the DATUM miner was \
         credited {credit} of it"
    );
}

fn record(hash: &str, height: u32, delta: Vec<(String, i64)>, books: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "ts": unix_now() - 3_600, "height": height, "hash": hash, "finder": DATUM, "coinbase_value": VALUE,
        "kind": "split", "owed_sats": 0, "split": [], "pool_sats": 0, "carry_paid": 30_000,
        "carry_delta": delta, "rebate_credited": 0, "rebate_delta": 0, "settled": true, "submit": "accepted",
        "gateway": "", "books": books
    })
}

/// Q5. A data directory as `primed-grace-214bb201` leaves it goes to this build, which adopts
/// its block debits and takes nothing off again; what this build then writes (a debt, the
/// applied debits, a record with a shortfall) goes back to `214bb201`, which starts on it.
#[test]
#[ignore]
fn a_data_directory_moves_between_the_hub_s_build_and_this_one() {
    let Ok(old_bin) = std::env::var("XBT119_OLD_PRIMED") else {
        eprintln!("XBT119_OLD_PRIMED is not set: skipped");
        return;
    };
    let _g = BIN.lock().unwrap_or_else(|e| e.into_inner());
    let pool = Identity::generate();
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP_A),
        parent: node_hex(&[0x41; 32]),
        bits: REGTEST_BITS,
        next_bits: Some(REGTEST_BITS),
    });
    let cfg = format!(
        "{}\nhouse-loopback = false\nmin-payout = 546\nwindow-min-work = 100000000\n\
         stratum-fee-bps = 10000\ndatum-rebate-bps = 1250\nstratum-grace-hours = 24\n\
         stratum-grace-datum-hours = 96\nstratum-grace-fee-bps = 2500\nstratum-grace-rebate-bps = 1250\n\
         stratum-grace-rearm-hours = 168\nstratum-grace-epoch = {}",
        node.config(0.2),
        unix_now() - 90_000
    );
    let keep = std::env::temp_dir().join(format!("primed-xbt119-move-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&keep);
    std::fs::create_dir_all(&keep).unwrap();
    let files = ["window.json", "blocks.jsonl", "identities.txt", "credits.bin", "grace.json"];
    let copy = |from: &std::path::Path, to: &std::path::Path| {
        for f in files {
            if from.join(f).exists() {
                std::fs::copy(from.join(f), to.join(f)).unwrap();
            }
        }
    };
    let h1 = "11".repeat(32);

    // 1. The hub's build on a ledger with carry and one settled block whose debits are live.
    std::env::set_var("PRIMED_BIN", &old_bin);
    let (old, _) = start_primed_seeded(&pool, &cfg, |dir| {
        let t = unix_now() - 600;
        let mut l = Ledger::open(dir).unwrap();
        l.credit(DATUM, DATUM_WORK, HEIGHT, t, SOURCE_DATUM).unwrap();
        l.credit(STRATUM, STRATUM_WORK, HEIGHT, t, SOURCE_STRATUM).unwrap();
        l.persist_window().unwrap();
        drop(l);
        // `window.json` with exactly the keys 41784ed has
        let meta = serde_json::json!({
            "target_work": 100_000_000u64, "lifetime_shares": 2, "lifetime_work": 1_000_000,
            "carry": {DATUM: 70_000, STRATUM: 5_000}, "rebate_owed": 1_234
        });
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
        let books = serde_json::json!({
            "rebate_owed_credited": 0, "rebate_deferred": 0, "debits_live": true,
            "debited": [[DATUM, 30_000]], "rebate_debited": 0, "credits_live": true, "rebate_added": 0
        });
        let line = record(&h1, HEIGHT - 5, vec![(DATUM.to_string(), -30_000)], books);
        std::fs::write(dir.join("blocks.jsonl"), format!("{line}\n")).unwrap();
    });
    wait_for_tip(&old);
    std::thread::sleep(Duration::from_secs(7));
    let st = settled_stats(&old);
    assert_eq!(st["window"]["carry_total_sats"], 75_000, "{st}");
    copy(&old.dir, &keep);
    drop(old);
    let by_old: serde_json::Value = serde_json::from_slice(&std::fs::read(keep.join("window.json")).unwrap()).unwrap();
    assert!(by_old.get("applied_debits_tracked").is_none() && by_old.get("debt").is_none(), "{by_old}");

    // 2. This build on what it left.
    std::env::remove_var("PRIMED_BIN");
    let (new, _) = start_primed_seeded(&pool, &cfg, |dir| copy(&keep, dir));
    wait_for_tip(&new);
    let st = settled_stats(&new);
    assert_eq!(
        (&st["window"]["carry_total_sats"], &st["window"]["carry_debt_sats"], &st["window"]["rebate_owed_sats"]),
        (&75_000.into(), &0.into(), &1_234.into()),
        "the release took a historical block's debit off again: {st}"
    );
    let by_new: serde_json::Value =
        serde_json::from_slice(&std::fs::read(new.dir.join("window.json")).unwrap()).unwrap();
    assert_eq!(by_new["applied_debits"], serde_json::json!([h1]), "{by_new}");
    assert_eq!(by_new["applied_debits_tracked"], true);
    assert_eq!(st["window"]["miners"].as_array().unwrap().len(), 2, "{st}");
    copy(&new.dir, &keep);
    drop(new);

    // 3. What this build writes when a coinbase pays carry twice, put there through the crate:
    //    a debt, a second applied debit, and a record with a shortfall and a paid-down debt.
    let h2 = "22".repeat(32);
    {
        let mut l = Ledger::open(&keep).unwrap();
        let mut books = Books::new(0, 0);
        l.book_debits(&[(STRATUM.to_string(), -9_000)], &mut books);
        l.note_debit_applied(&h2);
        l.sync().unwrap();
        assert_eq!((l.window.carry_of(STRATUM), l.window.debt_of(STRATUM)), (0, 4_000));
        let rec = BlockRecord {
            ts: u64::from(unix_now()) - 60,
            height: HEIGHT - 2,
            hash: h2.clone(),
            finder: Some(DATUM.into()),
            coinbase_value: VALUE,
            kind: "split".into(),
            owed_sats: 0,
            split: vec![],
            pool_sats: 0,
            carry_paid: 5_000,
            carry_delta: vec![(STRATUM.to_string(), -9_000)],
            rebate_credited: 0,
            rebate_delta: 0,
            settled: true,
            submit: "accepted".into(),
            gateway: String::new(),
            books: Some(Books { credits_live: true, ..books }),
            carry_shortfall_sats: 4_000,
            carry_reserved_sats: 0,
        };
        BlockLog::open(&keep).append(&rec).unwrap();
    }

    // 4. The hub's build on that: a rollback.
    std::env::set_var("PRIMED_BIN", &old_bin);
    let (back, _) = start_primed_seeded(&pool, &cfg, |dir| copy(&keep, dir));
    std::env::remove_var("PRIMED_BIN");
    wait_for_tip(&back);
    std::thread::sleep(Duration::from_secs(7));
    let st = settled_stats(&back);
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(back.dir.join("window.json")).unwrap()).unwrap();
    let log = std::fs::read_to_string(back.dir.join("blocks.jsonl")).unwrap();
    eprintln!(
        "rolled back to 214bb201: carry {} sats for {} holders, {} blocks listed; window.json keys now {:?}; \
         last block line: {}",
        st["window"]["carry_total_sats"],
        st["window"]["carry_holders"],
        st["blocks"].as_array().map_or(0, Vec::len),
        after.as_object().unwrap().keys().collect::<Vec<_>>(),
        log.lines().last().unwrap_or("")
    );
    assert_eq!(st["window"]["carry_total_sats"], 70_000, "{st}");
    assert_eq!(st["blocks"].as_array().unwrap().len(), 2, "{st}");
    let _ = std::fs::remove_dir_all(&keep);
}
