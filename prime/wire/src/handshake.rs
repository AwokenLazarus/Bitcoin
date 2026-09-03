//! The two-message handshake.
//!
//! **Client hello** (cmd 1, `sealed | signed`), sealed to the pool's long-term box key.
//! Plaintext, in order:
//!
//! ```text
//! identity sign pk (32) | identity box pk (32) | session sign pk (32) | session box pk (32)
//! user agent, NUL-terminated | 0xFE | header key seed (u32 LE) | random pad ... | signature (64)
//! ```
//!
//! The signature is by the *identity* signing key over everything before it.
//!
//! Two client generations exist and the server must tell them apart here, because they
//! expect different configuration layouts (see [`crate::mining::configure_v1`] and
//! [`crate::mining::configure_v3`]):
//!
//! * **OCEAN lineage** (`datum_gateway` v0.4.x and the BLAKE2b forks) — the pad after
//!   the seed is random.
//! * **Convoy lineage** — the pad begins with `"DRS\x01"`, a resume flag byte, and when
//!   the flag is set a 40-byte resume token from a previous session.
//!
//! **Server reply** (cmd 2, `sealed | signed`), sealed to the client's *session* box key,
//! signed by the pool's long-term signing key:
//!
//! ```text
//! the client's four public keys echoed back (128) | server session sign pk (32)
//! server session box pk (32) | MOTD, NUL-terminated | signature (64)
//! ```
//!
//! After that both sides box on `(their session box sk, other side's session box pk)` and
//! the server signs configuration with its *session* signing key.

use crate::crypto::{self, Identity, BOX_PK, SIGN_PK};
use crate::{Cursor, Error, Result};

pub const MAX_USER_AGENT: usize = 512;
pub const MAX_MOTD: usize = 511;
pub const RESUME_TOKEN: usize = 40;
const RESUME_MARKER: &[u8; 4] = b"DRS\x01";

/// Which configuration layout the client parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    /// Configure v1: 32-bit prime id, no resume token.
    Ocean,
    /// Configure v3: 64-bit prime id, resume token, feature flags.
    Convoy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientHello {
    /// Long-term keys: how the gateway identifies itself across sessions.
    pub identity_sign_pk: [u8; SIGN_PK],
    pub identity_box_pk: [u8; BOX_PK],
    /// Per-connection keys.
    pub session_sign_pk: [u8; SIGN_PK],
    pub session_box_pk: [u8; BOX_PK],
    pub user_agent: String,
    /// Seeds the header key streams and the session nonces.
    pub seed: u32,
    pub generation: Generation,
    /// A Convoy client asking to continue an earlier session.
    pub resume_token: Option<[u8; RESUME_TOKEN]>,
}

/// Open and validate a client hello payload (everything after the 4-byte header).
pub fn parse_client_hello(pool: &Identity, sealed: &[u8]) -> Result<ClientHello> {
    let plain = pool.unseal(sealed)?;
    // Signature first: the key that signed is inside the message, so read it, then verify.
    if plain.len() < 4 * 32 + crypto::SIG {
        return Err(Error::Short);
    }
    let identity_sign_pk: [u8; 32] = plain[..32].try_into().unwrap();
    let body = crypto::verify_trailing(&identity_sign_pk, &plain)?;

    let mut c = Cursor::new(body);
    c.take(32)?;
    let identity_box_pk = c.array::<32>()?;
    let session_sign_pk = c.array::<32>()?;
    let session_box_pk = c.array::<32>()?;
    let ua = c.cstr(MAX_USER_AGENT)?;
    if c.u8()? != 0xFE {
        return Err(Error::Malformed("hello separator"));
    }
    let seed = c.u32()?;
    let mut generation = Generation::Ocean;
    let mut resume_token = None;
    if c.remaining() >= 5 && &body[c.pos..c.pos + 4] == RESUME_MARKER {
        generation = Generation::Convoy;
        c.take(4)?;
        if c.u8()? == 1 {
            resume_token = Some(c.array::<RESUME_TOKEN>()?);
        }
    }
    Ok(ClientHello {
        identity_sign_pk,
        identity_box_pk,
        session_sign_pk,
        session_box_pk,
        user_agent: String::from_utf8_lossy(ua).into_owned(),
        seed,
        generation,
        resume_token,
    })
}

/// Bytes a Convoy-lineage client puts after the seed. Test/tool helper.
pub fn convoy_hello_extension(resume_token: Option<&[u8; RESUME_TOKEN]>) -> Vec<u8> {
    let mut v = RESUME_MARKER.to_vec();
    match resume_token {
        Some(t) => {
            v.push(1);
            v.extend_from_slice(t);
        }
        None => v.push(0),
    }
    v
}

/// Build the sealed, signed reply payload for a hello we accepted.
pub fn build_server_hello(pool: &Identity, session: &Identity, hello: &ClientHello, motd: &str) -> Vec<u8> {
    let mut m = Vec::with_capacity(6 * 32 + motd.len() + 1 + crypto::SIG);
    m.extend_from_slice(&hello.identity_sign_pk);
    m.extend_from_slice(&hello.identity_box_pk);
    m.extend_from_slice(&hello.session_sign_pk);
    m.extend_from_slice(&hello.session_box_pk);
    m.extend_from_slice(&session.sign_pk());
    m.extend_from_slice(&session.box_pk());
    let motd = motd.as_bytes();
    let n = motd.len().min(MAX_MOTD);
    m.extend_from_slice(&motd[..n]);
    m.push(0);
    let sig = pool.sign(&m);
    m.extend_from_slice(&sig);
    crypto::seal_to(&hello.session_box_pk, &m)
}

