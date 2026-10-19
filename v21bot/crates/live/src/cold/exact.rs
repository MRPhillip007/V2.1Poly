//! The exact engine (spec §5.3, §6.2): the decisions the backtest would take, on the live events put
//! back in the exchange's time order, and the demo fills of the signals. The port of `paper/engine.py`
//! (the detectors, the books, the order of the decisions), run on exchange time:
//!
//! - **the buffer**: events wait `hold_ms` behind the newest one that has come and go out by (exchange
//!   time, arrival) — the order of the archive. An event older than the last one released of its token
//!   is late and dropped. With nothing coming for `QUIET_MS`, the clock alone moves the line;
//! - **one exchange time** is released as `paper` takes one message: the detectors first, with the
//!   books as they were before it; then the books; then the books against the server's best prices;
//! - **the decisions** are taken in the backtest's order, by (t_trig, Up token id), once no coin has an
//!   earlier check running, the second before the signal has closed on Binance and the demo fill of
//!   the check is known;
//! - **the demo fill** is the book of the bought token at fixed moments: right after the band hit, at
//!   the match (send + half the round trip + the taker delay) and on the grid `GRID_MS` from the send.
//!
//! The items of one websocket frame come before the next `advance`: a snapshot's levels join it while
//! it waits in the buffer.

use std::collections::BTreeMap;

use v21_core::decision::{Decision, WindowEngine};
use v21_core::detector::{Check, CoinDetector};
use v21_core::fill::{fak_buy, FeeSchedule, Fill};
use v21_core::live_closes::LiveCloses;
use v21_core::params::{Params, Strategy};
use v21_core::window::{Coin, Side};
use v21_core::zscore::{p0_of, z_score, ZInfo};

use crate::book::{grid_tick, Book, BookSide};
use crate::hot::binance::Trade;
use crate::hot::items::FeedItem;
use crate::shared::msgs::{CoinArm, ShotRecord, WindowArm};

/// Delays from the send whose fills are kept, ms: the history is priced again with them once the taker
/// delay has been measured (spec §6.2).
pub const GRID_MS: [i64; 13] = [0, 50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 550, 600];

/// With no event for this long, every event older than the clock minus this has come.
const QUIET_MS: i64 = 2000;
/// A token's book is reloaded over REST at most this often, exchange time.
const RELOAD_EVERY_MS: i64 = 5000;
/// A gap this long between a coin's events inside t0..t1: the data of the minute is not whole. Also
/// how long before t0 a drop of both copies makes it not whole.
const SILENCE_MS: i64 = 5000;
/// A successful check without z waits for the Binance backfill until this second of the window.
const Z_DEADLINE_S: i64 = 1020;
/// The rows of the Up tokens kept for the journal: these ms of the window, both ends in.
const BBO_FROM_MS: i64 = 835_000;
const BBO_TO_MS: i64 = 871_000;
/// The window is done once the line is past this ms of it and every check is decided.
const DONE_AT_MS: i64 = 871_000;
/// A shot belongs to the check of its coin whose t_trig is at most this far from its own.
const SHOT_LINK_S: f64 = 0.3;

/// The slots of a demo: the fill without latency, at the match, then the grid.
const NO_LATENCY: usize = 0;
const AT_MATCH: usize = 1;
const GRID: usize = 2;

/// How the demo orders are priced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DemoTiming {
    /// Dollars before the fee.
    pub stake_usd: f64,
    /// The highest price taken.
    pub limit: f64,
    /// Half the round trip to the exchange, ms.
    pub half_rtt_ms: f64,
    /// The exchange's taker delay, ms.
    pub taker_delay_ms: f64,
}

/// The demo fill of an order.
#[derive(Clone, Debug, PartialEq)]
pub struct DemoFill {
    /// The match moment: send + half the round trip + the taker delay, exchange ms.
    pub match_ms: i64,
    /// The book at the match moment.
    pub at_match: Fill,
    /// The book right after the band hit: the price without latency.
    pub no_latency: Fill,
    /// The book at the send + `GRID_MS[k]`; `None` for the fill a shot waits for.
    pub by_delay: Option<[Fill; 13]>,
}

/// A check of the exact engine and the decisions of v2 and v2.1 on it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExactSignal {
    pub s: i64,
    pub check: Check,
    /// The number of a successful check in the window.
    pub rank: Option<u32>,
    pub z: Option<ZInfo>,
    pub v2: Decision,
    pub v21: Decision,
    /// How late the price of the check (the Up event in force at its fill moment) was delivered, ms.
    pub lag_ms: i64,
    /// When the order went (a shot) or would have gone, exchange ms.
    pub send_ms: i64,
    /// The fast engine shot on this check.
    pub shot: bool,
    /// For a successful check and for a check with a shot.
    pub demo: Option<DemoFill>,
}

