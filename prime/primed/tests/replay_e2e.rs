//! End-to-end: a hostile gateway against a real `primed`, replaying one genuine share.
//!
//! This is the loop from the September 2026 review (finding #1): submit a valid share, then
//! resubmit it with a job section that differs in a field nothing reads, then again from a
//! fresh connection. Before the fix each resubmission emptied the per-job dedup set and was
//! credited again. Now the pool credits it once and rejects every replay as duplicate work.
//!
//! The share is real diff-1 work — about 2^32 BLAKE2b hashes, a minute or two on a desktop
//! across all cores — so the test is ignored by default:
//!
//!     cargo test --release -p primed --test replay_e2e -- --ignored --nocapture

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use datum_wire::coinbase::{self, TxOut};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake;
use datum_wire::mining::{self, Blake2bSection, CoinbaseSection, JobSection, PowSubmit};
use datum_wire::pow::{self, Hash};
use datum_wire::verify::{job_work_for, JobSlot};
use datum_wire::cmd;

const HEIGHT: u32 = 966_267;
const VALUE: u64 = 312_538_966;
/// bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4
const POOL_SCRIPT: &str = "0014751e76e8199196d454941c45d1b3a323f1433bd6";
const POOL_ADDRESS: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

struct Primed {
    child: Child,
    stats: u16,
    dir: std::path::PathBuf,
}

impl Drop for Primed {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_primed(pool: &Identity) -> (Primed, u16) {
    let dir = std::env::temp_dir().join(format!("primed-replay-{}-{}", std::process::id(), free_port()));
    std::fs::create_dir_all(&dir).unwrap();
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
# nothing listens here: the poller warns and shares are accepted without a staleness check
rpc = "http://127.0.0.1:9"
poll = 5.0
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
        if TcpStream::connect(("127.0.0.1", listen)).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "primed did not start: {}", std::fs::read_to_string(p.dir.join("primed.err")).unwrap_or_default());
        std::thread::sleep(Duration::from_millis(100));
    }
    (p, listen)
}

fn stats(p: &Primed) -> serde_json::Value {
    let mut s = TcpStream::connect(("127.0.0.1", p.stats)).unwrap();
    s.write_all(b"GET /stats.json HTTP/1.0\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);
    let body = text.split("\r\n\r\n").nth(1).unwrap();
    serde_json::from_str(body).unwrap()
}

/// A gateway-side DATUM session.
struct Gateway {
    stream: TcpStream,
    send_keys: KeyStream,
    recv_keys: KeyStream,
    channel: Channel,
}

impl Gateway {
    fn connect(port: u16, pool: &Identity, identity: &Identity) -> Gateway {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let session = Identity::generate();
        let seed = 0x0badc0deu32;
        let hello = handshake::build_client_hello(&pool.box_pk(), identity, &session, "replay-test/0.1", seed, &[0u8; 16]);
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

    fn send_mining(&mut self, plain: &[u8]) {
        let payload = self.channel.encrypt(plain);
        let mut h = Header::new(cmd::MINING, payload.len());
        h.channel = true;
        let mut out = h.encode(&mut self.send_keys).to_vec();
        out.extend_from_slice(&payload);
        self.stream.write_all(&out).unwrap();
    }

    /// Next decrypted mining body (signature stripped), skipping keepalives.
    fn next_mining(&mut self) -> Vec<u8> {
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

    /// Submit and return `(status, reject_code)`.
    fn submit(&mut self, s: &PowSubmit) -> (u8, u16) {
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

fn pool_only_share(slot: u8, txn_total_weight: u32, now: u32) -> PowSubmit {
    let outs = vec![TxOut { value: VALUE, script: hex::decode(POOL_SCRIPT).unwrap() }];
    let (cb, tidx) = coinbase::build(HEIGHT, b"Lazarus", &outs, 0);
    let split_at = tidx + 1;
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
            prev_hash: [0x77; 32],
            target_byte_index: tidx as u16,
            nbits: 0x193c_2d40u32.to_le_bytes(),
            coinbaser_id: 0,
            height: HEIGHT,
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
fn grind_diff1(s: &mut PowSubmit) {
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

#[test]
#[ignore]
fn one_share_is_credited_once_no_matter_how_it_is_resubmitted() {
    let pool = Identity::generate();
    let gw_identity = Identity::generate();
    let (primed, port) = start_primed(&pool);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32;

    let mut share = pool_only_share(3, 0, now);
    grind_diff1(&mut share);

    let mut gw = Gateway::connect(port, &pool, &gw_identity);
    let (status, code) = gw.submit(&share);
    assert!(
        status == mining::ACCEPTED || status == mining::ACCEPTED_TENTATIVELY,
        "genuine share accepted (status 0x{status:02x}, code {code})"
    );

    // The finding's loop: alternate two job sections differing only in txn_total_weight,
    // resend the same share each time. Every one must be a duplicate now.
    for i in 0..10u32 {
        let mut again = share.clone();
        again.job.as_mut().unwrap().txn_total_weight = if i % 2 == 0 { 4_000_000 } else { 0 };
        let (status, code) = gw.submit(&again);
        assert_eq!((status, code), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK), "replay #{i} via job-section flip");
    }
    // Same share, no sections at all (cached job)
    let mut bare = share.clone();
    bare.job = None;
    bare.coinbase = None;
    assert_eq!(gw.submit(&bare), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK));
    // Same share on a different job slot
    let mut other_slot = share.clone();
    other_slot.job_id = 7;
    assert_eq!(gw.submit(&other_slot), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK));
    drop(gw);

    // A reconnect (fresh session, same or different gateway key) is still a duplicate.
    std::thread::sleep(Duration::from_millis(200));
    let mut gw2 = Gateway::connect(port, &pool, &gw_identity);
    assert_eq!(gw2.submit(&share), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK), "replay after reconnect");
    let mut gw3 = Gateway::connect(port, &pool, &Identity::generate());
    assert_eq!(gw3.submit(&share), (mining::REJECTED, mining::REJECT_DUPLICATE_WORK), "replay from another key");

    let st = stats(&primed);
    assert_eq!(st["totals"]["shares_accepted"], 1, "{st}");
    assert_eq!(st["totals"]["work_accepted"], 1);
    assert_eq!(st["totals"]["shares_rejected"], 14);
    assert_eq!(st["totals"]["seen_shares"], 1);
    assert_eq!(st["window"]["work"], 1);
    eprintln!("credited work = {} after 1 genuine share and 14 replays", st["window"]["work"]);
}
