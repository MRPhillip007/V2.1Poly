//! The stand of the end-to-end scenarios (task 18 of milestone 2): fake Polymarket (the scripted market
//! channel), Binance (primary and mirror: the stream, the klines, the time), Gamma, the CLOB and
//! Telegram. Binance's time is shifted so that the bot sees the exchange's clock at s + 830 s of the
//! window s = 1 790 910 900 (03:15 UTC, in the hours of v2.1) — every scenario runs on a clock far from
//! this machine's. The bot runs `run` on a thread of its own, with a config of the stand.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};
use v21_live::cold::journal::Journal;
use v21_live::shared::clock::local_ms;
use v21_live::shared::config::Config;
use v21_live::shared::secrets::Secrets;

use super::fake_binance::FakeBinance;
use super::fake_clob::FakeClob;
use super::fake_gamma::FakeGamma;
use super::fake_market::{Asks, Cut, FakeMarket};
use super::fake_telegram::FakeTelegram;

/// 2026-10-02 03:15 UTC.
pub const S: i64 = 1_790_910_900;
pub const S0: i64 = S * 1000;
/// The tokens: BTC Up/Down, ETH Up/Down, SOL Up/Down. BTC's Up id is the smallest, then ETH's.
pub const TOKENS: [&str; 6] = ["1001", "1002", "2001", "2002", "3001", "3002"];
/// The Up tokens' numbers.
pub const BTC: usize = 0;
pub const ETH: usize = 2;
pub const SOL: usize = 4;
/// A scenario that has not got what it waits for by then fails.
const LIMIT_MS: i64 = 120_000;

pub fn asks(token: usize, at_ms: i64, levels: &[(f64, f64)]) -> Asks {
    Asks { at_ms, token, levels: levels.to_vec() }
}

/// What a scenario changes in the stand.
pub struct Setup {
    pub script: Vec<Asks>,
    pub cuts: Vec<Cut>,
    pub bank_usd: f64,
    /// The primary Binance answers 451 to everything.
    pub primary_451: bool,
    /// What an Up contract of BTC, ETH and SOL pays.
    pub payout_up: [f64; 3],
    /// Both Binance streams are down in these ms of the window.
    pub binance_cut: Option<(i64, i64)>,
}

