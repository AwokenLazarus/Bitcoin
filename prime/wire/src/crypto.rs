//! NaCl primitives as the DATUM client uses them, wrapped so the rest of the crate never
//! touches key bytes directly.
//!
//! The client holds two keypairs per role: an ed25519 pair for signatures and an X25519
//! pair for boxes. The pool publishes the same two public keys as one 128-hex-char string
//! (`ed25519 pk || x25519 pk`), which is what a gateway pastes into its config.
//!
//! Session traffic uses `crypto_box_easy` with a precomputed shared key and a 24-byte
//! nonce that both sides derive deterministically from the hello and then increment per
//! message. The nonce is treated as six little-endian `u32` words: increment word 0, and
//! carry into the next word only when it wraps to zero.

use crypto_box::aead::{Aead, AeadInPlace};
use crypto_box::{PublicKey as BoxPublic, SalsaBox, SecretKey as BoxSecret};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::{CryptoRngCore, OsRng};

use crate::frame::feedback;
use crate::{Error, Result};

pub const SIGN_PK: usize = 32;
pub const BOX_PK: usize = 32;
pub const SIG: usize = 64;
pub const NONCE: usize = 24;
pub const MAC: usize = 16;
/// Overhead of a sealed box: ephemeral public key plus MAC.
pub const SEAL_OVERHEAD: usize = 32 + MAC;

/// A signing pair plus a box pair — what DATUM calls a key set.
pub struct Identity {
    sign: SigningKey,
    bx: BoxSecret,
}

impl Identity {
    pub fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    pub fn generate_with(rng: &mut impl CryptoRngCore) -> Self {
        Identity { sign: SigningKey::generate(rng), bx: BoxSecret::generate(rng) }
    }

    /// Rebuild from the 64 secret bytes returned by [`Identity::secret_bytes`].
    pub fn from_secret_bytes(b: &[u8; 64]) -> Self {
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&b[..32]);
        let mut bs = [0u8; 32];
        bs.copy_from_slice(&b[32..]);
        Identity { sign: SigningKey::from_bytes(&seed), bx: BoxSecret::from_bytes(bs) }
    }

    /// `ed25519 seed || x25519 secret`. Keep it on disk with mode 600.
    pub fn secret_bytes(&self) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&self.sign.to_bytes());
        out[32..].copy_from_slice(&self.bx.to_bytes());
        out
    }

    pub fn sign_pk(&self) -> [u8; SIGN_PK] {
        self.sign.verifying_key().to_bytes()
    }

    pub fn box_pk(&self) -> [u8; BOX_PK] {
        self.bx.public_key().to_bytes()
    }

    /// The 128-hex-char form a gateway config expects: signing key then box key.
    pub fn public_hex(&self) -> String {
        let mut s = hex::encode(self.sign_pk());
        s.push_str(&hex::encode(self.box_pk()));
        s
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; SIG] {
        self.sign.sign(msg).to_bytes()
    }

    /// Open a sealed box addressed to this identity's box key.
    pub fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        self.bx.unseal(sealed).map_err(|_| Error::Decrypt)
    }

    /// Precompute the session box with a remote box public key.
    pub fn precompute(&self, remote_box_pk: &[u8; BOX_PK]) -> SalsaBox {
        SalsaBox::new(&BoxPublic::from(*remote_box_pk), &self.bx)
    }
}

/// Seal `msg` to a remote box public key (`crypto_box_seal`).
pub fn seal_to(remote_box_pk: &[u8; BOX_PK], msg: &[u8]) -> Vec<u8> {
    BoxPublic::from(*remote_box_pk).seal(&mut OsRng, msg).expect("sealing cannot fail for in-memory buffers")
}

/// Verify a detached ed25519 signature.
pub fn verify(pk: &[u8; SIGN_PK], msg: &[u8], sig: &[u8]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(pk).map_err(|_| Error::Signature)?;
    let sig = Signature::from_slice(sig).map_err(|_| Error::Signature)?;
    vk.verify(msg, &sig).map_err(|_| Error::Signature)
}

/// Split `payload` into `(message, signature)` and verify it.
pub fn verify_trailing<'a>(pk: &[u8; SIGN_PK], payload: &'a [u8]) -> Result<&'a [u8]> {
    if payload.len() < SIG {
        return Err(Error::Short);
    }
    let (msg, sig) = payload.split_at(payload.len() - SIG);
    verify(pk, msg, sig)?;
    Ok(msg)
}

/// Both directional nonces, derived the way the client does from the hello's header-key
/// seed and the client's *session* signing public key.
///
/// Returns `(server_send, server_recv)`. The client calls these its receiver and sender
/// nonces respectively.
pub fn session_nonces(seed: u32, client_session_sign_pk: &[u8; SIGN_PK]) -> ([u8; NONCE], [u8; NONCE]) {
    let mut to_client = [0u8; NONCE];
    let mut from_client = [0u8; NONCE];
    let pk7 = u32::from_le_bytes(client_session_sign_pk[7..11].try_into().unwrap());
    let mut nk = seed.wrapping_sub(42) ^ pk7;
    for j in (0..NONCE).step_by(4) {
        let word = feedback(nk.wrapping_sub(42));
        to_client[j..j + 4].copy_from_slice(&word.to_le_bytes());
        from_client[j..j + 4].copy_from_slice(&(word ^ 0x5757_5757).to_le_bytes());
        nk = !word;
    }
    (to_client, from_client)
}

