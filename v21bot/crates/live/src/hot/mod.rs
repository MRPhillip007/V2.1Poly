//! The hot thread (spec §4.2, §5.3): the copies of the market channel and the Binance trades in, the
//! fast decisions out — with nothing allocated after the warm-up and no lock shared with the cold
//! runtime, only the SPSC queues and atomics.
//!
//! `HotState` is the path itself: a frame through the dedupe, the reader and the fast engine; a trade
//! into the closes and the gap tracker; the clock's ticks. Every event and every decision goes to the
//! cold runtime; a full queue drops the message and counts it, the hot thread never waits.
//! `spawn_hot` runs it on a thread of its own: one `select!` over the copies, Binance and the timer.

pub mod binance;
pub mod copies;
pub mod engine;
pub mod items;
pub mod timer;
pub mod wire;
pub mod ws;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer};
use v21_core::params::Params;
use v21_core::window::Coin;

use crate::health::HotHealth;
use crate::hot::binance::{BinanceEvent, BinanceStream, GapTracker, Trade};
use crate::hot::copies::FrameDedupe;
use crate::hot::engine::FastEngine;
use crate::hot::items::{read_frame, FeedItem};
use crate::hot::timer::next_tick_ms;
use crate::hot::wire::Frame;
use crate::hot::ws::{CopyEvent, PmCopy};
use crate::shared::clock::ExchangeClock;
use crate::shared::config::Config;
use crate::shared::msgs::{ColdMsg, FastOut, HotMsg, WindowArm};

/// The dedupe of the copies: 32 768 slots for 3 s of frames (the busiest second of the recording
/// fills a quarter of it).
const DEDUPE_SLOTS: usize = 32_768;
const DEDUPE_TTL_MS: i64 = 3_000;
/// A health record a minute.
const HEALTH_EVERY_MS: i64 = 60_000;
/// Room for the items of one frame and the decisions of one step before the first growth.
const ROOM: usize = 1024;
/// Copies of the market channel the hot thread runs at most.
const MAX_COPIES: usize = 2;

pub struct HotState {
    engine: FastEngine,
    copies: u8,
    dedupe: FrameDedupe,
    frame: Frame,
    items: Vec<FeedItem>,
    fast: Vec<FastOut>,
    gaps: GapTracker,
    gap_rows: Vec<(Coin, i64, i64)>,
    tokens: Vec<Box<str>>,
    window: Option<i64>,
    up: [bool; MAX_COPIES],
    /// No copy is connected while the window's copies should be.
    lost: bool,
    health: HotHealth,
    health_at: Option<i64>,
    to_cold: Producer<HotMsg>,
}

/// Puts a message on the queue to the cold runtime; a full queue drops it, counted.
fn push(to_cold: &mut Producer<HotMsg>, health: &mut HotHealth, msg: HotMsg) {
    if to_cold.push(msg).is_err() {
        health.dropped_msgs += 1;
    }
}

impl HotState {
    /// `p` is the rule of v2.1; `copies` copies of the market channel (1 or 2).
    pub fn new(p: Params, feed_silent_ms: i64, copies: u8, to_cold: Producer<HotMsg>) -> HotState {
        let copies = copies.clamp(1, MAX_COPIES as u8);
        let gaps = GapTracker::new(&p.watch_coins);
        HotState {
            engine: FastEngine::new(p, feed_silent_ms),
            copies,
            dedupe: FrameDedupe::new(DEDUPE_SLOTS, copies, DEDUPE_TTL_MS),
            frame: Frame::default(),
            items: Vec::with_capacity(ROOM),
            fast: Vec::with_capacity(ROOM),
            gaps,
            gap_rows: Vec::with_capacity(8),
            tokens: vec![],
            window: None,
            up: [false; MAX_COPIES],
            lost: false,
            health: HotHealth::default(),
            health_at: None,
            to_cold,
        }
    }

    /// The window armed, if any.
    pub fn window(&self) -> Option<i64> {
        self.window
    }

    fn arm(&mut self, arm: &WindowArm) {
        self.tokens = arm.tokens.iter().map(|t| Box::from(t.as_str())).collect();
        self.dedupe = FrameDedupe::new(DEDUPE_SLOTS, self.copies, DEDUPE_TTL_MS);
        self.engine.arm(arm);
        self.window = Some(arm.s);
        self.copies_closed();
    }

    pub fn on_cold(&mut self, msg: ColdMsg) {
        match msg {
            ColdMsg::Arm(arm) => self.arm(&arm),
            ColdMsg::Klines { coin, rows } => self.engine.on_klines(coin, &rows),
            ColdMsg::Fill { coin, filled } => self.engine.on_fill(coin, filled),
            ColdMsg::Pause(pause) => self.engine.set_pause(pause),
        }
    }

