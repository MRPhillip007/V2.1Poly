//! The fast engine (spec §5.3, §6.1): the decision at the start of a check, on the events as they come.
//!
//! - **the detectors** see the rows of their Up tokens in the order they arrive (a row older than the
//!   last one applied of its token is dropped) and the clock of the ticks. The book at 840 s is looked
//!   at on the tick of 840.000, or on the first row after it;
//! - **a check that starts is decided at once**, in the order of the backtest's numbering: the coin's
//!   number from the checks of the other coins (`rank_state`), the fast z on the hot thread's closes,
//!   the rule of v2.1 (`reason_for`), then the state of the bot — the pauses, the market's rules, a
//!   fresh snapshot after a drop of the feed, the limits of the shots;
//! - **a decision waits** while the number is open, or while the coin waits for the answer to its last
//!   shot; a check that closes still waiting is not shot.
//!
//! Nothing is allocated while a window runs (the coins' lists of checks are reserved at `arm`).

use v21_core::decision::{reason_for, Reason};
use v21_core::detector::{Check, CoinDetector};
use v21_core::live_closes::LiveCloses;
use v21_core::params::Params;
use v21_core::ranks::{rank_state, Earlier, RankState};
use v21_core::window::{Coin, Side};
use v21_core::zscore::z_score;

use crate::hot::binance::Trade;
use crate::hot::items::FeedItem;
use crate::shared::msgs::{CoinArm, FastOut, FastReason, FastSkip, ShotRecord, WindowArm};

/// Coins shot per window at most: $40 at risk (spec §6.1).
const MAX_SHOTS: usize = 2;
/// The pauses of the feeds (spec §7.4).
const PM_SILENT: &str = "пауза: лента Polymarket молчит";
const BN_SILENT: &str = "пауза: Binance молчит";
/// Checks of a coin in a window the lists are reserved for.
const CHECKS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// May shoot.
    Open,
    /// Shot on the check of `t_trig`: waits for the answer. `empty` — nothing was bought;
    /// `check_ok` — the check has got its price.
    AwaitFill { t_trig: f64, empty: bool, check_ok: Option<bool> },
    /// Bought, or shot on a check that succeeded: no more shots in the window.
    Done,
}

/// The answer to a shot and the outcome of its check, once both are known (spec §6.1): nothing bought
/// on a failed check opens the coin again; anything else ends it.
fn settled(mode: Mode) -> Mode {
    match mode {
        Mode::AwaitFill { empty: true, check_ok: Some(ok), .. } => {
            if ok {
                Mode::Done
            } else {
                Mode::Open
            }
        }
        m => m,
    }
}

/// The check of a coin that has started and has no price yet.
#[derive(Clone, Copy, Debug)]
struct Cur {
    t_trig: f64,
    side: Side,
    ask: f64,
    /// The exchange time of the row it started on.
    event_ts_ms: i64,
    decided: bool,
    /// A `Wait` has gone out.
    waited: bool,
}

struct FastCoin {
    arm: CoinArm,
    det: CoinDetector,
    /// Has had a successful check.
    ok: bool,
    mode: Mode,
    /// The coin's checks in the window: t_trig, and the outcome once it has its price.
    checks: Vec<(f64, Option<bool>)>,
    cur: Option<Cur>,
    /// The exchange time of the Up row whose prices the detector holds.
    up_ts_ms: i64,
}

struct FastToken {
    /// Its coin: a place in `FastWindow::coins`.
    coin: usize,
    /// The exchange time of its last row applied.
    last_ms: i64,
    /// After a drop of the feed: its changes wait for a fresh snapshot.
    awaiting: bool,
}

/// What the engine keeps across windows.
struct Ctx {
    /// The rule of v2.1.
    p: Params,
    feed_silent_ms: i64,
    /// The hot thread's closes of each coin.
    closes: [LiveCloses; 3],
    /// The first moment seen: a feed silent since then is silent.
    first_ms: Option<i64>,
    last_item_ms: Option<i64>,
    last_trade_ms: Option<i64>,
    /// From the cold side: the clock, the money, the stop.
    pause: Option<&'static str>,
    earlier: Vec<Earlier>,
    closing: Vec<(usize, Cur)>,
}