/// Advance a session nonce by one message.
#[inline]
pub fn bump_nonce(n: &mut [u8; NONCE]) {
    for j in (0..NONCE).step_by(4) {
        let w = u32::from_le_bytes(n[j..j + 4].try_into().unwrap()).wrapping_add(1);
        n[j..j + 4].copy_from_slice(&w.to_le_bytes());
        if w != 0 {
            return;
        }
    }
}

/// An established encrypted channel with a gateway.
pub struct Channel {
    bx: SalsaBox,
    send_nonce: [u8; NONCE],
    recv_nonce: [u8; NONCE],
}

impl Channel {
    pub fn new(bx: SalsaBox, send_nonce: [u8; NONCE], recv_nonce: [u8; NONCE]) -> Self {
        Channel { bx, send_nonce, recv_nonce }
    }

    /// Encrypt for the gateway: output is `MAC || ciphertext` as `crypto_box_easy` lays it out.
    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        let out = self
            .bx
            .encrypt((&self.send_nonce).into(), plain)
            .expect("box encryption of an in-memory buffer cannot fail");
        bump_nonce(&mut self.send_nonce);
        out
    }

    /// Decrypt a `MAC || ciphertext` payload from the gateway in place.
    ///
    /// On success the plaintext occupies `buf[..returned_len]`. On failure the nonce is
    /// *not* advanced; the session is broken at that point anyway and the caller should
    /// drop it.
    pub fn decrypt_in_place<'a>(&mut self, buf: &'a mut [u8]) -> Result<&'a mut [u8]> {
        if buf.len() < MAC {
            return Err(Error::Short);
        }
        let (tag, body) = buf.split_at_mut(MAC);
        let tag: [u8; MAC] = tag.try_into().unwrap();
        self.bx
            .decrypt_in_place_detached((&self.recv_nonce).into(), &[], body, (&tag).into())
            .map_err(|_| Error::Decrypt)?;
        bump_nonce(&mut self.recv_nonce);
        let n = body.len();
        Ok(&mut buf[MAC..MAC + n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_increment_carries_like_the_client() {
        let mut n = [0u8; NONCE];
        n[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        bump_nonce(&mut n);
        assert_eq!(&n[0..8], &[0, 0, 0, 0, 1, 0, 0, 0]);
        bump_nonce(&mut n);
        assert_eq!(&n[0..8], &[1, 0, 0, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn nonces_differ_per_direction_and_depend_on_inputs() {
        let pk = [7u8; 32];
        let (a, b) = session_nonces(1, &pk);
        assert_ne!(a, b);
        for j in (0..NONCE).step_by(4) {
            let x = u32::from_le_bytes(a[j..j + 4].try_into().unwrap());
            let y = u32::from_le_bytes(b[j..j + 4].try_into().unwrap());
            assert_eq!(x ^ y, 0x5757_5757);
        }
        assert_ne!(session_nonces(2, &pk).0, a);
        assert_ne!(session_nonces(1, &[8u8; 32]).0, a);
    }

    #[test]
    fn channel_round_trip_matches_crypto_box_easy_layout() {
        let server = Identity::generate();
        let client = Identity::generate();
        let (s2c, c2s) = session_nonces(0xdead_beef, &client.sign_pk());
        let mut s = Channel::new(server.precompute(&client.box_pk()), s2c, c2s);
        let mut c = Channel::new(client.precompute(&server.box_pk()), c2s, s2c);
        for i in 0..5 {
            let msg = vec![i as u8; 10 + i * 7];
            let mut ct = s.encrypt(&msg);
            assert_eq!(ct.len(), msg.len() + MAC);
            let pt = c.decrypt_in_place(&mut ct).unwrap();
            assert_eq!(pt, &msg[..]);
            let mut back = c.encrypt(&msg);
            assert_eq!(s.decrypt_in_place(&mut back).unwrap(), &msg[..]);
        }
        // a replay (stale nonce) fails
        let mut ct = s.encrypt(b"once");
        let mut copy = ct.clone();
        c.decrypt_in_place(&mut ct).unwrap();
        assert_eq!(c.decrypt_in_place(&mut copy), Err(Error::Decrypt));
    }

    #[test]
    fn seal_and_unseal() {
        let id = Identity::generate();
        let sealed = seal_to(&id.box_pk(), b"hello prime");
        assert_eq!(sealed.len(), 11 + SEAL_OVERHEAD);
        assert_eq!(id.unseal(&sealed).unwrap(), b"hello prime");
        assert_eq!(Identity::generate().unseal(&sealed), Err(Error::Decrypt));
    }

    #[test]
    fn identity_secret_round_trip_and_public_hex() {
        let id = Identity::generate();
        let again = Identity::from_secret_bytes(&id.secret_bytes());
        assert_eq!(id.public_hex(), again.public_hex());
        assert_eq!(id.public_hex().len(), 128);
        let sig = id.sign(b"m");
        assert!(verify(&again.sign_pk(), b"m", &sig).is_ok());
        assert!(verify(&again.sign_pk(), b"n", &sig).is_err());
    }
}