    /// The decisions of the last step go out; a shot's time from its frame is filled in.
    fn flush_fast(&mut self, rx: Option<Instant>) {
        let HotState { fast, to_cold, health, .. } = self;
        let ns = rx.map(|at| u64::try_from(at.elapsed().as_nanos()).unwrap_or(u64::MAX));
        for mut out in fast.drain(..) {
            if let Some(ns) = ns
                && !matches!(out, FastOut::Wait { .. })
            {
                health.decide_ns.add(ns);
                if let FastOut::Shot(shot) = &mut out {
                    shot.frame_to_decision_ns = ns;
                }
            }
            push(to_cold, health, HotMsg::Fast(out));
        }
    }

    /// A frame of copy `copy` that came at `now_ms` (exchange clock), `rx` by this machine's clock.
    pub fn on_frame(&mut self, copy: u8, raw: &[u8], now_ms: i64, rx: Instant) {
        self.health.frames += 1;
        if !self.dedupe.admit(copy, raw, now_ms) {
            self.health.dup_frames += 1;
            return;
        }
        self.items.clear();
        self.health.bad_parts += u64::from(read_frame(raw, &self.tokens, &mut self.frame, &mut self.items));
        for k in 0..self.items.len() {
            let item = self.items[k];
            if let FeedItem::Bbo { ts_ms, .. } = item {
                self.health.pm_lag_ms.add(u64::try_from(now_ms - ts_ms).unwrap_or(0));
            }
            self.engine.on_item(now_ms, &item, &mut self.fast);
            push(&mut self.to_cold, &mut self.health, HotMsg::Feed { rx_ms: now_ms, item });
        }
        self.flush_fast(Some(rx));
    }

    /// A trade of the Binance stream that came at `now_ms`.
    pub fn on_trade(&mut self, t: &Trade, now_ms: i64) {
        self.health.trades += 1;
        self.health.bn_lag_ms.add(u64::try_from(now_ms - t.ts_ms).unwrap_or(0));
        self.engine.on_trade(now_ms, t);
        push(&mut self.to_cold, &mut self.health, HotMsg::Trade(*t));
        if let Some((coin, q0, q1)) = self.gaps.on_trade(t) {
            self.engine.on_gap(coin, q0, q1);
            push(&mut self.to_cold, &mut self.health, HotMsg::Gap { coin, q0, q1 });
        }
    }

    /// The Binance stream connected (`fallback`: to the mirror) or dropped.
    pub fn on_binance(&mut self, up: bool, fallback: bool, now_ms: i64) {
        if !up {
            self.gap_rows.clear();
            self.gaps.on_disconnect(now_ms, &mut self.gap_rows);
            for k in 0..self.gap_rows.len() {
                let (coin, q0, q1) = self.gap_rows[k];
                self.engine.on_gap(coin, q0, q1);
                push(&mut self.to_cold, &mut self.health, HotMsg::Gap { coin, q0, q1 });
            }
        }
        push(&mut self.to_cold, &mut self.health, HotMsg::Binance { up, fallback });
    }

    /// A copy connected or dropped: the loss of the last one is a loss of the feed.
    pub fn on_copy(&mut self, copy: u8, up: bool, code: Option<u16>, now_ms: i64) {
        if usize::from(copy) >= MAX_COPIES {
            return;
        }
        let before = self.up.iter().any(|u| *u);
        self.up[usize::from(copy)] = up;
        let after = self.up.iter().any(|u| *u);
        self.health.copies_up = self.up.iter().filter(|u| **u).count() as u8;
        push(&mut self.to_cold, &mut self.health, HotMsg::Copy { copy, up, code });
        if before && !after {
            self.lost = true;
            self.engine.on_coverage_lost();
            push(&mut self.to_cold, &mut self.health, HotMsg::Coverage { lost: true, at_ms: now_ms });
        } else if !before && after && self.lost {
            self.lost = false;
            push(&mut self.to_cold, &mut self.health, HotMsg::Coverage { lost: false, at_ms: now_ms });
        }
    }

    /// The copies were closed on purpose (the window is over, or a new one begins): no loss.
    pub fn copies_closed(&mut self) {
        self.up = [false; MAX_COPIES];
        self.lost = false;
        self.health.copies_up = 0;
    }

    /// The clock's tick: the fast engine's clock, and the health record once a minute.
    pub fn on_tick(&mut self, now_ms: i64) {
        self.engine.on_tick(now_ms, &mut self.fast);
        self.flush_fast(None);
        let due = *self.health_at.get_or_insert(now_ms + HEALTH_EVERY_MS);
        if now_ms >= due {
            let copies_up = self.health.copies_up;
            let h = std::mem::replace(&mut self.health, HotHealth { copies_up, ..HotHealth::default() });
            push(&mut self.to_cold, &mut self.health, HotMsg::Health(Box::new(h)));
            self.health_at = Some(due + HEALTH_EVERY_MS);
        }
    }