/// The demo fill of a shot, as soon as its match moment has passed: the hot thread waits for it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShotFill {
    pub shot: ShotRecord,
    pub demo: DemoFill,
}

/// A row of the bbo table: exchange ms, best bid, best ask.
pub type BboRow = (i64, f64, f64);

/// What the window was like, for the journal.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSummary {
    pub s: i64,
    /// The Polymarket data of 840–870 s is whole: no gap over 5 s, no drop of both copies, every coin
    /// up to t1.
    pub ws_ok: bool,
    /// The Binance closes of `s − 61 ..= s + t1 − 1` are known for every watched coin.
    pub binance_ok: bool,
    /// Successful checks.
    pub n_signals: u32,
    /// The worst delivery lag of the events of 840–870 s.
    pub lag_max_ms: i64,
    /// Events dropped for coming after a later one of their token had gone out.
    pub late_dropped: u32,
    pub book_reloads: u32,
    /// From each drop of both copies to the moment every token had a fresh snapshot.
    pub coverage_lost_ms: i64,
    /// The Binance copy of the price to beat of each watched coin.
    pub p0: Vec<(Coin, Option<f64>)>,
    /// The rows (exchange ms, bid, ask) of each coin's Up token in 835–871 s, in time order.
    pub bbo: Vec<(Coin, Vec<BboRow>)>,
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant, reason = "a few per window, each moved once into the journal")]
pub enum ExactOut {
    Signal(ExactSignal),
    ShotFill(ShotFill),
    /// The token's book differs from the server's best prices: fetch it over REST.
    Reload { token: u8 },
    WindowDone(WindowSummary),
}

/// A row of the bbo table waiting in the buffer.
#[derive(Clone, Copy)]
struct BboItem {
    token: u8,
    bid: f64,
    ask: f64,
    rx_ms: i64,
    /// The best prices of a snapshot (they reach the detector while the token waits for one).
    snapshot: bool,
}

/// What waits in the buffer.
enum Body {
    Bbo(BboItem),
    /// A snapshot of the feed, its levels joining it, or a book fetched over REST.
    Book { token: u8, levels: Vec<(BookSide, u16, f64)>, rest: bool },
    Level { token: u8, side: BookSide, tick: Option<u16>, size: f64, best_bid: f64, best_ask: f64 },
    /// Both copies dropped.
    CoverageLost,
}

struct Token {
    /// Its coin: a place in `Window::coins`.
    coin: usize,
    book: Book,
    /// After a drop of both copies: changes wait for a fresh snapshot.
    awaiting: bool,
    /// The exchange time of its last released event.
    last_ms: i64,
    reload_at: Option<i64>,
    /// The buffer key of its snapshot whose levels are still coming.
    open_snapshot: Option<(i64, u64)>,
    /// The server's best prices of its last change in the time being released.
    server_best: Option<(f64, f64)>,
}

impl Token {
    fn new(coin: usize) -> Token {
        Token { coin, book: Book::new(), awaiting: false, last_ms: i64::MIN, reload_at: None, open_snapshot: None, server_best: None }
    }
}

/// What a demo is priced on.
struct DemoSpec {
    token: u8,
    fee: FeeSchedule,
    send_ms: i64,
    /// The no-latency moment.
    band_ms: i64,
}

struct Demo {
    token: u8,
    fee: FeeSchedule,
    band_ms: i64,
    stake: f64,
    limit: f64,
    match_ms: i64,
    fills: Vec<Option<Fill>>,
    /// Slots not taken yet.
    left: usize,
    /// The demo of a shot: a `ShotFill` goes out once it is complete.
    shot: Option<ShotRecord>,
}

impl Demo {
    fn fill(&self) -> DemoFill {
        let f = |k: usize| self.fills[k].expect("a demo is read once complete");
        DemoFill {
            match_ms: self.match_ms,
            at_match: f(AT_MATCH),
            no_latency: f(NO_LATENCY),
            by_delay: (self.fills.len() > GRID).then(|| std::array::from_fn(|k| f(GRID + k))),
        }
    }
}

/// A check that has started and waits for its price.
struct Started {
    t_trig: f64,
    demo: usize,
    send_ms: i64,
    shot: Option<ShotRecord>,
}

