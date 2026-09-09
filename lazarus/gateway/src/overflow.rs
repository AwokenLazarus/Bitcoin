//! Network-share overflow for the house stratum.
//!
//! Lazarus does not want more than ~30% of BLAKE2b network hashrate. The meter here is
//! everything Prime credits — house stratum *and* remote DATUM gateways — against the
//! node's network hashrate. While the pool is over the line, miners that are already
//! ours (an identity or source IP that has had a share accepted recently, or that sits
//! in the TIDES window) keep connecting locally; a miner we have never seen is relayed,
//! from its very first `mining.subscribe`, to another BLAKE2b pool. The relay is a plain
//! line pump: the upstream's extranonce, difficulty and jobs go straight to the miner and
//! its submits go straight back, under the miner's own username, so the other pool pays
//! it. Nothing here touches Prime or a DATUM session: those gateways are someone else's
//! process and cannot be redirected from this side.
//!
//! Hysteresis keeps the valve from flapping on the difference between our 10-minute
//! hashrate window and the node's multi-hour network estimate: overflow starts only after
//! `hold_polls` consecutive polls above `enter_pct` and ends after the same number below
//! `exit_pct`. A session that has been relayed stays relayed until the miner reconnects;
//! a local session is never spliced mid-stream (its extranonce and job ids belong to us).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// One pool we may relay to. `url` is what the pool UI links a relayed miner to;
/// `miner_url` may carry `{address}` for a per-miner page when the pool has one.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UpstreamCfg {
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub miner_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OverflowCfg {
    /// off | shadow | auto | force. Shadow meters and logs what it would have relayed.
    #[serde(default = "d_mode")]
    pub mode: String,
    #[serde(default = "d_enter")]
    pub enter_pct: f64,
    #[serde(default = "d_exit")]
    pub exit_pct: f64,
    #[serde(default = "d_hold")]
    pub hold_polls: u32,
    #[serde(default = "d_poll")]
    pub poll_secs: u64,
    /// How long an identity / IP stays "ours" after its last accepted share.
    #[serde(default = "d_gf_hours")]
    pub grandfather_hours: u64,
    /// `getnetworkhashps` window. The default 120 blocks lags a swing by many hours.
    #[serde(default = "d_nethash_blocks")]
    pub nethash_blocks: u64,
    #[serde(default = "d_prime_stats")]
    pub prime_stats_url: String,
    /// Grandfather set on disk; defaults next to the gateway config.
    #[serde(default)]
    pub state_file: Option<PathBuf>,
    /// `client.show_message` text; `{upstream}` is replaced with the pool name and
    /// `{pct}` with `enter_pct`.
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub upstreams: Vec<UpstreamCfg>,
}
fn d_mode() -> String { "off".into() }
fn d_enter() -> f64 { 32.0 }
fn d_exit() -> f64 { 27.0 }
fn d_hold() -> u32 { 3 }
fn d_poll() -> u64 { 30 }
fn d_gf_hours() -> u64 { 24 }
fn d_nethash_blocks() -> u64 { 60 }
fn d_prime_stats() -> String { "http://127.0.0.1:28916/stats.json".into() }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Off = 0,
    Shadow = 1,
    Auto = 2,
    Force = 3,
}
impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Mode::Off),
            "shadow" => Some(Mode::Shadow),
            "auto" => Some(Mode::Auto),
            "force" => Some(Mode::Force),
            _ => None,
        }
    }
    fn from_u8(v: u8) -> Mode {
        match v {
            1 => Mode::Shadow,
            2 => Mode::Auto,
            3 => Mode::Force,
            _ => Mode::Off,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Shadow => "shadow",
            Mode::Auto => "auto",
            Mode::Force => "force",
        }
    }
}

/// One step of the valve. `active` is the current overflow state, `hold` the number of
/// consecutive polls that have argued for flipping it.
pub fn hysteresis(active: bool, hold: u32, share_pct: f64, enter: f64, exit: f64, hold_polls: u32) -> (bool, u32) {
    let want_flip = if active { share_pct <= exit } else { share_pct >= enter };
    if !want_flip {
        return (active, 0);
    }
    let hold = hold + 1;
    if hold >= hold_polls.max(1) {
        (!active, 0)
    } else {
        (active, hold)
    }
}

