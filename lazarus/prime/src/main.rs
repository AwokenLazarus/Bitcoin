mod config;
mod ledger;
mod rpc;

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lazarus_protocol::handshake::{self, HELLO_XOR};
use lazarus_protocol::header::Header;
use lazarus_protocol::keys::{load_or_create_pool_keys, PoolKeys};
use lazarus_protocol::coinbaser::CoinbaserV2;
use lazarus_protocol::mining::{self, CoinbaserRequest, PowSubmit};
use lazarus_protocol::verify::{self, ShareContext};
use lazarus_protocol::{identity_of, identity_script};

use crate::config::Config;
use crate::ledger::Ledger;
use crate::rpc::ChainTip;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VerifyMode {
    Off,
    Log,
    Enforce,
}

fn verify_mode(s: &str) -> VerifyMode {
    match s {
        "off" => VerifyMode::Off,
        "enforce" => VerifyMode::Enforce,
        _ => VerifyMode::Log,
    }
}

/// Identity of a submission: repeat keys are the same work claimed twice.
fn share_key(s: &PowSubmit) -> [u8; 16] {
    let mut d = Vec::with_capacity(64);
    d.push(s.job_id);
    d.push(s.coinbase_id);
    d.extend_from_slice(&s.nonce.to_le_bytes());
    d.extend_from_slice(&s.ntime.to_le_bytes());
    d.extend_from_slice(&s.extranonce);
    if let Some(b) = &s.blake2b {
        d.extend_from_slice(&b.sia_nonce);
        d.extend_from_slice(&b.sia_ntime);
    }
    let h = lazarus_protocol::pow::sha256d(&d);
    let mut k = [0u8; 16];
    k.copy_from_slice(&h[..16]);
    k
}

struct Shared {
    cfg: Config,
    keys: PoolKeys,
    pool_script: Vec<u8>,
    ledger: Mutex<Ledger>,
    tip: Mutex<ChainTip>,
    clients: AtomicUsize,
    coinbaser_id: Mutex<u8>,
    tip_gen: AtomicU64,
    /// Splits we handed out, by coinbaser id, so a share's coinbase can be checked
    /// against what we asked for. Ids wrap at 255, which bounds this map.
    issued: Mutex<HashMap<u8, CoinbaserV2>>,
    verify: VerifyMode,
    verified: AtomicU64,
    rejected: AtomicU64,
    dupes: AtomicU64,
    fail_counts: Mutex<HashMap<&'static str, u64>>,
}

impl Shared {
    fn ledger_path(&self) -> std::path::PathBuf {
        self.cfg.data_dir.join("ledger.json")
    }
    fn bump_fail(&self, name: &'static str) {
        *self.fail_counts.lock().unwrap().entry(name).or_insert(0) += 1;
    }
    fn save_ledger(&self) {
        self.ledger.lock().unwrap().save(&self.ledger_path());
    }
}

fn read_exact(s: &mut TcpStream, n: usize) -> std::io::Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b)?;
    Ok(b)
}

