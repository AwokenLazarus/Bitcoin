use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, ToSocketAddrs};
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
    /// First `mining.set_difficulty` for a new session. Defaults to `vardiff_min`.
    /// ASIC stratum starts at 4096 (inside typical box firmware range). A 19–140 TH/s
    /// rig still climbs in a few seconds: eight shares trip a 4× step, so it does
    /// not sit at the start long enough to flood.
    #[serde(default)]
    vardiff_start: Option<u64>,
    /// Hard cap. ASIC default in config is 131072 (~140 TH/s at 4s/share).
    /// Unset means no extra cap beyond the 2^40 exponent limit.
    #[serde(default)]
    vardiff_max: Option<u64>,
    rpc: String,
    rpc_cookie: PathBuf,
    prime_host: String,
    /// 0 disables the Prime link entirely: the gateway builds its own coinbase and submits
    /// its own blocks. Only solo mode can do without Prime; pooled mode needs it for the
    /// TIDES split.
    prime_port: u16,
    #[serde(default)]
    pool_pubkey: Option<String>,
    coinbase_tag: Option<String>,
    /// off | log | enforce: whether a share that misses its vardiff target is rejected
    /// or only counted.
    verify_shares: Option<String>,
    /// `pooled` (default) or `solo`. Solo pays the miner who found the block, less the fee.
    #[serde(default)]
    mode: Option<String>,
    /// Solo: pool fee in basis points, taken out of every block this gateway finds.
    #[serde(default)]
    solo_fee_bps: Option<u32>,
    /// Solo: address the fee output pays. Required in solo mode.
    #[serde(default)]
    solo_fee_address: Option<String>,
    /// Solo: where the found-block log and the stats book are kept.
    #[serde(default)]
    solo_data_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Pooled,
    Solo,
}

/// Unset or unrecognised means pooled: a typo must not silently turn a pooled stratum
/// into one that pays whoever connects.
fn run_mode(s: Option<&str>) -> Mode {
    match s.unwrap_or("pooled") {
        "solo" => Mode::Solo,
        _ => Mode::Pooled,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VerifyMode {
    Off,
    Log,
    Enforce,
}

/// Unset or unrecognised means enforce. A missing key or a typo must not quietly turn
/// share validation off on a public stratum.
fn verify_mode(s: Option<&str>) -> VerifyMode {
    match s.unwrap_or("enforce") {
        "off" => VerifyMode::Off,
        "log" => VerifyMode::Log,
        _ => VerifyMode::Enforce,
    }
}

/// Lock, recovering the data if another thread panicked while holding it. Every shared
/// lock here is `lock().unwrap()`-style; a single poisoned mutex must not take down every
/// other miner's thread.
fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---- limits on what an anonymous stratum client can make us hold or do ----

/// Longest Stratum line we will buffer. Real submits are ~200 bytes; `read_line` with no
/// cap lets one client grow a String until the allocator fails.
const MAX_LINE: u64 = 8 * 1024;
/// Username bound. Prime's decoder rejects anything over 384 bytes anyway, and every
/// forwarded share carries a copy, so long names only amplify.
const MAX_USER: usize = 128;
/// Longest hex field accepted in `mining.submit` (8 bytes).
const MAX_HEX_FIELD: usize = 16;
/// Per-connection share hashes remembered for duplicate detection.
const SEEN_CAP: usize = 8192;
/// Rolling-window samples kept per miner; pruned by time as well.
const RECENT_CAP: usize = 4096;
/// Submit token bucket: refill per second, burst. A 140 TH/s rig at the start difficulty
/// submits ~8/s; anything sustaining more than 50/s is not mining.
const SUBMIT_RATE: f64 = 50.0;
const SUBMIT_BURST: f64 = 200.0;
/// Rate-limited or malformed submits before the connection is dropped.
const FLOOD_LIMIT: u64 = 1000;
/// Idle miners are disconnected after this long without a line; a half-open connection
/// otherwise keeps its thread and `Miner` entry forever.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// A client that stops reading gets this long before its socket write fails instead of
/// blocking the job broadcast.
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CONNS: usize = 2048;
const MAX_CONNS_PER_IP: usize = 32;

/// Printable ASCII, no whitespace or control bytes: nothing that can break Prime's NUL
/// terminated wire framing or forge log lines.
fn clean_user(u: &str) -> bool {
    !u.is_empty() && u.len() <= MAX_USER && u.bytes().all(|b| (0x21..0x7f).contains(&b))
}
/// Attacker-supplied text for a log line.
fn short(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_graphic()).take(48).collect()
}
/// Decode a submit hex field into an 8-byte little-endian slot; wrong-length or
/// oversized fields are refused rather than zero-padded.
fn hex_field(v: Option<&Value>) -> Option<[u8; 8]> {
    let s = v?.as_str()?;
    if s.is_empty() || s.len() > MAX_HEX_FIELD || !s.len().is_multiple_of(2) {
        return None;
    }
    let b = hex::decode(s).ok()?;
    let mut out = [0u8; 8];
    out[..b.len()].copy_from_slice(&b);
    Some(out)
}

/// Open connections, total and per source IP.
struct Conns {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}
impl Conns {
    fn try_acquire(&mut self, ip: IpAddr) -> bool {
        let n = self.per_ip.get(&ip).copied().unwrap_or(0);
        if self.total >= MAX_CONNS || n >= MAX_CONNS_PER_IP {
            return false;
        }
        self.total += 1;
        self.per_ip.insert(ip, n + 1);
        true
    }
    fn release(&mut self, ip: IpAddr) {
        self.total = self.total.saturating_sub(1);
        if let Some(n) = self.per_ip.get_mut(&ip) {
            *n -= 1;
            if *n == 0 {
                self.per_ip.remove(&ip);
            }
        }
    }
}
/// Releases the connection slot and the session's map entries even if the handler panics.
struct SessionGuard {
    st: Arc<Shared>,
    id: u64,
    ip: IpAddr,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        lk(&self.st.miners).remove(&self.id);
        lk(&self.st.miner_socks).remove(&self.id);
        lk(&self.st.conns).release(self.ip);
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
    /// `acc_n` at the last retarget, so a share flood can raise difficulty
    /// before `VARDIFF_INTERVAL` elapses.
    retarget_acc_n: u64,
    /// (job id, difficulty in force when that job was sent to this miner).
    ///
    /// A share has to be judged and paid at the difficulty the miner was working under,
    /// which is the one attached to its job, not whatever the session has been retargeted
    /// to since. Crediting the difficulty a hash happens to reach instead overpays by a
    /// factor of (1 + log2(assigned/actual)/2) whenever the two drift apart.
    job_diffs: VecDeque<(String, u64)>,
    /// (when, work) for a rolling hashrate window. Lifetime acc stays in `acc`.
    recent: VecDeque<(Instant, u64)>,
    /// Pow hashes already credited on this connection. The hash commits every miner
    /// controlled field, so a replay is exactly a repeated hash.
    seen: HashSet<[u8; 32]>,
    seen_order: VecDeque<[u8; 32]>,
    /// Submit token bucket and the count of refused/malformed submits.
    tokens: f64,
    tokens_at: Instant,
    flood: u64,
    /// Solo only: this session's own jobs, newest last. Every identity gets a different
    /// coinbase, so a submitted job id has to be resolved against the session that was
    /// handed it rather than a gateway-wide history.
    jobs: VecDeque<Arc<Job>>,
    /// Canonical payout identity (`user` up to the first `.`, bech32 folded to lowercase).
    ident: String,
}
/// Solo jobs remembered per session. Templates are republished every `JOB_REFRESH`, so
/// this is several minutes of history: long enough for any miner's round trip.
const SOLO_JOBS: usize = 32;
/// Returns false if this hash was already credited on this connection.
fn note_share(m: &mut Miner, hash: [u8; 32]) -> bool {
    if !m.seen.insert(hash) {
        return false;
    }
    m.seen_order.push_back(hash);
    while m.seen_order.len() > SEEN_CAP {
        if let Some(old) = m.seen_order.pop_front() {
            m.seen.remove(&old);
        }
    }
    true
}
/// Take one submit token; false when the client is over its rate.
fn take_token(m: &mut Miner) -> bool {
    let now = Instant::now();
    let dt = now.duration_since(m.tokens_at).as_secs_f64();
    m.tokens_at = now;
    m.tokens = (m.tokens + dt * SUBMIT_RATE).min(SUBMIT_BURST);
    if m.tokens >= 1.0 {
        m.tokens -= 1.0;
        true
    } else {
        false
    }
}
const HR_WINDOW: Duration = Duration::from_secs(60);
/// Aim for roughly one share per miner every few seconds. Left at difficulty 1 a single
/// 1 TH/s rig submits over 200 shares a second, which no pool can account for and which
/// buys nothing: the same hashrate is measured just as well from far fewer shares.
const VARDIFF_TARGET_SECS: f64 = 4.0;
const VARDIFF_INTERVAL: Duration = Duration::from_secs(20);
/// Raise difficulty as soon as this many shares land since the last retarget.
/// Stops a 19 TH/s unit at a too-low start from flooding (and getting kicked by
/// MRR) while we wait 20s for the clock.
const VARDIFF_QUICK_SHARES: u64 = 8;
const VARDIFF_GRACE: Duration = Duration::from_secs(30);
/// Estimate hashrate over at least the target share interval. A 50ms burst of
/// eight shares at the start difficulty is not 200 TH/s.
const VARDIFF_DT_MIN: f64 = VARDIFF_TARGET_SECS;
/// Never jump more than 4× (two powers of two) in one retarget.
const VARDIFF_STEP: u64 = 4;
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
fn pow2_clamp(n: u64, floor: u64, cap: u64) -> u64 {
    let floor = floor.max(1);
    let cap = cap.max(floor);
    let n = n.clamp(floor, cap);
    let pot = n.max(1).ilog2().min(40);
    (1u64 << pot).clamp(floor, cap)
}
fn vardiff_ideal(hs: f64) -> u64 {
    if !hs.is_finite() || hs <= 0.0 {
        return 1;
    }
    let ideal = hs * VARDIFF_TARGET_SECS / 4_294_967_296.0;
    let pot = ideal.max(1.0).log2().round().clamp(0.0, 40.0) as u32;
    1u64 << pot
}
/// Move toward `ideal`, but only 4× per step and stay inside [floor, cap].
fn step_vardiff(current: u64, ideal: u64, floor: u64, cap: u64) -> u64 {
    let cur = pow2_clamp(current, floor, cap);
    let want = pow2_clamp(ideal, floor, cap);
    if want > cur {
        pow2_clamp(cur.saturating_mul(VARDIFF_STEP).min(want), floor, cap)
    } else if want < cur {
        pow2_clamp((cur / VARDIFF_STEP).max(want).max(1), floor, cap)
    } else {
        cur
    }
}
fn miner_hs_window(m: &Miner, dt_floor: f64) -> f64 {
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
    let dt = first.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0).max(dt_floor);
    (work as f64) * ((1u64 << 32) as f64) / dt
}
fn miner_hs(m: &Miner) -> f64 {
    miner_hs_window(m, 5.0)
}
fn miner_hs_vardiff(m: &Miner) -> f64 {
    miner_hs_window(m, VARDIFF_DT_MIN)
}
fn should_retarget(m: &Miner) -> bool {
    m.last_retarget.elapsed() >= VARDIFF_INTERVAL
        || m.acc_n.saturating_sub(m.retarget_acc_n) >= VARDIFF_QUICK_SHARES
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
    while m.recent.len() > RECENT_CAP {
        m.recent.pop_front();
    }
}

