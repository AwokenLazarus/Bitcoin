//! Node poller: tracks the tip and difficulty, sizes the TIDES window, relays new-block
//! notifications, and confirms found blocks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::rpc::RpcError;
use crate::state::{hash_le, now, Shared, Tip};

pub async fn run(shared: Arc<Shared>) {
    let period = Duration::from_secs_f64(shared.cfg.poll.max(0.2));
    let mut confirm_at = Instant::now();
    let mut warned = false;
    // The tip this loop last ran the per-block work for. A session can publish a new tip before
    // the poller sees it (`refresh_ahead`), so "the tip moved" is judged here, not in `refresh`.
    let mut scanned: Option<String> = None;
    loop {
        match refresh(&shared).await {
            Ok((height, difficulty)) => {
                if warned {
                    log::info!("node is back");
                    warned = false;
                }
                if difficulty > 0.0 {
                    let target = shared.window_target(difficulty);
                    let mut ledger = shared.ledger.lock().unwrap();
                    if ledger.window.target_work() != target {
                        log::info!("TIDES window target -> {target} ({}x difficulty)", shared.cfg.window);
                        ledger.set_target(target);
                        if let Err(e) = ledger.persist_window() {
                            log::error!("ledger persist after target change failed: {e}");
                        }
                    }
                }
                if confirm_at <= Instant::now() {
                    confirm_at = Instant::now() + Duration::from_secs(30);
                    confirm_blocks(&shared, height).await;
                }
                let hash = shared.tip_snapshot().map(|t| t.hash);
                if hash.is_some() && hash != scanned {
                    scanned = hash;
                    // book the DATUM rebate share of any solo block the chain just buried
                    crate::solo::scan(&shared, height).await;
                }
            }
            Err(e) => {
                if !warned {
                    log::warn!(
                        "node rpc failed: {e} (jobs are held to the last tip it gave; with none yet, shares are refused)"
                    );
                    warned = true;
                }
            }
        }
        tokio::time::sleep(period).await;
    }
}

/// Read the node's tip and publish it if it moved; returns its height and difficulty.
///
/// Along with the tip come the things a job is held to (`Tip::check_job`): the tip's parent
/// and compact target from its header, and the target the node sets for the block after it.
/// Either lookup can fail on its own; the tip is published without it, shares that need it
/// are refused, and the next call asks again.
///
/// The poller calls this every period. Sessions go through [`refresh_ahead`].
pub async fn refresh(shared: &Shared) -> Result<(u32, f64), RpcError> {
    let mut last = shared.refresh.lock().await;
    let r = refresh_locked(shared).await;
    *last = Some(Instant::now());
    r
}

/// The least time between two refreshes that sessions cause, across all of them. Whether a
/// share is "ahead" is decided before any of its work is checked, so it costs a gateway nothing
/// to say so; without a floor that every session shares, a few hundred connections turn into a
/// few hundred node RPCs a second, queued in front of the poller.
const AHEAD_REFRESH_FLOOR: Duration = Duration::from_millis(250);
/// How long a session waits on the node for an early tip before carrying on without one.
const AHEAD_REFRESH_PATIENCE: Duration = Duration::from_secs(2);

/// A session's refresh, for when a gateway submits work past our tip, so good work is not
/// refused for the rest of a poll period. Does nothing if anyone refreshed while this call
/// waited its turn, or within [`AHEAD_REFRESH_FLOOR`]: that answer is as new as ours would be.
pub async fn refresh_ahead(shared: &Shared) {
    let asked = Instant::now();
    let mut last = shared.refresh.lock().await;
    if last.is_some_and(|t| t >= asked || t.elapsed() < AHEAD_REFRESH_FLOOR) {
        return;
    }
    // Bounded well under the RPC timeout: this runs on the session's own task, between a share
    // and its receipt, and a node that has stopped answering is the poller's to wait for.
    let _ = tokio::time::timeout(AHEAD_REFRESH_PATIENCE, refresh_locked(shared)).await;
    *last = Some(Instant::now());
}

async fn refresh_locked(shared: &Shared) -> Result<(u32, f64), RpcError> {
    let info = shared.rpc.getblockchaininfo().await?;
    let height = info.get("blocks").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let hash = info.get("bestblockhash").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let difficulty = info.get("difficulty").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let current = shared.tip_snapshot().filter(|t| t.hash == hash);
    let changed = current.is_none();
    let (mut parent_le, mut bits, mut next_bits) =
        current.as_ref().map_or((None, None, None), |t| (t.parent_le, t.bits, t.next_bits));
    if parent_le.is_none() || bits.is_none() {
        match shared.rpc.getblockheader(&hash).await {
            Ok(h) => {
                parent_le = h.get("previousblockhash").and_then(|v| v.as_str()).and_then(hash_le);
                bits = h.get("bits").and_then(compact);
            }
            Err(e) => log::warn!("getblockheader {hash}: {e}; work on the previous height is refused until it answers"),
        }
    }
    if next_bits.is_none() {
        match shared.rpc.getmininginfo().await {
            // `next` describes the block after the tip the node had when it answered
            Ok(m) if m.get("blocks").and_then(|v| v.as_u64()) == Some(u64::from(height)) => {
                next_bits = m.get("next").and_then(|n| n.get("bits")).and_then(compact);
            }
            Ok(_) => {}
            Err(e) => log::warn!("getmininginfo: {e}"),
        }
    }
    if next_bits.is_none() {
        // Nodes before v29 do not report `next`. Off a retarget boundary the next block's
        // target is the tip's, which the header gave us. On one (or on a network with
        // min-difficulty blocks) it stays unknown and `Tip::check_job` falls back to the
        // consensus bound: no easier than four times the tip's target.
        next_bits = bits.filter(|_| next_bits_follow_tip(&shared.cfg.network, height));
        if changed && next_bits.is_none() {
            log::warn!("next block's bits unknown at height {height}; a job's nbits is only bounded, not matched");
        }
    }
    let (seen_at, seen_ts) = current.as_ref().map_or_else(|| (Instant::now(), now()), |t| (t.seen_at, t.seen_ts));
    let tip = Tip { height, hash_le: hash_le(&hash), hash, difficulty, parent_le, bits, next_bits, seen_at, seen_ts };
    if changed {
        log::info!("tip height={height} hash={} difficulty={difficulty:.3}", &tip.hash[..tip.hash.len().min(16)]);
    }
    if current.as_ref() != Some(&tip) {
        let first = shared.tip_snapshot().is_none();
        shared.tip_tx.send_replace(Some(tip));
        if changed && !first {
            let _ = shared.notify.send(0);
        }
    }
    Ok((height, difficulty))
}

