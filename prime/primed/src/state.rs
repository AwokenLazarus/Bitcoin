//! State shared by every session, the node poller, and the stats server.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use datum_wire::crypto::Identity;
use tides::{BlockLog, BlockRecord, Ledger, SplitParams};
use tokio::sync::{broadcast, watch};

use crate::address::Network;
use crate::config::Config;
use crate::rpc::Rpc;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tip {
    pub height: u32,
    pub hash: String,
    pub difficulty: f64,
    /// When this Prime first saw the tip; shares for the previous height are accepted for
    /// a grace period after it.
    pub seen_at: Instant,
    pub seen_ts: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ClientInfo {
    pub id: u64,
    pub remote: String,
    pub user_agent: String,
    pub generation: &'static str,
    /// Hex prefix of the gateway's long-term signing key.
    pub gateway: String,
    /// `stratum` = house public gateway (higher fee); `datum` = external Prime client.
    pub fee_path: String,
    pub connected_ts: u64,
    pub identity: String,
    pub accepted: u64,
    pub rejected: u64,
    pub work: u64,
    pub last_share_ts: u64,
    pub coinbasers: u64,
    pub block_candidates: u64,
    pub last_reject: Option<&'static str>,
}

#[derive(Default)]
pub struct Totals {
    pub connections: AtomicU64,
    pub accepted: AtomicU64,
    pub rejected: AtomicU64,
    pub work: AtomicU64,
    pub coinbasers: AtomicU64,
    pub block_candidates: AtomicU64,
    pub blocks_submitted: AtomicU64,
    pub handshake_failures: AtomicU64,
}

impl Totals {
    pub fn add(&self, c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

pub struct Shared {
    pub cfg: Config,
    pub pool: Identity,
    pub pool_script: Vec<u8>,
    pub network: Network,
    pub split_params: SplitParams,
    pub ledger: Mutex<Ledger>,
    pub blocks: Mutex<Vec<BlockRecord>>,
    pub block_log: BlockLog,
    pub clients: Mutex<HashMap<u64, ClientInfo>>,
    pub tip_tx: watch::Sender<Option<Tip>>,
    pub tip: watch::Receiver<Option<Tip>>,
    /// Fired when a block candidate is found or the node tip moves; sessions relay a
    /// block-notify so gateways refresh their templates.
    pub notify: broadcast::Sender<u32>,
    pub rpc: Rpc,
    pub totals: Totals,
    pub started: Instant,
    pub started_ts: u64,
    pub next_client_id: AtomicU64,
}

impl Shared {
    pub fn client_update(&self, id: u64, f: impl FnOnce(&mut ClientInfo)) {
        if let Some(c) = self.clients.lock().unwrap().get_mut(&id) {
            f(c);
        }
    }

    pub fn tip_snapshot(&self) -> Option<Tip> {
        self.tip.borrow().clone()
    }

    /// Target work for the TIDES window from the current network difficulty.
    pub fn window_target(&self, difficulty: f64) -> u64 {
        let t = (difficulty * f64::from(self.cfg.window)).round().max(1.0) as u64;
        t.max(self.cfg.window_min_work)
    }

    pub fn record_block(&self, r: BlockRecord) {
        if let Err(e) = self.block_log.append(&r) {
            log::error!("block log append failed: {e}");
        }
        let mut b = self.blocks.lock().unwrap();
        b.push(r);
        if b.len() > 10_000 {
            let excess = b.len() - 10_000;
            b.drain(..excess);
        }
    }

    pub fn update_block(&self, hash: &str, f: impl FnOnce(&mut BlockRecord)) -> Option<BlockRecord> {
        let updated = {
            let mut b = self.blocks.lock().unwrap();
            let r = b.iter_mut().rev().find(|r| r.hash == hash)?;
            f(r);
            r.clone()
        };
        if let Err(e) = self.block_log.append(&updated) {
            log::error!("block log append failed: {e}");
        }
        Some(updated)
    }
}
