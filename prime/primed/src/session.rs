//! One DATUM gateway connection: handshake, configuration, coinbaser replies, share
//! verification and crediting, block relay.
//!
//! A session is a single task owning both halves of the socket; there is no per-message
//! locking beyond a short ledger critical section on accepted shares.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datum_wire::coinbaser::{self, Output};
use datum_wire::crypto::{self, Channel, Identity};
use datum_wire::frame::{Header, KeyStream, CLIENT_INITIAL_KEY};
use datum_wire::handshake::{self, ClientHello, Generation};
use datum_wire::mining::{self, ClientMsg, JobValidationReply, PowSubmit, ValidationStatus};
use datum_wire::pow::Hash;
use datum_wire::verify::{self, CoinbaseKind, JobSlot, Policy, VerifiedShare};
use datum_wire::{cmd, MAX_CMD_LEN};
use rand_core::{OsRng, RngCore};
use tides::{BlockRecord, Payee};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{interval, MissedTickBehavior};

use crate::address::{self};
use crate::state::{now, ClientInfo, Shared};

const MAX_HELLO: usize = 4096;
const KEEPALIVE: Duration = Duration::from_secs(20);
const IDLE_LIMIT: Duration = Duration::from_secs(300);
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(15);
const COINBASERS_KEPT: usize = 16;
const PENDING_BLOCK_TTL: Duration = Duration::from_secs(120);
const MAX_SEEN_PER_JOB: usize = 200_000;
const MAX_IDENTITIES: usize = 1 << 16;

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
}

struct IssuedCoinbaser {
    id: u8,
    value: u64,
    outputs: Vec<Output>,
    payees: Vec<Payee>,
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
    seen: Vec<HashSet<Hash>>,
    coinbasers: VecDeque<IssuedCoinbaser>,
    next_coinbaser_id: u8,
    pending_blocks: HashMap<u8, PendingBlock>,
    last_send: Instant,
    last_recv: Instant,
    gateway_hex: String,
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
    log::info!(
        "[{id}] {remote} hello ua={:?} gateway={gateway_hex} gen={:?}{}",
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
            ..Default::default()
        },
    );
    shared.totals.add(&shared.totals.connections, 1);

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
        seen: (0..mining::MAX_JOB_SLOTS).map(|_| HashSet::new()).collect(),
        coinbasers: VecDeque::new(),
        next_coinbaser_id: 1,
        pending_blocks: HashMap::new(),
        last_send: Instant::now(),
        last_recv: Instant::now(),
        gateway_hex,
    };
    let r = s.serve().await;
    shared.clients.lock().unwrap().remove(&id);
    r
}

impl Session {
    async fn serve(&mut self) -> Result<(), SessionError> {
        self.send_configure().await?;

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
                    self.pending_blocks.retain(|_, p| p.at.elapsed() < PENDING_BLOCK_TTL);
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
                        log::warn!("[{}] malformed mining message: {e}", self.id);
                        Ok(())
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
        let id = self.next_coinbaser_id;
        self.next_coinbaser_id = if id == 255 { 1 } else { id + 1 };

        let (split, target, total_work) = {
            let ledger = self.shared.ledger.lock().unwrap();
            let net = self.shared.network;
            let s = ledger.window.split(value, &self.shared.split_params, |ident| address::to_script(ident, net));
            (s, ledger.window.target_work(), ledger.window.total_work())
        };
        let mut outputs: Vec<Output> =
            split.payees.iter().map(|p| Output { sats: p.sats, script: p.script.clone() }).collect();
        if outputs.is_empty() {
            // The gateway treats a list shorter than one output as "no coinbaser" and
            // forgets the id; an empty window still gets a well-formed reply by naming the
            // pool's own script once (the gateway adds the remainder to the same script).
            outputs.push(Output { sats: self.shared.cfg.min_payout.max(1), script: self.shared.pool_script.clone() });
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
        Ok(())
    }

    fn issued(&self, id: u8) -> Option<&IssuedCoinbaser> {
        self.coinbasers.iter().rev().find(|c| c.id == id)
    }

    // --- shares ------------------------------------------------------------------------

    async fn on_pow(&mut self, s: PowSubmit) -> Result<(), SessionError> {
        let job_id = usize::from(s.job_id);
        if job_id >= self.slots.len() {
            return self.reject(&s, mining::REJECT_BAD_JOB_ID).await;
        }
        let identity = address::identity_of(&s.username).to_string();
        if identity.is_empty() || identity.len() > 128 || !identity.bytes().all(|b| b.is_ascii_graphic()) {
            return self.reject(&s, mining::REJECT_BAD_USERNAME).await;
        }

        // job/coinbase sections, then duplicate and staleness checks before any hashing
        let job_changed = s.job.as_ref().is_some_and(|j| self.slots[job_id].job.as_ref() != Some(j));
        self.slots[job_id].absorb(&s);
        if job_changed {
            self.seen[job_id].clear();
        }
        let (height, coinbaser_id) = match &self.slots[job_id].job {
            Some(j) => (j.height, j.coinbaser_id),
            None => return self.reject(&s, mining::REJECT_BAD_JOB_ID).await,
        };
        if let Some(tip) = self.shared.tip_snapshot() {
            let expected = tip.height + 1;
            let grace = Duration::from_secs(u64::from(self.shared.cfg.stale_grace_secs));
            let stale = height < expected && !(height + 1 == expected && tip.seen_at.elapsed() < grace);
            if stale || height > expected + 1 {
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
        if !self.seen[job_id].insert(v.hash) {
            return self.reject(&s, mining::REJECT_DUPLICATE_WORK).await;
        }
        if self.seen[job_id].len() > MAX_SEEN_PER_JOB {
            self.seen[job_id].clear();
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
                if let Err(e) = ledger.credit(&identity, v.work, v.height, ts as u32) {
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
        self.send_mining(&mining::share_receipt(mining::REJECTED, code, s.nonce32, s.target_pot, s.job_id), false).await
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
        self.pending_blocks.insert(job, PendingBlock { share: v, submit: s, hash_hex, at: Instant::now() });
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
            log::warn!(
                "[{}] gateway could not supply transactions for block {}: {:?}",
                self.id,
                pending.hash_hex,
                status
            );
            self.shared.update_block(&pending.hash_hex, |r| r.submit = "no-transactions".into());
            return Ok(());
        }
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
        let block = verify::assemble_block(&pending.share, &pending.submit, &txns);
        let hex_block = hex::encode(&block);
        let shared = self.shared.clone();
        let hash_hex = pending.hash_hex.clone();
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
        Ok(())
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
