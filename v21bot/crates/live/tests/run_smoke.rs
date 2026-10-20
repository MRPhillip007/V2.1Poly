//! `run` starts the threads and stops them when asked, even when no server answers: the clock is then
//! not measured, which pauses the shots and raises an alarm; the state and the journal are saved.

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use v21_live::cold::journal::Journal;
use v21_live::shared::config::Config;
use v21_live::shared::secrets::Secrets;

/// An address that takes every connection and drops it at once: every request fails fast.
fn dropping_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            drop(sock);
        }
    });
    addr
}

#[test]
fn run_starts_and_stops_with_no_server_answering() {
    let dir = tempfile::tempdir().unwrap();
    let addr = dropping_server();
    let strategy_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/data").replace('\\', "/");
    let out = dir.path().to_string_lossy().replace('\\', "/");
    let config = format!(
        r#"
strategy_dir = "{strategy_dir}"
[feeds]
binance_ws = "ws://{addr}/stream"
binance_ws_fallback = "ws://{addr}/stream"
binance_rest = "http://{addr}"
binance_rest_fallback = "http://{addr}"
binance_warmup_s = 60
[risk]
ntp_server = ""
[cores]
pin = false
[paths]
journal = "{out}/journal.jsonl"
state = "{out}/state.json"
stop_flag = "{out}/stop"
[urls]
gamma = "http://{addr}"
clob = "http://{addr}"
polymarket_ws = "ws://{addr}/ws/market"
geoblock = "http://{addr}/api/geoblock"
telegram_api = "http://{addr}"
"#
    );
    let path = dir.path().join("v21bot.toml");
    std::fs::write(&path, config).unwrap();
    let cfg = Config::load(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let bot = std::thread::spawn(move || v21_live::run::run(cfg, Secrets { telegram_token: None, telegram_chat_id: None }, flag));
    std::thread::sleep(Duration::from_millis(1500));
    stop.store(true, Ordering::Relaxed);
    bot.join().unwrap().unwrap();

    let journal = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let alarms: Vec<String> = journal.records("alarm").unwrap().iter().filter_map(|r| r["text"].as_str().map(str::to_string)).collect();
    assert!(alarms.iter().any(|a| a == "пауза: часы не измерены"), "{alarms:?}");
    assert!(!journal.records("error").unwrap().is_empty(), "the warm-up of Binance failed, and said so");
    assert!(dir.path().join("state.json").is_file());
    assert!(dir.path().join("trades.csv").is_file());
}
