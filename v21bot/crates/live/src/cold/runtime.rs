//! The cold runtime (spec §4.2): the clock, the path to the exchange, Binance over REST, the markets of
//! the windows, the exact engine, the demo account, the journal, the tables and Telegram.
//!
//! One loop on a 10-ms tick drains the hot thread's queue and does what is due; every request runs as a
//! task of its own and comes back as a `Done`, so that nothing waits for the network. The schedule of a
//! window is that of `paper/run_paper.py` (the Global Constraints of milestone 2): Gamma from the 540th
//! second, again every 20 s until the 700th; the outcomes from the 930th second every 30 s, every
//! 5 minutes after 2 hours; a window without positions stops waiting after 6 hours.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rtrb::{Consumer, Producer};
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use v21_core::params::Strategy;
use v21_core::window::{Coin, Side};

use crate::cold::binance_rest;
use crate::cold::clob_info::{fetch_book, fetch_market_info, measure_rtt, rules_note, MarketInfo, Rtt};
use crate::cold::csv::rebuild_tables;
use crate::cold::demo::{DemoDesk, DeskOut, STOPPED};
use crate::cold::exact::{DemoTiming, ExactEngine, ExactOut, ExactSignal, ShotFill};
use crate::cold::gamma::{fetch_outcomes, fetch_window, Market, Outcome};
use crate::cold::journal::{load_state, save_state, Journal, State, WindowWait};
use crate::cold::ntp::{clock_offset_ms, sntp_offset_ms};
use crate::cold::records::{
    bbo_records, day_summary, demo_record, entry_record, settle_record, shot_record, signal_record, window_record, window_result_record, FastSeen, WindowNotes,
};
use crate::cold::telegram::{fmt_alarm, fmt_entry, fmt_missed, fmt_result, fmt_summary, Telegram};
use crate::health::{health_record, CpuMeter};
use crate::hot::binance::backfill_rows;
use crate::hot::engine::{BN_SILENT, PM_SILENT};
use crate::hot::timer::{window_plan, WindowPlan};
use crate::shared::clock::{clock_pause, ExchangeClock};
use crate::shared::config::Config;
use crate::shared::msgs::{token_key, CoinArm, ColdMsg, FastOut, FastReason, HotMsg, WindowArm};
use crate::shared::secrets::Secrets;
use crate::shared::utc::fmt_utc;

const TICK: Duration = Duration::from_millis(10);
const CLOCK_EVERY_MS: i64 = 600_000;
/// Gamma is asked again this often until this second of the window.
const GAMMA_RETRY_MS: i64 = 20_000;
const GAMMA_UNTIL_S: i64 = 700;
const OUTCOMES_FROM_S: i64 = 930;
const OUTCOMES_EVERY_MS: i64 = 30_000;
const OUTCOMES_SLOW_AFTER_MS: i64 = 2 * 3_600_000;
const OUTCOMES_SLOW_EVERY_MS: i64 = 300_000;
const OUTCOMES_GIVE_UP_MS: i64 = 6 * 3_600_000;
/// The exact engine advances this often in 835–900 s of its window, else once a second.
const EXACT_FAST_MS: i64 = 50;
const EXACT_SLOW_MS: i64 = 1_000;
/// Both copies down this long: an alarm.
const COPIES_ALARM_MS: i64 = 10_000;
/// Half the round trip to the exchange before the first measurement.
const HALF_RTT_MS: f64 = 6.0;
/// A window that has not finished by this second of it is left (the exact engine decides by 1 020 s).
const WINDOW_GIVE_UP_S: i64 = 1_200;
/// Attempts of a backfill of Binance seconds, 2 s apart.
const BACKFILL_TRIES: u32 = 5;
/// The most messages of the hot thread taken in one tick.
const DRAIN: usize = 100_000;
/// An NTP measurement waits this long at most.
const NTP_WAIT: Duration = Duration::from_secs(4);
/// A fast decision belongs to the exact check of its coin whose t_trig is this close.
const PAIR_S: f64 = 0.3;

/// A book over REST: its time, bids and asks.
type RestBook = (i64, Vec<(f64, f64)>, Vec<(f64, f64)>);

