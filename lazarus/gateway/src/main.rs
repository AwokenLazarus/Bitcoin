use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use lazarus_protocol::cbtx;
use lazarus_protocol::coinbaser::{parse_coinbaser_v2, CoinbaserV2};
use lazarus_protocol::handshake;
use lazarus_protocol::keys::{generate_pool_keys, generate_session};
use lazarus_protocol::mining::{self, CoinbaserRequest, PowSubmit, SUB_BLOCKNOTIFY};
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
    /// off | log | enforce: whether a share that misses its vardiff target is rejected
    /// or only counted.
    verify_shares: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VerifyMode {
    Off,
    Log,
    Enforce,
}

fn verify_mode(s: Option<&str>) -> VerifyMode {
    match s.unwrap_or("log") {
        "off" => VerifyMode::Off,
        "enforce" => VerifyMode::Enforce,
        _ => VerifyMode::Log,
    }
}

struct Miner {
    host: String, user: String, ua: String, vdiff: u64,
    acc: u64, acc_n: u64, rej: u64, rej_n: u64, last: Instant,
    /// The difficulty in force before the last retarget, honoured for a grace window so
    /// a share already in flight is not rejected for meeting the target it was given.
    vdiff_prev: u64,
    vdiff_prev_until: Instant,
    last_retarget: Instant,
    /// (job id, difficulty in force when that job was sent to this miner).
    ///
    /// A share has to be judged and paid at the difficulty the miner was working under,
    /// which is the one attached to its job, not whatever the session has been retargeted
    /// to since. Crediting the difficulty a hash happens to reach instead overpays by a
    /// factor of (1 + log2(assigned/actual)/2) whenever the two drift apart.
    job_diffs: VecDeque<(String, u64)>,
    /// (when, work) for a rolling hashrate window. Lifetime acc stays in `acc`.
    recent: VecDeque<(Instant, u64)>,
}
const HR_WINDOW: Duration = Duration::from_secs(60);
/// Aim for roughly one share per miner every few seconds. Left at difficulty 1 a single
/// 1 TH/s rig submits over 200 shares a second, which no pool can account for and which
/// buys nothing: the same hashrate is measured just as well from far fewer shares.
const VARDIFF_TARGET_SECS: f64 = 4.0;
const VARDIFF_INTERVAL: Duration = Duration::from_secs(20);
const VARDIFF_GRACE: Duration = Duration::from_secs(30);
/// Job difficulties remembered per session. Matches the gateway job history, so any job a
/// miner can still name has its difficulty on record.
const JOB_DIFFS: usize = JOB_HISTORY;
/// Record the difficulty this job went out at, so its shares are paid at that rate.
fn assign_job(m: &mut Miner, job_id: &str) {
    m.job_diffs.push_back((job_id.to_string(), m.vdiff));
    while m.job_diffs.len() > JOB_DIFFS {
        m.job_diffs.pop_front();
    }
}
/// Difficulty is a power of two because a share target is issued as an exponent.
fn vardiff_for(hs: f64, floor: u64) -> u64 {
    if !hs.is_finite() || hs <= 0.0 {
        return floor;
    }
    let ideal = hs * VARDIFF_TARGET_SECS / 4_294_967_296.0;
    let pot = ideal.max(1.0).log2().round().clamp(0.0, 40.0) as u32;
    (1u64 << pot).max(floor)
}
fn miner_hs(m: &Miner) -> f64 {
    let now = Instant::now();
    let cutoff = now.checked_sub(HR_WINDOW).unwrap_or(now);
    let mut work = 0u64;
    let mut first: Option<Instant> = None;
    for (ts, w) in &m.recent {
        if *ts >= cutoff {
            if first.is_none() { first = Some(*ts); }
            work = work.saturating_add(*w);
        }
    }
    let dt = first.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0).max(5.0);
    (work as f64) * ((1u64 << 32) as f64) / dt
}
fn record_share(m: &mut Miner, work: u64) {
    let now = Instant::now();
    m.acc = m.acc.saturating_add(work);
    m.acc_n += 1;
    m.last = now;
    m.recent.push_back((now, work));
    let cutoff = now.checked_sub(HR_WINDOW).unwrap_or(now);
    while m.recent.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
        m.recent.pop_front();
    }
}