/// A check that has its price and waits for its turn.
struct Held {
    key: (f64, [u8; 32]),
    check: Check,
    demo: usize,
    send_ms: i64,
    shot: Option<ShotRecord>,
    lag_ms: i64,
}

struct CoinState {
    arm: CoinArm,
    det: CoinDetector,
    /// Has a successful check.
    ok: bool,
    started: Option<Started>,
    /// The Up event whose prices the detector holds: its arrival and its delivery lag.
    up_rx_ms: i64,
    up_lag_ms: i64,
    /// The exchange time of its latest event (either token).
    last_event_ms: Option<i64>,
    bbo: Vec<BboRow>,
}

struct Window {
    s: i64,
    s0: i64,
    t0_ms: i64,
    t1_ms: i64,
    /// The detector's parameters (those of v2 and v2.1 alike).
    p: Params,
    /// By token number; `None` for a token of no coin.
    tokens: Vec<Option<Token>>,
    coins: Vec<CoinState>,
    e2: WindowEngine,
    e21: WindowEngine,
    buf: BTreeMap<(i64, u64), Body>,
    seq: u64,
    newest_ms: Option<i64>,
    /// Events up to this exchange time have come: the release line.
    line_ms: i64,
    /// The detectors and the demos have been brought up to this time.
    passed_ms: i64,
    /// The demo slots to take: (moment, order) → (demo, slot).
    caps: BTreeMap<(i64, u64), (usize, usize)>,
    cap_seq: u64,
    demos: Vec<Demo>,
    held: Vec<Held>,
    /// Shots no check has claimed yet.
    shots: Vec<ShotRecord>,
    ws_ok: bool,
    lag_max_ms: i64,
    late_dropped: u32,
    book_reloads: u32,
    /// Both copies are down since then.
    lost_at: Option<i64>,
    coverage_lost_ms: i64,
    n_signals: u32,
    done: bool,
    asks: Vec<(f64, f64)>,
}

/// The place of a coin's closes.
fn ci(c: Coin) -> usize {
    match c {
        Coin::Btc => 0,
        Coin::Eth => 1,
        Coin::Sol => 2,
    }
}

impl Window {
    fn new(p: &Params, arm: &WindowArm) -> Window {
        let s0 = arm.s * 1000;
        let mut tokens: Vec<Option<Token>> = (0..arm.tokens.len()).map(|_| None).collect();
        for (i, c) in arm.coins.iter().enumerate() {
            for ix in [c.up_ix, c.down_ix] {
                if let Some(slot) = tokens.get_mut(usize::from(ix)) {
                    *slot = Some(Token::new(i));
                }
            }
        }
        let coins = arm
            .coins
            .iter()
            .map(|c| CoinState {
                arm: c.clone(),
                det: CoinDetector::new(p, c.coin, s0),
                ok: false,
                started: None,
                up_rx_ms: 0,
                up_lag_ms: 0,
                last_event_ms: None,
                bbo: vec![],
            })
            .collect();
        let mut w = Window {
            s: arm.s,
            s0,
            t0_ms: s0 + (p.t0 * 1000.0).round() as i64,
            t1_ms: s0 + (p.t1 * 1000.0).round() as i64,
            p: p.clone(),
            tokens,
            coins,
            e2: WindowEngine::new(arm.s),
            e21: WindowEngine::new(arm.s),
            buf: BTreeMap::new(),
            seq: 0,
            newest_ms: None,
            line_ms: i64::MIN,
            passed_ms: i64::MIN,
            caps: BTreeMap::new(),
            cap_seq: 0,
            demos: vec![],
            held: vec![],
            shots: vec![],
            ws_ok: true,
            lag_max_ms: 0,
            late_dropped: 0,
            book_reloads: 0,
            lost_at: None,
            coverage_lost_ms: 0,
            n_signals: 0,
            done: false,
            asks: Vec::with_capacity(1001),
        };
        if arm.resumed {
            // the start in the middle of the window is a drop of both copies at its start
            w.lose_coverage(s0);
        }
        w
    }

    fn token(&self, token: u8) -> Option<&Token> {
        self.tokens.get(usize::from(token)).and_then(Option::as_ref)
    }

    fn token_mut(&mut self, token: u8) -> Option<&mut Token> {
        self.tokens.get_mut(usize::from(token)).and_then(Option::as_mut)
    }

    fn push(&mut self, ts_ms: i64, body: Body) -> (i64, u64) {
        let key = (ts_ms, self.seq);
        self.seq += 1;
        self.buf.insert(key, body);
        key
    }