/// The place of a coin's closes.
fn ci(c: Coin) -> usize {
    match c {
        Coin::Btc => 0,
        Coin::Eth => 1,
        Coin::Sol => 2,
    }
}

impl Ctx {
    /// The pause that stops a shot now, if any.
    fn paused(&self, now_ms: i64) -> Option<&'static str> {
        let silent = |last: Option<i64>| now_ms - last.or(self.first_ms).unwrap_or(now_ms) > self.feed_silent_ms;
        if silent(self.last_item_ms) {
            Some(PM_SILENT)
        } else if silent(self.last_trade_ms) {
            Some(BN_SILENT)
        } else {
            self.pause
        }
    }
}

struct FastWindow {
    s: i64,
    t0_ms: i64,
    /// By token number; `None` for a token of no coin.
    tokens: Vec<Option<FastToken>>,
    coins: Vec<FastCoin>,
    /// Watched coins without a market: each may have had an earlier check.
    missing: usize,
    /// The book at 840 s has been looked at.
    started: bool,
}

fn key_lt(a: (f64, [u8; 32]), b: (f64, [u8; 32])) -> bool {
    a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).is_lt()
}

impl FastWindow {
    fn new(p: &Params, arm: &WindowArm) -> FastWindow {
        let s0 = arm.s * 1000;
        let mut tokens: Vec<Option<FastToken>> = (0..arm.tokens.len()).map(|_| None).collect();
        for (i, c) in arm.coins.iter().enumerate() {
            for ix in [c.up_ix, c.down_ix] {
                if let Some(slot) = tokens.get_mut(usize::from(ix)) {
                    *slot = Some(FastToken { coin: i, last_ms: i64::MIN, awaiting: false });
                }
            }
        }
        let coins = arm
            .coins
            .iter()
            .map(|c| FastCoin {
                arm: c.clone(),
                det: CoinDetector::new(p, c.coin, s0),
                ok: false,
                mode: if arm.traded.contains(&c.coin) { Mode::Done } else { Mode::Open },
                checks: Vec::with_capacity(CHECKS),
                cur: None,
                up_ts_ms: 0,
            })
            .collect();
        let mut w = FastWindow { s: arm.s, t0_ms: s0 + (p.t0 * 1000.0).round() as i64, tokens, coins, missing: arm.missing.len(), started: false };
        if arm.resumed {
            w.lose_coverage(p);
        }
        w
    }

    /// Both copies of the feed dropped: the books are unknown until fresh snapshots come, the checks
    /// running are forgotten.
    fn lose_coverage(&mut self, p: &Params) {
        for t in self.tokens.iter_mut().flatten() {
            t.awaiting = true;
        }
        let s0 = self.s * 1000;
        for c in &mut self.coins {
            if c.det.is_done() && !c.ok {
                // skipped for a missing book at t0: it may yet find one
                c.det = CoinDetector::new(p, c.arm.coin, s0);
            }
            c.det.reset();
            c.cur = None;
            c.checks.retain(|(_, outcome)| outcome.is_some());
            if let Mode::AwaitFill { check_ok, .. } = &mut c.mode
                && check_ok.is_none()
            {
                // the check of the shot is gone: as if it had failed
                *check_ok = Some(false);
                c.mode = settled(c.mode);
            }
        }
    }

    /// The detectors up to `h` by the clock, then the decisions.
    fn step(&mut self, cx: &mut Ctx, h: i64, now_ms: i64, out: &mut Vec<FastOut>) {
        for i in 0..self.coins.len() {
            let chk = self.coins[i].det.flush(h);
            let in_force = self.coins[i].up_ts_ms;
            self.after(cx, i, chk, in_force, None);
        }
        self.close(cx, out);
        self.decide_all(cx, now_ms, out);
    }

    /// A row (token, exchange ms, bid, ask) of the bbo table, in the order it came.
    fn on_bbo(&mut self, cx: &mut Ctx, now_ms: i64, row: (u8, i64, f64, f64), out: &mut Vec<FastOut>) {
        let (token, ts_ms, bid, ask) = row;
        let Some(t) = self.tokens.get_mut(usize::from(token)).and_then(Option::as_mut) else { return };
        if ts_ms < t.last_ms {
            return; // older than the last row applied
        }
        t.last_ms = ts_ms;
        let (i, awaiting) = (t.coin, t.awaiting);
        if awaiting || token != self.coins[i].arm.up_ix {
            return;
        }
        if !self.started && ts_ms > self.t0_ms {
            // no tick since 840 s: the book at 840 s is the one before this row
            self.started = true;
            self.step(cx, self.t0_ms + 1, now_ms, out);
        }
        let c = &mut self.coins[i];
        let in_force = c.up_ts_ms;
        let chk = c.det.update(ts_ms, bid, ask);
        c.up_ts_ms = ts_ms;
        self.after(cx, i, chk, in_force, Some(ts_ms));
        self.close(cx, out);
        self.decide_all(cx, now_ms, out);
    }

