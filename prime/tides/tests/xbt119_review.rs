//! XBT-119 review of the staged primed release: what the ledger does with carry a coinbase pays
//! twice (the XBT-111 debt), on in-memory windows through the crate's public API.
//!
//! The tests that pass state what holds. The ignored ones are findings: each asserts what the
//! books should say and fails on `prime/release-010-086-111-grace` @ c949b3c.
//!
//!     cargo test --release -p tides --test xbt119_review -- --include-ignored --nocapture

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use tides::{BlockLog, BlockRecord, Books, Ledger, Split, SplitParams, SOURCE_DATUM, SOURCE_STRATUM};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tides-xbt119-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn script(id: &str) -> Option<Vec<u8>> {
    Some(id.as_bytes().to_vec())
}

/// xorshift64*: the crate has no dev-dependencies and this needs none.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    /// Candidate seen: debits on the ledger.
    Pending,
    /// In the node's main chain: credits on too.
    Confirmed,
    Orphan,
}

struct Block {
    split: Split,
    delta: Vec<(String, i64)>,
    books: Books,
    state: State,
    /// Whether the node ever had it in the main chain.
    was_confirmed: bool,
}

/// What `Session::on_block_candidate` does with a found block's split.
fn find(l: &mut Ledger, split: &Split) -> Block {
    let mut delta = split.carry_delta(|_| true);
    let mut books = Books::new(split.rebate_owed_credited, split.rebate_deferred);
    l.book_debits(&delta, &mut books);
    if split.rebate_owed_credited > books.rebate_debited {
        tides::cap_rebate_credits(&mut delta, &split.rebate_credits, books.rebate_debited);
    }
    Block { split: split.clone(), delta, books, state: State::Pending, was_confirmed: false }
}

/// `node::settle_confirmed`.
fn confirm(l: &mut Ledger, b: &mut Block) {
    l.book_debits(&b.delta, &mut b.books);
    l.book_credits(&b.delta, &mut b.books);
    b.state = State::Confirmed;
    b.was_confirmed = true;
}

/// `node::mark_orphan`. Returns what `unbook` could not take back.
fn orphan(l: &mut Ledger, b: &mut Block) -> u64 {
    b.state = State::Orphan;
    l.unbook(&mut b.books)
}

/// Per identity: what the confirmed blocks earned it, and what their coinbases paid it.
fn tally(blocks: &[Block]) -> (BTreeMap<String, u64>, BTreeMap<String, u64>) {
    let (mut earned, mut paid) = (BTreeMap::new(), BTreeMap::new());
    for b in blocks.iter().filter(|b| b.state == State::Confirmed) {
        for p in &b.split.payees {
            *earned.entry(p.identity.clone()).or_insert(0) += p.sats - p.carry;
            *paid.entry(p.identity.clone()).or_insert(0) += p.sats;
        }
        for u in b.split.unpaid.iter().filter(|u| u.defers()) {
            *earned.entry(u.identity.clone()).or_insert(0) += u.earned;
        }
    }
    (earned, paid)
}