/// Everything a `getblocktemplate` gives us that does not depend on who is paid: the
/// block body, the merkle path for a coinbase and the header fields.
///
/// Held behind an `Arc` and shared by every job built on it. Solo mode gives each miner
/// its own coinbase, so it builds one job per identity per template; without this split
/// each of those jobs would own its own copy of the block's transactions (~300 KB), and
/// a few hundred connected miners would be gigabytes.
struct Template {
    height: u32,
    value: u64,
    prev_notify: String,
    nbits: [u8; 4],
    version: i32,
    curtime: u32,
    /// Sibling path for the coinbase, so Prime can recheck the merkle root.
    branches: Vec<[u8; 32]>,
    txids: Vec<[u8; 32]>,
    tx_hexes: Vec<Vec<u8>>,
    /// Transactions plus the coinbase.
    txn_count: u32,
    witness_commit: Option<Vec<u8>>,
    tag: String,
    /// Template weight limit and the weight of `tx_hexes`, so a coinbase can be checked
    /// against them before its job is published.
    weightlimit: Option<u64>,
    tx_weight: u64,
    prev_block: [u8; 32],
}

/// One template plus one coinbase: what a miner is actually handed.
struct Job {
    id: String,
    tpl: Arc<Template>,
    header: HeaderV2,
    /// Legacy coinbase bytes, sent with every share so Prime can audit the split.
    coinb1: Vec<u8>,
    cb: CoinbaserV2,
    job_id: u8,
    /// Solo only: the identity this coinbase pays. Pooled jobs pay the whole window.
    who: Option<String>,
}

impl Job {
    fn height(&self) -> u32 {
        self.tpl.height
    }
    fn value(&self) -> u64 {
        self.tpl.value
    }
    fn nbits(&self) -> [u8; 4] {
        self.tpl.nbits
    }
    fn txn_count(&self) -> u32 {
        self.tpl.txn_count
    }
    fn outputs(&self) -> usize {
        self.cb.outputs.len()
    }
    fn branches(&self) -> &[[u8; 32]] {
        &self.tpl.branches
    }
    /// Published as zero; the miner is free to roll these bytes and they are hashed.
    fn ntime(&self) -> String {
        hex::encode([0u8; 8])
    }
}

// ---- solo mining ----

/// Fee in sats. Rounds down, so the rounding sat goes to the miner, never to us.
fn fee_for(value: u64, bps: u32) -> u64 {
    ((u128::from(value) * u128::from(bps)) / 10_000) as u64
}

/// The solo coinbase: the miner who finds the block takes it all bar the fee.
///
/// The miner's output is first so a block explorer (and our own scanner) reads the finder
/// off the coinbase without knowing the pool's address.
fn solo_split(value: u64, miner_script: &[u8], fee_script: &[u8], fee_bps: u32, id: u8) -> Option<CoinbaserV2> {
    let fee = fee_for(value, fee_bps);
    let mine = value.checked_sub(fee)?;
    // Neither side may be empty: a zero output is unspendable clutter, and a zero fee
    // means we published a job that pays us nothing, which is a misconfiguration rather
    // than a discount.
    if mine == 0 || fee == 0 || miner_script.len() < 2 || fee_script.len() < 2 {
        return None;
    }
    if miner_script.len() > 64 || fee_script.len() > 64 {
        return None;
    }
    let cb = CoinbaserV2 {
        id,
        outputs: vec![
            lazarus_protocol::coinbaser::CoinbaserOutput { sats: mine, script: miner_script.to_vec() },
            lazarus_protocol::coinbaser::CoinbaserOutput { sats: fee, script: fee_script.to_vec() },
        ],
    };
    // The node rejects a coinbase paying more than the template allows, so this is the
    // one invariant worth asserting before a job goes out.
    if cb.value_sum() != value {
        return None;
    }
    Some(cb)
}

/// Payout identity for stats and for the coinbase script. Bech32 is case-insensitive, so
/// `BC1Q…` and `bc1q…` are one miner; anything else is left alone (base58 is not).
fn canon_identity(user: &str) -> String {
    let ident = identity_of(user);
    if ident.len() > 4 && ident[..3].eq_ignore_ascii_case("bc1") {
        ident.to_ascii_lowercase()
    } else {
        ident
    }
}

/// Difficulty this hash actually reached, as a power of two. Difficulty 2^pot needs
/// 32 + pot leading zero bits, so the achieved exponent is the surplus over 32.
fn hash_pot(hash: &[u8; 32]) -> u8 {
    let mut zeros = 0u32;
    for b in hash {
        if *b == 0 {
            zeros += 8;
        } else {
            zeros += b.leading_zeros();
            break;
        }
    }
    zeros.saturating_sub(32).min(255) as u8
}

#[derive(Clone, Default, serde::Serialize, Deserialize)]
struct SoloMiner {
    /// Difficulty-1 shares credited, lifetime.
    work: u64,
    shares: u64,
    last_ts: u64,
    /// Highest difficulty this identity has ever hit, as a power of two.
    best_pot: u8,
    blocks: u32,
}

#[derive(Clone, serde::Serialize, Deserialize)]
struct SoloBlock {
    height: u32,
    hash: String,
    who: String,
    ts: u64,
    value: u64,
    fee: u64,
}

/// Lifetime solo accounting. Payout is settled inside the block's own coinbase, so none of
/// this is owed to anyone: it exists so the pool UI can show what a solo miner is doing.
#[derive(Default, serde::Serialize, Deserialize)]
struct SoloBook {
    miners: HashMap<String, SoloMiner>,
    blocks: VecDeque<SoloBlock>,
    work: u64,
    shares: u64,
    #[serde(skip)]
    dirty: bool,
}
/// Identities kept in the book. Well past any real farm; the least recently seen are
/// dropped first so an address-cycling client cannot grow it without bound.
const SOLO_BOOK_MINERS: usize = 4096;
/// Blocks kept in memory for the API. `solo-blocks.jsonl` is the full record.
const SOLO_BOOK_BLOCKS: usize = 256;

impl SoloBook {
    fn credit(&mut self, ident: &str, work: u64, pot: u8) {
        self.work = self.work.saturating_add(work);
        self.shares += 1;
        self.dirty = true;
        let e = self.miners.entry(ident.to_string()).or_default();
        e.work = e.work.saturating_add(work);
        e.shares += 1;
        e.last_ts = unix_now();
        e.best_pot = e.best_pot.max(pot);
        if self.miners.len() > SOLO_BOOK_MINERS {
            self.evict();
        }
    }
    fn evict(&mut self) {
        // Keep the most recently seen. `blocks > 0` is never dropped: a solo miner who
        // found a block stays on the board.
        let mut by_seen: Vec<(String, u64)> = self
            .miners
            .iter()
            .filter(|(_, m)| m.blocks == 0)
            .map(|(k, m)| (k.clone(), m.last_ts))
            .collect();
        by_seen.sort_by_key(|(_, ts)| *ts);
        let over = self.miners.len().saturating_sub(SOLO_BOOK_MINERS);
        for (k, _) in by_seen.into_iter().take(over) {
            self.miners.remove(&k);
        }
    }
    fn note_block(&mut self, b: SoloBlock) {
        if let Some(m) = self.miners.get_mut(&b.who) {
            m.blocks += 1;
        }
        self.blocks.push_back(b);
        while self.blocks.len() > SOLO_BOOK_BLOCKS {
            self.blocks.pop_front();
        }
        self.dirty = true;
    }
}

fn solo_path(st: &Shared, name: &str) -> Option<PathBuf> {
    st.solo_dir.as_ref().map(|d| d.join(name))
}

/// Append-only record of every block this gateway has assembled, with what the node said
/// about it. The book is a cache; this file is the evidence.
fn append_solo_block(st: &Shared, b: &SoloBlock, outcome: &str) {
    let Some(p) = solo_path(st, "solo-blocks.jsonl") else { return };
    let line = json!({
        "height": b.height, "hash": b.hash, "who": b.who, "ts": b.ts,
        "value": b.value, "fee": b.fee, "fee_bps": st.solo_fee_bps, "outcome": outcome,
    });
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = wrote {
        log::warn!("solo block log {}: {e}", p.display());
    }
}

fn save_solo_book(st: &Shared) {
    let Some(p) = solo_path(st, "solo-book.json") else { return };
    let body = {
        let mut g = lk(&st.solo);
        g.dirty = false;
        serde_json::to_string(&*g).unwrap_or_default()
    };
    if body.is_empty() {
        return;
    }
    // Rewrite atomically: a truncated book read at the next start would lose every
    // identity's lifetime work, and a solo miner's best share is the one number they care
    // about between blocks.
    let tmp = p.with_extension("json.tmp");
    let done = std::fs::write(&tmp, body.as_bytes()).and_then(|_| std::fs::rename(&tmp, &p));
    if let Err(e) = done {
        log::warn!("solo book {}: {e}", p.display());
    }
}

fn load_solo_book(dir: Option<&PathBuf>) -> SoloBook {
    let Some(d) = dir else { return SoloBook::default() };
    let p = d.join("solo-book.json");
    match std::fs::read_to_string(&p) {
        Ok(s) => match serde_json::from_str::<SoloBook>(&s) {
            Ok(b) => {
                log::info!("solo book loaded: {} identities, {} blocks", b.miners.len(), b.blocks.len());
                b
            }
            Err(e) => {
                log::warn!("solo book {} unreadable ({e}); starting empty", p.display());
                SoloBook::default()
            }
        },
        Err(_) => SoloBook::default(),
    }
}

