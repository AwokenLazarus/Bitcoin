//! `primed` — the Lazarus DATUM Prime.
//!
//! Accepts stock DATUM gateways, tells them the TIDES coinbase split for every template
//! they build, verifies the BLAKE2b work they send back, credits the window, and relays
//! found blocks to the node.

mod address;
mod config;
mod node;
mod rpc;
mod session;
mod state;
mod stats;

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use clap::{Parser, Subcommand};
use datum_wire::crypto::Identity;
use tides::{BlockLog, Ledger, SplitParams};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use crate::address::Network;
use crate::config::Config;
use crate::state::{now, Shared, Totals};

#[derive(Parser)]
#[command(name = "primed", version, about = "Lazarus DATUM Prime: pool side of the DATUM protocol with TIDES payouts")]
struct Cli {
    /// Path to prime.toml
    #[arg(short, long, default_value = "prime.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the Prime (default).
    Run,
    /// Print the pool public key gateways should be configured with.
    Pubkey,
    /// Validate the config and exit.
    Check,
    /// Import credits from a legacy ledger.json into the window.
    ImportLedger { path: PathBuf },
    /// Print the current window as JSON.
    Window,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_secs().init();
    let cli = Cli::parse();
    let cfg = match Config::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("config: {e}");
            std::process::exit(2);
        }
    };
    for note in cfg.legacy_notes() {
        log::warn!("config: {note}");
    }
    let code = match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Check => match pool_payout_script(&cfg) {
            Ok(_) => {
                println!(
                    "ok: listen={} stats={} payout={} fee={}bps stratum={}bps window={}x",
                    cfg.listen, cfg.stats_listen, cfg.payout_address, cfg.fee_bps, cfg.stratum_fee_bps, cfg.window
                );
                0
            }
            Err(e) => {
                eprintln!("{e}");
                2
            }
        },
        Cmd::Pubkey => match load_or_create_key(&cfg) {
            Ok(k) => {
                println!("{}", k.public_hex());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Cmd::ImportLedger { path } => match Ledger::open(&cfg.data_dir).and_then(|mut l| {
            let n = l.import_json_credits(&path)?;
            l.sync()?;
            Ok((n, l.window.total_work(), l.window.len()))
        }) {
            Ok((n, work, rows)) => {
                println!("imported {n} credits; window now {rows} rows, {work} work");
                0
            }
            Err(e) => {
                eprintln!("import failed: {e}");
                1
            }
        },
        Cmd::Window => match Ledger::open(&cfg.data_dir) {
            Ok(l) => {
                let miners = l.window.miners();
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "work": l.window.total_work(), "target_work": l.window.target_work(), "rows": l.window.len(), "miners": miners,
                })).unwrap());
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        Cmd::Run => run(cfg),
    };
    std::process::exit(code);
}

fn load_or_create_key(cfg: &Config) -> Result<Identity, String> {
    let path = cfg.key_file();
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let bytes = hex::decode(s.trim()).map_err(|e| format!("{}: {e}", path.display()))?;
            match bytes.len() {
                64 => Ok(Identity::from_secret_bytes(&bytes.try_into().unwrap())),
                // lazarus-prime layout: ed25519 pk (32) | ed25519 sk as libsodium keeps it,
                // seed‖pk (64) | x25519 pk (32) | x25519 sk (32). Same identity, so gateways
                // that pinned the old pubkey keep connecting.
                160 => {
                    let mut arr = [0u8; 64];
                    arr[..32].copy_from_slice(&bytes[32..64]);
                    arr[32..].copy_from_slice(&bytes[128..160]);
                    let id = Identity::from_secret_bytes(&arr);
                    if id.sign_pk() != bytes[..32] || id.box_pk() != bytes[96..128] {
                        return Err(format!("{}: public keys do not match the secret halves", path.display()));
                    }
                    Ok(id)
                }
                n => Err(format!("{}: expected 64 (primed) or 160 (lazarus-prime) bytes, found {n}", path.display())),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&cfg.data_dir).map_err(|e| e.to_string())?;
            let id = Identity::generate();
            write_secret(path, hex::encode(id.secret_bytes()).as_bytes())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            log::info!("generated new pool key at {} — pubkey {}", path.display(), id.public_hex());
            Ok(id)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(unix)]
