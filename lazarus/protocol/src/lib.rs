//! Lazarus DATUM protocol — an original Rust implementation of the DATUM wire format.
//!
//! The protocol is OCEAN's. The wire format here is recovered from `datum_protocol.c` /
//! `.h` (MIT, Bitcoin Ocean, LLC).
//!
//! The first Rust reading of that protocol for the BLAKE2b Knots chain — and the Prime this
//! box originally ran — is Ratum by iohzrd, <https://github.com/iohzrd/ratum> (0.1.3,
//! `e828545`). This tree grew out of a vendored copy of it, and these modules still follow
//! the shape it established: `handshake`, `channel`, `nacl` and `header` against its
//! `core/src/datum/` framing and handshake; `mining` against its message subtypes;
//! `coinbaser` against its coinbaser v2 encoding; `pow` against its version 2 header,
//! target and nonce handling. Written from the C protocol and this chain's consensus rules
//! rather than copied, but the debt is real and worth naming.

pub mod address;
pub mod cbtx;
pub mod channel;
pub mod coinbaser;
pub mod handshake;
pub mod header;
pub mod keys;
pub mod mining;
pub mod nacl;
pub mod pow;
pub mod verify;
pub mod xor;

pub use address::{identity_of, identity_script};
pub use channel::ChannelKeys;
pub use coinbaser::{parse_coinbase, split_satisfied, CoinbaserOutput, CoinbaserV2, ParsedCoinbase};
pub use handshake::{open_hello, ClientHello, HELLO_XOR};
pub use header::Header;
pub use keys::{load_or_create_pool_keys, PoolKeys, SessionKeys};
pub use mining::{PowSubmit, SUB_COINBASER_REQ, SUB_SHARE};
pub use verify::{verify_share, ShareContext, VerifiedShare};
pub use xor::{header_xor_feedback, xor_u32};