    /// After a detector step: a check that got its price is recorded (and goes to `close` if it was
    /// never decided); a check that started becomes the coin's current one. `in_force` is the time of
    /// the Up row whose prices the step used; `this_ts` that of the row of the step, if a row made it.
    fn after(&mut self, cx: &mut Ctx, i: usize, chk: Option<Check>, in_force: i64, this_ts: Option<i64>) {
        let t0 = cx.p.t0;
        // the row that put the price in the band: the book at t0 is the row in force before
        let started_on = |t_trig: f64| match this_ts {
            Some(ts) if t_trig != t0 => ts,
            _ => in_force,
        };
        let c = &mut self.coins[i];
        if let Some(chk) = chk {
            match c.checks.iter_mut().rev().find(|(t, o)| o.is_none() && *t == chk.t_trig) {
                Some(e) => e.1 = Some(chk.ok),
                None => c.checks.push((chk.t_trig, Some(chk.ok))),
            }
            c.ok |= chk.ok;
            if let Mode::AwaitFill { t_trig, check_ok, .. } = &mut c.mode
                && *t_trig == chk.t_trig
            {
                *check_ok = Some(chk.ok);
                c.mode = settled(c.mode);
            }
            match c.cur.take() {
                Some(cur) if !cur.decided => cx.closing.push((i, cur)),
                Some(_) => {}
                // it started and got its price in one step
                None => cx.closing.push((
                    i,
                    Cur { t_trig: chk.t_trig, side: chk.side, ask: chk.ask_at_trig, event_ts_ms: started_on(chk.t_trig), decided: false, waited: false },
                )),
            }
        }
        if c.cur.is_none()
            && let Some((t_trig, side, ask)) = c.det.pending()
        {
            c.cur = Some(Cur { t_trig, side, ask, event_ts_ms: started_on(t_trig), decided: false, waited: false });
            c.checks.push((t_trig, None));
        }
    }

    /// The coin's number for a check of `t_trig`, from the checks of the other coins before it.
    fn rank(&self, cx: &mut Ctx, i: usize, t_trig: f64) -> RankState {
        let key = (t_trig, self.coins[i].arm.up_key);
        cx.earlier.clear();
        for (_, c) in self.coins.iter().enumerate().filter(|(j, _)| *j != i) {
            for &(t, outcome) in &c.checks {
                if key_lt((t, c.arm.up_key), key) {
                    cx.earlier.push(match outcome {
                        None => Earlier::Pending,
                        Some(true) => Earlier::Ok,
                        Some(false) => Earlier::Failed,
                    });
                }
            }
        }
        cx.earlier.extend(std::iter::repeat_n(Earlier::Pending, self.missing));
        rank_state(&cx.earlier, cx.p.max_coins)
    }

    /// The rule of the strategy on a check taken as successful, with its number and the fast z.
    fn rule(&self, cx: &Ctx, i: usize, cur: &Cur, rank: u32) -> (Reason, Option<v21_core::zscore::ZInfo>) {
        let coin = self.coins[i].arm.coin;
        let z = z_score(&cx.closes[ci(coin)], self.s, cur.t_trig, cur.side, &cx.p);
        let chk = Check { coin, t_trig: cur.t_trig, side: cur.side, ask_at_trig: cur.ask, px: cur.ask, ok: true };
        (reason_for(&cx.p, self.s, &chk, Some(rank), z.as_ref()).0, z)
    }

    fn skip(&self, i: usize, cur: &Cur, rank: Option<u32>, z: Option<v21_core::zscore::ZInfo>, reason: FastReason) -> FastOut {
        FastOut::Skip(FastSkip { s: self.s, coin: self.coins[i].arm.coin, side: cur.side, t_trig: cur.t_trig, rank, z, reason })
    }

