//! The configuration (spec §12 and the keys milestone 2 adds), the secrets file and the clock of the
//! exchange.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use v21_core::window::Coin;
use v21_live::shared::clock::{clock_pause, ExchangeClock};
use v21_live::shared::config::{Config, Mode};
use v21_live::shared::secrets::Secrets;

const EXAMPLE: &str = include_str!("../../../v21bot.example.toml");
const SECRETS_EXAMPLE: &str = include_str!("../../../secrets.example.toml");

fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

fn load(text: &str) -> anyhow::Result<Config> {
    let dir = tempfile::tempdir().unwrap();
    Config::load(&write(dir.path(), "v21bot.toml", text))
}

#[test]
fn example_config_loads_with_the_spec_defaults() {
    let cfg = load(EXAMPLE).unwrap();
    assert_eq!(cfg.mode, Mode::Demo);
    assert_eq!((cfg.stake_usd, cfg.demo_bank_usd), (20.0, 100.0));
    assert_eq!(cfg.strategy_dir, PathBuf::from("/root/15mbacktest"));
    let f = &cfg.feeds;
    assert_eq!((f.polymarket_copies, f.polymarket_open_s, f.polymarket_close_s, f.reorder_hold_ms, f.binance_warmup_s), (2, 600, 880, 500, 1000));
    assert_eq!(f.binance_ws, "wss://stream.binance.com:9443/stream");
    assert_eq!(f.binance_ws_fallback, "wss://data-stream.binance.vision/stream");
    assert_eq!((f.binance_rest.as_str(), f.binance_rest_fallback.as_str()), ("https://api.binance.com", "https://data-api.binance.vision"));
    assert_eq!((cfg.exec.warm_interval_s, cfg.exec.response_timeout_ms, cfg.exec.balance_poll_s, cfg.exec.taker_delay_ms), (5, 2000, 30, 250));
    let r = &cfg.risk;
    assert_eq!((r.feed_silent_ms, r.clock_max_offset_ms, r.max_restarts_10min, r.redeem_alarm_min), (2000, 50, 3, 60));
    assert_eq!((r.ledger_tolerance_usd, r.ntp_server.as_str()), (1.0, "time.google.com:123"));
    assert_eq!((cfg.cores.hot, cfg.cores.cold, cfg.cores.pin), (1, 0, true));
    assert_eq!(cfg.telegram.summary_utc, "07:30");
    assert_eq!(cfg.paths.journal, PathBuf::from("/var/lib/v21bot/journal.jsonl"));
    assert_eq!(cfg.paths.state, PathBuf::from("/var/lib/v21bot/state.json"));
    assert_eq!(cfg.paths.stop_flag, PathBuf::from("/var/lib/v21bot/STOP"));
    let u = &cfg.urls;
    assert_eq!((u.gamma.as_str(), u.clob.as_str()), ("https://gamma-api.polymarket.com", "https://clob.polymarket.com"));
    assert_eq!(u.polymarket_ws, "wss://ws-subscriptions-clob.polymarket.com/ws/market");
    assert_eq!((u.geoblock.as_str(), u.telegram_api.as_str()), ("https://polymarket.com/api/geoblock", "https://api.telegram.org"));
}

#[test]
fn an_empty_config_is_all_defaults() {
    // the example file writes out every default
    assert_eq!(load("").unwrap(), load(EXAMPLE).unwrap());
}

#[test]
fn stake_above_the_code_limit_is_refused() {
    let err = load("stake_usd = 20.01\n").unwrap_err();
    assert!(format!("{err:#}").contains("$20"), "{err:#}");
    assert!(load("stake_usd = 0.0\n").is_err());
    assert_eq!(load("stake_usd = 5.0\n").unwrap().stake_usd, 5.0);
}

#[test]
fn an_unknown_key_is_named() {
    for (text, key) in [("stake = 5.0\n", "stake"), ("[feeds]\npolymarket_copiez = 2\n", "polymarket_copiez"), ("[nope]\n", "nope")] {
        let err = load(text).unwrap_err();
        assert!(format!("{err:#}").contains(key), "{key}: {err:#}");
    }
}

