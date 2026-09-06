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

/// Largest non-`OP_RETURN` scriptPubKey an output of the generation transaction may carry
/// while RDTS is enforced, and the larger allowance for an `OP_RETURN` one.
pub const RDTS_MAX_OUTPUT_SCRIPT: usize = 34;
pub const RDTS_MAX_OUTPUT_DATA: usize = 83;

/// Whether a script may appear as a coinbase output under RDTS (BIP 110), which Knots
/// activates as a flag day at the BLAKE2b fork height. A block carrying a larger output
/// script is rejected as `bad-txns-vout-script-toolarge`, so such a script can never be
/// paid. Gateways enforce it as well: an oversized miner payout is left out of the coinbase
/// and an oversized *pool* payout stops the gateway serving work for the block at all.
pub fn rdts_output_ok(script: &[u8]) -> bool {
    match script.first() {
        None => true,
        Some(0x6a) => script.len() <= RDTS_MAX_OUTPUT_DATA,
        Some(_) => script.len() <= RDTS_MAX_OUTPUT_SCRIPT,
    }
}

/// Decode an address into its output script. `None` if it is not a valid address for `net`,
/// or if the script it decodes to could not be paid in a coinbase under RDTS — a witness
/// program longer than 32 bytes is well-formed but unpayable here, and treating it as
/// unpayable keeps its share in the pool remainder instead of handing the gateway an output
/// it would silently drop. Use [`decode_script`] to tell the two rejections apart.
pub fn to_script(addr: &str, net: Network) -> Option<Vec<u8>> {
    decode_script(addr, net).filter(|s| rdts_output_ok(s))
}

/// Decode an address into its output script with no coinbase-payability check.
pub fn decode_script(addr: &str, net: Network) -> Option<Vec<u8>> {
    let addr = addr.trim();
    if addr.is_empty() || addr.len() > 90 {
        return None;
    }
    let script = if let Ok((hrp, ver, prog)) = bech32::segwit::decode(addr) {
        if !hrp.as_str().eq_ignore_ascii_case(net.hrp()) {
            return None;
        }
        segwit_script(ver, &prog)
    } else {
        let raw = bs58::decode(addr).with_check(None).into_vec().ok()?;
        if raw.len() != 21 {
            return None;
        }
        let (ver, hash) = (raw[0], &raw[1..]);
        if ver == net.p2pkh_version() {
            let mut s = vec![0x76, 0xa9, 0x14];
            s.extend_from_slice(hash);
            s.extend_from_slice(&[0x88, 0xac]);
            s
        } else if ver == net.p2sh_version() {
            let mut s = vec![0xa9, 0x14];
            s.extend_from_slice(hash);
            s.push(0x87);
            s
        } else {
            return None;
        }
    };
    Some(script)
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

/// The form an identity is interned and paid under. Bech32 is case-insensitive, so `BC1Q…`
/// and `bc1q…` are one payout address and must be one TIDES row: split as two they can each
/// fall under `min-payout` and be paid nothing, and when they do pay they burn two coinbase
/// outputs. BIP 173 forbids mixed case, so anything the decoder accepts is already one case
/// and lowercasing it is safe and idempotent. Base58 is case-sensitive and is kept byte-exact;
/// so is anything that is not an address at all.
pub fn canonical_identity(ident: &str) -> String {
    if bech32::segwit::decode(ident).is_ok() {
        return ident.to_ascii_lowercase();
    }
    ident.to_string()
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
    fn rdts_rejects_output_scripts_a_coinbase_cannot_carry() {
        assert!(rdts_output_ok(&[]));
        assert!(rdts_output_ok(&[0x6a; RDTS_MAX_OUTPUT_DATA]));
        assert!(!rdts_output_ok(&[0x6a; RDTS_MAX_OUTPUT_DATA + 1]));
        assert!(rdts_output_ok(&[0x51; RDTS_MAX_OUTPUT_SCRIPT]));
        assert!(!rdts_output_ok(&[0x51; RDTS_MAX_OUTPUT_SCRIPT + 1]));

        // A 40-byte witness program is a valid address but a 42-byte output script, which
        // no block may carry once RDTS is enforced, so it is not payable.
        let hrp = bech32::Hrp::parse("bc").unwrap();
        let long = bech32::segwit::encode(hrp, Fe32::Z, &[0xab; 40]).unwrap();
        assert!(to_script(&long, Network::Mainnet).is_none());
        // 32 bytes is the largest program that still fits.
        let fits = bech32::segwit::encode(hrp, Fe32::P, &[0xab; 32]).unwrap();
        assert_eq!(to_script(&fits, Network::Mainnet).unwrap().len(), RDTS_MAX_OUTPUT_SCRIPT);
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

    #[test]
    fn bech32_identities_fold_to_one_case_and_the_same_script() {
        let lower = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        let upper = lower.to_ascii_uppercase();
        assert_eq!(canonical_identity(&upper), lower);
        assert_eq!(canonical_identity(lower), lower);
        assert_eq!(canonical_identity(&canonical_identity(&upper)), lower, "idempotent");
        assert_eq!(to_script(&upper, Network::Mainnet), to_script(lower, Network::Mainnet));
        let taproot = "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0";
        assert_eq!(canonical_identity(&taproot.to_ascii_uppercase()), taproot);
        // base58 stays byte-exact: changing case changes the address
        let legacy = "1BvBMSEYstWetqTFn5Au4m4GFg7xJaNVN2";
        assert_eq!(canonical_identity(legacy), legacy);
        assert_eq!(canonical_identity(&legacy.to_ascii_lowercase()), legacy.to_ascii_lowercase());
        // a mixed-case bech32 string is not an address and is not folded
        let mixed = "bc1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4";
        assert_eq!(canonical_identity(mixed), mixed);
        assert_eq!(canonical_identity("worker1"), "worker1");
        assert_eq!(canonical_identity("BC1QNOTREALLYANADDRESS"), "BC1QNOTREALLYANADDRESS");
    }
}