/// Least-loaded healthy upstream; ties go round-robin so equal loads spread evenly.
pub fn pick(loads: &[(bool, usize)], rr: usize) -> Option<usize> {
    let n = loads.len();
    if n == 0 {
        return None;
    }
    let min = loads.iter().filter(|(h, _)| *h).map(|(_, s)| *s).min()?;
    for k in 0..n {
        let i = (rr + k) % n;
        if loads[i].0 && loads[i].1 == min {
            return Some(i);
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Serve the miner ourselves.
    Local,
    /// Overflow is on and we would have relayed, but the mode is shadow.
    Shadow,
    /// Overflow is on but no upstream is healthy: serve locally rather than drop work.
    FailOpen,
    /// Relay to this upstream index.
    Proxy(usize),
}
pub fn decide(mode: Mode, active: bool, grandfathered: bool, choice: Option<usize>) -> Decision {
    let on = match mode {
        Mode::Off => false,
        Mode::Force => true,
        Mode::Auto | Mode::Shadow => active,
    };
    if !on || grandfathered {
        return Decision::Local;
    }
    if mode == Mode::Shadow {
        return Decision::Shadow;
    }
    match choice {
        Some(i) => Decision::Proxy(i),
        None => Decision::FailOpen,
    }
}

/// Identities and source IPs that have proved work here recently, with the unix time of
/// their last accepted share. Persisted so a gateway restart during overflow does not
/// turn every reconnecting miner into a stranger.
#[derive(Default, Serialize, Deserialize)]
pub struct Grandfather {
    #[serde(default)]
    idents: HashMap<String, u64>,
    #[serde(default)]
    ips: HashMap<String, u64>,
}
impl Grandfather {
    pub fn note_ident(&mut self, ident: &str, now: u64) {
        if !ident.is_empty() {
            self.idents.insert(ident.to_string(), now);
        }
    }
    pub fn note_ip(&mut self, ip: IpAddr, now: u64) {
        self.ips.insert(ip.to_string(), now);
    }
    pub fn has_ident(&self, ident: &str, now: u64, ttl: u64) -> bool {
        self.idents.get(ident).map(|t| now.saturating_sub(*t) <= ttl).unwrap_or(false)
    }
    pub fn has_ip(&self, ip: IpAddr, now: u64, ttl: u64) -> bool {
        self.ips.get(&ip.to_string()).map(|t| now.saturating_sub(*t) <= ttl).unwrap_or(false)
    }
    pub fn prune(&mut self, now: u64, ttl: u64) {
        self.idents.retain(|_, t| now.saturating_sub(*t) <= ttl);
        self.ips.retain(|_, t| now.saturating_sub(*t) <= ttl);
    }
    pub fn len(&self) -> (usize, usize) {
        (self.idents.len(), self.ips.len())
    }
}

pub struct Upstream {
    pub cfg: UpstreamCfg,
    healthy: AtomicBool,
    sessions: AtomicUsize,
    total: AtomicU64,
    checked_unix: AtomicU64,
    last_err: Mutex<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProxySession {
    pub id: u64,
    pub host: String,
    pub ip: String,
    pub user: String,
    pub identity: String,
    pub worker: String,
    pub ua: String,
    pub upstream: usize,
    pub upstream_name: String,
    pub since_unix: u64,
    pub submits: u64,
    pub accepted: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Meter {
    pub share_pct: f64,
    pub pool_hs: f64,
    pub stratum_hs: f64,
    pub net_hs: f64,
    pub ok: bool,
    pub updated_unix: u64,
    pub last_err: String,
}

pub struct Overflow {
    pub cfg: OverflowCfg,
    mode: AtomicU8,
    active: AtomicBool,
    hold: AtomicU32,
    flips: AtomicU64,
    active_since: AtomicU64,
    meter: Mutex<Meter>,
    grandfather: Mutex<Grandfather>,
    pub upstreams: Vec<Upstream>,
    rr: AtomicUsize,
    proxied: Mutex<HashMap<u64, ProxySession>>,
    proxied_total: AtomicU64,
    shadow_would: AtomicU64,
    fail_open: AtomicU64,
    state_file: PathBuf,
    dirty: AtomicBool,
}

/// Longest stratum line relayed in either direction. Same bound as the local handler.
const RELAY_MAX_LINE: u64 = 8 * 1024;
/// How long a miner has to follow `mining.subscribe` with `mining.authorize` before we
/// decide without its identity. Every firmware we have seen sends both in one round trip.
const AUTHORIZE_WAIT: Duration = Duration::from_secs(3);
/// Lines we will hold while waiting for the authorize.
const GATE_MAX_LINES: usize = 6;
const UPSTREAM_CONNECT: Duration = Duration::from_secs(5);
const HEALTH_EVERY: Duration = Duration::from_secs(60);
const SAVE_EVERY: Duration = Duration::from_secs(60);
const PROBE_UA: &str = "lazarus-overflow-probe/0.1";

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Overflow {
    pub fn new(cfg: OverflowCfg, default_dir: &Path) -> Overflow {
        let mode = Mode::parse(&cfg.mode).unwrap_or_else(|| {
            log::warn!("overflow: unknown mode {:?}; running off", cfg.mode);
            Mode::Off
        });
        let state_file = cfg.state_file.clone().unwrap_or_else(|| default_dir.join("overflow-grandfather.json"));
        let grandfather = match std::fs::read_to_string(&state_file) {
            Ok(s) => serde_json::from_str::<Grandfather>(&s).unwrap_or_else(|e| {
                log::warn!("overflow: grandfather file {} unreadable ({e}); starting empty", state_file.display());
                Grandfather::default()
            }),
            Err(_) => Grandfather::default(),
        };
        let (ni, nip) = grandfather.len();
        log::info!(
            "overflow: mode={} enter={}% exit={}% hold={} poll={}s grandfather={}h upstreams={} loaded {ni} identities / {nip} ips from {}",
            mode.as_str(), cfg.enter_pct, cfg.exit_pct, cfg.hold_polls, cfg.poll_secs, cfg.grandfather_hours,
            cfg.upstreams.iter().map(|u| format!("{}={}:{}", u.name, u.host, u.port)).collect::<Vec<_>>().join(","),
            state_file.display()
        );
        let upstreams = cfg
            .upstreams
            .iter()
            .cloned()
            .map(|c| Upstream {
                cfg: c,
                // Assume up until the first probe says otherwise, so a restart in overflow
                // does not fail-open for a minute.
                healthy: AtomicBool::new(true),
                sessions: AtomicUsize::new(0),
                total: AtomicU64::new(0),
                checked_unix: AtomicU64::new(0),
                last_err: Mutex::new(String::new()),
            })
            .collect();
        Overflow {
            cfg,
            mode: AtomicU8::new(mode as u8),
            active: AtomicBool::new(false),
            hold: AtomicU32::new(0),
            flips: AtomicU64::new(0),
            active_since: AtomicU64::new(0),
            meter: Mutex::new(Meter::default()),
            grandfather: Mutex::new(grandfather),
            upstreams,
            rr: AtomicUsize::new(0),
            proxied: Mutex::new(HashMap::new()),
            proxied_total: AtomicU64::new(0),
            shadow_would: AtomicU64::new(0),
            fail_open: AtomicU64::new(0),
            state_file,
            dirty: AtomicBool::new(false),
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.mode.load(Ordering::Relaxed))
    }
    pub fn set_mode(&self, s: &str) -> Option<Mode> {
        let m = Mode::parse(s)?;
        let old = self.mode();
        self.mode.store(m as u8, Ordering::Relaxed);
        if old != m {
            log::warn!("overflow: mode {} -> {} (admin)", old.as_str(), m.as_str());
        }
        Some(m)
    }
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
    /// True when a new session has to go through the gate at all.
    pub fn considering(&self) -> bool {
        match self.mode() {
            Mode::Off => false,
            Mode::Force => true,
            Mode::Auto | Mode::Shadow => self.is_active(),
        }
    }
    fn ttl(&self) -> u64 {
        self.cfg.grandfather_hours.max(1) * 3600
    }
    pub fn ip_grandfathered(&self, ip: IpAddr) -> bool {
        lk(&self.grandfather).has_ip(ip, unix_now(), self.ttl())
    }
    pub fn ident_grandfathered(&self, ident: &str) -> bool {
        !ident.is_empty() && lk(&self.grandfather).has_ident(ident, unix_now(), self.ttl())
    }
    /// An accepted share on a local session makes this miner ours.
    pub fn note_share(&self, ident: &str, ip: IpAddr) {
        let now = unix_now();
        let mut g = lk(&self.grandfather);
        g.note_ident(ident, now);
        g.note_ip(ip, now);
        self.dirty.store(true, Ordering::Relaxed);
    }
    /// Identities in Prime's window are owed the next blocks; they are ours whichever
    /// path their work arrived on.
    fn seed_from_prime(&self, stats: &Value) {
        let Some(miners) = stats.pointer("/window/miners").and_then(|m| m.as_array()) else { return };
        let now = unix_now();
        let ttl = self.ttl();
        let mut g = lk(&self.grandfather);
        let mut n = 0usize;
        for m in miners {
            let Some(ident) = m.get("identity").and_then(|i| i.as_str()) else { continue };
            let age = m.get("last_share_s").and_then(|a| a.as_u64()).unwrap_or(0);
            if age <= ttl {
                g.note_ident(ident, now.saturating_sub(age));
                n += 1;
            }
        }
        if n > 0 {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }
    pub fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let body = {
            let mut g = lk(&self.grandfather);
            g.prune(unix_now(), self.ttl());
            serde_json::to_string(&*g).unwrap_or_default()
        };
        let tmp = self.state_file.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, &self.state_file)) {
            log::warn!("overflow: cannot save {}: {e}", self.state_file.display());
        }
    }

    fn loads(&self) -> Vec<(bool, usize)> {
        self.upstreams.iter().map(|u| (u.healthy.load(Ordering::Relaxed), u.sessions.load(Ordering::Relaxed))).collect()
    }
    fn choose(&self) -> Option<usize> {
        let rr = self.rr.fetch_add(1, Ordering::Relaxed);
        pick(&self.loads(), rr)
    }

    /// One meter poll. `net_hs` asks the node; `stratum_hs` is this gateway's own live sum.
    pub fn tick(&self, net_hs: Option<f64>, stratum_hs: f64) {
        let prime = minreq::get(&self.cfg.prime_stats_url).with_timeout(8).send().ok().and_then(|r| serde_json::from_str::<Value>(r.as_str().ok()?).ok());
        if let Some(p) = prime.as_ref() {
            self.seed_from_prime(p);
        }
        let pool_ghs = prime.as_ref().and_then(|p| p.pointer("/hashrate/pool_ghs")).and_then(|v| v.as_f64());
        self.apply_meter(net_hs, stratum_hs, pool_ghs);
    }

    /// Apply a share reading. `pool_ghs` is Prime's credited GH/s (stratum + DATUM);
    /// `None` means Prime was unreachable and the stratum sum is incomplete.
    fn apply_meter(&self, net_hs: Option<f64>, stratum_hs: f64, pool_ghs: Option<f64>) {
        let mut m = Meter { stratum_hs, updated_unix: unix_now(), ..Default::default() };
        let prime_ok = pool_ghs.is_some();
        match (pool_ghs, net_hs) {
            (Some(pg), Some(net)) if net > 0.0 => {
                m.pool_hs = pg * 1e9;
                m.net_hs = net;
                m.share_pct = 100.0 * m.pool_hs / net;
                m.ok = true;
            }
            (None, Some(net)) if net > 0.0 => {
                // Prime stats down: this is the gateway's live session sum, not Prime's
                // credited pool. It misses DATUM, and it can also over-count vs the
                // credited window. Incomplete — do not treat it as the pool share.
                m.pool_hs = stratum_hs;
                m.net_hs = net;
                m.share_pct = 100.0 * stratum_hs / net;
                m.ok = true;
                m.last_err = "prime stats unreachable; share is stratum-only".into();
            }
            _ => {
                m.last_err = "network hashrate unavailable".into();
            }
        }
        if m.ok {
            let active = self.is_active();
            let hold = self.hold.load(Ordering::Relaxed);
            // Incomplete pool number must not flip overflow *off*. DATUM we cannot
            // see might still have us over the line. Entering on a stratum-only
            // reading is still safe — if house stratum alone is over `enter_pct`, we
            // are over.
            let (na, nh) = if !prime_ok && active {
                (true, 0)
            } else {
                hysteresis(active, hold, m.share_pct, self.cfg.enter_pct, self.cfg.exit_pct, self.cfg.hold_polls)
            };
            self.hold.store(nh, Ordering::Relaxed);
            if na != active {
                self.active.store(na, Ordering::Relaxed);
                self.flips.fetch_add(1, Ordering::Relaxed);
                self.active_since.store(if na { unix_now() } else { 0 }, Ordering::Relaxed);
                log::warn!(
                    "overflow: {} at {:.1}% of network (pool {:.2} PH/s, net {:.2} PH/s, mode {})",
                    if na { "ON — new house-stratum miners will be relayed" } else { "OFF — new miners come to Lazarus again" },
                    m.share_pct, m.pool_hs / 1e15, m.net_hs / 1e15, self.mode().as_str()
                );
            } else {
                log::info!("overflow: share {:.1}% (pool {:.2} PH/s, net {:.2} PH/s) active={} hold={} proxied={}",
                    m.share_pct, m.pool_hs / 1e15, m.net_hs / 1e15, na, nh, lk(&self.proxied).len());
            }
        } else {
            log::warn!("overflow: meter failed: {}", m.last_err);
        }
        *lk(&self.meter) = m;
    }

    /// Meter thread. `net_hs` is the node's `getnetworkhashps`; `stratum_hs` this
    /// gateway's live sum over local sessions.
    pub fn run_meter(self: Arc<Self>, net_hs: impl Fn() -> Option<f64> + Send + 'static, stratum_hs: impl Fn() -> f64 + Send + 'static) {
        let poll = Duration::from_secs(self.cfg.poll_secs.max(5));
        let mut last_save = Instant::now();
        loop {
            self.tick(net_hs(), stratum_hs());
            if last_save.elapsed() >= SAVE_EVERY {
                last_save = Instant::now();
                self.save();
            }
            thread::sleep(poll);
        }
    }

    /// Health thread: a stratum subscribe against every upstream once a minute.
    pub fn run_health(self: Arc<Self>) {
        loop {
            for (i, u) in self.upstreams.iter().enumerate() {
                let was = u.healthy.load(Ordering::Relaxed);
                let r = probe(&u.cfg.host, u.cfg.port);
                let ok = r.is_ok();
                u.healthy.store(ok, Ordering::Relaxed);
                u.checked_unix.store(unix_now(), Ordering::Relaxed);
                *lk(&u.last_err) = r.err().unwrap_or_default();
                if was != ok {
                    log::warn!("overflow: upstream {} ({}:{}) {}", u.cfg.name, u.cfg.host, u.cfg.port, if ok { "healthy" } else { "DOWN" });
                } else if !ok {
                    log::info!("overflow: upstream {} still down: {}", u.cfg.name, lk(&u.last_err));
                }
                let _ = i;
            }
            thread::sleep(HEALTH_EVERY);
        }
    }

    /// Where a not-yet-known miner would go right now, with the upstream connected and
    /// ready. Tries every healthy upstream in load order before giving up.
    fn connect_choice(&self) -> Option<(usize, TcpStream)> {
        for _ in 0..self.upstreams.len().max(1) {
            let i = self.choose()?;
            let u = &self.upstreams[i];
            match connect(&u.cfg.host, u.cfg.port) {
                Ok(s) => return Some((i, s)),
                Err(e) => {
                    log::warn!("overflow: connect {} ({}:{}) failed: {e}; marking down", u.cfg.name, u.cfg.host, u.cfg.port);
                    u.healthy.store(false, Ordering::Relaxed);
                    *lk(&u.last_err) = e;
                }
            }
        }
        None
    }

    /// Run at a session's first `mining.subscribe`, before anything is answered.
    /// Returns the lines read past `first` that the caller must still process locally,
    /// or `Gate::Relayed` once the relay has run to completion.
    pub fn gate(
        self: &Arc<Self>,
        id: u64,
        sock: &mut TcpStream,
        rdr: &mut BufReader<TcpStream>,
        first: &str,
        ip: IpAddr,
        host: &str,
        idle: Duration,
        canon: &dyn Fn(&str) -> String,
    ) -> Gate {
        if !self.considering() || self.ip_grandfathered(ip) {
            return Gate::Local(Vec::new());
        }
        // Hold the subscribe reply until the authorize names the miner.
        let _ = sock.set_read_timeout(Some(AUTHORIZE_WAIT));
        let mut buf: Vec<String> = Vec::new();
        let mut user = String::new();
        let deadline = Instant::now() + AUTHORIZE_WAIT;
        while buf.len() < GATE_MAX_LINES && Instant::now() < deadline {
            let mut line = String::new();
            match rdr.take(RELAY_MAX_LINE + 1).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(n) if n as u64 > RELAY_MAX_LINE || !line.ends_with('\n') => break,
                Ok(_) => {}
            }
            let is_auth = serde_json::from_str::<Value>(&line)
                .ok()
                .filter(|v| v.get("method").and_then(|m| m.as_str()) == Some("mining.authorize"))
                .and_then(|v| v.get("params")?.as_array()?.first()?.as_str().map(|s| s.to_string()));
            buf.push(line);
            if let Some(u) = is_auth {
                user = u;
                break;
            }
        }
        let _ = sock.set_read_timeout(Some(idle));
        let ident = canon(&user);
        let grandfathered = self.ident_grandfathered(&ident);
        let mode = self.mode();
        let active = self.is_active();
        // Shadow decides without touching the network.
        let dry = decide(mode, active, grandfathered, Some(0));
        match dry {
            Decision::Local => return Gate::Local(buf),
            Decision::Shadow => {
                self.shadow_would.fetch_add(1, Ordering::Relaxed);
                log::info!("overflow[shadow]: would relay {host} user={} (not grandfathered)", short(&user));
                return Gate::Local(buf);
            }
            _ => {}
        }
        let Some((idx, upstream)) = self.connect_choice() else {
            self.fail_open.fetch_add(1, Ordering::Relaxed);
            log::warn!("overflow: no healthy upstream; serving {host} user={} locally (fail-open)", short(&user));
            return Gate::Local(buf);
        };
        let ua: String = serde_json::from_str::<Value>(first)
            .ok()
            .and_then(|v| {
                let s = v.get("params")?.as_array()?.first()?.as_str()?.to_string();
                Some(s.chars().filter(|c| c.is_ascii_graphic() || *c == ' ').take(96).collect::<String>())
            })
            .unwrap_or_default();
        let name = self.upstreams[idx].cfg.name.clone();
        log::info!("overflow: relaying {host} user={} ua={} -> {} ({}:{})", short(&user), short(&ua), name, self.upstreams[idx].cfg.host, self.upstreams[idx].cfg.port);
        let worker = user.split_once('.').map(|(_, w)| w.to_string()).unwrap_or_default();
        let sess = ProxySession {
            id, host: host.to_string(), ip: ip.to_string(), user: user.clone(), identity: ident, worker, ua,
            upstream: idx, upstream_name: name.clone(), since_unix: unix_now(), submits: 0, accepted: 0,
        };
        lk(&self.proxied).insert(id, sess);
        self.upstreams[idx].sessions.fetch_add(1, Ordering::Relaxed);
        self.upstreams[idx].total.fetch_add(1, Ordering::Relaxed);
        self.proxied_total.fetch_add(1, Ordering::Relaxed);
        let _release = ProxyGuard { ov: self.clone(), id, idx };
        let mut initial: Vec<String> = Vec::with_capacity(1 + buf.len());
        initial.push(if first.ends_with('\n') { first.to_string() } else { format!("{first}\n") });
        initial.extend(buf);
        let msg = self
            .cfg
            .message
            .clone()
            .unwrap_or_else(|| "Lazarus is at capacity (holding under {pct}% of BLAKE2b network hashrate). This connection is relayed to {upstream}, which is the pool paying you for it. Reconnect later to return to Lazarus.".into())
            .replace("{upstream}", &name)
            .replace("{pct}", &fmt_pct(self.cfg.enter_pct));
        let stats = Arc::new(RelayStats::default());
        let miner = match sock.try_clone() {
            Ok(m) => m,
            Err(_) => return Gate::Relayed,
        };
        let s2 = stats.clone();
        let ov = self.clone();
        let sid = id;
        // Keep the session row's counters fresh without the pump holding our lock.
        let updater = thread::Builder::new().stack_size(64 * 1024).spawn(move || loop {
            thread::sleep(Duration::from_secs(5));
            if s2.done.load(Ordering::Relaxed) { break; }
            if let Some(p) = lk(&ov.proxied).get_mut(&sid) {
                p.submits = s2.submits.load(Ordering::Relaxed);
                p.accepted = s2.accepted.load(Ordering::Relaxed);
            }
        });
        pump(miner, rdr, upstream, &initial, Some(&msg), idle, &stats);
        stats.done.store(true, Ordering::Relaxed);
        drop(updater);
        log::info!("overflow: relay ended {host} user={} -> {} after {}s, {} submits / {} accepted",
            short(&user), name, unix_now().saturating_sub(lk(&self.proxied).get(&id).map(|p| p.since_unix).unwrap_or(unix_now())),
            stats.submits.load(Ordering::Relaxed), stats.accepted.load(Ordering::Relaxed));
        Gate::Relayed
    }

    pub fn status_json(&self) -> Value {
        let m = lk(&self.meter).clone();
        let (gi, gip) = lk(&self.grandfather).len();
        let proxied = lk(&self.proxied);
        let ups: Vec<Value> = self
            .upstreams
            .iter()
            .map(|u| json!({
                "name": u.cfg.name, "host": u.cfg.host, "port": u.cfg.port, "url": u.cfg.url, "miner_url": u.cfg.miner_url,
                "healthy": u.healthy.load(Ordering::Relaxed), "sessions": u.sessions.load(Ordering::Relaxed),
                "total_sessions": u.total.load(Ordering::Relaxed), "checked_unix": u.checked_unix.load(Ordering::Relaxed),
                "last_error": lk(&u.last_err).clone(),
            }))
            .collect();
        json!({
            "mode": self.mode().as_str(),
            "active": self.is_active(),
            "active_since_unix": self.active_since.load(Ordering::Relaxed),
            "hold": self.hold.load(Ordering::Relaxed),
            "flips": self.flips.load(Ordering::Relaxed),
            "enter_pct": self.cfg.enter_pct, "exit_pct": self.cfg.exit_pct, "hold_polls": self.cfg.hold_polls,
            "poll_secs": self.cfg.poll_secs, "grandfather_hours": self.cfg.grandfather_hours,
            "share_pct": m.share_pct, "pool_hs": m.pool_hs, "stratum_hs": m.stratum_hs,
            "datum_hs": (m.pool_hs - m.stratum_hs).max(0.0), "net_hs": m.net_hs,
            "meter_ok": m.ok, "meter_updated_unix": m.updated_unix, "meter_error": m.last_err,
            "grandfathered_identities": gi, "grandfathered_ips": gip,
            "proxied_sessions": proxied.len(), "proxied_total": self.proxied_total.load(Ordering::Relaxed),
            "shadow_would_relay": self.shadow_would.load(Ordering::Relaxed), "fail_open": self.fail_open.load(Ordering::Relaxed),
            "upstreams": ups,
        })
    }
    pub fn proxied_json(&self) -> Value {
        // Copy out under the lock, then release it: `status_json` takes the same lock.
        let mut rows: Vec<ProxySession> = lk(&self.proxied).values().cloned().collect();
        rows.sort_by_key(|s| s.id);
        let rows: Vec<Value> = rows
            .iter()
            .map(|s| {
                let u = &self.upstreams[s.upstream].cfg;
                let miner_url = u.miner_url.as_ref().map(|t| t.replace("{address}", &s.identity));
                json!({
                    "id": s.id, "host": s.host, "ip": s.ip, "user": s.user, "identity": s.identity, "worker": s.worker, "ua": s.ua,
                    "upstream": u.name, "upstream_host": u.host, "upstream_port": u.port, "upstream_url": u.url, "miner_url": miner_url,
                    "since_unix": s.since_unix, "connected_s": unix_now().saturating_sub(s.since_unix),
                    "submits": s.submits, "accepted": s.accepted,
                })
            })
            .collect();
        json!({ "overflow": self.status_json(), "proxied": rows })
    }
    /// Rows for the `/clients` HTML table, so a relayed miner is visible next to local ones.
    pub fn clients_rows(&self, start_index: usize, esc: &dyn Fn(&str) -> String) -> String {
        let p = lk(&self.proxied);
        let mut v: Vec<&ProxySession> = p.values().collect();
        v.sort_by_key(|s| s.id);
        let mut out = String::new();
        for (k, s) in v.into_iter().enumerate() {
            out.push_str(&format!(
                "<TR><TD>{}</TD><TD>{}</TD><TD>{}</TD><TD>relayed</TD><TD>{} s</TD><TD>proxy</TD><TD>{} ({})</TD><TD>0 (0)</TD><TD>&rarr; {}</TD><TD></TD><TD>{}</TD></TR>",
                start_index + k, esc(&s.host), esc(&s.user), unix_now().saturating_sub(s.since_unix), s.accepted, s.submits, esc(&s.upstream_name), esc(&s.ua)
            ));
        }
        out
    }
}

pub enum Gate {
    /// Serve locally; these lines were read past the subscribe and must still be handled.
    Local(Vec<String>),
    /// The relay ran and the session is over.
    Relayed,
}

struct ProxyGuard {
    ov: Arc<Overflow>,
    id: u64,
    idx: usize,
}
impl Drop for ProxyGuard {
    fn drop(&mut self) {
        lk(&self.ov.proxied).remove(&self.id);
        self.ov.upstreams[self.idx].sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct RelayStats {
    pub submits: AtomicU64,
    pub accepted: AtomicU64,
    pub miner_lines: AtomicU64,
    pub upstream_lines: AtomicU64,
    pub done: AtomicBool,
}

fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .next()
        .ok_or_else(|| "resolve: no address".to_string())?;
    let s = TcpStream::connect_timeout(&addr, UPSTREAM_CONNECT).map_err(|e| format!("connect: {e}"))?;
    let _ = s.set_nodelay(true);
    Ok(s)
}

/// Subscribe against an upstream and expect a JSON reply. No authorize, no shares.
pub fn probe(host: &str, port: u16) -> Result<(), String> {
    let mut s = connect(host, port)?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    s.write_all(format!("{}\n", json!({"id": 1, "method": "mining.subscribe", "params": [PROBE_UA]})).as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let rdr = BufReader::new(s);
    let mut line = String::new();
    match rdr.take(RELAY_MAX_LINE).read_line(&mut line) {
        Ok(0) => Err("closed before reply".into()),
        Err(e) => Err(format!("read: {e}")),
        Ok(_) => {
            let v: Value = serde_json::from_str(&line).map_err(|e| format!("bad json: {e}"))?;
            if v.get("result").map(|r| !r.is_null()).unwrap_or(false) || v.get("method").is_some() {
                Ok(())
            } else {
                Err(format!("subscribe refused: {}", line.trim()))
            }
        }
    }
}

/// The relay proper. `initial` (subscribe, authorize, anything read while waiting) goes
/// to the upstream first; then miner lines flow up and upstream lines flow down until
/// either side closes. Closing one side shuts the other so neither thread lingers.
pub fn pump(
    miner: TcpStream,
    miner_rdr: &mut BufReader<TcpStream>,
    upstream: TcpStream,
    initial: &[String],
    message: Option<&str>,
    idle: Duration,
    stats: &Arc<RelayStats>,
) {
    let _ = upstream.set_read_timeout(Some(idle));
    let _ = upstream.set_write_timeout(Some(Duration::from_secs(10)));
    let _ = miner.set_write_timeout(Some(Duration::from_secs(10)));
    let mut up_w = match upstream.try_clone() {
        Ok(u) => u,
        Err(_) => return,
    };
    for l in initial {
        if up_w.write_all(l.as_bytes()).is_err() {
            return;
        }
        if l.contains("mining.submit") {
            stats.submits.fetch_add(1, Ordering::Relaxed);
        }
    }
    let mut miner_w = match miner.try_clone() {
        Ok(m) => m,
        Err(_) => return,
    };
    if let Some(m) = message {
        let _ = miner_w.write_all(format!("{}\n", json!({"id": null, "method": "client.show_message", "params": [m]})).as_bytes());
    }
    // upstream -> miner
    let down_stats = stats.clone();
    let miner_shut = match miner.try_clone() {
        Ok(m) => m,
        Err(_) => return,
    };
    let up_shut = match upstream.try_clone() {
        Ok(u) => u,
        Err(_) => return,
    };
    let down = thread::Builder::new().stack_size(256 * 1024).spawn(move || {
        let mut rdr = BufReader::new(upstream);
        let mut line = String::new();
        loop {
            line.clear();
            match (&mut rdr).take(RELAY_MAX_LINE + 1).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(n) if n as u64 > RELAY_MAX_LINE || !line.ends_with('\n') => break,
                Ok(_) => {}
            }
            down_stats.upstream_lines.fetch_add(1, Ordering::Relaxed);
            if is_accept(&line) {
                down_stats.accepted.fetch_add(1, Ordering::Relaxed);
            }
            if miner_w.write_all(line.as_bytes()).is_err() {
                break;
            }
        }
        let _ = miner_shut.shutdown(Shutdown::Both);
    });
    if down.is_err() {
        let _ = up_shut.shutdown(Shutdown::Both);
        return;
    }
    // miner -> upstream, on the caller's thread
    let mut line = String::new();
    loop {
        line.clear();
        match miner_rdr.take(RELAY_MAX_LINE + 1).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) if n as u64 > RELAY_MAX_LINE || !line.ends_with('\n') => break,
            Ok(_) => {}
        }
        stats.miner_lines.fetch_add(1, Ordering::Relaxed);
        if line.contains("mining.submit") {
            stats.submits.fetch_add(1, Ordering::Relaxed);
        }
        if up_w.write_all(line.as_bytes()).is_err() {
            break;
        }
    }
    let _ = up_shut.shutdown(Shutdown::Both);
    let _ = miner.shutdown(Shutdown::Both);
}

