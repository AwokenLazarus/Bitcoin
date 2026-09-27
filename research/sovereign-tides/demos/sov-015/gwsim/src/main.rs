// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Mike Moore (AwokenLazarus)
//! SOV-013 soak gateway: the SOV-008 DATUM gateway simulator (`demos/sov-008/gwsim` @ 5737c30,
//! plus SOV-010's `sign`), extended for a 72 h soak:
//!
//! * **Receipt latency.** Every share is timed from the write to primed's share receipt (matched on
//!   `nonce32`, unique per session) and appended to `$GWSIM_LAT` as
//!   `<unix time> <kind> <status hex> <reason> <microseconds>`; `kind` is `job` (a template's
//!   job share, which carries no work and is refused high-hash after primed has taken its
//!   sections) or `grind` (a real diff-1 share, which primed accepts).
//! * **Grinding.** `{"grind": N}` on stdin grinds the newest job on N threads until one diff-1
//!   share is found, submits it, and stops. On regtest every diff-1 share also meets the block
//!   target, so primed records it as a block candidate, asks for the job's transactions and
//!   submits the block to its node: one accepted share is one real block. `{"grind": 0}` stops.
//! * **Witness commitment.** A template line may carry `"wc"` (getblocktemplate's
//!   `default_witness_commitment`); the coinbase then carries it, so a block primed assembles
//!   from a segwit template is valid.
//!
//! Templates arrive on stdin, one JSON object per line:
//!   {"prev": "<display hex>", "height": N, "bits": "<hex>", "value": sats, "txs": ["<raw hex>", ...], "wc": "<hex>"}
//! Each one becomes the next job slot (0..7, like stock). `{"answer": false}` makes the gateway
//! stop answering transaction requests.
//!
//! usage: gwsim <prime host:port> <prime.key file> <G secret hex (64 bytes)> <payout script hex> <name>
//!        gwsim register <G secret hex>          (prints "G signature" for the old demo register)
//!        gwsim sign <G secret hex> <message>    (SOV-P-002 §4.1 enrol signature)
//!        gwsim bench <threads> <seconds>        (hash rate of the grind loop)
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use datum_wire::cmd;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake;
use datum_wire::mining::{self, Blake2bSection, CoinbaseSection, JobSection, PowSubmit};
use datum_wire::pow::{self, Hash, JobWork};
use datum_wire::verify::{self, JobSlot};

const SLOTS: u8 = 8;
/// Nonces a grind thread tries between looks at the job and the stop flag.
const CHUNK: u32 = 1 << 16;

struct Out {
    stream: TcpStream,
    keys: KeyStream,
}

struct Link {
    out: Mutex<Out>,
    channel: Mutex<Channel>,
}

impl Link {
    fn send_mining(&self, plain: &[u8]) -> std::io::Result<()> {
        let payload = self.channel.lock().unwrap().encrypt(plain);
        let mut o = self.out.lock().unwrap();
        let mut h = Header::new(cmd::MINING, payload.len());
        h.channel = true;
        let mut buf = h.encode(&mut o.keys).to_vec();
        buf.extend_from_slice(&payload);
        o.stream.write_all(&buf)
    }
}

