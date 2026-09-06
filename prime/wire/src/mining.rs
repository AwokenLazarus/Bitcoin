//! Command 5: mining sub-commands.
//!
//! The first payload byte selects the sub-command. Client → server:
//!
//! | byte | meaning                                  |
//! |------|------------------------------------------|
//! | 0x10 | coinbaser request for a new job          |
//! | 0x27 | proof of work (share or block)           |
//! | 0x50 | reply to a job-validation request        |
//!
//! Server → client:
//!
//! | byte | meaning                                  |
//! |------|------------------------------------------|
//! | 0x99 | client configuration (must be signed)    |
//! | 0x11 | coinbaser reply                          |
//! | 0x8F | share receipt                            |
//! | 0x50 | job-validation request                   |
//! | 0xF9 | block notify: refresh your template now  |
//!
//! Clients pad most messages with random bytes after the 0xFE terminator; parsers here
//! stop at the terminator and ignore what follows.

use crate::{Cursor, Error, Result};

pub const SUB_COINBASER_REQUEST: u8 = 0x10;
pub const SUB_COINBASER_REPLY: u8 = 0x11;
pub const SUB_POW: u8 = 0x27;
pub const SUB_JOB_VALIDATION: u8 = 0x50;
pub const SUB_SHARE_RECEIPT: u8 = 0x8F;
pub const SUB_CONFIGURE: u8 = 0x99;
pub const SUB_BLOCK_NOTIFY: u8 = 0xF9;

pub const END: u8 = 0xFE;
pub const MAX_USERNAME: usize = 384;
pub const EXTRANONCE_LEN: usize = 12;
/// The job id is one byte on the wire. OCEAN/Convoy/fte gateways cycle through 8 slots,
/// the iohzrd fork through 256; the pool keeps a slot for every possible id so no lineage
/// is ever told its job is unknown. Empty slots cost nothing.
pub const MAX_JOB_SLOTS: usize = 256;

// PoW flags byte.
pub const FLAG_BLOCK: u8 = 0x01;
pub const FLAG_SUBSIDY_ONLY: u8 = 0x02;
pub const FLAG_QUICKDIFF: u8 = 0x04;
pub const FLAG_BLAKE2B: u8 = 0x08;
/// Bit in the first reserved byte: the header commits to `time_on_wire + time_offset`.
pub const RESERVED_USE_TIME_OFFSET: u8 = 0x01;
/// Algorithm id inside the 0x03 section.
pub const POW_ALGO_BLAKE2B: u8 = 1;

// Share receipt status.
pub const ACCEPTED: u8 = 0x50;
pub const ACCEPTED_TENTATIVELY: u8 = 0x55;
pub const REJECTED: u8 = 0x66;

// Reject reasons (the gateway logs the number).
pub const REJECT_BAD_JOB_ID: u16 = 10;
pub const REJECT_BAD_COINBASE_ID: u16 = 11;
pub const REJECT_BAD_EXTRANONCE_SIZE: u16 = 12;
pub const REJECT_BAD_TARGET: u16 = 13;
pub const REJECT_BAD_USERNAME: u16 = 14;
pub const REJECT_BAD_COINBASER_ID: u16 = 15;
pub const REJECT_BAD_MERKLE_COUNT: u16 = 16;
pub const REJECT_COINBASE_TOO_LARGE: u16 = 17;
pub const REJECT_COINBASE_MISSING: u16 = 18;
pub const REJECT_TARGET_MISMATCH: u16 = 19;
pub const REJECT_H_NOT_ZERO: u16 = 20;
pub const REJECT_HIGH_HASH: u16 = 21;
pub const REJECT_COINBASE_ID_MISMATCH: u16 = 22;
pub const REJECT_BAD_NTIME: u16 = 23;
pub const REJECT_BAD_VERSION: u16 = 24;
pub const REJECT_STALE_BLOCK: u16 = 25;
pub const REJECT_BAD_COINBASE: u16 = 26;
pub const REJECT_BAD_COINBASE_OUTPUTS: u16 = 27;
pub const REJECT_MISSING_POOL_TAG: u16 = 28;
pub const REJECT_DUPLICATE_WORK: u16 = 29;
pub const REJECT_OTHER: u16 = 30;