/// A `{"id": n, "result": true}` after the handshake is an accepted share. The first two
/// ids are subscribe/authorize on every firmware we know, but a miner may number them
/// differently, so this is an estimate for the status row, not accounting.
fn is_accept(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
    if v.get("method").is_some() {
        return false;
    }
    let Some(id) = v.get("id") else { return false };
    if id.is_null() {
        return false;
    }
    if let Some(n) = id.as_u64() {
        if n <= 2 {
            return false;
        }
    }
    v.get("result").and_then(|r| r.as_bool()) == Some(true)
}

fn short(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_graphic()).take(48).collect()
}

/// `25.0` → `25`, `27.5` → `27.5`: for the miner-facing message.
fn fmt_pct(p: f64) -> String {
    if p.fract() == 0.0 { format!("{}", p as i64) } else { format!("{p}") }
}

fn lk<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn hysteresis_needs_consecutive_polls_to_flip() {
        // below the line: nothing happens
        assert_eq!(hysteresis(false, 0, 20.0, 32.0, 27.0, 3), (false, 0));
        // above the line: hold counts up, flips on the third
        assert_eq!(hysteresis(false, 0, 33.0, 32.0, 27.0, 3), (false, 1));
        assert_eq!(hysteresis(false, 1, 33.0, 32.0, 27.0, 3), (false, 2));
        assert_eq!(hysteresis(false, 2, 33.0, 32.0, 27.0, 3), (true, 0));
        // a dip resets the count
        assert_eq!(hysteresis(false, 2, 31.9, 32.0, 27.0, 3), (false, 0));
        // active: between exit and enter stays active
        assert_eq!(hysteresis(true, 0, 29.0, 32.0, 27.0, 3), (true, 0));
        assert_eq!(hysteresis(true, 0, 35.0, 32.0, 27.0, 3), (true, 0));
        // active: below exit for three polls turns it off
        assert_eq!(hysteresis(true, 0, 26.0, 32.0, 27.0, 3), (true, 1));
        assert_eq!(hysteresis(true, 1, 26.0, 32.0, 27.0, 3), (true, 2));
        assert_eq!(hysteresis(true, 2, 26.0, 32.0, 27.0, 3), (false, 0));
        // hold_polls 0/1 flips at once
        assert_eq!(hysteresis(false, 0, 40.0, 32.0, 27.0, 1), (true, 0));
        assert_eq!(hysteresis(false, 0, 40.0, 32.0, 27.0, 0), (true, 0));
    }

    #[test]
    fn pick_is_least_loaded_then_round_robin() {
        assert_eq!(pick(&[], 0), None);
        assert_eq!(pick(&[(false, 0), (false, 0)], 0), None, "nothing healthy");
        assert_eq!(pick(&[(true, 5), (true, 2), (true, 9)], 0), Some(1));
        assert_eq!(pick(&[(true, 5), (false, 0), (true, 9)], 0), Some(0), "down upstream skipped even if empty");
        // equal loads spread by rr
        assert_eq!(pick(&[(true, 0), (true, 0), (true, 0)], 0), Some(0));
        assert_eq!(pick(&[(true, 0), (true, 0), (true, 0)], 1), Some(1));
        assert_eq!(pick(&[(true, 0), (true, 0), (true, 0)], 2), Some(2));
        assert_eq!(pick(&[(true, 0), (true, 0), (true, 0)], 3), Some(0));
        // rr tie-break only among the minimum
        assert_eq!(pick(&[(true, 1), (true, 0), (true, 0)], 0), Some(1));
        assert_eq!(pick(&[(true, 1), (true, 0), (true, 0)], 2), Some(2));
    }

    #[test]
    fn three_upstreams_fill_evenly() {
        let mut loads = vec![(true, 0usize), (true, 0), (true, 0)];
        for rr in 0..30 {
            let i = pick(&loads, rr).unwrap();
            loads[i].1 += 1;
        }
        assert_eq!(loads.iter().map(|l| l.1).collect::<Vec<_>>(), vec![10, 10, 10]);
        // one down: the other two split the rest
        loads[1].0 = false;
        for rr in 30..40 {
            let i = pick(&loads, rr).unwrap();
            assert_ne!(i, 1);
            loads[i].1 += 1;
        }
        assert_eq!(loads[0].1 + loads[2].1, 30);
        assert!((loads[0].1 as i64 - loads[2].1 as i64).abs() <= 1);
    }

    #[test]
    fn decide_respects_mode_and_grandfather() {
        assert_eq!(decide(Mode::Off, true, false, Some(0)), Decision::Local);
        assert_eq!(decide(Mode::Auto, false, false, Some(0)), Decision::Local, "not active");
        assert_eq!(decide(Mode::Auto, true, true, Some(0)), Decision::Local, "grandfathered stays");
        assert_eq!(decide(Mode::Auto, true, false, Some(2)), Decision::Proxy(2));
        assert_eq!(decide(Mode::Auto, true, false, None), Decision::FailOpen);
        assert_eq!(decide(Mode::Shadow, true, false, Some(0)), Decision::Shadow);
        assert_eq!(decide(Mode::Shadow, true, true, Some(0)), Decision::Local);
        assert_eq!(decide(Mode::Force, false, false, Some(1)), Decision::Proxy(1), "force ignores the meter");
        assert_eq!(decide(Mode::Force, false, true, Some(1)), Decision::Local, "force still keeps our miners");
    }

    #[test]
    fn grandfather_expires_and_round_trips() {
        let mut g = Grandfather::default();
        let ip: IpAddr = "8.218.46.116".parse().unwrap();
        g.note_ident("bc1qabc", 1000);
        g.note_ip(ip, 1000);
        assert!(g.has_ident("bc1qabc", 1000 + 3600, 86400));
        assert!(g.has_ip(ip, 1000 + 3600, 86400));
        assert!(!g.has_ident("bc1qabc", 1000 + 86401, 86400));
        assert!(!g.has_ident("bc1qxyz", 1000, 86400));
        assert!(!g.has_ident("", 1000, 86400));
        g.note_ident("", 1000);
        assert_eq!(g.len(), (1, 1), "empty identity is not recorded");
        let s = serde_json::to_string(&g).unwrap();
        let back: Grandfather = serde_json::from_str(&s).unwrap();
        assert!(back.has_ident("bc1qabc", 1000, 1));
        assert!(back.has_ip(ip, 1000, 1));
        let mut old = back;
        old.prune(1000 + 86401, 86400);
        assert_eq!(old.len(), (0, 0));
    }

    #[test]
    fn mode_parses_and_prints() {
        for m in [Mode::Off, Mode::Shadow, Mode::Auto, Mode::Force] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
        assert_eq!(Mode::parse(" AUTO "), Some(Mode::Auto));
        assert_eq!(Mode::parse("on"), None);
    }

    #[test]
    fn accept_detection_skips_handshake_and_notifies() {
        assert!(is_accept(r#"{"id":7,"result":true,"error":null}"#));
        assert!(!is_accept(r#"{"id":1,"result":[[["mining.notify","x"]],"aa",8],"error":null}"#));
        assert!(!is_accept(r#"{"id":2,"result":true,"error":null}"#));
        assert!(!is_accept(r#"{"id":null,"method":"mining.notify","params":[]}"#));
        assert!(!is_accept(r#"{"id":9,"result":false,"error":[23,"low",null]}"#));
        assert!(!is_accept("garbage"));
    }

    fn overflow_for_test(upstreams: Vec<UpstreamCfg>, mode: &str) -> Arc<Overflow> {
        let dir = std::env::temp_dir().join(format!("lz-ov-{}-{}", std::process::id(), unix_now()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = OverflowCfg {
            mode: mode.into(), enter_pct: 32.0, exit_pct: 27.0, hold_polls: 3, poll_secs: 30, grandfather_hours: 24,
            nethash_blocks: 60, prime_stats_url: "http://127.0.0.1:1/never".into(), state_file: None, message: None, upstreams,
        };
        Arc::new(Overflow::new(cfg, &dir))
    }

    /// A stand-in pool: answers subscribe with its own extranonce, authorize with true,
    /// pushes one notify, then echoes every submit as accepted. Records what it saw.
    fn fake_upstream() -> (u16, Arc<Mutex<Vec<String>>>) {
        let lis = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = lis.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        thread::spawn(move || {
            for s in lis.incoming() {
                let Ok(s) = s else { break };
                let seen = seen2.clone();
                thread::spawn(move || {
                    let mut w = s.try_clone().unwrap();
                    let mut rdr = BufReader::new(s);
                    let mut line = String::new();
                    while rdr.read_line(&mut line).unwrap_or(0) > 0 {
                        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                        seen.lock().unwrap().push(line.trim().to_string());
                        let id = v.get("id").cloned().unwrap_or(Value::Null);
                        match v.get("method").and_then(|m| m.as_str()) {
                            Some("mining.subscribe") => {
                                let _ = w.write_all(format!("{}\n", json!({"id": id, "result": [[["mining.notify","up1"]], "deadbeef", 8], "error": null})).as_bytes());
                                let _ = w.write_all(format!("{}\n", json!({"id": null, "method": "mining.set_difficulty", "params": [16384]})).as_bytes());
                                let _ = w.write_all(format!("{}\n", json!({"id": null, "method": "mining.notify", "params": ["job1", "00", "11", "", [], "a0000000", "1b03ffff", "0000000000000000", true]})).as_bytes());
                            }
                            Some("mining.authorize") => {
                                let _ = w.write_all(format!("{}\n", json!({"id": id, "result": true, "error": null})).as_bytes());
                            }
                            Some("mining.submit") => {
                                let _ = w.write_all(format!("{}\n", json!({"id": id, "result": true, "error": null})).as_bytes());
                            }
                            Some("quit") => {
                                let _ = w.shutdown(Shutdown::Both);
                                break;
                            }
                            _ => {
                                let _ = w.write_all(format!("{}\n", json!({"id": id, "result": null, "error": null})).as_bytes());
                            }
                        }
                        line.clear();
                    }
                });
            }
        });
        (port, seen)
    }

    /// A miner socket pair: the test speaks as the miner on one end; the gateway holds the other.
    fn miner_pair() -> (TcpStream, TcpStream) {
        let lis = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = lis.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = lis.accept().unwrap();
        (client, server)
    }

    fn up(name: &str, port: u16) -> UpstreamCfg {
        UpstreamCfg { name: name.into(), host: "127.0.0.1".into(), port, url: format!("https://{name}.test/"), miner_url: Some(format!("https://{name}.test/m/{{address}}")) }
    }

    #[test]
    fn probe_accepts_a_stratum_server_and_refuses_a_closed_port() {
        let (port, _) = fake_upstream();
        assert!(probe("127.0.0.1", port).is_ok());
        let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(probe("127.0.0.1", dead).is_err());
    }

    #[test]
    fn gate_relays_a_stranger_end_to_end_and_reports_it() {
        let (port, seen) = fake_upstream();
        let ov = overflow_for_test(vec![up("riptide", port)], "force");
        let (mut miner, gw_side) = miner_pair();
        let ip: IpAddr = "203.0.113.9".parse().unwrap();
        let ov2 = ov.clone();
        let handle = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(7, &mut sock, &mut rdr, &first, ip, "203.0.113.9:5000", Duration::from_secs(30), &|u| u.split('.').next().unwrap_or("").to_string())
        });
        // the miner: subscribe + authorize in one go, like real firmware
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"cgminer/4.10\"]}\n").unwrap();
        miner.write_all(b"{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qstranger.rig1\",\"x\"]}\n").unwrap();
        miner.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut rdr = BufReader::new(miner.try_clone().unwrap());
        let mut got = Vec::new();
        for _ in 0..5 {
            let mut l = String::new();
            if rdr.read_line(&mut l).unwrap_or(0) == 0 { break; }
            got.push(l);
        }
        let joined = got.concat();
        assert!(joined.contains("deadbeef"), "miner must see the upstream's extranonce, got {joined}");
        assert!(joined.contains("client.show_message"), "miner is told it is relayed");
        assert!(joined.contains("under 32%"), "message names the configured threshold, not a stale literal: {joined}");
        assert!(!joined.contains("{pct}") && !joined.contains("{upstream}"), "placeholders filled: {joined}");
        assert!(joined.contains("mining.notify"));
        // a submit goes up and its accept comes back
        miner.write_all(b"{\"id\":3,\"method\":\"mining.submit\",\"params\":[\"bc1qstranger.rig1\",\"job1\",\"0000000000000000\",\"0000000000000000\",\"0000000000000000\"]}\n").unwrap();
        let mut l = String::new();
        rdr.read_line(&mut l).unwrap();
        assert!(l.contains("\"id\":3") && l.contains("true"), "accept relayed back: {l}");
        thread::sleep(Duration::from_millis(100));
        {
            let st = ov.status_json();
            assert_eq!(st["proxied_sessions"], 1);
            assert_eq!(st["upstreams"][0]["sessions"], 1);
            let pj = ov.proxied_json();
            let row = &pj["proxied"][0];
            assert_eq!(row["identity"], "bc1qstranger");
            assert_eq!(row["worker"], "rig1");
            assert_eq!(row["upstream"], "riptide");
            assert_eq!(row["upstream_url"], "https://riptide.test/");
            assert_eq!(row["miner_url"], "https://riptide.test/m/bc1qstranger");
            assert!(ov.clients_rows(0, &|s| s.to_string()).contains("&rarr; riptide"));
        }
        // miner hangs up: the relay ends, the row goes away, the upstream saw everything
        drop(rdr);
        miner.shutdown(Shutdown::Both).unwrap();
        match handle.join().unwrap() {
            Gate::Relayed => {}
            Gate::Local(_) => panic!("stranger should have been relayed"),
        }
        assert_eq!(ov.status_json()["proxied_sessions"], 0);
        assert_eq!(ov.status_json()["upstreams"][0]["sessions"], 0);
        assert_eq!(ov.status_json()["proxied_total"], 1);
        let s = seen.lock().unwrap();
        assert!(s.iter().any(|l| l.contains("mining.subscribe") && l.contains("cgminer")));
        assert!(s.iter().any(|l| l.contains("mining.authorize") && l.contains("bc1qstranger.rig1")), "username forwarded verbatim");
        assert!(s.iter().any(|l| l.contains("mining.submit")));
    }

    #[test]
    fn gate_keeps_grandfathered_miners_local_and_returns_their_lines() {
        let (port, seen) = fake_upstream();
        let ov = overflow_for_test(vec![up("b2pool", port)], "force");
        let ip: IpAddr = "198.51.100.7".parse().unwrap();
        // known by identity, from a different IP
        ov.note_share("bc1qours", "192.0.2.1".parse().unwrap());
        let (mut miner, gw_side) = miner_pair();
        let ov2 = ov.clone();
        let h = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(8, &mut sock, &mut rdr, &first, ip, "h", Duration::from_secs(30), &|u| u.split('.').next().unwrap_or("").to_string())
        });
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
        miner.write_all(b"{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qours.rig9\",\"x\"]}\n").unwrap();
        match h.join().unwrap() {
            Gate::Local(lines) => {
                assert_eq!(lines.len(), 1, "the authorize read while waiting is handed back");
                assert!(lines[0].contains("mining.authorize"));
            }
            Gate::Relayed => panic!("our miner must not be relayed"),
        }
        assert!(seen.lock().unwrap().is_empty(), "upstream never contacted");

        // known by IP: decided before reading anything more
        let (mut miner, gw_side) = miner_pair();
        let ov2 = ov.clone();
        let h = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(9, &mut sock, &mut rdr, &first, "192.0.2.1".parse().unwrap(), "h", Duration::from_secs(30), &|u| u.to_string())
        });
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
        match h.join().unwrap() {
            Gate::Local(lines) => assert!(lines.is_empty()),
            Gate::Relayed => panic!(),
        }
    }

    #[test]
    fn gate_is_a_no_op_when_off_or_inactive_and_shadow_only_counts() {
        let (port, seen) = fake_upstream();
        for (mode, expect_shadow) in [("off", false), ("auto", false), ("shadow", true)] {
            let ov = overflow_for_test(vec![up("convoy", port)], mode);
            if mode == "shadow" {
                ov.active.store(true, Ordering::Relaxed);
            }
            let (mut miner, gw_side) = miner_pair();
            let ov2 = ov.clone();
            let h = thread::spawn(move || {
                let mut sock = gw_side;
                let mut rdr = BufReader::new(sock.try_clone().unwrap());
                let mut first = String::new();
                rdr.read_line(&mut first).unwrap();
                ov2.gate(1, &mut sock, &mut rdr, &first, "203.0.113.1".parse().unwrap(), "h", Duration::from_secs(30), &|u| u.to_string())
            });
            miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
            miner.write_all(b"{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qnew\",\"x\"]}\n").unwrap();
            match h.join().unwrap() {
                Gate::Local(_) => {}
                Gate::Relayed => panic!("{mode} must not relay"),
            }
            assert_eq!(ov.status_json()["shadow_would_relay"], if expect_shadow { 1 } else { 0 }, "mode {mode}");
        }
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn gate_fails_open_when_no_upstream_answers() {
        let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let ov = overflow_for_test(vec![up("gone", dead)], "force");
        let (mut miner, gw_side) = miner_pair();
        let ov2 = ov.clone();
        let h = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(1, &mut sock, &mut rdr, &first, "203.0.113.2".parse().unwrap(), "h", Duration::from_secs(30), &|u| u.to_string())
        });
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
        miner.write_all(b"{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qnew\",\"x\"]}\n").unwrap();
        match h.join().unwrap() {
            Gate::Local(lines) => assert_eq!(lines.len(), 1),
            Gate::Relayed => panic!("nowhere to relay to"),
        }
        let st = ov.status_json();
        assert_eq!(st["fail_open"], 1);
        assert_eq!(st["upstreams"][0]["healthy"], false, "the failed connect marks it down");
    }

    #[test]
    fn gate_decides_without_authorize_after_the_wait() {
        let (port, _) = fake_upstream();
        let ov = overflow_for_test(vec![up("riptide", port)], "force");
        let (mut miner, gw_side) = miner_pair();
        let ov2 = ov.clone();
        let t0 = Instant::now();
        let h = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(1, &mut sock, &mut rdr, &first, "203.0.113.3".parse().unwrap(), "h", Duration::from_secs(30), &|u| u.to_string())
        });
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
        // no authorize: after AUTHORIZE_WAIT the stranger is relayed anyway
        miner.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        let mut rdr = BufReader::new(miner.try_clone().unwrap());
        let mut l = String::new();
        rdr.read_line(&mut l).unwrap();
        assert!(l.contains("deadbeef") || l.contains("show_message"), "{l}");
        assert!(t0.elapsed() >= AUTHORIZE_WAIT - Duration::from_millis(200));
        miner.shutdown(Shutdown::Both).unwrap();
        drop(rdr);
        assert!(matches!(h.join().unwrap(), Gate::Relayed));
    }

    #[test]
    fn upstream_closing_ends_the_relay_and_drops_the_miner() {
        let (port, _) = fake_upstream();
        let ov = overflow_for_test(vec![up("riptide", port)], "force");
        let (mut miner, gw_side) = miner_pair();
        let ov2 = ov.clone();
        let h = thread::spawn(move || {
            let mut sock = gw_side;
            let mut rdr = BufReader::new(sock.try_clone().unwrap());
            let mut first = String::new();
            rdr.read_line(&mut first).unwrap();
            ov2.gate(1, &mut sock, &mut rdr, &first, "203.0.113.4".parse().unwrap(), "h", Duration::from_secs(30), &|u| u.to_string())
        });
        miner.write_all(b"{\"id\":1,\"method\":\"mining.subscribe\",\"params\":[\"x\"]}\n").unwrap();
        miner.write_all(b"{\"id\":2,\"method\":\"mining.authorize\",\"params\":[\"bc1qnew\",\"x\"]}\n").unwrap();
        // ask the fake pool to hang up on us
        miner.write_all(b"{\"id\":3,\"method\":\"quit\",\"params\":[]}\n").unwrap();
        miner.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut rdr = BufReader::new(miner.try_clone().unwrap());
        let mut l = String::new();
        // drain until EOF: the gateway must close our socket once the upstream is gone
        let mut eof = false;
        for _ in 0..20 {
            l.clear();
            match rdr.read_line(&mut l) {
                Ok(0) => { eof = true; break; }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        assert!(eof, "miner socket should be closed after upstream quit");
        assert!(matches!(h.join().unwrap(), Gate::Relayed));
        assert_eq!(ov.status_json()["proxied_sessions"], 0);
    }

    #[test]
    fn tick_meters_and_flips_with_hysteresis() {
        let ov = overflow_for_test(vec![], "auto");
        // prime stats unreachable in tests: stratum sum stands in, flagged; can enter
        for _ in 0..2 {
            ov.tick(Some(10e15), 4e15);
            assert!(!ov.is_active());
        }
        let m = ov.status_json();
        assert!((m["share_pct"].as_f64().unwrap() - 40.0).abs() < 1e-9);
        assert!(m["meter_error"].as_str().unwrap().contains("stratum-only"));
        ov.tick(Some(10e15), 4e15);
        assert!(ov.is_active(), "third poll over the line turns it on");
        assert_eq!(ov.status_json()["flips"], 1);
        // no network number: state holds
        ov.tick(None, 4e15);
        assert!(ov.is_active());
        assert_eq!(ov.status_json()["meter_ok"], false);
        // stratum-only 20% must NOT exit: the number is incomplete without Prime
        for _ in 0..3 {
            ov.tick(Some(10e15), 2e15);
        }
        assert!(ov.is_active(), "incomplete meter while Prime is down must not flip overflow off");
        assert_eq!(ov.status_json()["flips"], 1);
        // a full Prime reading below exit for three polls does turn it off
        for _ in 0..3 {
            ov.apply_meter(Some(10e15), 2e15, Some(2e6)); // 2e6 GH/s = 2e15 H/s = 20%
        }
        assert!(!ov.is_active(), "20% from Prime for three polls turns it off");
        assert_eq!(ov.status_json()["flips"], 2);
    }

    #[test]
    fn grandfather_persists_across_restart() {
        let dir = std::env::temp_dir().join(format!("lz-ov-persist-{}-{}", std::process::id(), unix_now()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = OverflowCfg {
            mode: "auto".into(), enter_pct: 32.0, exit_pct: 27.0, hold_polls: 3, poll_secs: 30, grandfather_hours: 24,
            nethash_blocks: 60, prime_stats_url: "http://127.0.0.1:1/never".into(), state_file: None, message: None, upstreams: vec![],
        };
        let ov = Overflow::new(cfg.clone(), &dir);
        ov.note_share("bc1qkeep", "192.0.2.50".parse().unwrap());
        ov.save();
        assert!(dir.join("overflow-grandfather.json").is_file());
        let again = Overflow::new(cfg, &dir);
        assert!(again.ident_grandfathered("bc1qkeep"));
        assert!(again.ip_grandfathered("192.0.2.50".parse().unwrap()));
        assert!(!again.ident_grandfathered("bc1qother"));
    }
}
