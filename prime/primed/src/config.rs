//! `prime.toml`. Keys are kebab-case to match the pool's existing install scripts.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// DATUM listener.
    #[serde(default = "d_listen")]
    pub listen: SocketAddr,
    /// HTTP listener for `/stats.json`, `/healthz`.
    #[serde(default = "d_stats_listen")]
    pub stats_listen: SocketAddr,
    /// What gateways should point at; only reported in stats.
    #[serde(default)]
    pub advertise_address: String,
    /// Ledger, key, block log, and exported `stats.json`/`ledger.json` live here.
    pub data_dir: PathBuf,
    /// Pool key file (64 hex bytes: ed25519 seed || x25519 secret). Generated if missing.
    #[serde(default)]
    pub key_file: Option<PathBuf>,
    #[serde(default = "d_motd")]
    pub motd: String,
    /// Address the pool's remainder and fee go to. Also the address gateways are configured with.
    pub payout_address: String,
    #[serde(default = "d_tag")]
    pub coinbase_tag: String,
    /// Identifies this Prime to gateways; nonzero.
    #[serde(default = "d_prime_id")]
    pub prime_id: u32,
    /// TIDES window in multiples of network difficulty.
    #[serde(default = "d_window")]
    pub window: u32,
    /// Floor on the window target, in difficulty-1 shares. Only matters where the network
    /// difficulty is tiny (regtest, fresh testnets); 0 disables.
    #[serde(default)]
    pub window_min_work: u64,
    /// Dust floor for split outputs, sats.
    #[serde(default = "d_min_payout")]
    pub min_payout: u64,
    #[serde(default)]
    pub fee_bps: u32,
    /// Public house-stratum fee. 0 means use `fee_bps` (same rate for everyone).
    #[serde(default)]
    pub stratum_fee_bps: u32,
    /// Gateway key prefixes (hex) that are the pool's own public stratum.
    #[serde(default)]
    pub house_gateways: Vec<String>,
    /// Treat loopback Prime connections as house stratum. Default on.
    #[serde(default = "d_true")]
    pub house_loopback: bool,
    /// Smallest share difficulty gateways may send (power of two). Also sent as the vardiff floor.
    #[serde(default = "d_min_diff")]
    pub min_diff: u64,
    /// Slack per issued output when checking a coinbase, sats.
    #[serde(default = "d_tolerance")]
    pub split_tolerance: u64,
    /// Which network's addresses to accept: mainnet | testnet | signet | regtest.
    #[serde(default = "d_network")]
    pub network: String,
    /// Node JSON-RPC endpoint and credentials. Cookie wins if both are set.
    pub rpc: String,
    #[serde(default)]
    pub rpc_cookie: Option<PathBuf>,
    #[serde(default)]
    pub rpc_user: Option<String>,
    #[serde(default)]
    pub rpc_password: Option<String>,
    /// Node poll interval, seconds.
    #[serde(default = "d_poll")]
    pub poll: f64,
    /// Shares for a height this many blocks behind the tip are stale. 0 means only the
    /// current height. The default tolerates a template refresh in flight.
    #[serde(default = "d_stale_grace")]
    pub stale_grace_secs: u32,
    /// Import this legacy `ledger.json` once on first start (if the ledger is empty).
    #[serde(default)]
    pub import_ledger: Option<PathBuf>,
    /// Shown in stats.
    #[serde(default = "d_headline")]
    pub headline: String,
    /// Most DATUM sessions held open at once. A session buffers job and coinbase state on
    /// the gateway's behalf, so this bounds what an unknown key can make the pool hold.
    #[serde(default = "d_max_connections")]
    pub max_connections: u32,
    /// Most sessions from one remote address. A gateway is one connection; a farm is a few.
    #[serde(default = "d_max_connections_per_ip")]
    pub max_connections_per_ip: u32,
    /// Coinbase section bytes one session may have Prime hold across all of its job slots.
    /// A stock gateway's eight slots of seven ~16 KiB coinbase classes is under 1 MiB; the
    /// sixteen live slots Prime keeps at eight 20 000-byte sections each is 2.5 MiB.
    #[serde(default = "d_session_coinbase_budget")]
    pub session_coinbase_budget: usize,

    // Keys the previous Prime used. Accepted so an existing config starts unchanged;
    // `load` reports each one it saw.
    #[serde(default)]
    activation_height: Option<u32>,
    #[serde(default)]
    verify_shares: Option<String>,
    #[serde(default)]
    require_split_gateway: Option<bool>,
}