fn handle(mut sock: TcpStream, st: Arc<Shared>) {
    let _ = sock.set_nodelay(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(800)));
    let peer = sock.peer_addr().ok();
    log::info!("gateway connected from {:?}", peer);
    st.clients.fetch_add(1, Ordering::Relaxed);

    let run = (|| -> Result<(), String> {
        let hdr = read_exact(&mut sock, 4).map_err(|e| e.to_string())?;
        let h = Header::decode_obfuscated(hdr.try_into().unwrap(), HELLO_XOR);
        if h.proto_cmd != 1 || !h.is_encrypted_pubkey {
            return Err("not a hello".into());
        }
        let body = read_exact(&mut sock, h.cmd_len as usize).map_err(|e| e.to_string())?;
        let hello = handshake::open_hello(&st.keys, &body).map_err(|e| e.to_string())?;
        log::info!("hello ua={} nk={:08x}", hello.version, hello.nk);

        let sess = handshake::new_session().map_err(|e| e.to_string())?;
        let mut ch = handshake::prime_channel_after_hello(&hello, &sess);
        let send_key = ch.next_send_hdr();
        let resp = handshake::encode_handshake_response(
            &st.keys,
            &hello,
            &sess,
            &st.cfg.motd,
            send_key,
        )
        .map_err(|e| e.to_string())?;
        sock.write_all(&resp).map_err(|e| e.to_string())?;
        log::info!("handshake response sent to {:?}", peer);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let cfg_body = mining::encode_config(
            &st.pool_script,
            st.cfg.prime_id,
            &st.cfg.coinbase_tag,
            st.cfg.min_diff,
        );
        let pkt = mining::wrap_mining(&mut ch, &cfg_body, Some(&sess));
        sock.write_all(&pkt).map_err(|e| e.to_string())?;
        log::info!("config sent to {:?}", peer);

        let mut sess_jobs: HashMap<u8, mining::JobSection> = HashMap::new();
        let mut sess_cbs: HashMap<u8, mining::CoinbaseSection> = HashMap::new();
        let mut sess_seen: HashSet<[u8; 16]> = HashSet::new();
        let mut last_gen = st.tip_gen.load(Ordering::Relaxed);
        loop {
            let gen = st.tip_gen.load(Ordering::Relaxed);
            if gen != last_gen {
                last_gen = gen;
                let pkt = mining::wrap_mining(&mut ch, &mining::encode_blocknotify(), None);
                if sock.write_all(&pkt).is_err() { break; }
                log::info!("blocknotify sent to {:?}", peer);
            }
            let raw = match read_exact(&mut sock, 4) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(_) => break,
            };
            let peek = Header::decode_obfuscated(raw.clone().try_into().unwrap(), ch.recv_hdr);
            let payload = match read_exact(&mut sock, peek.cmd_len as usize) {
                Ok(b) => b,
                Err(_) => break,
            };
            let Some((_h, plain)) =
                mining::open_frame(&mut ch, raw.try_into().unwrap(), &payload, None)
            else {
                log::warn!("drop undecryptable frame from {:?}", peer);
                break;
            };
            if plain.is_empty() {
                continue;
            }
            match plain[0] {
                mining::SUB_COINBASER_REQ => {
                    if let Some(req) = CoinbaserRequest::decode(&plain[1..]) {
                        let mut id = st.coinbaser_id.lock().unwrap();
                        *id = id.wrapping_add(1);
                        if *id == 0 {
                            *id = 1;
                        }
                        let cid = *id;
                        drop(id);
                        let cb = {
                            let led = st.ledger.lock().unwrap();
                            led.coinbaser(
                                req.value,
                                st.cfg.fee_bps,
                                st.cfg.min_payout,
                                &st.pool_script,
                                cid,
                            )
                        };
                        let nout = cb.outputs.len();
                        let blob = cb.encode();
                        st.issued.lock().unwrap().insert(cid, cb);
                        let body = mining::encode_coinbaser_resp(req.value, &blob);
                        let pkt = mining::wrap_mining(&mut ch, &body, None);
                        sock.write_all(&pkt).map_err(|e| e.to_string())?;
                        log::info!("coinbaser id={} value={} outputs={}", cid, req.value, nout);
                    }
                }
                mining::SUB_SHARE => {
                    let (ok, reason, nonce, pot, job) = match PowSubmit::decode(&plain) {
                        None => {
                            st.bump_fail("Undecodable");
                            (false, mining::REJECT_OTHER, 0, 0, 0)
                        }
                        Some(mut s) => {
                            // Gateways send the job and coinbase sections with every share.
                            // Cache them per connection so a peer that sends them once, or
                            // reuses a job across shares, still verifies.
                            if let Some(j) = &s.job {
                                sess_jobs.insert(s.job_id, j.clone());
                            }
                            if let Some(c) = &s.coinbase {
                                sess_cbs.insert(c.coinbase_id, c.clone());
                            }
                            if s.job.is_none() {
                                s.job = sess_jobs.get(&s.job_id).cloned();
                            }
                            if s.coinbase.is_none() {
                                s.coinbase = sess_cbs.get(&s.coinbase_id).cloned();
                            }
                            let ident = identity_of(&s.username);
                            if identity_script(&ident).is_none() {
                                st.bump_fail("BadUsername");
                                (false, mining::REJECT_BAD_USERNAME, s.nonce, s.target_byte, s.job_id)
                            } else {
                                let key = share_key(&s);
                                if !sess_seen.insert(key) {
                                    st.dupes.fetch_add(1, Ordering::Relaxed);
                                    (false, mining::REJECT_DUPLICATE, s.nonce, s.target_byte, s.job_id)
                                } else {
                                    if sess_seen.len() > 200_000 {
                                        sess_seen.clear();
                                    }
                                    // Read the tip before touching the ledger: stats takes the
                                    // same two locks in the opposite order.
                                    let (tip_height, window_target) = {
                                        let tip = st.tip.lock().unwrap();
                                        (
                                            tip.height,
                                            (tip.difficulty.max(0.0) * st.cfg.window_multiple as f64) as u64,
                                        )
                                    };
                                    let verdict = if st.verify == VerifyMode::Off {
                                        None
                                    } else {
                                        let issued =
                                            st.issued.lock().unwrap().get(&s.coinbase_id).cloned();
                                        Some(verify::verify_share(
                                            &s,
                                            &ShareContext {
                                                issued: issued.as_ref(),
                                                tip_height,
                                                now: Ledger::now(),
                                                min_diff: st.cfg.min_diff,
                                            },
                                        ))
                                    };
                                    match &verdict {
                                        Some(Ok(v)) => {
                                            st.verified.fetch_add(1, Ordering::Relaxed);
                                            if v.is_block_candidate {
                                                log::warn!(
                                                    "share meets the network target: height={} miner={}; a block should follow",
                                                    v.height,
                                                    ident
                                                );
                                            }
                                        }
                                        Some(Err(code)) => {
                                            st.bump_fail(verify::reject_name(*code));
                                            let n = st.rejected.fetch_add(1, Ordering::Relaxed);
                                            if n < 20 || n % 500 == 0 {
                                                log::warn!(
                                                    "share failed verification: {} job={} cb={} pot={} miner={}",
                                                    verify::reject_name(*code),
                                                    s.job_id,
                                                    s.coinbase_id,
                                                    s.target_byte,
                                                    ident
                                                );
                                            }
                                        }
                                        None => {}
                                    }
                                    let refused = match &verdict {
                                        Some(Err(code)) if st.verify == VerifyMode::Enforce => Some(*code),
                                        _ => None,
                                    };
                                    if let Some(code) = refused {
                                        (false, code, s.nonce, s.target_byte, s.job_id)
                                    } else {
                                        let work = match &verdict {
                                            Some(Ok(v)) => v.work,
                                            _ => s.difficulty().max(st.cfg.min_diff),
                                        };
                                        let mut led = st.ledger.lock().unwrap();
                                        led.credit(ident, work, key);
                                        led.trim(window_target.max(1));
                                        led.save_if_due(&st.ledger_path(), Duration::from_secs(30));
                                        (true, 0, s.nonce, s.target_byte, s.job_id)
                                    }
                                }
                            }
                        }
                    };
                    let body = mining::encode_share_response(ok, reason, nonce, pot, job);
                    let pkt = mining::wrap_mining(&mut ch, &body, None);
                    sock.write_all(&pkt).map_err(|e| e.to_string())?;
                }
                _ => {}
            }
        }
        Ok(())
    })();
    if let Err(e) = run {
        log::warn!("session {:?}: {e}", peer);
    }
    st.clients.fetch_sub(1, Ordering::Relaxed);
    log::info!("gateway disconnected {:?}", peer);
}