    /// The checks that got their price before they were decided: not shot.
    fn close(&mut self, cx: &mut Ctx, out: &mut Vec<FastOut>) {
        while let Some(k) = (0..cx.closing.len()).min_by(|&a, &b| {
            let key = |(i, cur): &(usize, Cur)| (cur.t_trig, self.coins[*i].arm.up_key);
            let (x, y) = (key(&cx.closing[a]), key(&cx.closing[b]));
            x.0.total_cmp(&y.0).then(x.1.cmp(&y.1))
        }) {
            let (i, cur) = cx.closing.swap_remove(k);
            let decision = if matches!(self.coins[i].mode, Mode::AwaitFill { .. }) {
                self.skip(i, &cur, None, None, FastReason::ShotLimit)
            } else {
                match self.rank(cx, i, cur.t_trig) {
                    RankState::Rank(n) => match self.rule(cx, i, &cur, n) {
                        (Reason::Enter, z) => self.skip(i, &cur, None, z, FastReason::RankUnknown),
                        (reason, z) => self.skip(i, &cur, Some(n), z, FastReason::Rule(reason)),
                    },
                    RankState::Wait => self.skip(i, &cur, None, None, FastReason::RankUnknown),
                }
            };
            out.push(decision);
        }
    }

    /// The undecided checks, in the order of their keys.
    fn decide_all(&mut self, cx: &mut Ctx, now_ms: i64, out: &mut Vec<FastOut>) {
        let mut done = [false; 3];
        loop {
            let next = (0..self.coins.len())
                .filter(|&i| !done.get(i).copied().unwrap_or(true))
                .filter_map(|i| self.coins[i].cur.filter(|c| !c.decided).map(|c| (i, (c.t_trig, self.coins[i].arm.up_key))))
                .min_by(|a, b| a.1.0.total_cmp(&b.1.0).then(a.1.1.cmp(&b.1.1)));
            let Some((i, _)) = next else { return };
            if let Some(d) = done.get_mut(i) {
                *d = true;
            }
            self.decide(cx, i, now_ms, out);
        }
    }

    fn decide(&mut self, cx: &mut Ctx, i: usize, now_ms: i64, out: &mut Vec<FastOut>) {
        let Some(cur) = self.coins[i].cur else { return };
        let rank = match (self.coins[i].mode, self.rank(cx, i, cur.t_trig)) {
            (Mode::AwaitFill { .. }, _) | (_, RankState::Wait) => {
                // wait for the answer to the last shot, or for the number
                if !cur.waited {
                    out.push(FastOut::Wait { s: self.s, coin: self.coins[i].arm.coin, t_trig: cur.t_trig });
                }
                if let Some(c) = self.coins[i].cur.as_mut() {
                    c.waited = true;
                }
                return;
            }
            (_, RankState::Rank(n)) => n,
        };
        let (reason, z) = self.rule(cx, i, &cur, rank);
        let c = &self.coins[i];
        let bought = match cur.side {
            Side::Up => c.arm.up_ix,
            Side::Down => c.arm.down_ix,
        };
        let engaged = self.coins.iter().filter(|c| c.mode != Mode::Open).count();
        let why = if reason != Reason::Enter {
            Some(FastReason::Rule(reason))
        } else if let Some(pause) = cx.paused(now_ms) {
            Some(FastReason::Paused(pause))
        } else if !c.arm.tradable {
            Some(FastReason::RulesChanged)
        } else if self.tokens.get(usize::from(bought)).and_then(Option::as_ref).is_none_or(|t| t.awaiting) {
            Some(FastReason::StaleBook)
        } else if c.mode == Mode::Done || engaged >= MAX_SHOTS {
            Some(FastReason::ShotLimit)
        } else {
            None
        };
        let decision = match why {
            Some(reason) => self.skip(i, &cur, Some(rank), z, reason),
            None => {
                self.coins[i].mode = Mode::AwaitFill { t_trig: cur.t_trig, empty: false, check_ok: None };
                FastOut::Shot(ShotRecord {
                    s: self.s,
                    coin: self.coins[i].arm.coin,
                    side: cur.side,
                    t_trig: cur.t_trig,
                    event_ts_ms: cur.event_ts_ms,
                    send_ms: now_ms,
                    rank,
                    z,
                    frame_to_decision_ns: 0,
                })
            }
        };
        out.push(decision);
        if let Some(c) = self.coins[i].cur.as_mut() {
            c.decided = true;
        }
    }
}

