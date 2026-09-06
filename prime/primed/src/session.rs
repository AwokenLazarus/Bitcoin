//! One DATUM gateway connection: handshake, configuration, coinbaser replies, share
//! verification and crediting, block relay.
//!
//! A session is a single task owning both halves of the socket; there is no per-message
//! locking beyond a short ledger critical section on accepted shares.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datum_wire::coinbaser::{self, Output};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake::{self, ClientHello, Generation};
use datum_wire::mining::{self, ClientMsg, JobValidationReply, PowSubmit, ValidationStatus};
use datum_wire::verify::{self, CoinbaseKind, JobSlot, Policy, VerifiedShare};
use datum_wire::{cmd, MAX_CMD_LEN};
use rand_core::{OsRng, RngCore};
use tides::{BlockRecord, Payee, SOURCE_DATUM, SOURCE_STRATUM};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{interval, MissedTickBehavior};

use crate::address::{self};
use crate::config::Config;
use crate::state::{now, ClientInfo, Seen, Shared};

fn house_stratum(cfg: &Config, remote: SocketAddr, gateway_hex: &str) -> bool {
    if cfg.house_loopback && remote.ip().is_loopback() {
        return true;
    }
    let g = gateway_hex.to_ascii_lowercase();
    cfg.house_gateways.iter().any(|h| !h.is_empty() && g.starts_with(h))
}

const MAX_HELLO: usize = 4096;
const KEEPALIVE: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(300);
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(15);
const COINBASERS_KEPT: usize = 16;
const PENDING_BLOCK_TTL: Duration = Duration::from_secs(120);
const MAX_IDENTITIES: usize = 1 << 16;
/// Job slots whose coinbase sections stay resident per session; see `Session::touch_slot`.
const MAX_LIVE_SLOTS: usize = 16;
/// Coinbaser requests a session may make at once, and how often one is added back. A
/// gateway asks once per template it builds — every ten seconds or so, and on every new
/// block — so a burst of 32 refilled one per second never touches a real one.
const COINBASER_BURST: u32 = 32;
const COINBASER_REFILL: Duration = Duration::from_secs(1);
/// How long stock DATUM's coinbaser thread blocks waiting for a reply before it publishes the
/// job without a split — `datum_protocol_coinbaser_fetch` in `datum_protocol.c`.
const STOCK_COINBASER_DEADLINE: Duration = Duration::from_secs(5);
/// A coinbaser reply this slow is worth a warning: still answered, but the margin against
/// `STOCK_COINBASER_DEADLINE` has gone from a rounding error to a fifth of the budget.
const COINBASER_SLOW: Duration = Duration::from_secs(1);
/// How often one session may repeat the "mining work without the split" warning.
const POOL_ONLY_WARN_EVERY: Duration = Duration::from_secs(300);
/// A session with this many rejects (or malformed messages) inside `REJECT_WINDOW` is
/// doing nothing useful and costing verification CPU: drop it. The live house gateway
/// runs at 13 rejects per 50 000 shares; a broken farm behind one gateway might manage
/// a few a second.
const REJECT_FLOOD: usize = 2_000;
const REJECT_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Wire(#[from] datum_wire::Error),
    #[error("{0}")]
    Bad(&'static str),
    #[error("handshake timed out")]
    HandshakeTimeout,
    #[error("idle")]
    Idle,
    #[error("reject flood: {0} rejected or malformed messages in {1}s")]
    RejectFlood(usize, u64),
}

struct IssuedCoinbaser {
    id: u8,
    value: u64,
    outputs: Vec<Output>,
    payees: Vec<Payee>,
}

/// How a coinbaser request gets answered. Deliberately has no "drop" variant: a request left
/// unanswered makes a stock gateway publish a coinbase paying only the pool, so the token
/// bucket may choose the cost of the answer but never withhold it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoinbaserAction {
    /// Inside the bucket: compute a fresh split under a new id.
    Fresh,
    /// Over the bucket, and this exact value was already answered: repeat that reply.
    Repeat(u8),
    /// Over the bucket with nothing to repeat: compute anyway, and count it.
    FreshOverRate,
}

fn coinbaser_action(tokens: u32, issued: &VecDeque<IssuedCoinbaser>, value: u64) -> CoinbaserAction {
    if tokens > 0 {
        return CoinbaserAction::Fresh;
    }
    match issued.iter().rev().find(|c| c.value == value) {
        Some(c) => CoinbaserAction::Repeat(c.id),
        None => CoinbaserAction::FreshOverRate,
    }
}

struct PendingBlock {
    share: VerifiedShare,
    submit: PowSubmit,
    hash_hex: String,
    at: Instant,
}

struct Session {
    shared: Arc<Shared>,
    id: u64,
    remote: SocketAddr,
    stream: TcpStream,
    recv_keys: KeyStream,
    send_keys: KeyStream,
    channel: Channel,
    session_key: Identity,
    hello: ClientHello,
    slots: Vec<JobSlot>,
    /// Coinbase section bytes held across all slots, against `cfg.session_coinbase_budget`.
    coinbase_bytes: usize,
    /// Slots in order of their last job change, oldest first.
    live_slots: VecDeque<usize>,
    coinbasers: VecDeque<IssuedCoinbaser>,
    next_coinbaser_id: u8,
    /// Block candidates waiting for the gateway's transaction list, by job id. A job can solve
    /// more than once (regtest does it every share; on mainnet it is rare but a lost block is
    /// the worst outcome), and the gateway's reply names only the job, so keep every candidate
    /// and submit them all from the one transaction set.
    pending_blocks: HashMap<u8, Vec<PendingBlock>>,
    last_send: Instant,
    last_recv: Instant,
    gateway_hex: String,
    /// Last time this session warned that the gateway's node is ahead of ours (rate limit).
    ahead_warned: Option<Instant>,
    /// Token bucket for coinbaser requests; see `on_coinbaser_request`.
    coinbaser_tokens: u32,
    coinbaser_refill_at: Instant,
    /// Timestamps of recent rejects and malformed messages; see `note_reject`.
    recent_rejects: VecDeque<Instant>,
    /// Accepted shares from this session whose coinbase paid only the pool, and when we last
    /// said so; see `note_pool_only_share`.
    pool_only_shares: u64,
    /// Of those, the ones on a full job — the kind that is not stock DATUM's per-height
    /// subsidy-only job and should not be happening.
    pool_only_full_jobs: u64,
    pool_only_warned: Option<Instant>,
}