#[derive(Clone)]
struct Job {
    id: String, header: HeaderV2, prev_notify: String, ntime: String,
    nbits: [u8; 4], value: u64, height: u32,
    /// Sibling path for the coinbase, so Prime can recheck the merkle root.
    branches: Vec<[u8; 32]>,
    /// Legacy coinbase bytes, sent with every share so Prime can audit the split.
    coinb1: Vec<u8>,
    job_id: u8,
    txn_count: u32, outputs: usize, tx_hexes: Vec<Vec<u8>>,
    cb: CoinbaserV2, witness_commit: Option<Vec<u8>>, tag: String,
}

struct Shared {
    cfg: GwCfg,
    job: Mutex<Option<Job>>,
    /// Recently published jobs, newest last. A miner submits against the job it was
    /// handed, which is often no longer the current one.
    jobs: Mutex<VecDeque<Job>>,
    miners: Mutex<HashMap<u64, Miner>>,
    next_id: AtomicU64,
    acc: AtomicU64,
    rej: AtomicU64,
    extra1: [u8; 4],
    prime_tx: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    last_cb: Mutex<Option<(u64, CoinbaserV2)>>,
    cb_cv: Condvar,
    gbt_kick: Condvar,
    gbt_due: Mutex<bool>,
    prime_urgent: Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    miner_socks: Mutex<HashMap<u64, TcpStream>>,
    published_outputs: AtomicUsize,
    published_height: AtomicU64,
    last_share_hdr: Mutex<Option<HeaderV2>>,
    last_share_job: Mutex<Option<String>>,
    job_seq: AtomicU64,
    /// Shares whose rebuilt header met the vardiff target, and those that did not.
    pow_ok: AtomicU64,
    pow_bad: AtomicU64,
    /// Valid shares that landed under the difficulty in force, normally because their
    /// target was raised while they were in flight. Credited for what they proved.
    verify: VerifyMode,
    /// Shares queued for Prime but not yet written, and shares dropped because the
    /// queue was full. Unbounded queueing here once grew to gigabytes of RSS when
    /// Prime could not keep up.
    prime_depth: AtomicU64,
    prime_dropped: AtomicU64,
    /// Submissions naming a job we no longer hold. Should stay near zero; a rising
    /// count means the history is too short or job ids are not unique.
    job_miss: AtomicU64,
    /// Unix seconds when we last published a template. A gateway that cannot reach the
    /// node still looks busy from the outside, because shares keep flowing against the
    /// stale job, so the age is surfaced in the audit and warned about in the log.
    last_pub_unix: AtomicU64,
}

