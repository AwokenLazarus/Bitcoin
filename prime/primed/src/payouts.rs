//! Stale balances paid by hand.
//!
//! A miner who stops with less than `min-payout` on the books is owed money no coinbase will
//! ever carry (or, with `stale-coinbase`, not for a while). The operator pays such balances
//! with an ordinary transaction from the pool's wallet. Between deciding to and that payment
//! confirming, the balance must not also go out in a coinbase, and afterwards it must leave
//! the books for good: that is a [`tides::Hold`], and this module is how the operator asks for
//! one while Prime is running.
//!
//! Requests are files, not a socket: `<data-dir>/payouts/<id>.request.json`, written by
//! whoever may write to the data directory, which is exactly who may already edit the ledger.
//! Prime answers in `<id>.json` and removes the request. Every change is also appended to
//! `payouts.jsonl`.
//!
//! ```text
//! {"action": "hold", "entries": [["bc1q…", 499226], …]}   set these balances aside
//! {"action": "paid", "txid": "…", "blockhash": "…"}       the payment confirmed: off the books
//! {"action": "release"}                                   abandoned: they are carry again
//! ```
//!
//! `hold` takes an entry only if the identity is stale right now and holds exactly the sats
//! named. `paid` is not taken on trust: Prime reads the transaction from its own node and
//! finishes the hold only if it is confirmed and pays every held identity's script at least
//! what is held. The answer's `ready_height` is the tip height from which the held amounts are
//! final (see [`tides::Hold`]); a payment must not be built before it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::address;
use crate::state::{now, Shared};

/// Confirmations before a payment counts as made.
const PAID_CONFIRMATIONS: i64 = 3;
/// A coinbaser can be mined on for this many blocks past the one it was issued for
/// (`session::COINBASER_GRACE_BLOCKS`), and it was issued for the block after the tip.
const READY_AFTER_BLOCKS: u32 = crate::session::COINBASER_GRACE_BLOCKS + 2;
const RETRY_EVERY: Duration = Duration::from_secs(30);
/// Requests left in place to be looked at again, with when, and the file as it then was: a
/// request written anew is a new request, and is not kept waiting on the old one's clock.
static WAITING: Mutex<Option<HashMap<String, (Instant, Option<std::time::SystemTime>)>>> = Mutex::new(None);
const MAX_REQUEST_BYTES: u64 = 4 << 20;
const MAX_ENTRIES: usize = 5_000;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum Request {
    Hold {
        entries: Vec<(String, u64)>,
    },
    Paid {
        txid: String,
        #[serde(default)]
        blockhash: Option<String>,
    },
    Release,
}

pub fn dir(shared: &Shared) -> PathBuf {
    shared.cfg.data_dir.join("payouts")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Handle every request waiting in the payouts directory. Called from the node poller.
pub async fn poll(shared: &Shared) {
    let dir = dir(shared);
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    let mut requests: Vec<(String, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let id = name.strip_suffix(".request.json")?.to_owned();
            Some((id, e.path()))
        })
        .collect();
    requests.sort();
    for (id, path) in requests {
        // a payment waiting for confirmations is looked at twice a minute, not every poll
        let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let not_yet = WAITING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .get(&id)
            .is_some_and(|(at, file)| *at > Instant::now() && *file == written);
        if not_yet {
            continue;
        }
        let answer = match read_request(&id, &path) {
            Ok(req) => handle(shared, &id, req).await,
            Err(e) => json!({"id": id, "ok": false, "error": e}),
        };
        // A payment the node does not show yet is not an error to give up on: leave the
        // request where it is and look again next time round.
        let mut guard = WAITING.lock().unwrap_or_else(|e| e.into_inner());
        let waiting = guard.get_or_insert_with(HashMap::new);
        if answer.get("retry").and_then(Value::as_bool) == Some(true) {
            waiting.insert(id.clone(), (Instant::now() + RETRY_EVERY, written));
            drop(guard);
            write_json(&dir.join(format!("{id}.json")), &answer);
            continue;
        }
        waiting.remove(&id);
        drop(guard);
        log::info!("payout {id}: {}", answer);
        write_json(&dir.join(format!("{id}.json")), &answer);
        append_log(&dir.join("payouts.jsonl"), &answer);
        if let Err(e) = std::fs::remove_file(&path) {
            // it would be handled again every second, and a hold must not be asked for twice
            log::error!("payout {id}: cannot remove {}: {e}; renaming", path.display());
            let _ = std::fs::rename(&path, path.with_extension("stuck"));
        }
    }
}

