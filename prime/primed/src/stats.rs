//! `stats.json` for the pool UI, plus a legacy `ledger.json` export the UI's hashrate code
//! reads. Served over HTTP and mirrored to files in the data directory.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::address;
use crate::state::{now, Shared};

/// One unit of window work is one difficulty-1 share: 2^32 hashes.
const HASHES_PER_WORK: f64 = 4_294_967_296.0;
const HASHRATE_WINDOW_S: u64 = 600;
const LEDGER_EXPORT_S: u64 = 3600;

pub fn build(shared: &Shared) -> Value {
    let ts = now();
    let tip = shared.tip_snapshot();
    let ledger = shared.ledger.lock().unwrap();
    let w = &ledger.window;

    // recent work per identity for hashrate; walk the window from the newest row
    let cutoff = ts.saturating_sub(HASHRATE_WINDOW_S) as u32;
    let mut recent: std::collections::HashMap<u32, (u64, u32)> = std::collections::HashMap::new();
    let mut recent_total = 0u64;
    let mut oldest = ts as u32;
    for c in w.credits().rev() {
        if c.ts <= cutoff {
            break;
        }
        let e = recent.entry(c.ident).or_insert((0, 0));
        e.0 += c.work;
        e.1 = e.1.max(c.ts);
        recent_total += c.work;
        oldest = oldest.min(c.ts);
    }
    // a Prime that has been up for two minutes averages over two minutes, not ten
    let span = (ts.saturating_sub(u64::from(oldest)).max(ts.saturating_sub(shared.started_ts)))
        .clamp(30, HASHRATE_WINDOW_S) as f64;
    let ghs = |work: u64| work as f64 * HASHES_PER_WORK / span / 1e9;

    // the split a block would pay right now: how the UI shows each miner's expected payout
    let sample_value = 312_500_000u64;
    let split = w.split(sample_value, &shared.split_params, |i| address::to_script(i, shared.network));
    let payout_of = |ident: &str| split.payees.iter().find(|p| p.identity == ident).map(|p| p.sats);

    let miners: Vec<Value> = w
        .miners()
        .into_iter()
        .map(|m| {
            let idx = ledger.window.identities().iter().position(|i| *i == m.identity).map(|i| i as u32);
            let (rw, last) = idx.and_then(|i| recent.get(&i)).copied().unwrap_or((0, m.last_ts));
            let payable = address::to_script(&m.identity, shared.network).is_some();
            json!({
                "identity": m.identity,
                "work": m.work,
                "stratum_work": m.stratum_work,
                "fee_path": if m.stratum_work * 2 > m.work { "stratum" } else { "datum" },
                "credits": m.credits,
                "share_percent": if w.total_work() > 0 { 100.0 * m.work as f64 / w.total_work() as f64 } else { 0.0 },
                "payout_sats": payout_of(&m.identity).unwrap_or(0),
                "payable": payable,
                "hashrate_ghs": ghs(rw),
                "last_share_s": ts.saturating_sub(u64::from(last.max(m.last_ts))),
            })
        })
        .collect();

    let blocks: Vec<Value> = {
        let b = shared.blocks.lock().unwrap();
        b.iter().rev().take(100).map(|r| serde_json::to_value(r).unwrap_or(Value::Null)).collect()
    };
    let owed: u64 =
        shared.blocks.lock().unwrap().iter().filter(|b| !b.kind.starts_with("orphan")).map(|b| b.owed_sats).sum();
    let clients: Vec<Value> = {
        let c = shared.clients.lock().unwrap();
        let mut v: Vec<_> = c.values().cloned().collect();
        v.sort_by_key(|c| c.id);
        v.into_iter()
            .map(|c| {
                let mut j = serde_json::to_value(&c).unwrap_or(Value::Null);
                if let Some(o) = j.as_object_mut() {
                    o.insert("connected_s".into(), json!(ts.saturating_sub(c.connected_ts)));
                    o.insert(
                        "last_share_s".into(),
                        json!(if c.last_share_ts > 0 { ts.saturating_sub(c.last_share_ts) } else { 0 }),
                    );
                }
                j
            })
            .collect()
    };

    let t = &shared.totals;
    let (host, port) = shared
        .cfg
        .advertise_address
        .rsplit_once(':')
        .map(|(h, p)| (h.to_string(), p.parse::<u16>().unwrap_or(shared.cfg.listen.port())))
        .unwrap_or_else(|| (shared.cfg.advertise_address.clone(), shared.cfg.listen.port()));

    json!({
        "ts": ts,
        "build": { "name": "primed", "version": env!("CARGO_PKG_VERSION") },
        "uptime_s": shared.started.elapsed().as_secs(),
        "started_ts": shared.started_ts,
        "pool": {
            "headline": shared.cfg.headline,
            "pubkey": shared.pool.public_hex(),
            "address": shared.cfg.payout_address,
            "script": hex::encode(&shared.pool_script),
            "tag": shared.cfg.coinbase_tag,
            "prime_id": shared.cfg.prime_id,
            "fee_bps": shared.cfg.fee_bps,
            "stratum_fee_bps": shared.cfg.stratum_fee_bps,
            "window_multiple": shared.cfg.window,
            "min_payout": shared.cfg.min_payout,
            "min_diff": shared.cfg.min_diff,
            "network": shared.cfg.network,
            "advertise": shared.cfg.advertise_address,
            "datum": { "host": host, "port": port, "pubkey": shared.pool.public_hex() },
        },
        "node": tip.as_ref().map(|t| json!({
            "height": t.height, "tip": t.hash, "difficulty": t.difficulty, "tip_age_s": ts.saturating_sub(t.seen_ts),
        })).unwrap_or(Value::Null),
        "window": {
            "shares": w.len(),
            "work": w.total_work(),
            "target_work": w.target_work(),
            "fill_percent": if w.target_work() > 0 { 100.0 * w.total_work() as f64 / w.target_work() as f64 } else { 0.0 },
            "miners": miners,
            "identities": w.identities().len(),
            "hashrate_ghs": ghs(recent_total),
            "sample_value": sample_value,
            "sample_pool_sats": split.pool_sats,
            "sample_fee_sats": split.fee_sats,
        },
        "hashrate": { "pool_ghs": ghs(recent_total), "window_s": HASHRATE_WINDOW_S },
        "totals": {
            "shares_accepted": t.accepted.load(Ordering::Relaxed),
            "shares_rejected": t.rejected.load(Ordering::Relaxed),
            "work_accepted": t.work.load(Ordering::Relaxed),
            "lifetime_shares": w.lifetime_shares,
            "lifetime_work": w.lifetime_work,
            "coinbasers": t.coinbasers.load(Ordering::Relaxed),
            "block_candidates": t.block_candidates.load(Ordering::Relaxed),
            "blocks_submitted": t.blocks_submitted.load(Ordering::Relaxed),
            "connections": t.connections.load(Ordering::Relaxed),
            "handshake_failures": t.handshake_failures.load(Ordering::Relaxed),
        },
        "clients": clients,
        "gateways": clients.len(),
        "owed": owed,
        "blocks": blocks,
    })
}

