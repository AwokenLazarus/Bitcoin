//! Mining subcommands (proto_cmd=5). Recovered from OCEAN datum_protocol.c.

use crate::header::{Header, ProtoCmd};
use crate::channel::ChannelKeys;
use crate::keys::SessionKeys;
use crate::nacl;

pub const SUB_COINBASER_REQ: u8 = 0x10;
pub const SUB_COINBASER_RESP: u8 = 0x11;
pub const SUB_SHARE: u8 = 0x27;
pub const SUB_SHARE_RESP: u8 = 0x8F;
pub const SUB_CONFIG: u8 = 0x99;
pub const SUB_BLOCKNOTIFY: u8 = 0xF9;

pub const SHARE_ACCEPT: u8 = 0x50;
pub const SHARE_ACCEPT_TENTATIVE: u8 = 0x55;
pub const SHARE_REJECT: u8 = 0x66;

pub const REJECT_BAD_JOB_ID: u16 = 10;
pub const REJECT_BAD_COINBASE_ID: u16 = 11;
pub const REJECT_BAD_EXTRANONCE_SIZE: u16 = 12;
pub const REJECT_BAD_TARGET: u16 = 13;
pub const REJECT_BAD_USERNAME: u16 = 14;
pub const REJECT_STALE: u16 = 25;
pub const REJECT_HIGH_HASH: u16 = 21;
pub const REJECT_DUPLICATE: u16 = 29;
pub const REJECT_OTHER: u16 = 30;

pub const SECTION_JOB: u8 = 0x01;
pub const SECTION_COINBASE: u8 = 0x02;
pub const SECTION_BLAKE2B: u8 = 0x03;
pub const STRUCT_END: u8 = 0xFE;

#[derive(Clone, Debug)]
pub struct CoinbaserRequest {
    pub value: u64,
    pub prevhash: [u8; 32],
}

impl CoinbaserRequest {
    pub fn decode(body: &[u8]) -> Option<Self> {
        // body is after the 0x10 byte
        if body.len() < 40 {
            return None;
        }
        let value = u64::from_le_bytes(body[0..8].try_into().ok()?);
        let mut prevhash = [0u8; 32];
        prevhash.copy_from_slice(&body[8..40]);
        Some(Self { value, prevhash })
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![SUB_COINBASER_REQ];
        o.extend_from_slice(&self.value.to_le_bytes());
        o.extend_from_slice(&self.prevhash);
        o.push(STRUCT_END);
        o
    }
}

pub fn encode_coinbaser_resp(value: u64, blob: &[u8]) -> Vec<u8> {
    let mut o = vec![SUB_COINBASER_RESP];
    o.extend_from_slice(&value.to_le_bytes());
    o.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    o.extend_from_slice(blob);
    o
}

/// Config 0x99 payload (without the 0x99 byte): version 1 + script + prime_id + tag + min_diff + 0x00 0xFE.
pub fn encode_config(script: &[u8], prime_id: u32, tag: &str, min_diff: u64) -> Vec<u8> {
    let mut o = vec![SUB_CONFIG, 1, script.len() as u8];
    o.extend_from_slice(script);
    o.extend_from_slice(&prime_id.to_le_bytes());
    let tb = tag.as_bytes();
    o.push(tb.len() as u8);
    o.extend_from_slice(tb);
    o.extend_from_slice(&min_diff.to_le_bytes());
    o.push(0);
    o.push(STRUCT_END);
    o
}

pub fn encode_share_response(accept: bool, reason: u16, nonce: u32, pot: u8, job_id: u8) -> Vec<u8> {
    let mut o = vec![SUB_SHARE_RESP];
    o.push(if accept { SHARE_ACCEPT } else { SHARE_REJECT });
    o.extend_from_slice(&reason.to_le_bytes());
    o.extend_from_slice(&nonce.to_le_bytes());
    o.push(pot);
    o.push(job_id);
    o
}

pub fn encode_blocknotify() -> Vec<u8> {
    vec![SUB_BLOCKNOTIFY]
}

#[derive(Clone, Debug)]
pub struct JobSection {
    pub prev_hash: [u8; 32],
    pub target_byte_index: u16,
    pub nbits: [u8; 4],
    pub coinbaser_id: u8,
    pub height: u32,
    pub coinbase_value: u64,
    pub txn_count: u32,
    pub txn_total_weight: u32,
    pub txn_total_size: u32,
    pub txn_total_sigops: u32,
    pub merkle_branches: Vec<[u8; 32]>,
}

