//! Returns and statistics as `ruslan_v2.returns`, `ruslan_v2.stats` and `alib.cl_t` compute them,
//! and the periods of the backtest (`half`).

mod common;

use common::strategy;
use v21_core::returns::{trade_return, win_of};
use v21_core::stats::{cl_t, half_of, stats, Half, TradeRow};
use v21_core::window::Side;

#[test]
fn golden_returns() {
    // rows of ruslan_rule_all_triggers.parquet
    assert_eq!(trade_return(0.84, 1.0, 0.0, 0.07), 0.1772905364677515);
    assert_eq!(trade_return(0.84, 1.0, 0.014, 0.07), 0.15911404184596445);
    assert_eq!(trade_return(0.88, 1.0, 0.0, 0.07), 0.12689769571959175);
    assert_eq!(trade_return(0.88, 1.0, 0.014, 0.07), 0.1103295871257195);
    assert_eq!(trade_return(0.80, 0.0, 0.014, 0.07), -1.0);
    // the price with slippage is capped at 0.99
    assert_eq!(trade_return(0.98, 1.0, 0.014, 0.07), 1.0 / (0.99 * (1.0 + 0.07 * (1.0 - 0.99))) - 1.0);
}

#[test]
fn cl_t_matches_alib() {
    let (mu, t, n) = cl_t(&[0.15, -1.0, 0.2, 0.1, -1.0, 0.3], &[1, 1, 2, 3, 3, 3]);
    assert!((mu - -0.2083333333333333).abs() < 1e-12, "{mu}");
    assert!((t - -2.0975431183552344).abs() < 1e-12, "{t}");
    assert_eq!(n, 6);
    let (mu, t, n) = cl_t(&[0.1, 0.2], &[1, 2]);
    assert!(mu.is_nan() && t.is_nan());
    assert_eq!(n, 2);
    // no spread at all: se is 0 and t is nan, not an infinity
    let (mu, t, _) = cl_t(&[0.5, 0.5, 0.5], &[1, 2, 3]);
    assert_eq!(mu, 0.5);
    assert!(t.is_nan());
}

#[test]
fn win_of_mirrors_down() {
    assert_eq!(win_of(Side::Up, 1.0), 1.0);
    assert_eq!(win_of(Side::Up, 0.0), 0.0);
    assert_eq!(win_of(Side::Down, 1.0), 0.0);
    assert_eq!(win_of(Side::Down, 0.0), 1.0);
}

#[test]
fn periods_follow_the_frozen_dates() {
    let d = strategy().dates;
    let before = |t: i64| t - 840_000; // a window start whose entry is at t
    assert_eq!(half_of(d.research_split_ms - 1, before(d.research_split_ms - 1), &d, false), Half::P1);
    assert_eq!(half_of(d.research_split_ms, before(d.research_split_ms), &d, false), Half::P2);
    assert_eq!(half_of(d.rule_selected_ms - 1, before(d.rule_selected_ms - 1), &d, false), Half::P2);
    assert_eq!(half_of(d.rule_selected_ms, before(d.rule_selected_ms), &d, false), Half::P3);
    assert_eq!(half_of(d.v2_forward_ms + 840_000, d.v2_forward_ms - 1, &d, false), Half::P3);
    assert_eq!(half_of(d.v2_forward_ms + 840_000, d.v2_forward_ms, &d, false), Half::V2Forward);
    assert_eq!(half_of(d.v21_forward_ms + 840_000, d.v21_forward_ms - 1, &d, false), Half::V2Forward);
    assert_eq!(half_of(d.v21_forward_ms + 840_000, d.v21_forward_ms, &d, false), Half::V21Forward);
    assert_eq!(half_of(1_786_500_000_000, 1_786_499_160_000, &d, true), Half::Pre);
}

#[test]
fn half_labels_are_the_backtest_ones() {
    let labels: Vec<&str> =
        [Half::Pre, Half::P1, Half::P2, Half::P3, Half::V2Forward, Half::V21Forward].iter().map(|h| h.label()).collect();
    assert_eq!(
        labels,
        [
            "0) 07.08-18.08 (PRE)",
            "1) 18.08-06.09",
            "2) 07.09-01.10 10:00",
            "3) 01.10 10:00-19:00 (after selection)",
            "4) v2 forward 01.10 19:00-15.10",
            "5) FORWARD v2.1 from 15.10",
        ]
    );
}

fn row(s0_ms: i64, t_trig_ms: i64, px: f64, win: f64) -> TradeRow {
    TradeRow {
        s0_ms,
        entry_ms: s0_ms + t_trig_ms,
        px,
        win,
        ret_top: trade_return(px, win, 0.0, 0.07),
        ret: trade_return(px, win, 0.014, 0.07),
    }
}

#[test]
fn stats_of_a_small_set() {
    let a = 1_790_000_100_000;
    let rows = [row(a, 840_000, 0.80, 1.0), row(a + 900_000, 845_000, 0.85, 0.0), row(a + 2 * 86_400_000, 850_000, 0.75, 1.0)];
    let s = stats(&rows, 0.014, 0.07).unwrap();
    assert_eq!(s.trades, 3);
    let days = (rows[2].entry_ms - rows[0].entry_ms) as f64 / 86_400_000.0;
    assert!((s.per_day - 3.0 / days).abs() < 1e-12);
    assert!((s.win_pct - 200.0 / 3.0).abs() < 1e-12);
    assert!((s.avg_price - 0.8).abs() < 1e-12);
    let be: f64 = rows.iter().map(|r| (r.px + 0.014) * (1.0 + 0.07 * (1.0 - r.px - 0.014))).sum::<f64>() / 3.0;
    assert!((s.breakeven_pct - be * 100.0).abs() < 1e-12);
    let rets: Vec<f64> = rows.iter().map(|r| r.ret).collect();
    let (mu, t, _) = cl_t(&rets, &rows.map(|r| r.s0_ms));
    assert!((s.roi_slip_pct - mu * 100.0).abs() < 1e-12 && (s.t_slip - t).abs() < 1e-12);
    assert!((s.pnl_100 - rets.iter().sum::<f64>() * 100.0).abs() < 1e-12);
}

#[test]
fn stats_of_two_trades_has_no_t_and_of_none_is_none() {
    let a = 1_790_000_100_000;
    let rows = [row(a, 840_000, 0.80, 1.0), row(a, 840_000, 0.80, 0.0)];
    let s = stats(&rows, 0.014, 0.07).unwrap();
    assert!(s.t_top.is_nan() && s.t_slip.is_nan());
    assert!((s.roi_slip_pct - (rows[0].ret + rows[1].ret) / 2.0 * 100.0).abs() < 1e-12);
    assert!((s.per_day - 2.0).abs() < 1e-12); // entries on the same moment: 0 days count as 1
    assert_eq!(stats(&[], 0.014, 0.07), None);
}
