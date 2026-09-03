//! Frame header and header-key schedule.
//!
//! Every message on the wire is a 4-byte header followed by `len` payload bytes. The
//! header is one little-endian `u32` packed as, from the least significant bit:
//!
//! | bits   | field                                                        |
//! |--------|--------------------------------------------------------------|
//! | 0–21   | payload length                                               |
//! | 22–23  | reserved                                                     |
//! | 24     | signed: an ed25519 signature trails the (decrypted) payload  |
//! | 25     | sealed: payload is a NaCl sealed box to the receiver's key   |
//! | 26     | channel: payload is a NaCl box on the negotiated session     |
//! | 27–31  | protocol command                                             |
//!
//! The `u32` is XORed with a per-direction rolling key. The client's very first header
//! uses a fixed key; both sides then reseed from a 4-byte value inside the hello and
//! advance the key through [`feedback`] after every header.

use crate::{Error, Result, MAX_CMD_LEN};

/// Key the client XORs onto its hello header before any negotiation.
pub const CLIENT_INITIAL_KEY: u32 = 0xDC87_1829;

/// One-way mixer that produces the next header key from the current one.
///
/// This is the client's `datum_header_xor_feedback`: a single-round murmur3-style
/// mix over the input with a fixed seed. Both sides must walk the identical sequence,
/// so the constants are part of the protocol, not a design choice here.
#[inline]
pub fn feedback(i: u32) -> u32 {
    let mut k = i.wrapping_mul(0xcc9e_2d51);
    k = k.rotate_left(15);
    k = k.wrapping_mul(0x1b87_3593);
    let mut h = 0xb10c_feed_u32 ^ k;
    h = h.rotate_left(13);
    h = h.wrapping_mul(5).wrapping_add(0xe654_6b64);
    h ^= 4;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// Rolling header key for one direction.
#[derive(Clone, Copy, Debug)]
pub struct KeyStream(pub u32);

impl KeyStream {
    /// Consume the current key and advance.
    #[inline]
    pub fn advance(&mut self) -> u32 {
        let k = self.0;
        self.0 = feedback(k);
        k
    }

    /// The two directional streams after the hello carrying `seed`.
    ///
    /// Returns `(server_recv, server_send)`: the client sends under `feedback(seed)` and
    /// receives under `feedback(!seed)`.
    pub fn from_seed(seed: u32) -> (KeyStream, KeyStream) {
        (KeyStream(feedback(seed)), KeyStream(feedback(!seed)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Header {
    pub len: u32,
    pub signed: bool,
    pub sealed: bool,
    pub channel: bool,
    pub cmd: u8,
}

impl Header {
    pub const SIZE: usize = 4;

    pub fn new(cmd: u8, len: usize) -> Self {
        Header { len: len as u32, signed: false, sealed: false, channel: false, cmd }
    }

    #[inline]
    pub fn to_u32(&self) -> u32 {
        (self.len & 0x003f_ffff)
            | (u32::from(self.signed) << 24)
            | (u32::from(self.sealed) << 25)
            | (u32::from(self.channel) << 26)
            | (u32::from(self.cmd & 0x1f) << 27)
    }

    #[inline]
    pub fn from_u32(v: u32) -> Self {
        Header {
            len: v & 0x003f_ffff,
            signed: v & (1 << 24) != 0,
            sealed: v & (1 << 25) != 0,
            channel: v & (1 << 26) != 0,
            cmd: (v >> 27) as u8,
        }
    }

    /// Serialize under the given key, advancing the stream.
    #[inline]
    pub fn encode(&self, keys: &mut KeyStream) -> [u8; 4] {
        (self.to_u32() ^ keys.advance()).to_le_bytes()
    }

    /// Parse a header under the given key, advancing the stream.
    #[inline]
    pub fn decode(bytes: [u8; 4], keys: &mut KeyStream) -> Result<Self> {
        let h = Header::from_u32(u32::from_le_bytes(bytes) ^ keys.advance());
        if h.len as usize > MAX_CMD_LEN {
            return Err(Error::TooLong);
        }
        Ok(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_bits_round_trip() {
        let h = Header { len: 0x3f_ffff, signed: true, sealed: false, channel: true, cmd: 31 };
        assert_eq!(Header::from_u32(h.to_u32()), h);
        let h = Header { len: 1, signed: false, sealed: true, channel: false, cmd: 1 };
        assert_eq!(Header::from_u32(h.to_u32()), h);
        // field positions match the packed C bitfield: len low, cmd in the top five bits
        assert_eq!(Header::new(5, 0x10).to_u32(), 0x2800_0010);
        assert_eq!(Header { len: 0, signed: true, sealed: true, channel: false, cmd: 1 }.to_u32(), 0x0b00_0000);
    }

    #[test]
    fn encode_and_decode_walk_the_same_key_stream() {
        let (mut a, mut b) = KeyStream::from_seed(0x1234_5678);
        let mut a2 = a;
        for i in 0..8u32 {
            let h = Header::new((i % 32) as u8, (i * 977) as usize);
            let bytes = h.encode(&mut a);
            assert_eq!(Header::decode(bytes, &mut a2).unwrap(), h);
        }
        // the other direction is an independent stream
        assert_ne!(a.0, b.advance());
    }

    #[test]
    fn feedback_is_deterministic_and_mixes() {
        let x = feedback(0);
        assert_eq!(x, feedback(0));
        assert_ne!(x, feedback(1));
        assert_ne!(feedback(CLIENT_INITIAL_KEY), CLIENT_INITIAL_KEY);
    }

    #[test]
    fn length_field_is_22_bits() {
        let mut k = KeyStream(0);
        let raw = (0x3f_ffffu32 | (5 << 27)).to_le_bytes();
        let h = Header::decode(raw, &mut k).unwrap();
        assert_eq!(h.len as usize, MAX_CMD_LEN - 1);
        assert_eq!(h.cmd, 5);
    }
}
