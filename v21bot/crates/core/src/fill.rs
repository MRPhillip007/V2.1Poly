//! The fee of the crypto markets and the demo FAK buy (spec §6.2): dollars before the fee spent on
//! the ask levels from the cheapest up to the limit price, the fee on top. `fee` and its rounding are
//! the port of `pmbot/src/shared/fees.rs` (the scanner's `pair.fee`).

use serde::{Deserialize, Serialize};

/// Fee terms of one market, as Gamma's `feeSchedule` gives them: for `shares` contracts bought at
/// price `p` the fee is `shares * rate * (p * (1 - p)) ^ exponent`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeeSchedule {
    pub rate: f64,
    pub exponent: f64,
}

/// Fee in dollars for `shares` contracts bought at `p`, rounded to 5 decimals.
pub fn fee(shares: f64, p: f64, fs: &FeeSchedule) -> f64 {
    round5(shares * fs.rate * (p * (1.0 - p)).powf(fs.exponent))
}

/// Python's `round(x, 5)`: round the exact decimal value of the double `x` at the 5th decimal, ties
/// to even, and read the result back as a double.
///
/// `(x * 1e5).round() / 1e5` is not the same thing. The product is a rounded double itself, so a
/// value a hair below a tie can land exactly on the tie and be rounded up. So when the product lands
/// exactly on a tie, its rounding error is asked for (a fused multiply-add gives it exactly) and the
/// sign of the error says which side of the tie the true value is on.
///
/// Past 2^52 / 1e5 (about 4.5e10, far beyond any fee) `x` is returned as it is.
fn round5(x: f64) -> f64 {
    const SCALE: f64 = 1e5;
    const ALL_INTEGERS: f64 = 4_503_599_627_370_496.0; // 2^52

    let scaled = x * SCALE;
    if !scaled.is_finite() || scaled.abs() >= ALL_INTEGERS {
        return x;
    }
    let floor = scaled.floor();
    let rounded = if scaled - floor == 0.5 {
        // `scaled` is the double nearest to `x * SCALE` and it sits on a tie; the exact product is
        // `scaled + err`.
        let err = x.mul_add(SCALE, -scaled);
        if err > 0.0 {
            floor + 1.0
        } else if err < 0.0 {
            floor
        } else {
            scaled.round_ties_even()
        }
    } else {
        scaled.round()
    };
    rounded / SCALE
}

/// What a FAK buy got.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    /// Contracts bought.
    pub shares: f64,
    /// Dollars paid for them before the fee.
    pub notional_usd: f64,
    /// The fee on top: `fee()` of the contracts taken at each level, summed.
    pub fee_usd: f64,
    /// Volume-weighted price per contract, fee excluded; NaN if nothing was bought.
    pub avg_price: f64,
    /// Ask levels it took contracts from.
    pub levels_used: u32,
    /// The part of the dollars asked that found no ask at or below the limit: cancelled.
    pub unfilled_usd: f64,
}

/// Spend up to `notional_usd` before the fee on the ask levels `(price, size)`, the cheapest first,
/// only at prices `<= limit`. A level gives `min(size, money left / price)` contracts; what cannot be
/// bought is cancelled (FAK). `asks` may come in any order.
pub fn fak_buy(asks: &[(f64, f64)], notional_usd: f64, limit: f64, fs: &FeeSchedule) -> Fill {
    if asks.is_sorted_by(|a, b| a.0 <= b.0) {
        walk(asks, notional_usd, limit, fs)
    } else {
        let mut sorted = asks.to_vec();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        walk(&sorted, notional_usd, limit, fs)
    }
}

fn walk(levels: &[(f64, f64)], notional_usd: f64, limit: f64, fs: &FeeSchedule) -> Fill {
    // the average is taken relative to the cheapest price: one level gives exactly its price
    let reference = levels.first().map_or(0.0, |l| l.0);
    let mut rest = notional_usd;
    let (mut shares, mut fee_usd, mut shift, mut used) = (0.0, 0.0, 0.0, 0u32);
    for &(p, size) in levels {
        if p > limit || rest <= 0.0 {
            break;
        }
        if size <= 0.0 {
            continue;
        }
        let cap = rest / p;
        let (take, spend) = if size < cap {
            (size, size * p) // the level is eaten whole, money is left
        } else {
            (cap, rest) // the money ends here: exactly zero left, no float dust
        };
        rest -= spend;
        shares += take;
        fee_usd += fee(take, p, fs);
        shift += take * (p - reference);
        used += 1;
    }
    Fill {
        shares,
        notional_usd: notional_usd - rest,
        fee_usd,
        avg_price: if shares > 0.0 { reference + shift / shares } else { f64::NAN },
        levels_used: used,
        unfilled_usd: rest,
    }
}

#[cfg(test)]
mod tests {
    //! The tests of `round5` from `pmbot/src/shared/fees.rs`.

    use super::*;

    #[test]
    fn round5_is_pythons_round() {
        // (x, Python's round(x, 5)): exact ties go to the even digit, near ties follow the exact
        // value of the double.
        for (x, want) in [
            (0.015625, 0.01562),
            (0.046875, 0.04688),
            (0.078125, 0.07812),
            (0.017325, 0.01733),
            (0.017325000000000004, 0.01733),
            (5e-6, 1e-5),
            (1.5e-5, 2e-5),
            (2.5e-5, 3e-5),
            (0.000125, 0.00013),
            (0.123455, 0.12345),
            (0.123445, 0.12344),
            (1.7500000000000002, 1.75),
            (1.75, 1.75),
            (0.0, 0.0),
        ] {
            assert_eq!(round5(x), want, "round5({x:e})");
            assert_eq!(round5(-x), -want, "round5(-{x:e})");
        }
        assert!(round5(f64::NAN).is_nan());
        assert_eq!(round5(f64::INFINITY), f64::INFINITY);
    }

    /// What Python's `round(x, 5)` does, spelled out: the exact decimal value of the double,
    /// rounded at the 5th decimal, read back as a double. (Rust prints the exact decimal
    /// expansion, so this is only a trustworthy oracle where the value is not exactly on a tie.)
    fn round5_by_decimal_string(x: f64) -> f64 {
        format!("{x:.5}").parse().unwrap()
    }

    #[test]
    fn round5_agrees_with_decimal_rounding() {
        // Every price tick against many share counts: this is where the fees come from.
        let rate = 0.07;
        let mut checked = 0u32;
        for tick in 1..1000u16 {
            let p = f64::from(tick) / 1000.0;
            for hundredths in (1..3000u32).step_by(7).chain([100, 500, 1000, 3333, 10_000]) {
                let shares = f64::from(hundredths) / 100.0;
                let x = shares * rate * (p * (1.0 - p));
                assert_eq!(round5(x), round5_by_decimal_string(x), "x = {x:e}");
                checked += 1;
            }
        }
        assert!(checked > 400_000);
    }

    #[test]
    fn round5_sends_exact_ties_to_the_even_digit() {
        // m / 64 for odd m is a double that sits exactly on a tie at the 5th decimal:
        // m / 64 * 1e5 = m * 3125 / 2 is an integer plus one half.
        for m in (1..2000u64).step_by(2) {
            let below = m * 3125 / 2;
            let even = if below % 2 == 0 { below } else { below + 1 };
            let x = m as f64 / 64.0;
            assert_eq!(round5(x), even as f64 / 1e5, "m = {m}");
        }
    }
}