/// Which of the two gateway-side faults produced a pool-only coinbase, read off the share rather
/// than guessed at.
///
/// The pool cannot fix either, but they are different bugs and the distinction decides where an
/// operator looks. If the coinbaser the job cited carried payees, the gateway had our split and
/// published a section that does not use it — that is stock's unconditional type-0 selection on
/// the first notify of a height, and it is what block 968440 was. If it carried none, the gateway
/// had nothing to place: either it never asked for this job's coinbaser, or it gave up before the
/// reply landed.
fn pool_only_cause(section: u8, coinbaser_id: u8, payees: usize) -> String {
    if payees > 0 {
        format!(
            "it published section {section} while holding coinbaser {coinbaser_id}, which carried {payees} miner \
             output{} — the split was in hand and the section it handed miners did not use it",
            if payees == 1 { "" } else { "s" },
        )
    } else {
        format!(
            "coinbaser {coinbaser_id} named by the job carried no miner outputs here, so the gateway had no split \
             to place when it built section {section} — it either never asked for this job's coinbaser or gave up \
             before our reply landed",
        )
    }
}

/// What an accepted pool-only share says about why its coinbase had no miner outputs.
struct PoolOnly {
    /// The share was on stock DATUM's per-height subsidy-only job (no transactions).
    subsidy_only: bool,
    txcount: u32,
    /// The gateway's own coinbase section index (`cbselect`) for the work it handed miners.
    section: u8,
    /// The coinbaser id the job named, and how many miner outputs we had issued under it.
    coinbaser_id: u8,
    payees: usize,
}

pub async fn run(shared: Arc<Shared>, mut stream: TcpStream, remote: SocketAddr) -> Result<(), SessionError> {
    let _ = stream.set_nodelay(true);
    let id = shared.next_client_id.fetch_add(1, Ordering::Relaxed);

    // --- hello -------------------------------------------------------------------------
    let mut initial = KeyStream(CLIENT_INITIAL_KEY);
    let (header, payload) =
        match tokio::time::timeout(HANDSHAKE_LIMIT, read_frame(&mut stream, &mut initial, MAX_HELLO)).await {
            Ok(r) => r?,
            Err(_) => return Err(SessionError::HandshakeTimeout),
        };
    if header.cmd != cmd::HELLO || !header.sealed || !header.signed || header.channel {
        return Err(SessionError::Bad("first frame is not a sealed, signed hello"));
    }
    let hello = handshake::parse_client_hello(&shared.pool, &payload)?;
    let session_key = Identity::generate();
    let (recv_keys, mut send_keys) = KeyStream::from_seed(hello.seed);
    let (send_nonce, recv_nonce) = crypto::session_nonces(hello.seed, &hello.session_sign_pk);
    let channel = Channel::new(session_key.precompute(&hello.session_box_pk), send_nonce, recv_nonce);

    let reply = handshake::build_server_hello(&shared.pool, &session_key, &hello, &shared.cfg.motd);
    let mut h = Header::new(cmd::HELLO_REPLY, reply.len());
    h.sealed = true;
    h.signed = true;
    write_frame(&mut stream, &h, &reply, &mut send_keys).await?;

    let gateway_hex = hex::encode(&hello.identity_sign_pk[..8]);
    let fee_path = if house_stratum(&shared.cfg, remote, &gateway_hex) { "stratum" } else { "datum" };
    log::info!(
        "[{id}] {remote} hello ua={:?} gateway={gateway_hex} gen={:?} fee={fee_path}{}",
        hello.user_agent,
        hello.generation,
        if hello.resume_token.is_some() { " (asked to resume; declined)" } else { "" }
    );
    shared.clients.lock().unwrap().insert(
        id,
        ClientInfo {
            id,
            remote: remote.to_string(),
            user_agent: hello.user_agent.clone(),
            generation: match hello.generation {
                Generation::Ocean => "ocean",
                Generation::Convoy => "convoy",
            },
            gateway: gateway_hex.clone(),
            connected_ts: now(),
            fee_path: fee_path.into(),
            ..Default::default()
        },
    );
    shared.totals.add(&shared.totals.connections, 1);
    // Removed on drop, so the clients table cannot keep a row for a session that panicked.
    let _row = ClientRow { shared: shared.clone(), id };

    let mut s = Session {
        shared: shared.clone(),
        id,
        remote,
        stream,
        recv_keys,
        send_keys,
        channel,
        session_key,
        hello,
        slots: (0..mining::MAX_JOB_SLOTS).map(|_| JobSlot::default()).collect(),
        coinbase_bytes: 0,
        live_slots: VecDeque::new(),
        coinbasers: VecDeque::new(),
        next_coinbaser_id: 1,
        pending_blocks: HashMap::new(),
        last_send: Instant::now(),
        last_recv: Instant::now(),
        ahead_warned: None,
        gateway_hex,
        coinbaser_tokens: COINBASER_BURST,
        coinbaser_refill_at: Instant::now(),
        recent_rejects: VecDeque::new(),
        pool_only_shares: 0,
        pool_only_full_jobs: 0,
        pool_only_warned: None,
    };
    s.serve().await
}

struct ClientRow {
    shared: Arc<Shared>,
    id: u64,
}

impl Drop for ClientRow {
    fn drop(&mut self) {
        if let Ok(mut c) = self.shared.clients.lock() {
            c.remove(&self.id);
        }
    }
}

impl Session {
    fn is_house_stratum(&self) -> bool {
        house_stratum(&self.shared.cfg, self.remote, &self.gateway_hex)
    }