    fn on_item(&mut self, rx_ms: i64, item: FeedItem) {
        match item {
            FeedItem::Bbo { token, ts_ms, bid, ask } => {
                let Some(t) = self.token_mut(token) else { return };
                let snapshot = t.open_snapshot.take().is_some();
                let late = ts_ms < t.last_ms;
                self.newest_ms = Some(self.newest_ms.map_or(ts_ms, |n| n.max(ts_ms)));
                if late {
                    self.late_dropped += 1;
                    return;
                }
                self.push(ts_ms, Body::Bbo(BboItem { token, bid, ask, rx_ms, snapshot }));
            }
            FeedItem::Snapshot { token, ts_ms } => {
                let Some(t) = self.token_mut(token) else { return };
                t.open_snapshot = None;
                let late = ts_ms < t.last_ms;
                self.newest_ms = Some(self.newest_ms.map_or(ts_ms, |n| n.max(ts_ms)));
                if !late {
                    let key = self.push(ts_ms, Body::Book { token, levels: vec![], rest: false });
                    if let Some(t) = self.token_mut(token) {
                        t.open_snapshot = Some(key);
                    }
                }
            }
            FeedItem::SnapLevel { token, side, tick, size } => {
                let Some(key) = self.token(token).and_then(|t| t.open_snapshot) else { return };
                if let Some(Body::Book { levels, .. }) = self.buf.get_mut(&key) {
                    levels.push((side, tick, size));
                }
            }
            FeedItem::Level { token, ts_ms, side, tick, size, best_bid, best_ask } => {
                let Some(t) = self.token_mut(token) else { return };
                t.open_snapshot = None;
                let late = ts_ms < t.last_ms;
                self.newest_ms = Some(self.newest_ms.map_or(ts_ms, |n| n.max(ts_ms)));
                if !late {
                    self.push(ts_ms, Body::Level { token, side, tick, size, best_bid, best_ask });
                }
            }
            FeedItem::TickSize { .. } => {}
        }
    }

    /// Both copies dropped (`paper` `_reset`): the books are unknown until fresh snapshots come, and
    /// the checks running are dropped.
    fn lose_coverage(&mut self, at_ms: i64) {
        for t in self.tokens.iter_mut().flatten() {
            t.book.clear();
            t.awaiting = true;
            t.server_best = None;
        }
        for c in &mut self.coins {
            if c.det.is_done() && !c.ok {
                // skipped for a missing book at t0: it may yet find one
                c.det = CoinDetector::new(&self.p, c.arm.coin, self.s0);
            }
            c.det.reset();
            if let Some(shot) = c.started.take().and_then(|st| st.shot) {
                self.shots.push(shot);
            }
        }
        self.lost_at.get_or_insert(at_ms);
    }

    fn release(&mut self, timing: &DemoTiming, out: &mut Vec<ExactOut>) {
        while let Some((&(ts, _), _)) = self.buf.first_key_value() {
            if ts > self.line_ms {
                break;
            }
            self.pass_time(ts - 1, timing, out);
            let mut group = vec![];
            while let Some(e) = self.buf.first_entry() {
                if e.key().0 != ts {
                    break;
                }
                group.push(e.remove());
            }
            self.apply(ts, group, timing, out);
        }
        self.pass_time(self.line_ms, timing, out);
    }

    /// Time has come up to `h` with every event up to it applied: the detectors and the demos.
    fn pass_time(&mut self, h: i64, timing: &DemoTiming, out: &mut Vec<ExactOut>) {
        for i in 0..self.coins.len() {
            let chk = self.coins[i].det.flush(h);
            let in_force = (self.coins[i].up_rx_ms, self.coins[i].up_lag_ms);
            self.after_detector(i, chk, in_force, None, timing);
        }
        self.passed_ms = self.passed_ms.max(h);
        self.take(self.passed_ms, out);
    }

