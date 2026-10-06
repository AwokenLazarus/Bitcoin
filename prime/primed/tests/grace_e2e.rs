//! End-to-end: the stratum grace in a real `primed`.
//!
//! The donation endpoint charges house-stratum work 100%. An address that has only just
//! started on the house stratum is charged the grace fee instead, for 24 hours, or 96 if it
//! has had DATUM work here. Addresses already on the house stratum when the grace is switched
//! on are held to the clock the operator declared (`stratum-grace-epoch`).
//!
//! The first test needs no proof of work and runs with the suite. The second grinds three real
//! diff-1 shares, a few minutes on a desktop, so it is ignored by default:
//!
//!     cargo test --release -p primed --test grace_e2e -- --ignored --nocapture

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::*;
use datum_wire::crypto::Identity;
use datum_wire::mining;
use tides::{Ledger, SOURCE_DATUM, SOURCE_STRATUM, SOURCE_STRATUM_GRACE};

const DATUM: &str = "bc1qk3kxstl02hqnhynwtx0zws7merw6ynut52vtzs";
const FAILOVER: &str = "bc1qpxcy2pgedcfccfpw0p9xpzm3edkgajmjl5xe02";
const OLD: &str = "bc1q4kar3d2l33utmscncmhc923gg8xy459qp2e554";
const NEW: &str = "38vJdhcMNudZSNHPQdmfCAL1VnZRjK4ouk";
const SAMPLE: u128 = 312_500_000;

fn unix_now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

/// The donation endpoint's knobs with its grace; `epoch` is a day and a minute ago, so an
/// address already on the house stratum has no grace left.
fn donation_config(epoch: u32) -> String {
    format!(
        "rpc = \"http://127.0.0.1:9\"\npoll = 5.0\nmin-payout = 546\n\
         stratum-fee-bps = 10000\ndatum-rebate-bps = 5000\n\
         stratum-grace-hours = 24\nstratum-grace-datum-hours = 96\n\
         stratum-grace-fee-bps = 2500\nstratum-grace-rebate-bps = 1250\n\
         stratum-grace-rearm-hours = 168\nstratum-grace-epoch = {epoch}"
    )
}

fn miner<'a>(st: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    st["window"]["miners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["identity"] == id)
        .unwrap_or_else(|| panic!("{id} is not in the window: {}", st["window"]["miners"]))
}

#[test]
fn a_window_from_before_the_grace_is_given_its_clocks_and_split_by_class() {
    let pool = Identity::generate();
    let now = unix_now();
    let epoch = now - 86_400 - 60;
    let t = now - 600;
    let (primed, _port) = start_primed_seeded(&pool, &donation_config(epoch), |dir| {
        let mut l = Ledger::open(dir).unwrap();
        l.credit(DATUM, 400_000, HEIGHT, t, SOURCE_DATUM).unwrap();
        l.credit(FAILOVER, 200_000, HEIGHT, t, SOURCE_DATUM).unwrap();
        l.credit(FAILOVER, 100_000, HEIGHT, t, SOURCE_STRATUM).unwrap();
        l.credit(OLD, 100_000, HEIGHT, t, SOURCE_STRATUM).unwrap();
        // as an earlier run of this build would have left it
        l.credit(NEW, 100_000, HEIGHT, t, SOURCE_STRATUM_GRACE).unwrap();
        l.persist_window().unwrap();
        assert!(!dir.join("grace.json").exists());
    });
    let st = settled_stats(&primed);
    let pool_st = &st["pool"];
    assert_eq!(
        (&pool_st["fee_bps"], &pool_st["stratum_fee_bps"], &pool_st["datum_rebate_bps"]),
        (&50.into(), &10_000.into(), &5_000.into())
    );
    assert_eq!(pool_st["stratum_grace_hours"], 24);
    assert_eq!(pool_st["stratum_grace_datum_hours"], 96);
    assert_eq!(pool_st["stratum_grace_fee_bps"], 2_500);
    assert_eq!(pool_st["stratum_grace_rebate_bps"], 1_250);
    assert_eq!(pool_st["stratum_grace_rearm_hours"], 168);
    assert_eq!(pool_st["stratum_grace_epoch"], epoch);

    // the clocks: already on stratum means the epoch, and DATUM work makes it 96 hours
    assert_eq!(miner(&st, OLD)["stratum_grace_until"], epoch + 86_400);
    assert_eq!(miner(&st, FAILOVER)["stratum_grace_until"], epoch + 96 * 3_600);
    assert_eq!(miner(&st, NEW)["stratum_grace_until"], epoch + 86_400);
    assert!(miner(&st, DATUM)["stratum_grace_until"].is_null());
    for (id, work, stratum, grace) in [
        (DATUM, 400_000, 0, 0),
        (FAILOVER, 300_000, 100_000, 0),
        (OLD, 100_000, 100_000, 0),
        (NEW, 100_000, 100_000, 100_000),
    ] {
        let m = miner(&st, id);
        assert_eq!((&m["work"], &m["stratum_work"], &m["grace_work"]), (&work.into(), &stratum.into(), &grace.into()));
    }

    // what a block found now would pay: DATUM work less 0.5%, full-fee stratum work nothing,
    // grace work less 25%
    let total = 900_000u128;
    let pay = |datum: u128, grace: u128| (SAMPLE * (datum * 9_950 + grace * 7_500) / total / 10_000) as u64;
    assert_eq!(miner(&st, DATUM)["payout_sats"], pay(400_000, 0));
    assert_eq!(miner(&st, FAILOVER)["payout_sats"], pay(200_000, 0));
    assert_eq!(miner(&st, NEW)["payout_sats"], pay(0, 100_000));
    assert_eq!(miner(&st, OLD)["payout_sats"], 0);
    // 50 points of the full-fee stratum work and 12.5 of the grace work, to DATUM work only
    let pot = SAMPLE * 200_000 * 5_000 / total / 10_000 + SAMPLE * 100_000 * 1_250 / total / 10_000;
    assert_eq!(miner(&st, DATUM)["rebate_sats"], (pot * 400_000 / 600_000) as u64);
    assert_eq!(miner(&st, FAILOVER)["rebate_sats"], (pot * 200_000 / 600_000) as u64);
    assert_eq!(miner(&st, OLD)["rebate_sats"], 0);
    assert_eq!(miner(&st, NEW)["rebate_sats"], 0);
    let paid: u64 = [DATUM, FAILOVER, NEW].iter().map(|id| miner(&st, id)["payout_sats"].as_u64().unwrap()).sum();
    assert_eq!(st["window"]["sample_pool_sats"].as_u64().unwrap() + paid, SAMPLE as u64);

    // the book reaches disk with the next flush
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !primed.dir.join("grace.json").exists() {
        assert!(std::time::Instant::now() < deadline, "grace.json was never written");
        std::thread::sleep(Duration::from_millis(250));
    }
    let book: serde_json::Value =
        serde_json::from_slice(&std::fs::read(primed.dir.join("grace.json")).unwrap()).unwrap();
    assert_eq!(book["stratum"][OLD]["start"], epoch);
    assert_eq!(book["stratum"].as_object().unwrap().len(), 3);
    assert!(book["datum_seen"][FAILOVER].is_number() && book["datum_seen"][DATUM].is_number());
}