/// One random history of finds, confirmations and orphans over coinbasers up to two finds old.
/// Returns the first identity whose books do not add up, with the figures.
fn run(seed: u64, orphan_confirmed: bool, orphan_any: bool) -> Result<(), String> {
    let d = dir(&format!("prop-{seed}-{orphan_confirmed}-{orphan_any}"));
    let mut l = Ledger::open(&d).unwrap();
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    // no rebate: this is the carry property (Q1). The rebate twin is its own test.
    let p = SplitParams { fee_bps: 50, min_payout: 60_000, ..SplitParams::default() };
    let miners: Vec<String> = (0..8).map(|i| format!("m{i}")).collect();
    let mut start: BTreeMap<String, u64> = BTreeMap::new();
    for (i, m) in miners.iter().enumerate() {
        // a spread from far over the floor to far under it, so both payees and carriers exist
        l.credit(m, 1 << (2 * i as u32), 1, 1, SOURCE_DATUM).unwrap();
        if rng.below(2) == 0 {
            let c = rng.below(100_000);
            l.set_carry(m, c);
            start.insert(m.clone(), c);
        }
    }
    let mut coinbasers: Vec<Split> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut trace = Vec::new();
    let mut short_total = 0u64;
    for step in 0..60 {
        match rng.below(10) {
            0..=1 => {
                let m = &miners[rng.below(8) as usize];
                l.credit(m, 1 + rng.below(4_000), 1, 2 + step, SOURCE_DATUM).unwrap();
            }
            2..=3 => {
                coinbasers.push(l.window.split(1_000_000, &p, 10, script));
                trace.push(format!("{step}: coinbaser #{}", coinbasers.len() - 1));
            }
            4..=6 if !coinbasers.is_empty() => {
                // one of the three newest: what `COINBASER_GRACE_BLOCKS` still honours
                let i = coinbasers.len() - 1 - rng.below(coinbasers.len().min(3) as u64) as usize;
                blocks.push(find(&mut l, &coinbasers[i]));
                trace.push(format!("{step}: block {} found on coinbaser #{i}", blocks.len() - 1));
            }
            7..=8 => {
                if let Some(i) = blocks.iter().position(|b| b.state == State::Pending) {
                    confirm(&mut l, &mut blocks[i]);
                    trace.push(format!("{step}: block {i} confirmed"));
                }
            }
            _ => {
                let pick = blocks.iter().position(|b| {
                    b.state == State::Pending && (orphan_any || blocks.iter().all(|o| o.state != State::Confirmed))
                        || (orphan_confirmed && b.state == State::Confirmed)
                });
                if let Some(i) = pick {
                    let was = blocks[i].state;
                    let short = orphan(&mut l, &mut blocks[i]);
                    short_total += short;
                    trace.push(format!("{step}: block {i} ({was:?}) orphaned, short {short}"));
                }
            }
        }
    }
    for (i, b) in blocks.iter_mut().enumerate().filter(|(_, b)| b.state == State::Pending) {
        confirm(&mut l, b);
        trace.push(format!("end: block {i} confirmed"));
    }
    let (earned, paid) = tally(&blocks);
    let _ = fs::remove_dir_all(&d);
    for m in &miners {
        let have = l.window.carry_of(m) as i128 - l.window.debt_of(m) as i128;
        let want = *start.get(m).unwrap_or(&0) as i128 + *earned.get(m).unwrap_or(&0) as i128
            - *paid.get(m).unwrap_or(&0) as i128;
        if have != want {
            return Err(format!(
                "seed {seed} (unbook reported {short_total} sats it could not take back): {m} started with {} carry, earned {} and was paid {} in confirmed blocks, so is owed {want}; \
                 the books say carry {} - debt {} = {have}\n  {}",
                start.get(m).unwrap_or(&0),
                earned.get(m).unwrap_or(&0),
                paid.get(m).unwrap_or(&0),
                l.window.carry_of(m),
                l.window.debt_of(m),
                trace.join("\n  ")
            ));
        }
    }
    Ok(())
}

/// Q1. Entitlement = coinbase outputs + remaining carry - debt, per payee and to the sat, through
/// finds on coinbasers up to two blocks old, later earnings, and orphans of candidates the node
/// never confirmed.
#[test]
fn every_payee_s_books_add_up_through_old_coinbasers_and_orphaned_candidates() {
    for seed in 1..=3_000 {
        if let Err(e) = run(seed, false, false) {
            panic!("{e}");
        }
    }
}

/// Q1, an orphan of either block. The same property when a candidate is orphaned while another
/// block is confirmed, in any order.
#[test]
fn every_payee_s_books_add_up_when_any_candidate_is_orphaned() {
    let failures: Vec<String> = (1..=3_000).filter_map(|s| run(s, false, true).err()).collect();
    assert!(failures.is_empty(), "{} of 3000 histories do not add up; the first:\n{}", failures.len(), failures[0]);
}

/// Q1, a block the node confirmed and then reorganised away: the books add up in every history
/// in which `unbook` took back all it had credited.
#[test]
fn every_payee_s_books_add_up_after_a_reorg_unless_a_credit_was_already_paid_out() {
    let failures: Vec<String> =
        (1..=3_000).filter_map(|s| run(s, true, true).err()).filter(|e| e.contains("reported 0 sats")).collect();
    assert!(failures.is_empty(), "{} histories do not add up; the first:\n{}", failures.len(), failures[0]);
}

/// Finding (Q1, low; the same in `primed-grace-214bb201`). A block is confirmed, the earnings it
/// deferred are paid out by a later coinbase, and the block is then reorganised away: `unbook`
/// cannot take the credit back, logs it, and the payee keeps it. With debts on the books that
/// amount could be one.
#[test]
#[ignore]
fn every_payee_s_books_add_up_when_a_confirmed_block_is_reorganised_away() {
    let failures: Vec<String> = (1..=3_000).filter_map(|s| run(s, true, true).err()).collect();
    assert!(failures.is_empty(), "{} of 3000 histories do not add up; the first:\n{}", failures.len(), failures[0]);
}