fn read_request(id: &str, path: &Path) -> Result<Request, String> {
    if !valid_id(id) {
        return Err("the id may hold letters, digits, '-', '_' and '.', 64 at most".into());
    }
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_REQUEST_BYTES {
        return Err("request too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let req: Request = serde_json::from_slice(&bytes).map_err(|e| format!("not a request: {e}"))?;
    if let Request::Hold { entries } = &req {
        if entries.is_empty() || entries.len() > MAX_ENTRIES {
            return Err(format!("a hold names 1..={MAX_ENTRIES} entries"));
        }
    }
    Ok(req)
}

async fn handle(shared: &Shared, id: &str, req: Request) -> Value {
    let ts = now();
    match req {
        Request::Hold { entries } => {
            let after = shared.cfg.stale_after_secs();
            if after == 0 {
                return json!({"id": id, "ok": false, "ts": ts, "error": "stale-after-days is 0: no balance is stale"});
            }
            let Some(tip) = shared.tip_snapshot() else {
                return json!({"id": id, "ok": false, "ts": ts, "error": "no tip from the node yet"});
            };
            // only what can be paid at all: an identity that is not an address has no output
            let (payable, unpayable): (Vec<_>, Vec<_>) =
                entries.into_iter().partition(|e| address::to_script(&e.0, shared.network).is_some());
            let ready = tip.height + READY_AFTER_BLOCKS;
            let mut ledger = shared.ledger.lock().unwrap();
            let res =
                ledger.hold_carry(id, &payable, ts as u32, after, shared.cfg.stale_min_payout, tip.height, ready);
            let answer = match res {
                Ok((held, skipped)) => {
                    let mut skipped: Vec<Value> =
                        skipped.into_iter().map(|s| serde_json::to_value(s).unwrap_or(Value::Null)).collect();
                    skipped.extend(unpayable.into_iter().map(
                        |(identity, requested)| json!({"identity": identity, "requested": requested, "reason": "not an address"}),
                    ));
                    json!({
                        "id": id, "ok": true, "ts": ts, "status": if held.is_empty() { "nothing-held" } else { "held" },
                        "height": tip.height, "ready_height": ready,
                        "held_sats": held.iter().map(|h| h.1).sum::<u64>(),
                        "entries": held, "skipped": skipped,
                    })
                }
                Err(e) => json!({"id": id, "ok": false, "ts": ts, "error": e}),
            };
            // money moved: on disk before anyone is told it did
            if let Err(e) = ledger.persist_window() {
                log::error!("payout {id}: ledger persist failed: {e}");
            }
            shared.drop_coinbaser_base();
            answer
        }
        Request::Release => {
            let mut ledger = shared.ledger.lock().unwrap();
            match ledger.release_hold(id) {
                Some(entries) => {
                    if let Err(e) = ledger.persist_window() {
                        log::error!("payout {id}: ledger persist failed: {e}");
                    }
                    shared.drop_coinbaser_base();
                    json!({"id": id, "ok": true, "ts": ts, "status": "released",
                           "released_sats": entries.iter().map(|e| e.1).sum::<u64>(), "entries": entries})
                }
                None => json!({"id": id, "ok": false, "ts": ts, "error": "no such hold"}),
            }
        }
        Request::Paid { txid, blockhash } => {
            let Some(hold) = shared.ledger.lock().unwrap().window.holds().get(id).cloned() else {
                return json!({"id": id, "ok": false, "ts": ts, "error": "no such hold"});
            };
            if txid.len() != 64 || !txid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return json!({"id": id, "ok": false, "ts": ts, "error": "txid is 64 hex characters"});
            }
            let mut params = vec![json!(txid), json!(true)];
            if let Some(b) = blockhash.filter(|b| b.len() == 64 && b.bytes().all(|c| c.is_ascii_hexdigit())) {
                params.push(json!(b));
            }
            let tx = match shared.rpc.call("getrawtransaction", Value::Array(params)).await {
                Ok(tx) => tx,
                Err(e) => {
                    return json!({"id": id, "ok": false, "ts": ts, "retry": true, "status": "waiting",
                                  "error": format!("the node does not show {txid}: {e} (without txindex, give the blockhash)")});
                }
            };
            let conf = tx.get("confirmations").and_then(Value::as_i64).unwrap_or(0);
            if conf < PAID_CONFIRMATIONS {
                return json!({"id": id, "ok": false, "ts": ts, "retry": true, "status": "waiting",
                              "error": format!("{txid} has {conf} confirmations; {PAID_CONFIRMATIONS} needed")});
            }
            if let Err(e) = pays_all(&tx, &hold.entries, |i| address::to_script(i, shared.network)) {
                return json!({"id": id, "ok": false, "ts": ts, "error": format!("{txid} is not this payment: {e}")});
            }
            let mut ledger = shared.ledger.lock().unwrap();
            // read again under the lock: a block on an older coinbaser may have drawn from it
            match ledger.finish_hold(id) {
                Some(entries) => {
                    if let Err(e) = ledger.persist_window() {
                        log::error!("payout {id}: ledger persist failed: {e}");
                    }
                    json!({"id": id, "ok": true, "ts": ts, "status": "paid", "txid": txid, "confirmations": conf,
                           "paid_sats": entries.iter().map(|e| e.1).sum::<u64>(), "entries": entries})
                }
                None => json!({"id": id, "ok": false, "ts": ts, "error": "no such hold"}),
            }
        }
    }
}

/// Whether a verbose `getrawtransaction` result pays every entry's script at least its sats.
fn pays_all(
    tx: &Value,
    entries: &[(String, u64)],
    script_for: impl Fn(&str) -> Option<Vec<u8>>,
) -> Result<(), String> {
    let mut paid: HashMap<String, u64> = HashMap::new();
    for out in tx.get("vout").and_then(Value::as_array).ok_or("no outputs")? {
        let script = out.pointer("/scriptPubKey/hex").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
        let sats = out.get("value").map(btc_to_sats).ok_or("an output has no value")?;
        *paid.entry(script).or_insert(0) += sats;
    }
    for (identity, sats) in entries {
        let script = script_for(identity).ok_or_else(|| format!("{identity} is not an address"))?;
        let got = paid.get(&hex::encode(script)).copied().unwrap_or(0);
        if got < *sats {
            return Err(format!("{identity} is held {sats} sats and paid {got}"));
        }
    }
    Ok(())
}

/// A node's BTC amount as sats, without going through a float's idea of 0.1.
fn btc_to_sats(v: &Value) -> u64 {
    let s = v.to_string();
    let s = s.trim_matches('"');
    if s.contains(['e', 'E']) {
        return (v.as_f64().unwrap_or(0.0) * 1e8).round() as u64;
    }
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut frac = frac.chars().take(8).collect::<String>();
    while frac.len() < 8 {
        frac.push('0');
    }
    whole.parse::<u64>().unwrap_or(0).saturating_mul(100_000_000).saturating_add(frac.parse::<u64>().unwrap_or(0))
}

fn write_json(path: &Path, v: &Value) {
    let tmp = path.with_extension("tmp");
    let body = serde_json::to_vec_pretty(v).unwrap_or_default();
    if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
        log::error!("cannot write {}: {e}", path.display());
    }
}

