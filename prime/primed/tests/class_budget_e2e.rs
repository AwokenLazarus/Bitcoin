//! End-to-end: `class-budget` on a real primed.
//!
//! The first test needs no work and runs by default: until a session has learned a budget, for
//! every session without the key, and with the key but not the fee wallet's, a gateway is sent
//! exactly what it always was. The second
//! grinds two real diff-1 shares (about 2^33 BLAKE2b hashes, a minute or two across all cores),
//! so it is ignored:
//!
//!     cargo test --release -p primed --test class_budget_e2e -- --ignored --nocapture

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::coinbaser::Output;
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection, PowSubmit};

const TIP_BITS: u32 = 0x1903_a30c;
const TIP: [u8; 32] = [0x42; 32];
/// A CONVOY build whose class 2 keeps about 17 outputs of any list, and two sessions the key
/// never applies to.
const CONVOY_UA: &str = "v0.4.1-beta/b9ea7dc3eb91352565ab487ec55ed6ee5964a440";
const RATUM_UA: &str = "ratum-gateway/0.1.28/f0569180c986";
/// Payees the seeded window lists, every one a P2WPKH output of 31 bytes.
const MINERS: u16 = 40;
/// `class-budget` on: the key, and the fee wallet's word that it holds back what capped blocks
/// defer.
const ON: &str = "class-budget = true\nclass-budget-fee-wallet-reserves = true";

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

/// The window: carry balances waiting for `MINERS` addresses, largest first.
fn miners() -> Vec<(String, u64)> {
    (0..MINERS)
        .map(|i| {
            let mut prog = [0x5a; 20];
            prog[..2].copy_from_slice(&i.to_le_bytes());
            (bech32::segwit::encode_v0(bech32::hrp::BC, &prog).unwrap(), 2_000_000 - u64::from(i) * 10_000)
        })
        .collect()
}

fn start(node: &MockNode, pool: &Identity, extra: &str) -> (Primed, u16) {
    let cfg = format!("{}\nhouse-loopback = false\n{extra}", node.config(0.2));
    let (primed, port) = start_primed_seeded(pool, &cfg, |dir| {
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
    (primed, port)
}

/// The script (hex) and tag a configure body names, for either version. A v3 configure also
/// carries a resume token drawn at random per session, which is left out.
fn configured(body: &[u8]) -> (String, String) {
    assert_eq!(body[0], mining::SUB_CONFIGURE, "{body:?}");
    let len = usize::from(body[2]);
    let script = hex::encode(&body[3..3 + len]);
    let at = 3 + len + if body[1] == 3 { 8 + mining::RESUME_TOKEN_LEN } else { 4 };
    let tag = String::from_utf8(body[at + 1..at + 1 + usize::from(body[at])].to_vec()).unwrap();
    (script, tag)
}

fn configures(msgs: &[Vec<u8>]) -> Vec<(String, String)> {
    msgs.iter().filter(|m| m[0] == mining::SUB_CONFIGURE).map(|m| configured(m)).collect()
}

fn row<'a>(stats: &'a serde_json::Value, gateway: &Identity) -> &'a serde_json::Value {
    let hex = hex::encode(&gateway.sign_pk()[..8]);
    let rows = stats["clients"].as_array().unwrap();
    rows.iter().find(|r| r["gateway"] == hex.as_str()).unwrap_or_else(|| panic!("no row for {hex}: {stats}"))
}

