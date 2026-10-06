//! XBT-110 review: a class budget has no floor.
//!
//! Written to FAIL on `prime/xbt-010-086` @ f4f0db3. A session's budget is whatever its own
//! Partial shares show three times, and nothing holds that to the size of a class CONVOY
//! actually builds. Against a real `primed` and a stand-in node; it grinds one diff-1 share, so
//! it is ignored by default:
//!
//!     cargo test --release -p primed --test xbt110_class_budget_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};

const TIP_BITS: u32 = 0x1903_a30c;
const TIP: [u8; 32] = [0x42; 32];
const CONVOY_UA: &str = "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440";
const MINERS: u16 = 40;
/// The smallest CONVOY class that keeps any payee is class 1: 500 bytes of coinbase, about 9
/// P2WPKH payees (the README's figure). Class 0 keeps none and teaches nothing.
const SMALLEST_REAL_CLASS_PAYEES: usize = 9;

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

/// Carry waiting for `MINERS` addresses, largest first, as in `class_budget_e2e`.
fn miners() -> Vec<(String, u64)> {
    (0..MINERS)
        .map(|i| {
            let mut prog = [0x5a; 20];
            prog[..2].copy_from_slice(&i.to_le_bytes());
            (bech32::segwit::encode_v0(bech32::hrp::BC, &prog).unwrap(), 2_000_000 - u64::from(i) * 10_000)
        })
        .collect()
}

fn share(slot: u8, id: u8, outs: &[TxOut], now: u32) -> PowSubmit {
    let mut s = pool_only_share(slot, HEIGHT, TIP, TIP_BITS, 0, now);
    let (cb, tidx, split_at) = coinbase::build(HEIGHT, b"Lazarus", outs, 0);
    s.coinbase_id = 2;
    s.coinbase = Some(CoinbaseSection {
        coinbase_id: 2,
        coinb1: cb[..split_at].to_vec(),
        coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
    });
    let job = s.job.as_mut().unwrap();
    job.target_byte_index = tidx as u16;
    job.coinbaser_id = id;
    s
}

/// A gateway (hostile, or one with a bug in how it fills a section) whose coinbase keeps only the
/// first payee of the list and sends the rest to the pool. Three shares, one per coinbaser, and
/// its session's budget is one output: from then on every coinbaser it is sent names one payee
/// and defers the other 39 to carry, where a real class 1 would have kept 9 and class 2, 17.
/// A block it finds is then a "split" owing nothing whose pool output holds 39 payees' money.
#[test]
fn a_class_budget_is_never_smaller_than_the_smallest_class_a_gateway_builds() {
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    });
    let (gw, pool) = (Identity::generate(), Identity::generate());
    let cfg = format!(
        "{}\nhouse-loopback = false\nclass-budget = true\nclass-budget-fee-wallet-reserves = true\nmin-diff = 2",
        node.config(0.2)
    );
    let (primed, port) = start_primed_seeded(&pool, &cfg, |dir| {
        let carry: serde_json::Map<String, serde_json::Value> =
            miners().into_iter().map(|(a, sats)| (a, sats.into())).collect();
        let meta = serde_json::json!({
            "target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "rebate_owed": 0, "carry": carry,
        });
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
        let ids: String = miners().into_iter().map(|(a, _)| a + "\n").collect();
        std::fs::write(dir.join("identities.txt"), ids).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(&primed)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip");
        std::thread::sleep(Duration::from_millis(100));
    }

    let (mut g, _) = Gateway::connect_convoy_as(port, &pool, &gw, CONVOY_UA);
    let mut list = Vec::new();
    for _ in 0..3 {
        list = g.request_coinbaser_seeing(VALUE, &TIP).2;
    }
    assert_eq!(list.len(), usize::from(MINERS) + 1);

    // the first payee in full, everything else to the pool: Partial(1)
    let one = vec![
        TxOut { value: list[0].sats, script: list[0].script.clone() },
        TxOut { value: VALUE - list[0].sats, script: hex::decode(POOL_SCRIPT).unwrap() },
    ];
    let mut s = share(3, 1, &one, unix_now());
    grind_diff1(&mut s);
    for (slot, id) in [(3u8, 1u8), (4, 2), (5, 3)] {
        let mut on = s.clone();
        on.job_id = slot;
        on.job.as_mut().unwrap().coinbaser_id = id;
        assert_eq!(g.submit(&on), (mining::ACCEPTED_TENTATIVELY, 0), "Partial, on coinbaser {id}");
    }
    let st = settled_stats(&primed);
    let key = hex::encode(&gw.sign_pk()[..8]);
    let row = st["clients"].as_array().unwrap().iter().find(|c| c["gateway"] == key.as_str()).expect("its row");
    eprintln!("class_budget_bytes after three one-payee shares: {}", row["class_budget_bytes"]);

    let (_, _, short) = g.request_coinbaser_seeing(VALUE, &TIP);
    let payees = short.len() - 1;
    eprintln!(
        "the next coinbaser names {payees} of {MINERS} payees; {} are deferred to carry",
        usize::from(MINERS) - payees
    );
    assert!(
        payees >= SMALLEST_REAL_CLASS_PAYEES,
        "three shares taught this session a budget of {} bytes, and its coinbasers now name {payees} payee(s) of \
         {MINERS}: no CONVOY class is that small",
        row["class_budget_bytes"]
    );
}
