//! End-to-end: a gateway that keeps, for each job, the script it was configured with when the job
//! was made (CONVOY from `695d035`, live as `971af86`) is left on the pool's script once one of
//! its shares shows it, and no other session changes.
//!
//! Every tip opens with the gateway on its own payout, so that the empty job it publishes
//! before the split arrives is its own. A gateway that reads the script again when the reply
//! lands is back on the pool's by the time it builds the full job. One that keeps the script
//! pays itself whatever a section of that job does not place from the list: nothing where the
//! whole list is placed, and the whole reward on section 0, which its miners are handed when the
//! split is late. That is refused as bad-coinbase-outputs where the gateway is a window payee,
//! and is its own uncredited solo work where it is not.
//!
//! The first test needs no work (a coinbase is judged before the hash) and runs by default. The
//! second grinds a real diff-1 share (~2^32 BLAKE2b hashes, about a minute across all cores),
//! so it is ignored:
//!
//!     cargo test --release -p primed --test pinned_script_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};

const TIP_BITS: u32 = 0x1903_a30c;
/// The gateway's own payout.
const GW: &str = "bc1qrp33g0q5c5txsp9arysrx4k6zdkfs4nce4xj0gdcccefvpysxf3qccfmv3";
const GW_SCRIPT: &str = "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262";
/// Another miner's, for a window that does not pay the gateway.
const OTHER: &str = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
/// The build on the Umbrel "DATUM (blake2b)" app, and one from before `695d035`.
const PINNING_UA: &str = "v0.4.1-beta/971af86b93de088e217a9e74d1ac70188565dbb7";
const EARLIER_UA: &str = "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440";
const TIP: [u8; 32] = [0x42; 32];
const NEXT_TIP: [u8; 32] = [0x43; 32];

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