/// Flush the book on a timer. Every share touches it, so writing per share would put a
/// file write in the submit path.
fn solo_save_loop(st: Arc<Shared>) {
    loop {
        thread::sleep(Duration::from_secs(60));
        if lk(&st.solo).dirty {
            save_solo_book(&st);
        }
    }
}

/// This identity's job on the current template. Every solo miner gets its own coinbase, so
/// its own merkle root and its own job.
fn solo_job_for(st: &Shared, ident: &str) -> Option<Arc<Job>> {
    let script = identity_script(ident)?;
    let tpl = lk(&st.tpl).clone()?;
    let seq = st.job_seq.fetch_add(1, Ordering::Relaxed);
    let slot = (seq % 255) as u8 + 1;
    let cb = solo_split(tpl.value, &script, &st.solo_fee_script, st.solo_fee_bps, slot)?;
    coinbase_job(&tpl, &st.extra1, cb, slot, seq, Some(ident.to_string())).map(Arc::new)
}

/// Hand every authorised miner its own job for a freshly published template.
///
/// Sockets are written with no lock held, as in `broadcast`: one client that has stopped
/// reading must not stall the template loop or anybody else's submit.
fn broadcast_solo(st: &Shared) {
    let targets: Vec<(u64, String)> = lk(&st.miners)
        .iter()
        .filter(|(_, m)| !m.ident.is_empty())
        .map(|(id, m)| (*id, m.ident.clone()))
        .collect();
    if targets.is_empty() {
        return;
    }
    let socks: HashMap<u64, TcpStream> = {
        let g = lk(&st.miner_socks);
        targets.iter().filter_map(|(id, _)| g.get(id).and_then(|s| s.try_clone().ok()).map(|c| (*id, c))).collect()
    };
    // One coinbase and one merkle root per distinct identity, not per connection: several
    // workers on one address share a job (their extranonce1 still differs, so their work
    // does not overlap).
    let mut built: HashMap<String, Arc<Job>> = HashMap::new();
    let mut sent: Vec<(u64, Arc<Job>)> = Vec::new();
    let mut dead: Vec<u64> = Vec::new();
    for (id, ident) in targets {
        let Some(mut sock) = socks.get(&id).and_then(|s| s.try_clone().ok()) else { continue };
        let job = match built.get(&ident) {
            Some(j) => j.clone(),
            None => {
                let Some(j) = solo_job_for(st, &ident) else { continue };
                built.insert(ident.clone(), j.clone());
                j
            }
        };
        if sock.write_all(notify_line(&job).as_bytes()).is_ok() {
            sent.push((id, job));
        } else {
            dead.push(id);
        }
    }
    {
        // miners before miner_socks, everywhere, or these two deadlock.
        let mut miners = lk(&st.miners);
        for (id, job) in sent {
            if let Some(m) = miners.get_mut(&id) {
                assign_job(m, &job.id);
                m.jobs.push_back(job);
                while m.jobs.len() > SOLO_JOBS {
                    m.jobs.pop_front();
                }
            }
        }
    }
    if !dead.is_empty() {
        let mut socks = lk(&st.miner_socks);
        for d in dead {
            socks.remove(&d);
        }
    }
}

struct Shared {
    cfg: GwCfg,
    mode: Mode,
    /// Newest template. Solo mode builds a job from it whenever a miner authorises, so a
    /// session that arrives mid-template does not wait for the next one.
    tpl: Mutex<Option<Arc<Template>>>,
    /// Solo: output script the fee is paid to, and the rate.
    solo_fee_script: Vec<u8>,
    solo_fee_bps: u32,
    solo: Mutex<SoloBook>,
    /// Solo: directory holding `solo-blocks.jsonl` and `solo-book.json`.
    solo_dir: Option<PathBuf>,
    /// Jobs are shared by reference: each carries the whole block body, and cloning it
    /// per `mining.submit` under the history lock was a multi-MB memcpy any client
    /// could trigger with a 100-byte line.
    job: Mutex<Option<Arc<Job>>>,
    /// Recently published jobs, newest last. A miner submits against the job it was
    /// handed, which is often no longer the current one.
    jobs: Mutex<VecDeque<Arc<Job>>>,
    conns: Mutex<Conns>,
    /// Block hashes already handed to `submitblock`, so a replayed solve is not re-sent.
    submitted: Mutex<HashSet<[u8; 32]>>,
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
    /// Solo: the job the last accepted share was built on. There is no single current job
    /// in solo mode, so this is what `/audit` reports.
    last_job: Mutex<Option<Arc<Job>>>,
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
        j.id.clone(), j.tpl.prev_notify.clone(), format!("000000{}00000000", hex::encode(h2)),
        "", [], "", hex::encode(j.nbits()), j.ntime(), true
    ]}))
}
fn broadcast(st: &Shared, line: &str, job_id: &str) {
    // Snapshot the sockets, then write with no lock held. Writing under `miners` and
    // `miner_socks` meant one client that stopped reading stalled the template loop and
    // every other miner's submit for the TCP retransmit lifetime (the sockets also carry
    // a write timeout now, as a second line of defence).
    let socks: Vec<(u64, TcpStream)> = lk(&st.miner_socks)
        .iter()
        .filter_map(|(id, s)| s.try_clone().ok().map(|c| (*id, c)))
        .collect();
    let mut ok = Vec::with_capacity(socks.len());
    let mut dead = Vec::new();
    for (mid, mut s) in socks {
        if s.write_all(line.as_bytes()).is_ok() { ok.push(mid) } else { dead.push(mid) }
    }
    {
        // miners before miner_socks, everywhere, or these two deadlock.
        let mut miners = lk(&st.miners);
        for mid in ok {
            if let Some(m) = miners.get_mut(&mid) {
                assign_job(m, job_id);
            }
        }
    }
    if !dead.is_empty() {
        let mut socks = lk(&st.miner_socks);
        for d in dead { socks.remove(&d); }
    }
}
fn send_prime(st: &Shared, body: Vec<u8>) {
    if st.prime_depth.load(Ordering::Relaxed) >= PRIME_QUEUE_CAP {
        let n = st.prime_dropped.fetch_add(1, Ordering::Relaxed);
        if n % 10_000 == 0 {
            log::warn!("Prime queue full; dropped {} shares so far", n + 1);
        }
        return;
    }
    if let Some(tx) = st.prime_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
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
    if let Some(tx) = st.prime_urgent.lock().unwrap_or_else(|e| e.into_inner()).as_ref() { let _ = tx.send(body); }
}
fn kick_gbt(st: &Shared) {
    *st.gbt_due.lock().unwrap_or_else(|e| e.into_inner()) = true;
    st.gbt_kick.notify_all();
}
fn wait_coinbaser(st: &Shared, value: u64, deadline: Instant) -> Option<CoinbaserV2> {
    let mut g = st.last_cb.lock().unwrap_or_else(|e| e.into_inner());
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
    let g = st.last_cb.lock().unwrap_or_else(|e| e.into_inner());
    let (_, cb) = g.as_ref()?;
    if cb.outputs.len() < 2 { return None; }
    Some(cb.scale_to(value))
}
/// Everything in a `getblocktemplate` that does not depend on the coinbase.
fn build_template(tpl: &Value, tag: &str) -> Option<Template> {
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
    // A template whose txids and bodies do not line up would give a block whose body
    // does not match its merkle root. Refuse it rather than publish it.
    if merkle.len() != txs.len() || tx_hexes.len() != txs.len() || txs.len() + 1 > u16::MAX as usize {
        log::warn!("template txid/data mismatch: txs={} txids={} bodies={}", txs.len(), merkle.len(), tx_hexes.len());
        return None;
    }
    let wit = tpl.get("default_witness_commitment").and_then(|x| x.as_str()).and_then(|h| hex::decode(h).ok());
    Some(Template {
        height,
        value,
        prev_notify: hex::encode(pow::prevblock_hidden(&prev)),
        nbits: bits,
        version,
        curtime,
        branches: pow::merkle_branches_for_coinbase(&merkle),
        txids: merkle,
        tx_hexes,
        txn_count: txs.len() as u32 + 1,
        witness_commit: wit,
        tag: tag.to_string(),
        weightlimit: tpl.get("weightlimit").and_then(|x| x.as_u64()),
        tx_weight: txs.iter().filter_map(|t| t.get("weight").and_then(|w| w.as_u64())).sum(),
        prev_block: prev,
    })
}

/// Job ids must be unique, including across restarts. Two jobs sharing an id cannot be
/// told apart at submission time, and a counter alone restarts at zero: a miner still
/// holding a pre-restart id would then be matched to an unrelated template. The high half
/// is a per-process salt from extranonce1, the low half the counter. Solo mode issues one
/// job per identity per template, so the counter half is 32 bits wide.
fn job_id_str(extra1: &[u8; 4], seq: u64) -> String {
    format!("{:04x}{:08x}", u16::from_le_bytes([extra1[0], extra1[1]]), seq & 0xffff_ffff)
}

