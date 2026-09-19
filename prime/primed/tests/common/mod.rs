//! What the end-to-end tests share: a Prime under test, a gateway-side DATUM session, a share
//! builder and grinder, and a stand-in for the node's JSON-RPC.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use datum_wire::cmd;
use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake;
use datum_wire::mining::{self, Blake2bSection, CoinbaseSection, JobSection, PowSubmit};
use datum_wire::pow::{self, Hash};
use datum_wire::verify::{job_work_for, JobSlot};

pub const HEIGHT: u32 = 966_267;
pub const VALUE: u64 = 312_538_966;
/// bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4
pub const POOL_SCRIPT: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
pub const POOL_ADDRESS: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

pub struct Primed {
    child: Child,
    stats: u16,
    pub dir: std::path::PathBuf,
}

impl Drop for Primed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Start a Prime. `node` is the `rpc`/`poll` part of its config, and anything else a test sets.
pub fn start_primed(pool: &Identity, node: &str) -> (Primed, u16) {
    start_primed_seeded(pool, node, |_| {})
}

/// [`start_primed`], with a chance to put files in the data directory before the Prime opens it.
pub fn start_primed_seeded(pool: &Identity, node: &str, seed: impl FnOnce(&std::path::Path)) -> (Primed, u16) {
    let dir = std::env::temp_dir().join(format!("primed-replay-{}-{}", std::process::id(), free_port()));
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);
    std::fs::write(dir.join("prime.key"), format!("{}\n", hex::encode(pool.secret_bytes()))).unwrap();
    let listen = free_port();
    let stats = free_port();
    let cfg = format!(
        r#"
listen = "127.0.0.1:{listen}"
stats-listen = "127.0.0.1:{stats}"
data-dir = "{dir}"
payout-address = "{POOL_ADDRESS}"
fee-bps = 50
min-diff = 1
{node}
"#,
        dir = dir.display()
    );
    let cfg_path = dir.join("prime.toml");
    std::fs::write(&cfg_path, cfg).unwrap();
    // PRIMED_BIN points the attack at another build (e.g. a deployed or pre-fix binary).
    let bin = std::env::var("PRIMED_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_primed").to_string());
    let child = Command::new(bin)
        .arg("-c")
        .arg(&cfg_path)
        .arg("run")
        .env("RUST_LOG", "info,primed::session=debug")
        .stdout(Stdio::from(std::fs::File::create(dir.join("primed.log")).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(dir.join("primed.err")).unwrap()))
        .spawn()
        .expect("spawn primed");
    let p = Primed { child, stats, dir };
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if TcpStream::connect(("127.0.0.1", listen)).is_ok() && TcpStream::connect(("127.0.0.1", p.stats)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "primed did not start: {}",
            std::fs::read_to_string(p.dir.join("primed.err")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    (p, listen)
}

pub fn stats(p: &Primed) -> serde_json::Value {
    let mut s = TcpStream::connect(("127.0.0.1", p.stats)).unwrap();
    s.write_all(b"GET /stats.json HTTP/1.0\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str(body).unwrap()
}

/// Stats that reflect everything the Prime has done up to now: the HTTP endpoint serves one
/// built document for half a second, so a read straight after a share can be the one from
/// before it.
pub fn settled_stats(p: &Primed) -> serde_json::Value {
    std::thread::sleep(Duration::from_millis(700));
    stats(p)
}

/// A gateway-side DATUM session.
pub struct Gateway {
    stream: TcpStream,
    send_keys: KeyStream,
    recv_keys: KeyStream,
    channel: Channel,
}

impl Gateway {
    pub fn connect(port: u16, pool: &Identity, identity: &Identity) -> Gateway {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let session = Identity::generate();
        let seed = 0x0badc0deu32;
        let hello =
            handshake::build_client_hello(&pool.box_pk(), identity, &session, "replay-test/0.1", seed, &[0u8; 16]);
        let mut initial = KeyStream(CLIENT_INITIAL_KEY);
        let mut h = Header::new(cmd::HELLO, hello.len());
        h.sealed = true;
        h.signed = true;
        let mut out = h.encode(&mut initial).to_vec();
        out.extend_from_slice(&hello);
        stream.write_all(&out).unwrap();

        // the server's (recv, send) is our (send, recv)
        let (send_keys, mut recv_keys) = KeyStream::from_seed(seed);
        let mut hb = [0u8; Header::SIZE];
        stream.read_exact(&mut hb).unwrap();
        let rh = Header::decode(hb, &mut recv_keys).unwrap();
        assert_eq!(rh.cmd, cmd::HELLO_REPLY);
        let mut payload = vec![0u8; rh.len as usize];
        stream.read_exact(&mut payload).unwrap();
        let (_srv_sign, srv_box, motd) = handshake::parse_server_hello(&pool.sign_pk(), &session, &payload).unwrap();
        assert_eq!(motd, "Lazarus");
        let (recv_nonce, send_nonce) = crypto::session_nonces(seed, &session.sign_pk());
        let channel = Channel::new(session.precompute(&srv_box), send_nonce, recv_nonce);
        let mut g = Gateway { stream, send_keys, recv_keys, channel };
        // configure arrives first; consume it so the channel nonces stay in step
        let cfg = g.next_mining();
        assert_eq!(cfg[0], mining::SUB_CONFIGURE, "first mining message is the configure");
        g
    }

    pub fn send_mining(&mut self, plain: &[u8]) {
        let payload = self.channel.encrypt(plain);
        let mut h = Header::new(cmd::MINING, payload.len());
        h.channel = true;
        let mut out = h.encode(&mut self.send_keys).to_vec();
        out.extend_from_slice(&payload);
        self.stream.write_all(&out).unwrap();
    }

    /// Next decrypted mining body (signature stripped), skipping keepalives.
    pub fn next_mining(&mut self) -> Vec<u8> {
        loop {
            let mut hb = [0u8; Header::SIZE];
            self.stream.read_exact(&mut hb).expect("read header");
            let h = Header::decode(hb, &mut self.recv_keys).unwrap();
            let mut payload = vec![0u8; h.len as usize];
            if h.len > 0 {
                self.stream.read_exact(&mut payload).unwrap();
            }
            if h.cmd != cmd::MINING {
                continue;
            }
            assert!(h.channel);
            let body = self.channel.decrypt_in_place(&mut payload).unwrap();
            let body = if h.signed { &body[..body.len() - crypto::SIG] } else { &body[..] };
            return body.to_vec();
        }
    }

    /// Ask for a coinbaser for a template worth `value` on `prev_hash`; returns the id of the
    /// split the Prime issued.
    pub fn request_coinbaser(&mut self, value: u64, prev_hash: &Hash) -> u8 {
        let mut m = vec![mining::SUB_COINBASER_REQUEST];
        m.extend_from_slice(&value.to_le_bytes());
        m.extend_from_slice(prev_hash);
        m.push(mining::END);
        self.send_mining(&m);
        loop {
            let m = self.next_mining();
            if m[0] == mining::SUB_COINBASER_REPLY {
                assert_eq!(u64::from_le_bytes(m[1..9].try_into().unwrap()), value);
                return m[13];
            }
        }
    }

    /// Submit and return `(status, reject_code)`.
    pub fn submit(&mut self, s: &PowSubmit) -> (u8, u16) {
        self.send_mining(&s.encode());
        loop {
            let m = self.next_mining();
            if m[0] == mining::SUB_SHARE_RECEIPT {
                assert_eq!(m[9], s.job_id);
                return (m[1], u16::from_le_bytes([m[2], m[3]]));
            }
        }
    }
}

pub fn pool_only_share(
    slot: u8,
    height: u32,
    prev_hash: Hash,
    nbits: u32,
    txn_total_weight: u32,
    now: u32,
) -> PowSubmit {
    let outs = vec![TxOut { value: VALUE, script: hex::decode(POOL_SCRIPT).unwrap() }];
    let (cb, tidx, split_at) = coinbase::build(height, b"Lazarus", &outs, 0);
    let coinb1 = cb[..split_at].to_vec();
    let coinb2 = cb[split_at + coinbase::EXTRANONCE_SLOT..].to_vec();
    let txs: Vec<Hash> = (0..3u64)
        .map(|i| {
            let mut a = [0u8; 32];
            a[..8].copy_from_slice(&(i + 1).to_le_bytes());
            a
        })
        .collect();
    PowSubmit {
        job_id: slot,
        coinbase_id: 0,
        flags: mining::FLAG_BLAKE2B,
        target_pot: 0,
        ntime32: 0,
        nonce32: 0,
        version: 0xa000_0000,
        extranonce: [0x0b, 0x10, 0xc0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8],
        username: "bc1qminer.rig".into(),
        reserved: [0; 4],
        blake2b: Some(Blake2bSection { ntime: [0; 8], nonce: [0; 8] }),
        time_on_wire: Some(now),
        job: Some(JobSection {
            prev_hash,
            target_byte_index: tidx as u16,
            nbits: nbits.to_le_bytes(),
            coinbaser_id: 0,
            height,
            coinbase_value: VALUE,
            txn_count: txs.len() as u32,
            txn_total_weight,
            txn_total_size: 0,
            txn_total_sigops: 0,
            merkle_branches: pow::merkle_branches_for_coinbase(&txs),
        }),
        coinbase: Some(CoinbaseSection { coinbase_id: 0, coinb1, coinb2 }),
    }
}

/// Grind a real difficulty-1 share on every core.
pub fn grind_diff1(s: &mut PowSubmit) {
    let mut slot = JobSlot::default();
    slot.absorb(s).unwrap();
    let jw = Arc::new(job_work_for(&slot, s, false).unwrap());
    let target = pow::share_target_le(0).unwrap();
    let ntime = s.blake2b.as_ref().unwrap().ntime;
    let found = Arc::new(AtomicBool::new(false));
    let tried = Arc::new(AtomicU64::new(0));
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as u32;
    let started = Instant::now();
    let mut handles = Vec::new();
    for t in 0..threads {
        let jw = jw.clone();
        let found = found.clone();
        let tried = tried.clone();
        handles.push(std::thread::spawn(move || -> Option<[u8; 8]> {
            let mut nonce = [0u8; 8];
            nonce[4..].copy_from_slice(&t.to_le_bytes());
            let mut n = 0u32;
            loop {
                if n.is_multiple_of(65536) {
                    if found.load(Ordering::Relaxed) {
                        return None;
                    }
                    tried.fetch_add(65536, Ordering::Relaxed);
                }
                nonce[..4].copy_from_slice(&n.to_le_bytes());
                if pow::meets_target(&jw.hash(&nonce, &ntime), &target) {
                    found.store(true, Ordering::Relaxed);
                    return Some(nonce);
                }
                n = n.wrapping_add(1);
                if n == 0 {
                    return None;
                }
            }
        }));
    }
    let reporter = {
        let found = found.clone();
        let tried = tried.clone();
        std::thread::spawn(move || {
            while !found.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(10));
                let h = tried.load(Ordering::Relaxed);
                eprintln!(
                    "grinding: {:.2} GH tried in {}s ({:.1} MH/s; diff-1 needs ~4.29 GH on average)",
                    h as f64 / 1e9,
                    started.elapsed().as_secs(),
                    h as f64 / started.elapsed().as_secs_f64() / 1e6
                );
            }
        })
    };
    let mut nonce = None;
    for h in handles {
        if let Some(n) = h.join().unwrap() {
            nonce = Some(n);
        }
    }
    let _ = reporter.join();
    let nonce = nonce.expect("some thread found a share");
    s.blake2b.as_mut().unwrap().nonce = nonce;
    s.nonce32 = u32::from_le_bytes(nonce[..4].try_into().unwrap());
    eprintln!("found a diff-1 share in {}s", started.elapsed().as_secs());
}

