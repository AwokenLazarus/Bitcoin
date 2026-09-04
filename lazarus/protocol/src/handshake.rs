use crate::nacl;
use thiserror::Error;

use crate::channel::ChannelKeys;
use crate::header::{Header, ProtoCmd};
use crate::keys::{generate_session, PoolKeys, SessionKeys};
use crate::xor::header_xor_feedback;

pub const HELLO_XOR: u32 = 0xDC871829;
pub const SEAL_BYTES: usize = 48;
pub const SIGN_BYTES: usize = 64;

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error("crypto")]
    Crypto,
    #[error("bad hello")]
    BadHello,
    #[error("bad signature")]
    BadSig,
    #[error("truncated")]
    Truncated,
}

#[derive(Clone)]
pub struct ClientHello {
    pub lt_ed: [u8; 32],
    pub lt_x: [u8; 32],
    pub sess_ed: [u8; 32],
    pub sess_x: [u8; 32],
    pub version: String,
    pub nk: u32,
}

/// Prime: open a client hello (proto_cmd=1, sealed to pool x25519, signed with client LT ed).
pub fn open_hello(pool: &PoolKeys, cipher: &[u8]) -> Result<ClientHello, HandshakeError> {
    if cipher.len() < SEAL_BYTES + 128 + 2 + SIGN_BYTES {
        return Err(HandshakeError::Truncated);
    }
    let plain = nacl::box_seal_open(cipher, &pool.x_pk, &pool.x_sk)
        .map_err(|_| HandshakeError::Crypto)?;
    if plain.len() < 128 + 2 + SIGN_BYTES {
        return Err(HandshakeError::Truncated);
    }
    let (msg, sig) = plain.split_at(plain.len() - SIGN_BYTES);
    let mut lt_ed = [0u8; 32];
    lt_ed.copy_from_slice(&msg[0..32]);
    nacl::verify_detached(
        sig.try_into().map_err(|_| HandshakeError::Truncated)?,
        msg,
        &lt_ed,
    )
    .map_err(|_| HandshakeError::BadSig)?;
    let mut lt_x = [0u8; 32];
    let mut sess_ed = [0u8; 32];
    let mut sess_x = [0u8; 32];
    lt_x.copy_from_slice(&msg[32..64]);
    sess_ed.copy_from_slice(&msg[64..96]);
    sess_x.copy_from_slice(&msg[96..128]);
    let rest = &msg[128..];
    let z = rest.iter().position(|&b| b == 0).ok_or(HandshakeError::BadHello)?;
    let version = String::from_utf8_lossy(&rest[..z]).into_owned();
    let after = &rest[z + 1..];
    if after.is_empty() || after[0] != 0xFE || after.len() < 5 {
        return Err(HandshakeError::BadHello);
    }
    let nk = u32::from_le_bytes(after[1..5].try_into().unwrap());
    Ok(ClientHello {
        lt_ed,
        lt_x,
        sess_ed,
        sess_x,
        version,
        nk,
    })
}

/// Prime handshake response body (unsigned): echo four client keys + pool session keys + MOTD.
pub fn handshake_response_body(
    hello: &ClientHello,
    pool_sess: &SessionKeys,
    motd: &str,
) -> Vec<u8> {
    let mut b = Vec::with_capacity(192 + motd.len() + 1);
    b.extend_from_slice(&hello.lt_ed);
    b.extend_from_slice(&hello.lt_x);
    b.extend_from_slice(&hello.sess_ed);
    b.extend_from_slice(&hello.sess_x);
    b.extend_from_slice(&pool_sess.ed_pk);
    b.extend_from_slice(&pool_sess.x_pk);
    b.extend_from_slice(motd.as_bytes());
    b.push(0);
    b
}

/// Sign with pool long-term ed25519, seal to client session x25519, wrap header proto_cmd=2.
pub fn encode_handshake_response(
    pool: &PoolKeys,
    hello: &ClientHello,
    pool_sess: &SessionKeys,
    motd: &str,
    send_hdr: u32,
) -> Result<Vec<u8>, HandshakeError> {
    let body = handshake_response_body(hello, pool_sess, motd);
    let mut signed = body.clone();
    let sig = nacl::sign_detached(&body, &pool.ed_sk).map_err(|_| HandshakeError::Crypto)?;
    signed.extend_from_slice(&sig);
    let sealed = nacl::box_seal(&signed, &hello.sess_x).map_err(|_| HandshakeError::Crypto)?;
    let h = Header {
        cmd_len: sealed.len() as u32,
        is_signed: true,
        is_encrypted_pubkey: true,
        is_encrypted_channel: false,
        proto_cmd: ProtoCmd::HandshakeResp as u8,
    };
    let mut out = h.encode_obfuscated(send_hdr).to_vec();
    out.extend_from_slice(&sealed);
    Ok(out)
}

