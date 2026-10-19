//! `WindowEngine`, ported with `EngineTests` of `paper/tests/test_strategy.py` (v2 parameters),
//! plus the rules v2.1 changes: the coins that trade and the hours.

mod common;

use common::{strategy, S};
use v21_core::decision::{Decision, Reason, WindowEngine, ENTER, SKIP};
use v21_core::detector::Check;
use v21_core::params::Params;
use v21_core::window::{Coin, Side};
use v21_core::zscore::ZInfo;

/// 2026-10-02 00:00 UTC.
const DAY: i64 = 1_790_899_200;

fn v2() -> Params {
    strategy().v2
}

/// Unix start of the window that opens at hour:minute UTC on 2026-10-02.
fn window_start(hour: i64, minute: i64) -> i64 {
    DAY + hour * 3600 + minute * 60
}

fn up_check(t_trig: f64, coin: Coin) -> Check {
    Check { coin, t_trig, side: Side::Up, ask_at_trig: 0.80, px: 0.80, ok: true }
}

fn failed_check() -> Check {
    Check { coin: Coin::Btc, t_trig: 845.0, side: Side::Up, ask_at_trig: 0.80, px: 0.90, ok: false }
}

fn zi(z: f64) -> ZInfo {
    ZInfo { z, p0: 100.0, ef: 100.1, sigma_bp: 1.0, h: 20.0, cushion_bp: 5.0 }
}

fn reason(d: &Decision, p: &Params) -> String {
    d.reason.label(p)
}

