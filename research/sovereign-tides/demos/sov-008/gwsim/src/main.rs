// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Mike Moore (AwokenLazarus)
//! SOV-008 demo gateway: a DATUM client that talks the real protocol to primed.
//!
//! It is what primed sees of a gateway, minus the hashing: the encrypted handshake under a
//! persisted identity key `G`, one share per template carrying the job and coinbase sections
//! (the only way a gateway ever tells a Prime about a job), and answers to
//! `request_full_block` with that job's raw transactions. The shares carry no real work, so
//! primed refuses them (high hash) after it has taken their job sections; a diff-1 BLAKE2b share
//! is ~2^32 hashes, far too many for a CPU regtest demo with seven gateways.
//!
//! Templates arrive on stdin, one JSON object per line, from the demo harness:
//!   {"prev": "<display hex>", "height": N, "bits": "<hex>", "value": sats, "txs": ["<raw hex>", ...]}
//! Each one becomes the next job slot (0..7, like stock). A line `{"answer": false}` makes the
//! gateway stop answering transaction requests (a gateway that will not be checked).
//!
//! usage: gwsim <prime host:port> <prime.key file> <G secret hex (64 bytes)> <payout script hex> <name>
//!        gwsim register <G secret hex>     (prints "G signature" for POST /sovereignty/register)
use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use datum_wire::cmd;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake;
use datum_wire::mining::{self, Blake2bSection, CoinbaseSection, JobSection, PowSubmit};
use datum_wire::pow::{self, Hash};
use datum_wire::verify;

const SLOTS: u8 = 8;

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

fn hex32_le(display: &str) -> Hash {
    let mut b: Hash = hex::decode(display).expect("hex").try_into().expect("32 bytes");
    b.reverse();
    b
}

fn now() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32
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
    let hello = handshake::build_client_hello(&pool.box_pk(), &g, &session, "gwsim/0.1 (SOV-008)", seed, &[0u8; 16]);
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

    {
        let (link, jobs, answer, asked, name) = (link.clone(), jobs.clone(), answer.clone(), asked.clone(), name.clone());
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

    let mut slot: u8 = SLOTS - 1;
    let mut nonce: u64 = 0;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if let Some(b) = v.get("answer").and_then(|x| x.as_bool()) {
            answer.store(b, Ordering::Relaxed);
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
        let outs = vec![TxOut { value, script: payout.clone() }];
        let (cb, tidx, split_at) = coinbase::build(height, b"Lazarus", &outs, 0);
        nonce += 1;
        let mut n8 = [0u8; 8];
        n8.copy_from_slice(&nonce.to_le_bytes());
        let s = PowSubmit {
            job_id: slot,
            coinbase_id: 0,
            flags: mining::FLAG_BLAKE2B,
            target_pot: 0,
            ntime32: 0,
            nonce32: nonce as u32,
            version: 0xa000_0000,
            extranonce: [0x0b, 0x10, 0xc0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8],
            username: format!("{name}.sim"),
            reserved: [0; 4],
            blake2b: Some(Blake2bSection { ntime: [0; 8], nonce: n8 }),
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
        if link.send_mining(&s.encode()).is_err() {
            eprintln!("[{name}] send failed");
            break;
        }
    }
    // keep the session up until the harness kills us
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