impl Default for Setup {
    fn default() -> Self {
        Setup { script: vec![], cuts: vec![], bank_usd: 100.0, primary_451: false, payout_up: [1.0; 3], binance_cut: None }
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn condition(coin: usize) -> String {
    format!("0x{coin:064}")
}

/// The window's three markets as Gamma has them after the outcome (its rules are the real ones).
fn events(payout_up: [f64; 3]) -> Vec<Value> {
    ["btc", "eth", "sol"]
        .iter()
        .enumerate()
        .map(|(k, coin)| {
            let mut e = fixture("gamma_closed.json")[0].clone();
            e["slug"] = json!(format!("{coin}-updown-15m-{S}"));
            let m = &mut e["markets"][0];
            m["conditionId"] = json!(condition(k));
            m["clobTokenIds"] = json!(serde_json::to_string(&[TOKENS[2 * k], TOKENS[2 * k + 1]]).unwrap());
            m["outcomePrices"] = json!(serde_json::to_string(&[payout_up[k].to_string(), (1.0 - payout_up[k]).to_string()]).unwrap());
            e
        })
        .collect()
}

pub struct Stand {
    pub dir: tempfile::TempDir,
    pub shift_ms: i64,
    pub market: FakeMarket,
    pub primary: FakeBinance,
    pub mirror: FakeBinance,
    pub gamma: FakeGamma,
    pub clob: FakeClob,
    pub tg: FakeTelegram,
    cfg: Config,
    began_ms: i64,
}

impl Stand {
    pub async fn start(setup: Setup) -> Stand {
        let shift_ms = (S + 830) * 1000 - local_ms();
        // every Up token out of the band (ask 0.95), its bid low so that Down is out of it too
        let books = (0..6).map(|k| (vec![(0.04, 100.0)], vec![(if k % 2 == 0 { 0.95 } else { 0.96 }, 100.0)])).collect();
        let market = FakeMarket::start(S, shift_ms, TOKENS.iter().map(|t| t.to_string()).collect(), books, setup.script, setup.cuts).await;
        let primary = FakeBinance::start(shift_ms, setup.primary_451, 100).await;
        let mirror = FakeBinance::start(shift_ms, false, 100).await;
        if let Some((from, until)) = setup.binance_cut {
            primary.cut(S0 + from, S0 + until);
            mirror.cut(S0 + from, S0 + until);
        }
        let gamma = FakeGamma::start(events(setup.payout_up)).await;
        let clob = FakeClob::start().await;
        for k in 0..3 {
            clob.set_market(&condition(k), fixture("clob_market.json"));
        }
        let tg = FakeTelegram::start().await;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config { demo_bank_usd: setup.bank_usd, strategy_dir: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/data")), ..Config::default() };
        cfg.feeds.binance_ws = primary.ws.clone();
        cfg.feeds.binance_ws_fallback = mirror.ws.clone();
        cfg.feeds.binance_rest = primary.rest.clone();
        cfg.feeds.binance_rest_fallback = mirror.rest.clone();
        cfg.risk.ntp_server = String::new();
        cfg.risk.clock_max_offset_ms = 10_000_000_000_000;
        cfg.cores.pin = false;
        cfg.paths.journal = dir.path().join("journal.jsonl");
        cfg.paths.state = dir.path().join("state.json");
        cfg.paths.stop_flag = dir.path().join("stop");
        cfg.urls.gamma = gamma.url.clone();
        cfg.urls.clob = clob.url.clone();
        cfg.urls.polymarket_ws = market.ws.clone();
        cfg.urls.telegram_api = tg.url.clone();
        Stand { dir, shift_ms, market, primary, mirror, gamma, clob, tg, cfg, began_ms: local_ms() }
    }

    /// The exchange's clock, ms into the window.
    pub fn window_ms(&self) -> i64 {
        local_ms() + self.shift_ms - S0
    }

    /// The bot, running until `stop` or until it is dropped.
    pub fn bot(&self) -> Bot {
        let stop = Arc::new(AtomicBool::new(false));
        let (cfg, flag) = (self.cfg.clone(), stop.clone());
        let secrets = Secrets { telegram_token: Some("1:e2e".into()), telegram_chat_id: Some("42".into()) };
        let thread = std::thread::spawn(move || v21_live::run::run(cfg, secrets, flag));
        Bot { stop, thread: Some(thread) }
    }

    pub fn journal(&self) -> Vec<Value> {
        Journal::open(&self.cfg.paths.journal).and_then(|j| j.all()).unwrap_or_default()
    }

    /// Waits until `ok` holds for the journal's records; a scenario that does not get there in its
    /// 120 s fails, with the journal shown.
    pub async fn wait_for(&self, what: &str, ok: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        loop {
            let recs = self.journal();
            if ok(&recs) {
                return recs;
            }
            if local_ms() - self.began_ms > LIMIT_MS {
                let shown: Vec<String> = recs.iter().map(|r| r.to_string()).collect();
                panic!("waited in vain for {what}; the journal:\n{}", shown.join("\n"));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Sleeps until the exchange's clock is `ms` into the window.
    pub async fn until(&self, ms: i64) {
        let wait = ms - self.window_ms();
        if wait > 0 {
            tokio::time::sleep(Duration::from_millis(wait as u64)).await;
        }
    }

    /// The journal holds no error.
    pub fn assert_no_errors(&self) {
        let errors: Vec<String> = of(&self.journal(), "error").iter().map(|r| r.to_string()).collect();
        assert!(errors.is_empty(), "errors in the journal:
{}", errors.join("
"));
    }

    /// A table the bot wrote next to its journal: (header, rows).
    pub fn table(&self, name: &str) -> (Vec<String>, Vec<Vec<String>>) {
        let raw = std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default();
        let mut lines = raw.trim_start_matches('\u{feff}').lines().map(|l| l.split(',').map(str::to_string).collect::<Vec<_>>());
        let header = lines.next().unwrap_or_default();
        (header, lines.collect())
    }
}

/// The bot of a scenario.
pub struct Bot {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<anyhow::Result<()>>>,
}

impl Bot {
    /// Stops the bot and waits for it: what `run` returned.
    pub fn stop(mut self) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().map_or(Ok(()), |t| t.join().expect("the bot's thread"))
    }
}

impl Drop for Bot {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The records of `kind`.
pub fn of<'a>(recs: &'a [Value], kind: &str) -> Vec<&'a Value> {
    recs.iter().filter(|r| r["kind"] == kind).collect()
}