#[test]
fn the_strategy_comes_from_strategy_dir() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["strategy_v2.json", "strategy_v2_1.json"] {
        std::fs::copy(format!("{}/../core/tests/data/{name}", env!("CARGO_MANIFEST_DIR")), dir.path().join(name)).unwrap();
    }
    let text = format!("strategy_dir = {:?}\n", dir.path().to_str().unwrap());
    let strategy = load(&text).unwrap().strategy().unwrap();
    assert_eq!(strategy.v21.trade_coins, vec![Coin::Btc, Coin::Eth]);
    // without the files the error names the path
    let err = load("strategy_dir = \"Z:/no/such/dir\"\n").unwrap().strategy().unwrap_err();
    assert!(format!("{err:#}").contains("strategy_v2.json"), "{err:#}");
}

#[test]
fn secrets_debug_hides_values_and_errors_quote_no_line() {
    let s: Secrets = "telegram_token = \"TOKEN-VALUE\"\ntelegram_chat_id = \"CHAT-VALUE\"\nsigner_key = \"0xSIGNER\"\n".parse().unwrap();
    assert_eq!((s.telegram_token.as_deref(), s.telegram_chat_id.as_deref()), (Some("TOKEN-VALUE"), Some("CHAT-VALUE")));
    let shown = format!("{s:?}");
    assert!(!shown.contains("TOKEN-VALUE") && !shown.contains("CHAT-VALUE") && shown.contains("***"), "{shown}");
    // a broken line is named by its number, never quoted
    let err = "\ntelegram_token = \"SECRET-IN-A-BROKEN-LINE\n".parse::<Secrets>().unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("line 2") && !msg.contains("SECRET-IN-A-BROKEN-LINE"), "{msg}");
    // a value of the wrong kind and an unknown key are named without their values
    let msg = format!("{:#}", "telegram_token = 12345\n".parse::<Secrets>().unwrap_err());
    assert!(msg.contains("telegram_token") && !msg.contains("12345"), "{msg}");
    let msg = format!("{:#}", "telegram_tokn = \"X-VALUE\"\n".parse::<Secrets>().unwrap_err());
    assert!(msg.contains("telegram_tokn") && !msg.contains("X-VALUE"), "{msg}");
}

#[test]
fn a_chat_id_may_be_a_number_and_empty_values_are_none() {
    let s: Secrets = "telegram_token = \"\"\ntelegram_chat_id = -1001234\n".parse().unwrap();
    assert_eq!((s.telegram_token.as_deref(), s.telegram_chat_id.as_deref()), (None, Some("-1001234")));
    let s: Secrets = SECRETS_EXAMPLE.parse().unwrap();
    assert_eq!((s.telegram_token.as_deref(), s.telegram_chat_id.as_deref()), (None, None));
}

#[test]
fn secrets_load_names_a_missing_file() {
    let err = Secrets::load(Path::new("Z:/no/such/secrets.toml")).unwrap_err();
    assert!(format!("{err:#}").contains("secrets.toml"), "{err:#}");
}

#[cfg(unix)]
#[test]
fn a_secrets_file_open_to_others_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "secrets.toml", "telegram_token = \"T\"\n");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(format!("{:#}", Secrets::load(&path).unwrap_err()).contains("chmod 600"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Secrets::load(&path).is_ok());
}

fn local_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

#[test]
fn exchange_clock_adds_the_offset() {
    let clock = ExchangeClock::new();
    assert_eq!(clock.offset_ms(), None);
    clock.set_offset_ms(-1042);
    let before = local_ms();
    let now = clock.now_ms();
    let after = local_ms();
    assert!(before - 1042 <= now && now <= after - 1042, "{before} {now} {after}");
    assert_eq!(clock.offset_ms(), Some(-1042));
    assert!((clock.now_us() / 1000 - clock.now_ms()).abs() <= 5);
}

#[test]
fn clock_pause_rules() {
    assert_eq!(clock_pause(None, 50), Some("пауза: часы не измерены"));
    assert_eq!(clock_pause(Some(51), 50), Some("пауза: часы сбиты"));
    assert_eq!(clock_pause(Some(-51), 50), Some("пауза: часы сбиты"));
    assert_eq!(clock_pause(Some(-50), 50), None);
    assert_eq!(clock_pause(Some(0), 50), None);
}