    /// The events of one exchange time, in the order they came.
    fn apply(&mut self, ts: i64, group: Vec<Body>, timing: &DemoTiming, out: &mut Vec<ExactOut>) {
        if group.iter().any(|b| matches!(b, Body::CoverageLost)) {
            self.lose_coverage(ts);
        }
        // 1. the detectors, with the books as they were before this time
        for b in &group {
            if let Body::Bbo(b) = b {
                self.on_bbo(ts, *b, timing);
            }
        }
        // a check that started at an earlier moment (the book at t0) is priced on these books
        self.take((ts - 1).max(self.passed_ms), out);
        // 2. the books
        for b in group {
            let token = match b {
                Body::Bbo(b) => b.token,
                Body::Book { token, .. } | Body::Level { token, .. } => token,
                Body::CoverageLost => continue,
            };
            let Some(t) = self.token_mut(token) else { continue };
            t.last_ms = ts;
            match b {
                Body::Book { levels, rest, .. } => {
                    t.book.clear();
                    for (side, tick, size) in levels {
                        t.book.set(side, tick, size);
                    }
                    t.awaiting &= rest;
                    t.server_best = None;
                }
                Body::Level { side, tick, size, best_bid, best_ask, .. } if !t.awaiting => {
                    if let Some(tick) = tick {
                        t.book.set(side, tick, size);
                    }
                    // the levels a trade took come without a change of their own
                    t.book.trim(best_bid, best_ask);
                    t.server_best = Some((best_bid, best_ask));
                }
                _ => {}
            }
        }
        if let Some(at) = self.lost_at
            && self.tokens.iter().flatten().all(|t| !t.awaiting)
        {
            // every token has a fresh snapshot: the copies are back
            self.coverage_lost_ms += ts - at;
            if at <= self.t1_ms && ts >= self.t0_ms - SILENCE_MS {
                self.ws_ok = false;
            }
            self.lost_at = None;
        }
        // 3. the books against the server's best prices (spec §5.2)
        for (ix, t) in self.tokens.iter_mut().enumerate() {
            let Some(t) = t else { continue };
            let Some((bid, ask)) = t.server_best.take() else { continue };
            if !t.book.matches(bid, ask) && t.reload_at.is_none_or(|r| ts - r >= RELOAD_EVERY_MS) {
                t.reload_at = Some(ts);
                self.book_reloads += 1;
                out.push(ExactOut::Reload { token: u8::try_from(ix).expect("a token number") });
            }
        }
    }

    /// A row of the bbo table: the coin's stream in event time, then its detector.
    fn on_bbo(&mut self, ts: i64, b: BboItem, timing: &DemoTiming) {
        let Some(t) = self.token(b.token) else { return };
        let (i, awaiting) = (t.coin, t.awaiting);
        let (t0_ms, t1_ms) = (self.t0_ms, self.t1_ms);
        let c = &mut self.coins[i];
        if let Some(prev) = c.last_event_ms
            && ts - prev > SILENCE_MS
            && ts > t0_ms
            && prev < t1_ms
        {
            self.ws_ok = false;
        }
        if (t0_ms..=t1_ms).contains(&ts) {
            self.lag_max_ms = self.lag_max_ms.max(b.rx_ms - ts);
        }
        c.last_event_ms = Some(c.last_event_ms.map_or(ts, |p| p.max(ts)));
        if b.token != c.arm.up_ix {
            return;
        }
        if (BBO_FROM_MS..=BBO_TO_MS).contains(&(ts - self.s0)) {
            c.bbo.push((ts, b.bid, b.ask));
        }
        if awaiting && !b.snapshot {
            return;
        }
        let in_force = (c.up_rx_ms, c.up_lag_ms);
        let chk = c.det.update(ts, b.bid, b.ask);
        (c.up_rx_ms, c.up_lag_ms) = (b.rx_ms, b.rx_ms - ts);
        self.after_detector(i, chk, in_force, Some(b.rx_ms), timing);
    }

    /// After a detector step: the check it settled is held for its turn, a check it started gets its
    /// demo. `in_force` is the Up event whose prices the step used (arrival, lag); `this_rx` the
    /// arrival of the event of the step, if an event made it.
    fn after_detector(&mut self, i: usize, chk: Option<Check>, in_force: (i64, i64), this_rx: Option<i64>, timing: &DemoTiming) {
        let t0 = self.p.t0;
        // the event that put the price in the band: the book at t0 is the event in force before
        let start_rx = |t_trig: f64| match this_rx {
            Some(rx) if t_trig != t0 => rx,
            _ => in_force.0,
        };
        if let Some(chk) = chk {
            let st = match self.coins[i].started.take() {
                Some(st) if st.t_trig == chk.t_trig => st,
                // it started and settled in one step
                _ => self.start(i, chk.t_trig, chk.side, start_rx(chk.t_trig), timing),
            };
            let c = &mut self.coins[i];
            c.ok |= chk.ok;
            self.held.push(Held {
                key: (chk.t_trig, c.arm.up_key),
                check: chk,
                demo: st.demo,
                send_ms: st.send_ms,
                shot: st.shot,
                lag_ms: in_force.1,
            });
        }
        if self.coins[i].started.is_none()
            && let Some((t_trig, side, _)) = self.coins[i].det.pending()
        {
            let st = self.start(i, t_trig, side, start_rx(t_trig), timing);
            self.coins[i].started = Some(st);
        }
    }