/// Client-side hello builder. Only used by tests and tools that impersonate a gateway;
/// a real gateway is the C client.
pub fn build_client_hello(
    pool_box_pk: &[u8; BOX_PK],
    identity: &Identity,
    session: &Identity,
    user_agent: &str,
    seed: u32,
    pad: &[u8],
) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(&identity.sign_pk());
    m.extend_from_slice(&identity.box_pk());
    m.extend_from_slice(&session.sign_pk());
    m.extend_from_slice(&session.box_pk());
    m.extend_from_slice(user_agent.as_bytes());
    m.push(0);
    m.push(0xFE);
    m.extend_from_slice(&seed.to_le_bytes());
    m.extend_from_slice(pad);
    let sig = identity.sign(&m);
    m.extend_from_slice(&sig);
    crypto::seal_to(pool_box_pk, &m)
}

/// Client-side parse of the server reply. Test/tool helper.
pub fn parse_server_hello(
    pool_sign_pk: &[u8; SIGN_PK],
    session: &Identity,
    sealed: &[u8],
) -> Result<([u8; SIGN_PK], [u8; BOX_PK], String)> {
    let plain = session.unseal(sealed)?;
    let body = crypto::verify_trailing(pool_sign_pk, &plain)?;
    let mut c = Cursor::new(body);
    c.take(128)?;
    let ssk = c.array::<32>()?;
    let sbk = c.array::<32>()?;
    let motd = c.cstr(MAX_MOTD)?;
    Ok((ssk, sbk, String::from_utf8_lossy(motd).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip() {
        let pool = Identity::generate();
        let gw_id = Identity::generate();
        let gw_sess = Identity::generate();
        let sealed =
            build_client_hello(&pool.box_pk(), &gw_id, &gw_sess, "v0.4.1-beta/abc(tag)", 0x0102_0304, &[9u8; 37]);
        let h = parse_client_hello(&pool, &sealed).unwrap();
        assert_eq!(h.identity_sign_pk, gw_id.sign_pk());
        assert_eq!(h.identity_box_pk, gw_id.box_pk());
        assert_eq!(h.session_sign_pk, gw_sess.sign_pk());
        assert_eq!(h.session_box_pk, gw_sess.box_pk());
        assert_eq!(h.user_agent, "v0.4.1-beta/abc(tag)");
        assert_eq!(h.seed, 0x0102_0304);
        assert_eq!(h.generation, Generation::Ocean);
        assert_eq!(h.resume_token, None);

        // Convoy lineage: marker, then no token; then a token
        let mut ext = convoy_hello_extension(None);
        ext.extend_from_slice(&[7u8; 50]);
        let sealed = build_client_hello(&pool.box_pk(), &gw_id, &gw_sess, "ua", 5, &ext);
        let h2 = parse_client_hello(&pool, &sealed).unwrap();
        assert_eq!(h2.generation, Generation::Convoy);
        assert_eq!(h2.resume_token, None);
        let tok = [0xab; RESUME_TOKEN];
        let sealed = build_client_hello(&pool.box_pk(), &gw_id, &gw_sess, "ua", 5, &convoy_hello_extension(Some(&tok)));
        let h3 = parse_client_hello(&pool, &sealed).unwrap();
        assert_eq!(h3.generation, Generation::Convoy);
        assert_eq!(h3.resume_token, Some(tok));

        let srv_sess = Identity::generate();
        let reply = build_server_hello(&pool, &srv_sess, &h, "Lazarus");
        let (ssk, sbk, motd) = parse_server_hello(&pool.sign_pk(), &gw_sess, &reply).unwrap();
        assert_eq!(ssk, srv_sess.sign_pk());
        assert_eq!(sbk, srv_sess.box_pk());
        assert_eq!(motd, "Lazarus");
    }

    #[test]
    fn hello_to_the_wrong_pool_or_with_a_bad_signature_fails() {
        let pool = Identity::generate();
        let other = Identity::generate();
        let gw = Identity::generate();
        let sealed = build_client_hello(&other.box_pk(), &gw, &gw, "ua", 1, &[]);
        assert_eq!(parse_client_hello(&pool, &sealed), Err(Error::Decrypt));

        // forge: sign with a key that is not the identity key in the message
        let imposter = Identity::generate();
        let mut m = Vec::new();
        m.extend_from_slice(&gw.sign_pk());
        m.extend_from_slice(&gw.box_pk());
        m.extend_from_slice(&gw.sign_pk());
        m.extend_from_slice(&gw.box_pk());
        m.extend_from_slice(b"ua\0\xFE\x01\0\0\0");
        let sig = imposter.sign(&m);
        m.extend_from_slice(&sig);
        let sealed = crypto::seal_to(&pool.box_pk(), &m);
        assert_eq!(parse_client_hello(&pool, &sealed), Err(Error::Signature));
    }
}