fn d_listen() -> SocketAddr {
    "0.0.0.0:28915".parse().unwrap()
}
fn d_stats_listen() -> SocketAddr {
    "127.0.0.1:28916".parse().unwrap()
}
fn d_motd() -> String {
    "Lazarus".into()
}
fn d_tag() -> String {
    "Lazarus".into()
}
fn d_prime_id() -> u32 {
    1
}
fn d_window() -> u32 {
    8
}
fn d_min_payout() -> u64 {
    546
}
fn d_min_diff() -> u64 {
    1
}
fn d_tolerance() -> u64 {
    2
}
fn d_network() -> String {
    "mainnet".into()
}
fn d_poll() -> f64 {
    0.5
}
fn d_stale_grace() -> u32 {
    30
}
fn d_headline() -> String {
    "Lazarus".into()
}
fn d_true() -> bool {
    true
}
fn d_max_connections() -> u32 {
    256
}
fn d_max_connections_per_ip() -> u32 {
    8
}
fn d_session_coinbase_budget() -> usize {
    4 << 20
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut c: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if !c.min_diff.is_power_of_two() {
            return Err("min-diff must be a power of two".into());
        }
        if c.prime_id == 0 {
            return Err("prime-id must be nonzero".into());
        }
        if c.window == 0 {
            return Err("window must be at least 1".into());
        }
        if c.fee_bps > 10_000 {
            return Err("fee-bps cannot exceed 10000".into());
        }
        if c.stratum_fee_bps == 0 {
            c.stratum_fee_bps = c.fee_bps;
        }
        if c.stratum_fee_bps > 10_000 {
            return Err("stratum-fee-bps cannot exceed 10000".into());
        }
        for g in &mut c.house_gateways {
            *g = g.to_ascii_lowercase();
        }
        if c.coinbase_tag.len() > 32 {
            return Err("coinbase-tag is too long (32 bytes max)".into());
        }
        if c.max_connections == 0 || c.max_connections_per_ip == 0 {
            return Err("max-connections and max-connections-per-ip must be at least 1".into());
        }
        if c.session_coinbase_budget < 64 * 1024 {
            return Err("session-coinbase-budget must be at least 65536 bytes (one huge coinbase class)".into());
        }
        if c.key_file.is_none() {
            // A data dir left by lazarus-prime keeps its identity: same key file, same pubkey.
            let ours = c.data_dir.join("prime.key");
            let legacy = c.data_dir.join("lazarus-prime.key");
            c.key_file = Some(if !ours.exists() && legacy.exists() { legacy } else { ours });
        }
        Ok(c)
    }

    /// One line per legacy key present in the file, explaining why it no longer applies.
    pub fn legacy_notes(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.activation_height.is_some() {
            v.push("activation-height is ignored: every share is verified as BLAKE2b header v2; SHA256d shares are rejected as bad-version".into());
        }
        if let Some(mode) = &self.verify_shares {
            v.push(format!("verify-shares = {mode:?} is ignored: shares are always verified and the coinbase is always checked against the issued TIDES split"));
        }
        if self.require_split_gateway.is_some() {
            v.push("require-split-gateway is ignored: stock DATUM gateways pay the split from the coinbaser reply, so none need a patched user agent".into());
        }
        v
    }

    pub fn key_file(&self) -> &Path {
        self.key_file.as_deref().unwrap()
    }

    pub fn min_pot(&self) -> u8 {
        self.min_diff.trailing_zeros() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file the previous Prime shipped, minus the placeholder address.
    const LEGACY: &str = r#"
listen = "0.0.0.0:28915"
stats-listen = "127.0.0.1:28916"
advertise-address = "stratum.awokenlazarus.xyz:28915"
data-dir = "/home/umbrel/blake2b/lazarus-prime"
motd = "Lazarus"
min-diff = 1
payout-address = "bc1qt5praystcdle0nq04e3h02yjszha82uzhww85x6972lcy40k4eyqz9jfaq"
coinbase-tag = "Lazarus"
prime-id = 1
window = 8
min-payout = 546
fee-bps = 50
activation-height = 961640
headline = "Lazarus"
rpc = "http://127.0.0.1:9332"
rpc-cookie = "/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie"
poll = 0.5
verify-shares = "enforce"
require-split-gateway = true
"#;

    #[test]
    fn legacy_prime_toml_loads_unchanged() {
        let dir = std::env::temp_dir().join(format!("primed-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("prime.toml");
        let text = LEGACY.replace("/home/umbrel/blake2b/lazarus-prime", dir.to_str().unwrap());
        std::fs::write(&p, &text).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.listen.port(), 28915);
        assert_eq!(c.fee_bps, 50);
        assert_eq!(c.stratum_fee_bps, 50);
        assert!(c.house_loopback);
        assert_eq!(c.window, 8);
        assert_eq!(c.key_file(), dir.join("prime.key"));
        assert_eq!(c.min_pot(), 0);
        assert_eq!(c.legacy_notes().len(), 3);
        // a data dir the old Prime left behind keeps its key, hence its pubkey
        std::fs::write(dir.join("lazarus-prime.key"), "00").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.key_file(), dir.join("lazarus-prime.key"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_keys_are_still_errors() {
        let dir = std::env::temp_dir().join(format!("primed-cfg-u-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("prime.toml");
        std::fs::write(&p, format!("{LEGACY}\nfee_percent = 1\n")).unwrap();
        let r = Config::load(&p);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(r.unwrap_err().contains("fee_percent"));
    }
}
