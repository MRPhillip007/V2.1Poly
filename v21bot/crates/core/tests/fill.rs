//! The fee of the crypto markets and the demo FAK buy (spec §6.2): dollars before the fee spent on
//! the ask levels from the cheapest up to the limit, the fee on top.

use v21_core::fill::{fak_buy, fee, FeeSchedule};

/// The fee terms of the 15m crypto markets.
const FS: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };
/// Another exponent, so that nothing quietly assumes exponent 1.
const FS2: FeeSchedule = FeeSchedule { rate: 0.0625, exponent: 2.0 };

#[test]
fn fee_rounds_like_python() {
    // (shares, price, scanner.pair.fee(shares, price, FeeSchedule(0.07, 1.0))), the set of
    // pmbot/src/shared/fees.rs. The first eight are cases where rounding the float product `x * 1e5`
    // (instead of the value of x itself) ends up one unit of the 5th decimal away from Python.
    for (shares, p, want) in [
        (5.0, 0.03, 0.01018),
        (26.4, 0.125, 0.20212),
        (11.0, 0.15, 0.09817),
        (25.0, 0.21, 0.29032),
        (8.2, 0.25, 0.10762),
        (14.92, 0.25, 0.19582),
        (19.4, 0.25, 0.25462),
        (22.2, 0.25, 0.29137),
        (100.0, 0.5, 1.75),
        (1.0, 0.55, 0.01733),
        (1.0, 0.45, 0.01733),
        (0.01, 0.5, 0.00018),
        (3.0, 0.7, 0.0441),
    ] {
        assert_eq!(fee(shares, p, &FS), want, "fee({shares}, {p})");
    }
}

#[test]
fn fee_uses_the_exponent() {
    // (shares, price, scanner.pair.fee) with FeeSchedule(0.0625, 2.0)
    for (shares, p, want) in [(10.0, 0.5, 0.03906), (33.33, 0.37, 0.11319), (1.0, 0.9, 0.00051)] {
        assert_eq!(fee(shares, p, &FS2), want, "fee({shares}, {p})");
    }
}

#[test]
fn one_level_fills_whole() {
    let f = fak_buy(&[(0.80, 100.0)], 20.0, 0.88, &FS);
    assert_eq!((f.shares, f.notional_usd, f.fee_usd, f.avg_price, f.levels_used, f.unfilled_usd), (25.0, 20.0, 0.28, 0.80, 1, 0.0));
}

#[test]
fn levels_are_walked_cheapest_first_up_to_the_limit() {
    // given out of order; 0.90 is above the limit
    let f = fak_buy(&[(0.85, 10.0), (0.80, 10.0), (0.90, 100.0)], 20.0, 0.88, &FS);
    assert_eq!((f.shares, f.notional_usd, f.unfilled_usd, f.levels_used), (20.0, 16.5, 3.5, 2));
    assert!((f.avg_price - 0.825).abs() < 1e-12, "{}", f.avg_price);
    // the fee is that of each level: fee(10, 0.80) + fee(10, 0.85)
    assert!((f.fee_usd - (0.112 + 0.08925)).abs() < 1e-12, "{}", f.fee_usd);
}

#[test]
fn the_budget_ends_inside_a_level() {
    let f = fak_buy(&[(0.80, 10.0), (0.85, 100.0)], 20.0, 0.88, &FS);
    assert_eq!((f.notional_usd, f.unfilled_usd, f.levels_used), (20.0, 0.0, 2));
    assert!((f.shares - (10.0 + 12.0 / 0.85)).abs() < 1e-12, "{}", f.shares);
}

#[test]
fn a_level_at_the_limit_is_taken() {
    let f = fak_buy(&[(0.88, 5.0)], 20.0, 0.88, &FS);
    assert_eq!((f.shares, f.levels_used), (5.0, 1));
}

#[test]
fn nothing_under_the_limit_is_no_fill() {
    for asks in [&[(0.90, 100.0)][..], &[]] {
        let f = fak_buy(asks, 20.0, 0.88, &FS);
        assert_eq!((f.shares, f.notional_usd, f.fee_usd, f.levels_used, f.unfilled_usd), (0.0, 0.0, 0.0, 0, 20.0));
        assert!(f.avg_price.is_nan());
    }
}