    /// A check has started: its shot, its send, its demo.
    fn start(&mut self, i: usize, t_trig: f64, side: Side, start_rx: i64, timing: &DemoTiming) -> Started {
        let arm = &self.coins[i].arm;
        let (coin, fee) = (arm.coin, arm.fee);
        let token = match side {
            Side::Up => arm.up_ix,
            Side::Down => arm.down_ix,
        };
        let shot = self.take_shot(coin, t_trig);
        // without a shot: when the order would have gone, at 840 s or when the event came
        let send_ms = shot.map_or(start_rx.max(self.t0_ms), |s| s.send_ms);
        let band_ms = self.s0 + (t_trig * 1000.0).round() as i64;
        let demo = self.new_demo(DemoSpec { token, fee, send_ms, band_ms }, None, true, None, timing);
        Started { t_trig, demo, send_ms, shot }
    }

    /// The nearest shot of `coin` no check has claimed, within `SHOT_LINK_S` of `t_trig`.
    fn take_shot(&mut self, coin: Coin, t_trig: f64) -> Option<ShotRecord> {
        let gap = |s: &ShotRecord| (s.t_trig - t_trig).abs();
        let k = (0..self.shots.len())
            .filter(|&k| self.shots[k].coin == coin && gap(&self.shots[k]) <= SHOT_LINK_S + 1e-9)
            .min_by(|&a, &b| gap(&self.shots[a]).total_cmp(&gap(&self.shots[b])))?;
        Some(self.shots.swap_remove(k))
    }

    fn new_demo(&mut self, spec: DemoSpec, no_latency: Option<Fill>, grid: bool, shot: Option<ShotRecord>, timing: &DemoTiming) -> usize {
        let match_ms = spec.send_ms + (timing.half_rtt_ms + timing.taker_delay_ms).round() as i64;
        let n = if grid { GRID + GRID_MS.len() } else { GRID };
        let mut fills = vec![None; n];
        fills[NO_LATENCY] = no_latency;
        let id = self.demos.len();
        self.demos.push(Demo {
            token: spec.token,
            fee: spec.fee,
            band_ms: spec.band_ms,
            stake: timing.stake_usd,
            limit: timing.limit,
            match_ms,
            fills,
            left: n - usize::from(no_latency.is_some()),
            shot,
        });
        if no_latency.is_none() {
            self.capture(spec.band_ms, id, NO_LATENCY);
        }
        self.capture(match_ms, id, AT_MATCH);
        if grid {
            for (k, d) in GRID_MS.iter().enumerate() {
                self.capture(spec.send_ms + d, id, GRID + k);
            }
        }
        id
    }

    fn capture(&mut self, at_ms: i64, demo: usize, slot: usize) {
        self.caps.insert((at_ms, self.cap_seq), (demo, slot));
        self.cap_seq += 1;
    }

    /// Prices the demo slots whose moments are up to `h` on the books as they stand.
    fn take(&mut self, h: i64, out: &mut Vec<ExactOut>) {
        while let Some(e) = self.caps.first_entry() {
            if e.key().0 > h {
                break;
            }
            let (id, slot) = e.remove();
            let d = &mut self.demos[id];
            let Some(t) = self.tokens.get(usize::from(d.token)).and_then(Option::as_ref) else { continue };
            t.book.asks_upto(d.limit, &mut self.asks);
            d.fills[slot] = Some(fak_buy(&self.asks, d.stake, d.limit, &d.fee));
            d.left -= 1;
            if d.left == 0
                && let Some(shot) = d.shot
            {
                out.push(ExactOut::ShotFill(ShotFill { shot, demo: d.fill() }));
            }
        }
    }

