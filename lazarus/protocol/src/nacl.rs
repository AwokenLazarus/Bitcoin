/// libsodium-compatible crypto_sign / crypto_box for DATUM.
use crypto_box::aead::{Aead, Payload};
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;

pub const SEAL_BYTES: usize = 48;
pub const MAC_BYTES: usize = 16;
pub const SIGN_BYTES: usize = 64;

pub fn sign_keypair() -> ([u8; 32], [u8; 64]) {
    let sk = SigningKey::generate(&mut OsRng);
    let pk = sk.verifying_key().to_bytes();
    (pk, sk.to_keypair_bytes())
}

pub fn box_keypair() -> ([u8; 32], [u8; 32]) {
    let sk = SecretKey::generate(&mut OsRng);
    let pk = PublicKey::from(&sk);
    (pk.to_bytes(), sk.to_bytes())
}

pub fn sign_detached(msg: &[u8], ed_sk64: &[u8; 64]) -> Result<[u8; 64], ()> {
    let sk = SigningKey::from_keypair_bytes(ed_sk64).map_err(|_| ())?;
    Ok(sk.sign(msg).to_bytes())
}

pub fn verify_detached(sig: &[u8; 64], msg: &[u8], ed_pk: &[u8; 32]) -> Result<(), ()> {
    let pk = VerifyingKey::from_bytes(ed_pk).map_err(|_| ())?;
    let s = Signature::from_bytes(sig);
    pk.verify(msg, &s).map_err(|_| ())
}

fn salsa(remote_x: &[u8; 32], local_x_sk: &[u8; 32]) -> Result<SalsaBox, ()> {
    let pk = PublicKey::from(*remote_x);
    let sk = SecretKey::from(*local_x_sk);
    Ok(SalsaBox::new(&pk, &sk))
}

fn to_sodium(ct_and_tag: &[u8]) -> Result<Vec<u8>, ()> {
    if ct_and_tag.len() < MAC_BYTES {
        return Err(());
    }
    let (ct, tag) = ct_and_tag.split_at(ct_and_tag.len() - MAC_BYTES);
    let mut o = tag.to_vec();
    o.extend_from_slice(ct);
    Ok(o)
}

fn from_sodium(mac_and_ct: &[u8]) -> Result<Vec<u8>, ()> {
    if mac_and_ct.len() < MAC_BYTES {
        return Err(());
    }
    let (tag, ct) = mac_and_ct.split_at(MAC_BYTES);
    let mut o = ct.to_vec();
    o.extend_from_slice(tag);
    Ok(o)
}

pub fn box_easy_afternm(
    plain: &[u8],
    nonce: &[u8; 24],
    remote_x: &[u8; 32],
    local_x_sk: &[u8; 32],
) -> Result<Vec<u8>, ()> {
    let b = salsa(remote_x, local_x_sk)?;
    let n = crypto_box::Nonce::from(*nonce);
    b.encrypt(&n, Payload { msg: plain, aad: b"" }).map_err(|_| ())
}

pub fn box_open_easy_afternm(
    cipher: &[u8],
    nonce: &[u8; 24],
    remote_x: &[u8; 32],
    local_x_sk: &[u8; 32],
) -> Result<Vec<u8>, ()> {
    let b = salsa(remote_x, local_x_sk)?;
    let n = crypto_box::Nonce::from(*nonce);
    b.decrypt(&n, Payload { msg: cipher, aad: b"" }).map_err(|_| ())
}

pub fn box_seal(plain: &[u8], recipient_x: &[u8; 32]) -> Result<Vec<u8>, ()> {
    let pk = PublicKey::from(*recipient_x);
    pk.seal(&mut OsRng, plain).map_err(|_| ())
}

pub fn box_seal_open(cipher: &[u8], _x_pk: &[u8; 32], x_sk: &[u8; 32]) -> Result<Vec<u8>, ()> {
    let sk = SecretKey::from(*x_sk);
    sk.unseal(cipher).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sign_roundtrip() {
        let (pk, sk) = sign_keypair();
        let sig = sign_detached(b"hello", &sk).unwrap();
        verify_detached(&sig, b"hello", &pk).unwrap();
        assert!(verify_detached(&sig, b"nope", &pk).is_err());
    }
    #[test]
    fn box_roundtrip() {
        let (apk, ask) = box_keypair();
        let (bpk, bsk) = box_keypair();
        let n = [7u8; 24];
        let c = box_easy_afternm(b"ping", &n, &bpk, &ask).unwrap();
        let p = box_open_easy_afternm(&c, &n, &apk, &bsk).unwrap();
        assert_eq!(p, b"ping");
    }
    #[test]
    fn seal_roundtrip() {
        let (pk, sk) = box_keypair();
        let c = box_seal(b"sealed", &pk).unwrap();
        let p = box_seal_open(&c, &pk, &sk).unwrap();
        assert_eq!(p, b"sealed");
    }
}