    async fn serve(&mut self) -> Result<(), SessionError> {
        self.send_configure().await?;
        // Ask the gateway to rebuild its templates right now.
        //
        // A gateway that just reconnected is still serving jobs whose coinbases carry a split
        // from coinbaser ids the *previous* Prime process issued. This session has no record of
        // those ids, so every share on one of those jobs pays outputs Prime cannot vouch for and
        // is refused as bad-coinbase-outputs — valid miner work, thrown away, purely because we
        // restarted. A block-notify makes the gateway build fresh jobs and ask for a fresh
        // coinbaser, so it rotates off that stale work in seconds instead of minutes.
        //
        // This is also the only thing Prime can usefully push. An unsolicited *coinbaser* is
        // worse than useless: stock only reads the reply buffer from inside a fetch and only when
        // the value matches the one it asked for, so a pushed split is discarded — and because
        // the buffer index flips and signals the condvar on every reply, one arriving mid-fetch
        // wakes that fetch on the wrong slot and makes it give up with no outputs at all.
        self.send_mining(&mining::block_notify(), false).await?;

        let mut notify = self.shared.notify.subscribe();
        let mut tick = interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // `read_buf` is cancel-safe where `read_exact` is not; frames are cut from `inbuf`.
        let mut inbuf = InBuf::default();
        let mut pending: Option<Header> = None;
        loop {
            inbuf.make_room();
            tokio::select! {
                n = self.stream.read_buf(&mut inbuf.data) => {
                    if n? == 0 {
                        return Err(SessionError::Io(std::io::ErrorKind::UnexpectedEof.into()));
                    }
                    self.last_recv = Instant::now();
                    loop {
                        let h = match pending {
                            Some(h) => h,
                            None => {
                                let Some(hb) = inbuf.take(Header::SIZE) else { break };
                                let h = Header::decode(hb.try_into().unwrap(), &mut self.recv_keys)?;
                                if h.len as usize > MAX_CMD_LEN {
                                    return Err(SessionError::Bad("frame too large"));
                                }
                                pending = Some(h);
                                h
                            }
                        };
                        let Some(payload) = inbuf.take(h.len as usize) else { break };
                        let mut payload = payload.to_vec();
                        pending = None;
                        self.handle_frame(h, &mut payload).await?;
                    }
                }
                n = notify.recv() => {
                    match n {
                        Ok(_) => self.send_mining(&mining::block_notify(), false).await?,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            self.send_mining(&mining::block_notify(), false).await?;
                        }
                        Err(_) => {}
                    }
                }
                _ = tick.tick() => {
                    if self.last_recv.elapsed() > IDLE_LIMIT {
                        return Err(SessionError::Idle);
                    }
                    if self.last_send.elapsed() > KEEPALIVE {
                        // an empty INFO frame: 4 bytes, logged by nobody, resets the
                        // gateway's nothing-from-server watchdog
                        let h = Header::new(cmd::INFO, 0);
                        write_frame(&mut self.stream, &h, &[], &mut self.send_keys).await?;
                        self.last_send = Instant::now();
                    }
                    self.pending_blocks.retain(|_, v| {
                        v.retain(|p| p.at.elapsed() < PENDING_BLOCK_TTL);
                        !v.is_empty()
                    });
                }
            }
        }
    }

    async fn send_configure(&mut self) -> Result<(), SessionError> {
        let cfg = &self.shared.cfg;
        let body = match self.hello.generation {
            Generation::Ocean => {
                mining::configure_v1(&self.shared.pool_script, cfg.prime_id, &cfg.coinbase_tag, cfg.min_diff)
            }
            Generation::Convoy => {
                let mut token = [0u8; mining::RESUME_TOKEN_LEN];
                OsRng.fill_bytes(&mut token);
                mining::configure_v3(
                    &self.shared.pool_script,
                    u64::from(cfg.prime_id),
                    &token,
                    &cfg.coinbase_tag,
                    cfg.min_diff,
                )
            }
        };
        self.send_mining(&body, true).await
    }

    /// Encrypt (and optionally sign with the session key) a mining payload and send it.
    async fn send_mining(&mut self, plain: &[u8], signed: bool) -> Result<(), SessionError> {
        let payload = if signed {
            let mut m = Vec::with_capacity(plain.len() + crypto::SIG);
            m.extend_from_slice(plain);
            m.extend_from_slice(&self.session_key.sign(plain));
            self.channel.encrypt(&m)
        } else {
            self.channel.encrypt(plain)
        };
        let mut h = Header::new(cmd::MINING, payload.len());
        h.channel = true;
        h.signed = signed;
        write_frame(&mut self.stream, &h, &payload, &mut self.send_keys).await?;
        self.last_send = Instant::now();
        Ok(())
    }

    async fn handle_frame(&mut self, h: Header, payload: &mut [u8]) -> Result<(), SessionError> {
        if h.sealed {
            return Err(SessionError::Bad("sealed frame after handshake"));
        }
        let mut body: &[u8] = if h.channel { self.channel.decrypt_in_place(payload)? } else { payload };
        if h.signed {
            body = crypto::verify_trailing(&self.hello.session_sign_pk, body)?;
        }
        match h.cmd {
            cmd::MINING => {
                if !h.channel {
                    return Err(SessionError::Bad("plaintext mining frame"));
                }
                match mining::parse_client(body) {
                    Ok(ClientMsg::CoinbaserRequest(r)) => self.on_coinbaser_request(r.value, r.prev_hash).await,
                    Ok(ClientMsg::Pow(p)) => self.on_pow(*p).await,
                    Ok(ClientMsg::JobValidation(v)) => self.on_validation(v).await,
                    Ok(ClientMsg::Unknown(sub)) => {
                        log::debug!("[{}] ignoring unknown mining sub-command 0x{sub:02x}", self.id);
                        Ok(())
                    }
                    Err(e) => {
                        log::debug!("[{}] malformed mining message: {e}", self.id);
                        self.note_reject()
                    }
                }
            }
            cmd::HELLO => Err(SessionError::Bad("second hello")),
            other => {
                log::debug!("[{}] ignoring command {other}", self.id);
                Ok(())
            }
        }
    }

    // --- coinbaser ---------------------------------------------------------------------

    async fn on_coinbaser_request(&mut self, value: u64, prev_hash: [u8; 32]) -> Result<(), SessionError> {
        // A coinbaser request is never left unanswered.
        //
        // Stock DATUM's coinbaser thread sends this request and then blocks on a condvar for
        // five seconds. On timeout it publishes the job with `available_coinbase_outputs_count`
        // still zero, which builds a coinbase paying only the pool script — work that owes the
        // whole reward back to the window if it finds a block. Silence is the one answer that
        // guarantees that outcome, so the bucket below only decides whether the reply is
        // recomputed or repeated, never whether it is sent.
        //
        // This is not why 968440 paid the pool alone — stock selects coinbase type 0 on the
        // first notify of every height regardless of any reply (see the README). It is so that
        // the pool is never the *second* cause of the same thing.
        //
        // For the same reason Prime never sends a coinbaser the gateway did not ask for: the
        // reply is only used when its value equals the requested one, and it lands in a global
        // two-slot buffer whose index flips per reply, so a spurious one can make a legitimate
        // fetch read the wrong value and fall back to exactly the pool-only coinbase this
        // guards against.
        let started = Instant::now();
        let elapsed = self.coinbaser_refill_at.elapsed();
        if elapsed >= COINBASER_REFILL {
            let n = (elapsed.as_secs_f64() / COINBASER_REFILL.as_secs_f64()) as u32;
            self.coinbaser_tokens = (self.coinbaser_tokens.saturating_add(n)).min(COINBASER_BURST);
            self.coinbaser_refill_at = Instant::now();
        }
        match coinbaser_action(self.coinbaser_tokens, &self.coinbasers, value) {
            CoinbaserAction::Fresh => self.coinbaser_tokens -= 1,
            CoinbaserAction::Repeat(prev_id) => {
                // Already answered for this exact value: repeat that reply rather than issue a
                // new id. Costs nothing and is what the gateway would have kept anyway. Matched
                // on id *and* value, and never sent empty: a list the gateway reads as shorter
                // than one output is "no coinbaser" to it, which forgets the id.
                let repeat = self
                    .coinbasers
                    .iter()
                    .rev()
                    .find(|c| c.id == prev_id && c.value == value && !c.outputs.is_empty())
                    .map(|c| coinbaser::encode_v2(c.id, &c.outputs));
                if let Some(repeat) = repeat {
                    log::debug!("[{}] coinbaser over rate; repeating #{prev_id} for value={value}", self.id);
                    self.shared.totals.add(&self.shared.totals.coinbasers_repeated, 1);
                    self.send_mining(&mining::coinbaser_reply(value, &repeat), false).await?;
                    return self.note_coinbaser_latency(started, Some(prev_id));
                }
                self.shared.totals.add(&self.shared.totals.coinbasers_over_rate, 1);
            }
            CoinbaserAction::FreshOverRate => {
                self.shared.totals.add(&self.shared.totals.coinbasers_over_rate, 1);
            }
        }

        let id = self.next_coinbaser_id;
        self.next_coinbaser_id = if id == 255 { 1 } else { id + 1 };

        // Computed off a shared snapshot, so a reply never waits on the ledger mutex behind
        // share crediting or the other gateways asking at the same tip change.
        let base = self.shared.coinbaser_base();
        let split =
            tides::split::compute(base.miners.clone(), base.total_work, value, &self.shared.split_params, |ident| {
                base.script_for(ident)
            });
        let (target, total_work) = (base.target_work, base.total_work);
        let mut outputs: Vec<Output> =
            split.payees.iter().map(|p| Output { sats: p.sats, script: p.script.clone() }).collect();
        // The list is complete: the pool's fee and whatever the split could not place go
        // last, to the pool's own script, so the outputs sum to `value`. A stock gateway
        // pays the list verbatim and only appends its own pool output for funds left over
        // (none when the template value matches); lazarus-gateway writes exactly the list,
        // so without this line the fee would be burned. Last, because the size classes a
        // stock gateway builds for small miners keep a prefix of the list, and the pool's
        // remainder is what those may drop.
        if split.pool_sats > 0 || outputs.is_empty() {
            // Also guarantees at least one output: a gateway treats a shorter list as "no
            // coinbaser" and forgets the id.
            outputs.push(Output { sats: split.pool_sats.max(1), script: self.shared.pool_script.clone() });
        }
        let encoded = coinbaser::encode_v2(id, &outputs);
        self.send_mining(&mining::coinbaser_reply(value, &encoded), false).await?;

        let mut ph = prev_hash;
        ph.reverse();
        log::debug!(
            "[{}] coinbaser #{id} value={value} prev={} outputs={} pool={} window={}/{}",
            self.id,
            &hex::encode(ph)[..16],
            outputs.len(),
            split.pool_sats,
            total_work,
            target
        );
        self.coinbasers.push_back(IssuedCoinbaser { id, value, outputs, payees: split.payees });
        while self.coinbasers.len() > COINBASERS_KEPT {
            self.coinbasers.pop_front();
        }
        self.shared.totals.add(&self.shared.totals.coinbasers, 1);
        self.shared.client_update(self.id, |c| c.coinbasers += 1);
        self.note_coinbaser_latency(started, Some(id))
    }

    /// Record how long a coinbaser reply took to reach the wire, and say so if it came close
    /// to the five seconds a stock gateway waits before giving up and mining a pool-only
    /// coinbase. This is the number that goes wrong first when the pool is the reason a
    /// gateway published unsplit work.
    fn note_coinbaser_latency(&mut self, started: Instant, id: Option<u8>) -> Result<(), SessionError> {
        let took = started.elapsed();
        self.shared.totals.raise(&self.shared.totals.coinbaser_max_us, took.as_micros() as u64);
        if took >= COINBASER_SLOW {
            self.shared.totals.add(&self.shared.totals.coinbasers_slow, 1);
            log::warn!(
                "[{}] {} coinbaser{} took {} ms: a stock gateway gives up at {} s and then mines a coinbase paying only the pool",
                self.id,
                self.gateway_hex,
                id.map(|i| format!(" #{i}")).unwrap_or_default(),
                took.as_millis(),
                STOCK_COINBASER_DEADLINE.as_secs(),
            );
        }
        Ok(())
    }

    fn issued(&self, id: u8) -> Option<&IssuedCoinbaser> {
        self.coinbasers.iter().rev().find(|c| c.id == id)
    }

    /// Undo a coinbase section this share brought in (it pushed the session over budget).
    fn drop_coinbase(&mut self, job_id: usize, added: Option<u8>) {
        let Some(id) = added else { return };
        let before = self.slots[job_id].coinbase_bytes();
        self.slots[job_id].forget_coinbase(id);
        let after = self.slots[job_id].coinbase_bytes();
        self.coinbase_bytes = self.coinbase_bytes.saturating_sub(before.saturating_sub(after));
    }

    /// Note that `job_id` just started a new job, and evict the sections of the slot that
    /// has gone longest without one once more than `MAX_LIVE_SLOTS` hold any. A stock gateway
    /// rotates eight slots and never reaches this; lazarus-gateway walks all 255 but resends
    /// its sections with every share, so an evicted slot simply refills when next used.
    fn touch_slot(&mut self, job_id: usize) {
        self.live_slots.retain(|&s| s != job_id);
        self.live_slots.push_back(job_id);
        while self.live_slots.len() > MAX_LIVE_SLOTS {
            if let Some(old) = self.live_slots.pop_front() {
                self.slots[old].evict_sections();
            }
        }
    }

    // --- shares ------------------------------------------------------------------------

    async fn on_pow(&mut self, s: PowSubmit) -> Result<(), SessionError> {
        let job_id = usize::from(s.job_id);
        if job_id >= self.slots.len() {
            return self.reject(&s, mining::REJECT_BAD_JOB_ID).await;
        }
        // Bech32 folds to one case so one payout address is one TIDES row (see
        // `canonical_identity`); base58 and non-addresses are kept byte-exact.
        let identity = address::canonical_identity(address::identity_of(&s.username));
        if identity.is_empty() || identity.len() > 128 || !identity.bytes().all(|b| b.is_ascii_graphic()) {
            return self.reject(&s, mining::REJECT_BAD_USERNAME).await;
        }

        // Job/coinbase sections, then staleness before any hashing. What a session can make
        // Prime hold is bounded three ways: a section has a size cap and a slot an id cap
        // (`JobSlot::absorb`), only the `MAX_LIVE_SLOTS` most recently (re)started slots keep
        // their sections, and the bytes across slots are budgeted. A stock gateway sends
        // each section once per job and never again — even after a reject — so a section is
        // never dropped just because the share carrying it failed.
        let held_before = self.slots[job_id].coinbase_bytes();
        let absorbed = match self.slots[job_id].absorb(&s) {
            Ok(a) => a,
            Err(code) => return self.reject(&s, code).await,
        };
        if absorbed.job_changed {
            self.touch_slot(job_id);
            self.coinbase_bytes = self.slots.iter().map(JobSlot::coinbase_bytes).sum();
        } else {
            let held_after = self.slots[job_id].coinbase_bytes();
            self.coinbase_bytes = self.coinbase_bytes.saturating_sub(held_before).saturating_add(held_after);
        }
        if absorbed.coinbase_added.is_some() && self.coinbase_bytes > self.shared.cfg.session_coinbase_budget {
            self.drop_coinbase(job_id, absorbed.coinbase_added);
            log::warn!(
                "[{}] {} over the coinbase budget ({} bytes across slots); refusing section",
                self.id,
                self.remote,
                self.shared.cfg.session_coinbase_budget
            );
            return self.reject(&s, mining::REJECT_COINBASE_TOO_LARGE).await;
        }
        let (height, coinbaser_id) = match &self.slots[job_id].job {
            Some(j) => (j.height, j.coinbaser_id),
            None => return self.reject(&s, mining::REJECT_BAD_JOB_ID).await,
        };
        if let Some(tip) = self.shared.tip_snapshot() {
            let expected = tip.height + 1;
            let grace = Duration::from_secs(u64::from(self.shared.cfg.stale_grace_secs));
            let stale = height < expected && !(height + 1 == expected && tip.seen_at.elapsed() < grace);
            if height > expected + 1 {
                // The gateway's node is at least two blocks past ours. That is our node lagging
                // (peers, sync, or an isolated node), not a stale gateway; say so, once a minute,
                // because every share from every healthy gateway is being refused meanwhile.
                if self.ahead_warned.map_or(true, |t| t.elapsed() > Duration::from_secs(60)) {
                    self.ahead_warned = Some(Instant::now());
                    log::warn!(
                        "[{}] {} submits work for height {height} but our node's tip is {}: the pool node is behind the gateway's node; check its peers and sync. Rejecting as stale until it catches up.",
                        self.id, self.gateway_hex, tip.height
                    );
                }
                return self.reject(&s, mining::REJECT_STALE_BLOCK).await;
            }
            if stale {
                return self.reject(&s, mining::REJECT_STALE_BLOCK).await;
            }
        }

        let issued_outputs = self.issued(coinbaser_id).map(|c| c.outputs.clone());
        let pool_script = self.shared.pool_script.clone();
        let policy = Policy {
            pool_script: &pool_script,
            issued: issued_outputs.as_deref(),
            tolerance: self.shared.cfg.split_tolerance,
            now: now() as u32,
            min_pot: self.shared.cfg.min_pot(),
        };
        let v = match verify::verify(&mut self.slots[job_id], &s, &policy) {
            Ok(v) => v,
            Err(code) => return self.reject(&s, code).await,
        };
        // One credit per hash, pool-wide and for as long as the height is live: the set is
        // shared, keyed by height, and never cleared by anything a gateway can send.
        let seen = self.shared.seen.lock().unwrap().insert(v.height, v.hash);
        match seen {
            Seen::Fresh => {}
            Seen::Duplicate => return self.reject(&s, mining::REJECT_DUPLICATE_WORK).await,
            Seen::Full => {
                log::warn!(
                    "[{}] share set full at height {}; refusing work rather than forgetting any",
                    self.id,
                    v.height
                );
                return self.reject(&s, mining::REJECT_OTHER).await;
            }
        }

        // credit
        let ts = now();
        let credited = {
            let mut ledger = self.shared.ledger.lock().unwrap();
            let table_full =
                ledger.window.identities().len() >= MAX_IDENTITIES && ledger.window.work_of(&identity) == 0;
            if table_full && address::to_script(&identity, self.shared.network).is_none() {
                false
            } else {
                let source = if self.is_house_stratum() { SOURCE_STRATUM } else { SOURCE_DATUM };
                if let Err(e) = ledger.credit(&identity, v.work, v.height, ts as u32, source) {
                    log::error!("ledger write failed: {e}");
                }
                true
            }
        };
        if !credited {
            return self.reject(&s, mining::REJECT_BAD_USERNAME).await;
        }
        self.shared.totals.add(&self.shared.totals.accepted, 1);
        self.shared.totals.add(&self.shared.totals.work, v.work);
        self.shared.client_update(self.id, |c| {
            c.accepted += 1;
            c.work += v.work;
            c.last_share_ts = ts;
            c.identity = identity.clone();
        });
        let status = if matches!(v.coinbase_kind, CoinbaseKind::Split) {
            mining::ACCEPTED
        } else {
            mining::ACCEPTED_TENTATIVELY
        };
        if matches!(v.coinbase_kind, CoinbaseKind::PoolOnly) {
            // Why this job had no miner outputs is the whole question, and the share answers it:
            // `s.coinbase_id` is the gateway's own section index (its `cbselect`), and the
            // coinbaser we issued for the job says whether it had a split to place at all.
            let payees = self.issued(coinbaser_id).map_or(0, |c| c.payees.len());
            self.note_pool_only_share(PoolOnly {
                subsidy_only: s.subsidy_only(),
                txcount: v.commitment.txcount,
                section: s.coinbase_id,
                coinbaser_id,
                payees,
            });
        }
        self.send_mining(&mining::share_receipt(status, 0, s.nonce32, s.target_pot, s.job_id), false).await?;

        if v.is_block_candidate {
            self.on_block_candidate(s, v, identity).await?;
        }
        Ok(())
    }

    async fn reject(&mut self, s: &PowSubmit, code: u16) -> Result<(), SessionError> {
        let name = mining::reject_name(code);
        log::debug!("[{}] reject job={} user={:?} pot={}: {name}", self.id, s.job_id, s.username, s.target_pot);
        self.shared.totals.add(&self.shared.totals.rejected, 1);
        self.shared.client_update(self.id, |c| {
            c.rejected += 1;
            c.last_reject = Some(name);
        });
        self.send_mining(&mining::share_receipt(mining::REJECTED, code, s.nonce32, s.target_pot, s.job_id), false).await?;
        self.note_reject()
    }

    /// Note an accepted share whose coinbase paid only the pool script.
    ///
    /// The share is good work and is credited; what it cannot do is pay the window if it turns
    /// out to be a block, because the coinbase it committed to has no miner outputs. A gateway
    /// publishing that work says so over thousands of shares before it gets lucky, so this is
    /// the signal to act on — 968440 was found on a job like this and nothing in the log said
    /// so beforehand.
    ///
    /// The two kinds are worth telling apart by cost. Stock DATUM emits one subsidy-only job per
    /// height (`JOB_STATE_EMPTY_PLUS`, no transactions, coinbase id `0xff`), which is cheap to
    /// lose. A *full* job with a pool-only coinbase is the expensive one: a whole template's
    /// fees and subsidy with no miner outputs, which is what 968440 was.
    fn note_pool_only_share(&mut self, p: PoolOnly) {
        let PoolOnly { subsidy_only, txcount, section, coinbaser_id, payees } = p;
        self.pool_only_shares += 1;
        self.shared.totals.add(&self.shared.totals.pool_only_shares, 1);
        if !subsidy_only {
            self.pool_only_full_jobs += 1;
            self.shared.totals.add(&self.shared.totals.pool_only_full_jobs, 1);
        }
        let (n, full) = (self.pool_only_shares, self.pool_only_full_jobs);
        self.shared.client_update(self.id, |c| {
            c.pool_only_shares = n;
            c.pool_only_full_jobs = full;
        });
        if self.pool_only_warned.is_some_and(|t| t.elapsed() < POOL_ONLY_WARN_EVERY) {
            return;
        }
        self.pool_only_warned = Some(Instant::now());
        if full == 0 {
            // Every one so far is the per-height subsidy-only job every stock gateway sends.
            log::warn!(
                "[{}] {} {} has {n} share{} on a subsidy-only coinbase paying just the pool. Stock DATUM emits one \
                 such job per height, so a low count here is expected; a block found on one owes the window.",
                self.id,
                self.remote,
                self.gateway_hex,
                if n == 1 { "" } else { "s" },
            );
            return;
        }
        let cause = pool_only_cause(section, coinbaser_id, payees);
        log::warn!(
            "[{}] {} {} is mining FULL jobs whose coinbase pays only the pool ({full} of {n} pool-only share{}, \
             latest carried {} transactions): a whole template's fees and subsidy with no miner outputs, so a \
             block found on it owes the window everything — 968440 was one of these. Diagnosis: {cause}. Either \
             way it is fixed at the gateway: lazarus/patches/datum-gateway-split-only.patch, or lazarus-gateway.",
            self.id,
            self.remote,
            self.gateway_hex,
            if n == 1 { "" } else { "s" },
            txcount.saturating_sub(1),
        );
    }

    /// Count a reject or malformed message against the session's flood budget.
    fn note_reject(&mut self) -> Result<(), SessionError> {
        let now = Instant::now();
        self.recent_rejects.push_back(now);
        while self.recent_rejects.front().is_some_and(|t| now.duration_since(*t) > REJECT_WINDOW) {
            self.recent_rejects.pop_front();
        }
        if self.recent_rejects.len() > REJECT_FLOOD {
            return Err(SessionError::RejectFlood(self.recent_rejects.len(), REJECT_WINDOW.as_secs()));
        }
        Ok(())
    }

    // --- blocks ------------------------------------------------------------------------

    async fn on_block_candidate(&mut self, s: PowSubmit, v: VerifiedShare, finder: String) -> Result<(), SessionError> {
        let mut disp = v.hash;
        disp.reverse();
        let hash_hex = hex::encode(disp);
        log::info!(
            "[{}] BLOCK CANDIDATE from {} height={} hash={hash_hex} coinbase={:?} value={} finder={finder} gateway={}",
            self.id,
            self.remote,
            v.height,
            v.coinbase_kind,
            v.coinbase_value,
            self.gateway_hex
        );
        self.shared.totals.add(&self.shared.totals.block_candidates, 1);
        self.shared.client_update(self.id, |c| c.block_candidates += 1);

        // what the window is owed if this coinbase did not pay the full split
        let job_cb_id = self.slots[usize::from(s.job_id)].job.as_ref().map(|j| j.coinbaser_id);
        let issued = job_cb_id.and_then(|id| self.issued(id));
        let fee = tides::split::fee_for(v.coinbase_value, self.shared.cfg.fee_bps);
        let (kind, owed, split): (&str, u64, Vec<(String, u64)>) = match (&v.coinbase_kind, issued) {
            (CoinbaseKind::Split, Some(c)) => {
                ("split", 0, c.payees.iter().map(|p| (p.identity.clone(), p.sats)).collect())
            }
            (CoinbaseKind::Split, None) => ("split", 0, vec![]),
            (CoinbaseKind::Partial(_), Some(c)) => {
                let unpaid: u64 = c.payees.iter().filter(|p| v.coinbase.paid_to(&p.script) == 0).map(|p| p.sats).sum();
                ("partial", unpaid, c.payees.iter().map(|p| (p.identity.clone(), p.sats)).collect())
            }
            (CoinbaseKind::PoolOnly, Some(c)) => {
                // the reward this block would have split had the gateway carried the outputs
                let scaled: Vec<(String, u64)> =
                    c.payees.iter().map(|p| (p.identity.clone(), scale(p.sats, v.coinbase_value, c.value))).collect();
                let owed = scaled.iter().map(|x| x.1).sum();
                ("pool-only", owed, scaled)
            }
            (CoinbaseKind::PoolOnly, None) => {
                // no coinbaser was issued for this job: split by the live window instead
                let ledger = self.shared.ledger.lock().unwrap();
                let net = self.shared.network;
                let sp =
                    ledger.window.split(v.coinbase_value, &self.shared.split_params, |i| address::to_script(i, net));
                let owed = sp.paid_sats();
                ("pool-only", owed, sp.payees.iter().map(|p| (p.identity.clone(), p.sats)).collect())
            }
            (CoinbaseKind::Partial(_), None) | (CoinbaseKind::Foreign, _) => ("unknown", 0, vec![]),
        };
        let _ = fee;
        let record = BlockRecord {
            ts: now(),
            height: v.height,
            hash: hash_hex.clone(),
            finder: Some(finder),
            coinbase_value: v.coinbase_value,
            kind: kind.into(),
            owed_sats: owed,
            split,
            pool_sats: v.paid_to_pool,
            settled: false,
            submit: "pending".into(),
            gateway: self.gateway_hex.clone(),
        };
        self.shared.record_block(record);

        // ask for the transactions so we can submit the block ourselves as a backup
        let job = s.job_id;
        self.pending_blocks
            .entry(job)
            .or_default()
            .push(PendingBlock { share: v, submit: s, hash_hex, at: Instant::now() });
        self.send_mining(&mining::request_full_block(job), false).await?;
        // every other gateway should refresh its template now
        let _ = self.shared.notify.send(self.id as u32);
        Ok(())
    }

    async fn on_validation(&mut self, v: JobValidationReply) -> Result<(), SessionError> {
        let job = v.job();
        let Some(pending) = self.pending_blocks.remove(&job) else {
            log::debug!("[{}] unsolicited validation reply for job {job}", self.id);
            return Ok(());
        };
        let (status, txns) = match v {
            JobValidationReply::FullBlock { status, txns, .. } => (status, txns),
            JobValidationReply::Transactions { status, txns, .. } => (status, txns),
            JobValidationReply::ShortIds { .. } => {
                self.pending_blocks.insert(job, pending);
                return Ok(());
            }
        };
        if status != ValidationStatus::Ok {
            for p in &pending {
                log::warn!(
                    "[{}] gateway could not supply transactions for block {}: {:?}",
                    self.id,
                    p.hash_hex,
                    status
                );
                self.shared.update_block(&p.hash_hex, |r| r.submit = "no-transactions".into());
            }
            return Ok(());
        }
        // one transaction set per job; every candidate solved on this job assembles from it
        for p in pending {
            self.submit_candidate(p, &txns);
        }
        Ok(())
    }

    fn submit_candidate(&self, pending: PendingBlock, txns: &[Vec<u8>]) {
        let expected = pending.share.commitment.txcount.saturating_sub(1) as usize;
        if txns.len() != expected && txns.len() != pending.share.commitment.txcount as usize {
            log::warn!(
                "[{}] block {}: gateway sent {} transactions, header commits to {}",
                self.id,
                pending.hash_hex,
                txns.len(),
                pending.share.commitment.txcount
            );
        }
        let block = verify::assemble_block(&pending.share, &pending.submit, txns);
        let hex_block = hex::encode(&block);
        let shared = self.shared.clone();
        let hash_hex = pending.hash_hex;
        let id = self.id;
        tokio::spawn(async move {
            let outcome = match shared.rpc.submitblock(&hex_block).await {
                Ok(serde_json::Value::Null) => "accepted".to_string(),
                Ok(serde_json::Value::String(s)) => s,
                Ok(other) => other.to_string(),
                Err(e) => format!("rejected: {e}"),
            };
            log::info!("[{id}] submitblock {hash_hex} ({} bytes): {outcome}", block.len());
            shared.totals.add(&shared.totals.blocks_submitted, 1);
            shared.update_block(&hash_hex, |r| r.submit = outcome);
        });
    }
}