/// Finding (Q1). Carry is paid by block 1 and again by block 2 (a coinbaser from before block
/// 1): a debt. Block 1 is then orphaned. Its debit goes back on as carry while the debt stays:
/// the sum is right, but the balance is one the next coinbaser pays out, a third coinbase output
/// for one balance, and the debt it leaves is collected only from earnings a later block defers.
#[test]
#[ignore]
fn an_orphaned_first_payment_is_set_against_the_debt_not_handed_out_again() {
    let d = dir("orphan-first");
    let mut l = Ledger::open(&d).unwrap();
    l.set_carry("m", 100_000);
    let delta = vec![("m".to_string(), -100_000i64)];
    let (mut first, mut second) = (Books::new(0, 0), Books::new(0, 0));
    l.book_debits(&delta, &mut first);
    l.book_debits(&delta, &mut second);
    assert_eq!((l.window.carry_of("m"), l.window.debt_of("m")), (0, 100_000));
    // block 1 is not in the chain after all; block 2 is, and it paid the 100 000
    l.unbook(&mut first);
    let offered = l
        .window
        .split(1_000_000, &SplitParams::default(), 10, script)
        .payees
        .iter()
        .find(|p| p.identity == "m")
        .map_or(0, |p| p.carry);
    let _ = fs::remove_dir_all(&d);
    assert_eq!(
        (l.window.carry_of("m"), l.window.debt_of("m"), offered),
        (0, 0, 0),
        "m was owed 100000 and block 2 paid it; the books now hold carry and debt side by side and the next coinbaser \
         offers the carry again"
    );
}

/// Finding (Q3). A debt is collected only from earnings a block defers to carry. A payee whose
/// share clears the floor in every coinbase is paid in full every time and the debt never moves.
#[test]
#[ignore]
fn a_debt_is_collected_from_a_payee_whose_every_block_clears_the_floor() {
    let d = dir("debt-payee");
    let mut l = Ledger::open(&d).unwrap();
    l.credit("big", 1_000, 1, 1, SOURCE_STRATUM).unwrap();
    l.credit("other", 1_000, 1, 1, SOURCE_STRATUM).unwrap();
    l.set_carry("big", 250_000);
    let p = SplitParams { min_payout: 546, ..SplitParams::default() };
    let old = l.window.split(1_000_000, &p, 10, script);
    // paid once, and again by a block on the same coinbaser
    let mut a = find(&mut l, &old);
    confirm(&mut l, &mut a);
    let mut b = find(&mut l, &old);
    confirm(&mut l, &mut b);
    assert_eq!(l.window.debt_of("big"), 250_000);
    // ten more blocks, each on a fresh coinbaser, each paying `big` half the reward
    let mut paid = 0u64;
    for _ in 0..10 {
        let s = l.window.split(1_000_000, &p, 10, script);
        paid += s.payees.iter().find(|x| x.identity == "big").unwrap().sats;
        let mut blk = find(&mut l, &s);
        confirm(&mut l, &mut blk);
    }
    let left = l.window.debt_of("big");
    let _ = fs::remove_dir_all(&d);
    assert_eq!(left, 0, "ten blocks paid big {paid} sats in their coinbases and its 250000 debt is still {left}");
}

fn record(hash: &str, delta: Vec<(String, i64)>, books: Books) -> BlockRecord {
    BlockRecord {
        ts: 1,
        height: 1,
        hash: hash.into(),
        finder: None,
        coinbase_value: 1_000_000,
        kind: "split".into(),
        owed_sats: 0,
        split: vec![],
        pool_sats: 0,
        carry_paid: 0,
        carry_delta: delta,
        rebate_credited: 0,
        rebate_delta: 0,
        settled: true,
        submit: "accepted".into(),
        gateway: String::new(),
        books: Some(books),
        carry_shortfall_sats: 0,
        carry_reserved_sats: 0,
    }
}