/// Bind one coinbase to one template. `who` is set in solo mode: the identity this
/// coinbase pays.
fn coinbase_job(
    tpl: &Arc<Template>,
    extra1: &[u8; 4],
    cb: CoinbaserV2,
    job_id: u8,
    seq: u64,
    who: Option<String>,
) -> Option<Job> {
    let mut extra = extra1.to_vec(); extra.extend_from_slice(&[0u8; 8]);
    let cbleg = cbtx::coinbase_legacy(tpl.height, &tpl.tag, &extra, &cb, tpl.witness_commit.as_deref());
    // The node picked `transactions` assuming a modest coinbase. Prime's split can be up to
    // 512 outputs, so check the assembled block against the template's own weight limit
    // rather than publish a job whose solve would be bad-blk-weight.
    if let Some(limit) = tpl.weightlimit {
        let cbwit = cbtx::coinbase_witness(tpl.height, &tpl.tag, &extra, &cb, tpl.witness_commit.as_deref());
        let cb_weight = 3 * cbleg.len() as u64 + cbwit.len() as u64;
        let total = 4 * 80 + 4 * 9 + cb_weight + tpl.tx_weight;
        if total > limit {
            log::warn!("block would weigh {total} > weightlimit {limit} (coinbase {cb_weight}, {} outputs); not publishing", cb.outputs.len());
            return None;
        }
    }
    let mut hdr = HeaderV2::default();
    hdr.version = tpl.version; hdr.prev_block = tpl.prev_block; hdr.time = tpl.curtime;
    hdr.bits = u32::from_le_bytes(tpl.nbits); hdr.height = tpl.height as i32;
    hdr.txcount = tpl.txn_count as u16;
    hdr.merkle_root = pow::merkle_root_sha256d(&cbleg, &tpl.txids);
    hdr.extranonce = pow::header_extranonce(extra1);
    Some(Job {
        id: job_id_str(extra1, seq),
        tpl: tpl.clone(),
        header: hdr,
        coinb1: cbleg,
        cb,
        job_id,
        who,
    })
}
fn connect_prime(cfg: &GwCfg) -> Option<(TcpStream, lazarus_protocol::ChannelKeys, lazarus_protocol::SessionKeys)> {
    let pk = hex::decode(cfg.pool_pubkey.as_deref().unwrap_or("").trim()).ok()?;
    if pk.len() != 64 { log::error!("pool_pubkey must be 128 hex chars"); return None; }
    let mut pool_x = [0u8; 32]; pool_x.copy_from_slice(&pk[32..64]);
    let local = generate_pool_keys(); let sess = generate_session();
    let (hello, _nk, mut ch) =
        handshake::encode_client_hello(&local, &sess, &pool_x, handshake::SPLIT_GATEWAY_UA).ok()?;
    // Bounded connect and handshake: a Prime (or middlebox) that accepts TCP and goes
    // quiet would otherwise hang this loop forever, with every share dropped meanwhile.
    let addr = (cfg.prime_host.as_str(), cfg.prime_port).to_socket_addrs().ok()?.next()?;
    let mut sock = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).ok()?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(10)));
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
                                *st.last_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some((value, cb));
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
fn handle_miner(mut sock: TcpStream, st: Arc<Shared>, ip: IpAddr) {
    let host = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    let host_label = host.clone();
    let id = st.next_id.fetch_add(1, Ordering::Relaxed);
    // From here on the slot and map entries are released by the guard, panic or not.
    let _guard = SessionGuard { st: st.clone(), id, ip };
    let _ = sock.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = sock.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(rdr_sock) = sock.try_clone().ok() else { return };
    let mut rdr = BufReader::new(rdr_sock);
    let mut user = String::new(); let mut ua = String::new();
    let vmin = st.cfg.vardiff_min.max(1);
    let vmax = st.cfg.vardiff_max.unwrap_or(1u64 << 40).max(vmin);
    let vstart = pow2_clamp(st.cfg.vardiff_start.unwrap_or(vmin), vmin, vmax);
    // Every session gets its own extranonce1. Sharing one across the gateway makes
    // identical rigs walk identical (extranonce2, nonce) pairs, so they submit the same
    // shares and the dedupe keeps only whichever arrived first, quietly moving credit
    // from one miner to another.
    let sess_en1 = (u32::from_le_bytes(st.extra1) ^ (id as u32)).to_le_bytes();
    let now = Instant::now();
    st.miners.lock().unwrap_or_else(|e| e.into_inner()).insert(id, Miner { host, user: String::new(), ua: String::new(), vdiff: vstart, acc: 0, acc_n: 0, rej: 0, rej_n: 0, last: now, vdiff_prev: vstart, vdiff_prev_until: now,
        // First retarget after a handful of shares (quickdiff) or ~4s, not a full
        // 20s at the start value.
        last_retarget: now.checked_sub(VARDIFF_INTERVAL - Duration::from_secs(4)).unwrap_or(now),
        retarget_acc_n: 0,
        recent: VecDeque::new(), job_diffs: VecDeque::new(),
        seen: HashSet::new(), seen_order: VecDeque::new(),
        tokens: SUBMIT_BURST, tokens_at: now, flood: 0,
        jobs: VecDeque::new(), ident: String::new() });
    let mut line = String::new();
    loop {
        line.clear();
        // Bounded read: a client streaming bytes with no newline gets cut off at
        // MAX_LINE instead of growing this String until the allocator gives up.
        let n = match (&mut rdr).take(MAX_LINE + 1).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => n as u64,
        };
        if n > MAX_LINE || !line.ends_with('\n') {
            log::warn!("{host_label}: line too long or unterminated; dropping connection");
            break;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            // Garbage counts toward the flood budget too; a valid miner never sends it.
            let over = lk(&st.miners).get_mut(&id).map(|m| { m.flood += 1; m.flood > FLOOD_LIMIT }).unwrap_or(true);
            if over { break; }
            continue;
        };
        let method = msg.get("method").and_then(|x| x.as_str()).unwrap_or("");
        let mid = msg.get("id").cloned().unwrap_or(Value::Null);
        match method {
            "mining.subscribe" => {
                if let Some(u) = msg.get("params").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()) {
                    ua = u.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(96).collect();
                }
                if let Ok(c) = sock.try_clone() { lk(&st.miner_socks).insert(id, c); }
                send_line(&mut sock, &json!({"id": mid, "result": [[["mining.notify", "lz"]], hex::encode(sess_en1), 8], "error": null}));
                let d0 = lk(&st.miners).get(&id).map(|m| m.vdiff).unwrap_or(vmin);
                send_line(&mut sock, &json!({"id": null, "method": "mining.set_difficulty", "params": [d0]}));
                // Solo has nothing to send yet: the coinbase pays whoever authorises, so
                // the first job waits for `mining.authorize`. Every firmware we have seen
                // authorises in the same round trip.
                if st.mode == Mode::Pooled {
                    let first_job = lk(&st.job).clone();
                    if let Some(j) = first_job {
                        if j.outputs() >= 2 {
                            let _ = write!(sock, "{}", notify_line(&j));
                            if let Some(m) = lk(&st.miners).get_mut(&id) {
                                assign_job(m, &j.id);
                            }
                        }
                    }
                }
            }
            "mining.authorize" => {
                let raw = msg.get("params").and_then(|p| p.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("");
                let ident = canon_identity(raw);
                let ok = clean_user(raw) && identity_script(&ident).is_some();
                if ok { user = raw.to_string(); } else { user.clear(); }
                send_line(&mut sock, &json!({"id": mid, "result": ok, "error": if ok { Value::Null } else { json!([14, "BadUsername", null]) }}));
                if let Some(m) = lk(&st.miners).get_mut(&id) {
                    m.user = user.clone();
                    m.ua = ua.clone();
                    m.ident = if ok { ident.clone() } else { String::new() };
                }
                // Solo: this address now has a coinbase, so it can have a job. Re-authorising
                // costs one coinbase and one merkle root, and counts toward the flood budget
                // so it cannot be used as a work generator.
                if ok && st.mode == Mode::Solo {
                    if let Some(m) = lk(&st.miners).get_mut(&id) { m.flood += 1; }
                    if let Some(j) = solo_job_for(&st, &ident) {
                        if sock.write_all(notify_line(&j).as_bytes()).is_ok() {
                            if let Some(m) = lk(&st.miners).get_mut(&id) {
                                assign_job(m, &j.id);
                                m.jobs.push_back(j);
                                while m.jobs.len() > SOLO_JOBS { m.jobs.pop_front(); }
                            }
                        }
                    } else {
                        log::warn!("{host_label}: no solo job for {} (no template yet?)", short(&ident));
                    }
                }
            }
            "mining.submit" => {
                // Rate limit first: over budget, the line costs us nothing further.
                let (allowed, over) = match lk(&st.miners).get_mut(&id) {
                    Some(m) => {
                        let ok = take_token(m);
                        if !ok { m.flood += 1; }
                        (ok, m.flood > FLOOD_LIMIT)
                    }
                    None => (false, true),
                };
                if over {
                    log::warn!("{host_label}: submit flood; dropping connection");
                    break;
                }
                if !allowed {
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([25, "RateLimited", null])}));
                    continue;
                }
                if user.is_empty() {
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([24, "Unauthorized", null])}));
                    continue;
                }
                let params = msg.get("params").and_then(|p| p.as_array()).cloned().unwrap_or_default();
                // Rebuild against the job the miner actually hashed. Jobs republish about
                // once a second, so reaching for the current job instead rebuilds a stale
                // share against the wrong template: it fails its own target, and a real
                // solve would be assembled into a block nobody solved.
                let want = params.get(1).and_then(|x| x.as_str()).unwrap_or("").to_string();
                // Solo jobs are per identity, so a job id only means something to the
                // session it was handed to; pooled jobs are gateway-wide.
                let job: Option<Arc<Job>> = if want.len() > 16 {
                    None
                } else if st.mode == Mode::Solo {
                    lk(&st.miners).get(&id).and_then(|m| m.jobs.iter().rev().find(|j| j.id == want).cloned())
                } else {
                    lk(&st.jobs).iter().rev().find(|j| j.id == want).cloned()
                };
                let Some(j) = job else {
                    let n = st.job_miss.fetch_add(1, Ordering::Relaxed);
                    if n < 30 || n % 200 == 0 {
                        if st.mode == Mode::Solo {
                            let held = lk(&st.miners).get(&id).map(|m| m.jobs.len()).unwrap_or(0);
                            log::warn!("stale solo job want={} held={} user={}", short(&want), held, short(&user));
                        } else {
                            let hist = lk(&st.jobs);
                            let oldest = hist.front().map(|q| q.id.as_str()).unwrap_or("-");
                            let newest = hist.back().map(|q| q.id.as_str()).unwrap_or("-");
                            log::warn!("stale job want={} held={} range={}..{}", short(&want), hist.len(), oldest, newest);
                        }
                    }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([21, "StaleJob", null])}));
                    continue;
                };
                if j.outputs() < 2 || params.len() < 5 {
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([21, "UnsplitJob", null])})); continue;
                }
                let (Some(en2), Some(sia_t), Some(sia_n)) = (hex_field(params.get(2)), hex_field(params.get(3)), hex_field(params.get(4))) else {
                    if let Some(m) = lk(&st.miners).get_mut(&id) { m.flood += 1; }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([20, "BadParams", null])}));
                    continue;
                };
                let mut hdr = j.header.clone();
                hdr.nonce = u32::from_le_bytes(sia_n[0..4].try_into().unwrap_or([0; 4]));
                hdr.nonce2 = u32::from_le_bytes(sia_n.get(4..8).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]));
                let mut en12 = Vec::from(sess_en1);
                en12.extend_from_slice(&en2);
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
                    let g = st.miners.lock().unwrap_or_else(|e| e.into_inner());
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
                // A repeated hash is the same work submitted twice: refuse it before it is
                // counted, credited, forwarded to Prime or fed to the vardiff estimate.
                let fresh = lk(&st.miners).get_mut(&id).map(|m| note_share(m, hash)).unwrap_or(false);
                if !fresh {
                    st.rej.fetch_add(1, Ordering::Relaxed);
                    if let Some(m) = lk(&st.miners).get_mut(&id) { m.rej_n += 1; m.flood += 1; }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([22, "Duplicate", null])}));
                    continue;
                }
                *lk(&st.last_share_hdr) = Some(hdr.clone());
                *lk(&st.last_share_job) = Some(j.id.clone());
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
                    if let Some(m) = lk(&st.miners).get_mut(&id) {
                        m.rej = m.rej.saturating_add(credit);
                        m.rej_n += 1;
                    }
                    send_line(&mut sock, &json!({"id": mid, "result": false, "error": json!([23, "LowDifficultyShare", null])}));
                    continue;
                }
                if !share_ok {
                    // log / off: the miner is told yes and the miss is logged, but the share
                    // is not credited, forwarded to Prime (which would refuse it) or allowed
                    // to steer vardiff. Otherwise arbitrary bytes count as accepted work.
                    send_line(&mut sock, &json!({"id": mid, "result": true, "error": null}));
                    continue;
                }
                st.acc.fetch_add(1, Ordering::Relaxed);
                if st.mode == Mode::Solo {
                    lk(&st.solo).credit(&j.who.clone().unwrap_or_default(), credit, hash_pot(&hash));
                }
                let mut retarget = None;
                if let Some(m) = st.miners.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
                    record_share(m, credit);
                    if should_retarget(m) {
                        m.last_retarget = Instant::now();
                        m.retarget_acc_n = m.acc_n;
                        let want = step_vardiff(m.vdiff, vardiff_ideal(miner_hs_vardiff(m)), vmin, vmax);
                        if want != m.vdiff {
                            m.vdiff_prev = m.vdiff;
                            m.vdiff_prev_until = Instant::now() + VARDIFF_GRACE;
                            m.vdiff = want;
                            retarget = Some((m.vdiff_prev, want));
                        }
                    }
                }
                if let Some((from, d)) = retarget {
                    log::info!("vardiff user={} host={} {} -> {}", short(&user), host_label, from, d);
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
                        prev_hash: j.header.prev_block, target_byte_index: 0, nbits: j.nbits(),
                        coinbaser_id: j.cb.id, height: j.height(), coinbase_value: j.value(),
                        txn_count: j.txn_count(), txn_total_weight: 0, txn_total_size: 0, txn_total_sigops: 0,
                        merkle_branches: j.branches().to_vec(),
                    }),
                    coinbase: Some(mining::CoinbaseSection {
                        coinbase_id: j.cb.id,
                        coinb1: j.coinb1.clone(),
                        coinb2: Vec::new(),
                    }),
                    blake2b: Some(mining::Blake2bSection { sia_ntime: sia_t, sia_nonce: sia_n, time_on_wire: j.header.time }),
                };
                send_prime(&st, submit.encode());
                if st.mode == Mode::Solo {
                    *lk(&st.last_job) = Some(j.clone());
                }
                if let Some(tgt) = pow::bits_to_target(j.header.bits) {
                    let hit = pow::meets_target(&hash, &tgt);
                    if hit {
                        log::info!("share meets nbits height={} hash_hi={:02x}{:02x}{:02x}{:02x}", j.height(), hash[0], hash[1], hash[2], hash[3]);
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
    // map entries and the connection slot are released by `_guard`.
}
fn assemble_block(extra1: &[u8; 4], j: &Job, hdr: &HeaderV2) -> Vec<u8> {
    let mut extra = extra1.to_vec(); extra.extend_from_slice(&[0u8; 8]);
    let cbw = cbtx::coinbase_witness(j.height(), &j.tpl.tag, &extra, &j.cb, j.tpl.witness_commit.as_deref());
    let mut blk = hdr.serialize().to_vec();
    blk.extend_from_slice(&cbtx::compact(1 + j.tpl.tx_hexes.len() as u64));
    blk.extend_from_slice(&cbw);
    for tx in &j.tpl.tx_hexes { blk.extend_from_slice(tx); }
    blk
}
fn audit_json(st: &Shared) -> String {
    // Pooled has one current job. Solo has one per identity, so audit the job the last
    // accepted share was built on: a real miner's coinbase, assembled the way a solve
    // would be. With no shares yet, stand in the fee address for the miner so `block_hex`
    // is still a complete block this template would accept — that is what makes
    // `getblocktemplate {"mode":"proposal"}` a usable check on a quiet gateway.
    let job = match st.mode {
        Mode::Pooled => lk(&st.job).clone(),
        Mode::Solo => match lk(&st.last_job).clone() {
            Some(j) => Some(j),
            None => solo_job_for(st, st.cfg.solo_fee_address.as_deref().unwrap_or("").trim()),
        },
    };
    let Some(j) = job else { return json!({"error":"no job"}).to_string(); };
    let blk = assemble_block(&st.extra1, &j, &j.header);
    let outs: Vec<Value> = j.cb.outputs.iter().map(|o| json!({
        "sats": o.sats,
        "script": hex::encode(&o.script),
    })).collect();
    let share = {
        let hid = st.last_share_job.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let hdr = st.last_share_hdr.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match (hid, hdr) {
            (Some(id), Some(h)) if id == j.id => {
                let sblk = assemble_block(&st.extra1, &j, &h);
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
        "height": j.height(),
        "value": j.value(),
        "output_sum": j.cb.outputs.iter().map(|o| o.sats).sum::<u64>(),
        "outputs": j.outputs(),
        "tx_count": j.txn_count(),
        "block_bytes": blk.len(),
        "witness_commit": j.tpl.witness_commit.as_ref().map(|w| hex::encode(w)),
        "coinbase_outputs": outs,
        "block_hex": hex::encode(&blk),
        "share": share,
        "share_pow_ok": st.pow_ok.load(Ordering::Relaxed),
        "share_pow_bad": st.pow_bad.load(Ordering::Relaxed),
        "verify_mode": format!("{:?}", st.verify),
        "mode": format!("{:?}", st.mode),
        "payee": j.who.clone(),
        "fee_bps": st.solo_fee_bps,
        "prime_queue": st.prime_depth.load(Ordering::Relaxed),
        "prime_dropped": st.prime_dropped.load(Ordering::Relaxed),
        "job_miss": st.job_miss.load(Ordering::Relaxed),
        "template_age_s": unix_now().saturating_sub(st.last_pub_unix.load(Ordering::Relaxed)),
    }).to_string()
}
fn maybe_submit_block(st: &Shared, j: &Job, hdr: &HeaderV2) {
    let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else { return };
    {
        // Once per solve, whichever connection it arrives on.
        let mut seen = lk(&st.submitted);
        if !seen.insert(hdr.pow_hash()) {
            log::warn!("duplicate block solve height={} ignored", j.height());
            return;
        }
        if seen.len() > 1024 { seen.clear(); }
    }
    let blk = assemble_block(&st.extra1, j, hdr);
    log::info!("submitblock height={} outputs={} bytes={}", j.height(), j.outputs(), blk.len());
    let body = json!({"jsonrpc":"1.0","id":"sb","method":"submitblock","params":[hex::encode(&blk)]});
    let outcome = match minreq::post(&st.cfg.rpc).with_timeout(30).with_header("Authorization", &auth).with_header("Content-Type", "application/json").with_body(body.to_string()).send() {
        Ok(r) => {
            let txt = r.as_str().unwrap_or("");
            let v: Value = serde_json::from_str(txt).unwrap_or(Value::Null);
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                log::warn!("submitblock rpc error: {err}");
                format!("error:{err}")
            } else {
                match v.get("result") {
                    Some(Value::Null) | None => {
                        log::info!("submitblock accepted height={}", j.height());
                        "accepted".to_string()
                    }
                    Some(x) if x.as_str() == Some("inconclusive") => {
                        log::info!("submitblock inconclusive height={}", j.height());
                        "inconclusive".to_string()
                    }
                    Some(x) => {
                        log::warn!("submitblock result: {x}");
                        format!("rejected:{x}")
                    }
                }
            }
        }
        Err(e) => {
            log::warn!("submitblock http {e}");
            format!("http:{e}")
        }
    };
    // Solo: the payout is settled in this block's own coinbase, so the only record that
    // matters is who found it and what they were paid. Logged whatever the node said,
    // including a rejection: a solo miner is owed an explanation either way.
    if let Some(who) = j.who.clone() {
        let hash = hdr.pow_hash();
        let mut display = hash;
        display.reverse();
        let fee = j.cb.outputs.last().map(|o| o.sats).unwrap_or(0);
        let rec = SoloBlock {
            height: j.height(),
            hash: hex::encode(display),
            who,
            ts: unix_now(),
            value: j.value(),
            fee,
        };
        log::info!(
            "solo block height={} who={} value={} fee={} outcome={}",
            rec.height, short(&rec.who), rec.value, rec.fee, outcome
        );
        append_solo_block(st, &rec, &outcome);
        lk(&st.solo).note_block(rec);
        save_solo_book(st);
    }
}
fn api_loop(st: Arc<Shared>) {
    let Ok(lis) = TcpListener::bind(&st.cfg.api_listen) else { log::error!("api bind {}", st.cfg.api_listen); return; };
    log::info!("api {}", st.cfg.api_listen);
    // A handful of requests in flight at once; one stalled client must not freeze the
    // stats for everyone (the listener used to serve them one at a time with no timeouts).
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for s in lis.incoming() {
        let Ok(mut s) = s else { thread::sleep(Duration::from_millis(20)); continue };
        if inflight.fetch_add(1, Ordering::SeqCst) >= 8 {
            inflight.fetch_sub(1, Ordering::SeqCst);
            let _ = s.write_all(b"HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            continue;
        }
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
        let st = st.clone();
        let inflight2 = inflight.clone();
        let spawned = thread::Builder::new().stack_size(256 * 1024).spawn(move || {
            let inflight = inflight2;
            let mut buf = [0u8; 512]; let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (ctype, body) = if path.starts_with("/audit") {
                ("application/json", audit_json(&st))
            } else if path.starts_with("/solo.json") {
                ("application/json", solo_json(&st))
            } else if path.starts_with("/clients") {
                ("text/html", clients_html(&st))
            } else {
                ("text/html", home_html(&st))
            };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}", body.len());
            inflight.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() { inflight.fetch_sub(1, Ordering::SeqCst); }
    }
}
/// Solo accounting for the pool UI: who is mining solo here, what they have proved, and
/// which blocks they have found. Nothing here is owed — a solo payout is inside the block
/// the miner found — so this is a scoreboard, not a ledger.
fn solo_json(st: &Shared) -> String {
    let mut live: HashMap<String, (f64, u32)> = HashMap::new();
    for m in lk(&st.miners).values() {
        if m.ident.is_empty() {
            continue;
        }
        let e = live.entry(m.ident.clone()).or_insert((0.0, 0));
        e.0 += miner_hs(m);
        e.1 += 1;
    }
    let book = lk(&st.solo);
    let mut rows: Vec<Value> = Vec::with_capacity(book.miners.len());
    for (ident, m) in book.miners.iter() {
        let (hr, workers) = live.remove(ident).unwrap_or((0.0, 0));
        rows.push(json!({
            "identity": ident, "hashrate": hr, "workers": workers,
            "work": m.work, "shares": m.shares,
            "best_diff": 1u64 << m.best_pot.min(63), "blocks": m.blocks, "last_ts": m.last_ts,
        }));
    }
    // Connected but not yet credited a share: still worth showing, or a new miner looks
    // like it is not being seen at all.
    for (ident, (hr, workers)) in live {
        rows.push(json!({
            "identity": ident, "hashrate": hr, "workers": workers,
            "work": 0, "shares": 0, "best_diff": 0, "blocks": 0, "last_ts": 0,
        }));
    }
    let tpl = lk(&st.tpl).clone();
    json!({
        "mode": "solo",
        "profile": st.cfg.profile.clone().unwrap_or_default(),
        "fee_bps": st.solo_fee_bps,
        "fee_script": hex::encode(&st.solo_fee_script),
        "vardiff": {"min": st.cfg.vardiff_min, "start": st.cfg.vardiff_start.unwrap_or(st.cfg.vardiff_min), "max": st.cfg.vardiff_max},
        "height": tpl.as_ref().map(|t| t.height).unwrap_or(0),
        "value": tpl.as_ref().map(|t| t.value).unwrap_or(0),
        "template_age_s": unix_now().saturating_sub(st.last_pub_unix.load(Ordering::Relaxed)),
        "hashrate": rows.iter().filter_map(|r| r.get("hashrate").and_then(|v| v.as_f64())).sum::<f64>(),
        "work": book.work,
        "shares": book.shares,
        "shares_rejected": st.rej.load(Ordering::Relaxed),
        "miners": rows,
        "blocks": book.blocks.iter().rev().map(|b| json!({
            "height": b.height, "hash": b.hash, "who": b.who, "ts": b.ts,
            "value": b.value, "fee": b.fee,
        })).collect::<Vec<Value>>(),
    })
    .to_string()
}
fn home_html(st: &Shared) -> String {
    let acc = st.acc.load(Ordering::Relaxed); let rej = st.rej.load(Ordering::Relaxed);
    let outs = st.published_outputs.load(Ordering::Relaxed); let ph = st.published_height.load(Ordering::Relaxed);
    let hr: f64 = st.miners.lock().unwrap_or_else(|e| e.into_inner()).values().map(miner_hs).sum();
    format!("<html><body>Estimated Hashrate: {}<br>Local Shares Accepted: {}<br>Local Shares Rejected: {}<br>Coinbase outputs: {}<br>Published height: {}<br>Share PoW verified: {} / missed: {} (mode {:?})<br>Prime queue: {} (dropped {})</body></html>", parse_hr_label(hr), acc, rej, outs, ph, st.pow_ok.load(Ordering::Relaxed), st.pow_bad.load(Ordering::Relaxed), st.verify, st.prime_depth.load(Ordering::Relaxed), st.prime_dropped.load(Ordering::Relaxed))
}
fn clients_html(st: &Shared) -> String {
    let mut rows = String::from("<TABLE><TR><TD>#</TD><TD>Host</TD><TD>Auth Username</TD><TD></TD><TD>Last</TD><TD>VDiff</TD><TD>A</TD><TD>R</TD><TD>HR</TD><TD></TD><TD>UA</TD></TR>");
    for (i, m) in st.miners.lock().unwrap_or_else(|e| e.into_inner()).values().enumerate() {
        rows.push_str(&format!("<TR><TD>{}</TD><TD>{}</TD><TD>{}</TD><TD></TD><TD>{:.1} s</TD><TD>{}</TD><TD>{} ({})</TD><TD>{} ({})</TD><TD>{}</TD><TD></TD><TD>{}</TD></TR>",
            i, html_esc(&m.host), html_esc(&m.user), m.last.elapsed().as_secs_f64(), m.vdiff, m.acc, m.acc_n, m.rej, m.rej_n,
            parse_hr_label(miner_hs(m)), html_esc(&m.ua)));
    }
    rows.push_str("</TABLE>"); format!("<html><body>{rows}</body></html>")
}
fn gbt_loop(st: Arc<Shared>) {
    let tag = st.cfg.coinbase_tag.clone().unwrap_or_else(|| "Lazarus".into());
    let mut last_hash = String::new();
    let mut last_pub = Instant::now()
        .checked_sub(JOB_REFRESH)
        .unwrap_or_else(Instant::now);
    let mut cookie_missing_logged = false;
    loop {
        // Re-read the cookie every pass. bitcoind writes a fresh one on every restart, and a
        // gateway holding the old value gets 401s forever while its miners hash a stale job
        // (observed: 2.5 minutes of stale work across a node upgrade). The file is tiny.
        let Some(auth) = cookie_auth(&st.cfg.rpc_cookie) else {
            if !cookie_missing_logged {
                log::warn!("rpc cookie {} unreadable; node restarting? retrying", st.cfg.rpc_cookie.display());
                cookie_missing_logged = true;
            }
            std::thread::sleep(Duration::from_secs(2));
            continue;
        };
        cookie_missing_logged = false;
        let hash = rpc(&st.cfg.rpc, &auth, "getbestblockhash", json!([])).and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default();
        let new_tip = !hash.is_empty() && hash != last_hash;
        if new_tip { last_hash = hash.clone(); }
        let kicked = { let mut d = st.gbt_due.lock().unwrap_or_else(|e| e.into_inner()); let v = *d; *d = false; v };
        if new_tip || kicked || last_pub.elapsed() >= JOB_REFRESH {
            if let Some(tpl) = rpc(&st.cfg.rpc, &auth, "getblocktemplate", json!([{"rules": ["segwit", "blake2b"]}])) {
                let value = tpl.get("coinbasevalue").and_then(|x| x.as_u64()).unwrap_or(0);
                let height = tpl.get("height").and_then(|x| x.as_u64()).unwrap_or(0);
                let prev = hex_rev(tpl.get("previousblockhash").and_then(|x| x.as_str()).unwrap_or("")).unwrap_or([0u8; 32]);
                if value > 0 && st.mode == Mode::Solo {
                    // Solo needs no coinbaser round trip: the split is this template's
                    // value less our fee, and the miner's half depends on who is asking,
                    // so the jobs are built per identity once the template is in place.
                    match build_template(&tpl, &tag) {
                        Some(t) => {
                            let t = Arc::new(t);
                            log::info!("published solo template height={} txs~{} value={} fee_bps={}", t.height, t.txn_count, t.value, st.solo_fee_bps);
                            st.published_outputs.store(2, Ordering::Relaxed);
                            st.published_height.store(height, Ordering::Relaxed);
                            *lk(&st.tpl) = Some(t);
                            last_pub = Instant::now();
                            st.last_pub_unix.store(unix_now(), Ordering::Relaxed);
                            broadcast_solo(&st);
                        }
                        None => log::warn!("solo: unusable template at height {height}"),
                    }
                } else if value > 0 {
                    send_prime_urgent(&st, CoinbaserRequest { value, prevhash: prev }.encode());
                    let first = st.last_cb.lock().unwrap_or_else(|e| e.into_inner()).is_none();
                    let wait = if first || new_tip { Duration::from_millis(1500) } else { Duration::from_millis(250) };
                    // Never build a coinbase that pays out more than the template allows,
                    // or nothing at all: the block would be bad-cb-amount (or burn the
                    // reward) the day it is found. Prime is trusted, but this is a cheap
                    // check against a bug there.
                    let split = split_for_value(&st, value, wait).and_then(|cb| {
                        let scaled = cb.value_sum() != value;
                        let cb = if cb.value_sum() > value { cb.scale_to(value) } else { cb };
                        if cb.value_sum() == 0 || cb.value_sum() > value {
                            log::warn!("coinbaser split sums to {} for value={}; not publishing", cb.value_sum(), value);
                            None
                        } else {
                            Some((cb, scaled))
                        }
                    });
                    if let Some((cb, scaled)) = split {
                        let seq = st.job_seq.fetch_add(1, Ordering::Relaxed);
                        let jid = (seq % 255) as u8 + 1;
                        let built = build_template(&tpl, &tag)
                            .map(Arc::new)
                            .and_then(|t| coinbase_job(&t, &st.extra1, cb, jid, seq, None));
                        if let Some(j) = built {
                            let j = Arc::new(j);
                            log::info!("published job height={} txs~{} outputs={} value={}{}", j.height(), j.txn_count(), j.outputs(), value, if scaled { " (scaled)" } else { "" });
                            st.published_outputs.store(j.outputs(), Ordering::Relaxed);
                            st.published_height.store(height, Ordering::Relaxed);
                            let line = notify_line(&j);
                            let jid_str = j.id.clone();
                            {
                                let mut hist = lk(&st.jobs);
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
                            *st.job.lock().unwrap_or_else(|e| e.into_inner()) = Some(j);
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
        let mut due = st.gbt_due.lock().unwrap_or_else(|e| e.into_inner());
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
    let mode = run_mode(cfg.mode.as_deref());
    // Solo pays whoever solves the block, so the fee output is the only thing standing
    // between us and mining for free. A missing or unparseable address is fatal rather
    // than a gateway that quietly gives the whole subsidy away.
    let (fee_script, fee_bps) = match mode {
        Mode::Solo => {
            let addr = cfg.solo_fee_address.as_deref().unwrap_or("").trim().to_string();
            let script = identity_script(&addr)
                .expect("solo mode needs solo_fee_address to be a payable address");
            let bps = cfg.solo_fee_bps.unwrap_or(250);
            assert!(bps > 0 && bps <= 10_000, "solo_fee_bps must be 1..=10000");
            (script, bps)
        }
        Mode::Pooled => (Vec::new(), 0),
    };
    let solo_dir = match mode {
        Mode::Solo => Some(
            cfg.solo_data_dir
                .clone()
                .unwrap_or_else(|| cli.config.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()),
        ),
        Mode::Pooled => None,
    };
    if let Some(d) = solo_dir.as_ref() {
        if let Err(e) = std::fs::create_dir_all(d) {
            log::warn!("solo data dir {}: {e}", d.display());
        }
    }
    // prime_port 0 means standalone: solo needs nothing from Prime (it builds its own
    // coinbase and submits its own blocks), and a pooled gateway cannot work without it.
    let prime_on = cfg.prime_port != 0;
    assert!(prime_on || mode == Mode::Solo, "pooled mode needs a Prime to get the TIDES split from");
    log::info!(
        "lazarus-gateway profile={} mode={:?} stratum={} api={} vardiff_min={} vardiff_start={} vardiff_max={} verify={:?} prime={}",
        cfg.profile.as_deref().unwrap_or("asic"),
        mode,
        cfg.stratum_listen,
        cfg.api_listen,
        cfg.vardiff_min,
        cfg.vardiff_start.unwrap_or(cfg.vardiff_min),
        cfg.vardiff_max.unwrap_or(1u64 << 40),
        verify_mode(cfg.verify_shares.as_deref()),
        if prime_on { format!("{}:{}", cfg.prime_host, cfg.prime_port) } else { "off".into() }
    );
    if mode == Mode::Solo {
        log::info!(
            "solo fee {}.{:02}% to {} tag={}",
            fee_bps / 100,
            fee_bps % 100,
            cfg.solo_fee_address.as_deref().unwrap_or(""),
            cfg.coinbase_tag.as_deref().unwrap_or("Lazarus")
        );
    }
    let (tx, rx) = mpsc::channel();
    let (utx, urx) = mpsc::channel();
    let st = Arc::new(Shared {
        cfg: cfg.clone(), mode,
        tpl: Mutex::new(None),
        solo_fee_script: fee_script, solo_fee_bps: fee_bps,
        solo: Mutex::new(load_solo_book(solo_dir.as_ref())),
        solo_dir,
        job: Mutex::new(None), jobs: Mutex::new(VecDeque::new()),
        conns: Mutex::new(Conns { total: 0, per_ip: HashMap::new() }),
        submitted: Mutex::new(HashSet::new()),
        miners: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1), acc: AtomicU64::new(0), rej: AtomicU64::new(0), extra1,
        // Nothing drains the queue when Prime is off, so the sender is dropped: shares are
        // then never enqueued rather than piling up until the cap kicks in.
        prime_tx: Mutex::new(if prime_on { Some(tx) } else { None }),
        last_cb: Mutex::new(None), cb_cv: Condvar::new(),
        gbt_kick: Condvar::new(), gbt_due: Mutex::new(true),
        prime_urgent: Mutex::new(if prime_on { Some(utx) } else { None }),
        miner_socks: Mutex::new(HashMap::new()), published_outputs: AtomicUsize::new(0),
        published_height: AtomicU64::new(0),
        last_share_hdr: Mutex::new(None), last_share_job: Mutex::new(None),
        last_job: Mutex::new(None),
        job_seq: AtomicU64::new(0), pow_ok: AtomicU64::new(0), pow_bad: AtomicU64::new(0),
        verify: verify_mode(cfg.verify_shares.as_deref()),
        prime_depth: AtomicU64::new(0), prime_dropped: AtomicU64::new(0),
        job_miss: AtomicU64::new(0), last_pub_unix: AtomicU64::new(0),
    });
    if prime_on {
        let s = st.clone();
        thread::spawn(move || prime_loop(s, rx, urx));
    }
    if mode == Mode::Solo {
        let s = st.clone();
        thread::spawn(move || solo_save_loop(s));
    }
    { let s = st.clone(); thread::spawn(move || api_loop(s)); }
    { let s = st.clone(); thread::spawn(move || gbt_loop(s)); }
    let lis = TcpListener::bind(&st.cfg.stratum_listen).expect("stratum bind");
    log::info!("stratum {}", st.cfg.stratum_listen);
    let mut refused_logged = Instant::now() - Duration::from_secs(60);
    for inc in lis.incoming() {
        let s = match inc {
            Ok(s) => s,
            // EMFILE and friends: back off instead of spinning the accept loop.
            Err(_) => { thread::sleep(Duration::from_millis(50)); continue; }
        };
        let ip = s.peer_addr().map(|a| a.ip()).unwrap_or(IpAddr::from([0, 0, 0, 0]));
        if !lk(&st.conns).try_acquire(ip) {
            if refused_logged.elapsed() >= Duration::from_secs(60) {
                refused_logged = Instant::now();
                log::warn!("connection limit reached; refusing {ip} (total cap {MAX_CONNS}, per-ip {MAX_CONNS_PER_IP})");
            }
            drop(s);
            continue;
        }
        let st2 = st.clone();
        // thread::spawn panics when the OS refuses a thread; Builder returns Err instead,
        // so a connection flood degrades rather than killing the accept loop.
        let spawned = thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(move || handle_miner(s, st2, ip));
        if spawned.is_err() {
            lk(&st.conns).release(ip);
        }
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;

    fn miner() -> Miner {
        let now = Instant::now();
        Miner { host: String::new(), user: String::new(), ua: String::new(), vdiff: 1, acc: 0, acc_n: 0, rej: 0, rej_n: 0, last: now,
            vdiff_prev: 1, vdiff_prev_until: now, last_retarget: now, retarget_acc_n: 0, job_diffs: VecDeque::new(),
            recent: VecDeque::new(), seen: HashSet::new(), seen_order: VecDeque::new(), tokens: SUBMIT_BURST, tokens_at: now, flood: 0,
            jobs: VecDeque::new(), ident: String::new() }
    }

    fn p2wpkh(fill: u8) -> Vec<u8> {
        let mut s = vec![0x00, 0x14];
        s.extend_from_slice(&[fill; 20]);
        s
    }

    fn template() -> Arc<Template> {
        let gbt = json!({
            "previousblockhash": "00000000000000000002a7c4c1e48d76c5a37902165a270156b7a8d72728a054",
            "bits": "1d00ffff",
            "height": 962_049,
            "coinbasevalue": 312_500_000u64,
            "curtime": 1_750_000_000u64,
            "version": 2,
            "transactions": [],
            "weightlimit": 4_000_000u64,
        });
        Arc::new(build_template(&gbt, "Lazarus/solo").expect("template"))
    }

    #[test]
    fn solo_split_pays_the_miner_everything_but_the_fee() {
        let value = 312_500_000u64;
        for (bps, want_fee) in [(250u32, 7_812_500u64), (50, 1_562_500)] {
            let cb = solo_split(value, &p2wpkh(1), &p2wpkh(2), bps, 3).expect("split");
            assert_eq!(cb.outputs.len(), 2);
            // Miner first: the finder is read off the coinbase without knowing our address.
            assert_eq!(cb.outputs[0].script, p2wpkh(1));
            assert_eq!(cb.outputs[1].script, p2wpkh(2));
            assert_eq!(cb.outputs[1].sats, want_fee, "fee at {bps} bps");
            assert_eq!(cb.outputs[0].sats, value - want_fee);
            assert_eq!(cb.value_sum(), value, "a coinbase over the template value is bad-cb-amount");
        }
    }

    #[test]
    fn solo_split_rounding_favours_the_miner() {
        // 2.5% of 10_001 is 250.025: the odd sat must not land on the fee output.
        let cb = solo_split(10_001, &p2wpkh(1), &p2wpkh(2), 250, 1).expect("split");
        assert_eq!(cb.outputs[1].sats, 250);
        assert_eq!(cb.outputs[0].sats, 9_751);
        assert_eq!(cb.value_sum(), 10_001);
    }

    #[test]
    fn solo_split_refuses_a_value_too_small_to_split() {
        // Under 40 sats the 2.5% fee rounds to zero; publishing that job would mine for free.
        assert!(solo_split(39, &p2wpkh(1), &p2wpkh(2), 250, 1).is_none());
        assert!(solo_split(0, &p2wpkh(1), &p2wpkh(2), 250, 1).is_none());
        assert!(solo_split(1_000, &p2wpkh(1), &[], 250, 1).is_none());
        assert!(solo_split(1_000, &p2wpkh(1), &p2wpkh(2), 10_000, 1).is_none(), "a 100% fee leaves the miner nothing");
    }

    #[test]
    fn solo_jobs_are_per_identity_and_share_one_template() {
        let tpl = template();
        let extra1 = [7u8, 8, 9, 10];
        let a = coinbase_job(&tpl, &extra1, solo_split(tpl.value, &p2wpkh(1), &p2wpkh(9), 250, 1).unwrap(), 1, 1, Some("a".into())).unwrap();
        let b = coinbase_job(&tpl, &extra1, solo_split(tpl.value, &p2wpkh(2), &p2wpkh(9), 250, 2).unwrap(), 2, 2, Some("b".into())).unwrap();
        // One copy of the block body between them, however many miners connect.
        assert!(Arc::ptr_eq(&a.tpl, &b.tpl));
        assert_ne!(a.id, b.id, "two jobs sharing an id cannot be told apart at submit time");
        assert_ne!(a.coinb1, b.coinb1);
        assert_ne!(a.header.merkle_root, b.header.merkle_root);
        assert_eq!(a.height(), 962_049);
        assert_eq!(a.value(), tpl.value);
        assert_eq!(a.txn_count(), 1);
        assert_eq!(a.outputs(), 2);
        assert_eq!(a.who.as_deref(), Some("a"));
    }

    /// The bytes a solve would actually hand to `submitblock`: whatever this asserts is
    /// what lands on chain.
    #[test]
    fn an_assembled_solo_block_pays_the_miner_and_the_fee() {
        let tpl = template();
        let extra1 = [1u8, 2, 3, 4];
        let miner = p2wpkh(3);
        let cb = solo_split(tpl.value, &miner, &p2wpkh(9), 250, 1).unwrap();
        let j = coinbase_job(&tpl, &extra1, cb, 1, 1, Some("bc1qsolo".into())).unwrap();
        let blk = assemble_block(&extra1, &j, &j.header);
        // header, then the transaction count, then the coinbase (this template has no
        // other transactions and no witness commitment).
        let n_tx = &blk[pow::HEADER_V2_SIZE..pow::HEADER_V2_SIZE + 1];
        assert_eq!(n_tx, &[1u8]);
        let parsed = lazarus_protocol::coinbaser::parse_coinbase(&blk[pow::HEADER_V2_SIZE + 1..])
            .expect("the block body must parse as a coinbase");
        assert_eq!(parsed.height, Some(962_049), "BIP34 height");
        assert_eq!(parsed.outputs.len(), 2);
        assert_eq!(parsed.outputs[0].script, miner, "the miner is paid first");
        assert_eq!(parsed.outputs[0].sats, tpl.value - 7_812_500);
        assert_eq!(parsed.outputs[1].script, p2wpkh(9));
        assert_eq!(parsed.outputs[1].sats, 7_812_500, "2.5% of 3.125 BTC");
        assert_eq!(
            parsed.outputs.iter().map(|o| o.sats).sum::<u64>(),
            tpl.value,
            "paying more than coinbasevalue is bad-cb-amount"
        );
        // The header the miner hashed commits to exactly these outputs, so the fee cannot
        // be swapped out after the fact.
        assert_eq!(j.header.merkle_root, pow::merkle_root_sha256d(&parsed.legacy, &tpl.txids));
    }

    #[test]
    fn pooled_jobs_carry_no_payee() {
        let tpl = template();
        let cb = solo_split(tpl.value, &p2wpkh(1), &p2wpkh(9), 250, 1).unwrap();
        let j = coinbase_job(&tpl, &[0, 0, 0, 0], cb, 1, 1, None).unwrap();
        assert!(j.who.is_none(), "a pooled solve must not be logged as somebody's solo block");
    }

    #[test]
    fn job_ids_stay_unique_over_many_identities() {
        let extra1 = [1u8, 2, 3, 4];
        let ids: HashSet<String> = (0..200_000u64).map(|seq| job_id_str(&extra1, seq)).collect();
        assert_eq!(ids.len(), 200_000, "solo issues one job per identity per template");
        assert!(job_id_str(&extra1, 5).len() <= 16, "submit rejects a job id over 16 chars");
    }

    #[test]
    fn canon_identity_folds_bech32_case_only() {
        let lower = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        assert_eq!(canon_identity(&lower.to_ascii_uppercase()), lower);
        assert_eq!(canon_identity(&format!("{lower}.rig1")), lower);
        // Base58 is case-sensitive: folding it would change the address.
        assert_eq!(canon_identity("1PyBLoCKdiaC46vD9CWcmxa3ey2VzSc5Q2"), "1PyBLoCKdiaC46vD9CWcmxa3ey2VzSc5Q2");
    }

    #[test]
    fn hash_pot_is_the_difficulty_the_share_reached() {
        // A hash that just meets 2^pot has 32 + pot leading zero bits.
        for pot in [0u8, 1, 7, 8, 20] {
            let t = pow::target_for_pot(pot);
            assert_eq!(hash_pot(&t), pot, "target for pot {pot}");
            assert!(pow::meets_target(&t, &pow::target_for_pot(pot)));
        }
        assert_eq!(hash_pot(&[0xff; 32]), 0);
    }

    #[test]
    fn solo_book_keeps_block_finders_when_it_evicts() {
        let mut b = SoloBook::default();
        b.credit("winner", 1024, 12);
        b.note_block(SoloBlock { height: 1, hash: "h".into(), who: "winner".into(), ts: 1, value: 10, fee: 1 });
        for i in 0..SOLO_BOOK_MINERS + 64 {
            b.credit(&format!("m{i}"), 1, 0);
        }
        assert!(b.miners.len() <= SOLO_BOOK_MINERS + 1, "held {}", b.miners.len());
        let w = b.miners.get("winner").expect("a miner who found a block stays on the board");
        assert_eq!(w.blocks, 1);
        assert_eq!(w.best_pot, 12);
    }

    #[test]
    fn solo_book_survives_a_round_trip() {
        let mut b = SoloBook::default();
        b.credit("bc1qx", 4096, 14);
        b.note_block(SoloBlock { height: 962_049, hash: "abc".into(), who: "bc1qx".into(), ts: 7, value: 100, fee: 2 });
        let s = serde_json::to_string(&b).unwrap();
        let back: SoloBook = serde_json::from_str(&s).unwrap();
        assert_eq!(back.work, 4096);
        assert_eq!(back.shares, 1);
        assert_eq!(back.blocks.len(), 1);
        assert_eq!(back.miners["bc1qx"].best_pot, 14);
        assert_eq!(back.miners["bc1qx"].blocks, 1);
    }

    #[test]
    fn mode_and_fee_default_safely() {
        assert_eq!(run_mode(None), Mode::Pooled);
        assert_eq!(run_mode(Some("Solo")), Mode::Pooled, "only exactly \"solo\" turns on solo payouts");
        assert_eq!(run_mode(Some("solo")), Mode::Solo);
        assert_eq!(fee_for(312_500_000, 250), 7_812_500);
        assert_eq!(fee_for(312_500_000, 50), 1_562_500);
        assert_eq!(fee_for(0, 250), 0);
    }

    #[test]
    fn usernames_are_bounded_printable_ascii() {
        assert!(clean_user("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.rig1"));
        assert!(!clean_user(""));
        assert!(!clean_user(&"a".repeat(MAX_USER + 1)));
        assert!(clean_user(&"a".repeat(MAX_USER)));
        assert!(!clean_user("bc1q\u{0}abc"), "NUL would cut Prime's wire framing");
        assert!(!clean_user("bc1q\nabc"), "newline forges log lines");
        assert!(!clean_user("bc1q abc"));
        assert!(!clean_user("bc1q\u{1b}[31mabc"));
        assert!(!clean_user("bc1qé"));
    }

    #[test]
    fn hex_fields_are_fixed_width_and_bounded() {
        assert_eq!(hex_field(Some(&json!("0102030405060708"))), Some([1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(hex_field(Some(&json!("01020304"))), Some([1, 2, 3, 4, 0, 0, 0, 0]));
        assert_eq!(hex_field(Some(&json!(""))), None);
        assert_eq!(hex_field(Some(&json!("abc"))), None, "odd length");
        assert_eq!(hex_field(Some(&json!("zz"))), None);
        assert_eq!(hex_field(Some(&json!("010203040506070809"))), None, "over 8 bytes");
        assert_eq!(hex_field(Some(&json!(&"00".repeat(1_000_000)))), None);
        assert_eq!(hex_field(Some(&json!(5))), None);
        assert_eq!(hex_field(None), None);
    }

    #[test]
    fn duplicate_shares_are_refused_and_memory_is_capped() {
        let mut m = miner();
        let h = [7u8; 32];
        assert!(note_share(&mut m, h));
        assert!(!note_share(&mut m, h), "same hash twice on one connection");
        for i in 0..(SEEN_CAP as u32 * 2) {
            let mut x = [0u8; 32];
            x[..4].copy_from_slice(&i.to_le_bytes());
            x[31] = 1;
            note_share(&mut m, x);
        }
        assert!(m.seen.len() <= SEEN_CAP);
        assert_eq!(m.seen.len(), m.seen_order.len());
    }

    #[test]
    fn submit_token_bucket_refuses_a_flood_then_refills() {
        let mut m = miner();
        let mut ok = 0;
        for _ in 0..(SUBMIT_BURST as usize + 50) {
            if take_token(&mut m) { ok += 1; }
        }
        assert_eq!(ok, SUBMIT_BURST as usize);
        assert!(!take_token(&mut m));
        m.tokens_at -= Duration::from_secs(1);
        let mut refilled = 0;
        while take_token(&mut m) { refilled += 1; }
        assert!((refilled as f64 - SUBMIT_RATE).abs() <= 1.0, "refilled={refilled}");
    }

    #[test]
    fn recent_window_is_capped_by_count() {
        let mut m = miner();
        for _ in 0..(RECENT_CAP + 100) { record_share(&mut m, 1); }
        assert_eq!(m.recent.len(), RECENT_CAP);
        assert_eq!(m.acc, (RECENT_CAP + 100) as u64);
    }

    #[test]
    fn connection_caps_total_and_per_ip() {
        let mut c = Conns { total: 0, per_ip: HashMap::new() };
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        for _ in 0..MAX_CONNS_PER_IP { assert!(c.try_acquire(a)); }
        assert!(!c.try_acquire(a), "per-ip cap");
        assert!(c.try_acquire(b), "other ip still fine");
        c.release(a);
        assert!(c.try_acquire(a));
        for _ in 0..MAX_CONNS_PER_IP { c.release(a); }
        c.release(b);
        assert_eq!(c.total, 0);
        assert!(c.per_ip.is_empty());
    }

    #[test]
    fn verify_mode_defaults_to_enforce() {
        assert_eq!(verify_mode(None), VerifyMode::Enforce);
        assert_eq!(verify_mode(Some("enforce")), VerifyMode::Enforce);
        assert_eq!(verify_mode(Some("typo")), VerifyMode::Enforce);
        assert_eq!(verify_mode(Some("log")), VerifyMode::Log);
        assert_eq!(verify_mode(Some("off")), VerifyMode::Off);
    }

    #[test]
    fn log_text_is_sanitised() {
        assert_eq!(short("abc\n\u{1b}[31mdef"), "abc[31mdef");
        assert_eq!(short(&"x".repeat(500)).len(), 48);
    }
}

#[cfg(test)]
mod vardiff_tests {
    use super::*;

    const MIN: u64 = 1024;
    const MAX: u64 = 131072;
    const START: u64 = 4096;

    #[test]
    fn burst_from_start_does_not_jump_past_4x_or_cap() {
        let huge = 1u64 << 40;
        assert_eq!(step_vardiff(START, huge, MIN, MAX), 16384);
        assert_eq!(step_vardiff(16384, huge, MIN, MAX), 65536);
        assert_eq!(step_vardiff(65536, huge, MIN, MAX), MAX);
        assert_eq!(step_vardiff(MAX, huge, MIN, MAX), MAX);
    }

    #[test]
    fn slow_miner_steps_down_to_floor() {
        assert_eq!(step_vardiff(START, 1, MIN, MAX), MIN);
        assert_eq!(step_vardiff(MIN, 1, MIN, MAX), MIN);
    }

    #[test]
    fn nineteen_ths_leaves_start_in_one_step() {
        let ideal = vardiff_ideal(19e12);
        assert_eq!(ideal, 16384);
        assert_eq!(step_vardiff(START, ideal, MIN, MAX), 16384);
    }

    #[test]
    fn five_point_five_ths_stays_at_start() {
        let ideal = vardiff_ideal(5.5e12);
        assert_eq!(ideal, START);
        assert_eq!(step_vardiff(START, ideal, MIN, MAX), START);
    }

    #[test]
    fn twenty_seven_ths_reaches_ideal_in_two_steps() {
        let ideal = vardiff_ideal(27e12);
        assert_eq!(ideal, 32768);
        let mut d = START;
        d = step_vardiff(d, ideal, MIN, MAX);
        assert_eq!(d, 16384);
        d = step_vardiff(d, ideal, MIN, MAX);
        assert_eq!(d, 32768);
    }

    #[test]
    fn one_forty_ths_reaches_cap_in_three_steps() {
        let ideal = vardiff_ideal(140e12);
        assert_eq!(ideal, MAX);
        let mut d = START;
        let mut steps = 0;
        while d != ideal && steps < 8 {
            d = step_vardiff(d, ideal, MIN, MAX);
            steps += 1;
        }
        assert_eq!(d, MAX);
        assert!(steps <= 3, "steps={steps}");
    }
}