    fn on_copy_event(&mut self, ev: CopyEvent, now_ms: i64, rx: Instant) {
        match ev {
            CopyEvent::Frame { copy, raw } => self.on_frame(copy, &raw, now_ms, rx),
            CopyEvent::Up { copy } => self.on_copy(copy, true, None, now_ms),
            CopyEvent::Down { copy, code, .. } => self.on_copy(copy, false, code, now_ms),
        }
    }
}

/// The next event of a copy; none while there is no copy.
async fn next_copy(copy: &mut Option<PmCopy>) -> CopyEvent {
    match copy {
        Some(c) => c.next().await,
        None => std::future::pending().await,
    }
}

/// Pins this thread to core `core`, if there is one of that number.
pub(crate) fn pin(core: usize) {
    if let Some(id) = core_affinity::get_core_ids().unwrap_or_default().into_iter().find(|c| c.id == core) {
        core_affinity::set_for_current(id);
    }
}

/// Runs the hot thread until `stop` is set.
pub fn spawn_hot(cfg: &Config, p: Params, to_cold: Producer<HotMsg>, from_cold: Consumer<ColdMsg>, clock: Arc<ExchangeClock>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    let cfg = cfg.clone();
    std::thread::Builder::new()
        .name("v21-hot".into())
        .spawn(move || {
            if cfg.cores.pin {
                pin(cfg.cores.hot);
            }
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("the hot thread's runtime");
            rt.block_on(hot_loop(cfg, p, to_cold, from_cold, clock, stop));
        })
        .expect("the hot thread")
}

/// What woke the hot loop.
enum Woke {
    Copy(CopyEvent, Instant),
    Binance(BinanceEvent),
    Tick,
}

/// The window's subscription: when the copies connect and close, and its tokens.
struct Subscription {
    connect_at_ms: i64,
    close_at_ms: i64,
    tokens: Vec<String>,
}

async fn hot_loop(cfg: Config, p: Params, to_cold: Producer<HotMsg>, mut from_cold: Consumer<ColdMsg>, clock: Arc<ExchangeClock>, stop: Arc<AtomicBool>) {
    let coins = p.watch_coins.clone();
    let n = cfg.feeds.polymarket_copies.clamp(1, MAX_COPIES as u8);
    let mut st = HotState::new(p, cfg.risk.feed_silent_ms, n, to_cold);
    let mut binance = BinanceStream::new([cfg.feeds.binance_ws.clone(), cfg.feeds.binance_ws_fallback.clone()], &coins);
    let mut copies: [Option<PmCopy>; MAX_COPIES] = [None, None];
    let mut sub: Option<Subscription> = None;
    while !stop.load(Ordering::Relaxed) {
        let now = clock.now_ms();
        let wait = Duration::from_millis(u64::try_from(next_tick_ms(now, st.window()) - now).unwrap_or(0));
        let woke = {
            let [c0, c1] = &mut copies;
            tokio::select! {
                ev = next_copy(c0) => Woke::Copy(ev, Instant::now()),
                ev = next_copy(c1) => Woke::Copy(ev, Instant::now()),
                ev = binance.next() => Woke::Binance(ev),
                () = tokio::time::sleep(wait) => Woke::Tick,
            }
        };
        match woke {
            Woke::Copy(ev, rx) => st.on_copy_event(ev, clock.now_ms(), rx),
            Woke::Binance(BinanceEvent::Trade(t)) => st.on_trade(&t, clock.now_ms()),
            Woke::Binance(BinanceEvent::Up { fallback }) => st.on_binance(true, fallback, clock.now_ms()),
            Woke::Binance(BinanceEvent::Down { .. }) => st.on_binance(false, false, clock.now_ms()),
            Woke::Tick => {
                let now = clock.now_ms();
                while let Ok(msg) = from_cold.pop() {
                    if let ColdMsg::Arm(arm) = &msg {
                        // a new window: the old subscription ends
                        copies = [None, None];
                        let s0 = arm.s * 1000;
                        sub = Some(Subscription {
                            connect_at_ms: s0 + cfg.feeds.polymarket_open_s * 1000,
                            close_at_ms: s0 + cfg.feeds.polymarket_close_s * 1000,
                            tokens: arm.tokens.clone(),
                        });
                    }
                    st.on_cold(msg);
                }
                if let Some(w) = &sub {
                    if now >= w.close_at_ms {
                        copies = [None, None];
                        st.copies_closed();
                        sub = None;
                    } else if now >= w.connect_at_ms && copies.iter().all(Option::is_none) {
                        for (i, slot) in copies.iter_mut().enumerate().take(usize::from(n)) {
                            *slot = Some(PmCopy::new(&cfg.urls.polymarket_ws, &w.tokens, i as u8));
                        }
                    }
                }
                st.on_tick(now);
            }
        }
    }
}