#[derive(Clone, Debug)]
pub struct CoinbaseSection {
    pub coinbase_id: u8,
    pub coinb1: Vec<u8>,
    pub coinb2: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Blake2bSection {
    pub sia_ntime: [u8; 8],
    pub sia_nonce: [u8; 8],
    pub time_on_wire: u32,
}

#[derive(Clone, Debug)]
pub struct PowSubmit {
    pub job_id: u8,
    pub coinbase_id: u8,
    pub is_block: bool,
    pub subsidy_only: bool,
    pub quickdiff: bool,
    pub target_byte: u8,
    pub ntime: u32,
    pub nonce: u32,
    pub version: u32,
    pub extranonce: Vec<u8>,
    pub username: String,
    pub use_time_offset: bool,
    pub job: Option<JobSection>,
    pub coinbase: Option<CoinbaseSection>,
    pub blake2b: Option<Blake2bSection>,
}

impl PowSubmit {
    pub fn difficulty(&self) -> u64 {
        1u64 << (self.target_byte.min(63))
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut i = 0;
        if data.is_empty() {
            return None;
        }
        if data[0] == SUB_SHARE {
            i = 1;
        }
        if i + 17 > data.len() {
            return None;
        }
        let job_id = data[i];
        let coinbase_id = data[i + 1];
        let flags = data[i + 2];
        let target_byte = data[i + 3];
        let ntime = u32::from_le_bytes(data[i + 4..i + 8].try_into().ok()?);
        let nonce = u32::from_le_bytes(data[i + 8..i + 12].try_into().ok()?);
        let version = u32::from_le_bytes(data[i + 12..i + 16].try_into().ok()?);
        let en_size = data[i + 16] as usize;
        i += 17;
        if en_size != 12 || i + en_size > data.len() {
            return None;
        }
        let extranonce = data[i..i + en_size].to_vec();
        i += en_size;
        let rest = &data[i..];
        let nul = rest.iter().take(385).position(|&b| b == 0)?;
        let username = String::from_utf8_lossy(&rest[..nul]).into_owned();
        i += nul + 1;
        if i + 4 > data.len() {
            return None;
        }
        let use_time_offset = data[i] & 1 != 0;
        i += 4;
        let mut job = None;
        let mut coinbase = None;
        let mut blake2b = None;
        while i < data.len() {
            let m = data[i];
            i += 1;
            match m {
                STRUCT_END => break,
                SECTION_JOB => {
                    if i + 32 + 2 + 4 + 1 + 4 + 8 + 16 + 1 > data.len() {
                        return None;
                    }
                    let mut prev_hash = [0u8; 32];
                    prev_hash.copy_from_slice(&data[i..i + 32]);
                    i += 32;
                    let target_byte_index = u16::from_le_bytes(data[i..i + 2].try_into().ok()?);
                    i += 2;
                    let mut nbits = [0u8; 4];
                    nbits.copy_from_slice(&data[i..i + 4]);
                    i += 4;
                    let coinbaser_id = data[i];
                    i += 1;
                    let height = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    let coinbase_value = u64::from_le_bytes(data[i..i + 8].try_into().ok()?);
                    i += 8;
                    let txn_count = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    let txn_total_weight = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    let txn_total_size = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    let txn_total_sigops = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    let n = data[i] as usize;
                    i += 1;
                    if n > 24 || i + n * 32 > data.len() {
                        return None;
                    }
                    let mut merkle_branches = Vec::with_capacity(n);
                    for _ in 0..n {
                        let mut b = [0u8; 32];
                        b.copy_from_slice(&data[i..i + 32]);
                        merkle_branches.push(b);
                        i += 32;
                    }
                    job = Some(JobSection {
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
                SECTION_COINBASE => {
                    if i + 1 + 4 > data.len() {
                        return None;
                    }
                    let coinbase_id = data[i];
                    i += 1;
                    let len1 = u16::from_le_bytes(data[i..i + 2].try_into().ok()?) as usize;
                    i += 2;
                    let len2 = u16::from_le_bytes(data[i..i + 2].try_into().ok()?) as usize;
                    i += 2;
                    if i + len1 + len2 > data.len() {
                        return None;
                    }
                    let coinb1 = data[i..i + len1].to_vec();
                    i += len1;
                    let coinb2 = data[i..i + len2].to_vec();
                    i += len2;
                    coinbase = Some(CoinbaseSection {
                        coinbase_id,
                        coinb1,
                        coinb2,
                    });
                }
                SECTION_BLAKE2B => {
                    if i + 1 + 8 + 8 + 1 + 4 > data.len() {
                        return None;
                    }
                    if data[i] != 0x01 {
                        return None;
                    }
                    i += 1;
                    let mut sia_ntime = [0u8; 8];
                    sia_ntime.copy_from_slice(&data[i..i + 8]);
                    i += 8;
                    let mut sia_nonce = [0u8; 8];
                    sia_nonce.copy_from_slice(&data[i..i + 8]);
                    i += 8;
                    if data[i] != 0x04 {
                        return None;
                    }
                    i += 1;
                    let time_on_wire = u32::from_le_bytes(data[i..i + 4].try_into().ok()?);
                    i += 4;
                    blake2b = Some(Blake2bSection {
                        sia_ntime,
                        sia_nonce,
                        time_on_wire,
                    });
                }
                _ => return None,
            }
        }
        Some(Self {
            job_id,
            coinbase_id,
            is_block: flags & 1 != 0,
            subsidy_only: flags & 2 != 0,
            quickdiff: flags & 4 != 0,
            target_byte,
            ntime,
            nonce,
            version,
            extranonce,
            username,
            use_time_offset,
            job,
            coinbase,
            blake2b,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![SUB_SHARE];
        o.push(self.job_id);
        o.push(self.coinbase_id);
        let mut flags = 0u8;
        if self.is_block {
            flags |= 1;
        }
        if self.subsidy_only {
            flags |= 2;
        }
        if self.quickdiff {
            flags |= 4;
        }
        o.push(flags);
        o.push(self.target_byte);
        o.extend_from_slice(&self.ntime.to_le_bytes());
        o.extend_from_slice(&self.nonce.to_le_bytes());
        o.extend_from_slice(&self.version.to_le_bytes());
        o.push(12);
        let mut en = self.extranonce.clone();
        en.resize(12, 0);
        o.extend_from_slice(&en[..12]);
        o.extend_from_slice(self.username.as_bytes());
        o.push(0);
        o.push(if self.use_time_offset { 1 } else { 0 });
        o.extend_from_slice(&[0, 0, 0]);
        if let Some(j) = &self.job {
            o.push(SECTION_JOB);
            o.extend_from_slice(&j.prev_hash);
            o.extend_from_slice(&j.target_byte_index.to_le_bytes());
            o.extend_from_slice(&j.nbits);
            o.push(j.coinbaser_id);
            o.extend_from_slice(&j.height.to_le_bytes());
            o.extend_from_slice(&j.coinbase_value.to_le_bytes());
            o.extend_from_slice(&j.txn_count.to_le_bytes());
            o.extend_from_slice(&j.txn_total_weight.to_le_bytes());
            o.extend_from_slice(&j.txn_total_size.to_le_bytes());
            o.extend_from_slice(&j.txn_total_sigops.to_le_bytes());
            o.push(j.merkle_branches.len() as u8);
            for b in &j.merkle_branches {
                o.extend_from_slice(b);
            }
        }
        if let Some(c) = &self.coinbase {
            o.push(SECTION_COINBASE);
            o.push(c.coinbase_id);
            o.extend_from_slice(&(c.coinb1.len() as u16).to_le_bytes());
            o.extend_from_slice(&(c.coinb2.len() as u16).to_le_bytes());
            o.extend_from_slice(&c.coinb1);
            o.extend_from_slice(&c.coinb2);
        }
        if let Some(b) = &self.blake2b {
            o.push(SECTION_BLAKE2B);
            o.push(0x01);
            o.extend_from_slice(&b.sia_ntime);
            o.extend_from_slice(&b.sia_nonce);
            o.push(0x04);
            o.extend_from_slice(&b.time_on_wire.to_le_bytes());
        }
        o.push(STRUCT_END);
        o
    }
}

/// Wrap a mining body: optionally sign with session ed, then channel-encrypt, then header.
pub fn wrap_mining(
    ch: &mut ChannelKeys,
    body: &[u8],
    sign_with: Option<&SessionKeys>,
) -> Vec<u8> {
    let mut plain = body.to_vec();
    let signed = sign_with.is_some();
    if let Some(sk) = sign_with {
        if let Ok(sig) = nacl::sign_detached(&plain, &sk.ed_sk) {
            plain.extend_from_slice(&sig);
        }
    }
    let cipher = ch.seal_channel(&plain);
    let h = Header {
        cmd_len: cipher.len() as u32,
        is_signed: signed,
        is_encrypted_pubkey: false,
        is_encrypted_channel: true,
        proto_cmd: ProtoCmd::Mining as u8,
    };
    let mut out = h.encode_obfuscated(ch.next_send_hdr()).to_vec();
    out.extend_from_slice(&cipher);
    out
}

/// Open a channel-encrypted mining (or other) frame. Returns (header, plaintext without sig if signed).
pub fn open_frame(
    ch: &mut ChannelKeys,
    hdr_raw: [u8; 4],
    payload: &[u8],
    verify_ed: Option<&[u8; 32]>,
) -> Option<(Header, Vec<u8>)> {
    let key = ch.next_recv_hdr();
    let h = Header::decode_obfuscated(hdr_raw, key);
    if payload.len() != h.cmd_len as usize {
        return None;
    }
    let mut plain = if h.is_encrypted_channel {
        ch.open_channel(payload).ok()?
    } else {
        payload.to_vec()
    };
    if h.is_signed {
        if plain.len() < 64 {
            return None;
        }
        let (msg, sig) = plain.split_at(plain.len() - 64);
        if let Some(pk) = verify_ed {
            if nacl::verify_detached(&sig.try_into().ok()?, msg, pk).is_err() {
                return None;
            }
        }
        plain = msg.to_vec();
    }
    Some((h, plain))
}
