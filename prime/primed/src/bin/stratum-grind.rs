//! Fast CPU stratum miner for BLAKE2b header-v2 (regtest / e2e). Same work the
//! Python cpu-miner builds; release-mode cores finish a diff-1 share in tens of seconds.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use datum_wire::pow::{self, Hash};
use serde_json::{json, Value};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut host = "127.0.0.1".to_string();
    let mut port = 19334u16;
    let mut user = String::new();
    let mut skip_empty = false;
    let mut skip_n = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--host" => host = args.next().expect("--host value"),
            "--port" => port = args.next().expect("--port value").parse().unwrap(),
            "--user" => user = args.next().expect("--user value"),
            "--skip-empty" => skip_empty = true,
            "--skip-n" => skip_n = true,
            _ => {}
        }
    }
    if user.is_empty() {
        eprintln!("usage: stratum-grind --host HOST --port PORT --user ADDRESS [--skip-empty|--skip-n]");
        std::process::exit(2);
    }

    let stream = TcpStream::connect((host.as_str(), port)).expect("stratum connect");
    stream.set_nodelay(true).ok();
    let mut writer = stream.try_clone().unwrap();
    send(&mut writer, json!({"id":1,"method":"mining.subscribe","params":["stratum-grind/0.1"]}));
    send(&mut writer, json!({"id":2,"method":"mining.authorize","params":[user, "x"]}));
    send(&mut writer, json!({"id":3,"method":"mining.suggest_difficulty","params":[1]}));

    let current_job = Arc::new(Mutex::new(None::<Job>));
    let (hit_tx, hit_rx) = mpsc::channel::<Hit>();
    let stop = Arc::new(AtomicBool::new(false));
    let hashes = Arc::new(AtomicU64::new(0));
    let nproc = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    for i in 0..nproc {
        let jobs = current_job.clone();
        let hit_tx = hit_tx.clone();
        let stop = stop.clone();
        let hashes = hashes.clone();
        thread::spawn(move || worker(i as u64, nproc as u64, jobs, hit_tx, stop, hashes));
    }

    let writer = Arc::new(Mutex::new(writer));
    let w2 = writer.clone();
    let user_c = user.clone();
    thread::spawn(move || {
        for hit in hit_rx {
            let mut w = w2.lock().unwrap();
            send(
                &mut *w,
                json!({
                    "id": 100,
                    "method": "mining.submit",
                    "params": [user_c, hit.job_id, hex::encode(hit.en2), hex::encode(hit.ntime), hex::encode(hit.nonce)]
                }),
            );
            println!("submit job={} nonce={}", hit.job_id, hex::encode(hit.nonce));
            let _ = std::io::stdout().flush();
        }
    });

    let reader = BufReader::new(stream);
    let t0 = Instant::now();
    let mut last = t0;
    let mut current: Option<Job> = None;
    for line in reader.lines() {
        let line = line.expect("stratum read");
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        if v.get("method").and_then(|m| m.as_str()) == Some("mining.notify") {
            if let Some(job) = parse_notify(&v) {
                if (skip_empty && is_empty_cbselect(&job.id)) || (skip_n && is_new_block_job(&job.id)) {
                    println!("skip {} {}", if skip_empty { "empty" } else { "n" }, job.id);
                    continue;
                }
                println!("job {} coinb1={}b", job.id, job.coinb1.len());
                let _ = std::io::stdout().flush();
                current = Some(job.clone());
                *current_job.lock().unwrap() = Some(job);
            }
        } else if v.get("method").and_then(|m| m.as_str()) == Some("mining.set_difficulty") {
            println!("difficulty {}", v["params"][0]);
        } else if v.get("result").is_some() && v.get("id").and_then(|i| i.as_u64()) == Some(1) {
            if let Some(en1) = parse_en1(&v) {
                println!("subscribed en1={}", hex::encode(&en1));
                SUB_EN1.lock().unwrap().replace(en1);
            }
        }
        if last.elapsed().as_secs() >= 10 {
            let h = hashes.load(Ordering::Relaxed);
            let s = t0.elapsed().as_secs_f64().max(0.001);
            println!("{:.2} MH/s hashes={h} job={}", h as f64 / s / 1e6, current.as_ref().map(|j| j.id.as_str()).unwrap_or("-"));
            let _ = std::io::stdout().flush();
            last = Instant::now();
        }
    }
    stop.store(true, Ordering::Relaxed);
}

static SUB_EN1: Mutex<Option<Vec<u8>>> = Mutex::new(None);

#[derive(Clone)]
struct Job {
    id: String,
    sia_prev: Hash,
    ntime: [u8; 8],
    coinb1: Vec<u8>,
    target: Hash,
}

