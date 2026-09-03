//! DATUM wire protocol, pool side.
//!
//! This crate speaks the protocol that a DATUM Gateway (the MIT-licensed C client from
//! Bitcoin Ocean / CONVOY, and its BLAKE2b forks) uses to talk to a "Prime". The gateway
//! source is the only specification the protocol has; the byte layouts here were read
//! from that source (`datum_protocol.c`, `datum_coinbaser.c`, `datum_pow.c`) and
//! reimplemented. No pool-side code from any other project is used.
//!
//! Layers, bottom up:
//!
//! * [`frame`] — the 4-byte obfuscated header and the XOR key feedback that walks it.
//! * [`crypto`] — NaCl primitives the client uses: sealed boxes, precomputed boxes,
//!   ed25519 signatures, and the deterministic session nonces both sides derive from the
//!   client's hello.
//! * [`handshake`] — parse the client hello (cmd 1), build the server reply (cmd 2).
//! * [`mining`] — the cmd 5 sub-commands in both directions: coinbaser requests and
//!   replies, share submissions and receipts, job-validation requests and replies,
//!   client configuration, block notify.
//! * [`coinbaser`] — the "coinbaser v2" split encoding a gateway turns into outputs.
//! * [`pow`] — BLAKE2b header-v2 proof of work as Bitcoin Knots defines it, so Prime can
//!   rebuild the header a miner hashed from the share and check the work is real.
//! * [`coinbase`] — a small parser for the legacy-serialized coinbase transaction, so
//!   Prime can see who a share's coinbase actually pays.

pub mod coinbase;
pub mod coinbaser;
pub mod crypto;
pub mod frame;
pub mod handshake;
pub mod mining;
pub mod pow;
pub mod verify;

/// Largest command payload the protocol allows (22-bit length field).
pub const MAX_CMD_LEN: usize = 1 << 22;

/// Protocol-level command numbers (5-bit field in the frame header).
pub mod cmd {
    /// Client → server: handshake init (sealed to the pool key, signed by the client).
    /// Server → client: ping.
    pub const HELLO: u8 = 1;
    /// Server → client: handshake reply (sealed to the client session key, signed by the pool).
    pub const HELLO_REPLY: u8 = 2;
    /// Both directions: mining sub-commands, encrypted on the session channel.
    pub const MINING: u8 = 5;
    /// Server → client: free-text message shown in the gateway log.
    pub const INFO: u8 = 7;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("message too short")]
    Short,
    #[error("bad signature")]
    Signature,
    #[error("decryption failed")]
    Decrypt,
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("command payload exceeds protocol limit")]
    TooLong,
}

pub type Result<T> = core::result::Result<T, Error>;

/// Little-endian read helpers over a cursor; every DATUM integer is little-endian.
pub struct Cursor<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(Error::Short);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    /// NUL-terminated string; the terminator is consumed.
    pub fn cstr(&mut self, max: usize) -> Result<&'a [u8]> {
        let rest = &self.buf[self.pos..];
        let end = rest.iter().take(max + 1).position(|&b| b == 0).ok_or(Error::Malformed("unterminated string"))?;
        let s = &rest[..end];
        self.pos += end + 1;
        Ok(s)
    }
}
