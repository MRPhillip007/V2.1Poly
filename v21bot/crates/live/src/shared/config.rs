//! The configuration file `v21bot.toml`: spec §12 and the keys milestone 2 adds. Every key has a
//! default (the values of `v21bot.example.toml`); a key the bot does not know is an error that names
//! it.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use v21_core::params::{load_strategy, Strategy};

/// The most a stake can be, whatever the config says (spec §7.2).
pub const MAX_STAKE_USD: f64 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// No orders: the demo account trades on the demo fills.
    Demo,
    /// Orders as well (milestone 3).
    Live,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub mode: Mode,
    /// Dollars before the fee per order; at most `MAX_STAKE_USD`.
    pub stake_usd: f64,
    /// The starting cash of the demo account.
    pub demo_bank_usd: f64,
    /// The folder of `strategy_v2.json` and `strategy_v2_1.json`.
    pub strategy_dir: PathBuf,
    pub feeds: Feeds,
    pub exec: Exec,
    pub risk: Risk,
    pub cores: Cores,
    pub telegram: TelegramCfg,
    pub paths: Paths,
    pub urls: Urls,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Feeds {
    pub polymarket_copies: u8,
    /// The second of the window the copies connect at, and the one they close at.
    pub polymarket_open_s: i64,
    pub polymarket_close_s: i64,
    /// How long the exact engine holds events to put them in the exchange's time order.
    pub reorder_hold_ms: i64,
    pub binance_ws: String,
    pub binance_ws_fallback: String,
    pub binance_rest: String,
    pub binance_rest_fallback: String,
    /// Seconds of klines read at the start.
    pub binance_warmup_s: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Exec {
    /// How often the path to the exchange is measured.
    pub warm_interval_s: u64,
    pub response_timeout_ms: u64,
    pub balance_poll_s: u64,
    /// The taker delay of the demo until live orders measure it (spec §6.2).
    pub taker_delay_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Risk {
    /// A feed silent this long in 835–870 s of the window pauses the shots (spec §7.4).
    pub feed_silent_ms: i64,
    /// A clock offset beyond this pauses the shots (spec §7.4).
    pub clock_max_offset_ms: i64,
    /// `host:port` of the NTP server the clock offset is measured against; empty: Binance only.
    pub ntp_server: String,
    pub ledger_tolerance_usd: f64,
    pub max_restarts_10min: u32,
    pub redeem_alarm_min: u32,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cores {
    pub hot: usize,
    pub cold: usize,
    /// Pin the threads to these cores (off in tests).
    pub pin: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelegramCfg {
    /// The UTC time of the day's summary, "HH:MM".
    pub summary_utc: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// The journal; the CSV tables are written next to it.
    pub journal: PathBuf,
    pub state: PathBuf,
    pub stop_flag: PathBuf,
}

/// The addresses the bot talks to; the tests point them at local fake servers.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Urls {
    pub gamma: String,
    pub clob: String,
    pub polymarket_ws: String,
    pub geoblock: String,
    pub telegram_api: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Demo,
            stake_usd: 20.0,
            demo_bank_usd: 100.0,
            strategy_dir: PathBuf::from("/root/15mbacktest"),
            feeds: Feeds::default(),
            exec: Exec::default(),
            risk: Risk::default(),
            cores: Cores::default(),
            telegram: TelegramCfg::default(),
            paths: Paths::default(),
            urls: Urls::default(),
        }
    }
}

impl Default for Feeds {
    fn default() -> Self {
        Feeds {
            polymarket_copies: 2,
            polymarket_open_s: 600,
            polymarket_close_s: 880,
            reorder_hold_ms: 500,
            binance_ws: "wss://stream.binance.com:9443/stream".into(),
            binance_ws_fallback: "wss://data-stream.binance.vision/stream".into(),
            binance_rest: "https://api.binance.com".into(),
            binance_rest_fallback: "https://data-api.binance.vision".into(),
            binance_warmup_s: 1000,
        }
    }
}

impl Default for Exec {
    fn default() -> Self {
        Exec { warm_interval_s: 5, response_timeout_ms: 2000, balance_poll_s: 30, taker_delay_ms: 250 }
    }
}

impl Default for Risk {
    fn default() -> Self {
        Risk {
            feed_silent_ms: 2000,
            clock_max_offset_ms: 50,
            ntp_server: "time.google.com:123".into(),
            ledger_tolerance_usd: 1.0,
            max_restarts_10min: 3,
            redeem_alarm_min: 60,
        }
    }
}

impl Default for Cores {
    fn default() -> Self {
        Cores { hot: 1, cold: 0, pin: true }
    }
}

impl Default for TelegramCfg {
    fn default() -> Self {
        TelegramCfg { summary_utc: "07:30".into() }
    }
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            journal: PathBuf::from("/var/lib/v21bot/journal.jsonl"),
            state: PathBuf::from("/var/lib/v21bot/state.json"),
            stop_flag: PathBuf::from("/var/lib/v21bot/STOP"),
        }
    }
}

impl Default for Urls {
    fn default() -> Self {
        Urls {
            gamma: "https://gamma-api.polymarket.com".into(),
            clob: "https://clob.polymarket.com".into(),
            polymarket_ws: "wss://ws-subscriptions-clob.polymarket.com/ws/market".into(),
            geoblock: "https://polymarket.com/api/geoblock".into(),
            telegram_api: "https://api.telegram.org".into(),
        }
    }
}

impl Config {
    /// Reads and checks the config file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| anyhow!("{}: {e}", path.display()))?;
        cfg.check().with_context(|| format!("{}", path.display()))?;
        Ok(cfg)
    }

    fn check(&self) -> Result<()> {
        if !(self.stake_usd > 0.0 && self.stake_usd <= MAX_STAKE_USD) {
            bail!("stake_usd = {}: нужна ставка больше 0 и не выше предела в коде ($20)", self.stake_usd);
        }
        if self.feeds.polymarket_copies == 0 {
            bail!("feeds.polymarket_copies = 0: нужна хотя бы одна копия ленты");
        }
        Ok(())
    }

    /// The frozen strategy files of `strategy_dir`, with the hash check of milestone 1.
    pub fn strategy(&self) -> Result<Strategy> {
        let read = |name: &str| {
            let path = self.strategy_dir.join(name);
            std::fs::read_to_string(&path).with_context(|| format!("{}: cannot read", path.display()))
        };
        load_strategy(&read("strategy_v2.json")?, &read("strategy_v2_1.json")?).map_err(|e| anyhow!("the strategy files: {e}"))
    }
}