struct Hit {
    job_id: String,
    en2: [u8; 8],
    ntime: [u8; 8],
    nonce: [u8; 8],
}

/// Stock DATUM appends the coinbase class as two hex digits. `00` is the
/// pool-only "empty" class; `ff` / a leading `N` is subsidy-only empty work.
fn is_empty_cbselect(id: &str) -> bool {
    let rest = id.strip_prefix('N').or_else(|| id.strip_prefix('Q')).unwrap_or(id);
    rest.len() >= 2 && matches!(&rest[rest.len() - 2..], "00" | "ff" | "FF")
}

/// Stock `new_block` work: subsidy-only coinbase (`N…` / `…ff`). On a full
/// template Prime classifies that as Foreign; type `00` is the late-full path.
fn is_new_block_job(id: &str) -> bool {
    id.starts_with('N') || id.ends_with("ff") || id.ends_with("FF")
}

fn parse_en1(v: &Value) -> Option<Vec<u8>> {
    let result = v.get("result")?;
    let arr = result.as_array()?;
    // [[["mining.notify", "..."]], "en1hex", extraNonce2Size]
    let en = arr.get(1)?.as_str()?;
    hex::decode(en).ok()
}

fn parse_notify(v: &Value) -> Option<Job> {
    let p = v.get("params")?.as_array()?;
    let id = p.first()?.as_str()?.to_string();
    let prev = hex::decode(p.get(1)?.as_str()?).ok()?;
    let coinb1 = hex::decode(p.get(2)?.as_str()?).ok()?;
    if coinb1.len() != 39 {
        return None;
    }
    let ntime_hex = p.get(7)?.as_str()?;
    let mut ntime = [0u8; 8];
    if ntime_hex.len() == 16 {
        let raw = hex::decode(ntime_hex).ok()?;
        ntime.copy_from_slice(&raw);
    } else {
        let v = u32::from_str_radix(ntime_hex, 16).ok()?;
        ntime[..4].copy_from_slice(&v.to_be_bytes());
    }
    let mut prev_arr = [0u8; 32];
    if prev.len() != 32 {
        return None;
    }
    prev_arr.copy_from_slice(&prev);
    let sia_prev = if prev_arr[..6] == [0; 6] { prev_arr } else { pow::sia_prevhash(&prev_arr) };
    Some(Job { id, sia_prev, ntime, coinb1, target: pow::share_target_le(0).unwrap() })
}

fn send(w: &mut TcpStream, v: Value) {
    writeln!(w, "{v}").ok();
    w.flush().ok();
}

fn worker(
    idx: u64,
    stride: u64,
    jobs: Arc<Mutex<Option<Job>>>,
    hit_tx: mpsc::Sender<Hit>,
    stop: Arc<AtomicBool>,
    hashes: Arc<AtomicU64>,
) {
    let mut job = loop {
        if let Some(j) = jobs.lock().unwrap().clone() {
            break j;
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut en1 = SUB_EN1.lock().unwrap().clone().unwrap_or_default();
    let mut en2 = [0u8; 8];
    en2[..8].copy_from_slice(&(idx.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_le_bytes());
    let mut root = root_for(&job.coinb1, &en1, &en2);
    let mut nonce = idx;
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if nonce.wrapping_sub(idx) % (stride * 8192) == 0 {
            if let Some(j) = jobs.lock().unwrap().clone() {
                if j.id != job.id {
                    job = j;
                    nonce = idx;
                    en1 = SUB_EN1.lock().unwrap().clone().unwrap_or_default();
                    root = root_for(&job.coinb1, &en1, &en2);
                }
            }
        }
        let n8 = nonce.to_le_bytes();
        let work = pow::work_header(&job.sia_prev, &n8, &job.ntime, &root);
        let hash = pow::pow_hash_le(&work, &[0u8; 16], 0);
        if nonce.wrapping_sub(idx) % (stride * 4096) == 0 {
            hashes.fetch_add(4096, Ordering::Relaxed);
        }
        if pow::meets_target(&hash, &job.target) {
            let _ = hit_tx.send(Hit { job_id: job.id.clone(), en2, ntime: job.ntime, nonce: n8 });
        }
        nonce = nonce.wrapping_add(stride);
    }
}

fn root_for(coinb1: &[u8], en1: &[u8], en2: &[u8; 8]) -> Hash {
    let mut extra = [0u8; 12];
    let mut buf = en1.to_vec();
    buf.extend_from_slice(en2);
    let n = buf.len().min(12);
    extra[..n].copy_from_slice(&buf[..n]);
    let mut leaf = [0u8; 52];
    leaf[1..40].copy_from_slice(coinb1);
    leaf[40..].copy_from_slice(&extra);
    pow::blake2b256(&leaf)
}
