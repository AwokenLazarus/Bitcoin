//! End-to-end: the worker names a DATUM gateway forwards reach `stats.json`, per session and per
//! address, and change nothing about who is credited.
//!
//! The first test needs no proof of work and runs with the suite. The second grinds four real
//! diff-1 shares (~2^32 BLAKE2b hashes each, a few minutes across all cores), so it is ignored:
//!
//!     cargo test --release -p primed --test workers_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::crypto::Identity;
use datum_wire::mining;

const MINER: &str = "bc1qvchspt9gm5dq0geq3kxx53k3n87znwwvwc30t0";
const TIP_BITS: u32 = 0x1903_a30c;
const TIP: [u8; 32] = [0x42; 32];

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

fn row<'a>(stats: &'a serde_json::Value, gateway: &Identity) -> &'a serde_json::Value {
    let hex = hex::encode(&gateway.sign_pk()[..8]);
    let rows = stats["clients"].as_array().unwrap();
    rows.iter().find(|r| r["gateway"] == hex.as_str()).unwrap_or_else(|| panic!("no row for {hex}: {stats}"))
}

fn start(house_loopback: bool) -> (MockNode, Identity, Primed, u16) {
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    });
    let pool = Identity::generate();
    let (primed, port) = start_primed(&pool, &format!("{}\nhouse-loopback = {house_loopback}", node.config(0.2)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(&primed)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    (node, pool, primed, port)
}

/// A DATUM gateway's row lists its workers from the start, empty until a share names one, so a
/// reader can tell "no names yet" from a Prime that does not report them. The pool's own
/// gateway has no list: the pool site has its workers from the gateway itself.
#[test]
fn a_datum_gateway_s_row_has_a_worker_list_and_the_house_gateway_s_does_not() {
    let (_node, pool, primed, port) = start(false);
    let gw = Identity::generate();
    let mut session = Gateway::connect(port, &pool, &gw);
    session.request_coinbaser_seeing(VALUE, &TIP);
    let st = settled_stats(&primed);
    let r = row(&st, &gw);
    assert_eq!((&r["workers"], &r["workers_overflow"]), (&serde_json::json!([]), &serde_json::json!([])), "{r}");
    drop(primed);

    let (_node, pool, primed, port) = start(true);
    let gw = Identity::generate();
    let mut session = Gateway::connect(port, &pool, &gw);
    session.request_coinbaser_seeing(VALUE, &TIP);
    let st = settled_stats(&primed);
    let r = row(&st, &gw);
    assert_eq!(r["fee_path"], "stratum");
    assert!(r.get("workers").is_none() && r.get("workers_overflow").is_none(), "{r}");
}

/// The task's reproduction: shares as `addr.A301`, `addr.A302`, the bare address, and one under
/// a name no machine would send. One identity is credited all four; the names are rows.
#[test]
#[ignore]
fn shares_from_two_workers_a_hostile_name_and_no_name_are_one_identity_and_four_rows() {
    let hostile = format!("{}\u{1b}[2J<script>", "W".repeat(100));
    let users = [format!("{MINER}.A301"), format!("{MINER}.A302"), format!("{MINER}.{hostile}"), MINER.to_string()];
    let pool = Identity::generate();
    let (primed, port) = start_primed(&pool, "rpc = \"http://127.0.0.1:9\"\npoll = 5.0\nhouse-loopback = false");
    let gw = Identity::generate();
    let mut session = Gateway::connect(port, &pool, &gw);
    for (i, user) in users.iter().enumerate() {
        let mut share = pool_only_share(3 + i as u8, HEIGHT, [0x61 + i as u8; 32], 0x193c_2d40, 0, unix_now());
        share.username = user.clone();
        share.extranonce[0] = 0x50 + i as u8;
        grind_diff1(&mut share);
        let (status, code) = session.submit(&share);
        assert!(
            status == mining::ACCEPTED || status == mining::ACCEPTED_TENTATIVELY,
            "{user}: share accepted (status 0x{status:02x}, code {code})"
        );
    }
    let st = settled_stats(&primed);
    assert_eq!(st["totals"]["shares_accepted"], 4, "{st}");
    // crediting is by address alone, as before
    let miners = st["window"]["miners"].as_array().unwrap();
    assert_eq!(miners.len(), 1, "{st}");
    assert_eq!(
        (&miners[0]["identity"], &miners[0]["work"], &miners[0]["credits"]),
        (&MINER.into(), &4.into(), &4.into())
    );

    let r = row(&st, &gw);
    assert_eq!((&r["identity"], &r["work"], &r["accepted"]), (&MINER.into(), &4.into(), &4.into()), "{r}");
    let workers = r["workers"].as_array().unwrap();
    let names: Vec<&str> = workers.iter().map(|w| w["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["", "A301", "A302", &"W".repeat(32)], "{r}");
    for w in workers {
        assert_eq!((&w["identity"], &w["work"], &w["shares"]), (&MINER.into(), &1.into(), &1.into()), "{w}");
        assert!(w["hashrate_ghs"].as_f64().unwrap() > 0.0, "{w}");
        assert!(w["last_share_s"].as_u64().unwrap() < 3_600, "{w}");
    }
    assert_eq!(r["workers_overflow"], serde_json::json!([]));
    eprintln!("{}", serde_json::to_string_pretty(&r["workers"]).unwrap());
}
