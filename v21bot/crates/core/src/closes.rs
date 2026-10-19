//! Per-second closes of a Binance symbol, as `z_score` reads them.
//!
//! `close[q]` is the price of the last trade with time in `[q, q+1)` s: the close of the 1-s bar
//! opened at second q, so the price at the moment q+1. A second without trades repeats the previous
//! price. Ranges of seconds are `q0..=q1`, unix seconds.

/// The three questions `z_score` asks about the closes of one symbol.
pub trait Closes {
    /// The close of second q, if known.
    fn close(&self, q: i64) -> Option<f64>;
    /// The sum of the closes of `q0..=q1`, if all are known. `q0 > q1` is a bug and panics.
    fn sum_closes(&self, q0: i64, q1: i64) -> Option<f64>;
    /// The sum over `q0..=q1` of `(ln close[q] - ln close[q-1])²`; the bar `q0 - 1` must be known
    /// as well. `q0 > q1` is a bug and panics.
    fn sum_sq_returns(&self, q0: i64, q1: i64) -> Option<f64>;
}

/// The archived closes of the backtest (`data/bn1s`): `close[i]` is the bar opened at second
/// `t0 + i`.
///
/// Its sums reproduce the backtest digit for digit: the backtest takes every window sum as a
/// difference of running sums over the whole file, built by sequential addition (`np.cumsum`).
/// That carries rounding noise of about 1e-11 relative, and far more on z where sigma is tiny, so a
/// sum over the window alone would differ from it by more than the 1e-9 the replay must agree to.
#[derive(Clone, Debug)]
pub struct ArrayCloses {
    t0: i64,
    c: Vec<f64>,
    /// `np.r_[0, np.cumsum(c)]`.
    cs: Vec<f64>,
    /// `np.r_[0, np.cumsum(np.r_[0, np.diff(np.log(c))] ** 2)]`.
    c1: Vec<f64>,
}

impl ArrayCloses {
    pub fn new(t0: i64, closes: Vec<f64>) -> ArrayCloses {
        let n = closes.len();
        let mut cs = Vec::with_capacity(n + 1);
        cs.push(0.0);
        for (i, &c) in closes.iter().enumerate() {
            cs.push(cs[i] + c);
        }
        let logs: Vec<f64> = closes.iter().map(|c| c.ln()).collect();
        let mut c1 = Vec::with_capacity(n + 1);
        c1.push(0.0);
        for i in 0..n {
            let r = if i == 0 { 0.0 } else { logs[i] - logs[i - 1] };
            c1.push(c1[i] + r * r);
        }
        ArrayCloses { t0, c: closes, cs, c1 }
    }

    /// True if every second `q0..=q1` is in the file.
    pub fn covers(&self, q0: i64, q1: i64) -> bool {
        self.t0 <= q0 && q0 <= q1 && q1 < self.t0 + self.c.len() as i64
    }

    /// The first second of the file.
    pub fn t0(&self) -> i64 {
        self.t0
    }

    /// The number of seconds in the file.
    pub fn len(&self) -> usize {
        self.c.len()
    }

    pub fn is_empty(&self) -> bool {
        self.c.is_empty()
    }

    fn ix(&self, q: i64) -> usize {
        usize::try_from(q - self.t0).expect("a covered second is not before t0")
    }
}

fn check_range(q0: i64, q1: i64) {
    assert!(q0 <= q1, "empty range of seconds: {q0}..{q1}");
}

impl Closes for ArrayCloses {
    fn close(&self, q: i64) -> Option<f64> {
        self.covers(q, q).then(|| self.c[self.ix(q)])
    }

    fn sum_closes(&self, q0: i64, q1: i64) -> Option<f64> {
        check_range(q0, q1);
        self.covers(q0, q1).then(|| self.cs[self.ix(q1) + 1] - self.cs[self.ix(q0)])
    }

    fn sum_sq_returns(&self, q0: i64, q1: i64) -> Option<f64> {
        check_range(q0, q1);
        self.covers(q0 - 1, q1).then(|| self.c1[self.ix(q1) + 1] - self.c1[self.ix(q0)])
    }
}