#[test]
fn engine_third_counts_rejected() {
    let p = v2();
    let mut eng = WindowEngine::new(&p, S); // the window of 03:15 UTC
    let got: Vec<Decision> = [(Coin::Btc, 0.5), (Coin::Eth, 2.0), (Coin::Sol, 2.0)]
        .iter()
        .enumerate()
        .map(|(i, &(c, z))| eng.decide(&up_check(845.0 + i as f64, c), Some(&zi(z))))
        .collect();
    assert_eq!(got.iter().map(|d| reason(d, &p)).collect::<Vec<_>>(), ["z < 1.25", "вход", "третья монета"]);
    assert_eq!(got.iter().map(|d| d.rank).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
    assert_eq!(got.iter().map(|d| d.trade).collect::<Vec<_>>(), [false, true, false]);
    assert!(got.iter().all(|d| d.counted && d.hour_ok));
}

#[test]
fn engine_decision_carries_the_check_and_the_z() {
    let p = v2();
    let mut eng = WindowEngine::new(&p, S);
    let chk = Check { coin: Coin::Eth, t_trig: 851.2, side: Side::Down, ask_at_trig: 0.81, px: 0.82, ok: true };
    let z = zi(2.0);
    let got = eng.decide(&chk, Some(&z));
    assert_eq!(
        got,
        Decision { check: chk, counted: true, rank: Some(1), z: Some(z), hour_ok: true, trade: true, reason: Reason::Enter }
    );
}

#[test]
fn engine_hours() {
    // (window start, whether its entry moment, 14 min later, is in an allowed hour of 00-07 or 13-16 UTC)
    let p = v2();
    for ((hour, minute), allowed) in [
        ((6, 45), true),
        ((15, 45), true),
        ((23, 45), false),
        ((7, 0), false),
        ((16, 0), false),
        ((0, 0), true),
        ((12, 45), false),
        ((13, 0), true),
    ] {
        let got = WindowEngine::new(&p, window_start(hour, minute)).decide(&up_check(845.0, Coin::Btc), Some(&zi(2.0)));
        assert_eq!(got.hour_ok, allowed, "{hour:02}:{minute:02}");
        assert_eq!(reason(&got, &p), if allowed { "вход" } else { "вне часов" });
        assert_eq!(got.trade, allowed);
        assert!(got.counted);
        assert_eq!(got.rank, Some(1));
    }
}

#[test]
fn engine_hour_is_the_hour_of_the_entry_moment() {
    // the window of 06:45 is in the hours 00-07, but an entry 15 minutes into it is already 07:00
    let p = v2();
    let mut eng = WindowEngine::new(&p, window_start(6, 45));
    assert_eq!(eng.decide(&up_check(899.0, Coin::Btc), Some(&zi(2.0))).reason, Reason::Enter);
    assert_eq!(eng.decide(&up_check(900.0, Coin::Btc), Some(&zi(2.0))).reason, Reason::OutOfHours);
    assert!(WindowEngine::new(&p, window_start(12, 45)).decide(&up_check(900.0, Coin::Btc), Some(&zi(2.0))).hour_ok);
}

#[test]
fn engine_failed_check_not_counted() {
    let p = v2();
    let mut eng = WindowEngine::new(&p, S);
    let bad = eng.decide(&failed_check(), Some(&zi(2.0)));
    assert!(!bad.counted);
    assert_eq!(bad.rank, None);
    assert!(!bad.trade);
    assert_eq!(reason(&bad, &p), "ask > 0.88 через 0.3 с");
    assert_eq!(eng.decide(&up_check(845.0, Coin::Btc), Some(&zi(2.0))).rank, Some(1));
    assert_eq!(eng.decide(&failed_check(), Some(&zi(2.0))).rank, None);
    assert_eq!(eng.decide(&up_check(845.0, Coin::Btc), Some(&zi(2.0))).rank, Some(2));
}

#[test]
fn engine_no_binance() {
    let p = v2();
    let got = WindowEngine::new(&p, S).decide(&up_check(845.0, Coin::Btc), None);
    assert_eq!((reason(&got, &p).as_str(), got.rank, got.trade, got.counted), ("нет данных Binance", Some(1), false, true));
    assert_eq!(got.z, None);
}

#[test]
fn engine_nan_z_is_no_binance_data() {
    // sigma 0 gives z = nan, which compares false with everything: it must not slip through as a good z
    let p = v2();
    let z = ZInfo { z: f64::NAN, p0: 100.0, ef: 100.0, sigma_bp: 0.0, h: 20.0, cushion_bp: 0.0 };
    let got = WindowEngine::new(&p, S).decide(&up_check(845.0, Coin::Btc), Some(&z));
    assert_eq!((reason(&got, &p).as_str(), got.trade, got.rank), ("нет данных Binance", false, Some(1)));
    assert!(got.z.unwrap().z.is_nan());
}

#[test]
fn engine_z_threshold_is_inclusive() {
    let p = v2();
    assert_eq!(WindowEngine::new(&p, S).decide(&up_check(845.0, Coin::Btc), Some(&zi(1.25))).reason, Reason::Enter);
    assert_eq!(WindowEngine::new(&p, S).decide(&up_check(845.0, Coin::Btc), Some(&zi(1.2499999))).reason, Reason::ZBelow);
    assert_eq!(WindowEngine::new(&p, S).decide(&up_check(845.0, Coin::Btc), Some(&zi(-3.0))).reason, Reason::ZBelow);
}

#[test]
fn engine_reasons_come_in_the_stated_order() {
    // failed check beats everything; then third coin, hours, Binance data, z
    let p = v2();
    let up = up_check(845.0, Coin::Btc);
    let mut eng = WindowEngine::new(&p, window_start(23, 45)); // out of hours
    assert_eq!(eng.decide(&failed_check(), None).reason, Reason::FailedCheck);
    assert_eq!(eng.decide(&up, None).reason, Reason::OutOfHours);
    assert_eq!(eng.decide(&up, Some(&zi(0.1))).reason, Reason::OutOfHours);
    assert_eq!(eng.decide(&up, None).reason, Reason::ThirdCoin);
    let mut eng = WindowEngine::new(&p, S);
    assert_eq!(eng.decide(&up, None).reason, Reason::NoBinance);
    assert_eq!(eng.decide(&up, Some(&zi(0.1))).reason, Reason::ZBelow);
    assert_eq!(eng.decide(&up, Some(&zi(0.1))).reason, Reason::ThirdCoin);
}

#[test]
fn engines_of_two_windows_count_apart() {
    let p = v2();
    let up = up_check(845.0, Coin::Btc);
    let (mut a, mut b) = (WindowEngine::new(&p, S), WindowEngine::new(&p, S + 900));
    assert_eq!(a.decide(&up, Some(&zi(2.0))).rank, Some(1));
    assert_eq!(b.decide(&up, Some(&zi(2.0))).rank, Some(1));
    assert_eq!(a.decide(&up, Some(&zi(2.0))).rank, Some(2));
}

// ---------------------------------------------------------------- v2.1

#[test]
fn v21_sol_is_counted_but_not_traded() {
    let s = strategy();
    let mut eng = WindowEngine::new(&s.v21, S);
    let d = [
        eng.decide(&up_check(845.0, Coin::Sol), Some(&zi(2.0))),
        eng.decide(&up_check(846.0, Coin::Btc), Some(&zi(2.0))),
        eng.decide(&up_check(847.0, Coin::Eth), Some(&zi(2.0))),
    ];
    assert_eq!(d.iter().map(|x| x.reason).collect::<Vec<_>>(), [Reason::CoinNotTraded, Reason::Enter, Reason::ThirdCoin]);
    assert_eq!(d.iter().map(|x| x.rank).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
}

#[test]
fn v21_hours_are_00_07_only() {
    let s = strategy();
    // the window of 13:00: allowed in v2, not in v2.1
    let w = window_start(13, 0);
    assert_eq!(w, 1_790_946_000);
    let btc = up_check(845.0, Coin::Btc);
    assert_eq!(WindowEngine::new(&s.v2, w).decide(&btc, Some(&zi(2.0))).reason, Reason::Enter);
    assert_eq!(WindowEngine::new(&s.v21, w).decide(&btc, Some(&zi(2.0))).reason, Reason::OutOfHours);
    assert_eq!(WindowEngine::new(&s.v21, window_start(6, 45)).decide(&btc, Some(&zi(2.0))).reason, Reason::Enter);
}

#[test]
fn v21_reason_order_puts_the_coin_after_the_hours() {
    let s = strategy();
    let sol = up_check(845.0, Coin::Sol);
    // SOL out of hours: the hours speak first
    assert_eq!(WindowEngine::new(&s.v21, window_start(13, 0)).decide(&sol, None).reason, Reason::OutOfHours);
    // SOL in the hours, without z: the coin speaks before the missing Binance data
    assert_eq!(WindowEngine::new(&s.v21, S).decide(&sol, None).reason, Reason::CoinNotTraded);
    // and v2 trades SOL, so there the missing data is the reason
    assert_eq!(WindowEngine::new(&s.v2, S).decide(&sol, None).reason, Reason::NoBinance);
}

#[test]
fn labels_are_the_exact_strings() {
    let s = strategy();
    let p = &s.v21;
    for (r, want) in [
        (Reason::Enter, "вход"),
        (Reason::FailedCheck, "ask > 0.88 через 0.3 с"),
        (Reason::ThirdCoin, "третья монета"),
        (Reason::OutOfHours, "вне часов"),
        (Reason::CoinNotTraded, "монета не торгуется"),
        (Reason::NoBinance, "нет данных Binance"),
        (Reason::ZBelow, "z < 1.25"),
    ] {
        assert_eq!(r.label(p), want);
    }
    assert_eq!((ENTER, SKIP), ("вход", "пропуск"));
}