fn stats_loop(st: Arc<Shared>) {
    let lis = match TcpListener::bind(&st.cfg.stats_listen) {
        Ok(l) => l,
        Err(e) => {
            log::error!("stats bind {}: {e}", st.cfg.stats_listen);
            return;
        }
    };
    log::info!("stats on {}", st.cfg.stats_listen);
    for s in lis.incoming() {
        let Ok(mut s) = s else { continue };
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let mut buf = [0u8; 256];
        let _ = s.read(&mut buf);
        let body = stats_json(&st);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = s.write_all(resp.as_bytes());
    }
}

fn stats_json(st: &Shared) -> String {
    let tip = st.tip.lock().unwrap().clone();
    let led = st.ledger.lock().unwrap();
    let by = led.by_identity();
    let win = led.window_work();
    let target = (tip.difficulty.max(0.0) * st.cfg.window_multiple as f64) as u64;
    // payout_sats is a preview: what each identity would take from a nominal 6.25 BTC
    // block at the current window. The live coinbaser uses the template's own subsidy.
    let preview = led.coinbaser(
        625_000_000,
        st.cfg.fee_bps,
        st.cfg.min_payout,
        &st.pool_script,
        1,
    );
    let mut preview_sats: HashMap<Vec<u8>, u64> = HashMap::new();
    for o in &preview.outputs {
        *preview_sats.entry(o.script.clone()).or_insert(0) += o.sats;
    }
    let miners: Vec<serde_json::Value> = by
        .iter()
        .map(|(ident, work)| {
            let pct = if win == 0 { 0.0 } else { 100.0 * *work as f64 / win as f64 };
            let script = identity_script(ident);
            let payout = script
                .as_ref()
                .and_then(|s| preview_sats.get(s))
                .copied()
                .unwrap_or(0);
            serde_json::json!({
                "identity": ident,
                "work": work,
                "share_percent": pct,
                "payout_sats": payout,
                "payable": script.is_some(),
            })
        })
        .collect();
    let shares = led.shares;
    let carry_n = led.carry_len();
    let rows = led.credits_len();
    let cb_outs = preview.outputs.len();
    drop(led);

    serde_json::json!({
        "pool": {
            "pubkey": st.keys.pubkey_hex(),
            "listen": st.cfg.listen,
            "advertise": st.cfg.advertise,
            "motd": st.cfg.motd,
            "tag": st.cfg.coinbase_tag,
            "headline": st.cfg.headline,
            "fee_bps": st.cfg.fee_bps,
            "min_payout": st.cfg.min_payout,
            "min_diff": st.cfg.min_diff,
            "window_multiple": st.cfg.window_multiple,
            "payout_script": hex::encode(&st.pool_script),
        },
        "node": {
            "chain": tip.chain,
            "tip_height": tip.height,
            "tip_hash": tip.hash,
            "difficulty": tip.difficulty,
        },
        "window": {
            "work": win,
            "target_work": target,
            "shares": shares,
            "miners": miners,
            "carry_identities": carry_n,
            "coinbaser_outputs": cb_outs,
            "ledger_rows": rows,
        },
        "verify": {
            "mode": format!("{:?}", st.verify),
            "verified": st.verified.load(Ordering::Relaxed),
            "failed": st.rejected.load(Ordering::Relaxed),
            "duplicates": st.dupes.load(Ordering::Relaxed),
            "reasons": st.fail_counts.lock().unwrap().clone(),
        },
        "clients": st.clients.load(Ordering::Relaxed),
    })
    .to_string()
}