/// A Prime whose window owes `payee` a balance and which remembers `GW` as the payout of each of
/// `gateways`.
fn start(node: &MockNode, pool: &Identity, payee: &str, gateways: &[&Identity]) -> (Primed, u16) {
    let cfg = format!("{}\nhouse-loopback = false", node.config(0.2));
    let now = unix_now();
    let (primed, port) = start_primed_seeded(pool, &cfg, |dir| {
        let meta = serde_json::json!({
            "target_work": 8, "lifetime_shares": 0, "lifetime_work": 0, "rebate_owed": 0,
            "carry": {payee: 499_226}, "last_seen": {payee: now - 8 * 86_400},
        });
        std::fs::write(dir.join("window.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("identities.txt"), format!("{payee}\n")).unwrap();
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

/// What a message from the Prime is, for the ones a session is steered with: a configure as
/// (script, tag), and a block-notify as ("notify", "").
fn steer(body: &[u8]) -> (String, String) {
    if body[0] == mining::SUB_BLOCK_NOTIFY {
        return ("notify".into(), String::new());
    }
    assert_eq!(body[0], mining::SUB_CONFIGURE, "{body:?}");
    let len = usize::from(body[2]);
    let script = hex::encode(&body[3..3 + len]);
    let at = 3 + len + if body[1] == 3 { 8 + mining::RESUME_TOKEN_LEN } else { 4 };
    let tag = String::from_utf8(body[at + 1..at + 1 + usize::from(body[at])].to_vec()).unwrap();
    (script, tag)
}

fn steers(msgs: &[Vec<u8>]) -> Vec<(String, String)> {
    msgs.iter().map(|m| steer(m)).collect()
}

fn notify() -> (String, String) {
    ("notify".into(), String::new())
}

fn solo() -> (String, String) {
    (GW_SCRIPT.to_string(), "Lazarus/solo".to_string())
}

fn to_pool() -> (String, String) {
    (POOL_SCRIPT.to_string(), "Lazarus".to_string())
}

fn row<'a>(stats: &'a serde_json::Value, gateway: &Identity) -> &'a serde_json::Value {
    let hex = hex::encode(&gateway.sign_pk()[..8]);
    let rows = stats["clients"].as_array().unwrap();
    rows.iter().find(|r| r["gateway"] == hex.as_str()).unwrap_or_else(|| panic!("no row for {hex}: {stats}"))
}

/// Submit, returning everything the Prime sent up to the receipt and the receipt's
/// `(status, reject_code)`.
fn submit_seeing(g: &mut Gateway, s: &PowSubmit) -> (Vec<Vec<u8>>, (u8, u16)) {
    g.send_mining(&s.encode());
    let mut before = Vec::new();
    loop {
        let m = g.next_mining();
        if m[0] == mining::SUB_SHARE_RECEIPT {
            return (before, (m[1], u16::from_le_bytes([m[2], m[3]])));
        }
        before.push(m);
    }
}

/// A share on the first full job of a tip, as a gateway that kept its own script for it hands a
/// small miner: section 0, the whole reward to that script, on a job citing coinbaser 1.
fn kept_script_share(now: u32) -> PowSubmit {
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

/// The live case: the gateway is a window payee, so the share is refused. That one refusal is
/// what the session learns from.
#[test]
fn a_gateway_whose_job_kept_its_own_script_is_left_on_the_pools_script_from_then_on() {
    let node = node();
    let (pinning_gw, earlier_gw) = (Identity::generate(), Identity::generate());
    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, GW, &[&pinning_gw, &earlier_gw]);

    // Both are known gateways, so both open on their own payout, and both are turned to the
    // pool's script with the reply: nothing in a hello says which one keeps the first.
    let (mut pinning, first) = Gateway::connect_convoy_as(port, &pool, &pinning_gw, PINNING_UA);
    assert_eq!(steer(&first), solo());
    let (before, id, outputs) = pinning.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(steers(&before), vec![notify(), to_pool()]);
    assert_eq!(id, 1);
    assert!(outputs.iter().any(|o| hex::encode(&o.script) == GW_SCRIPT), "the gateway is a listed payee");
    let (mut earlier, first) = Gateway::connect_convoy_as(port, &pool, &earlier_gw, EARLIER_UA);
    assert_eq!(steer(&first), solo());
    let (before, _, _) = earlier.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(steers(&before), vec![notify(), to_pool()]);

    // Its job was made on its own script and kept it. Refused, as a listed payee paid the whole
    // reward always is, and then asked for fresh jobs: it is on the pool's script already.
    let share = kept_script_share(unix_now());
    let refused = (mining::REJECTED, mining::REJECT_BAD_COINBASE_OUTPUTS);
    assert_eq!(submit_seeing(&mut pinning, &share), (vec![], refused));
    assert_eq!(steer(&pinning.next_mining()), notify());
    // Learned once: the next share on that job is refused and nothing more is sent for it.
    assert_eq!(submit_seeing(&mut pinning, &share), (vec![], refused));
    let (before, _, _) = pinning.request_coinbaser_seeing(VALUE + 1, &TIP);
    assert_eq!(steers(&before), vec![to_pool()], "one notify, not one per refused share");

    // A new tip: the other session goes to its own payout until the split, and this one does not.
    *node.chain.lock().unwrap() = Chain {
        height: HEIGHT,
        tip: node_hex(&NEXT_TIP),
        parent: node_hex(&TIP),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    };
    assert_eq!(steer(&earlier.next_mining()), solo());
    assert_eq!(steer(&earlier.next_mining()), notify());
    assert_eq!(steer(&pinning.next_mining()), notify(), "not configured with its own payout again");
    let (before, _, _) = pinning.request_coinbaser_seeing(VALUE, &NEXT_TIP);
    assert_eq!(steers(&before), vec![to_pool()]);

    let st = settled_stats(&primed);
    assert_eq!(row(&st, &pinning_gw)["pins_script"], true, "{st}");
    assert_eq!(row(&st, &pinning_gw)["rejected"], 2, "{st}");
    assert!(row(&st, &earlier_gw).get("pins_script").is_none(), "a row without it is the row it always was");

    // Its next session opens on the pool's script, so no job it makes on connecting can pay it
    // what a section leaves over. The other gateway's opens as it always has.
    drop(pinning);
    drop(earlier);
    let (_again, first) = Gateway::connect_convoy_as(port, &pool, &pinning_gw, PINNING_UA);
    assert_eq!(steer(&first), to_pool());
    let (_again, first) = Gateway::connect_convoy_as(port, &pool, &earlier_gw, EARLIER_UA);
    assert_eq!(steer(&first), solo());
}

/// The quiet case: the gateway is not in the list, so the same share is its own solo work.
/// Accepted and credited to nobody, which no reject counter shows; learned from all the same.
#[test]
#[ignore]
fn an_unlisted_gateways_kept_script_share_is_its_solo_work_and_is_learned_from_too() {
    let mut share = kept_script_share(unix_now());
    grind_diff1(&mut share);

    let node = node();
    let gw = Identity::generate();
    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, OTHER, &[&gw]);
    let (mut session, first) = Gateway::connect_convoy_as(port, &pool, &gw, PINNING_UA);
    assert_eq!(steer(&first), solo());
    let (_, id, outputs) = session.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(id, 1);
    assert!(outputs.iter().all(|o| hex::encode(&o.script) != GW_SCRIPT), "the gateway is not a listed payee");

    let (before, receipt) = submit_seeing(&mut session, &share);
    assert_eq!(receipt, (mining::ACCEPTED_TENTATIVELY, 0));
    assert_eq!(steers(&before), vec![notify()]);

    let st = settled_stats(&primed);
    let r = row(&st, &gw);
    assert_eq!(r["pins_script"], true, "{r}");
    assert_eq!(
        (r["solo_full_shares"].as_u64(), r["work"].as_u64(), r["rejected"].as_u64()),
        (Some(1), Some(0), Some(0)),
        "{r}"
    );
    drop(session);
    let (_again, first) = Gateway::connect_convoy_as(port, &pool, &gw, PINNING_UA);
    assert_eq!(steer(&first), to_pool());
}
