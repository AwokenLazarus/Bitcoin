mod config;
mod ledger;
mod rpc;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lazarus_protocol::handshake::{self, HELLO_XOR};
use lazarus_protocol::header::Header;
use lazarus_protocol::keys::{load_or_create_pool_keys, PoolKeys};
use lazarus_protocol::mining::{self, CoinbaserRequest, PowSubmit};
use lazarus_protocol::{identity_of, identity_script};

use crate::config::Config;
use crate::ledger::Ledger;
use crate::rpc::ChainTip;

struct Shared {
    cfg: Config,
    keys: PoolKeys,
    pool_script: Vec<u8>,
    ledger: Mutex<Ledger>,
    tip: Mutex<ChainTip>,
    clients: AtomicUsize,
    coinbaser_id: Mutex<u8>,
    persist_every: AtomicUsize,
    tip_gen: AtomicU64,
}

impl Shared {
    fn ledger_path(&self) -> std::path::PathBuf {
        self.cfg.data_dir.join("ledger.json")
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
                        let (blob, nout) = {
                            let led = st.ledger.lock().unwrap();
                            let cb = led.coinbaser(
                                req.value,
                                st.cfg.fee_bps,
                                st.cfg.min_payout,
                                &st.pool_script,
                                cid,
                            );
                            let n = cb.outputs.len();
                            (cb.encode(), n)
                        };
                        let body = mining::encode_coinbaser_resp(req.value, &blob);
                        let pkt = mining::wrap_mining(&mut ch, &body, None);
                        sock.write_all(&pkt).map_err(|e| e.to_string())?;
                        log::info!("coinbaser id={} value={} outputs={}", cid, req.value, nout);
                    }
                }
                mining::SUB_SHARE => {
                    let (ok, reason, nonce, pot, job) = match PowSubmit::decode(&plain) {
                        None => (false, mining::REJECT_OTHER, 0, 0, 0),
                        Some(s) => {
                            let ident = identity_of(&s.username);
                            if identity_script(&ident).is_none() {
                                log::info!("share reject BadUsername");
                                (false, mining::REJECT_BAD_USERNAME, s.nonce, s.target_byte, s.job_id)
                            } else {
                                let work = s.difficulty().max(st.cfg.min_diff);
                                {
                                    let mut led = st.ledger.lock().unwrap();
                                    led.credit(ident, work, s.nonce, s.job_id);
                                    let tip = st.tip.lock().unwrap();
                                    let target = (tip.difficulty.max(0.0) * st.cfg.window_multiple as f64) as u64;
                                    led.trim(target.max(1));
                                    let n = st.persist_every.fetch_add(1, Ordering::Relaxed) + 1;
                                    if n % 25 == 0 {
                                        led.save(&st.ledger_path());
                                    }
                                }
                                log::info!("share accepted job={} pot={} nonce={:08x}", s.job_id, s.target_byte, s.nonce);
                                (true, 0, s.nonce, s.target_byte, s.job_id)
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
    let miners: Vec<serde_json::Value> = {
        let blob = led.coinbaser(
            6_2500_0000,
            st.cfg.fee_bps,
            st.cfg.min_payout,
            &st.pool_script,
            1,
        );
        let pay: std::collections::HashMap<String, u64> = {
            // payout_sats shown from a nominal 6.25 BTC split so the UI has numbers;
            // actual coinbaser uses the job's subsidy.
            let _ = &blob;
            std::collections::HashMap::new()
        };
        let _ = pay;
        by.iter()
            .map(|(ident, work)| {
                let pct = if win == 0 { 0.0 } else { 100.0 * *work as f64 / win as f64 };
                let payable = identity_script(ident).is_some();
                serde_json::json!({
                    "identity": ident,
                    "work": work,
                    "share_percent": pct,
                    "payout_sats": 0,
                    "payable": payable,
                })
            })
            .collect()
    };
    // Fill payout_sats from a 6.25 BTC example split (UI only).
    let split = led.coinbaser(
        625_000_000,
        st.cfg.fee_bps,
        st.cfg.min_payout,
        &st.pool_script,
        1,
    );
    let shares = led.shares;
    let carry_n = led.carry_len();
    let cb_outs = split.outputs.len();
    drop(led);
    let mut sats: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    // identities aren't on outputs; UI uses payout_sats from window miners — leave 0 if we can't map.
    let _ = split;
    let _ = sats;

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
        "lazarus-prime listen={} advertise={} fee_bps={} tag={} pubkey={}…",
        cfg.listen,
        cfg.advertise,
        cfg.fee_bps,
        cfg.coinbase_tag,
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
        persist_every: AtomicUsize::new(0),
        tip_gen: AtomicU64::new(0),
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