/// What the stand-in node says its chain is. Tests change it under the lock.
#[derive(Clone)]
pub struct Chain {
    pub height: u32,
    /// Tip and its parent, as the node prints them (big-endian hex).
    pub tip: String,
    pub parent: String,
    pub bits: u32,
    /// `getmininginfo.next.bits`; `None` plays a node that predates the field.
    pub next_bits: Option<u32>,
}

/// A JSON-RPC endpoint that answers the calls the tip check makes and counts them.
pub struct MockNode {
    pub port: u16,
    pub chain: Arc<std::sync::Mutex<Chain>>,
    pub calls: Arc<AtomicU64>,
    /// What `getblockheader` says of a block that is not the tip: its `confirmations`
    /// (negative: not in the main chain). A hash not listed is one the node has never seen.
    pub confirmations: Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>>,
}

impl MockNode {
    pub fn start(chain: Chain) -> MockNode {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let chain = Arc::new(std::sync::Mutex::new(chain));
        let calls = Arc::new(AtomicU64::new(0));
        let confirmations: Arc<std::sync::Mutex<std::collections::HashMap<String, i64>>> = Default::default();
        let (c, n, confs) = (chain.clone(), calls.clone(), confirmations.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let (c, n, confs) = (c.clone(), n.clone(), confs.clone());
                std::thread::spawn(move || {
                    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 4096];
                    let body = loop {
                        let Ok(k) = conn.read(&mut tmp) else { return };
                        if k == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..k]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        let Some(at) = text.find("\r\n\r\n") else { continue };
                        let len = text[..at]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buf.len() >= at + 4 + len {
                            break text[at + 4..at + 4 + len].to_string();
                        }
                    };
                    let req: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let ch = c.lock().unwrap().clone();
                    let result = match req["method"].as_str().unwrap_or("") {
                        "getblockchaininfo" => {
                            n.fetch_add(1, Ordering::Relaxed);
                            serde_json::json!({"chain": "main", "blocks": ch.height, "bestblockhash": ch.tip, "difficulty": 1.0})
                        }
                        "getblockheader" => {
                            let asked = req["params"][0].as_str().unwrap_or("").to_string();
                            if asked == ch.tip {
                                serde_json::json!({"previousblockhash": ch.parent, "bits": format!("{:08x}", ch.bits), "height": ch.height, "confirmations": 1})
                            } else {
                                match confs.lock().unwrap().get(&asked) {
                                    Some(n) => serde_json::json!({"confirmations": n}),
                                    None => serde_json::json!({"__error": -5}),
                                }
                            }
                        }
                        "getmininginfo" => match ch.next_bits {
                            Some(b) => serde_json::json!({"blocks": ch.height, "next": {"bits": format!("{b:08x}")}}),
                            None => serde_json::json!({"blocks": ch.height}),
                        },
                        _ => serde_json::Value::Null,
                    };
                    let out = if let Some(code) = result.get("__error") {
                        serde_json::json!({"result": null, "error": {"code": code, "message": "Block not found"}, "id": req["id"]})
                    } else if result.is_null() {
                        serde_json::json!({"result": null, "error": {"code": -32601, "message": "Method not found"}, "id": req["id"]})
                    } else {
                        serde_json::json!({"result": result, "error": null, "id": req["id"]})
                    }
                    .to_string();
                    let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", out.len(), out);
                });
            }
        });
        MockNode { port, chain, calls, confirmations }
    }

    /// The `rpc` part of a Prime config pointing at this node.
    pub fn config(&self, poll: f64) -> String {
        format!("rpc = \"http://127.0.0.1:{}\"\nrpc-user = \"u\"\nrpc-password = \"p\"\npoll = {poll}", self.port)
    }
}

/// A block hash as the node prints it, for the wire-order bytes a job section carries.
pub fn node_hex(wire: &Hash) -> String {
    let mut h = *wire;
    h.reverse();
    hex::encode(h)
}
