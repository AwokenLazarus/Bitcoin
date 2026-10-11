//! End-to-end: a share refused as bad-coinbase-outputs leaves one line in the journal saying
//! what its coinbase paid, set against what the session would have taken.
//!
//! The receipt a gateway gets carries the code alone, and a stock gateway refused on every share
//! hangs up after 30 s (a refusal is not an answer to its watchdog) and comes straight back, so
//! what is left to read afterwards is a row of short sessions and this line. A coinbase is
//! judged before the hash, so the test needs no work.

mod common;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::Identity;
use datum_wire::mining::{self, CoinbaseSection};

const TIP_BITS: u32 = 0x1903_a30c;
const TIP: [u8; 32] = [0x42; 32];
/// A script the pool never issued and knows no gateway by.
const STRANGER_SCRIPT: &str = "00201863143c14c5166804bd19203356da136c985678cd4d27a1b8c6329604903262";

#[test]
fn a_refused_coinbase_is_described_once_a_minute_in_the_journal() {
    let node = MockNode::start(Chain {
        height: HEIGHT - 1,
        tip: node_hex(&TIP),
        parent: node_hex(&[0x41; 32]),
        bits: TIP_BITS,
        next_bits: Some(TIP_BITS),
    });
    let pool = Identity::generate();
    let (primed, port) = start_primed(&pool, &format!("{}\nhouse-loopback = false", node.config(0.2)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while stats(&primed)["node"]["height"].as_u64().unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "primed never read the node's tip");
        std::thread::sleep(Duration::from_millis(100));
    }

    let ua = "v0.4.1-beta/971af86b93de088e217a9e74d1ac70188565dbb7";
    let (mut gateway, _) = Gateway::connect_convoy_as(port, &pool, &Identity::generate(), ua);
    let (id, issued) = gateway.request_coinbaser_outputs(VALUE, &TIP);

    // the whole reward to a script of nobody's, on a job naming the reply just sent
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32;
    let mut share = pool_only_share(3, HEIGHT, TIP, TIP_BITS, 0, now);
    let outs = vec![TxOut { value: VALUE, script: hex::decode(STRANGER_SCRIPT).unwrap() }];
    let (cb, tidx, split_at) = coinbase::build(HEIGHT, b"Lazarus", &outs, 0);
    share.coinbase = Some(CoinbaseSection {
        coinbase_id: 0,
        coinb1: cb[..split_at].to_vec(),
        coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
    });
    let job = share.job.as_mut().unwrap();
    job.target_byte_index = tidx as u16;
    job.coinbaser_id = id;
    let refused = (mining::REJECTED, mining::REJECT_BAD_COINBASE_OUTPUTS);
    assert_eq!(gateway.submit(&share), refused);
    assert_eq!(gateway.submit(&share), refused);

    settled_stats(&primed);
    let journal = std::fs::read_to_string(primed.dir.join("primed.err")).unwrap();
    let lines: Vec<&str> = journal.lines().filter(|l| l.contains("refused as bad-coinbase-outputs")).collect();
    assert_eq!(lines.len(), 1, "one line a minute, not one a share: {journal}");
    let want = format!(
        "ua={ua:?}: refused as bad-coinbase-outputs: job 3 section 0 names coinbaser {id}, issued here with {} outputs, \
         and its coinbase pays {VALUE} sats: 0 to the pool, nothing to a script of the gateway's own, as no payout is \
         known for it, 0 to 0 listed scripts, {VALUE} to 1 scripts neither the list nor the session names (the \
         first: {VALUE} sats to {STRANGER_SCRIPT}), 0 in OP_RETURNs",
        issued.len(),
    );
    assert!(lines[0].ends_with(&want), "{}", lines[0]);
}