/// The fast engine of the hot thread: one window at a time, the Binance closes of every coin.
pub struct FastEngine {
    cx: Ctx,
    win: Option<FastWindow>,
}

impl FastEngine {
    /// `p` is the rule of v2.1; a feed silent longer than `feed_silent_ms` pauses the shots.
    pub fn new(p: Params, feed_silent_ms: i64) -> FastEngine {
        FastEngine {
            cx: Ctx {
                p,
                feed_silent_ms,
                closes: std::array::from_fn(|_| LiveCloses::new()),
                first_ms: None,
                last_item_ms: None,
                last_trade_ms: None,
                pause: None,
                earlier: Vec::with_capacity(4 * CHECKS),
                closing: Vec::with_capacity(8),
            },
            win: None,
        }
    }

    /// The window to watch from now on.
    pub fn arm(&mut self, arm: &WindowArm) {
        self.win = Some(FastWindow::new(&self.cx.p, arm));
    }

    /// An item of the market channel, as it came at `now_ms` (exchange clock).
    pub fn on_item(&mut self, now_ms: i64, item: &FeedItem, out: &mut Vec<FastOut>) {
        let FastEngine { cx, win } = self;
        cx.first_ms.get_or_insert(now_ms);
        cx.last_item_ms = Some(now_ms);
        let Some(w) = win else { return };
        match *item {
            FeedItem::Snapshot { token, ts_ms } => {
                if let Some(t) = w.tokens.get_mut(usize::from(token)).and_then(Option::as_mut)
                    && ts_ms >= t.last_ms
                {
                    t.last_ms = ts_ms;
                    t.awaiting = false;
                }
            }
            FeedItem::Bbo { token, ts_ms, bid, ask } => w.on_bbo(cx, now_ms, (token, ts_ms, bid, ask), out),
            _ => {}
        }
    }

    pub fn on_trade(&mut self, now_ms: i64, t: &Trade) {
        self.cx.first_ms.get_or_insert(now_ms);
        self.cx.last_trade_ms = Some(now_ms);
        self.cx.closes[ci(t.coin)].add_trade(t.ts_ms, t.price);
    }

    pub fn on_klines(&mut self, coin: Coin, rows: &[(i64, f64)]) {
        self.cx.closes[ci(coin)].add_klines(rows);
    }

    /// Binance seconds `q0..=q1` were lost.
    pub fn on_gap(&mut self, coin: Coin, q0: i64, q1: i64) {
        self.cx.closes[ci(coin)].mark_gap(q0, q1);
    }

    /// The clock is at `now_ms` (exchange clock): the book at 840 s, the checks that got their price.
    pub fn on_tick(&mut self, now_ms: i64, out: &mut Vec<FastOut>) {
        let FastEngine { cx, win } = self;
        cx.first_ms.get_or_insert(now_ms);
        let Some(w) = win else { return };
        if now_ms < w.t0_ms {
            return;
        }
        let start = w.t0_ms + 1;
        if !w.started {
            w.started = true;
            w.step(cx, start, now_ms, out);
        }
        w.step(cx, now_ms.max(start), now_ms, out);
    }

    /// Both copies of the market channel dropped.
    pub fn on_coverage_lost(&mut self) {
        let FastEngine { cx, win } = self;
        if let Some(w) = win {
            w.lose_coverage(&cx.p);
        }
        cx.closing.clear();
    }

    /// The answer to the coin's last shot: whether anything was bought.
    pub fn on_fill(&mut self, coin: Coin, filled: bool) {
        let Some(c) = self.win.as_mut().and_then(|w| w.coins.iter_mut().find(|c| c.arm.coin == coin)) else { return };
        if let Mode::AwaitFill { empty, .. } = &mut c.mode {
            if filled {
                c.mode = Mode::Done;
            } else {
                *empty = true;
                c.mode = settled(c.mode);
            }
        }
    }

    /// A pause from the cold side (the clock, the money, the stop), or none.
    pub fn set_pause(&mut self, pause: Option<&'static str>) {
        self.cx.pause = pause;
    }
}