pub fn reject_name(code: u16) -> &'static str {
    match code {
        REJECT_BAD_JOB_ID => "bad-job-id",
        REJECT_BAD_COINBASE_ID => "bad-coinbase-id",
        REJECT_BAD_EXTRANONCE_SIZE => "bad-extranonce-size",
        REJECT_BAD_TARGET => "bad-target",
        REJECT_BAD_USERNAME => "bad-username",
        REJECT_BAD_COINBASER_ID => "bad-coinbaser-id",
        REJECT_BAD_MERKLE_COUNT => "bad-merkle-count",
        REJECT_COINBASE_TOO_LARGE => "coinbase-too-large",
        REJECT_COINBASE_MISSING => "coinbase-missing",
        REJECT_TARGET_MISMATCH => "target-mismatch",
        REJECT_H_NOT_ZERO => "h-not-zero",
        REJECT_HIGH_HASH => "high-hash",
        REJECT_COINBASE_ID_MISMATCH => "coinbase-id-mismatch",
        REJECT_BAD_NTIME => "bad-ntime",
        REJECT_BAD_VERSION => "bad-version",
        REJECT_STALE_BLOCK => "stale",
        REJECT_BAD_COINBASE => "bad-coinbase",
        REJECT_BAD_COINBASE_OUTPUTS => "bad-coinbase-outputs",
        REJECT_MISSING_POOL_TAG => "missing-pool-tag",
        REJECT_DUPLICATE_WORK => "duplicate",
        _ => "other",
    }
}

// ---------------------------------------------------------------------------------------
// Client → server
// ---------------------------------------------------------------------------------------

/// Everything a gateway may send on the mining channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMsg {
    CoinbaserRequest(CoinbaserRequest),
    Pow(Box<PowSubmit>),
    JobValidation(JobValidationReply),
    /// A sub-command this Prime does not know. Ignore it; newer clients add optional ones.
    Unknown(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaserRequest {
    /// Coinbase value of the template the gateway is about to mine.
    pub value: u64,
    /// The template's previous block hash, internal byte order.
    pub prev_hash: [u8; 32],
}

/// Job data the gateway sends the first time a job slot is used (and again whenever the
/// slot moves to a new job).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSection {
    pub prev_hash: [u8; 32],
    /// Offset into the assembled coinbase (`coinb1 || extranonce || coinb2`) of the byte
    /// carrying the share's power-of-two difficulty.
    pub target_byte_index: u16,
    /// Compact target, little-endian bytes of the `u32`.
    pub nbits: [u8; 4],
    /// Which coinbaser reply the job's split came from.
    pub coinbaser_id: u8,
    pub height: u32,
    pub coinbase_value: u64,
    pub txn_count: u32,
    pub txn_total_weight: u32,
    pub txn_total_size: u32,
    pub txn_total_sigops: u32,
    /// SHA256d merkle branches for the coinbase, internal byte order.
    pub merkle_branches: Vec<[u8; 32]>,
}

impl JobSection {
    pub fn nbits_u32(&self) -> u32 {
        u32::from_le_bytes(self.nbits)
    }
}

/// One of the gateway's coinbase variants for the job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseSection {
    /// 0..=5 for a real coinbase, 0xFF for the subsidy-only one.
    pub coinbase_id: u8,
    pub coinb1: Vec<u8>,
    pub coinb2: Vec<u8>,
}