fn keys(v: &serde_json::Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

/// What one Prime sent one gateway: the configure it opened with, then for each of two
/// coinbaser requests the configures ahead of the reply, and the reply.
type Sent = ((String, String), Vec<(Vec<(String, String)>, u8, Vec<Output>)>);

#[test]
fn with_nothing_learned_a_gateway_is_sent_what_it_always_was() {
    let node = node();
    let (convoy_gw, ratum_gw, ocean_gw) = (Identity::generate(), Identity::generate(), Identity::generate());
    let mut runs: Vec<(Vec<Sent>, serde_json::Value)> = Vec::new();
    for key in ["", "class-budget = true", ON] {
        let pool = Identity::generate();
        let (primed, port) = start(&node, &pool, key);
        let said = std::fs::read_to_string(primed.dir.join("primed.err")).unwrap();
        let unreserved = said.contains("class-budget = true is ignored until class-budget-fee-wallet-reserves");
        assert_eq!(unreserved, key == "class-budget = true", "{said}");
        let mut sent = Vec::new();
        let mut open = Vec::new();
        for (gw, ua, convoy) in
            [(&convoy_gw, CONVOY_UA, true), (&ratum_gw, RATUM_UA, true), (&ocean_gw, CONVOY_UA, false)]
        {
            let (mut g, first) = if convoy {
                Gateway::connect_convoy_as(port, &pool, gw, ua)
            } else {
                Gateway::connect_as(port, &pool, gw, ua)
            };
            let replies = [VALUE, VALUE + 1]
                .into_iter()
                .map(|value| {
                    let (before, id, outputs) = g.request_coinbaser_seeing(value, &TIP);
                    assert_eq!(outputs.len(), usize::from(MINERS) + 1, "every miner, then the pool");
                    (configures(&before), id, outputs)
                })
                .collect();
            sent.push((configured(&first), replies));
            open.push(g);
        }
        runs.push((sent, settled_stats(&primed)));
    }
    let [(without, off), (unreserved, ignored), (with, on)] = <[_; 3]>::try_from(runs).unwrap();
    assert_eq!(with, without, "the same configures and the same coinbasers");
    assert_eq!(unreserved, without);

    // Without the key, or with it but not the fee wallet's, no row, and nothing in the document,
    // says anything of class budgets.
    for doc in [&off, &ignored] {
        for gw in [&convoy_gw, &ratum_gw, &ocean_gw] {
            assert!(!keys(row(doc, gw)).iter().any(|k| k.starts_with("class_budget")), "{}", row(doc, gw));
        }
        assert!(!keys(&doc["totals"]).iter().any(|k| k.starts_with("class_budget")), "{}", doc["totals"]);
        assert!(doc.get("carry_reserved").is_none());
    }

    // With it, the CONVOY session's row says it has learned nothing yet, and the two it does not
    // apply to (ratum, and an OCEAN-generation hello) are the rows they always were.
    let r = row(&on, &convoy_gw);
    assert_eq!((r.get("class_budget_bytes"), &r["class_budget_replies"]), (Some(&serde_json::Value::Null), &0.into()));
    let mut expect = keys(row(&off, &convoy_gw));
    expect.extend(["class_budget_bytes".to_string(), "class_budget_replies".to_string()]);
    assert_eq!(keys(r), expect);
    for gw in [&ratum_gw, &ocean_gw] {
        assert_eq!(keys(row(&on, gw)), keys(row(&off, gw)), "{}", row(&on, gw));
    }
    let t = &on["totals"];
    assert_eq!(
        (&t["class_budget_replies"], &t["class_budget_ceiling_replies"], &t["class_budget_held_off"]),
        (&0.into(), &0.into(), &false.into()),
        "{t}"
    );
    assert_eq!(on["carry_reserved"], 0);
}

/// What a size class with `room` bytes keeps of `list`, as CONVOY builds it: first fit in order,
/// stopping under 30 bytes, and the value it did not place to the configured script, the pool's.
fn class_cut(list: &[Output], room: usize) -> Vec<TxOut> {
    let (mut left, mut placed) = (room, 0u64);
    let mut outs = Vec::new();
    for o in list {
        if left < 30 {
            break;
        }
        if o.script.len() + 9 <= left {
            outs.push(TxOut { value: o.sats, script: o.script.clone() });
            left -= o.script.len() + 9;
            placed += o.sats;
        }
    }
    outs.push(TxOut { value: VALUE - placed, script: hex::decode(POOL_SCRIPT).unwrap() });
    outs
}

/// A share on job slot `slot`, citing coinbaser `id`, whose coinbase section 2 pays `outs`.
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

/// A b9ea7dc class 2 keeps 16 of the 40 payees: Partial. Seen on three coinbasers, that is the
/// session's budget, the next list names only the 16 that fit, and the class keeps all of it:
/// a share on it is a full split.
#[test]
#[ignore]
fn a_class_limited_session_learns_its_budget_and_its_next_list_is_mined_whole() {
    let node = node();
    let gw = Identity::generate();
    let pool = Identity::generate();
    let (primed, port) = start(&node, &pool, &format!("{ON}\nmin-diff = 1"));
    let room = 16 * 31 + 20;

    let (mut g, _) = Gateway::connect_convoy_as(port, &pool, &gw, CONVOY_UA);
    let mut lists = Vec::new();
    for want in 1..=3u8 {
        let (_, id, list) = g.request_coinbaser_seeing(VALUE, &TIP);
        assert_eq!((id, list.len()), (want, usize::from(MINERS) + 1));
        lists.push(list);
    }
    assert!(lists.iter().all(|l| *l == lists[0]), "one window, one list");
    let list = lists.remove(0);
    let partial = class_cut(&list, room);
    assert_eq!(partial.len(), 17, "16 payees, then the rest to the pool");

    // Only a share that earns work is a sighting, and one that earns is in the duplicate set:
    // three shares are ground, one per coinbaser, as three templates' shares would be.
    let now = unix_now();
    for (k, (slot, id)) in [(3u8, 1u8), (4, 2), (5, 3)].into_iter().enumerate() {
        let mut s1 = share(slot, id, &partial, now + k as u32);
        grind_diff1(&mut s1);
        assert_eq!(g.submit(&s1), (mining::ACCEPTED_TENTATIVELY, 0), "Partial, on coinbaser {id}");
    }
    let st = settled_stats(&primed);
    assert_eq!(row(&st, &gw)["class_budget_bytes"], 16 * 31, "{}", row(&st, &gw));

    let (before, id, short) = g.request_coinbaser_seeing(VALUE, &TIP);
    assert_eq!(configures(&before), vec![(POOL_SCRIPT.to_string(), "Lazarus".to_string())]);
    assert_eq!(id, 4);
    assert_eq!(short[..16], list[..16], "the head of the list, in its order");
    assert_eq!((short.len(), hex::encode(&short[16].script)), (17, POOL_SCRIPT.to_string()), "then the pool");
    assert_eq!(short.iter().map(|o| o.sats).sum::<u64>(), VALUE);
    let st = settled_stats(&primed);
    assert_eq!((&row(&st, &gw)["class_budget_replies"], &st["totals"]["class_budget_replies"]), (&1.into(), &1.into()));

    let whole = class_cut(&short, room);
    assert_eq!(
        whole[..16].iter().map(|o| o.value).collect::<Vec<_>>(),
        short[..16].iter().map(|o| o.sats).collect::<Vec<_>>()
    );
    let mut s2 = share(6, id, &whole, unix_now());
    grind_diff1(&mut s2);
    assert_eq!(g.submit(&s2), (mining::ACCEPTED, 0), "a full split");
    let st = settled_stats(&primed);
    assert_eq!((row(&st, &gw)["accepted"].as_u64(), row(&st, &gw)["rejected"].as_u64()), (Some(4), Some(0)));
}