fn append_log(path: &Path, v: &Value) {
    use std::io::Write;
    let line = format!("{v}\n");
    if let Err(e) =
        std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(line.as_bytes()))
    {
        log::error!("cannot append to {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_are_read_exactly() {
        assert_eq!(btc_to_sats(&json!(0.00499226)), 499_226);
        assert_eq!(btc_to_sats(&json!(0.1)), 10_000_000);
        assert_eq!(btc_to_sats(&json!(3.125)), 312_500_000);
        assert_eq!(btc_to_sats(&json!(1)), 100_000_000);
        assert_eq!(btc_to_sats(&json!(1e-8)), 1);
        assert_eq!(btc_to_sats(&json!("0.00010000")), 10_000);
    }

    #[test]
    fn a_payment_must_cover_every_held_balance() {
        let script = |id: &str| (!id.starts_with("bad")).then(|| vec![0x00, 0x14, id.as_bytes()[0]]);
        let tx = json!({"vout": [
            {"value": 0.00499226, "scriptPubKey": {"hex": "001461"}},
            {"value": 0.0001, "scriptPubKey": {"hex": "001462"}},
            {"value": 0.0002, "scriptPubKey": {"hex": "001462"}},
            {"value": 1.5, "scriptPubKey": {"hex": "0014ff"}},
        ]});
        let held = |v: &[(&str, u64)]| v.iter().map(|(i, s)| (i.to_string(), *s)).collect::<Vec<_>>();
        assert!(pays_all(&tx, &held(&[("a", 499_226), ("b", 30_000)]), script).is_ok(), "two outputs to b add up");
        assert!(pays_all(&tx, &held(&[("a", 499_227)]), script).is_err(), "a sat short");
        assert!(pays_all(&tx, &held(&[("c", 1)]), script).is_err(), "not paid at all");
        assert!(pays_all(&tx, &held(&[("bad", 1)]), script).is_err());
        assert!(pays_all(&json!({}), &held(&[("a", 1)]), script).is_err());
    }

    #[test]
    fn ids_cannot_leave_the_directory() {
        assert!(valid_id("stale-20260919"));
        for bad in ["", "../x", "a/b", ".hidden", "a b", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad:?}");
        }
    }
}
