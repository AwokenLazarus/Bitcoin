use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use lazarus_protocol::coinbaser::{parse_coinbaser_v2, CoinbaserOutput, CoinbaserV2};
use lazarus_protocol::handshake;
use lazarus_protocol::keys::{generate_pool_keys, generate_session};
use lazarus_protocol::mining::{self, CoinbaserRequest, PowSubmit};
use lazarus_protocol::pow::{self, HeaderV2};
use lazarus_protocol::{identity_of, identity_script, Header};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Parser, Debug)]
struct Cli { #[arg(long)] config: PathBuf }

#[derive(Clone, Debug, Deserialize)]
struct GwCfg {
    profile: Option<String>,
    stratum_listen: String,
    api_listen: String,
    vardiff_min: u64,
    rpc: String,
    rpc_cookie: PathBuf,
    prime_host: String,
    prime_port: u16,
    pool_pubkey: String,
    coinbase_tag: Option<String>,
}

struct Miner {
    host: String, user: String, ua: String, vdiff: u64,
    acc: u64, acc_n: u64, rej: u64, rej_n: u64, last: Instant,
}

#[derive(Clone)]
struct Job {
    id: String, header: HeaderV2, prev_notify: String, ntime: String,
    nbits: [u8; 4], value: u64, height: u32, merkle: Vec<[u8; 32]>,
    txn_count: u32, outputs: usize, tx_hexes: Vec<Vec<u8>>,
    cb: CoinbaserV2, witness_commit: Option<Vec<u8>>, tag: String,
}

struct Shared {
    cfg: GwCfg,
    job: Mutex<Option<Job>>,
    miners: Mutex<HashMap<u64, Miner>>,
    next_id: AtomicU64,
    acc: AtomicU64,
    rej: AtomicU64,
    extra1: [u8; 4],
    prime_tx: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    last_cb: Mutex<Option<(u64, CoinbaserV2)>>,
    cb_cv: Condvar,
    miner_socks: Mutex<HashMap<u64, TcpStream>>,
    published_outputs: AtomicUsize,
}
fn cookie_auth(p: &PathBuf) -> Option<String> {
    let raw = std::fs::read_to_string(p).ok()?;
    Some(format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw.trim().as_bytes())))
}
fn rpc(url: &str, auth: &str, method: &str, params: Value) -> Option<Value> {
    let body = json!({"jsonrpc":"1.0","id":"g","method":method,"params":params});
    let r = minreq::post(url).with_header("Authorization", auth).with_header("Content-Type", "application/json").with_body(body.to_string()).send().ok()?;
    let v: Value = serde_json::from_str(r.as_str().ok()?).ok()?;
    v.get("result").cloned()
}
fn hex_rev(h: &str) -> Option<[u8; 32]> {
    let mut v: [u8; 32] = hex::decode(h).ok()?.try_into().ok()?; v.reverse(); Some(v)
}
fn bits_le(h: &str) -> Option<[u8; 4]> {
    let v = hex::decode(h).ok()?; if v.len() != 4 { return None; }
    let mut a = [0u8; 4]; a.copy_from_slice(&v); a.reverse(); Some(a)
}
fn compact(n: u64) -> Vec<u8> {
    if n < 0xfd { vec![n as u8] } else if n <= 0xffff {
        let mut v = vec![0xfd]; v.extend_from_slice(&(n as u16).to_le_bytes()); v
    } else {
        let mut v = vec![0xfe]; v.extend_from_slice(&(n as u32).to_le_bytes()); v
    }
}
fn scriptsig(height: u32, tag: &str, extra: &[u8]) -> Vec<u8> {
    let mut s = Vec::new();
    if height < 0x100 { s.push(1); s.push(height as u8); }
    else if height < 0x10000 { s.push(2); s.extend_from_slice(&(height as u16).to_le_bytes()); }
    else { s.push(3); s.extend_from_slice(&height.to_le_bytes()[..3]); }
    s.push(tag.len() as u8); s.extend_from_slice(tag.as_bytes()); s.extend_from_slice(extra); s
}
fn push_outs(t: &mut Vec<u8>, cb: &CoinbaserV2, wit: Option<&[u8]>) {
    let mut outs = cb.outputs.clone();
    if let Some(w) = wit { outs.push(CoinbaserOutput { sats: 0, script: w.to_vec() }); }
    t.extend_from_slice(&compact(outs.len() as u64));
    for o in &outs {
        t.extend_from_slice(&o.sats.to_le_bytes());
        t.extend_from_slice(&compact(o.script.len() as u64));
        t.extend_from_slice(&o.script);
    }
}
fn coinbase_legacy(height: u32, tag: &str, extra: &[u8], cb: &CoinbaserV2, wit: Option<&[u8]>) -> Vec<u8> {
    let ss = scriptsig(height, tag, extra);
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes()); t.push(1);
    t.extend_from_slice(&[0u8; 32]); t.extend_from_slice(&0xffffffffu32.to_le_bytes());
    t.extend_from_slice(&compact(ss.len() as u64)); t.extend_from_slice(&ss);
    t.extend_from_slice(&0xffffffffu32.to_le_bytes());
    push_outs(&mut t, cb, wit); t.extend_from_slice(&0u32.to_le_bytes()); t
}
fn coinbase_witness(height: u32, tag: &str, extra: &[u8], cb: &CoinbaserV2, wit: Option<&[u8]>) -> Vec<u8> {
    let ss = scriptsig(height, tag, extra);
    let mut t = Vec::new();
    t.extend_from_slice(&1u32.to_le_bytes()); t.push(0); t.push(1); t.push(1);
    t.extend_from_slice(&[0u8; 32]); t.extend_from_slice(&0xffffffffu32.to_le_bytes());
    t.extend_from_slice(&compact(ss.len() as u64)); t.extend_from_slice(&ss);
    t.extend_from_slice(&0xffffffffu32.to_le_bytes());
    push_outs(&mut t, cb, wit);
    // BIP141: witness comes before locktime
    t.push(1); t.push(32); t.extend_from_slice(&[0u8; 32]);
    t.extend_from_slice(&0u32.to_le_bytes()); t
}
fn header_v2_bytes(h: &HeaderV2) -> [u8; 164] {
    let mut b = [0u8; 164];
    b[0..4].copy_from_slice(&(h.version as u32 | pow::V2_FLAG).to_le_bytes());
    b[4..36].copy_from_slice(&h.prev_block); b[36..68].copy_from_slice(&h.merkle_root);
    b[68..72].copy_from_slice(&h.time_on_wire().to_le_bytes());
    b[72..76].copy_from_slice(&h.bits.to_le_bytes()); b[76..80].copy_from_slice(&h.nonce.to_le_bytes());
    b[80..84].copy_from_slice(&h.nonce2.to_le_bytes()); b[84..88].copy_from_slice(&h.nonce3.to_le_bytes());
    b[88..104].copy_from_slice(&h.extranonce); b[104..108].copy_from_slice(&h.time_offset.to_le_bytes());
    b[108..110].copy_from_slice(&h.txcount.to_le_bytes()); b[110] = h.flags; b[111] = h.xor_key_mask_clear_bits;
    b[112..128].copy_from_slice(&h.xor_key); b[128..132].copy_from_slice(&h.height.to_le_bytes());
    b[132..164].copy_from_slice(&h.mm_rhs); b
}
fn notify_line(j: &Job) -> String {
    let h2 = &j.header.coinb1_sia()[3..35];
    format!("{}\n", json!({"id":null,"method":"mining.notify","params":[
        j.id.clone(), j.prev_notify.clone(), format!("000000{}00000000", hex::encode(h2)),
        "", [], "", hex::encode(j.nbits), j.ntime.clone(), true
    ]}))
}
fn broadcast(st: &Shared, line: &str) {
    let mut dead = Vec::new();
    let mut socks = st.miner_socks.lock().unwrap();
    for (mid, s) in socks.iter_mut() { if s.write_all(line.as_bytes()).is_err() { dead.push(*mid); } }
    for d in dead { socks.remove(&d); }
}
fn send_prime(st: &Shared, body: Vec<u8>) {
    if let Some(tx) = st.prime_tx.lock().unwrap().as_ref() { let _ = tx.send(body); }
}
fn wait_coinbaser(st: &Shared, value: u64, deadline: Instant) -> Option<CoinbaserV2> {
    let mut g = st.last_cb.lock().unwrap();
    loop {
        if let Some((v, cb)) = g.as_ref() {
            if *v == value && cb.outputs.len() >= 2 { return Some(cb.clone()); }
        }
        let now = Instant::now();
        if now >= deadline { return None; }
        let (gg, w) = st.cb_cv.wait_timeout(g, deadline.saturating_duration_since(now)).ok()?;
        g = gg;
        if w.timed_out() { return None; }
    }
}
fn build_split_job(tpl: &Value, tag: &str, extra1: &[u8; 4], cb: CoinbaserV2) -> Option<Job> {
    let prev = hex_rev(tpl.get("previousblockhash")?.as_str()?)?;
    let bits = bits_le(tpl.get("bits")?.as_str()?)?;
    let height = tpl.get("height")?.as_u64()? as u32;
    let value = tpl.get("coinbasevalue")?.as_u64()?;
    let curtime = tpl.get("curtime")?.as_u64()? as u32;
    let version = tpl.get("version")?.as_u64()? as i32;
    let txs = tpl.get("transactions")?.as_array()?.clone();
    let mut merkle = Vec::new(); let mut tx_hexes = Vec::new();
    for tx in &txs {
        if let Some(h) = tx.get("txid").or_else(|| tx.get("hash")).and_then(|x| x.as_str()) {
            if let Ok(mut b) = hex::decode(h) {
                if b.len() == 32 { b.reverse(); let mut a = [0u8; 32]; a.copy_from_slice(&b); merkle.push(a); }
            }
        }
        if let Some(d) = tx.get("data").and_then(|x| x.as_str()) {
            if let Ok(raw) = hex::decode(d) { tx_hexes.push(raw); }
        }
    }
    let wit = tpl.get("default_witness_commitment").and_then(|x| x.as_str()).and_then(|h| hex::decode(h).ok());
    let mut extra = extra1.to_vec(); extra.extend_from_slice(&[0u8; 8]);
    let cbleg = coinbase_legacy(height, tag, &extra, &cb, wit.as_deref());
    let mut hdr = HeaderV2::default();
    hdr.version = version; hdr.prev_block = prev; hdr.time = curtime;
    hdr.bits = u32::from_le_bytes(bits); hdr.height = height as i32;
    hdr.txcount = (txs.len() + 1) as u16;
    hdr.merkle_root = pow::merkle_root_sha256d(&cbleg, &merkle);
    hdr.extranonce[..4].copy_from_slice(extra1);
    Some(Job {
        id: format!("{:08x}", curtime ^ height), prev_notify: hex::encode(pow::prevblock_hidden(&prev)),
        ntime: hex::encode([0u8; 8]), nbits: bits, value, height, merkle,
        txn_count: txs.len() as u32 + 1, outputs: cb.outputs.len(), tx_hexes, cb, witness_commit: wit,
        tag: tag.to_string(), header: hdr,
    })
}
fn connect_prime(cfg: &GwCfg) -> Option<(TcpStream, lazarus_protocol::ChannelKeys, lazarus_protocol::SessionKeys)> {
    let pk = hex::decode(cfg.pool_pubkey.trim()).ok()?;
    if pk.len() != 64 { log::error!("pool_pubkey must be 128 hex chars"); return None; }
    let mut pool_x = [0u8; 32]; pool_x.copy_from_slice(&pk[32..64]);
    let local = generate_pool_keys(); let sess = generate_session();
    let (hello, _nk, mut ch) = handshake::encode_client_hello(&local, &sess, &pool_x, "lazarus-gateway/0.1").ok()?;
    let mut sock = TcpStream::connect((cfg.prime_host.as_str(), cfg.prime_port)).ok()?;
    let _ = sock.set_nodelay(true); sock.write_all(&hello).ok()?;
    let mut hdr = [0u8; 4]; sock.read_exact(&mut hdr).ok()?;
    let h = Header::decode_obfuscated(hdr, ch.recv_hdr);
    let mut payload = vec![0u8; h.cmd_len as usize]; sock.read_exact(&mut payload).ok()?;
    let _ = ch.next_recv_hdr();
    let plain = lazarus_protocol::nacl::box_seal_open(&payload, &sess.x_pk, &sess.x_sk).ok()?;
    if plain.len() < 256 { return None; }
    let (body, sig) = plain.split_at(plain.len() - 64);
    let mut pool_ed = [0u8; 32]; pool_ed.copy_from_slice(&pk[..32]);
    lazarus_protocol::nacl::verify_detached(sig.try_into().ok()?, body, &pool_ed).ok()?;
    let mut pool_sess_x = [0u8; 32]; pool_sess_x.copy_from_slice(&body[160..192]);
    ch.set_precomp(&pool_sess_x, &sess.x_sk);
    log::info!("DATUM handshake ok with Prime {}: {}", cfg.prime_host, cfg.prime_port);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(400)));
    let mut hdr2 = [0u8; 4];
    if sock.read_exact(&mut hdr2).is_ok() {
        let peek = Header::decode_obfuscated(hdr2, ch.recv_hdr);
        let mut p2 = vec![0u8; peek.cmd_len as usize];
        if sock.read_exact(&mut p2).is_ok() {
            let _ = mining::open_frame(&mut ch, hdr2, &p2, None);
            log::info!("Prime config received");
        }
    }
    Some((sock, ch, sess))
}
fn prime_loop(st: Arc<Shared>, rx: mpsc::Receiver<Vec<u8>>) {
    loop {
        let Some((mut sock, mut ch, _sess)) = connect_prime(&st.cfg) else {
            log::warn!("Prime connect failed; retry"); thread::sleep(Duration::from_secs(2)); continue;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(body) => { let pkt = mining::wrap_mining(&mut ch, &body, None); if sock.write_all(&pkt).is_err() { break; } }
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let mut hdr = [0u8; 4];
            match sock.read_exact(&mut hdr) {
                Ok(()) => {
                    let peek = Header::decode_obfuscated(hdr, ch.recv_hdr);
                    let mut payload = vec![0u8; peek.cmd_len as usize];
                    if sock.read_exact(&mut payload).is_err() { break; }
                    let Some((_h, plain)) = mining::open_frame(&mut ch, hdr, &payload, None) else { break; };
                    if plain.first() == Some(&mining::SUB_COINBASER_RESP) && plain.len() >= 13 {
                        let value = u64::from_le_bytes(plain[1..9].try_into().unwrap());
                        let n = u32::from_le_bytes(plain[9..13].try_into().unwrap()) as usize;
                        if 13 + n <= plain.len() {
                            if let Some(cb) = parse_coinbaser_v2(&plain[13..13 + n]) {
                                log::info!("coinbaser applied value={} outputs={}", value, cb.outputs.len());
                                *st.last_cb.lock().unwrap() = Some((value, cb));
                                st.cb_cv.notify_all();
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        log::warn!("Prime session ended; reconnect"); thread::sleep(Duration::from_secs(1));
    }
}
fn send_line(s: &mut TcpStream, v: &Value) { let _ = writeln!(s, "{v}"); }
fn html_esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }
fn parse_hr_label(hs: f64) -> String {
    if hs >= 1e12 { format!("{:.2} TH/sec", hs / 1e12) }
    else if hs >= 1e9 { format!("{:.2} GH/sec", hs / 1e9) }
    else { format!("{:.2} MH/sec", hs / 1e6) }
}
fn handle_miner(mut sock: TcpStream, st: Arc<Shared>) {
    let host = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let id = st.next_id.fetch_add(1, Ordering::Relaxed);
    let Some(rdr_sock) = sock.try_clone().ok() else { return };
    let rdr = BufReader::new(rdr_sock);
    let mut user = String::new(); let mut ua = String::new();
    let vmin = st.cfg.vardiff_min.max(1);
    st.miners.lock().unwrap().insert(id, Miner { host, user: String::new(), ua: String::new(), vdiff: vmin, acc: 0, acc_n: 0, rej: 0, rej_n: 0, last: Instant::now() });
    for line in rdr.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(|x| x.as_str()).unwrap_or("");
        let mid = msg.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "mining.subscribe" => {
                if let Some(u) = msg.get("params").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()) { ua = u.to_string(); }
                if let Ok(c) = sock.try_clone() { st.miner_socks.lock().unwrap().insert(id, c); }
                send_line(&mut sock, &json!({"id": mid, "result": [[["mining.notify", "lz"]], hex::encode(st.extra1), 8], "error": null}));
                send_line(&mut sock, &json!({"id": null, "method": "mining.set_difficulty", "params": [vmin]}));
                if let Some(j) = st.job.lock().unwrap().as_ref() {
                    if j.outputs >= 2 { let _ = write!(sock, "{}", notify_line(j)); }
                }
            }
            "mining.authorize" => {
                user = msg.get("params").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let ok = identity_script(&identity_of(&user)).is_some();
                send_line(&mut sock, &json!({"id": mid, "result": ok, "error": if ok { Value::Null } else { json!([14, "BadUsername", null]) }}));
                if let Some(m) = st.miners.lock().unwrap().get_mut(&id) { m.user = user.clone(); m.ua = ua.clone(); }
            }
            "mining.submit" => {
                let params = msg.get("params").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                let job = st.job.lock().unwrap().clone();
                let Some(j) = job else { send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([21, "NoJob", null])})); continue; };
                if j.outputs < 2 || params.len() < 5 {
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([21, "UnsplitJob", null])})); continue;
                }
                let en2 = hex::decode(params[2].as_str().unwrap_or("")).unwrap_or_default();
                let ntime = hex::decode(params[3].as_str().unwrap_or("")).unwrap_or(vec![0; 8]);
                let nonce = hex::decode(params[4].as_str().unwrap_or("")).unwrap_or(vec![0; 8]);
                let mut sia_n = [0u8; 8]; let mut sia_t = [0u8; 8];
                for (i, b) in ntime.iter().take(8).enumerate() { sia_t[i] = *b; }
                for (i, b) in nonce.iter().take(8).enumerate() { sia_n[i] = *b; }
                let mut hdr = j.header.clone();
                hdr.nonce = u32::from_le_bytes(sia_n[0..4].try_into().unwrap_or([0; 4]));
                hdr.nonce2 = u32::from_le_bytes(sia_n.get(4..8).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));
                hdr.extranonce[..4].copy_from_slice(&st.extra1);
                let mut p = [0u8; 8]; for (i, b) in en2.iter().take(8).enumerate() { p[i] = *b; }
                hdr.extranonce[4..12].copy_from_slice(&p);
                let pot = vmin.max(1).ilog2() as u8;
                let hash = hdr.pow_hash_le();
                // Accept on a published split job. Local header reconstruct is used only
                // to attempt submitblock when it meets nBits; intminer share format is Sia-style.
                st.acc.fetch_add(1, Ordering::Relaxed);
                if let Some(m) = st.miners.lock().unwrap().get_mut(&id) { m.acc += vmin; m.acc_n += 1; m.last = Instant::now(); }
                let mut en = Vec::from(st.extra1); en.extend_from_slice(&en2); en.resize(12, 0);
                let submit = PowSubmit {
                    job_id: 0, coinbase_id: j.cb.id, is_block: false, subsidy_only: false, quickdiff: false,
                    target_byte: pot, ntime: j.header.time, nonce: hdr.nonce, version: j.header.version as u32,
                    extranonce: en, username: user.clone(), use_time_offset: false,
                    job: Some(mining::JobSection {
                        prev_hash: j.header.prev_block, target_byte_index: 0, nbits: j.nbits,
                        coinbaser_id: j.cb.id, height: j.height, coinbase_value: j.value,
                        txn_count: j.txn_count, txn_total_weight: 0, txn_total_size: 0, txn_total_sigops: 0,
                        merkle_branches: j.merkle.iter().take(16).cloned().collect(),
                    }),
                    coinbase: None,
                    blake2b: Some(mining::Blake2bSection { sia_ntime: sia_t, sia_nonce: sia_n, time_on_wire: j.header.time }),
                };
                send_prime(&st, submit.encode());
                if let Some(tgt) = pow::bits_to_target(j.header.bits) {
                    let hit = pow::meets_target(&hash, &tgt);
                    if hit {
                        log::info!("share meets nbits height={} hash_hi={:02x}{:02x}{:02x}{:02x}", j.height, hash[31], hash[30], hash[29], hash[28]);
                        maybe_submit_block(&st, &j, &hdr);
                    }
                } else {
                    log::warn!("bits_to_target failed bits={:08x}", j.header.bits);
                }
                send_line(&mut sock, &json!({"id": mid, "result": true, "error": null}));
            }
            _ => send_line(&mut sock, &json!({"id": mid, "result": null, "error": null})),
        }
    }
    st.miners.lock().unwrap().remove(&id);
    st.miner_socks.lock().unwrap().remove(&id);
}
fn maybe_submit_block(st: &Shared, j: &Job, hdr: &HeaderV2) {
    let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else { return };
    let mut extra = st.extra1.to_vec(); extra.extend_from_slice(&[0u8; 8]);
    let cbw = coinbase_witness(j.height, &j.tag, &extra, &j.cb, j.witness_commit.as_deref());
    let mut blk = header_v2_bytes(hdr).to_vec();
    blk.extend_from_slice(&compact(1 + j.tx_hexes.len() as u64));
    blk.extend_from_slice(&cbw);
    for tx in &j.tx_hexes { blk.extend_from_slice(tx); }
    log::info!("submitblock height={} outputs={} bytes={}", j.height, j.outputs, blk.len());
    let body = json!({"jsonrpc":"1.0","id":"sb","method":"submitblock","params":[hex::encode(&blk)]});
    match minreq::post(&st.cfg.rpc).with_header("Authorization", &auth).with_header("Content-Type", "application/json").with_body(body.to_string()).send() {
        Ok(r) => {
            let txt = r.as_str().unwrap_or("");
            let v: Value = serde_json::from_str(txt).unwrap_or(Value::Null);
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                log::warn!("submitblock rpc error: {err}");
            } else {
                match v.get("result") {
                    Some(Value::Null) | None => log::info!("submitblock accepted height={}", j.height),
                    Some(x) if x.as_str() == Some("inconclusive") => log::info!("submitblock inconclusive height={}", j.height),
                    Some(x) => log::warn!("submitblock result: {x}"),
                }
            }
        }
        Err(e) => log::warn!("submitblock http {e}"),
    }
}
fn api_loop(st: Arc<Shared>) {
    let Ok(lis) = TcpListener::bind(&st.cfg.api_listen) else { log::error!("api bind {}", st.cfg.api_listen); return; };
    log::info!("api {}", st.cfg.api_listen);
    for s in lis.incoming() {
        let Ok(mut s) = s else { continue };
        let mut buf = [0u8; 512]; let n = s.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/");
        let body = if path.starts_with("/clients") { clients_html(&st) } else { home_html(&st) };
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    }
}
fn home_html(st: &Shared) -> String {
    let acc = st.acc.load(Ordering::Relaxed); let rej = st.rej.load(Ordering::Relaxed);
    let outs = st.published_outputs.load(Ordering::Relaxed);
    let hr: f64 = st.miners.lock().unwrap().values().map(|m| {
        let dt = m.last.elapsed().as_secs_f64().max(1.0);
        (m.acc as f64) * (1u64 << 32) as f64 / dt.max(30.0)
    }).sum();
    format!("<html><body>Estimated Hashrate: {}<br>Local Shares Accepted: {}<br>Local Shares Rejected: {}<br>Coinbase outputs: {}</body></html>", parse_hr_label(hr), acc, rej, outs)
}
fn clients_html(st: &Shared) -> String {
    let mut rows = String::from("<TABLE><TR><TD>#</TD><TD>Host</TD><TD>Auth Username</TD><TD></TD><TD>Last</TD><TD>VDiff</TD><TD>A</TD><TD>R</TD><TD>HR</TD><TD></TD><TD>UA</TD></TR>");
    for (i, m) in st.miners.lock().unwrap().values().enumerate() {
        rows.push_str(&format!("<TR><TD>{}</TD><TD>{}</TD><TD>{}</TD><TD></TD><TD>{:.1} s</TD><TD>{}</TD><TD>{} ({})</TD><TD>{} ({})</TD><TD>{}</TD><TD></TD><TD>{}</TD></TR>",
            i, html_esc(&m.host), html_esc(&m.user), m.last.elapsed().as_secs_f64(), m.vdiff, m.acc, m.acc_n, m.rej, m.rej_n,
            parse_hr_label((m.acc as f64) * (1u64 << 32) as f64 / 30.0), html_esc(&m.ua)));
    }
    rows.push_str("</TABLE>"); format!("<html><body>{rows}</body></html>")
}
fn gbt_loop(st: Arc<Shared>) {
    let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else { log::error!("missing rpc cookie"); return; };
    let tag = st.cfg.coinbase_tag.clone().unwrap_or_else(|| "Lazarus".into());
    loop {
        if let Some(tpl) = rpc(&st.cfg.rpc, &auth, "getblocktemplate", json!([{"rules": ["segwit", "blake2b"]}])) {
            let value = tpl.get("coinbasevalue").and_then(|x| x.as_u64()).unwrap_or(0);
            let prev = hex_rev(tpl.get("previousblockhash").and_then(|x| x.as_str()).unwrap_or("")).unwrap_or([0u8; 32]);
            if value > 0 {
                send_prime(&st, CoinbaserRequest { value, prevhash: prev }.encode());
                if let Some(cb) = wait_coinbaser(&st, value, Instant::now() + Duration::from_secs(3)) {
                    if let Some(j) = build_split_job(&tpl, &tag, &st.extra1, cb) {
                        log::info!("published job height={} txs~{} outputs={}", j.height, j.txn_count, j.outputs);
                        st.published_outputs.store(j.outputs, Ordering::Relaxed);
                        let line = notify_line(&j);
                        *st.job.lock().unwrap() = Some(j);
                        broadcast(&st, &line);
                    }
                } else {
                    log::warn!("no split coinbaser for value={}; not publishing unsplit job", value);
                }
            }
        }
        thread::sleep(Duration::from_secs(8));
    }
}
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let raw = std::fs::read_to_string(&cli.config).expect("config");
    let cfg: GwCfg = serde_json::from_str(&raw).expect("json config");
    let extra1: [u8; 4] = rand::random();
    log::info!("lazarus-gateway profile={} stratum={} api={} vardiff_min={}", cfg.profile.as_deref().unwrap_or("asic"), cfg.stratum_listen, cfg.api_listen, cfg.vardiff_min);
    let (tx, rx) = mpsc::channel();
    let st = Arc::new(Shared {
        cfg: cfg.clone(), job: Mutex::new(None), miners: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1), acc: AtomicU64::new(0), rej: AtomicU64::new(0), extra1,
        prime_tx: Mutex::new(Some(tx)), last_cb: Mutex::new(None), cb_cv: Condvar::new(),
        miner_socks: Mutex::new(HashMap::new()), published_outputs: AtomicUsize::new(0),
    });
    { let s = st.clone(); thread::spawn(move || prime_loop(s, rx)); }
    { let s = st.clone(); thread::spawn(move || api_loop(s)); }
    { let s = st.clone(); thread::spawn(move || gbt_loop(s)); }
    let lis = TcpListener::bind(&st.cfg.stratum_listen).expect("stratum bind");
    log::info!("stratum {}", st.cfg.stratum_listen);
    for inc in lis.incoming() {
        if let Ok(s) = inc { let st = st.clone(); thread::spawn(move || handle_miner(s, st)); }
    }
}
