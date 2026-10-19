//! The distance filter of v2: how many standard deviations of the remaining move the current
//! cushion over the price to beat is worth. The port of `paper/strategy.py::z_score`, whose order of
//! operations is that of `ruslan_v2.binance_features` and fixes the bits of the result.

use crate::closes::Closes;
use crate::params::Params;
use crate::window::{Side, TWAP_S, WINDOW_S};

/// The distance filter at a signal, and the quantities it is made of.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZInfo {
    /// Distance in sigmas, signed for the side; NaN when sigma or h is 0.
    pub z: f64,
    /// Binance copy of the price to beat: the mean of the 60 closes before the window start.
    pub p0: f64,
    /// Expected final 60-s average if the price does not move any more.
    pub ef: f64,
    /// 1-s volatility of the last `sig_lb` seconds, basis points.
    pub sigma_bp: f64,
    /// Effective seconds left for the TWAP rule.
    pub h: f64,
    /// `side * ln(ef / p0)`, basis points.
    pub cushion_bp: f64,
}

/// The Binance copy of the price to beat of window s: the mean of the 60 closes before it.
pub fn p0_of(c: &impl Closes, s: i64) -> Option<f64> {
    Some(c.sum_closes(s - TWAP_S - 1, s - 2)? / TWAP_S as f64)
}

/// The distance filter at `t_trig` seconds into window s (unix seconds). The moment is
/// `t = s + floor(t_trig)`: the seconds up to t − 1 have closed by then. `None` if a close it needs
/// is not known.
pub fn z_score(c: &impl Closes, s: i64, t_trig: f64, side: Side, p: &Params) -> Option<ZInfo> {
    let end = s + WINDOW_S;
    let t = s + t_trig.floor() as i64;
    let first = end - TWAP_S - 1; // the first bar of the final minute
    let last = (t - 1).min(end - 2); // the last bar of it that has closed by t
    let known = (last - first + 1).max(0); // how many of its 60 bars are known
    let nf = TWAP_S - known; // and how many are still to come
    let p0 = p0_of(c, s)?;
    let seen = if known > 0 { c.sum_closes(first, last)? } else { 0.0 };
    let price = c.close(t - 1)?;
    let squares = c.sum_sq_returns(t - p.sig_lb, t - 1)?;
    let ef = (seen + nf as f64 * price) / TWAP_S as f64;
    let a = (first - (t - 1)).max(1); // seconds from the last closed bar to the first one unknown
    let h = ((nf * nf * a) as f64 + ((nf - 1) * nf * (2 * nf - 1)) as f64 / 6.0) / (TWAP_S * TWAP_S) as f64;
    let sigma = (squares / p.sig_lb as f64).sqrt();
    let sign = match side {
        Side::Up => 1.0,
        Side::Down => -1.0,
    };
    let cushion = sign * (ef / p0).ln();
    let z = if sigma > 0.0 && h > 0.0 { cushion / (sigma * h.sqrt()) } else { f64::NAN };
    Some(ZInfo { z, p0, ef, sigma_bp: sigma * 1e4, h, cushion_bp: cushion * 1e4 })
}
