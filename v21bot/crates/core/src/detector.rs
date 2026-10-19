//! The entry trigger of one coin in one window: the port of `paper/strategy.py::CoinDetector`,
//! which is the streaming form of the loop in `ruslan_v2.book_triggers`.
//!
//! The detector is fed with the best bid/ask updates of the coin's Up token:
//! - **the book at t0** is the last update at or before t0. With none, the coin is skipped (done),
//!   as the backtest skips a token with `j0 < 0` — except after a `reset()`. It is band-checked as
//!   of time t0, which the first update (or flush) after t0 does;
//! - **a band check** looks at the book an update leaves, for the updates in (t0, t1]: Up first,
//!   then Down; the update dates the check. While one is waiting, no other starts;
//! - **the fill price** is the ask of the checked side in force `lat` seconds after the check, that
//!   is after the last update at or before `t_trig + lat`; the first update after that moment (or a
//!   flush past it) settles the check;
//! - **settled**: `px <= max_fill` is a success and the coin is done. Otherwise band checks resume
//!   with the first update after the fill moment — the very one that settled it, if an update did.
//!
//! Times are seconds since the window start, `(ts_ms - s0_ms) / 1000`; prices are rounded to three
//! decimals exactly as the backtest's `np.round(x, 3)`.

use crate::numpy::round3;
use crate::params::Params;
use crate::window::{Coin, Side};

/// One check of the entry rule: a side in the band at `t_trig`, and its price `lat` seconds later.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Check {
    pub coin: Coin,
    /// Seconds since the window start of the update that hit the band (t0: the book at t0).
    pub t_trig: f64,
    /// The side whose ask was in the band; Up is looked at first.
    pub side: Side,
    /// The ask of that side at `t_trig`.
    pub ask_at_trig: f64,
    /// The ask of that side `lat` seconds later: the price the order would fill at.
    pub px: f64,
    /// `px <= max_fill`: the order fills. Otherwise the check failed and the watching goes on.
    pub ok: bool,
}

/// The entry trigger of one coin in one window.
#[derive(Clone, Debug)]
pub struct CoinDetector {
    coin: Coin,
    s0_ms: i64,
    lo: f64,
    hi: f64,
    t0: f64,
    t1: f64,
    lat: f64,
    max_fill: f64,
    /// The Up ask in force, rounded; `None` until the first update (and after a reset).
    ua: Option<f64>,
    /// The Down ask in force: 1 − the Up bid, rounded.
    da: Option<f64>,
    /// The check waiting for its fill price: (t_trig, side, ask at t_trig).
    pending: Option<(f64, Side, f64)>,
    /// Nothing more will come from this coin in this window.
    done: bool,
    /// The book at t0 has been looked at.
    started: bool,
    /// `reset()` has been called.
    was_reset: bool,
}

impl CoinDetector {
    pub fn new(p: &Params, coin: Coin, s0_ms: i64) -> CoinDetector {
        CoinDetector {
            coin,
            s0_ms,
            lo: p.lo,
            hi: p.hi,
            t0: p.t0,
            t1: p.t1,
            lat: p.lat,
            max_fill: p.max_fill,
            ua: None,
            da: None,
            pending: None,
            done: false,
            started: false,
            was_reset: false,
        }
    }

    /// Forget the book and the check waiting for its price, e.g. after a reconnect: nothing is known
    /// again until the next update, which is band-checked like any other, however late it comes. So
    /// after a reset a missing book at t0 no longer drops the coin (that rule is for data that never
    /// covered the window). A coin that is done stays done.
    pub fn reset(&mut self) {
        self.ua = None;
        self.da = None;
        self.pending = None;
        self.was_reset = true;
    }

    fn band(&mut self, t: f64) {
        let (Some(ua), Some(da)) = (self.ua, self.da) else { return };
        if self.lo <= ua && ua <= self.hi {
            self.pending = Some((t, Side::Up, ua));
        } else if self.lo <= da && da <= self.hi {
            self.pending = Some((t, Side::Down, da));
        }
    }

    /// The first moment after t0: the book at t0 is band-checked, as of t0 itself.
    fn start_840(&mut self, t: f64) {
        if self.started || t <= self.t0 {
            return;
        }
        self.started = true;
        if self.ua.is_some() {
            self.band(self.t0);
        } else if !self.was_reset {
            self.done = true;
        }
    }

    /// Settle the waiting check if t is past its fill moment, with the book as it stands: the update
    /// that brought t is not applied yet, so the book is the one in force at the fill moment.
    fn resolve(&mut self, t: f64) -> Option<Check> {
        let (t_trig, side, ask_at_trig) = self.pending?;
        if t <= t_trig + self.lat {
            return None;
        }
        let px = match side {
            Side::Up => self.ua,
            Side::Down => self.da,
        }?;
        self.pending = None;
        let ok = px <= self.max_fill;
        if ok {
            self.done = true;
        }
        Some(Check { coin: self.coin, t_trig, side, ask_at_trig, px, ok })
    }

    /// The best bid and ask of the Up token as of server time `ts_ms` (unix ms). Returns the check
    /// this update settles, if any; updates must come in time order.
    pub fn update(&mut self, ts_ms: i64, bid: f64, ask: f64) -> Option<Check> {
        let t = (ts_ms - self.s0_ms) as f64 / 1000.0;
        self.start_840(t);
        let out = self.resolve(t);
        self.ua = Some(round3(ask));
        self.da = Some(round3(1.0 - bid));
        if t > self.t0 && !self.done && self.pending.is_none() && t <= self.t1 {
            self.band(t);
        }
        out
    }

    /// No update came, but time did: do what the clock alone decides. The caller guarantees that
    /// every update up to `now_ms` has been fed. After t1, with no check waiting, the coin is done.
    pub fn flush(&mut self, now_ms: i64) -> Option<Check> {
        let t = (now_ms - self.s0_ms) as f64 / 1000.0;
        self.start_840(t);
        let out = self.resolve(t);
        if t > self.t1 && self.pending.is_none() {
            self.done = true;
        }
        out
    }

    /// The check waiting for its fill price: (t_trig, side, ask at t_trig).
    pub fn pending(&self) -> Option<(f64, Side, f64)> {
        self.pending
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The Up ask in force, rounded.
    pub fn ua(&self) -> Option<f64> {
        self.ua
    }

    /// The Down ask in force (1 − the Up bid), rounded.
    pub fn da(&self) -> Option<f64> {
        self.da
    }
}