/// Whether the block after `height` must carry the same compact target as the block at it.
fn next_bits_follow_tip(network: &str, height: u32) -> bool {
    match network {
        // fPowNoRetargeting
        "regtest" => true,
        "mainnet" | "signet" => !(height + 1).is_multiple_of(2016),
        // testnet allows min-difficulty blocks whenever the tip is 20 minutes old
        _ => false,
    }
}

/// A compact target as the node prints it (`"1d00ffff"`).
fn compact(v: &serde_json::Value) -> Option<u32> {
    u32::from_str_radix(v.as_str()?, 16).ok()
}

/// How many blocks past a candidate we keep re-checking one we have called an orphan. A block
/// found on a gateway whose node lagged the pool node by one is a competing tip until the next
/// block lands; if that lands on the gateway's branch, the "orphan" is main chain after all.
const ORPHAN_RECHECK_BLOCKS: u32 = 100;

/// Mark recorded blocks settled once the node has them in the main chain.
async fn confirm_blocks(shared: &Shared, tip_height: u32) {
    let pending: Vec<(String, u32)> = shared
        .blocks
        .lock()
        .unwrap()
        .iter()
        .rev()
        .filter(|b| {
            if b.settled {
                return false;
            }
            if b.kind.starts_with("orphan") {
                b.height + ORPHAN_RECHECK_BLOCKS > tip_height
            } else {
                b.height + 2000 > tip_height
            }
        })
        .take(20)
        .map(|b| (b.hash.clone(), b.height))
        .collect();
    for (hash, height) in pending {
        match shared.rpc.getblockheader(&hash).await {
            Ok(h) => {
                let conf = h.get("confirmations").and_then(|v| v.as_i64()).unwrap_or(0);
                if conf > 0 {
                    log::info!("block {hash} at {height} confirmed ({conf})");
                    let mut reapply = Vec::new();
                    let mut rebate = 0i64;
                    shared.update_block(&hash, |r| {
                        if let Some(kind) = r.kind.strip_prefix("orphan:") {
                            log::info!("block {} at {} is back in the main chain", r.hash, r.height);
                            r.kind = kind.to_string();
                            // mark_orphan reversed its carry and rebate; put them back
                            reapply = r.carry_delta.clone();
                            rebate = r.rebate_delta;
                        }
                        r.settled = true;
                    });
                    if !reapply.is_empty() || rebate != 0 {
                        let mut ledger = shared.ledger.lock().unwrap();
                        ledger.settle_carry(&reapply);
                        let owed = ledger.settle_rebate(rebate);
                        log::info!(
                            "block {hash}: re-applied carry for {} identities, DATUM rebate {rebate:+} -> owed {owed}",
                            reapply.len()
                        );
                    }
                } else if conf < 0 {
                    mark_orphan(shared, &hash, height, "is not in the main chain");
                }
            }
            Err(RpcError::Node { code: -5, .. }) => {
                // unknown to the node yet; if the chain has moved well past it, it lost
                if tip_height > height + 6 {
                    mark_orphan(shared, &hash, height, "never reached the node");
                }
            }
            Err(e) => log::debug!("getblockheader {hash}: {e}"),
        }
    }
}

/// Label a recorded block as orphaned, once. Orphans stay unsettled so `confirm_blocks` keeps
/// re-checking them for a while; this must not stack a prefix per pass.
fn mark_orphan(shared: &Shared, hash: &str, height: u32, why: &str) {
    let already = shared
        .blocks
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.hash == hash)
        .map_or(true, |r| r.kind.starts_with("orphan:"));
    if already {
        return;
    }
    log::warn!("block {hash} at {height} {why}");
    let mut reverse = Vec::new();
    let mut rebate = 0i64;
    shared.update_block(hash, |r| {
        r.kind = format!("orphan:{}", r.kind);
        // an orphan's coinbase paid nobody: give back the carry it cleared and take back
        // the earnings it deferred (the work is still in the window to be paid properly);
        // likewise the DATUM rebate it paid down is still owed
        reverse = r.carry_delta.iter().map(|(i, d)| (i.clone(), -*d)).collect();
        rebate = -r.rebate_delta;
    });
    if !reverse.is_empty() || rebate != 0 {
        let mut ledger = shared.ledger.lock().unwrap();
        ledger.settle_carry(&reverse);
        let owed = ledger.settle_rebate(rebate);
        log::info!(
            "block {hash}: reversed carry for {} identities, DATUM rebate {rebate:+} -> owed {owed}",
            reverse.len()
        );
    }
}
