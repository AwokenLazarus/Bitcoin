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
    /// Days without a credited share after which a balance under `min-payout` is *stale*: its
    /// owner has stopped mining and nothing more will be added to it. Stale balances are
    /// listed in `stats.json` and can be set aside for a payment made by hand (`payouts/`);
    /// with `stale-coinbase` they are also paid by the next block with room. 0 disables both.
    #[serde(default = "d_stale_after_days")]
    pub stale_after_days: u32,
    /// Smallest stale balance worth an output, sats. Not below the dust limit.
    #[serde(default = "d_stale_min_payout")]
    pub stale_min_payout: u64,
    /// Pay stale balances in the coinbase, after every miner in the window has been placed.
    #[serde(default = "d_true")]
    pub stale_coinbase: bool,
    /// Most stale balances one coinbase pays; a backlog drains over blocks.
    #[serde(default = "d_stale_per_block")]
    pub stale_per_block: usize,
    #[serde(default)]
    pub fee_bps: u32,
    /// Fee on an upgraded empty-solo coinbase (gateway + pool), basis points. Default 750
    /// (7.5%), matching the dedicated solo ports. Stock gateways without a fee output
    /// still classify as EmptySolo at 0% via the script-flip path.
    #[serde(default = "d_empty_solo_fee")]
    pub empty_solo_fee_bps: u32,
    /// How a stock gateway's *full* pool-only job is treated after Prime has seen one.
    /// `owe` (default): configure(pool) with each coinbaser so they pool-mine the split
    /// until the next tip. `gateway-solo`: leave them on their own script so those jobs
    /// become gateway-solo with no debt.
    #[serde(default = "d_owe")]
    pub stock_full_pool_only: String,
    /// Test hook: sleep this long before answering a coinbaser, so a stock gateway's
    /// 5 s fetch times out. 0 (default) is production.
    #[serde(default)]
    pub coinbaser_delay_ms: u64,
    /// Public house-stratum fee. 0 means use `fee_bps` (same rate for everyone).
    #[serde(default)]
    pub stratum_fee_bps: u32,
    /// Share of the house-stratum fee handed to DATUM work instead of kept, basis points of
    /// stratum work's value (750 = 7.5 points of a 15% stratum fee). 0 (default) disables it.
    #[serde(default)]
    pub datum_rebate_bps: u32,
    /// Grace for an address that starts on the house stratum (`tides::grace`): for this many
    /// hours from its first stratum share its stratum work pays `stratum-grace-fee-bps`
    /// instead of `stratum-fee-bps`. 0 (default): no grace.
    #[serde(default)]
    pub stratum_grace_hours: u32,
    /// The same, for an address that has had DATUM work credited here: someone whose own
    /// gateway is down, not someone mining on ours. 0 (default): same as `stratum-grace-hours`.
    #[serde(default)]
    pub stratum_grace_datum_hours: u32,
    /// Fee on house-stratum work inside its grace. Required when grace is on, and no higher
    /// than `stratum-fee-bps`.
    #[serde(default)]
    pub stratum_grace_fee_bps: Option<u32>,
    /// Share of the grace fee handed to DATUM work, basis points of the grace work's value.
    #[serde(default)]
    pub stratum_grace_rebate_bps: u32,
    /// Hours off the house stratum after which an address's next stratum share starts a new
    /// grace. 0 (default): one grace per address.
    #[serde(default)]
    pub stratum_grace_rearm_hours: u32,
    /// Unix time the grace is taken to have started for addresses already on the house
    /// stratum when grace is first switched on. 0 (default): their oldest share in the window.
    #[serde(default)]
    pub stratum_grace_epoch: u32,
    /// Share of a solo block's reward owed to DATUM work when a `solo-coinbase-tag` block
    /// paying the pool script lands on chain, basis points of the block's coinbase value.
    /// Paid down out of the pool's kept fee in later splits. 0 (default) disables it.
    #[serde(default)]
    pub solo_rebate_bps: u32,
    /// Coinbase tag the dedicated solo gateways stamp, for spotting their blocks on chain.
    #[serde(default = "d_solo_tag")]
    pub solo_coinbase_tag: String,
    /// Gateway keys (hex) that are the pool's own public stratum: the full 64-digit identity
    /// key, or a prefix of it no shorter than 16 digits. These sessions are trusted with
    /// things no stranger is (`Policy::trusted_target`), and a short prefix is one a stranger
    /// can grind a key to match, so give the full key.
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
    /// How a share is credited when its difficulty is not part of what was hashed and its
    /// gateway is not the pool's own: `2^this` if the hash meets it, nothing otherwise, whatever
    /// the share claims (see `Policy::uncommitted_pot`). Fair on average for work at any
    /// difficulty up to it; only work above it is under-credited, so keep it at or above the
    /// largest vardiff a gateway's miners run at. A power-of-two exponent: 20 is 1 048 576.
    #[serde(default = "d_uncommitted_pot")]
    pub uncommitted_pot: u8,
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
    /// How long a gateway is refused the first time its own node hands Prime a block the chain
    /// rejects (see `node::says_outdated_node`). The gateway builds its own template, so a
    /// consensus rule its node does not know is a block the whole window loses. Short on
    /// purpose: Prime cannot see a node's version, so the way back in is to upgrade and
    /// reconnect, and an operator who did that should not be kept waiting. 0 turns this off.
    #[serde(default = "d_quarantine_minutes")]
    pub quarantine_minutes: u64,
    /// The refusal doubles with each further rejected block and stops growing here. A gateway
    /// that was upgraded never reaches the second strike; one that was not is refused for
    /// longer and longer without ever being banned outright.
    #[serde(default = "d_quarantine_max_hours")]
    pub quarantine_max_hours: u64,
    /// Strikes are forgotten after this long without another rejected block, so an operator who
    /// upgrades months later starts clean.
    #[serde(default = "d_quarantine_forget_hours")]
    pub quarantine_forget_hours: u64,
    /// Gateway identity keys refused outright, whatever they submit. The 16-hex `gateway=`
    /// from the logs is enough; a longer prefix or the whole key also works.
    #[serde(default)]
    pub blocked_gateways: Vec<String>,
    /// Gateways that are another pool's stratum front: that pool's hashers point at its stratum
    /// port, and its own gateway and node relay them here as if they were DATUM miners. Their
    /// work is stratum work: charged `stratum-fee-bps`, with no grace (a grace is for a miner
    /// whose own gateway is down), and it is not a sighting on DATUM. Nothing else about the
    /// session changes: it gets none of the trust the pool's own gateway has. Matched like
    /// `blocked-gateways`, on 16 or more hex digits of the gateway key.
    #[serde(default)]
    pub stratum_front_gateways: Vec<String>,
    /// The same, by the address the gateway connects from: a front changes its key at will.
    #[serde(default)]
    pub stratum_front_ips: Vec<std::net::IpAddr>,
    /// Coinbase section bytes one session may have Prime hold across all of its job slots.
    /// A stock gateway's eight slots of seven ~16 KiB coinbase classes is under 1 MiB; the
    /// sixteen live slots Prime keeps at eight 20 000-byte sections each is 2.5 MiB.
    #[serde(default = "d_session_coinbase_budget")]
    pub session_coinbase_budget: usize,
    /// Refuse a hello whose user agent is not `lazarus-gateway*` and does not contain
    /// `lazarus-split`. Stock OCEAN / FlyTheElephant empty-first jobs and Convoy size-class
    /// prefixes cannot put a full TIDES split in the coinbase; closing the session is the
    /// only pool-side way to stop them hashing those jobs as us. Default off; Lazarus sets
    /// this true.
    #[serde(default)]
    pub require_split_gateway: bool,

    // Keys the previous Prime used. Accepted so an existing config starts unchanged;
    // `load` reports each one it saw.
    #[serde(default)]
    activation_height: Option<u32>,
    #[serde(default)]
    verify_shares: Option<String>,
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
fn d_stale_per_block() -> usize {
    25
}
fn d_stale_after_days() -> u32 {
    7
}
fn d_stale_min_payout() -> u64 {
    10_000
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
fn d_uncommitted_pot() -> u8 {
    20
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

fn d_quarantine_minutes() -> u64 {
    60
}

fn d_quarantine_max_hours() -> u64 {
    24
}

fn d_quarantine_forget_hours() -> u64 {
    168
}
fn d_session_coinbase_budget() -> usize {
    4 << 20
}
fn d_empty_solo_fee() -> u32 {
    750
}
fn d_owe() -> String {
    "owe".into()
}
fn d_solo_tag() -> String {
    "Lazarus/solo".into()
}

impl Config {
    /// `stale-after-days` in seconds; 0 when the rule is off.
    pub fn stale_after_secs(&self) -> u32 {
        self.stale_after_days.saturating_mul(86_400)
    }

    /// The stratum grace clocks' settings. Not enabled unless `stratum-grace-hours` is set.
    pub fn grace(&self) -> tides::GraceParams {
        let secs = self.stratum_grace_hours.saturating_mul(3_600);
        tides::GraceParams {
            secs,
            datum_secs: self.stratum_grace_datum_hours.saturating_mul(3_600).max(secs),
            rearm_secs: self.stratum_grace_rearm_hours.saturating_mul(3_600),
            epoch: self.stratum_grace_epoch,
        }
    }

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
        if c.stale_min_payout < 546 {
            return Err("stale-min-payout cannot be below the dust limit (546)".into());
        }
        if c.stale_after_days > 3650 {
            return Err("stale-after-days is in days (3650 at most; 0 disables)".into());
        }
        if c.stratum_fee_bps == 0 {
            c.stratum_fee_bps = c.fee_bps;
        }
        if c.stratum_fee_bps > 10_000 {
            return Err("stratum-fee-bps cannot exceed 10000".into());
        }
        if c.datum_rebate_bps > c.stratum_fee_bps {
            return Err("datum-rebate-bps cannot exceed stratum-fee-bps (the rebate comes out of that fee)".into());
        }
        if c.solo_rebate_bps > 10_000 {
            return Err("solo-rebate-bps cannot exceed 10000".into());
        }
        if c.stratum_grace_hours > 8_760 || c.stratum_grace_datum_hours > 8_760 || c.stratum_grace_rearm_hours > 87_600
        {
            return Err("stratum-grace-*-hours are in hours (a year at most; ten for the re-arm)".into());
        }
        if c.grace().enabled() {
            let Some(fee) = c.stratum_grace_fee_bps else {
                return Err("stratum-grace-fee-bps must be set when stratum-grace-hours is (0 is a free grace)".into());
            };
            if fee > c.stratum_fee_bps {
                return Err("stratum-grace-fee-bps cannot exceed stratum-fee-bps (a grace is not a penalty)".into());
            }
            if c.stratum_grace_rebate_bps > fee {
                return Err(
                    "stratum-grace-rebate-bps cannot exceed stratum-grace-fee-bps (the rebate comes out of that fee)"
                        .into(),
                );
            }
            if c.stratum_grace_datum_hours != 0 && c.stratum_grace_datum_hours < c.stratum_grace_hours {
                return Err("stratum-grace-datum-hours cannot be shorter than stratum-grace-hours".into());
            }
        } else if c.stratum_grace_fee_bps.is_some()
            || c.stratum_grace_rebate_bps != 0
            || c.stratum_grace_rearm_hours != 0
            || c.stratum_grace_epoch != 0
        {
            return Err("stratum-grace-* keys are set but stratum-grace-hours is 0: set it, or remove them".into());
        }
        if c.solo_coinbase_tag.is_empty() || c.solo_coinbase_tag.len() > 32 {
            return Err("solo-coinbase-tag must be 1..=32 bytes".into());
        }
        for g in &mut c.house_gateways {
            *g = g.to_ascii_lowercase();
        }
        for g in &mut c.stratum_front_gateways {
            *g = g.trim().to_ascii_lowercase();
            if g.len() < 16 || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("stratum-front-gateways: {g:?} is not 16 or more hex digits of a gateway key"));
            }
        }
        if c.uncommitted_pot > 63 {
            return Err("uncommitted-pot is a power-of-two exponent, 63 at most".into());
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
        if c.empty_solo_fee_bps > 10_000 {
            return Err("empty-solo-fee-bps cannot exceed 10000".into());
        }
        match c.stock_full_pool_only.as_str() {
            "owe" | "gateway-solo" => {}
            other => return Err(format!("stock-full-pool-only must be owe or gateway-solo, not {other:?}")),
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
        // Said, not enforced: a Prime that will not start takes every gateway down with it.
        for g in &self.house_gateways {
            if !g.bytes().all(|b| b.is_ascii_hexdigit()) || g.len() > 64 {
                v.push(format!("house-gateways entry {g:?} is not hex digits of a gateway key and matches nothing"));
            } else if g.len() < 64 {
                v.push(format!(
                    "house-gateways entry {g:?} is a {}-digit prefix: a house gateway is trusted with its shares' difficulty, and a short prefix is one a stranger can grind a key to match. Give the full 64-digit key",
                    g.len()
                ));
            }
        }
        if self.activation_height.is_some() {
            v.push("activation-height is ignored: every share is verified as BLAKE2b header v2; SHA256d shares are rejected as bad-version".into());
        }
        if let Some(mode) = &self.verify_shares {
            v.push(format!("verify-shares = {mode:?} is ignored: shares are always verified and the coinbase is always checked against the issued TIDES split"));
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
        assert_eq!(c.legacy_notes().len(), 2);
        assert!(c.require_split_gateway);
        // a data dir the old Prime left behind keeps its key, hence its pubkey
        std::fs::write(dir.join("lazarus-prime.key"), "00").unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.key_file(), dir.join("lazarus-prime.key"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rebate_knobs_load_default_off_and_are_bounded_by_the_stratum_fee() {
        let dir = std::env::temp_dir().join(format!("primed-cfg-r-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("prime.toml");
        let base = LEGACY.replace("/home/umbrel/blake2b/lazarus-prime", dir.to_str().unwrap());
        // legacy config: rebates off, tag defaulted
        std::fs::write(&p, &base).unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!((c.datum_rebate_bps, c.solo_rebate_bps), (0, 0));
        assert_eq!(c.solo_coinbase_tag, "Lazarus/solo");
        // production Lazarus knobs (DATUM 0% is fee-bps in the live toml; this fixture's
        // legacy body still has fee-bps = 50) plus a solo rebate to prove that knob loads
        std::fs::write(&p, format!("{base}\nstratum-fee-bps = 1000\ndatum-rebate-bps = 500\nsolo-rebate-bps = 100\n"))
            .unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!((c.fee_bps, c.stratum_fee_bps, c.datum_rebate_bps, c.solo_rebate_bps), (50, 1000, 500, 100));
        // a rebate larger than the fee it comes out of is a config error, not a silent clamp
        std::fs::write(&p, format!("{base}\nstratum-fee-bps = 1000\ndatum-rebate-bps = 1001\n")).unwrap();
        assert!(Config::load(&p).unwrap_err().contains("datum-rebate-bps"));

        // stratum grace: off unless asked for
        std::fs::write(&p, &base).unwrap();
        assert!(!Config::load(&p).unwrap().grace().enabled());
        // the donation endpoint with its grace, and the same grace before the fee is raised
        let grace = "stratum-grace-hours = 24\nstratum-grace-datum-hours = 96\nstratum-grace-fee-bps = 2500\n\
                     stratum-grace-rebate-bps = 1250\nstratum-grace-rearm-hours = 168\nstratum-grace-epoch = 1791262800\n";
        for (stratum, rebate) in [(10_000, 5_000), (2_500, 1_250)] {
            std::fs::write(&p, format!("{base}\nstratum-fee-bps = {stratum}\ndatum-rebate-bps = {rebate}\n{grace}"))
                .unwrap();
            let c = Config::load(&p).unwrap();
            let g = c.grace();
            assert!(g.enabled());
            assert_eq!((g.secs, g.datum_secs, g.rearm_secs, g.epoch), (86_400, 345_600, 604_800, 1_791_262_800));
            assert_eq!((c.stratum_grace_fee_bps, c.stratum_grace_rebate_bps), (Some(2_500), 1_250));
        }
        // datum hours default to the plain grace
        std::fs::write(
            &p,
            format!("{base}\nstratum-fee-bps = 1000\nstratum-grace-hours = 24\nstratum-grace-fee-bps = 0\n"),
        )
        .unwrap();
        assert_eq!(Config::load(&p).unwrap().grace().datum_secs, 86_400);
        // another pool's stratum front, by key and by address
        std::fs::write(
            &p,
            format!(
                "{base}\nstratum-front-gateways = [\"097B7017CCFD7669\"]\nstratum-front-ips = [\"207.244.247.51\"]\n"
            ),
        )
        .unwrap();
        let c = Config::load(&p).unwrap();
        assert_eq!(c.stratum_front_gateways, vec!["097b7017ccfd7669".to_string()]);
        assert_eq!(c.stratum_front_ips, vec!["207.244.247.51".parse::<std::net::IpAddr>().unwrap()]);
        std::fs::write(&p, &base).unwrap();
        let c = Config::load(&p).unwrap();
        assert!(c.stratum_front_gateways.is_empty() && c.stratum_front_ips.is_empty());
        for bad in [
            "stratum-front-gateways = [\"097b7017\"]",
            "stratum-front-gateways = [\"not-a-key-not-a-key\"]",
            "stratum-front-ips = [\"ctrlpool.com\"]",
        ] {
            std::fs::write(&p, format!("{base}\n{bad}\n")).unwrap();
            assert!(Config::load(&p).is_err(), "{bad}");
        }
        // each of these is a mistake, and none is silently repaired
        for (keys, names) in [
            ("stratum-grace-hours = 24\n", "stratum-grace-fee-bps must be set"),
            ("stratum-grace-hours = 24\nstratum-grace-fee-bps = 1001\n", "cannot exceed stratum-fee-bps"),
            (
                "stratum-grace-hours = 24\nstratum-grace-fee-bps = 500\nstratum-grace-rebate-bps = 501\n",
                "stratum-grace-rebate-bps",
            ),
            (
                "stratum-grace-hours = 24\nstratum-grace-datum-hours = 12\nstratum-grace-fee-bps = 500\n",
                "cannot be shorter",
            ),
            ("stratum-grace-fee-bps = 500\n", "stratum-grace-hours is 0"),
            ("stratum-grace-epoch = 5\n", "stratum-grace-hours is 0"),
        ] {
            std::fs::write(&p, format!("{base}\nstratum-fee-bps = 1000\n{keys}")).unwrap();
            let e = Config::load(&p).unwrap_err();
            assert!(e.contains(names), "{keys:?}: {e}");
        }
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

    /// A house gateway is trusted with its shares' difficulty, so a key prefix short enough to
    /// grind is worth a warning. It is never worth refusing to start: that takes the pool down.
    #[test]
    fn a_weak_house_gateway_entry_is_warned_about_not_fatal() {
        let dir = std::env::temp_dir().join(format!("primed-cfg-h-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("prime.toml");
        let base = LEGACY.replace("/home/umbrel/blake2b/lazarus-prime", dir.to_str().unwrap());
        let load = |entry: &str| {
            std::fs::write(&p, format!("{base}house-gateways = [\"{entry}\"]\n")).unwrap();
            Config::load(&p).expect("loads whatever the entry")
        };
        let notes = |c: &Config| c.legacy_notes().into_iter().filter(|n| n.contains("house-gateways")).count();
        let full = "9D992E5CFEC05102".repeat(4);
        let c = load(&full);
        assert_eq!(c.house_gateways, vec![full.to_ascii_lowercase()]);
        assert_eq!(notes(&c), 0);
        for weak in ["9d99", "9d992e5cfec05102", "9d992e5cfec0510g", &format!("{full}00")] {
            assert_eq!(notes(&load(weak)), 1, "{weak:?}");
        }
        assert_eq!(load(&full).uncommitted_pot, 20);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