/// The 64-bit hasher fields a BLAKE2b (Sia-layout) miner returns.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Blake2bSection {
    /// Raw 8 bytes at offset 40 of the 80-byte work: `time_offset (4 LE) || nonce3 (4 LE)`.
    pub ntime: [u8; 8],
    /// Raw 8 bytes at offset 32 of the 80-byte work: `nonce (4 LE) || nonce2 (4 LE)`.
    pub nonce: [u8; 8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowSubmit {
    pub job_id: u8,
    pub coinbase_id: u8,
    pub flags: u8,
    /// Power-of-two share difficulty the gateway wrote into the coinbase.
    pub target_pot: u8,
    pub ntime32: u32,
    pub nonce32: u32,
    pub version: u32,
    pub extranonce: [u8; EXTRANONCE_LEN],
    pub username: String,
    pub reserved: [u8; 4],
    pub blake2b: Option<Blake2bSection>,
    /// The header's serialized time (present on BLAKE2b shares).
    pub time_on_wire: Option<u32>,
    pub job: Option<JobSection>,
    pub coinbase: Option<CoinbaseSection>,
}

impl PowSubmit {
    pub fn is_block(&self) -> bool {
        self.flags & FLAG_BLOCK != 0
    }
    pub fn subsidy_only(&self) -> bool {
        self.flags & FLAG_SUBSIDY_ONLY != 0
    }
    pub fn quickdiff(&self) -> bool {
        self.flags & FLAG_QUICKDIFF != 0
    }
    /// The fte/Convoy lineage sets `FLAG_BLAKE2B` *and* sends the 0x03 section; the
    /// iohzrd lineage sends only the section (its algorithm byte is the signal). Either
    /// marks a BLAKE2b share.
    pub fn is_blake2b(&self) -> bool {
        self.flags & FLAG_BLAKE2B != 0 || self.blake2b.is_some()
    }
    pub fn use_time_offset(&self) -> bool {
        self.reserved[0] & RESERVED_USE_TIME_OFFSET != 0
    }
    /// Work the share claims, in units of a difficulty-1 share.
    pub fn claimed_work(&self) -> u64 {
        if self.target_pot >= 64 {
            u64::MAX
        } else {
            1u64 << self.target_pot
        }
    }

    /// Serialize as a gateway would. Used by tests and the bundled test client.
    pub fn encode(&self) -> Vec<u8> {
        let mut m = Vec::with_capacity(256);
        m.push(SUB_POW);
        m.push(self.job_id);
        m.push(self.coinbase_id);
        m.push(self.flags);
        m.push(self.target_pot);
        m.extend_from_slice(&self.ntime32.to_le_bytes());
        m.extend_from_slice(&self.nonce32.to_le_bytes());
        m.extend_from_slice(&self.version.to_le_bytes());
        m.push(EXTRANONCE_LEN as u8);
        m.extend_from_slice(&self.extranonce);
        let u = self.username.as_bytes();
        m.extend_from_slice(&u[..u.len().min(MAX_USERNAME)]);
        m.push(0);
        m.extend_from_slice(&self.reserved);
        if let Some(b) = &self.blake2b {
            m.push(0x03);
            m.push(POW_ALGO_BLAKE2B);
            m.extend_from_slice(&b.ntime);
            m.extend_from_slice(&b.nonce);
            if let Some(t) = self.time_on_wire {
                m.push(0x04);
                m.extend_from_slice(&t.to_le_bytes());
            }
        }
        if let Some(j) = &self.job {
            m.push(0x01);
            m.extend_from_slice(&j.prev_hash);
            m.extend_from_slice(&j.target_byte_index.to_le_bytes());
            m.extend_from_slice(&j.nbits);
            m.push(j.coinbaser_id);
            m.extend_from_slice(&j.height.to_le_bytes());
            m.extend_from_slice(&j.coinbase_value.to_le_bytes());
            m.extend_from_slice(&j.txn_count.to_le_bytes());
            m.extend_from_slice(&j.txn_total_weight.to_le_bytes());
            m.extend_from_slice(&j.txn_total_size.to_le_bytes());
            m.extend_from_slice(&j.txn_total_sigops.to_le_bytes());
            m.push(j.merkle_branches.len() as u8);
            for b in &j.merkle_branches {
                m.extend_from_slice(b);
            }
        }
        if let Some(c) = &self.coinbase {
            m.push(0x02);
            m.push(c.coinbase_id);
            m.extend_from_slice(&(c.coinb1.len() as u16).to_le_bytes());
            m.extend_from_slice(&(c.coinb2.len() as u16).to_le_bytes());
            m.extend_from_slice(&c.coinb1);
            m.extend_from_slice(&c.coinb2);
        }
        m.push(END);
        m
    }

    /// Parse the bytes after the 0x27 sub-command byte.
    pub fn decode(body: &[u8]) -> Result<PowSubmit> {
        let mut c = Cursor::new(body);
        let job_id = c.u8()?;
        let coinbase_id = c.u8()?;
        let flags = c.u8()?;
        let target_pot = c.u8()?;
        let ntime32 = c.u32()?;
        let nonce32 = c.u32()?;
        let version = c.u32()?;
        if c.u8()? as usize != EXTRANONCE_LEN {
            return Err(Error::Malformed("extranonce size"));
        }
        let extranonce = c.array::<EXTRANONCE_LEN>()?;
        let username = String::from_utf8_lossy(c.cstr(MAX_USERNAME)?).into_owned();
        let reserved = c.array::<4>()?;

        let mut s = PowSubmit {
            job_id,
            coinbase_id,
            flags,
            target_pot,
            ntime32,
            nonce32,
            version,
            extranonce,
            username,
            reserved,
            blake2b: None,
            time_on_wire: None,
            job: None,
            coinbase: None,
        };

        loop {
            match c.u8()? {
                END => break,
                0x03 => {
                    if c.u8()? != POW_ALGO_BLAKE2B {
                        return Err(Error::Malformed("pow algorithm"));
                    }
                    let ntime = c.array::<8>()?;
                    let nonce = c.array::<8>()?;
                    s.blake2b = Some(Blake2bSection { ntime, nonce });
                }
                0x04 => s.time_on_wire = Some(c.u32()?),
                0x01 => {
                    let prev_hash = c.array::<32>()?;
                    let target_byte_index = c.u16()?;
                    let nbits = c.array::<4>()?;
                    let coinbaser_id = c.u8()?;
                    let height = c.u32()?;
                    let coinbase_value = c.u64()?;
                    let txn_count = c.u32()?;
                    let txn_total_weight = c.u32()?;
                    let txn_total_size = c.u32()?;
                    let txn_total_sigops = c.u32()?;
                    let n = c.u8()? as usize;
                    let mut merkle_branches = Vec::with_capacity(n);
                    for _ in 0..n {
                        merkle_branches.push(c.array::<32>()?);
                    }
                    s.job = Some(JobSection {
                        prev_hash,
                        target_byte_index,
                        nbits,
                        coinbaser_id,
                        height,
                        coinbase_value,
                        txn_count,
                        txn_total_weight,
                        txn_total_size,
                        txn_total_sigops,
                        merkle_branches,
                    });
                }
                0x02 => {
                    let id = c.u8()?;
                    let l1 = c.u16()? as usize;
                    let l2 = c.u16()? as usize;
                    let coinb1 = c.take(l1)?.to_vec();
                    let coinb2 = c.take(l2)?.to_vec();
                    s.coinbase = Some(CoinbaseSection { coinbase_id: id, coinb1, coinb2 });
                }
                _ => return Err(Error::Malformed("pow section")),
            }
        }
        Ok(s)
    }
}

/// Status byte in a job-validation reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationStatus {
    Ok,
    /// 0xF0 unknown job, 0xF1 no template, 0xF2 too many transactions, 0xF3 bad slot,
    /// 0xF4 bad request.
    Error(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobValidationReply {
    /// 0x90: 48-bit SipHash ids of every transaction, plus a running XOR of the raw ids.
    ShortIds { job: u8, status: ValidationStatus, ids: Vec<u64>, crosscheck: Option<[u8; 32]> },
    /// 0x91: the raw transactions we asked for by index.
    Transactions { job: u8, status: ValidationStatus, txns: Vec<Vec<u8>> },
    /// 0x92: every raw transaction in the job except the coinbase.
    FullBlock { job: u8, status: ValidationStatus, txns: Vec<Vec<u8>> },
}

fn read_status(c: &mut Cursor) -> Result<ValidationStatus> {
    Ok(match c.u8()? {
        0x01 => ValidationStatus::Ok,
        e => ValidationStatus::Error(e),
    })
}

fn read_txn_list(c: &mut Cursor) -> Result<Vec<Vec<u8>>> {
    let n = c.u16()? as usize;
    let mut txns = Vec::with_capacity(n);
    for _ in 0..n {
        let lo = c.u16()? as usize;
        let hi = c.u8()? as usize;
        txns.push(c.take(lo | (hi << 16))?.to_vec());
    }
    Ok(txns)
}

impl JobValidationReply {
    /// Parse the bytes after the 0x50 sub-command byte.
    pub fn decode(body: &[u8]) -> Result<Self> {
        let mut c = Cursor::new(body);
        let kind = c.u8()?;
        let job = c.u8()?;
        let status = read_status(&mut c)?;
        Ok(match kind {
            0x90 => {
                if status != ValidationStatus::Ok {
                    return Ok(JobValidationReply::ShortIds { job, status, ids: vec![], crosscheck: None });
                }
                let n = c.u16()? as usize;
                let mut ids = Vec::with_capacity(n);
                for _ in 0..n {
                    let lo = c.u32()? as u64;
                    let hi = c.u16()? as u64;
                    ids.push(lo | (hi << 32));
                }
                let crosscheck = if n > 0 { Some(c.array::<32>()?) } else { None };
                JobValidationReply::ShortIds { job, status, ids, crosscheck }
            }
            0x91 => {
                let txns = if status == ValidationStatus::Ok { read_txn_list(&mut c)? } else { vec![] };
                JobValidationReply::Transactions { job, status, txns }
            }
            0x92 => {
                let txns = if status == ValidationStatus::Ok { read_txn_list(&mut c)? } else { vec![] };
                JobValidationReply::FullBlock { job, status, txns }
            }
            _ => return Err(Error::Malformed("job validation kind")),
        })
    }

    pub fn job(&self) -> u8 {
        match self {
            JobValidationReply::ShortIds { job, .. }
            | JobValidationReply::Transactions { job, .. }
            | JobValidationReply::FullBlock { job, .. } => *job,
        }
    }
}

/// Parse a decrypted client mining payload.
pub fn parse_client(payload: &[u8]) -> Result<ClientMsg> {
    let (&sub, body) = payload.split_first().ok_or(Error::Short)?;
    Ok(match sub {
        SUB_COINBASER_REQUEST => {
            let mut c = Cursor::new(body);
            let value = c.u64()?;
            let prev_hash = c.array::<32>()?;
            if c.u8()? != END {
                return Err(Error::Malformed("coinbaser request"));
            }
            ClientMsg::CoinbaserRequest(CoinbaserRequest { value, prev_hash })
        }
        SUB_POW => ClientMsg::Pow(Box::new(PowSubmit::decode(body)?)),
        SUB_JOB_VALIDATION => ClientMsg::JobValidation(JobValidationReply::decode(body)?),
        other => ClientMsg::Unknown(other),
    })
}

// ---------------------------------------------------------------------------------------
// Server → client
// ---------------------------------------------------------------------------------------

/// Convoy configure flag: the pool does not run ABW, so the gateway must not wait for an
/// assignment before building templates.
pub const CONFIG_FLAG_ABW_DISABLED: u8 = 0x01;
pub const RESUME_TOKEN_LEN: usize = 40;

/// Caps the newest Convoy-lineage gateways apply when parsing a configure: a pool script
/// over [`MAX_CONFIG_POOL_SCRIPT`] bytes is a malformed configure, and a coinbase tag that
/// reaches [`MAX_CONFIG_TAG`] is refused outright as one that could never fit. Both are far
/// above what a real configure carries, and both are hard rejections, so stay inside them.
pub const MAX_CONFIG_POOL_SCRIPT: usize = 83;
pub const MAX_CONFIG_TAG: usize = 81;

/// Client configuration body (before signing), **version 1** — OCEAN-lineage gateways
/// including the BLAKE2b forks.
///
/// `pool_script` is the scriptPubKey the gateway pays the remainder to; `tag` is the
/// primary coinbase tag; `vardiff_min` must be a power of two.
pub fn configure_v1(pool_script: &[u8], prime_id: u32, tag: &str, vardiff_min: u64) -> Vec<u8> {
    debug_assert!(pool_script.len() <= MAX_CONFIG_POOL_SCRIPT && tag.len() <= MAX_CONFIG_TAG);
    debug_assert!(vardiff_min.is_power_of_two());
    let mut m = Vec::with_capacity(20 + pool_script.len() + tag.len());
    m.push(SUB_CONFIGURE);
    m.push(1);
    m.push(pool_script.len() as u8);
    m.extend_from_slice(pool_script);
    m.extend_from_slice(&prime_id.to_le_bytes());
    m.push(tag.len() as u8);
    m.extend_from_slice(tag.as_bytes());
    m.extend_from_slice(&vardiff_min.to_le_bytes());
    m.push(0);
    m.push(END);
    m
}

/// Client configuration body, **version 3** — Convoy-lineage gateways.
///
/// Layout: `3 | script | prime_id u64 | resume token (40) | tag | vardiff_min u64 |
/// flags | 0xFE`. A gateway that presented the same `prime_id`/token in its hello treats
/// the connection as resumed and replays unanswered shares; any other value makes it
/// drop its queue and start clean. No bulk-frame marker is appended, so the gateway
/// stays on plain per-message receipts.
pub fn configure_v3(
    pool_script: &[u8],
    prime_id: u64,
    resume_token: &[u8; RESUME_TOKEN_LEN],
    tag: &str,
    vardiff_min: u64,
) -> Vec<u8> {
    debug_assert!(pool_script.len() <= MAX_CONFIG_POOL_SCRIPT && tag.len() <= MAX_CONFIG_TAG);
    debug_assert!(vardiff_min.is_power_of_two());
    let mut m = Vec::with_capacity(64 + pool_script.len() + tag.len());
    m.push(SUB_CONFIGURE);
    m.push(3);
    m.push(pool_script.len() as u8);
    m.extend_from_slice(pool_script);
    m.extend_from_slice(&prime_id.to_le_bytes());
    m.extend_from_slice(resume_token);
    m.push(tag.len() as u8);
    m.extend_from_slice(tag.as_bytes());
    m.extend_from_slice(&vardiff_min.to_le_bytes());
    m.push(CONFIG_FLAG_ABW_DISABLED);
    m.push(END);
    m
}

/// Coinbaser reply for a request that asked about `value`.
pub fn coinbaser_reply(value: u64, coinbaser_v2: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(13 + coinbaser_v2.len());
    m.push(SUB_COINBASER_REPLY);
    m.extend_from_slice(&value.to_le_bytes());
    m.extend_from_slice(&(coinbaser_v2.len() as u32).to_le_bytes());
    m.extend_from_slice(coinbaser_v2);
    m
}

/// Share receipt. `reason` is only meaningful when `status == REJECTED`.
pub fn share_receipt(status: u8, reason: u16, nonce32: u32, target_pot: u8, job_id: u8) -> [u8; 10] {
    let mut m = [0u8; 10];
    m[0] = SUB_SHARE_RECEIPT;
    m[1] = status;
    m[2..4].copy_from_slice(&reason.to_le_bytes());
    m[4..8].copy_from_slice(&nonce32.to_le_bytes());
    m[8] = target_pot;
    m[9] = job_id;
    m
}

pub fn request_short_ids(job: u8) -> [u8; 3] {
    [SUB_JOB_VALIDATION, 0x10, job]
}

pub fn request_transactions(job: u8, indexes: &[u16]) -> Vec<u8> {
    let mut m = Vec::with_capacity(5 + 2 * indexes.len());
    m.push(SUB_JOB_VALIDATION);
    m.push(0x11);
    m.push(job);
    m.extend_from_slice(&(indexes.len() as u16).to_le_bytes());
    for i in indexes {
        m.extend_from_slice(&i.to_le_bytes());
    }
    m
}

pub fn request_full_block(job: u8) -> [u8; 3] {
    [SUB_JOB_VALIDATION, 0x12, job]
}

pub fn block_notify() -> [u8; 1] {
    [SUB_BLOCK_NOTIFY]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PowSubmit {
        PowSubmit {
            job_id: 3,
            coinbase_id: 4,
            flags: FLAG_BLAKE2B | FLAG_QUICKDIFF,
            target_pot: 13,
            ntime32: 0x0102_0304,
            nonce32: 0x0a0b_0c0d,
            version: 0xa000_0000,
            extranonce: [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab],
            username: "bc1qexample.rig1".into(),
            reserved: [RESERVED_USE_TIME_OFFSET, 0, 0, 0],
            blake2b: Some(Blake2bSection { ntime: [1, 2, 3, 4, 5, 6, 7, 8], nonce: [9, 10, 11, 12, 13, 14, 15, 16] }),
            time_on_wire: Some(1_788_400_000),
            job: Some(JobSection {
                prev_hash: [0xc0; 32],
                target_byte_index: 47,
                nbits: 0x193c_2d40u32.to_le_bytes(),
                coinbaser_id: 200,
                height: 966_267,
                coinbase_value: 312_538_966,
                txn_count: 58,
                txn_total_weight: 1,
                txn_total_size: 2,
                txn_total_sigops: 3,
                merkle_branches: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            }),
            coinbase: Some(CoinbaseSection { coinbase_id: 4, coinb1: vec![1, 2, 3, 4, 5], coinb2: vec![6, 7] }),
        }
    }

    #[test]
    fn pow_round_trip_with_trailing_pad() {
        let s = sample();
        let mut wire = s.encode();
        assert_eq!(wire[0], SUB_POW);
        wire.extend_from_slice(&[0x55; 40]); // client pads after END
        match parse_client(&wire).unwrap() {
            ClientMsg::Pow(p) => assert_eq!(*p, s),
            other => panic!("{other:?}"),
        }
        assert!(s.use_time_offset());
        assert!(s.is_blake2b() && s.quickdiff() && !s.is_block());
        assert_eq!(s.claimed_work(), 8192);
    }

    /// The iohzrd gateway leaves `FLAG_BLAKE2B` clear and marks the algorithm only in the
    /// 0x03 section; a share with neither is SHA256d.
    #[test]
    fn blake2b_detected_from_section_alone() {
        let mut s = sample();
        s.flags = FLAG_QUICKDIFF;
        let wire = s.encode();
        match parse_client(&wire).unwrap() {
            ClientMsg::Pow(p) => {
                assert!(p.is_blake2b());
                assert_eq!(*p, s);
            }
            other => panic!("{other:?}"),
        }
        s.blake2b = None;
        assert!(!s.is_blake2b());
    }

    #[test]
    fn pow_without_optional_sections() {
        let mut s = sample();
        s.job = None;
        s.coinbase = None;
        s.blake2b = None;
        s.time_on_wire = None;
        let wire = s.encode();
        assert_eq!(PowSubmit::decode(&wire[1..]).unwrap(), s);
    }

    #[test]
    fn pow_rejects_garbage() {
        let mut wire = sample().encode();
        let end = wire.len() - 1;
        wire[end] = 0x77; // unknown section instead of END
        assert!(PowSubmit::decode(&wire[1..]).is_err());
        let mut wire = sample().encode();
        wire[17] = 8; // extranonce size
        assert_eq!(PowSubmit::decode(&wire[1..]), Err(Error::Malformed("extranonce size")));
        assert!(PowSubmit::decode(&wire[1..20]).is_err());
    }

    #[test]
    fn coinbaser_request_parses() {
        let mut m = vec![SUB_COINBASER_REQUEST];
        m.extend_from_slice(&312_538_966u64.to_le_bytes());
        m.extend_from_slice(&[7u8; 32]);
        m.push(END);
        m.extend_from_slice(&[1, 2, 3]);
        assert_eq!(
            parse_client(&m).unwrap(),
            ClientMsg::CoinbaserRequest(CoinbaserRequest { value: 312_538_966, prev_hash: [7u8; 32] })
        );
        assert_eq!(parse_client(&[0x42, 1, 2]).unwrap(), ClientMsg::Unknown(0x42));
        assert_eq!(parse_client(&[]), Err(Error::Short));
    }

    #[test]
    fn validation_replies_parse() {
        // full block with two txns
        let mut m = vec![SUB_JOB_VALIDATION, 0x92, 5, 0x01, 2, 0];
        m.extend_from_slice(&[3, 0, 0, 0xaa, 0xbb, 0xcc]);
        m.extend_from_slice(&[1, 0, 0, 0xdd]);
        m.push(END);
        match parse_client(&m).unwrap() {
            ClientMsg::JobValidation(JobValidationReply::FullBlock { job, status, txns }) => {
                assert_eq!(job, 5);
                assert_eq!(status, ValidationStatus::Ok);
                assert_eq!(txns, vec![vec![0xaa, 0xbb, 0xcc], vec![0xdd]]);
            }
            other => panic!("{other:?}"),
        }
        // error reply
        let m = [SUB_JOB_VALIDATION, 0x91, 0xFF, 0xF3];
        match parse_client(&m).unwrap() {
            ClientMsg::JobValidation(JobValidationReply::Transactions { status, txns, .. }) => {
                assert_eq!(status, ValidationStatus::Error(0xF3));
                assert!(txns.is_empty());
            }
            other => panic!("{other:?}"),
        }
        // short ids
        let mut m = vec![SUB_JOB_VALIDATION, 0x90, 1, 0x01, 1, 0];
        m.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        m.extend_from_slice(&[9u8; 32]);
        m.push(END);
        match parse_client(&m).unwrap() {
            ClientMsg::JobValidation(JobValidationReply::ShortIds { ids, crosscheck, .. }) => {
                assert_eq!(ids, vec![0x6655_4433_2211]);
                assert_eq!(crosscheck, Some([9u8; 32]));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn server_messages_have_the_documented_layout() {
        let c = configure_v1(&[0x00, 0x14, 0xaa], 1, "Lazarus", 1024);
        assert_eq!(c[0], SUB_CONFIGURE);
        assert_eq!(c[1], 1);
        assert_eq!(c[2], 3);
        assert_eq!(&c[3..6], &[0x00, 0x14, 0xaa]);
        assert_eq!(&c[6..10], &1u32.to_le_bytes());
        assert_eq!(c[10], 7);
        assert_eq!(&c[11..18], b"Lazarus");
        assert_eq!(&c[18..26], &1024u64.to_le_bytes());
        assert_eq!(&c[26..], &[0, END]);

        let tok = [0x5a; RESUME_TOKEN_LEN];
        let c = configure_v3(&[0x00, 0x14, 0xaa], 0x0102_0304_0506_0708, &tok, "Lazarus", 1024);
        assert_eq!(c[1], 3);
        assert_eq!(&c[3..6], &[0x00, 0x14, 0xaa]);
        assert_eq!(&c[6..14], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&c[14..54], &tok);
        assert_eq!(c[54], 7);
        assert_eq!(&c[55..62], b"Lazarus");
        assert_eq!(&c[62..70], &1024u64.to_le_bytes());
        assert_eq!(&c[70..], &[CONFIG_FLAG_ABW_DISABLED, END]);

        let r = coinbaser_reply(99, &[1, 2, 3]);
        assert_eq!(r[0], SUB_COINBASER_REPLY);
        assert_eq!(&r[1..9], &99u64.to_le_bytes());
        assert_eq!(&r[9..13], &3u32.to_le_bytes());
        assert_eq!(&r[13..], &[1, 2, 3]);

        let s = share_receipt(REJECTED, REJECT_HIGH_HASH, 0xdead_beef, 5, 2);
        assert_eq!(s, [SUB_SHARE_RECEIPT, REJECTED, 21, 0, 0xef, 0xbe, 0xad, 0xde, 5, 2]);

        assert_eq!(request_transactions(2, &[1, 300]), vec![SUB_JOB_VALIDATION, 0x11, 2, 2, 0, 1, 0, 44, 1]);
        assert_eq!(request_full_block(6), [SUB_JOB_VALIDATION, 0x12, 6]);
    }
}