fn rpc_loop(st: Arc<Shared>) {
    let Some(auth) = rpc::cookie_basic(&st.cfg.rpc_cookie) else {
        log::warn!("no rpc cookie; stats height will stay 0");
        return;
    };
    let mut last_hash = String::new();
    loop {
        if let Some(t) = rpc::tip(&st.cfg.rpc, &auth) {
            let new_block = t.hash != last_hash && !last_hash.is_empty();
            last_hash = t.hash.clone();
            *st.tip.lock().unwrap() = t;
            if new_block {
                st.tip_gen.fetch_add(1, Ordering::Relaxed);
                log::info!("new tip {}", last_hash);
                if let Some(info) = rpc::coinbase_info(&st.cfg.rpc, &auth, &last_hash, &st.cfg.coinbase_tag) {
                    if info.is_ours && info.value_outputs >= 2 {
                        let mut led = st.ledger.lock().unwrap();
                        led.clear_carry();
                        led.save(&st.ledger_path());
                        log::info!("split coinbase confirmed ({} value outputs); unpaid carry cleared", info.value_outputs);
                    } else if info.is_ours {
                        log::warn!(
                            "our block used unsplit template ({} value output); keeping contributor carry",
                            info.value_outputs
                        );
                        st.save_ledger();
                    }
                }
            }
        }
        thread::sleep(Duration::from_secs_f64(st.cfg.poll_secs.max(0.2)));
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cfg = Config::load();
    let keys = load_or_create_pool_keys(cfg.key_path()).expect("pool keys");
    let pool_script = identity_script(&cfg.payout_address).unwrap_or_else(|| {
        log::error!("payout-address is not a valid Bitcoin address");
        std::process::exit(1);
    });
    log::info!(
        "lazarus-prime listen={} advertise={} fee_bps={} tag={} verify={} pubkey={}…",
        cfg.listen,
        cfg.advertise,
        cfg.fee_bps,
        cfg.coinbase_tag,
        cfg.verify_shares,
        &keys.pubkey_hex()[..16]
    );
    let mut ledger = Ledger::load(&cfg.data_dir.join("ledger.json"));
    let carry_path = cfg.data_dir.join("unpaid-carry.json");
    let n = ledger.load_carry_file(&carry_path);
    if n > 0 {
        log::info!("merged unpaid carry identities={}", n);
        ledger.save(&cfg.data_dir.join("ledger.json"));
        let loaded = cfg.data_dir.join("unpaid-carry.loaded");
        let _ = std::fs::rename(&carry_path, loaded);
    }
    let st = Arc::new(Shared {
        cfg: cfg.clone(),
        keys,
        pool_script,
        ledger: Mutex::new(ledger),
        tip: Mutex::new(ChainTip::default()),
        clients: AtomicUsize::new(0),
        coinbaser_id: Mutex::new(1),
        tip_gen: AtomicU64::new(0),
        issued: Mutex::new(HashMap::new()),
        verify: verify_mode(&cfg.verify_shares),
        verified: AtomicU64::new(0),
        rejected: AtomicU64::new(0),
        dupes: AtomicU64::new(0),
        fail_counts: Mutex::new(HashMap::new()),
    });
    {
        let s = st.clone();
        thread::spawn(move || stats_loop(s));
    }
    {
        let s = st.clone();
        thread::spawn(move || rpc_loop(s));
    }
    let lis = TcpListener::bind(&cfg.listen).expect("bind prime");
    log::info!("DATUM Prime on {}", cfg.listen);
    for inc in lis.incoming() {
        match inc {
            Ok(s) => {
                let st = st.clone();
                thread::spawn(move || handle(s, st));
            }
            Err(e) => log::warn!("accept: {e}"),
        }
    }
}
