//! The numpy operations whose exact results the backtest depends on.

/// `np.round(x, 3)`: numpy multiplies by 10**3, rounds half to even (`rint`) and divides back,
/// and so does this, operation for operation, so the result is the same double.
pub fn round3(x: f64) -> f64 {
    (x * 1000.0).round_ties_even() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    // values checked against numpy on 2026-10-18
    #[test]
    fn round3_matches_numpy() {
        for (x, want) in [
            (0.0005, 0.0),
            (0.0015, 0.002),
            (0.0025, 0.002),
            (1.0 - 0.81, 0.19),
            (0.6996, 0.7),
            (0.8806, 0.881),
            (0.8804, 0.88),
            (0.6994, 0.699),
            (0.12345, 0.123),
            (0.8885, 0.888),
        ] {
            assert_eq!(round3(x), want, "{x}");
        }
        for k in 0..=1000 {
            assert_eq!(round3(k as f64 / 1000.0), k as f64 / 1000.0);
            assert_eq!(round3(1.0 - k as f64 / 1000.0), (1000 - k) as f64 / 1000.0);
        }
    }
}