    fn on_shot(&mut self, shot: &ShotRecord, timing: &DemoTiming) {
        if shot.s != self.s {
            return;
        }
        let Some(i) = self.coins.iter().position(|c| c.arm.coin == shot.coin) else { return };
        let arm = &self.coins[i].arm;
        let token = match shot.side {
            Side::Up => arm.up_ix,
            Side::Down => arm.down_ix,
        };
        let spec = DemoSpec { token, fee: arm.fee, send_ms: shot.send_ms, band_ms: shot.event_ts_ms };
        // the fill the hot thread waits for
        self.new_demo(spec, None, false, Some(*shot), timing);
        // a check of its coin the exact engine has already seen start takes it, and the shot's send
        let near = |t: f64| (t - shot.t_trig).abs() <= SHOT_LINK_S + 1e-9;
        if let Some(old) = self.coins[i].started.as_ref().filter(|st| st.shot.is_none() && near(st.t_trig)).map(|st| st.demo) {
            let demo = self.resend(old, shot, timing);
            let st = self.coins[i].started.as_mut().expect("just seen");
            (st.demo, st.send_ms, st.shot) = (demo, shot.send_ms, Some(*shot));
            return;
        }
        let gap = |h: &Held| (h.check.t_trig - shot.t_trig).abs();
        let held = (0..self.held.len())
            .filter(|&k| self.held[k].check.coin == shot.coin && self.held[k].shot.is_none() && near(self.held[k].check.t_trig))
            .min_by(|&a, &b| gap(&self.held[a]).total_cmp(&gap(&self.held[b])));
        if let Some(k) = held {
            let demo = self.resend(self.held[k].demo, shot, timing);
            let h = &mut self.held[k];
            (h.demo, h.send_ms, h.shot) = (demo, shot.send_ms, Some(*shot));
            return;
        }
        self.shots.push(*shot);
    }

    /// The demo of a check priced again from the send of its shot; the no-latency fill stays.
    fn resend(&mut self, old: usize, shot: &ShotRecord, timing: &DemoTiming) -> usize {
        let d = &self.demos[old];
        let spec = DemoSpec { token: d.token, fee: d.fee, send_ms: shot.send_ms, band_ms: d.band_ms };
        let no_latency = d.fills[NO_LATENCY];
        self.new_demo(spec, no_latency, true, None, timing)
    }

    /// The held checks in the backtest's order, as far as the clock allows. `paper` also waits for a
    /// check of another coin with an earlier key that has no price yet; here there is none by then: the
    /// line is past the held check's fill moment, so past every earlier one, and the detectors have
    /// been brought up to it.
    fn decide(&mut self, strategy: &Strategy, closes: &[LiveCloses; 3], now_ms: i64, out: &mut Vec<ExactOut>) {
        while let Some(k) = (0..self.held.len()).min_by(|&a, &b| {
            let (x, y) = (self.held[a].key, self.held[b].key);
            x.0.total_cmp(&y.0).then(x.1.cmp(&y.1))
        }) {
            let h = &self.held[k];
            if (h.check.ok || h.shot.is_some()) && self.demos[h.demo].left > 0 {
                return;
            }
            // the exact z: the second before the signal has closed
            let cl = &closes[ci(h.check.coin)];
            let t = self.s + h.check.t_trig.floor() as i64;
            if !cl.is_final(t - 1, now_ms) {
                return;
            }
            let z = z_score(cl, self.s, h.check.t_trig, h.check.side, &strategy.v2);
            if h.check.ok && z.is_none() && now_ms < (self.s + Z_DEADLINE_S) * 1000 {
                return; // a hole of Binance's may yet be filled
            }
            let h = self.held.swap_remove(k);
            let v2 = self.e2.decide(&strategy.v2, &h.check, z.as_ref());
            let v21 = self.e21.decide(&strategy.v21, &h.check, z.as_ref());
            self.n_signals += u32::from(h.check.ok);
            let demo = (h.check.ok || h.shot.is_some()).then(|| self.demos[h.demo].fill());
            out.push(ExactOut::Signal(ExactSignal {
                s: self.s,
                check: h.check,
                rank: v21.rank,
                z,
                v2,
                v21,
                lag_ms: h.lag_ms,
                send_ms: h.send_ms,
                shot: h.shot.is_some(),
                demo,
            }));
        }
    }

    /// The summary, once the line is past the decision minute and every check is decided.
    fn finish(&mut self, strategy: &Strategy, closes: &[LiveCloses; 3], scratch: &mut Vec<f64>, out: &mut Vec<ExactOut>) {
        if self.done || self.line_ms < self.s0 + DONE_AT_MS || !self.held.is_empty() || self.coins.iter().any(|c| !c.det.is_done()) {
            return;
        }
        self.done = true;
        if let Some(at) = self.lost_at.take() {
            // the copies did not come back in the window
            self.coverage_lost_ms += (self.s0 + DONE_AT_MS - at).max(0);
            if at <= self.t1_ms {
                self.ws_ok = false;
            }
        }
        if self.coins.iter().any(|c| c.last_event_ms.is_none_or(|t| t < self.t1_ms)) {
            self.ws_ok = false; // a stream did not reach the end of the minute
        }
        let p = &strategy.v2;
        let last = self.s + p.t1 as i64 - 1;
        let binance_ok = p.watch_coins.iter().all(|&c| closes[ci(c)].get(self.s - 61, last, scratch));
        out.push(ExactOut::WindowDone(WindowSummary {
            s: self.s,
            ws_ok: self.ws_ok,
            binance_ok,
            n_signals: self.n_signals,
            lag_max_ms: self.lag_max_ms,
            late_dropped: self.late_dropped,
            book_reloads: self.book_reloads,
            coverage_lost_ms: self.coverage_lost_ms,
            p0: p.watch_coins.iter().map(|&c| (c, p0_of(&closes[ci(c)], self.s))).collect(),
            bbo: self.coins.iter_mut().map(|c| (c.arm.coin, std::mem::take(&mut c.bbo))).collect(),
        }));
    }
}

