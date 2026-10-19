//! The live buffer of the 1-s closes of one Binance symbol: the port of
//! `paper/closes.py::SecondCloses`, held in a ring of `CAPACITY_S` seconds so that nothing is
//! allocated after `new()`.
//!
//! A trade sets the close of its second; the latest trade (by time) in a second wins. Klines only
//! fill seconds that have no trade of their own, so they repair holes and never overwrite what the
//! stream delivered. `mark_gap()` declares seconds lost: a range that touches one is unknown until
//! trades or klines fill it in. `prune(before)` forgets the seconds before `before` and keeps the
//! price in force at `before`; a second the ring can no longer hold next to the newest one is given
//! up the same way.
//!
//! The sums are taken over the range asked: live, in a sliding window (spec §5.1). Until second q
//! closes, its close is the last trade that has come — the fast z of spec §5.3; `is_final` says
//! when it has closed.

use crate::closes::Closes;

/// The seconds the ring holds: far more than the 1 000-s warm-up and a window z looks at.
pub const CAPACITY_S: i64 = 8192;

/// No second, no trade time.
const NONE: i64 = i64::MIN;

#[derive(Clone, Copy)]
struct Slot {
    /// The second this slot holds; `NONE` if it holds none.
    q: i64,
    /// Its own close; NaN if it has none (a quiet second, or one lost in a gap).
    px: f64,
    /// Time (ms) of the latest trade in it; `NONE` if its close came from a kline, or there is none.
    tts: i64,
    /// Declared lost and not filled yet.
    gap: bool,
}

const VACANT: Slot = Slot { q: NONE, px: f64::NAN, tts: NONE, gap: false };

/// The live closes of one symbol.
pub struct LiveCloses {
    slots: Box<[Slot]>,
    /// The first known second: a range starting earlier is unknown (`paper`'s `_lo`).
    lo: Option<i64>,
    /// Set by `prune()`: older data is forgotten and ignored from then on (`paper`'s `_floor`).
    floor: Option<i64>,
    /// The newest second written (data or gap): with `CAPACITY_S`, what the ring holds.
    hi: Option<i64>,
    /// The latest trade time received, whether it was kept or not.
    latest_trade_ms: Option<i64>,
    /// The latest second a kline row was received for.
    latest_kline_q: Option<i64>,
}

impl Default for LiveCloses {
    fn default() -> Self {
        Self::new()
    }
}

fn check_range(q0: i64, q1: i64) {
    assert!(q0 <= q1, "empty range of seconds: {q0}..{q1}");
}

fn ix(q: i64) -> usize {
    usize::try_from(q.rem_euclid(CAPACITY_S)).expect("a remainder is not negative")
}

impl LiveCloses {
    /// An empty buffer; its ring is allocated here, once.
    pub fn new() -> LiveCloses {
        let n = usize::try_from(CAPACITY_S).expect("a small capacity");
        LiveCloses {
            slots: vec![VACANT; n].into_boxed_slice(),
            lo: None,
            floor: None,
            hi: None,
            latest_trade_ms: None,
            latest_kline_q: None,
        }
    }

    /// The slot of second q, if it holds q.
    fn slot(&self, q: i64) -> Option<&Slot> {
        let s = &self.slots[ix(q)];
        (s.q == q).then_some(s)
    }

    /// The slot of second q for writing; one that held another second starts empty.
    fn slot_mut(&mut self, q: i64) -> &mut Slot {
        let s = &mut self.slots[ix(q)];
        if s.q != q {
            *s = Slot { q, ..VACANT };
        }
        s
    }

    fn live(&self, q: i64) -> bool {
        self.floor.is_none_or(|f| q >= f)
    }

    /// Second q is about to be written. A second newer than all held moves the ring on: what would
    /// not fit next to it is given up as `prune()` gives it up, so the floor never lies more than
    /// `CAPACITY_S` seconds behind the newest second.
    fn advance_to(&mut self, q: i64) {
        if self.hi.is_none_or(|hi| q > hi) {
            self.prune(q - CAPACITY_S + 1);
            self.hi = Some(q);
        }
    }

    fn put(&mut self, q: i64, price: f64) {
        let s = self.slot_mut(q);
        s.px = price;
        s.gap = false;
        if self.lo.is_none_or(|lo| q < lo) {
            self.lo = Some(q);
        }
    }

    /// A trade at time `ts_ms` (unix ms): the last one in its second is that second's close.
    pub fn add_trade(&mut self, ts_ms: i64, price: f64) {
        self.latest_trade_ms = Some(self.latest_trade_ms.map_or(ts_ms, |t| t.max(ts_ms)));
        let q = ts_ms.div_euclid(1000);
        if !self.live(q) || self.slot(q).is_some_and(|s| s.tts != NONE && s.tts > ts_ms) {
            return; // pruned away, or earlier in time than the trade already held
        }
        self.advance_to(q);
        self.slot_mut(q).tts = ts_ms;
        self.put(q, price);
    }