fn write_secret(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(data)?;
    f.write_all(b"\n")
}

#[cfg(not(unix))]
fn write_secret(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)
}

/// The network and the pool's own coinbase output script, or why the config cannot be used.
/// `check` runs this too, so an unpayable payout address is caught before a restart.
fn pool_payout_script(cfg: &Config) -> Result<(Network, Vec<u8>), String> {
    let network = Network::parse(&cfg.network).ok_or_else(|| format!("unknown network {:?}", cfg.network))?;
    match address::decode_script(&cfg.payout_address, network) {
        // The pool's output carries the value the split could not place, so a gateway cannot
        // just leave it out: the newest ones refuse to serve work for the block instead.
        Some(s) if !address::rdts_output_ok(&s) => Err(format!(
            "payout-address {:?} decodes to a {}-byte output script, which a coinbase cannot \
             carry (limit {} bytes); gateways will not serve work for it",
            cfg.payout_address,
            s.len(),
            address::RDTS_MAX_OUTPUT_SCRIPT
        )),
        Some(s) => Ok((network, s)),
        None => Err(format!("payout-address {:?} is not a valid {} address", cfg.payout_address, cfg.network)),
    }
}

fn run(cfg: Config) -> i32 {
    let (network, pool_script) = match pool_payout_script(&cfg) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let pool = match load_or_create_key(&cfg) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let rpc = match rpc::Rpc::new(
        &cfg.rpc,
        cfg.rpc_cookie.as_deref(),
        cfg.rpc_user.as_deref(),
        cfg.rpc_password.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("rpc: {e}");
            return 2;
        }
    };
    let mut ledger = match Ledger::open(&cfg.data_dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ledger: {e}");
            return 1;
        }
    };
    if let Some(p) = &cfg.import_ledger {
        if ledger.window.is_empty() && p.exists() {
            match ledger.import_json_credits(p) {
                Ok(n) => log::info!("imported {n} credits from {}", p.display()),
                Err(e) => log::warn!("legacy import from {} failed: {e}", p.display()),
            }
        }
    }
    log::info!(
        "ledger: {} rows, {} work (target {}), {} identities, lifetime {} shares",
        ledger.window.len(),
        ledger.window.total_work(),
        ledger.window.target_work(),
        ledger.window.identities().len(),
        ledger.window.lifetime_shares
    );
    warn_if_window_cliff(&cfg.data_dir, &ledger);
    let block_log = BlockLog::open(&cfg.data_dir);
    let blocks = block_log.read_all().unwrap_or_default();

    let (tip_tx, tip) = watch::channel(None);
    let (notify, _) = broadcast::channel(64);
    let shared = Arc::new(Shared {
        split_params: SplitParams {
            fee_bps: cfg.fee_bps,
            stratum_fee_bps: cfg.stratum_fee_bps,
            min_payout: cfg.min_payout,
            // The gateway accepts at most 512 coinbaser entries; one is the pool's own
            // output appended after the payees. The byte budget leaves room for it too.
            max_outputs: 511,
            output_budget_bytes: 14_000 - 9 - 64,
        },
        pool_script,
        pool,
        network,
        ledger: Mutex::new(ledger),
        blocks: Mutex::new(blocks),
        block_log,
        clients: Mutex::new(Default::default()),
        seen: Mutex::new(Default::default()),
        connections: Mutex::new(Default::default()),
        tip_tx,
        tip,
        notify,
        rpc,
        totals: Totals::default(),
        started: Instant::now(),
        started_ts: now(),
        next_client_id: AtomicU64::new(1),
        coinbaser_base: Mutex::new(None),
        cfg,
    });
    log::info!("pool pubkey {}", shared.pool.public_hex());
    log::info!(
        "payout {} fee {}bps stratum {}bps window {}x min-diff {}",
        shared.cfg.payout_address,
        shared.cfg.fee_bps,
        shared.cfg.stratum_fee_bps,
        shared.cfg.window,
        shared.cfg.min_diff
    );

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    rt.block_on(async move {
        let listener = match TcpListener::bind(shared.cfg.listen).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("listen {} failed: {e}", shared.cfg.listen);
                return 1;
            }
        };
        log::info!("DATUM listening on {}", shared.cfg.listen);
        tokio::spawn(node::run(shared.clone()));
        tokio::spawn(stats::serve(shared.clone()));
        tokio::spawn(stats::housekeeping(shared.clone()));

        let accept = {
            let shared = shared.clone();
            async move {
                let mut refused_warned = Instant::now() - std::time::Duration::from_secs(60);
                loop {
                    match listener.accept().await {
                        Ok((stream, remote)) => {
                            // Admit before spawning: a session holds job and coinbase state
                            // for its gateway, so the count of them is the memory bound.
                            let admitted = shared.connections.lock().unwrap().admit(
                                remote.ip(),
                                shared.cfg.max_connections,
                                shared.cfg.max_connections_per_ip,
                            );
                            if let Err(why) = admitted {
                                shared.totals.add(&shared.totals.connections_refused, 1);
                                if refused_warned.elapsed() >= std::time::Duration::from_secs(10) {
                                    refused_warned = Instant::now();
                                    log::warn!("{remote} refused: {why}");
                                }
                                drop(stream);
                                continue;
                            }
                            let shared = shared.clone();
                            tokio::spawn(async move {
                                // Released on drop, so a panicking session (tokio catches
                                // it) still gives its slot back.
                                let _slot = ConnectionSlot { shared: shared.clone(), ip: remote.ip() };
                                match session::run(shared.clone(), stream, remote).await {
                                    Ok(()) => log::info!("{remote} closed"),
                                    Err(session::SessionError::Io(e)) => log::info!("{remote} disconnected: {e}"),
                                    Err(session::SessionError::Idle) => log::info!("{remote} idle, dropped"),
                                    Err(e) => {
                                        shared.totals.add(&shared.totals.handshake_failures, 1);
                                        log::warn!("{remote} dropped: {e}");
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            log::warn!("accept: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    }
                }
            }
        };
        tokio::select! {
            _ = accept => {}
            _ = shutdown_signal() => {}
        }
        log::info!("shutting down; persisting TIDES window");
        if let Err(e) = shared.ledger.lock().unwrap().persist_window() {
            log::error!("final ledger persist failed: {e}");
        }
        0
    })
}

struct ConnectionSlot {
    shared: Arc<Shared>,
    ip: std::net::IpAddr,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        if let Ok(mut c) = self.shared.connections.lock() {
            c.release(self.ip);
        }
    }
}

/// If the last `stats.json` (written by the previous process) disagrees with the
/// window we just loaded, the file and RAM had drifted — the cliff we hit on 2026-09-05.
fn warn_if_window_cliff(dir: &std::path::Path, ledger: &Ledger) {
    let raw = match std::fs::read(dir.join("stats.json")) {
        Ok(b) => b,
        Err(_) => return,
    };
    let v: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(miners) = v.pointer("/window/miners").and_then(|m| m.as_array()) else {
        return;
    };
    let live: std::collections::HashMap<String, u64> =
        ledger.window.miners().into_iter().map(|m| (m.identity, m.work)).collect();
    let mut cliffs = 0u32;
    for m in miners {
        let Some(id) = m.get("identity").and_then(|x| x.as_str()) else { continue };
        let prev = m.get("work").and_then(|x| x.as_u64()).unwrap_or(0);
        if prev < 1_000_000 {
            continue;
        }
        let now = live.get(id).copied().unwrap_or(0);
        if now + prev / 10 < prev {
            log::error!(
                "TIDES window cliff on reload: {id} work {prev} -> {now} ({}% of prior). \
                 Disk and the previous process disagreed; miners will see a next-block drop.",
                if prev > 0 { 100 * now / prev } else { 0 }
            );
            cliffs += 1;
        }
    }
    if cliffs == 0 {
        log::info!("TIDES window on disk matches the previous process (no reload cliff)");
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("sigterm handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