/// The legacy `ledger.json` shape (`{"credits":[{ts,identity,work}], ...}`) for the last hour.
pub fn legacy_ledger(shared: &Shared) -> Value {
    let ledger = shared.ledger.lock().unwrap();
    let w = &ledger.window;
    let cutoff = now().saturating_sub(LEDGER_EXPORT_S) as u32;
    let mut credits = Vec::new();
    for c in w.credits().rev() {
        if c.ts <= cutoff {
            break;
        }
        credits.push(
            json!({ "ts": c.ts, "identity": w.identity(c.ident).unwrap_or(""), "work": c.work, "height": c.height }),
        );
    }
    credits.reverse();
    json!({ "credits": credits, "shares": w.lifetime_shares, "window_work": w.total_work(), "target_work": w.target_work() })
}

pub async fn serve(shared: Arc<Shared>) {
    let listener = match TcpListener::bind(shared.cfg.stats_listen).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("stats listener {} failed: {e}", shared.cfg.stats_listen);
            return;
        }
    };
    log::info!("stats on http://{}/stats.json", shared.cfg.stats_listen);
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let shared = shared.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let n = match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await {
                Ok(Ok(n)) => n,
                _ => return,
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (status, ctype, body) = match path.split('?').next().unwrap_or("/") {
                "/" | "/stats.json" | "/stats" => ("200 OK", "application/json", build(&shared).to_string()),
                "/ledger.json" => ("200 OK", "application/json", legacy_ledger(&shared).to_string()),
                "/healthz" => ("200 OK", "text/plain", "ok\n".to_string()),
                _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        });
    }
}

/// Mirror stats and the legacy ledger to files, and flush the ledger, on a timer.
pub async fn housekeeping(shared: Arc<Shared>) {
    let mut n = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        n += 1;
        {
            let mut ledger = shared.ledger.lock().unwrap();
            let r = if n.is_multiple_of(12) { ledger.sync() } else { ledger.flush() };
            if let Err(e) = r {
                log::error!("ledger flush failed: {e}");
            }
        }
        let dir = shared.cfg.data_dir.clone();
        let stats = build(&shared).to_string();
        let legacy = legacy_ledger(&shared).to_string();
        let _ = tokio::task::spawn_blocking(move || {
            write_atomic(&dir.join("stats.json"), stats.as_bytes());
            write_atomic(&dir.join("ledger.json"), legacy.as_bytes());
        })
        .await;
    }
}

fn write_atomic(path: &std::path::Path, data: &[u8]) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
        log::debug!("could not write {}", path.display());
    }
}