/// Q5, forward. A `window.json` with neither `debt` nor `applied_debits` (what
/// `primed-grace-214bb201` writes) beside a block log whose records say their debits are live:
/// the first start adopts them, takes nothing off again, and the next start has nothing to do.
#[test]
fn a_ledger_from_before_applied_debits_is_adopted_not_debited_again() {
    let d = dir("adopt");
    fs::create_dir_all(&d).unwrap();
    // exactly the keys 41784ed's `Meta` has
    let meta = serde_json::json!({
        "target_work": 0, "lifetime_shares": 0, "lifetime_work": 0,
        "carry": {"a": 70_000, "b": 5_000}, "rebate_owed": 1_234
    });
    fs::write(d.join("window.json"), meta.to_string()).unwrap();
    let log = BlockLog::open(&d);
    // two historical blocks that each paid `a` 30 000 of carry and drew 500 of owed rebate
    for h in ["h1", "h2"] {
        let books = Books {
            debits_live: true,
            debited: vec![("a".into(), 30_000)],
            rebate_debited: 500,
            credits_live: true,
            ..Books::new(500, 0)
        };
        log.append(&record(h, vec![("a".into(), -30_000)], books)).unwrap();
    }
    let blocks = log.read_all().unwrap();
    let mut l = Ledger::open(&d).unwrap();
    assert!(!l.debits_tracked());
    l.reconcile_debits(&blocks);
    assert_eq!(l.window.carry_of("a"), 70_000, "reconcile before adoption is a no-op");
    l.adopt_legacy_debits(&blocks);
    l.sync().unwrap();
    assert_eq!((l.window.carry_of("a"), l.window.carry_of("b"), l.window.rebate_owed()), (70_000, 5_000, 1_234));
    drop(l);
    let mut l = Ledger::open(&d).unwrap();
    assert!(l.debits_tracked());
    l.reconcile_debits(&blocks);
    assert_eq!((l.window.carry_of("a"), l.window.total_debt(), l.window.rebate_owed()), (70_000, 0, 1_234));
    let _ = fs::remove_dir_all(&d);
}

/// Q4. What `applied_debits` costs: the size of `window.json` and the time of one `sync` with
/// the hub's block count, and with a year's more.
#[test]
#[ignore]
fn bench_applied_debits_sync() {
    for n in [0usize, 1_400, 15_000, 50_000] {
        let d = dir(&format!("bench-{n}"));
        let mut l = Ledger::open(&d).unwrap();
        for i in 0..300 {
            l.set_carry(&format!("bc1q{i:038}"), 10_000 + i);
        }
        for i in 0..n {
            l.note_debit_applied(&format!("{i:064x}"));
        }
        l.sync().unwrap();
        let t = std::time::Instant::now();
        let rounds = 20;
        for i in 0..rounds {
            l.note_debit_applied(&format!("{:064x}", u64::MAX - i));
            l.sync().unwrap();
        }
        let each = t.elapsed() / rounds as u32;
        let size = fs::metadata(d.join("window.json")).unwrap().len();
        eprintln!("applied_debits = {n:>6}: window.json {size:>9} bytes, one sync {each:?}");
        let _ = fs::remove_dir_all(&d);
    }
}

/// Finding (Q7, low). `blocks.jsonl` is appended to, so a kill or a power cut can leave half a
/// line at its end. Half a line of ASCII is a bad line: logged, skipped, and the Prime starts.
/// Half a line that ends inside a multi-byte character (a finder's username is whatever UTF-8
/// the miner sent) is not valid UTF-8, `read_all` fails on it, and the release refuses to start
/// until someone trims the file by hand.
#[test]
#[ignore]
fn a_block_log_torn_inside_a_character_is_a_bad_line_not_an_unreadable_file() {
    let d = dir("torn");
    fs::create_dir_all(&d).unwrap();
    let log = BlockLog::open(&d);
    log.append(&record("h1", vec![], Books::new(0, 0))).unwrap();
    let mut second = record("h2", vec![], Books::new(0, 0));
    second.finder = Some("minér".into());
    let line = serde_json::to_vec(&second).unwrap();
    let cut = line.iter().position(|b| *b == 0xc3).unwrap() + 1;
    let mut bytes = fs::read(d.join("blocks.jsonl")).unwrap();
    bytes.extend_from_slice(&line[..cut]);
    fs::write(d.join("blocks.jsonl"), bytes).unwrap();
    let got = log.read_all().map(|b| b.len()).map_err(|e| e.to_string());
    let _ = fs::remove_dir_all(&d);
    assert_eq!(got, Ok(1), "the whole record before the torn one is still the block log");
}
