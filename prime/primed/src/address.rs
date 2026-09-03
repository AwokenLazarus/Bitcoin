//! Payout address → scriptPubKey. Bech32/bech32m segwit and base58check legacy.

use bech32::Fe32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Testnet,
    Signet,
    Regtest,
}

impl Network {
    pub fn parse(s: &str) -> Option<Network> {
        Some(match s {
            "mainnet" | "main" | "bitcoin" => Network::Mainnet,
            "testnet" | "test" | "testnet4" | "testnet3" => Network::Testnet,
            "signet" => Network::Signet,
            "regtest" => Network::Regtest,
            _ => return None,
        })
    }

    fn hrp(self) -> &'static str {
        match self {
            Network::Mainnet => "bc",
            Network::Testnet | Network::Signet => "tb",
            Network::Regtest => "bcrt",
        }
    }

    fn p2pkh_version(self) -> u8 {
        match self {
            Network::Mainnet => 0x00,
            _ => 0x6f,
        }
    }

    fn p2sh_version(self) -> u8 {
        match self {
            Network::Mainnet => 0x05,
            _ => 0xc4,
        }
    }
}

/// Decode an address into its output script. `None` if it is not a valid address for `net`.
pub fn to_script(addr: &str, net: Network) -> Option<Vec<u8>> {
    let addr = addr.trim();
    if addr.is_empty() || addr.len() > 90 {
        return None;
    }
    if let Ok((hrp, ver, prog)) = bech32::segwit::decode(addr) {
        if !hrp.as_str().eq_ignore_ascii_case(net.hrp()) {
            return None;
        }
        return Some(segwit_script(ver, &prog));
    }
    let raw = bs58::decode(addr).with_check(None).into_vec().ok()?;
    if raw.len() != 21 {
        return None;
    }
    let (ver, hash) = (raw[0], &raw[1..]);
    if ver == net.p2pkh_version() {
        let mut s = vec![0x76, 0xa9, 0x14];
        s.extend_from_slice(hash);
        s.extend_from_slice(&[0x88, 0xac]);
        Some(s)
    } else if ver == net.p2sh_version() {
        let mut s = vec![0xa9, 0x14];
        s.extend_from_slice(hash);
        s.push(0x87);
        Some(s)
    } else {
        None
    }
}

fn segwit_script(ver: Fe32, prog: &[u8]) -> Vec<u8> {
    let v = ver.to_u8();
    let mut s = Vec::with_capacity(2 + prog.len());
    s.push(if v == 0 { 0x00 } else { 0x50 + v });
    s.push(prog.len() as u8);
    s.extend_from_slice(prog);
    s
}

/// The identity a DATUM username maps to: the part before the first `.` (worker suffix)
/// or `~` (gateway username modifier). A gateway with `stratum_username_mod` resolves
/// `addr~mod.worker` itself; one without forwards it verbatim, and the pool must not
/// treat `addr~mod` as an address.
pub fn identity_of(username: &str) -> &str {
    let u = username.trim();
    let end = u.find(['.', '~']).unwrap_or(u.len());
    &u[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segwit_and_legacy() {
        let s = to_script("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4", Network::Mainnet).unwrap();
        assert_eq!(hex::encode(s), "0014751e76e8199196d454941c45d1b3a323f1433bd6");
        let s = to_script("bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0", Network::Mainnet).unwrap();
        assert_eq!(hex::encode(s), "512079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798");
        let s = to_script("1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2", Network::Mainnet).unwrap();
        assert_eq!(hex::encode(s), "76a91477bff20c60e522dfaa3350c39b030a5d004e839a88ac");
        let s = to_script("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy", Network::Mainnet).unwrap();
        assert_eq!(hex::encode(s), "a914b472a266d0bd89c13706a4132ccfb16f7c3b9fcb87");
        assert!(to_script("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", Network::Mainnet).is_none());
        assert!(to_script("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx", Network::Testnet).is_some());
        assert!(to_script("worker1", Network::Mainnet).is_none());
        assert!(to_script("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t5", Network::Mainnet).is_none());
    }

    #[test]
    fn identity_strips_worker() {
        assert_eq!(identity_of("bc1qabc.rig1"), "bc1qabc");
        assert_eq!(identity_of(" bc1qabc "), "bc1qabc");
        assert_eq!(identity_of("plain"), "plain");
        assert_eq!(identity_of("bc1qabc~x.rig1"), "bc1qabc");
        assert_eq!(identity_of("bc1qabc.rig1~x"), "bc1qabc");
        assert_eq!(identity_of("~x"), "");
    }
}