fn scale(sats: u64, value: u64, issued_value: u64) -> u64 {
    if issued_value == 0 {
        return sats;
    }
    ((u128::from(sats) * u128::from(value)) / u128::from(issued_value)) as u64
}

// --- framing -----------------------------------------------------------------------------

#[derive(Default)]
struct InBuf {
    data: Vec<u8>,
    pos: usize,
}

impl InBuf {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.data.len() - self.pos < n {
            return None;
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }

    fn make_room(&mut self) {
        if self.pos > 0 && self.pos >= self.data.len() / 2 {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        if self.data.capacity() - self.data.len() < 4096 {
            self.data.reserve(16 * 1024);
        }
    }
}

/// Handshake-only reader; runs under a timeout outside the main select loop.
async fn read_frame(
    stream: &mut TcpStream,
    keys: &mut KeyStream,
    max: usize,
) -> Result<(Header, Vec<u8>), SessionError> {
    let mut hb = [0u8; Header::SIZE];
    stream.read_exact(&mut hb).await?;
    let h = Header::decode(hb, keys)?;
    let len = h.len as usize;
    if len > max {
        return Err(SessionError::Bad("frame too large"));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((h, payload))
}

async fn write_frame(
    stream: &mut TcpStream,
    h: &Header,
    payload: &[u8],
    keys: &mut KeyStream,
) -> Result<(), SessionError> {
    let mut out = Vec::with_capacity(Header::SIZE + payload.len());
    out.extend_from_slice(&h.encode(keys));
    out.extend_from_slice(payload);
    stream.write_all(&out).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issued(id: u8, value: u64) -> IssuedCoinbaser {
        IssuedCoinbaser { id, value, outputs: vec![Output { sats: value, script: vec![0x00, 0x14, id] }], payees: vec![] }
    }

    /// The property block 968440 came down to: whatever the bucket says, the gateway gets an
    /// answer. Silence makes stock DATUM time out after `STOCK_COINBASER_DEADLINE` and publish
    /// a coinbase with no miner outputs.
    #[test]
    fn a_coinbaser_request_is_never_dropped() {
        let mut empty = VecDeque::new();
        let mut seen = VecDeque::new();
        seen.push_back(issued(7, 312_500_000));
        for tokens in [0u32, 1, 32] {
            for value in [312_500_000u64, 312_644_067] {
                for q in [&mut empty, &mut seen] {
                    // no variant of the decision withholds a reply
                    match coinbaser_action(tokens, q, value) {
                        CoinbaserAction::Fresh | CoinbaserAction::Repeat(_) | CoinbaserAction::FreshOverRate => {}
                    }
                }
            }
        }
    }

    /// The two faults must not be reported as each other: an operator told "the split was in hand"
    /// goes and looks at coinbase selection, and one told it was missing goes and looks at the
    /// coinbaser request path. Live 968440-class shares are the first case.
    #[test]
    fn pool_only_cause_separates_the_two_gateway_faults() {
        let had_split = pool_only_cause(0, 1, 41);
        assert!(had_split.contains("section 0"), "{had_split}");
        assert!(had_split.contains("41 miner outputs"), "{had_split}");
        assert!(had_split.contains("split was in hand"), "{had_split}");

        let no_split = pool_only_cause(4, 7, 0);
        assert!(no_split.contains("no split"), "{no_split}");
        assert!(!no_split.contains("in hand"), "{no_split}");

        // singular reads correctly; a one-payee split is still a split the gateway ignored
        assert!(pool_only_cause(0, 2, 1).contains("1 miner output —"), "{}", pool_only_cause(0, 2, 1));
    }

    #[test]
    fn inside_the_bucket_every_request_is_computed_fresh() {
        let mut q = VecDeque::new();
        q.push_back(issued(7, 312_500_000));
        assert_eq!(coinbaser_action(1, &q, 312_500_000), CoinbaserAction::Fresh);
        assert_eq!(coinbaser_action(32, &q, 312_500_000), CoinbaserAction::Fresh);
    }

    #[test]
    fn over_the_bucket_repeats_the_reply_for_that_exact_value() {
        let mut q = VecDeque::new();
        q.push_back(issued(7, 312_500_000));
        q.push_back(issued(8, 312_644_067));
        // The gateway only accepts a reply whose value equals the one it asked about, so a
        // repeat is only usable when the value matches exactly.
        assert_eq!(coinbaser_action(0, &q, 312_500_000), CoinbaserAction::Repeat(7));
        assert_eq!(coinbaser_action(0, &q, 312_644_067), CoinbaserAction::Repeat(8));
        // newest wins when a value was answered twice
        q.push_back(issued(9, 312_500_000));
        assert_eq!(coinbaser_action(0, &q, 312_500_000), CoinbaserAction::Repeat(9));
        // a value never answered is computed rather than skipped
        assert_eq!(coinbaser_action(0, &q, 999_999_999), CoinbaserAction::FreshOverRate);
        assert_eq!(coinbaser_action(0, &VecDeque::new(), 312_500_000), CoinbaserAction::FreshOverRate);
    }

    /// The warning has to fire while there is still margin, or it is just an obituary.
    #[test]
    fn the_slow_warning_leaves_room_before_a_gateway_gives_up() {
        assert!(COINBASER_SLOW < STOCK_COINBASER_DEADLINE);
    }
}