    /// Rows `(open_ms, close)` of one REST response of 1-s klines.
    ///
    /// Binance serves a 1-s kline only for a second with trades, so a second missing between the
    /// first and the last row had no trades and is no longer a hole. Seconds after the last row stay
    /// as they were: if the response is known to reach further, add a closing row for the last second
    /// it covers.
    pub fn add_klines(&mut self, rows: &[(i64, f64)]) {
        let (mut first, mut last) = (i64::MAX, i64::MIN);
        for &(open_ms, close) in rows {
            let q = open_ms.div_euclid(1000);
            first = first.min(q);
            last = last.max(q);
            if self.live(q) && self.slot(q).is_none_or(|s| s.tts == NONE) {
                self.advance_to(q);
                self.put(q, close);
            }
        }
        if first > last {
            return;
        }
        self.latest_kline_q = Some(self.latest_kline_q.map_or(last, |k| k.max(last)));
        let (Some(hi), Some(floor)) = (self.hi, self.floor) else { return };
        for q in first.max(floor)..=last.min(hi) {
            if let Some(s) = self.slots.get_mut(ix(q)).filter(|s| s.q == q) {
                s.gap = false;
            }
        }
    }

    /// Seconds `q0..=q1` are lost: forget what is known of them until trades or klines fill them in.
    pub fn mark_gap(&mut self, q0: i64, q1: i64) {
        if q1 < q0 || !self.live(q1) {
            return;
        }
        self.advance_to(q1);
        let q0 = self.floor.map_or(q0, |f| q0.max(f));
        for q in q0..=q1 {
            let s = self.slot_mut(q);
            s.px = f64::NAN;
            s.tts = NONE;
            s.gap = true;
        }
    }

    /// Forget every second older than `before`. What `get()` says for seconds from `before` on stays.
    pub fn prune(&mut self, before: i64) {
        self.floor = Some(self.floor.map_or(before, |f| f.max(before)));
        let Some(lo) = self.lo else { return };
        if before <= lo {
            return;
        }
        if self.slot(before).is_none_or(|s| s.px.is_nan()) {
            // the price in force at `before` lives on in that second
            if let Some(carried) = self.price_at(before) {
                self.slot_mut(before).px = carried;
                if self.hi.is_none_or(|hi| before > hi) {
                    self.hi = Some(before);
                }
            }
        }
        self.lo = Some(before);
    }

    /// The price in force at second q: its own close, else that of the nearest earlier second.
    /// `None` before the first known second, or when a gap lies in between.
    fn price_at(&self, q: i64) -> Option<f64> {
        let (lo, hi) = (self.lo?, self.hi?);
        let mut q = q.min(hi); // nothing is known after the newest second
        while q >= lo {
            if let Some(s) = self.slot(q) {
                if s.gap {
                    return None;
                }
                if !s.px.is_nan() {
                    return Some(s.px);
                }
            }
            q -= 1;
        }
        None
    }

    /// The close of second `q` on from `cur`, the close of the second before it: its own close if it
    /// has one, else `cur`; `None` for a gap.
    fn step(&self, q: i64, cur: f64) -> Option<f64> {
        match self.slot(q) {
            Some(s) if s.gap => None,
            Some(s) if !s.px.is_nan() => Some(s.px),
            _ => Some(cur),
        }
    }

    /// The closes of seconds `q0..=q1` into `out` (cleared first); a second without trades repeats
    /// the previous price. False if `q0` is before the first known second or the range touches a gap
    /// that is not filled. `q0 > q1` is a bug and panics.
    pub fn get(&self, q0: i64, q1: i64, out: &mut Vec<f64>) -> bool {
        check_range(q0, q1);
        out.clear();
        let Some(mut cur) = self.price_at(q0) else { return false };
        for q in q0..=q1 {
            match self.step(q, cur) {
                Some(c) => cur = c,
                None => {
                    out.clear();
                    return false;
                }
            }
            out.push(cur);
        }
        true
    }

    /// The time of the latest trade received.
    pub fn latest_trade_ms(&self) -> Option<i64> {
        self.latest_trade_ms
    }

    /// Second q has closed (spec §5.3): a trade with a time of `q + 1` s or later has come, or a
    /// kline of a second from q on, or the clock `now_ms` is past `q + 1.5` s.
    pub fn is_final(&self, q: i64, now_ms: i64) -> bool {
        let end_ms = (q + 1) * 1000;
        self.latest_trade_ms.is_some_and(|t| t >= end_ms)
            || self.latest_kline_q.is_some_and(|k| k >= q)
            || now_ms >= end_ms + 1500
    }
}

impl Closes for LiveCloses {
    fn close(&self, q: i64) -> Option<f64> {
        self.price_at(q)
    }

    fn sum_closes(&self, q0: i64, q1: i64) -> Option<f64> {
        check_range(q0, q1);
        let mut cur = self.price_at(q0)?;
        let mut sum = 0.0;
        for q in q0..=q1 {
            cur = self.step(q, cur)?;
            sum += cur;
        }
        Some(sum)
    }

    fn sum_sq_returns(&self, q0: i64, q1: i64) -> Option<f64> {
        check_range(q0, q1);
        let mut prev = self.price_at(q0 - 1)?;
        let mut prev_ln = prev.ln();
        let mut sum = 0.0;
        for q in q0..=q1 {
            let cur = self.step(q, prev)?;
            if cur != prev {
                // a return is a difference of logs, as the backtest takes it
                let cur_ln = cur.ln();
                let r = cur_ln - prev_ln;
                sum += r * r;
                (prev, prev_ln) = (cur, cur_ln);
            }
        }
        Some(sum)
    }
}
