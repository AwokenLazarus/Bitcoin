use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use thiserror::Error;

use crate::nacl;

#[derive(Clone)]
pub struct PoolKeys {
    pub ed_pk: [u8; 32],
    pub ed_sk: [u8; 64],
    pub x_pk: [u8; 32],
    pub x_sk: [u8; 32],
}

#[derive(Clone)]
pub struct SessionKeys {
    pub ed_pk: [u8; 32],
    pub ed_sk: [u8; 64],
    pub x_pk: [u8; 32],
    pub x_sk: [u8; 32],
}

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("key file must be 160 hex-decoded bytes")]
    BadLen,
}

pub fn generate_pool_keys() -> PoolKeys {
    let (ed_pk, ed_sk) = nacl::sign_keypair();
    let (x_pk, x_sk) = nacl::box_keypair();
    PoolKeys { ed_pk, ed_sk, x_pk, x_sk }
}

pub fn generate_session() -> SessionKeys {
    let (ed_pk, ed_sk) = nacl::sign_keypair();
    let (x_pk, x_sk) = nacl::box_keypair();
    SessionKeys { ed_pk, ed_sk, x_pk, x_sk }
}

/// On-disk order matches the live Umbrel key file (ed pk, ed sk, x pk, x sk).
fn encode(k: &PoolKeys) -> [u8; 160] {
    let mut o = [0u8; 160];
    o[0..32].copy_from_slice(&k.ed_pk);
    o[32..96].copy_from_slice(&k.ed_sk);
    o[96..128].copy_from_slice(&k.x_pk);
    o[128..160].copy_from_slice(&k.x_sk);
    o
}

fn decode(b: &[u8]) -> Result<PoolKeys, KeyError> {
    if b.len() != 160 {
        return Err(KeyError::BadLen);
    }
    let mut k = PoolKeys {
        ed_sk: [0; 64],
        ed_pk: [0; 32],
        x_sk: [0; 32],
        x_pk: [0; 32],
    };
    k.ed_pk.copy_from_slice(&b[0..32]);
    k.ed_sk.copy_from_slice(&b[32..96]);
    k.x_pk.copy_from_slice(&b[96..128]);
    k.x_sk.copy_from_slice(&b[128..160]);
    Ok(k)
}

/// Load a 320-hex-char key file. If `path` is missing, try sibling `ratum-prime.key`
/// (same 160-byte layout) so a Prime cutover keeps the advertised pool pubkey.
pub fn load_or_create_pool_keys(path: &Path) -> Result<PoolKeys, KeyError> {
    if path.exists() {
        return load_key_file(path);
    }
    if let Some(dir) = path.parent() {
        let legacy = dir.join("ratum-prime.key");
        if legacy.exists() {
            return load_key_file(&legacy);
        }
        fs::create_dir_all(dir)?;
    }
    let k = generate_pool_keys();
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true).mode(0o600);
    let mut f = opts.open(path)?;
    f.write_all(hex::encode(encode(&k)).as_bytes())?;
    Ok(k)
}

fn load_key_file(path: &Path) -> Result<PoolKeys, KeyError> {
    let mut f = fs::File::open(path)?;
    let mut hx = String::new();
    f.read_to_string(&mut hx)?;
    let raw = hex::decode(hx.trim()).map_err(|_| KeyError::BadLen)?;
    decode(&raw)
}

impl PoolKeys {
    pub fn pubkey_hex(&self) -> String {
        let mut v = [0u8; 64];
        v[..32].copy_from_slice(&self.ed_pk);
        v[32..].copy_from_slice(&self.x_pk);
        hex::encode(v)
    }
}
