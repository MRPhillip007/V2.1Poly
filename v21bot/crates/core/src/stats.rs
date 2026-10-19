//! The statistics of a set of trades as the backtest reports them (`ruslan_v2.stats`,
//! `alib.cl_t`), and its periods (the `half` column).

use std::collections::BTreeMap;

use crate::params::Dates;

/// Mean, cluster-robust t (clusters `groups`) and n, as `alib.cl_t`; fewer than 3 values give NaN.
pub fn cl_t(x: &[f64], groups: &[i64]) -> (f64, f64, usize) {
    let n = x.len();
    if n < 3 {
        return (f64::NAN, f64::NAN, n);
    }
    let mu = x.iter().sum::<f64>() / n as f64;
    let mut sums: BTreeMap<i64, f64> = BTreeMap::new();
    for (v, g) in x.iter().zip(groups) {
        *sums.entry(*g).or_insert(0.0) += v - mu;
    }
    let se = sums.values().map(|s| s * s).sum::<f64>().sqrt() / n as f64;
    (mu, if se > 0.0 { mu / se } else { f64::NAN }, n)
}

/// One trade as the statistics need it.
#[derive(Clone, Copy, Debug)]
pub struct TradeRow {
    pub s0_ms: i64,
    pub entry_ms: i64,
    pub px: f64,
    pub win: f64,
    /// Return at the best ask, without slippage.
    pub ret_top: f64,
    /// Return with the backtest's slippage.
    pub ret: f64,
}

/// The columns of `ruslan_v2.stats`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats {
    pub trades: usize,
    pub per_day: f64,
    pub win_pct: f64,
    pub avg_price: f64,
    pub breakeven_pct: f64,
    pub roi_top_pct: f64,
    pub t_top: f64,
    pub roi_slip_pct: f64,
    pub t_slip: f64,
    pub pnl_100: f64,
}

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (sum, n) = v.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    sum / n as f64
}

/// The statistics of `rows`; `None` for an empty set.
pub fn stats(rows: &[TradeRow], slip: f64, fee: f64) -> Option<Stats> {
    if rows.is_empty() {
        return None;
    }
    let n = rows.len();
    let first = rows.iter().map(|r| r.entry_ms).min()?;
    let last = rows.iter().map(|r| r.entry_ms).max()?;
    let days = (last - first) as f64 / 86_400_000.0;
    let days = if days == 0.0 { 1.0 } else { days };
    let groups: Vec<i64> = rows.iter().map(|r| r.s0_ms).collect();
    let roi = |ret: &dyn Fn(&TradeRow) -> f64| -> (f64, f64) {
        let x: Vec<f64> = rows.iter().map(ret).collect();
        if n > 2 {
            let (mu, t, _) = cl_t(&x, &groups);
            (mu, t)
        } else {
            (mean(x.into_iter()), f64::NAN)
        }
    };
    let (roi_top, t_top) = roi(&|r| r.ret_top);
    let (roi_slip, t_slip) = roi(&|r| r.ret);
    Some(Stats {
        trades: n,
        per_day: n as f64 / days,
        win_pct: mean(rows.iter().map(|r| r.win)) * 100.0,
        avg_price: mean(rows.iter().map(|r| r.px)),
        breakeven_pct: mean(rows.iter().map(|r| (r.px + slip) * (1.0 + fee * (1.0 - r.px - slip)))) * 100.0,
        roi_top_pct: roi_top * 100.0,
        t_top,
        roi_slip_pct: roi_slip * 100.0,
        t_slip,
        pnl_100: rows.iter().map(|r| r.ret).sum::<f64>() * 100.0,
    })
}

/// The period of a trade: the `half` column of `ruslan_v2.py` as `ruslan_v2_1.py` relabels it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Half {
    Pre,
    P1,
    P2,
    P3,
    V2Forward,
    V21Forward,
}

impl Half {
    pub fn label(self) -> &'static str {
        match self {
            Half::Pre => "0) 07.08-18.08 (PRE)",
            Half::P1 => "1) 18.08-06.09",
            Half::P2 => "2) 07.09-01.10 10:00",
            Half::P3 => "3) 01.10 10:00-19:00 (after selection)",
            Half::V2Forward => "4) v2 forward 01.10 19:00-15.10",
            Half::V21Forward => "5) FORWARD v2.1 from 15.10",
        }
    }
}

/// The period of a trade entered at `entry_ms` in the window `s0_ms`; `pre` for the archive of
/// 07–18.08.
pub fn half_of(entry_ms: i64, s0_ms: i64, d: &Dates, pre: bool) -> Half {
    if pre {
        Half::Pre
    } else if s0_ms >= d.v21_forward_ms {
        Half::V21Forward
    } else if entry_ms < d.research_split_ms {
        Half::P1
    } else if entry_ms < d.rule_selected_ms {
        Half::P2
    } else if s0_ms < d.v2_forward_ms {
        Half::P3
    } else {
        Half::V2Forward
    }
}
