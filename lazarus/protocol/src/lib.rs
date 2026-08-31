//! Lazarus DATUM protocol — original implementation from OCEAN `datum_gateway` C.
//!
//! Wire format recovered from `datum_protocol.c` / `.h` (MIT, Bitcoin Ocean, LLC).
//! This crate is not a rebrand of third-party Rust pool software.

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