/// Shares written and not yet answered: nonce32 -> (sent at, kind).
type Pending = Arc<Mutex<HashMap<u32, (Instant, &'static str)>>>;

struct LatLog(Mutex<Option<std::fs::File>>);

impl LatLog {
    fn open() -> Self {
        let f = std::env::var("GWSIM_LAT")
            .ok()
            .and_then(|p| OpenOptions::new().create(true).append(true).open(p).ok());
        LatLog(Mutex::new(f))
    }
    fn write(&self, line: &str) {
        if let Some(f) = self.0.lock().unwrap().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// The job being ground: a share for it (target pot 0, nonce to fill in) and its hashing state.
struct GrindJob {
    gen: u32,
    submit: PowSubmit,
    work: JobWork,
}

fn hex32_le(display: &str) -> Hash {
    let mut b: Hash = hex::decode(display).expect("hex").try_into().expect("32 bytes");
    b.reverse();
    b
}

fn now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
}

fn now_f() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

/// Grind `job` from `start` for `CHUNK` nonces; the nonce's upper half is the thread and
/// generation, so threads and restarts never repeat work.
fn grind_chunk(job: &GrindJob, tid: u32, start: u32, target: &Hash) -> Option<[u8; 8]> {
    let ntime = job.submit.blake2b.as_ref().unwrap().ntime;
    let mut n8 = [0u8; 8];
    n8[4..6].copy_from_slice(&(tid as u16).to_le_bytes());
    n8[6..8].copy_from_slice(&(job.gen as u16).to_le_bytes());
    for n in start..start.saturating_add(CHUNK) {
        n8[..4].copy_from_slice(&n.to_le_bytes());
        if pow::meets_target(&job.work.hash(&n8, &ntime), target) {
            return Some(n8);
        }
    }
    None
}

fn bench(threads: usize, secs: f64) {
    let mut s = PowSubmit {
        job_id: 0,
        coinbase_id: 0,
        flags: mining::FLAG_BLAKE2B,
        target_pot: 0,
        ntime32: 0,
        nonce32: 0,
        version: 0xa000_0000,
        extranonce: [0; 12],
        username: "bench".into(),
        reserved: [0; 4],
        blake2b: Some(Blake2bSection { ntime: [0; 8], nonce: [0; 8] }),
        time_on_wire: Some(now()),
        job: None,
        coinbase: None,
    };
    let (cb, tidx, split_at) = coinbase::build(200, b"Lazarus", &[TxOut { value: 1, script: vec![0x51] }], 0);
    s.job = Some(JobSection {
        prev_hash: [7; 32],
        target_byte_index: tidx as u16,
        nbits: 0x207f_ffffu32.to_le_bytes(),
        coinbaser_id: 0,
        height: 200,
        coinbase_value: 1,
        txn_count: 0,
        txn_total_weight: 0,
        txn_total_size: 0,
        txn_total_sigops: 0,
        merkle_branches: vec![],
    });
    s.coinbase = Some(CoinbaseSection {
        coinbase_id: 0,
        coinb1: cb[..split_at].to_vec(),
        coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
    });
    let mut slot = JobSlot::default();
    slot.absorb(&s).unwrap();
    let work = verify::job_work_for(&slot, &s, false).unwrap();
    let job = Arc::new(GrindJob { gen: 1, submit: s, work });
    let never = [0u8; 32];
    let t0 = Instant::now();
    let done = Arc::new(AtomicU64::new(0));
    let hs: Vec<_> = (0..threads)
        .map(|t| {
            let (job, done) = (job.clone(), done.clone());
            std::thread::spawn(move || {
                let mut start = 0u32;
                while t0.elapsed().as_secs_f64() < secs {
                    grind_chunk(&job, t as u32, start, &never);
                    start = start.wrapping_add(CHUNK);
                    done.fetch_add(u64::from(CHUNK), Ordering::Relaxed);
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let rate = done.load(Ordering::Relaxed) as f64 / t0.elapsed().as_secs_f64();
    println!(
        "{threads} threads: {:.2} MH/s; a diff-1 share every {:.0} s on average",
        rate / 1e6,
        4_294_967_296.0 / rate
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    // `gwsim register <G secret hex>`: G and its signature over primed's registration message
    if a.len() == 3 && a[1] == "register" {
        let g = Identity::from_secret_bytes(&hex::decode(&a[2]).expect("G hex").try_into().expect("64-byte G"));
        let g_hex = hex::encode(g.sign_pk());
        let sig = g.sign(format!("LZT1 register G={g_hex} pool=lazarus").as_bytes());
        println!("{g_hex} {}", hex::encode(sig));
        return;
    }
    // SOV-010: sign a registration message (SOV-P-002 §4.1) with G, as `lazarus-gateway enrol` does
    if a.len() == 4 && a[1] == "sign" {
        let g = Identity::from_secret_bytes(&hex::decode(&a[2]).expect("G hex").try_into().expect("64-byte G"));
        let msg = std::fs::read(&a[3]).expect("message file");
        let sig = g.sign(&[b"XBT-SOVEREIGN-TIDES/register\0".as_slice(), &msg].concat());
        println!("{}", hex::encode(sig));
        return;
    }
    if a.len() == 4 && a[1] == "bench" {
        bench(a[2].parse().expect("threads"), a[3].parse().expect("seconds"));
        return;
    }
    if a.len() != 6 {
        eprintln!("usage: gwsim <prime host:port> <prime.key> <G secret hex> <payout script hex> <name>");
        std::process::exit(2);
    }
    let pool_secret: [u8; 64] = hex::decode(std::fs::read_to_string(&a[2]).expect("prime.key").trim())
        .expect("prime.key hex")
        .try_into()
        .expect("64-byte prime.key");
    let pool = Identity::from_secret_bytes(&pool_secret);
    let g = Identity::from_secret_bytes(&hex::decode(&a[3]).expect("G hex").try_into().expect("64-byte G"));
    let payout = hex::decode(&a[4]).expect("payout script hex");
    let name = a[5].clone();

    let mut stream = TcpStream::connect(&a[1]).expect("connect to primed");
    stream.set_nodelay(true).ok();
    let session = Identity::generate();
    let seed = u32::from_le_bytes(session.sign_pk()[..4].try_into().unwrap());
    let hello = handshake::build_client_hello(&pool.box_pk(), &g, &session, "gwsim/0.2 (SOV-013)", seed, &[0u8; 16]);
    let mut initial = KeyStream(CLIENT_INITIAL_KEY);
    let mut h = Header::new(cmd::HELLO, hello.len());
    h.sealed = true;
    h.signed = true;
    let mut out = h.encode(&mut initial).to_vec();
    out.extend_from_slice(&hello);
    stream.write_all(&out).expect("hello");
    let (send_keys, mut recv_keys) = KeyStream::from_seed(seed);
    let mut hb = [0u8; Header::SIZE];
    stream.read_exact(&mut hb).expect("server hello header");
    let rh = Header::decode(hb, &mut recv_keys).expect("server hello header");
    assert_eq!(rh.cmd, cmd::HELLO_REPLY, "server hello");
    let mut payload = vec![0u8; rh.len as usize];
    stream.read_exact(&mut payload).expect("server hello");
    let (_srv_sign, srv_box, _motd) =
        handshake::parse_server_hello(&pool.sign_pk(), &session, &payload).expect("server hello verifies");
    let (recv_nonce, send_nonce) = crypto::session_nonces(seed, &session.sign_pk());
    let link = Arc::new(Link {
        out: Mutex::new(Out { stream: stream.try_clone().unwrap(), keys: send_keys }),
        channel: Mutex::new(Channel::new(session.precompute(&srv_box), send_nonce, recv_nonce)),
    });
    eprintln!("[{name}] connected as G={}", hex::encode(g.sign_pk()));

    // slot -> raw transactions of the job in it
    let jobs: Arc<Mutex<Vec<Vec<Vec<u8>>>>> = Arc::new(Mutex::new(vec![Vec::new(); SLOTS as usize]));
    let answer = Arc::new(AtomicBool::new(true));
    let asked = Arc::new(AtomicU64::new(0));
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let lat = Arc::new(LatLog::open());

    {
        let (link, jobs, answer, asked, name, pending, lat) =
            (link.clone(), jobs.clone(), answer.clone(), asked.clone(), name.clone(), pending.clone(), lat.clone());
        let mut stream = stream;
        std::thread::spawn(move || loop {
            let mut hb = [0u8; Header::SIZE];
            if stream.read_exact(&mut hb).is_err() {
                eprintln!("[{name}] primed closed the connection");
                std::process::exit(0);
            }
            let h = Header::decode(hb, &mut recv_keys).expect("header");
            let mut payload = vec![0u8; h.len as usize];
            if h.len > 0 && stream.read_exact(&mut payload).is_err() {
                std::process::exit(0);
            }
            if h.cmd != cmd::MINING {
                continue;
            }
            let body = {
                let mut ch = link.channel.lock().unwrap();
                let body = ch.decrypt_in_place(&mut payload).expect("decrypt").to_vec();
                if h.signed { body[..body.len() - crypto::SIG].to_vec() } else { body }
            };
            if body.len() >= 10 && body[0] == mining::SUB_SHARE_RECEIPT {
                let at = Instant::now();
                let status = body[1];
                let reason = u16::from_le_bytes([body[2], body[3]]);
                let n32 = u32::from_le_bytes(body[4..8].try_into().unwrap());
                if let Some((sent, kind)) = pending.lock().unwrap().remove(&n32) {
                    let us = at.duration_since(sent).as_micros();
                    lat.write(&format!("{:.3} {kind} {status:02x} {reason} {us}\n", now_f()));
                    if kind == "grind" {
                        eprintln!("[{name}] grind share receipt {status:02x} reason {reason} in {us} us");
                    }
                }
                continue;
            }
            if body.len() >= 3 && body[0] == mining::SUB_JOB_VALIDATION && body[1] == 0x12 {
                let slot = body[2];
                let n = asked.fetch_add(1, Ordering::Relaxed) + 1;
                if !answer.load(Ordering::Relaxed) {
                    continue;
                }
                let txs = jobs.lock().unwrap().get(slot as usize).cloned().unwrap_or_default();
                let mut m = vec![mining::SUB_JOB_VALIDATION, 0x92, slot];
                if txs.is_empty() {
                    m.push(0xF0);
                } else {
                    m.push(0x01);
                    m.extend_from_slice(&(txs.len() as u16).to_le_bytes());
                    for t in &txs {
                        m.extend_from_slice(&(t.len() as u16).to_le_bytes());
                        m.push((t.len() >> 16) as u8);
                        m.extend_from_slice(t);
                    }
                }
                m.push(mining::END);
                let _ = link.send_mining(&m);
                eprintln!("[{name}] check #{n}: sent job {slot} ({} txs)", txs.len());
            }
        });
    }

    // nonce32 for the next share (unique per session: receipts are matched on it)
    let seq = Arc::new(AtomicU32::new(1));
    let send_share = {
        let (link, pending, seq) = (link.clone(), pending.clone(), seq.clone());
        move |mut s: PowSubmit, kind: &'static str| -> std::io::Result<()> {
            let n = seq.fetch_add(1, Ordering::Relaxed);
            s.nonce32 = n;
            let bytes = s.encode();
            let mut p = pending.lock().unwrap();
            if p.len() > 10_000 {
                p.clear(); // receipts that never came; do not grow without bound
            }
            p.insert(n, (Instant::now(), kind));
            drop(p);
            link.send_mining(&bytes)
        }
    };

    // grinding: `want` threads wanted (0 = idle), the job in `grind`, finds on `found`
    let want = Arc::new(AtomicUsize::new(0));
    let grind: Arc<Mutex<Option<Arc<GrindJob>>>> = Arc::new(Mutex::new(None));
    let (found_tx, found_rx) = mpsc::channel::<(u32, [u8; 8])>();
    let mut spawned = 0usize;
    let mut gen: u32 = 0;
    let share_target = pow::share_target_le(0).unwrap();
    {
        let (grind, want, send_share, name) = (grind.clone(), want.clone(), send_share.clone(), name.clone());
        std::thread::spawn(move || {
            for (g, n8) in found_rx {
                let job = grind.lock().unwrap().clone();
                let Some(job) = job.filter(|j| j.gen == g) else { continue };
                if want.swap(0, Ordering::SeqCst) == 0 {
                    continue; // another thread's find already went out
                }
                let mut s = job.submit.clone();
                s.blake2b.as_mut().unwrap().nonce = n8;
                eprintln!("[{name}] found a diff-1 share on job {} (height {})", s.job_id, s.job.as_ref().unwrap().height);
                if send_share(s, "grind").is_err() {
                    eprintln!("[{name}] grind share send failed");
                }
            }
        });
    }

    let mut slot: u8 = SLOTS - 1;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let Some(b) = v.get("answer").and_then(|x| x.as_bool()) {
            answer.store(b, Ordering::Relaxed);
            continue;
        }
        if let Some(n) = v.get("grind").and_then(|x| x.as_u64()) {
            let n = n as usize;
            while spawned < n {
                let (grind, want, found_tx) = (grind.clone(), want.clone(), found_tx.clone());
                let tid = spawned as u32;
                std::thread::spawn(move || {
                    let mut last_gen = u32::MAX;
                    let mut start = 0u32;
                    loop {
                        if (tid as usize) >= want.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        let job = grind.lock().unwrap().clone();
                        let Some(job) = job else {
                            std::thread::sleep(Duration::from_millis(50));
                            continue;
                        };
                        if job.gen != last_gen {
                            last_gen = job.gen;
                            start = 0;
                        }
                        if let Some(n8) = grind_chunk(&job, tid, start, &share_target) {
                            let _ = found_tx.send((job.gen, n8));
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        start = start.wrapping_add(CHUNK);
                    }
                });
                spawned += 1;
            }
            want.store(n, Ordering::SeqCst);
            eprintln!("[{name}] grind on {n} threads");
            continue;
        }
        let txs: Vec<Vec<u8>> = v["txs"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).filter_map(|x| hex::decode(x).ok()).collect())
            .unwrap_or_default();
        let ids: Vec<Hash> = txs.iter().filter_map(|t| verify::txid(t)).collect();
        if ids.len() != txs.len() {
            eprintln!("[{name}] template with an unparsable tx, skipped");
            continue;
        }
        let height = v["height"].as_u64().unwrap_or(0) as u32;
        let value = v["value"].as_u64().unwrap_or(0);
        let bits = u32::from_str_radix(v["bits"].as_str().unwrap_or("207fffff"), 16).unwrap_or(0x207f_ffff);
        let prev = hex32_le(v["prev"].as_str().unwrap_or(&"00".repeat(32)));
        slot = (slot + 1) % SLOTS;
        jobs.lock().unwrap()[slot as usize] = txs;
        let mut outs = vec![TxOut { value, script: payout.clone() }];
        if let Some(wc) = v.get("wc").and_then(|x| x.as_str()).and_then(|x| hex::decode(x).ok()) {
            outs.push(TxOut { value: 0, script: wc });
        }
        let (cb, tidx, split_at) = coinbase::build(height, b"Lazarus", &outs, 0);
        let s = PowSubmit {
            job_id: slot,
            coinbase_id: 0,
            flags: mining::FLAG_BLAKE2B,
            target_pot: 0,
            ntime32: 0,
            nonce32: 0,
            version: 0xa000_0000,
            extranonce: [0x0b, 0x10, 0xc0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8],
            username: format!("{name}.sim"),
            reserved: [0; 4],
            blake2b: Some(Blake2bSection { ntime: [0; 8], nonce: [0; 8] }),
            time_on_wire: Some(now()),
            job: Some(JobSection {
                prev_hash: prev,
                target_byte_index: tidx as u16,
                nbits: bits.to_le_bytes(),
                coinbaser_id: 0,
                height,
                coinbase_value: value,
                txn_count: ids.len() as u32,
                txn_total_weight: 4_000,
                txn_total_size: 0,
                txn_total_sigops: 0,
                merkle_branches: pow::merkle_branches_for_coinbase(&ids),
            }),
            coinbase: Some(CoinbaseSection {
                coinbase_id: 0,
                coinb1: cb[..split_at].to_vec(),
                coinb2: cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec(),
            }),
        };
        // the job's own share (no work: refused high-hash once primed has its sections)
        let mut job_share = s.clone();
        job_share.blake2b.as_mut().unwrap().nonce = (u64::from(seq.load(Ordering::Relaxed))).to_le_bytes();
        if send_share(job_share, "job").is_err() {
            eprintln!("[{name}] send failed");
            break;
        }
        // grinding follows the newest job
        if want.load(Ordering::Relaxed) > 0 || spawned > 0 {
            let mut js = JobSlot::default();
            if js.absorb(&s).is_ok() {
                if let Some(work) = verify::job_work_for(&js, &s, false) {
                    gen = gen.wrapping_add(1);
                    *grind.lock().unwrap() = Some(Arc::new(GrindJob { gen, submit: s, work }));
                }
            }
        }
    }
    // keep the session up until the harness kills us
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
