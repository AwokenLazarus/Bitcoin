//! Node poller: tracks the tip and difficulty, sizes the TIDES window, relays new-block
//! notifications, and confirms found blocks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::state::{now, Shared, Tip};

pub async fn run(shared: Arc<Shared>) {
    let period = Duration::from_secs_f64(shared.cfg.poll.max(0.2));
    let mut confirm_at = Instant::now();
    let mut warned = false;
    loop {
        match shared.rpc.getblockchaininfo().await {
            Ok(info) => {
                if warned {
                    log::info!("node is back");
                    warned = false;
                }
                let height = info.get("blocks").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let hash = info.get("bestblockhash").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let difficulty = info.get("difficulty").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let current = shared.tip_snapshot();
                let changed = current.as_ref().is_none_or(|t| t.hash != hash);
                if changed {
                    let tip = Tip { height, hash: hash.clone(), difficulty, seen_at: Instant::now(), seen_ts: now() };
                    log::info!("tip height={height} hash={} difficulty={difficulty:.3}", &hash[..hash.len().min(16)]);
                    shared.tip_tx.send_replace(Some(tip));
                    if current.is_some() {
                        let _ = shared.notify.send(0);
                    }
                }
                if difficulty > 0.0 {
                    let target = shared.window_target(difficulty);
                    let mut ledger = shared.ledger.lock().unwrap();
                    if ledger.window.target_work() != target {
                        log::info!("TIDES window target -> {target} ({}x difficulty)", shared.cfg.window);
                        ledger.set_target(target);
                    }
                }
                if confirm_at <= Instant::now() {
                    confirm_at = Instant::now() + Duration::from_secs(30);
                    confirm_blocks(&shared, height).await;
                }
            }
            Err(e) => {
                if !warned {
                    log::warn!("node rpc failed: {e} (shares are still accepted without a staleness check)");
                    warned = true;
                }
            }
        }
        tokio::time::sleep(period).await;
    }
}

/// Mark recorded blocks settled once the node has them in the main chain.
async fn confirm_blocks(shared: &Shared, tip_height: u32) {
    let pending: Vec<(String, u32)> = shared
        .blocks
        .lock()
        .unwrap()
        .iter()
        .rev()
        .filter(|b| !b.settled && !b.kind.starts_with("orphan") && b.height + 2000 > tip_height)
        .take(20)
        .map(|b| (b.hash.clone(), b.height))
        .collect();
    for (hash, height) in pending {
        match shared.rpc.getblockheader(&hash).await {
            Ok(h) => {
                let conf = h.get("confirmations").and_then(|v| v.as_i64()).unwrap_or(0);
                if conf > 0 {
                    log::info!("block {hash} at {height} confirmed ({conf})");
                    shared.update_block(&hash, |r| r.settled = true);
                } else if conf < 0 {
                    log::warn!("block {hash} at {height} is not in the main chain");
                    shared.update_block(&hash, |r| r.kind = format!("orphan:{}", r.kind));
                }
            }
            Err(crate::rpc::RpcError::Node { code: -5, .. }) => {
                // unknown to the node yet; if the chain has moved well past it, it lost
                if tip_height > height + 6 {
                    log::warn!("block {hash} at {height} never reached the node");
                    shared.update_block(&hash, |r| r.kind = format!("orphan:{}", r.kind));
                }
            }
            Err(e) => log::debug!("getblockheader {hash}: {e}"),
        }
    }
}
