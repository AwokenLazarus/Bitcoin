//! End-to-end: a gateway whose build is in `held-split-builds` owes as any gateway does until its
//! own shares show its payout (a payout remembered from an earlier session is not taken for it),
//! and nothing else changes: not its coinbaser, not any other session, and with the key unset
//! not that session either.
//!
//! The gateway here is also a window payee, which is the case the design review found would
//! have every one of its shares refused. The first test needs no work and runs by default. The
//! second grinds a real diff-1 share (~2^32 BLAKE2b hashes, about a minute across all cores),
//! so it is ignored:
//!
//!     cargo test --release -p primed --test held_split_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};

const TIP_BITS: u32 = 0x1903_a30c;
/// The gateway's own payout, and a window miner with a balance the next coinbaser pays: the
/// gateway is a listed payee.
const GW: &str = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
const GW_SCRIPT: &str = "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262";
/// An old OCEAN-hash CONVOY build, as the live one says it, and a current one.
const HELD_UA: &str = "v0.4.1-beta/e894b8ac29ae06bf6e3b14dafd21f72dcd65fb84";
const STOCK_UA: &str = "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440";
const TIP: [u8; 32] = [0x42; 32];

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

fn node() -> MockNode {
    MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    })
}

/// A Prime whose window owes `GW` a balance and which remembers `GW` as the payout of each of
/// `gateways`. Loopback is not the house gateway here: the pool's own is never held.
fn start(node: &MockNode, pool: &Identity, held: bool, gateways: &[&Identity]) -> (Primed, u16) {
    let key = if held { "held-split-builds = [\"e894b8a\", \"f74c22a\"]" } else { "" };
    let cfg = format!("{}\nhouse-loopback = false\n{key}", node.config(0.2));
    let now = unix_now();
    let (primed, port) = start_primed_seeded(pool, &cfg, |dir| {
        let meta = serde_json::json!({
            "target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "rebate_owed": 0,
            "carry": {GW: 499_226}, "last_seen": {GW: now - 8 * 86_400},
        });
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("identities.txt"), format!("{GW}\n")).unwrap();
        let scripts: serde_json::Map<String, serde_json::Value> = gateways
            .iter()
            .map(|g| (hex::encode(g.sign_pk()), serde_json::json!({"identity": GW, "script_hex": GW_SCRIPT})))
            .collect();
        std::fs::write(dir.join("gateway-scripts.json"), serde_json::Value::Object(scripts).to_string()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(&primed)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip");
        std::thread::sleep(Duration::from_millis(100));
    }
    (primed, port)
}

/// The script (hex) and tag a configure body names, for either version.
fn configured(body: &[u8]) -> (String, String) {
    assert_eq!(body[0], mining::SUB_CONFIGURE, "{body:?}");
    let len = usize::from(body[2]);
    let script = hex::encode(&body[3..3 + len]);
    let at = 3 + len + if body[1] == 3 { 8 + mining::RESUME_TOKEN_LEN } else { 4 };
    let tag = String::from_utf8(body[at + 1..at + 1 + usize::from(body[at])].to_vec()).unwrap();
    (script, tag)
}

/// The configures among messages the Prime sent, each as (script, tag). The block-notify it
/// sends straight after the first configure is not one.
fn configures(msgs: &[Vec<u8>]) -> Vec<(String, String)> {
    msgs.iter().filter(|m| m[0] == mining::SUB_CONFIGURE).map(|m| configured(m)).collect()
}

fn row<'a>(stats: &'a serde_json::Value, gateway: &Identity) -> &'a serde_json::Value {
    let hex = hex::encode(&gateway.sign_pk()[..8]);
    let rows = stats["clients"].as_array().unwrap();
    rows.iter().find(|r| r["gateway"] == hex.as_str()).unwrap_or_else(|| panic!("no row for {hex}: {stats}"))
}