/// Shares allowed to wait for Prime. Roughly a few seconds of a fast miner.
const PRIME_QUEUE_CAP: u64 = 20_000;
/// Jobs kept for submission lookup. A share naming a job we no longer hold cannot be
/// rebuilt, so it has to be turned away; the history therefore has to outlive the
/// slowest miner's round trip, across block boundaries. Each entry carries the block's
/// transactions, so this is also the memory bound: roughly 300 KB apiece.
const JOB_HISTORY: usize = 96;
/// How stale a template may get before it is rebuilt to pick up new transactions.
/// Republishing every couple of seconds churned the job history faster than miners
/// could answer it, and restarted their work for no gain.
const JOB_REFRESH: Duration = Duration::from_secs(20);
/// Template age that means something is wrong rather than merely quiet.
const STALE_TEMPLATE_SECS: u64 = 90;
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn cookie_auth(p: &PathBuf) -> Option<String> {
    let raw = std::fs::read_to_string(p).ok()?;
    Some(format!("Basic {}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw.trim().as_bytes())))
}
/// How long to wait on a node RPC before giving up and retrying.
///
/// Without this the call blocks forever if the node stops answering, which freezes the
/// template loop: the gateway keeps serving the last job it built, miners keep hashing it,
/// and every share is spent on a template the chain has already moved past. Observed in
/// production with the height, coinbase value and transaction count all frozen while
/// shares kept arriving. Failing fast and retrying is always better than hanging.
const RPC_TIMEOUT_SECS: u64 = 10;
fn rpc(url: &str, auth: &str, method: &str, params: Value) -> Option<Value> {
    let body = json!({"jsonrpc":"1.0","id":"g","method":method,"params":params});
    let r = minreq::post(url).with_timeout(RPC_TIMEOUT_SECS).with_header("Authorization", auth).with_header("Content-Type", "application/json").with_body(body.to_string()).send().ok()?;
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
/// The 52-byte blake2b preimage (`hash1`) under a candidate field layout, then the ASIC
fn notify_line(j: &Job) -> String {
    let h2 = &j.header.coinb1_sia()[3..35];
    format!("{}\n", json!({"id":null,"method":"mining.notify","params":[
        j.id.clone(), j.prev_notify.clone(), format!("000000{}00000000", hex::encode(h2)),
        "", [], "", hex::encode(j.nbits), j.ntime.clone(), true
    ]}))
}
fn broadcast(st: &Shared, line: &str, job_id: &str) {
    let mut dead = Vec::new();
    // miners before miner_socks, everywhere, or these two deadlock.
    let mut miners = st.miners.lock().unwrap();
    let mut socks = st.miner_socks.lock().unwrap();
    for (mid, s) in socks.iter_mut() {
        if s.write_all(line.as_bytes()).is_err() {
            dead.push(*mid);
            continue;
        }
        if let Some(m) = miners.get_mut(mid) {
            assign_job(m, job_id);
        }
    }
    for d in dead { socks.remove(&d); }
}
fn send_prime(st: &Shared, body: Vec<u8>) {
    if st.prime_depth.load(Ordering::Relaxed) >= PRIME_QUEUE_CAP {
        let n = st.prime_dropped.fetch_add(1, Ordering::Relaxed);
        if n % 10_000 == 0 {
            log::warn!("Prime queue full; dropped {} shares so far", n + 1);
        }
        return;
    }
    if let Some(tx) = st.prime_tx.lock().unwrap().as_ref() {
        if tx.send(body).is_ok() {
            st.prime_depth.fetch_add(1, Ordering::Relaxed);
        }
    }
}
/// Depth is a gate on share delivery, so it must never wrap below zero.
fn release_prime_slots(st: &Shared, n: u64) {
    let _ = st.prime_depth.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
        Some(v.saturating_sub(n))
    });
}
fn send_prime_urgent(st: &Shared, body: Vec<u8>) {
    if let Some(tx) = st.prime_urgent.lock().unwrap().as_ref() { let _ = tx.send(body); }
}
fn kick_gbt(st: &Shared) {
    *st.gbt_due.lock().unwrap() = true;
    st.gbt_kick.notify_all();
}
fn wait_coinbaser(st: &Shared, value: u64, deadline: Instant) -> Option<CoinbaserV2> {
    let mut g = st.last_cb.lock().unwrap();
    loop {
        if let Some((v, cb)) = g.as_ref() {
            if cb.outputs.len() >= 2 && *v == value { return Some(cb.clone()); }
        }
        let now = Instant::now();
        if now >= deadline { return None; }
        let (gg, w) = st.cb_cv.wait_timeout(g, deadline.saturating_duration_since(now)).ok()?;
        g = gg;
        if w.timed_out() { return None; }
    }
}
/// Prefer a fresh Prime split for this template value; otherwise scale the last good split.
fn split_for_value(st: &Shared, value: u64, wait: Duration) -> Option<CoinbaserV2> {
    if let Some(cb) = wait_coinbaser(st, value, Instant::now() + wait) {
        return Some(cb);
    }
    let g = st.last_cb.lock().unwrap();
    let (_, cb) = g.as_ref()?;
    if cb.outputs.len() < 2 { return None; }
    Some(cb.scale_to(value))
}
fn build_split_job(tpl: &Value, tag: &str, extra1: &[u8; 4], cb: CoinbaserV2, job_id: u8, seq: u64) -> Option<Job> {
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
    let cbleg = cbtx::coinbase_legacy(height, tag, &extra, &cb, wit.as_deref());
    let mut hdr = HeaderV2::default();
    hdr.version = version; hdr.prev_block = prev; hdr.time = curtime;
    hdr.bits = u32::from_le_bytes(bits); hdr.height = height as i32;
    hdr.txcount = (txs.len() + 1) as u16;
    hdr.merkle_root = pow::merkle_root_sha256d(&cbleg, &merkle);
    hdr.extranonce = pow::header_extranonce(extra1);
    Some(Job {
        // Must be unique, including across restarts. Two jobs sharing an id cannot be
        // told apart at submission time, and a counter alone restarts at zero: a miner
        // still holding a pre-restart id would then be matched to an unrelated template.
        // The high half is a per-process salt from extranonce1, the low half the counter.
        id: format!("{:04x}{:04x}", u16::from_le_bytes([extra1[0], extra1[1]]), seq & 0xffff),
        prev_notify: hex::encode(pow::prevblock_hidden(&prev)),
        ntime: hex::encode([0u8; 8]), nbits: bits, value, height,
        branches: pow::merkle_branches_for_coinbase(&merkle), coinb1: cbleg, job_id,
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
fn prime_loop(st: Arc<Shared>, rx: mpsc::Receiver<Vec<u8>>, urgent: mpsc::Receiver<Vec<u8>>) {
    loop {
        let Some((mut sock, mut ch, _sess)) = connect_prime(&st.cfg) else {
            log::warn!("Prime connect failed; retry"); thread::sleep(Duration::from_secs(2)); continue;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
        // Without this a slow Prime back-pressures the socket and the gateway blocks in
        // write_all forever, so the share queue fills and every new share is dropped.
        // On timeout the framing is no longer trustworthy, so we drop the connection and
        // reconnect with a fresh session rather than resume mid-frame.
        let _ = sock.set_write_timeout(Some(Duration::from_secs(5)));
        st.prime_depth.store(0, Ordering::Relaxed);
        loop {
            // (body, counted): only the share queue holds slots
            let mut outgoing: Vec<(Vec<u8>, bool)> = Vec::new();
            while let Ok(body) = urgent.try_recv() { outgoing.push((body, false)); }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(body) => outgoing.push((body, true)),
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
            while let Ok(body) = rx.try_recv() { outgoing.push((body, true)); }
            let mut dead = false;
            let mut released = 0u64;
            for (body, counted) in outgoing {
                let pkt = mining::wrap_mining(&mut ch, &body, None);
                let sent = sock.write_all(&pkt).is_ok();
                if counted { released += 1; }
                if !sent { dead = true; break; }
            }
            release_prime_slots(&st, released);
            if dead { break; }
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
                    } else if plain.first() == Some(&SUB_BLOCKNOTIFY) {
                        log::info!("Prime blocknotify");
                        kick_gbt(&st);
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
    let host_label = host.clone();
    let id = st.next_id.fetch_add(1, Ordering::Relaxed);
    let Some(rdr_sock) = sock.try_clone().ok() else { return };
    let rdr = BufReader::new(rdr_sock);
    let mut user = String::new(); let mut ua = String::new();
    let vmin = st.cfg.vardiff_min.max(1);
    // Every session gets its own extranonce1. Sharing one across the gateway makes
    // identical rigs walk identical (extranonce2, nonce) pairs, so they submit the same
    // shares and the dedupe keeps only whichever arrived first, quietly moving credit
    // from one miner to another.
    let sess_en1 = (u32::from_le_bytes(st.extra1) ^ (id as u32)).to_le_bytes();
    let now = Instant::now();
    st.miners.lock().unwrap().insert(id, Miner { host, user: String::new(), ua: String::new(), vdiff: vmin, acc: 0, acc_n: 0, rej: 0, rej_n: 0, last: now, vdiff_prev: vmin, vdiff_prev_until: now,
        // Let the first retarget happen within a few seconds. Everyone starts at the
        // pool floor, so a gateway restart briefly puts the whole farm back on
        // difficulty 1 until vardiff catches up.
        last_retarget: now.checked_sub(VARDIFF_INTERVAL - Duration::from_secs(4)).unwrap_or(now),
        recent: VecDeque::new(), job_diffs: VecDeque::new() });
    for line in rdr.lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(|x| x.as_str()).unwrap_or("");
        let mid = msg.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "mining.subscribe" => {
                if let Some(u) = msg.get("params").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()) { ua = u.to_string(); }
                if let Ok(c) = sock.try_clone() { st.miner_socks.lock().unwrap().insert(id, c); }
                send_line(&mut sock, &json!({"id": mid, "result": [[["mining.notify", "lz"]], hex::encode(sess_en1), 8], "error": null}));
                let d0 = st.miners.lock().unwrap().get(&id).map(|m| m.vdiff).unwrap_or(vmin);
                send_line(&mut sock, &json!({"id": null, "method": "mining.set_difficulty", "params": [d0]}));
                let first_job = st.job.lock().unwrap().clone();
                if let Some(j) = first_job {
                    if j.outputs >= 2 {
                        let _ = write!(sock, "{}", notify_line(&j));
                        if let Some(m) = st.miners.lock().unwrap().get_mut(&id) {
                            assign_job(m, &j.id);
                        }
                    }
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
                // Rebuild against the job the miner actually hashed. Jobs republish about
                // once a second, so reaching for the current job instead rebuilds a stale
                // share against the wrong template: it fails its own target, and a real
                // solve would be assembled into a block nobody solved.
                let want = params.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let job = {
                    let hist = st.jobs.lock().unwrap();
                    hist.iter().rev().find(|j| j.id == want).cloned()
                };
                let Some(j) = job else {
                    let n = st.job_miss.fetch_add(1, Ordering::Relaxed);
                    if n < 30 || n % 200 == 0 {
                        let hist = st.jobs.lock().unwrap();
                        let oldest = hist.front().map(|q| q.id.as_str()).unwrap_or("-");
                        let newest = hist.back().map(|q| q.id.as_str()).unwrap_or("-");
                        log::warn!("stale job want={} held={} range={}..{}", want, hist.len(), oldest, newest);
                    }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([21, "StaleJob", null])}));
                    continue;
                };
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
                let mut en12 = Vec::from(sess_en1);
                en12.extend(en2.iter().take(8).copied());
                en12.resize(12, 0);
                hdr.extranonce = pow::header_extranonce(&en12);
                // The ASIC rolls an 8-byte ntime inside the 80-byte pass. We publish it as
                // zero, but a miner is free to roll it, and those bytes are hashed.
                hdr.time_offset = u32::from_le_bytes(sia_t[0..4].try_into().unwrap_or([0; 4]));
                hdr.nonce3 = u32::from_le_bytes(sia_t[4..8].try_into().unwrap_or([0; 4]));
                // Judge and pay the share at the difficulty its own job went out at. That is
                // what the miner was working under; the session may have been retargeted
                // several times since, and holding a share to a target it was never given
                // would reject work the miner genuinely did.
                // The difficulty this job went out at, to this session.
                //
                // One entry per job now that a retarget no longer re-stamps it, so the min
                // is just defensive. Falling back to the grace window matters for a share
                // naming a job older than the history we keep difficulties for.
                let accept_diff = {
                    let g = st.miners.lock().unwrap();
                    match g.get(&id) {
                        Some(m) => m
                            .job_diffs
                            .iter()
                            .filter(|(jid, _)| *jid == want)
                            .map(|(_, d)| *d)
                            .min()
                            .unwrap_or_else(|| {
                                if Instant::now() < m.vdiff_prev_until {
                                    m.vdiff.min(m.vdiff_prev)
                                } else {
                                    m.vdiff
                                }
                            }),
                        None => vmin,
                    }
                };
                let pot = accept_diff.max(1).ilog2() as u8;
                let credit = accept_diff.max(1);
                let hash = hdr.pow_hash();
                *st.last_share_hdr.lock().unwrap() = Some(hdr.clone());
                *st.last_share_job.lock().unwrap() = Some(j.id.clone());
                // The rebuilt header is the one we would submit as a block, so a share that
                // misses the difficulty we handed out means our assembly disagrees with the
                // miner and a real solve would be thrown away. Measure it on every share.
                let share_ok = pow::meets_target(&hash, &pow::target_for_pot(pot));
                if share_ok {
                    st.pow_ok.fetch_add(1, Ordering::Relaxed);
                } else {
                    let n = st.pow_bad.fetch_add(1, Ordering::Relaxed);
                    if n < 20 || n % 500 == 0 {
                        log::warn!(
                            "share missed its job target pot={} job={} nonce={:08x} nonce2={:08x} en2={} hash={:02x}{:02x}{:02x}{:02x}",
                            pot, j.id, hdr.nonce, hdr.nonce2, hex::encode(&en2),
                            hash[0], hash[1], hash[2], hash[3]
                        );
                    }
                }
                if !share_ok && st.verify == VerifyMode::Enforce {
                    st.rej.fetch_add(1, Ordering::Relaxed);
                    if let Some(m) = st.miners.lock().unwrap().get_mut(&id) {
                        m.rej = m.rej.saturating_add(credit);
                        m.rej_n += 1;
                    }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([23, "LowDifficultyShare", null])}));
                    continue;
                }
                st.acc.fetch_add(1, Ordering::Relaxed);
                let mut retarget = None;
                if let Some(m) = st.miners.lock().unwrap().get_mut(&id) {
                    record_share(m, credit);
                    if m.last_retarget.elapsed() >= VARDIFF_INTERVAL {
                        m.last_retarget = Instant::now();
                        let want = vardiff_for(miner_hs(m), vmin);
                        if want != m.vdiff {
                            m.vdiff_prev = m.vdiff;
                            m.vdiff_prev_until = Instant::now() + VARDIFF_GRACE;
                            m.vdiff = want;
                            retarget = Some(want);
                        }
                    }
                }
                if let Some(d) = retarget {
                    log::info!("vardiff user={} host={} -> {}", user, host_label, d);
                    // Difficulty only, and deliberately no new job: it takes effect on the
                    // next one, which keeps a job id tied to a single difficulty for this
                    // session. Re-stamping the current job instead makes shares already in
                    // flight look as though they missed a target they were never given.
                    send_line(&mut sock, &json!({"id": null, "method": "mining.set_difficulty", "params": [d]}));
                }
                let en = en12.clone();
                let submit = PowSubmit {
                    job_id: j.job_id, coinbase_id: j.cb.id, is_block: false, subsidy_only: false, quickdiff: false,
                    target_byte: pot, ntime: j.header.time, nonce: hdr.nonce, version: j.header.version as u32,
                    extranonce: en, username: user.clone(), use_time_offset: false,
                    job: Some(mining::JobSection {
                        prev_hash: j.header.prev_block, target_byte_index: 0, nbits: j.nbits,
                        coinbaser_id: j.cb.id, height: j.height, coinbase_value: j.value,
                        txn_count: j.txn_count, txn_total_weight: 0, txn_total_size: 0, txn_total_sigops: 0,
                        merkle_branches: j.branches.clone(),
                    }),
                    coinbase: Some(mining::CoinbaseSection {
                        coinbase_id: j.cb.id,
                        coinb1: j.coinb1.clone(),
                        coinb2: Vec::new(),
                    }),
                    blake2b: Some(mining::Blake2bSection { sia_ntime: sia_t, sia_nonce: sia_n, time_on_wire: j.header.time }),
                };
                send_prime(&st, submit.encode());
                if let Some(tgt) = pow::bits_to_target(j.header.bits) {
                    let hit = pow::meets_target(&hash, &tgt);
                    if hit {
                        log::info!("share meets nbits height={} hash_hi={:02x}{:02x}{:02x}{:02x}", j.height, hash[0], hash[1], hash[2], hash[3]);
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
fn assemble_block(st: &Shared, j: &Job, hdr: &HeaderV2) -> Vec<u8> {
    let mut extra = st.extra1.to_vec(); extra.extend_from_slice(&[0u8; 8]);
    let cbw = cbtx::coinbase_witness(j.height, &j.tag, &extra, &j.cb, j.witness_commit.as_deref());
    let mut blk = hdr.serialize().to_vec();
    blk.extend_from_slice(&cbtx::compact(1 + j.tx_hexes.len() as u64));
    blk.extend_from_slice(&cbw);
    for tx in &j.tx_hexes { blk.extend_from_slice(tx); }
    blk
}
fn audit_json(st: &Shared) -> String {
    let job = st.job.lock().unwrap().clone();
    let Some(j) = job else { return json!({"error":"no job"}).to_string(); };
    let blk = assemble_block(st, &j, &j.header);
    let outs: Vec<Value> = j.cb.outputs.iter().map(|o| json!({
        "sats": o.sats,
        "script": hex::encode(&o.script),
    })).collect();
    let share = {
        let hid = st.last_share_job.lock().unwrap().clone();
        let hdr = st.last_share_hdr.lock().unwrap().clone();
        match (hid, hdr) {
            (Some(id), Some(h)) if id == j.id => {
                let sblk = assemble_block(st, &j, &h);
                Some(json!({
                    "job_id": id,
                    "nonce": h.nonce,
                    "nonce2": h.nonce2,
                    "block_hex": hex::encode(&sblk),
                    "block_bytes": sblk.len(),
                }))
            }
            _ => None,
        }
    };
    json!({
        "height": j.height,
        "value": j.value,
        "output_sum": j.cb.outputs.iter().map(|o| o.sats).sum::<u64>(),
        "outputs": j.outputs,
        "tx_count": j.txn_count,
        "block_bytes": blk.len(),
        "witness_commit": j.witness_commit.as_ref().map(|w| hex::encode(w)),
        "coinbase_outputs": outs,
        "block_hex": hex::encode(&blk),
        "share": share,
        "share_pow_ok": st.pow_ok.load(Ordering::Relaxed),
        "share_pow_bad": st.pow_bad.load(Ordering::Relaxed),
        "verify_mode": format!("{:?}", st.verify),
        "prime_queue": st.prime_depth.load(Ordering::Relaxed),
        "prime_dropped": st.prime_dropped.load(Ordering::Relaxed),
        "job_miss": st.job_miss.load(Ordering::Relaxed),
        "template_age_s": unix_now().saturating_sub(st.last_pub_unix.load(Ordering::Relaxed)),
    }).to_string()
}
fn maybe_submit_block(st: &Shared, j: &Job, hdr: &HeaderV2) {
    let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else { return };
    let blk = assemble_block(st, j, hdr);
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
        let (ctype, body) = if path.starts_with("/audit") {
            ("application/json", audit_json(&st))
        } else if path.starts_with("/clients") {
            ("text/html", clients_html(&st))
        } else {
            ("text/html", home_html(&st))
        };
        let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    }
}
fn home_html(st: &Shared) -> String {
    let acc = st.acc.load(Ordering::Relaxed); let rej = st.rej.load(Ordering::Relaxed);
    let outs = st.published_outputs.load(Ordering::Relaxed); let ph = st.published_height.load(Ordering::Relaxed);
    let hr: f64 = st.miners.lock().unwrap().values().map(miner_hs).sum();
    format!("<html><body>Estimated Hashrate: {}<br>Local Shares Accepted: {}<br>Local Shares Rejected: {}<br>Coinbase outputs: {}<br>Published height: {}<br>Share PoW verified: {} / missed: {} (mode {:?})<br>Prime queue: {} (dropped {})</body></html>", parse_hr_label(hr), acc, rej, outs, ph, st.pow_ok.load(Ordering::Relaxed), st.pow_bad.load(Ordering::Relaxed), st.verify, st.prime_depth.load(Ordering::Relaxed), st.prime_dropped.load(Ordering::Relaxed))
}
fn clients_html(st: &Shared) -> String {
    let mut rows = String::from("<TABLE><TR><TD>#</TD><TD>Host</TD><TD>Auth Username</TD><TD></TD><TD>Last</TD><TD>VDiff</TD><TD>A</TD><TD>R</TD><TD>HR</TD><TD></TD><TD>UA</TD></TR>");
    for (i, m) in st.miners.lock().unwrap().values().enumerate() {
        rows.push_str(&format!("<TR><TD>{}</TD><TD>{}</TD><TD>{}</TD><TD></TD><TD>{:.1} s</TD><TD>{}</TD><TD>{} ({})</TD><TD>{} ({})</TD><TD>{}</TD><TD></TD><TD>{}</TD></TR>",
            i, html_esc(&m.host), html_esc(&m.user), m.last.elapsed().as_secs_f64(), m.vdiff, m.acc, m.acc_n, m.rej, m.rej_n,
            parse_hr_label(miner_hs(m)), html_esc(&m.ua)));
    }
    rows.push_str("</TABLE>"); format!("<html><body>{rows}</body></html>")
}
fn gbt_loop(st: Arc<Shared>) {
    let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else { log::error!("missing rpc cookie"); return; };
    let tag = st.cfg.coinbase_tag.clone().unwrap_or_else(|| "Lazarus".into());
    let mut last_hash = String::new();
    let mut last_pub = Instant::now()
        .checked_sub(JOB_REFRESH)
        .unwrap_or_else(Instant::now);
    loop {
        let hash = rpc(&st.cfg.rpc, &auth, "getbestblockhash", json!([])).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
        let new_tip = !hash.is_empty() && hash != last_hash;
        if new_tip { last_hash = hash.clone(); }
        let kicked = { let mut d = st.gbt_due.lock().unwrap(); let v = *d; *d = false; v };
        if new_tip || kicked || last_pub.elapsed() >= JOB_REFRESH {
            if let Some(tpl) = rpc(&st.cfg.rpc, &auth, "getblocktemplate", json!([{"rules": ["segwit", "blake2b"]}])) {
                let value = tpl.get("coinbasevalue").and_then(|x| x.as_u64()).unwrap_or(0);
                let height = tpl.get("height").and_then(|x| x.as_u64()).unwrap_or(0);
                let prev = hex_rev(tpl.get("previousblockhash").and_then(|x| x.as_str()).unwrap_or("")).unwrap_or([0u8; 32]);
                if value > 0 {
                    send_prime_urgent(&st, CoinbaserRequest { value, prevhash: prev }.encode());
                    let first = st.last_cb.lock().unwrap().is_none();
                    let wait = if first || new_tip { Duration::from_millis(1500) } else { Duration::from_millis(250) };
                    if let Some(cb) = split_for_value(&st, value, wait) {
                        let scaled = cb.value_sum() != value;
                        let seq = st.job_seq.fetch_add(1, Ordering::Relaxed);
                        let jid = (seq % 255) as u8 + 1;
                        if let Some(j) = build_split_job(&tpl, &tag, &st.extra1, cb, jid, seq) {
                            log::info!("published job height={} txs~{} outputs={} value={}{}", j.height, j.txn_count, j.outputs, value, if scaled { " (scaled)" } else { "" });
                            st.published_outputs.store(j.outputs, Ordering::Relaxed);
                            st.published_height.store(height, Ordering::Relaxed);
                            let line = notify_line(&j);
                            let jid_str = j.id.clone();
                            {
                                let mut hist = st.jobs.lock().unwrap();
                                hist.push_back(j.clone());
                                // Kept regardless of height. A share against a job from
                                // the previous height is still work the miner did, and
                                // dropping the job means we cannot rebuild it and have to
                                // turn the share away. Only block submission cares about
                                // height, and a stale block is refused by the node anyway.
                                while hist.len() > JOB_HISTORY {
                                    hist.pop_front();
                                }
                            }
                            *st.job.lock().unwrap() = Some(j);
                            last_pub = Instant::now();
                            st.last_pub_unix.store(unix_now(), Ordering::Relaxed);
                            broadcast(&st, &line, &jid_str);
                        }
                    } else {
                        log::warn!("no split coinbaser for value={}; not publishing unsplit job", value);
                    }
                }
            }
        }
        let age = unix_now().saturating_sub(st.last_pub_unix.load(Ordering::Relaxed));
        if st.last_pub_unix.load(Ordering::Relaxed) > 0 && age > STALE_TEMPLATE_SECS {
            log::warn!(
                "template is {}s old; node rpc may be unreachable. miners are hashing a stale job",
                age
            );
        }
        let mut due = st.gbt_due.lock().unwrap();
        let (_g, _) = st.gbt_kick.wait_timeout(due, Duration::from_secs(2)).unwrap_or_else(|e| e.into_inner());
        due = _g;
        drop(due);
    }
}
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let raw = std::fs::read_to_string(&cli.config).expect("config");
    let cfg: GwCfg = serde_json::from_str(&raw).expect("json config");
    let extra1: [u8; 4] = rand::random();
    log::info!("lazarus-gateway profile={} stratum={} api={} vardiff_min={} verify={:?}", cfg.profile.as_deref().unwrap_or("asic"), cfg.stratum_listen, cfg.api_listen, cfg.vardiff_min, verify_mode(cfg.verify_shares.as_deref()));
    let (tx, rx) = mpsc::channel();
    let (utx, urx) = mpsc::channel();
    let st = Arc::new(Shared {
        cfg: cfg.clone(), job: Mutex::new(None), jobs: Mutex::new(VecDeque::new()),
        miners: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1), acc: AtomicU64::new(0), rej: AtomicU64::new(0), extra1,
        prime_tx: Mutex::new(Some(tx)), last_cb: Mutex::new(None), cb_cv: Condvar::new(),
        gbt_kick: Condvar::new(), gbt_due: Mutex::new(true),
        prime_urgent: Mutex::new(Some(utx)),
        miner_socks: Mutex::new(HashMap::new()), published_outputs: AtomicUsize::new(0),
        published_height: AtomicU64::new(0),
        last_share_hdr: Mutex::new(None), last_share_job: Mutex::new(None),
        job_seq: AtomicU64::new(0), pow_ok: AtomicU64::new(0), pow_bad: AtomicU64::new(0),
        verify: verify_mode(cfg.verify_shares.as_deref()),
        prime_depth: AtomicU64::new(0), prime_dropped: AtomicU64::new(0),
        job_miss: AtomicU64::new(0), last_pub_unix: AtomicU64::new(0),
    });
    { let s = st.clone(); thread::spawn(move || prime_loop(s, rx, urx)); }
    { let s = st.clone(); thread::spawn(move || api_loop(s)); }
    { let s = st.clone(); thread::spawn(move || gbt_loop(s)); }
    let lis = TcpListener::bind(&st.cfg.stratum_listen).expect("stratum bind");
    log::info!("stratum {}", st.cfg.stratum_listen);
    for inc in lis.incoming() {
        if let Ok(s) = inc { let st = st.clone(); thread::spawn(move || handle_miner(s, st)); }
    }
}