/// The exact engine of the cold runtime: one window at a time, the Binance closes of every coin.
pub struct ExactEngine {
    strategy: Strategy,
    hold_ms: i64,
    timing: DemoTiming,
    closes: [LiveCloses; 3],
    now_ms: i64,
    win: Option<Window>,
    scratch: Vec<f64>,
}

impl ExactEngine {
    pub fn new(strategy: Strategy, hold_ms: i64, timing: DemoTiming) -> ExactEngine {
        ExactEngine {
            strategy,
            hold_ms,
            timing,
            closes: std::array::from_fn(|_| LiveCloses::new()),
            now_ms: i64::MIN,
            win: None,
            scratch: vec![],
        }
    }

    /// The window to watch from now on; what is left of the one before is forgotten.
    pub fn arm(&mut self, arm: &WindowArm) {
        // the detector's parameters are those of v2 and v2.1 alike
        self.win = Some(Window::new(&self.strategy.v2, arm));
    }

    fn open(&mut self) -> Option<&mut Window> {
        self.win.as_mut().filter(|w| !w.done)
    }

    /// An item of the market channel that arrived at `rx_ms` (exchange clock).
    pub fn on_item(&mut self, rx_ms: i64, item: FeedItem) {
        if let Some(w) = self.open() {
            w.on_item(rx_ms, item);
        }
    }

    /// A book fetched over REST after a mismatch: the book only, the detectors do not see it.
    pub fn on_rest_book(&mut self, token: u8, ts_ms: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
        let Some(w) = self.open() else { return };
        if w.token(token).is_none_or(|t| ts_ms < t.last_ms) {
            return;
        }
        let side = |side: BookSide| move |&(p, size): &(f64, f64)| Some((side, grid_tick(p)?, size));
        let levels = bids.iter().filter_map(side(BookSide::Bid)).chain(asks.iter().filter_map(side(BookSide::Ask))).collect();
        w.push(ts_ms, Body::Book { token, levels, rest: true });
    }

    /// Both copies of the market channel dropped at `at_ms` (exchange clock).
    pub fn on_coverage_lost(&mut self, at_ms: i64) {
        if let Some(w) = self.open() {
            w.push(at_ms, Body::CoverageLost);
        }
    }

    pub fn on_trade(&mut self, t: &Trade) {
        self.closes[ci(t.coin)].add_trade(t.ts_ms, t.price);
    }

    pub fn on_klines(&mut self, coin: Coin, rows: &[(i64, f64)]) {
        self.closes[ci(coin)].add_klines(rows);
    }

    /// Binance seconds `q0..=q1` were lost.
    pub fn on_gap(&mut self, coin: Coin, q0: i64, q1: i64) {
        self.closes[ci(coin)].mark_gap(q0, q1);
    }

    /// A shot of the fast engine: its fill comes as a `ShotFill`; the check it belongs to is sent at
    /// its moment.
    pub fn on_shot(&mut self, shot: &ShotRecord) {
        let timing = self.timing;
        if let Some(w) = self.open() {
            w.on_shot(shot, &timing);
        }
    }

    /// How the demos started from now on are priced.
    pub fn set_timing(&mut self, timing: DemoTiming) {
        self.timing = timing;
    }

    /// The clock is at `now_ms` (exchange clock): release what may go, decide what may be decided.
    pub fn advance(&mut self, now_ms: i64, out: &mut Vec<ExactOut>) {
        self.now_ms = self.now_ms.max(now_ms);
        let now = self.now_ms;
        let Some(w) = self.win.as_mut().filter(|w| !w.done) else { return };
        let newest = w.newest_ms.unwrap_or(i64::MIN);
        w.line_ms = w.line_ms.max(now.min(newest.max(now - QUIET_MS)) - self.hold_ms);
        w.release(&self.timing, out);
        w.decide(&self.strategy, &self.closes, now, out);
        w.finish(&self.strategy, &self.closes, &mut self.scratch, out);
    }
}