#[test]
fn a_held_split_build_owes_until_its_own_shares_show_its_payout_and_nothing_else_changes() {
    let pool_script = POOL_SCRIPT.to_string();
    let solo = || (GW_SCRIPT.to_string(), "Lazarus/solo".to_string());
    let to_pool = || (pool_script.clone(), "Lazarus".to_string());
    let node = node();
    let (held_gw, stock_gw, new_gw) = (Identity::generate(), Identity::generate(), Identity::generate());

    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, true, &[&held_gw, &stock_gw]);
    // Held, its payout remembered from an earlier session: opened on that script as any known
    // gateway is, and turned back to the pool with every reply until this session's own shares
    // show it. What was remembered is whoever dominated then, which need not be who mines now.
    let (mut held, first) = Gateway::connect_as(port, &pool, &held_gw, HELD_UA);
    assert_eq!(configured(&first), solo());
    let (before, _, held_outputs) = held.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(configures(&before), vec![to_pool()], "not held on a remembered payout");
    assert!(held_outputs.iter().any(|o| hex::encode(&o.script) == GW_SCRIPT), "the gateway is a listed payee");
    let (before, _, _) = held.request_coinbaser_seeing(VALUE + 1, &TIP);
    assert_eq!(configures(&before), vec![to_pool()], "nor with a later reply");

    // Another build on the same Prime, the same payout: configure(pool) with every reply, as ever.
    let (mut stock, first) = Gateway::connect_as(port, &pool, &stock_gw, STOCK_UA);
    assert_eq!(configured(&first), solo());
    let (before, _, stock_outputs) = stock.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(configures(&before), vec![to_pool()]);
    assert_eq!(stock_outputs, held_outputs, "a held session is issued the same split");

    // Held, but its payout not yet known: nothing changes until its shares name one.
    let (mut fresh, first) = Gateway::connect_as(port, &pool, &new_gw, HELD_UA);
    assert_eq!(configured(&first), to_pool());
    let (before, _, _) = fresh.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(configures(&before), vec![to_pool()]);

    let st = settled_stats(&primed);
    assert_eq!(row(&st, &held_gw)["held_split"], true, "{st}");
    assert_eq!(row(&st, &new_gw)["held_split"], true, "{st}");
    assert!(row(&st, &stock_gw).get("held_split").is_none(), "a row without it is the row it always was");
    drop(primed);

    // The key unset: the same build, the same payout, the same session as before the key.
    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, false, &[&held_gw]);
    let (mut held, first) = Gateway::connect_as(port, &pool, &held_gw, HELD_UA);
    assert_eq!(configured(&first), solo());
    let (before, _, outputs) = held.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(configures(&before), vec![to_pool()]);
    assert_eq!(outputs, held_outputs);
    assert!(row(&settled_stats(&primed), &held_gw).get("held_split").is_none());
}

/// Section 0 of an old OCEAN-hash build: the whole reward to the one configured script, the
/// gateway's own, on a job citing coinbaser 1.
fn section_zero_share(now: u32) -> PowSubmit {
    let mut s = pool_only_share(3, HEIGHT, TIP, TIP_BITS, 0, now);
    let outs = vec![TxOut { value: VALUE, script: hex::decode(GW_SCRIPT).unwrap() }];
    let (cb, tidx, split_at) = coinbase::build(HEIGHT, b"Lazarus/solo", &outs, 0);
    s.coinbase = Some(CoinbaseSection {
        coinbase_id: 0,
        coinb1: cb[..split_at].to_vec(),
        coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
    });
    let job = s.job.as_mut().unwrap();
    job.target_byte_index = tidx as u16;
    job.coinbaser_id = 1;
    s.username = format!("{GW}.rig");
    s
}

/// The review's finding, and its fix. A gateway that is a window payee and mines section 0 on
/// its own script cites a coinbaser listing that script. Held to the list it is a listed payee
/// paid the whole reward, and refused. Held-split, it is the gateway's own solo work: accepted,
/// and nothing credited to the window.
#[test]
#[ignore]
fn a_listed_held_split_gateways_section_zero_share_is_its_solo_work_not_refused() {
    let mut share = section_zero_share(unix_now());
    grind_diff1(&mut share);

    let node = node();
    let (held_gw, stock_gw) = (Identity::generate(), Identity::generate());
    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, true, &[&held_gw, &stock_gw]);

    // unflagged, the same coinbase against the same list: refused
    let (mut stock, _) = Gateway::connect_as(port, &pool, &stock_gw, STOCK_UA);
    let (_, id, outputs) = stock.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(id, 1);
    assert!(outputs.iter().any(|o| hex::encode(&o.script) == GW_SCRIPT));
    assert_eq!(stock.submit(&share), (mining::REJECTED, mining::REJECT_BAD_COINBASE_OUTPUTS));

    // held-split: its own solo work, and the window gains nothing
    let (mut held, _) = Gateway::connect_as(port, &pool, &held_gw, HELD_UA);
    let (_, id, _) = held.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(id, 1);
    assert_eq!(held.submit(&share), (mining::ACCEPTED_TENTATIVELY, 0));

    let st = settled_stats(&primed);
    let r = row(&st, &held_gw);
    assert_eq!(
        (r["solo_full_shares"].as_u64(), r["work"].as_u64(), r["rejected"].as_u64()),
        (Some(1), Some(0), Some(0)),
        "{r}"
    );
    assert_eq!(row(&st, &stock_gw)["rejected"], 1);
    assert_eq!(st["totals"]["work_accepted"], 0, "{st}");
    assert_eq!(st["totals"]["pool_only_full_jobs"], 0, "{st}");
}