pub fn prime_channel_after_hello(
    hello: &ClientHello,
    pool_sess: &SessionKeys,
) -> ChannelKeys {
    ChannelKeys::for_prime_after_hello(hello.nk, &hello.sess_ed, &hello.sess_x, &pool_sess.x_sk)
}

/// Client (gateway) hello: sign with LT ed, seal to pool x25519.
pub fn encode_client_hello(
    local: &PoolKeys,
    session: &SessionKeys,
    pool_x_pk: &[u8; 32],
    version: &str,
) -> Result<(Vec<u8>, u32, ChannelKeys), HandshakeError> {
    let mut body = Vec::new();
    body.extend_from_slice(&local.ed_pk);
    body.extend_from_slice(&local.x_pk);
    body.extend_from_slice(&session.ed_pk);
    body.extend_from_slice(&session.x_pk);
    body.extend_from_slice(version.as_bytes());
    body.push(0);
    body.push(0xFE);
    let nk: u32 = rand::random();
    body.extend_from_slice(&nk.to_le_bytes());
    let pad = 1 + (rand::random::<u8>() as usize % 40);
    body.extend(std::iter::repeat(rand::random::<u8>()).take(pad));
    let sig = nacl::sign_detached(&body, &local.ed_sk).map_err(|_| HandshakeError::Crypto)?;
    body.extend_from_slice(&sig);
    let sealed = nacl::box_seal(&body, pool_x_pk).map_err(|_| HandshakeError::Crypto)?;
    let h = Header {
        cmd_len: sealed.len() as u32,
        is_signed: true,
        is_encrypted_pubkey: true,
        is_encrypted_channel: false,
        proto_cmd: ProtoCmd::HelloOrPing as u8,
    };
    let mut out = h.encode_obfuscated(HELLO_XOR).to_vec();
    out.extend_from_slice(&sealed);
    // Client keys after hello (OCEAN C)
    let send_hdr = header_xor_feedback(nk);
    let recv_hdr = header_xor_feedback(!nk);
    let ch = ChannelKeys::for_client_after_hello(nk, &session.ed_pk);
    let _ = (send_hdr, recv_hdr);
    Ok((out, nk, ch))
}

pub fn new_session() -> Result<SessionKeys, HandshakeError> {
    Ok(generate_session())
}

/// Hello UA `lazarus-gateway` sends. Prime allowlists this prefix when
/// `require-split-gateway` is on.
pub const SPLIT_GATEWAY_UA: &str = "lazarus-gateway/0.1";

/// Gateways that never publish empty/tiny coinbase jobs while pooled.
///
/// Stock OCEAN DATUM (`v0.4.1-beta/...`) blasts `JOB_STATE_EMPTY_PLUS` / type-0
/// "tiny" work (pool script only) the instant a new template arrives, then
/// waits for the coinbaser. A block found on that work pays the TIDES window
/// nothing. `lazarus-gateway` refuses to publish those jobs. A patched
/// `datum_gateway` advertises `lazarus-split` in its hello UA.
pub fn is_split_gateway(ua: &str) -> bool {
    let u = ua.to_ascii_lowercase();
    u.starts_with("lazarus-gateway") || u.contains("lazarus-split")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlists_lazarus_gateway_and_patched_ocean() {
        assert!(is_split_gateway(SPLIT_GATEWAY_UA));
        assert!(is_split_gateway("lazarus-gateway/0.2"));
        assert!(is_split_gateway("v0.4.1-beta+lazarus-split/abc(tag)"));
        assert!(is_split_gateway("DATUM/lazarus-split"));
    }

    #[test]
    fn refuses_stock_ocean_empty_first() {
        assert!(!is_split_gateway("v0.4.1-beta/c4e7a8c(v0.4.1beta)"));
        assert!(!is_split_gateway("v0.4.1-beta"));
        assert!(!is_split_gateway(""));
        assert!(!is_split_gateway("ratum/0.1.3"));
    }
}