/// What a request brought back.
enum Done {
    Clock(Option<i64>),
    Rtt(Option<f64>),
    Markets { s: i64, got: Result<Vec<(Market, Option<MarketInfo>)>> },
    Book { s: i64, token: u8, got: Result<RestBook> },
    Klines { coin: Coin, q0: i64, q1: i64, tries: u32, got: Result<Vec<(i64, f64)>> },
    Outcomes { s: i64, got: Result<Vec<(Coin, Outcome)>> },
}

/// The window being watched.
struct Win {
    plan: WindowPlan,
    arm: WindowArm,
    notes: WindowNotes,
    /// The fast decisions (shots and skips) and the waits before them: (coin, t_trig, when).
    fast: Vec<FastSeen>,
    waits: Vec<(Coin, f64, i64)>,
    n_trades: u32,
    /// Both copies of the feed are down since then.
    copies_lost_at: Option<i64>,
    copies_alarmed: bool,
    /// The feed pauses already alarmed in this window.
    paused: Vec<&'static str>,
    done: bool,
}

struct Cold {
    cfg: Config,
    strategy: Strategy,
    http: reqwest::Client,
    clock: Arc<ExchangeClock>,
    from_hot: Consumer<HotMsg>,
    to_hot: Producer<ColdMsg>,
    journal: Journal,
    tables: PathBuf,
    state: State,
    desk: DemoDesk,
    exact: ExactEngine,
    tg: Arc<Telegram>,
    tx: UnboundedSender<Done>,
    rx: UnboundedReceiver<Done>,
    rtt: Rtt,
    clock_at_ms: i64,
    clock_busy: bool,
    rtt_at_ms: i64,
    rtt_busy: bool,
    /// The pause last sent to the hot thread (`Some(None)` once sent).
    pause_sent: Option<Option<&'static str>>,
    plan: Option<WindowPlan>,
    last_s: Option<i64>,
    gamma_at_ms: i64,
    gamma_busy: bool,
    win: Option<Win>,
    /// The windows awaiting outcomes: when to ask next.
    outcomes_at: BTreeMap<i64, i64>,
    exact_at_ms: i64,
    binance_fallback: bool,
    cpu: CpuMeter,
}

/// Runs the cold side until `stop` is set.
pub async fn run_cold(
    cfg: Config,
    strategy: Strategy,
    secrets: Secrets,
    from_hot: Consumer<HotMsg>,
    to_hot: Producer<ColdMsg>,
    clock: Arc<ExchangeClock>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut cold = Cold::open(cfg, strategy, &secrets, from_hot, to_hot, clock)?;
    drop(secrets);
    cold.start().await;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !stop.load(Ordering::Relaxed) {
        tokio::select! {
            _ = tick.tick() => cold.step(),
            Some(done) = cold.rx.recv() => cold.on_done(done),
        }
    }
    cold.finish()
}

