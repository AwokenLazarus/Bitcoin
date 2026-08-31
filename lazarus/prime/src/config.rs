use clap::Parser;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "lazarus-prime", about = "Lazarus DATUM Prime")]
pub struct Cli {
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct FileCfg {
    listen: Option<String>,
    #[serde(rename = "stats-listen")]
    stats_listen: Option<String>,
    #[serde(rename = "advertise-address")]
    advertise_address: Option<String>,
    #[serde(rename = "data-dir")]
    data_dir: Option<String>,
    motd: Option<String>,
    #[serde(rename = "min-diff")]
    min_diff: Option<u64>,
    #[serde(rename = "payout-address")]
    payout_address: Option<String>,
    #[serde(rename = "coinbase-tag")]
    coinbase_tag: Option<String>,
    #[serde(rename = "prime-id")]
    prime_id: Option<u32>,
    window: Option<u64>,
    #[serde(rename = "min-payout")]
    min_payout: Option<u64>,
    #[serde(rename = "fee-bps")]
    fee_bps: Option<u64>,
    #[serde(rename = "activation-height")]
    activation_height: Option<u64>,
    headline: Option<String>,
    rpc: Option<String>,
    #[serde(rename = "rpc-cookie")]
    rpc_cookie: Option<String>,
    poll: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub stats_listen: String,
    pub advertise: String,
    pub data_dir: PathBuf,
    pub key_path: PathBuf,
    pub motd: String,
    pub min_diff: u64,
    pub payout_address: String,
    pub coinbase_tag: String,
    pub prime_id: u32,
    pub window_multiple: u64,
    pub min_payout: u64,
    pub fee_bps: u64,
    pub activation_height: u64,
    pub headline: String,
    pub rpc: String,
    pub rpc_cookie: PathBuf,
    pub poll_secs: f64,
}

impl Config {
    pub fn load() -> Self {
        let cli = Cli::parse();
        let mut f = FileCfg::default();
        if let Some(p) = &cli.config {
            let raw = std::fs::read_to_string(p).expect("read config");
            f = toml::from_str(&raw).expect("parse config");
        }
        let data_dir = PathBuf::from(f.data_dir.unwrap_or_else(|| ".".into()));
        let key_path = data_dir.join("lazarus-prime.key");
        Config {
            listen: f.listen.unwrap_or_else(|| "0.0.0.0:28915".into()),
            stats_listen: f.stats_listen.unwrap_or_else(|| "127.0.0.1:28916".into()),
            advertise: f.advertise_address.unwrap_or_else(|| "stratum.awokenlazarus.xyz:28915".into()),
            data_dir,
            key_path,
            motd: f.motd.unwrap_or_else(|| "Lazarus".into()),
            min_diff: f.min_diff.unwrap_or(1),
            payout_address: f.payout_address.unwrap_or_default(),
            coinbase_tag: f.coinbase_tag.unwrap_or_else(|| "Lazarus".into()),
            prime_id: f.prime_id.unwrap_or(1),
            window_multiple: f.window.unwrap_or(8),
            min_payout: f.min_payout.unwrap_or(546),
            fee_bps: f.fee_bps.unwrap_or(0),
            activation_height: f.activation_height.unwrap_or(961640),
            headline: f.headline.unwrap_or_else(|| "Lazarus".into()),
            rpc: f.rpc.unwrap_or_else(|| "http://127.0.0.1:9332".into()),
            rpc_cookie: PathBuf::from(f.rpc_cookie.unwrap_or_else(|| {
                "/home/umbrel/umbrel/app-data/bitcoin-knots/data/bitcoin/.cookie".into()
            })),
            poll_secs: f.poll.unwrap_or(0.5),
        }
    }

    pub fn key_path(&self) -> &Path {
        &self.key_path
    }
}