#[test]
#[ignore]
fn a_share_on_the_house_stratum_is_tagged_by_its_address_s_clock() {
    let pool = Identity::generate();
    let now = unix_now();
    let epoch = now - 86_400 - 60;
    let t = now - 600;
    // loopback is the house gateway (house-loopback defaults on), as on the hub
    let (primed, port) = start_primed_seeded(&pool, &donation_config(epoch), |dir| {
        let mut l = Ledger::open(dir).unwrap();
        l.credit(DATUM, 400_000, HEIGHT, t, SOURCE_DATUM).unwrap();
        l.credit(FAILOVER, 200_000, HEIGHT, t, SOURCE_DATUM).unwrap();
        l.credit(OLD, 100_000, HEIGHT, t, SOURCE_STRATUM).unwrap();
        l.persist_window().unwrap();
    });
    let mut gw = Gateway::connect(port, &pool, &Identity::generate());
    for (i, id) in [NEW, OLD, FAILOVER].into_iter().enumerate() {
        let mut share = pool_only_share(3 + i as u8, HEIGHT, [0x71 + i as u8; 32], 0x193c_2d40, 0, unix_now());
        share.username = format!("{id}.rig");
        share.extranonce[0] = 0x40 + i as u8;
        grind_diff1(&mut share);
        let (status, code) = gw.submit(&share);
        assert!(
            status == mining::ACCEPTED || status == mining::ACCEPTED_TENTATIVELY,
            "{id}: share accepted (status 0x{status:02x}, code {code})"
        );
    }
    let done = unix_now();
    let st = settled_stats(&primed);
    assert_eq!(st["totals"]["shares_accepted"], 3, "{st}");
    let near = |v: &serde_json::Value, want: u32| v.as_u64().is_some_and(|u| u.abs_diff(u64::from(want)) <= 900);

    // never on stratum before: all of it grace, for 24 hours from the share
    let m = miner(&st, NEW);
    assert_eq!((&m["work"], &m["stratum_work"], &m["grace_work"]), (&1.into(), &1.into(), &1.into()), "{m}");
    assert!(near(&m["stratum_grace_until"], done + 86_400), "{m}");
    // on stratum since before the epoch, a day ago: its grace is spent, the new share pays in full
    let m = miner(&st, OLD);
    assert_eq!((&m["stratum_work"], &m["grace_work"]), (&100_001.into(), &0.into()), "{m}");
    assert_eq!(m["stratum_grace_until"], epoch + 86_400);
    // a DATUM miner whose machines fell back to the house stratum: grace, for 96 hours
    let m = miner(&st, FAILOVER);
    assert_eq!((&m["work"], &m["stratum_work"], &m["grace_work"]), (&200_001.into(), &1.into(), &1.into()), "{m}");
    assert!(near(&m["stratum_grace_until"], done + 96 * 3_600), "{m}");
    eprintln!("NEW {}\nOLD {}\nFAILOVER {}", miner(&st, NEW), miner(&st, OLD), miner(&st, FAILOVER));
}