impl Cold {
    fn open(cfg: Config, strategy: Strategy, secrets: &Secrets, from_hot: Consumer<HotMsg>, to_hot: Producer<ColdMsg>, clock: Arc<ExchangeClock>) -> Result<Cold> {
        let journal = Journal::open(&cfg.paths.journal)?;
        let tables = cfg.paths.journal.parent().map(PathBuf::from).unwrap_or_default();
        let state = load_state(&cfg.paths.state, &journal, cfg.demo_bank_usd).context("the restart state")?;
        let desk = DemoDesk::new(state.account.clone(), cfg.stake_usd);
        let timing = DemoTiming { stake_usd: cfg.stake_usd, limit: strategy.v21.max_fill, half_rtt_ms: HALF_RTT_MS, taker_delay_ms: cfg.exec.taker_delay_ms as f64 };
        let exact = ExactEngine::new(strategy.clone(), cfg.feeds.reorder_hold_ms, timing);
        let tg = Arc::new(Telegram::new(secrets, &cfg.urls.telegram_api));
        let (tx, rx) = unbounded_channel();
        Ok(Cold {
            http: crate::cold::http_client(),
            cfg,
            strategy,
            clock,
            from_hot,
            to_hot,
            journal,
            tables,
            state,
            desk,
            exact,
            tg,
            tx,
            rx,
            rtt: Rtt::default(),
            clock_at_ms: 0,
            clock_busy: false,
            rtt_at_ms: 0,
            rtt_busy: false,
            pause_sent: None,
            plan: None,
            last_s: None,
            gamma_at_ms: 0,
            gamma_busy: false,
            win: None,
            outcomes_at: BTreeMap::new(),
            exact_at_ms: 0,
            binance_fallback: false,
            cpu: CpuMeter::default(),
        })
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    fn rest_bases(&self) -> Vec<String> {
        vec![self.cfg.feeds.binance_rest.clone(), self.cfg.feeds.binance_rest_fallback.clone()]
    }

    /// The first measurement of the clock and the warm-up of the closes, before any window.
    async fn start(&mut self) {
        let start = json!({
            "mode": "demo",
            "stake_usd": self.cfg.stake_usd,
            "demo_bank_usd": self.cfg.demo_bank_usd,
            "taker_delay_ms": self.cfg.exec.taker_delay_ms,
            "version": env!("CARGO_PKG_VERSION"),
        });
        self.append("start", start);
        let offset = measure_clock(self.http.clone(), self.cfg.risk.ntp_server.clone(), self.rest_bases()).await;
        self.on_clock(offset);
        let now_s = self.now().div_euclid(1000);
        let coins = self.strategy.v2.watch_coins.clone();
        let bases = self.rest_bases();
        let from = now_s - self.cfg.feeds.binance_warmup_s;
        let warm = coins.iter().map(|&c| binance_rest::klines(&self.http, &bases, c.binance_symbol(), from, now_s - 1));
        let rows = futures_util::future::join_all(warm).await;
        for (&coin, got) in coins.iter().zip(rows) {
            match got {
                Ok(rows) => self.give_klines(coin, rows),
                Err(e) => self.error("binance warm-up", None, &format!("{}: {e:#}", coin.as_str())),
            }
        }
        // the windows a restart left waiting, and the positions of windows not yet listed
        let mut waiting: Vec<i64> = self.state.unsettled.keys().copied().collect();
        for p in &self.desk.account().positions {
            self.state.unsettled.entry(p.s).or_insert_with(|| WindowWait { coins: vec![p.coin], done: vec![] });
            waiting.push(p.s);
        }
        for s in waiting {
            self.outcomes_at.insert(s, (s + OUTCOMES_FROM_S) * 1000);
        }
        if self.desk.pause() == Some(STOPPED) {
            self.alarm("стоп демо-счёта: денег меньше ставки и резерва, открытых позиций нет");
        }
        self.update_pause();
    }

    /// One tick of the loop.
    fn step(&mut self) {
        let now = self.now();
        self.drain(now);
        self.schedule(now);
        if now >= self.exact_at_ms {
            let mut out = vec![];
            self.exact.advance(now, &mut out);
            for o in out {
                self.on_exact(o, now);
            }
            let fast = self.win.as_ref().is_some_and(|w| (w.plan.s * 1000 + 835_000..=w.plan.s * 1000 + 900_000).contains(&now));
            self.exact_at_ms = now + if fast { EXACT_FAST_MS } else { EXACT_SLOW_MS };
        }
        if let Some(w) = &mut self.win
            && let Some(at) = w.copies_lost_at
            && !w.copies_alarmed
            && now - at > COPIES_ALARM_MS
        {
            w.copies_alarmed = true;
            self.alarm("обе копии ленты Polymarket недоступны дольше 10 с");
        }
    }

    /// The hot thread's messages.
    fn drain(&mut self, now: i64) {
        for _ in 0..DRAIN {
            let Ok(msg) = self.from_hot.pop() else { break };
            match msg {
                HotMsg::Feed { rx_ms, item } => self.exact.on_item(rx_ms, item),
                HotMsg::Trade(t) => self.exact.on_trade(&t),
                HotMsg::Gap { coin, q0, q1 } => {
                    self.exact.on_gap(coin, q0, q1);
                    self.backfill(coin, q0, q1, 0);
                }
                HotMsg::Fast(out) => self.on_fast(out, now),
                HotMsg::Copy { .. } => {}
                HotMsg::Coverage { lost, at_ms } => {
                    if lost {
                        self.exact.on_coverage_lost(at_ms);
                    }
                    if let Some(w) = &mut self.win {
                        w.copies_lost_at = lost.then_some(now);
                    }
                }
                HotMsg::Binance { up, fallback } => {
                    self.binance_fallback = up && fallback;
                    if self.binance_fallback
                        && let Some(w) = &mut self.win
                    {
                        w.notes.binance_fallback = true;
                    }
                }
                HotMsg::Health(h) => {
                    let rec = health_record(&h, self.clock.offset_ms(), self.rtt.median_ms(), self.cpu.sample());
                    self.append("health", rec);
                    if h.dropped_msgs > 0 {
                        self.alarm(&format!("очередь горячего потока потеряла {} сообщений за минуту", h.dropped_msgs));
                    }
                }
            }
        }
    }

    fn on_fast(&mut self, out: FastOut, now: i64) {
        let Some(w) = &mut self.win else { return };
        match out {
            FastOut::Wait { coin, t_trig, .. } => w.waits.push((coin, t_trig, now)),
            FastOut::Shot(_) | FastOut::Skip(_) => {
                let (coin, t_trig) = match out {
                    FastOut::Shot(s) => (s.coin, s.t_trig),
                    FastOut::Skip(k) => (k.coin, k.t_trig),
                    FastOut::Wait { .. } => unreachable!("matched above"),
                };
                let wait_ms = w.waits.iter().rev().find(|(c, t, _)| *c == coin && *t == t_trig).map(|(_, _, at)| now - at);
                w.fast.push(FastSeen { out, wait_ms });
                let silent = match out {
                    FastOut::Skip(k) => match k.reason {
                        FastReason::Paused(p) if (p == PM_SILENT || p == BN_SILENT) && !w.paused.contains(&p) => Some(p),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(p) = silent {
                    w.paused.push(p);
                    self.alarm(&format!("{p} в решающую минуту"));
                }
                if let FastOut::Shot(shot) = out {
                    self.exact.on_shot(&shot);
                    self.append("shot", shot_record(&shot));
                }
            }
        }
    }

    fn on_exact(&mut self, o: ExactOut, now: i64) {
        match o {
            ExactOut::Signal(sig) => self.on_signal(&sig),
            ExactOut::ShotFill(f) => self.on_shot_fill(&f),
            ExactOut::Reload { token } => {
                let Some(w) = &self.win else { return };
                let Some(id) = w.arm.tokens.get(usize::from(token)).cloned() else { return };
                let (http, clob, tx, s) = (self.http.clone(), self.cfg.urls.clob.clone(), self.tx.clone(), w.plan.s);
                tokio::spawn(async move {
                    let got = fetch_book(&http, &clob, &id).await;
                    let _ = tx.send(Done::Book { s, token, got });
                });
            }
            ExactOut::WindowDone(summary) => {
                let Some(w) = &mut self.win else { return };
                w.done = true;
                let rec = window_record(&summary, &w.notes, w.n_trades);
                let coins = w.arm.coins.iter().map(|c| c.coin).collect();
                let s = summary.s;
                self.append("window", rec);
                for r in bbo_records(&summary) {
                    self.append("bbo", r);
                }
                self.state.unsettled.insert(s, WindowWait { coins, done: vec![] });
                self.outcomes_at.insert(s, (s + OUTCOMES_FROM_S) * 1000);
                self.save();
                self.tables();
                self.plan = Some(window_plan(now, Some(s), self.cfg.feeds.polymarket_open_s, self.cfg.feeds.polymarket_close_s));
            }
        }
    }

    fn on_signal(&mut self, sig: &ExactSignal) {
        let fast = self.win.as_ref().and_then(|w| {
            w.fast
                .iter()
                .filter(|f| {
                    let (coin, t) = match f.out {
                        FastOut::Shot(s) => (s.coin, s.t_trig),
                        FastOut::Skip(k) => (k.coin, k.t_trig),
                        FastOut::Wait { coin, t_trig, .. } => (coin, t_trig),
                    };
                    coin == sig.check.coin && (t - sig.check.t_trig).abs() <= PAIR_S + 1e-9
                })
                .min_by(|a, b| {
                    let t = |f: &FastSeen| match f.out {
                        FastOut::Shot(s) => s.t_trig,
                        FastOut::Skip(k) => k.t_trig,
                        FastOut::Wait { t_trig, .. } => t_trig,
                    };
                    (t(a) - sig.check.t_trig).abs().total_cmp(&(t(b) - sig.check.t_trig).abs())
                })
                .copied()
        });
        self.append("signal", signal_record(sig, fast.as_ref(), &self.strategy));
        if sig.v21.trade && sig.demo.as_ref().is_some_and(|d| d.at_match.shares <= 0.0) {
            self.telegram(fmt_missed(sig.check.coin, sig.check.side, sig.s));
        }
    }

    fn on_shot_fill(&mut self, f: &ShotFill) {
        self.append("demo", demo_record(f));
        let coin = f.shot.coin;
        match self.desk.on_shot_fill(f) {
            DeskOut::Entry { pos, balance } => {
                self.append("entry", entry_record(&pos, f, self.cfg.stake_usd, balance));
                if !self.state.traded.contains(&(pos.s, pos.coin)) {
                    self.state.traded.push((pos.s, pos.coin));
                }
                if let Some(w) = &mut self.win {
                    w.n_trades += 1;
                }
                self.save();
                self.tables();
                let _ = self.to_hot.push(ColdMsg::Fill { coin, filled: true });
                self.telegram(fmt_entry(coin, pos.side, pos.s, &f.demo.at_match, f.shot.z.map(|z| z.z)));
            }
            _ => {
                let _ = self.to_hot.push(ColdMsg::Fill { coin, filled: false });
            }
        }
        self.update_pause();
    }

    /// What is due: the clock, the path to the exchange, the window's markets, the outcomes, the
    /// day's summary.
    fn schedule(&mut self, now: i64) {
        if now >= self.clock_at_ms && !self.clock_busy {
            self.clock_busy = true;
            let (http, server, bases, tx) = (self.http.clone(), self.cfg.risk.ntp_server.clone(), self.rest_bases(), self.tx.clone());
            tokio::spawn(async move {
                let _ = tx.send(Done::Clock(measure_clock(http, server, bases).await));
            });
        }
        if now >= self.rtt_at_ms && !self.rtt_busy {
            self.rtt_busy = true;
            let (http, clob, tx) = (self.http.clone(), self.cfg.urls.clob.clone(), self.tx.clone());
            tokio::spawn(async move {
                let _ = tx.send(Done::Rtt(measure_rtt(&http, &clob).await.ok()));
            });
        }
        self.schedule_window(now);
        let due: Vec<i64> = self.outcomes_at.iter().filter(|(_, at)| **at <= now).map(|(s, _)| *s).collect();
        for s in due {
            let positions = self.desk.account().positions.iter().any(|p| p.s == s);
            if !positions && now > s * 1000 + OUTCOMES_GIVE_UP_MS {
                self.outcomes_at.remove(&s);
                self.state.unsettled.remove(&s);
                self.save();
                continue;
            }
            let every = if now > s * 1000 + OUTCOMES_SLOW_AFTER_MS { OUTCOMES_SLOW_EVERY_MS } else { OUTCOMES_EVERY_MS };
            self.outcomes_at.insert(s, now + every);
            let coins = self.state.unsettled.get(&s).map_or_else(|| self.strategy.v2.watch_coins.clone(), |w| w.coins.clone());
            let (http, gamma, tx) = (self.http.clone(), self.cfg.urls.gamma.clone(), self.tx.clone());
            tokio::spawn(async move {
                let got = fetch_outcomes(&http, &gamma, s, &coins).await;
                let _ = tx.send(Done::Outcomes { s, got });
            });
        }
        self.summary(now);
    }

    fn schedule_window(&mut self, now: i64) {
        let open = self.cfg.feeds.polymarket_open_s;
        let close = self.cfg.feeds.polymarket_close_s;
        let plan = *self.plan.get_or_insert_with(|| window_plan(now, None, open, close));
        // a window that never finished is left
        if let Some(w) = &self.win
            && !w.done
            && now > (w.plan.s + WINDOW_GIVE_UP_S) * 1000
        {
            let s = w.plan.s;
            self.error("window", Some(s), "the exact engine did not finish the window");
            self.win = None;
            self.plan = Some(window_plan(now, Some(s), open, close));
            return;
        }
        let armed = self.win.as_ref().is_some_and(|w| w.plan.s == plan.s);
        if armed || self.gamma_busy || now < plan.gamma_at_ms.max(self.gamma_at_ms) || self.last_s.is_some_and(|l| l >= plan.s) {
            return;
        }
        if self.win.as_ref().is_some_and(|w| !w.done) {
            return; // the window before is still being decided
        }
        self.gamma_busy = true;
        let (http, gamma, clob, tx, s) = (self.http.clone(), self.cfg.urls.gamma.clone(), self.cfg.urls.clob.clone(), self.tx.clone(), plan.s);
        let coins = self.strategy.v2.watch_coins.clone();
        tokio::spawn(async move {
            let got = async {
                let markets = fetch_window(&http, &gamma, s, &coins).await?;
                let mut out = vec![];
                for m in markets {
                    let info = fetch_market_info(&http, &clob, &m.condition_id).await.ok();
                    out.push((m, info));
                }
                anyhow::Ok(out)
            }
            .await;
            let _ = tx.send(Done::Markets { s, got });
        });
    }

    fn on_done(&mut self, done: Done) {
        let now = self.now();
        match done {
            Done::Clock(offset) => {
                self.clock_busy = false;
                self.clock_at_ms = now + CLOCK_EVERY_MS;
                self.on_clock(offset);
            }
            Done::Rtt(ms) => {
                self.rtt_busy = false;
                self.rtt_at_ms = now + self.cfg.exec.warm_interval_s as i64 * 1000;
                if let Some(ms) = ms {
                    self.rtt.push(ms);
                    self.exact.set_timing(self.timing());
                }
            }
            Done::Markets { s, got } => self.on_markets(s, got, now),
            Done::Book { s, token, got } => match got {
                Ok((ts, bids, asks)) if self.win.as_ref().is_some_and(|w| w.plan.s == s) => self.exact.on_rest_book(token, ts, &bids, &asks),
                Ok(_) => {}
                Err(e) => self.error("book", Some(s), &format!("{e:#}")),
            },
            Done::Klines { coin, q0, q1, tries, got } => {
                let rows = got.map(|mut rows| backfill_rows(&mut rows, q1).then_some(rows));
                match rows {
                    Ok(Some(rows)) => self.give_klines(coin, rows),
                    _ if tries + 1 < BACKFILL_TRIES => self.backfill(coin, q0, q1, tries + 1),
                    Ok(None) => self.error("backfill", None, &format!("{}: no klines for {q0}..{q1}", coin.as_str())),
                    Err(e) => self.error("backfill", None, &format!("{}: {e:#}", coin.as_str())),
                }
            }
            Done::Outcomes { s, got } => match got {
                Ok(list) => self.on_outcomes(s, &list),
                Err(e) => self.error("outcomes", Some(s), &format!("{e:#}")),
            },
        }
    }

    fn on_markets(&mut self, s: i64, got: Result<Vec<(Market, Option<MarketInfo>)>>, now: i64) {
        self.gamma_busy = false;
        let Some(plan) = self.plan.filter(|p| p.s == s) else { return };
        let markets = match got {
            Ok(m) if !m.is_empty() => m,
            other => {
                if let Err(e) = other {
                    self.error("gamma", Some(s), &format!("{e:#}"));
                }
                if now < (s + GAMMA_UNTIL_S) * 1000 {
                    self.gamma_at_ms = now + GAMMA_RETRY_MS;
                } else {
                    self.append("window", json!({"s": s, "markets_ok": false, "note": "нет рынков окна в Gamma"}));
                    self.last_s = Some(s);
                    self.plan = Some(window_plan(now, Some(s), self.cfg.feeds.polymarket_open_s, self.cfg.feeds.polymarket_close_s));
                }
                return;
            }
        };
        let mut tokens = vec![];
        let mut coins = vec![];
        let mut notes = WindowNotes { binance_fallback: self.binance_fallback, ..WindowNotes::default() };
        for (m, info) in &markets {
            let note = rules_note(m, info.as_ref());
            let up_ix = u8::try_from(tokens.len()).unwrap_or(u8::MAX);
            tokens.push(m.up.clone());
            tokens.push(m.down.clone());
            coins.push(CoinArm {
                coin: m.coin,
                up_ix,
                down_ix: up_ix.saturating_add(1),
                up_key: token_key(&m.up).unwrap_or([0; 32]),
                fee: info.map_or(m.fee, |i| i.fee),
                tradable: note.is_none(),
                note: note.clone().unwrap_or_default(),
            });
            if let Some(n) = note {
                notes.rules.push((m.coin, n));
            }
        }
        notes.missing = self.strategy.v2.watch_coins.iter().copied().filter(|c| !markets.iter().any(|(m, _)| m.coin == *c)).collect();
        let traded = self.state.traded.iter().filter(|(ts, _)| *ts == s).map(|(_, c)| *c).collect();
        let arm = WindowArm { s, tokens, coins, missing: notes.missing.clone(), resumed: plan.resumed || now > s * 1000 + 835_000, traded };
        for (coin, note) in notes.rules.clone() {
            self.alarm(&format!("правила рынка {} изменились: {note}", coin.as_str()));
        }
        self.exact.arm(&arm);
        let _ = self.to_hot.push(ColdMsg::Arm(arm.clone()));
        self.last_s = Some(s);
        self.win = Some(Win { plan, arm, notes, fast: vec![], waits: vec![], n_trades: 0, copies_lost_at: None, copies_alarmed: false, paused: vec![], done: false });
    }

    fn on_outcomes(&mut self, s: i64, list: &[(Coin, Outcome)]) {
        let done_before = self.state.unsettled.get(&s).map(|w| w.done.clone()).unwrap_or_default();
        let new: Vec<(Coin, Outcome)> = list.iter().copied().filter(|(c, _)| !done_before.contains(c)).collect();
        if new.is_empty() {
            return;
        }
        let settled_utc = fmt_utc(self.now().div_euclid(1000));
        for (coin, o) in &new {
            for out in self.desk.on_outcome(s, *coin, o.payout_up) {
                match out {
                    DeskOut::Settled { id, coin, side, payout, pnl, balance } => {
                        self.append("settle", settle_record(&id, o, payout, pnl, payout - pnl, balance, &settled_utc));
                        let per_share = match side {
                            Side::Up => o.payout_up,
                            Side::Down => 1.0 - o.payout_up,
                        };
                        self.telegram(fmt_result(coin, side, per_share, pnl, balance));
                    }
                    DeskOut::Stopped => self.alarm("стоп демо-счёта: денег меньше ставки и резерва, открытых позиций нет"),
                    _ => {}
                }
            }
        }
        self.append("window_result", window_result_record(s, &new));
        let positions = self.desk.account().positions.iter().any(|p| p.s == s);
        if let Some(w) = self.state.unsettled.get_mut(&s) {
            w.done.extend(new.iter().map(|(c, _)| *c));
            if !positions && w.coins.iter().all(|c| w.done.contains(c)) {
                self.state.unsettled.remove(&s);
                self.outcomes_at.remove(&s);
            }
        }
        self.save();
        self.tables();
        self.update_pause();
    }

    /// The day's summary at `summary_utc`.
    fn summary(&mut self, now: i64) {
        let text = fmt_utc(now.div_euclid(1000));
        let (date, time) = (&text[..10], &text[11..16]);
        if time < self.cfg.telegram.summary_utc.as_str() || self.state.last_summary_date.as_deref() == Some(date) {
            return;
        }
        let date = date.to_string();
        match self.journal.all_but_bbo() {
            Ok(recs) => {
                let d = day_summary(&recs, &date, self.desk.account().cash, self.cfg.stake_usd);
                self.telegram(fmt_summary(&d));
            }
            Err(e) => self.error("summary", None, &format!("{e:#}")),
        }
        self.state.last_summary_date = Some(date);
        self.save();
    }

    fn on_clock(&mut self, offset: Option<i64>) {
        if let Some(ms) = offset {
            self.clock.set_offset_ms(ms);
            self.state.clock_offset_ms = Some(ms);
        }
        self.update_pause();
    }

    /// The pause of the shots: the clock first, then the money.
    fn update_pause(&mut self) {
        let clock = clock_pause(self.clock.offset_ms(), self.cfg.risk.clock_max_offset_ms);
        let pause = clock.or(self.desk.pause());
        if self.pause_sent == Some(pause) {
            return;
        }
        let was = self.pause_sent.flatten();
        if let Some(p) = clock
            && was != Some(p)
        {
            self.alarm(p);
        }
        if self.to_hot.push(ColdMsg::Pause(pause)).is_ok() {
            self.pause_sent = Some(pause);
        }
    }

    fn timing(&self) -> DemoTiming {
        DemoTiming {
            stake_usd: self.cfg.stake_usd,
            limit: self.strategy.v21.max_fill,
            half_rtt_ms: self.rtt.median_ms().map_or(HALF_RTT_MS, |m| m / 2.0),
            taker_delay_ms: self.cfg.exec.taker_delay_ms as f64,
        }
    }

    /// Klines for both engines.
    fn give_klines(&mut self, coin: Coin, rows: Vec<(i64, f64)>) {
        self.exact.on_klines(coin, &rows);
        let _ = self.to_hot.push(ColdMsg::Klines { coin, rows });
    }

    fn backfill(&mut self, coin: Coin, q0: i64, q1: i64, tries: u32) {
        let (http, bases, tx) = (self.http.clone(), self.rest_bases(), self.tx.clone());
        tokio::spawn(async move {
            if tries > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            let got = binance_rest::klines(&http, &bases, coin.binance_symbol(), q0, q1).await;
            let _ = tx.send(Done::Klines { coin, q0, q1, tries, got });
        });
    }

    fn append(&mut self, kind: &str, rec: Value) {
        if let Err(e) = self.journal.append(kind, rec) {
            eprintln!("v21bot: the journal: {e:#}");
        }
    }

    fn error(&mut self, place: &str, s: Option<i64>, error: &str) {
        eprintln!("v21bot: {place}: {error}");
        self.append("error", json!({"s": s, "where": place, "error": error}));
    }

    fn alarm(&mut self, text: &str) {
        let at_ms = self.now();
        self.append("alarm", json!({"at_ms": at_ms, "text": text}));
        self.telegram(fmt_alarm(text));
    }

    fn telegram(&self, text: String) {
        let tg = self.tg.clone();
        tokio::spawn(async move {
            tg.send(&text).await;
        });
    }

    fn save(&mut self) {
        self.state.account = self.desk.account().clone();
        if let Err(e) = save_state(&self.cfg.paths.state, &self.state) {
            self.error("state", None, &format!("{e:#}"));
        }
    }

    fn tables(&mut self) {
        match rebuild_tables(&self.journal, &self.tables) {
            Ok(true) => {}
            Ok(false) => eprintln!("v21bot: a table is held by another program: it is written at the next rebuild"),
            Err(e) => self.error("tables", None, &format!("{e:#}")),
        }
    }

    fn finish(mut self) -> Result<()> {
        self.save();
        self.tables();
        self.journal.sync()
    }
}

/// The clock's offset: NTP if a server is set and answers, else Binance's time.
async fn measure_clock(http: reqwest::Client, ntp_server: String, bases: Vec<String>) -> Option<i64> {
    if !ntp_server.is_empty()
        && let Ok(Ok((offset, _))) = tokio::time::timeout(NTP_WAIT, sntp_offset_ms(&ntp_server)).await
    {
        return Some(offset);
    }
    let (server, sent, received) = binance_rest::server_time_ms(&http, &bases).await.ok()?;
    Some(clock_offset_ms(server, sent, received))
}
