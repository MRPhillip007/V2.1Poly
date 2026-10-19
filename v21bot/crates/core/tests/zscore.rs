//! `ArrayCloses` and `z_score`, with the archive-source tests of `paper/tests/test_closes_z.py`
//! (ArrayClosesTests, ZScoreTests) and the formulas of `STRATEGY_V2.md` §3 as the reference.

mod common;

use common::{random_closes, strategy, S};
use v21_core::closes::{ArrayCloses, Closes};
use v21_core::params::Params;
use v21_core::window::Side;
use v21_core::zscore::{p0_of, z_score, ZInfo};

fn p() -> Params {
    strategy().v21
}

/// The z of `STRATEGY_V2.md` §3 with plain sums over the closes `px` of the seconds from `first`.
fn naive_z(px: &[f64], first: i64, s: i64, t_trig: f64, side: Side, sig_lb: i64) -> ZInfo {
    let c = |q: i64| px[usize::try_from(q - first).unwrap()];
    let p0 = (s - 61..=s - 2).map(c).sum::<f64>() / 60.0;
    let end = s + 900;
    let t = s + t_trig.floor() as i64;
    let first_bar = end - 61;
    let last = (t - 1).min(end - 2);
    let known: Vec<f64> = (first_bar..=last).map(c).collect();
    let nf = 60 - known.len() as i64;
    let price = c(t - 1);
    let ef = (known.iter().sum::<f64>() + nf as f64 * price) / 60.0;
    let sq = (t - sig_lb..t).map(|q| (c(q).ln() - c(q - 1).ln()).powi(2)).sum::<f64>();
    let sigma = (sq / sig_lb as f64).sqrt();
    let a = (first_bar - (t - 1)).max(1);
    let h = ((nf * nf * a) as f64 + ((nf - 1) * nf * (2 * nf - 1)) as f64 / 6.0) / 3600.0;
    let sign = if side == Side::Up { 1.0 } else { -1.0 };
    let cushion = sign * (ef / p0).ln();
    let z = if sigma > 0.0 && h > 0.0 { cushion / (sigma * h.sqrt()) } else { f64::NAN };
    ZInfo { z, p0, ef, sigma_bp: sigma * 1e4, h, cushion_bp: cushion * 1e4 }
}

fn close_enough(got: &ZInfo, want: &ZInfo) {
    for (name, g, w) in [
        ("z", got.z, want.z),
        ("p0", got.p0, want.p0),
        ("ef", got.ef, want.ef),
        ("sigma_bp", got.sigma_bp, want.sigma_bp),
        ("h", got.h, want.h),
        ("cushion_bp", got.cushion_bp, want.cushion_bp),
    ] {
        assert!((g - w).abs() <= 1e-9, "{name}: got {g}, want {w}");
    }
}

fn random_source(seed: u64) -> (Vec<f64>, ArrayCloses) {
    let px = random_closes(S - 400, S + 900, seed);
    let source = ArrayCloses::new(S - 400, px.clone());
    (px, source)
}

fn z_on_random(t_trig: f64) -> ZInfo {
    let (_, source) = random_source(7);
    z_score(&source, S, t_trig, Side::Up, &p()).unwrap()
}

// ---------------------------------------------------------------- ArrayCloses

const T: i64 = 1_790_000_000;

fn ten() -> ArrayCloses {
    ArrayCloses::new(T, (0..10).map(|i| 10.0 + i as f64).collect())
}

#[test]
fn array_closes_close_and_covers() {
    let a = ten();
    assert_eq!((a.t0(), a.len()), (T, 10));
    assert_eq!(a.close(T + 2), Some(12.0));
    assert_eq!(a.close(T), Some(10.0));
    assert_eq!(a.close(T + 9), Some(19.0));
    assert_eq!(a.close(T - 1), None);
    assert_eq!(a.close(T + 10), None);
    assert!(a.covers(T, T + 9));
    assert!(a.covers(T + 3, T + 3));
    assert!(!a.covers(T - 1, T + 9));
    assert!(!a.covers(T, T + 10));
}

#[test]
fn array_closes_sums() {
    let a = ten();
    assert_eq!(a.sum_closes(T + 2, T + 4), Some(39.0));
    assert_eq!(a.sum_closes(T - 1, T + 1), None);
    assert_eq!(a.sum_closes(T + 8, T + 10), None);
    // a return needs the bar before the first second asked
    assert_eq!(a.sum_sq_returns(T, T + 3), None);
    let want: f64 = (1..=3).map(|i| ((10.0 + i as f64).ln() - (9.0 + i as f64).ln()).powi(2)).sum();
    assert!((a.sum_sq_returns(T + 1, T + 3).unwrap() - want).abs() < 1e-15);
}

#[test]
#[should_panic]
fn an_inverted_range_is_a_bug() {
    ten().sum_closes(T + 5, T + 4);
}

#[test]
fn running_sums_equal_the_prefix_arithmetic() {
    // the backtest's sums are differences of running sums over the whole file, to the bit
    let px = random_closes(T, T + 999, 5);
    let a = ArrayCloses::new(T, px.clone());
    let mut cs = vec![0.0];
    let mut c1 = vec![0.0];
    for (i, &c) in px.iter().enumerate() {
        cs.push(cs[i] + c);
        let r = if i == 0 { 0.0 } else { c.ln() - px[i - 1].ln() };
        c1.push(c1[i] + r * r);
    }
    for (q0, q1) in [(T, T + 999), (T + 37, T + 96), (T + 500, T + 500), (T + 1, T + 300)] {
        let (i0, i1) = ((q0 - T) as usize, (q1 - T) as usize);
        assert_eq!(a.sum_closes(q0, q1), Some(cs[i1 + 1] - cs[i0]));
    }
    // a return needs the bar before its second, so these ranges start after the first one
    for (q0, q1) in [(T + 1, T + 999), (T + 37, T + 96), (T + 500, T + 500), (T + 1, T + 300)] {
        let (i0, i1) = ((q0 - T) as usize, (q1 - T) as usize);
        assert_eq!(a.sum_sq_returns(q0, q1), Some(c1[i1 + 1] - c1[i0]));
    }
}

// ---------------------------------------------------------------- z

#[test]
fn h_at_the_golden_moments() {
    // h of ruslan_rule_all_triggers.parquet at t_trig 840.0 and 841.716
    assert_eq!(z_on_random(840.0).h, 19.502777777777776);
    assert_eq!(z_on_random(841.716).h, 18.535833333333333);
}

#[test]
fn z_matches_the_spec_formulas() {
    let (px, source) = random_source(11);
    for t_trig in [840.0, 840.4, 843.7, 855.5, 869.9, 870.0] {
        for side in [Side::Up, Side::Down] {
            let got = z_score(&source, S, t_trig, side, &p()).unwrap();
            close_enough(&got, &naive_z(&px, S - 400, S, t_trig, side, 300));
        }
    }
}

#[test]
fn z_before_the_final_minute() {
    // the backtest also defines h when the whole final minute is still unknown (the factor a)
    let (px, source) = random_source(13);
    for t_trig in [500.0, 700.5, 838.9, 839.0, 839.99, 840.0] {
        let got = z_score(&source, S, t_trig, Side::Up, &p()).unwrap();
        close_enough(&got, &naive_z(&px, S - 400, S, t_trig, Side::Up, 300));
    }
}

#[test]
fn z_down_mirrors_up() {
    let (_, source) = random_source(17);
    let up = z_score(&source, S, 850.0, Side::Up, &p()).unwrap();
    let down = z_score(&source, S, 850.0, Side::Down, &p()).unwrap();
    assert_eq!(down.z, -up.z);
    assert_eq!(down.cushion_bp, -up.cushion_bp);
    assert_eq!((down.p0, down.ef, down.sigma_bp, down.h), (up.p0, up.ef, up.sigma_bp, up.h));
}

#[test]
fn z_uses_whole_seconds_of_t_trig() {
    let (_, source) = random_source(19);
    let z = |t: f64| z_score(&source, S, t, Side::Up, &p());
    assert_eq!(z(843.0), z(843.999));
    assert_ne!(z(843.999), z(844.0));
}

#[test]
fn z_is_none_without_the_prices() {
    let px = random_closes(S - 400, S + 900, 23);
    // history that starts too late for the price to beat, or ends before the last price
    assert_eq!(z_score(&ArrayCloses::new(S - 50, px[350..].to_vec()), S, 850.0, Side::Up, &p()), None);
    assert_eq!(z_score(&ArrayCloses::new(S - 400, px[..700].to_vec()), S, 850.0, Side::Up, &p()), None);
    assert!(p0_of(&ArrayCloses::new(S - 50, px[350..].to_vec()), S).is_none());
    assert!(p0_of(&ArrayCloses::new(S - 400, px.clone()), S).is_some());
}

#[test]
fn z_is_nan_when_sigma_is_zero() {
    let flat = ArrayCloses::new(S - 400, vec![100.0; 1301]);
    for side in [Side::Up, Side::Down] {
        let zi = z_score(&flat, S, 850.0, side, &p()).unwrap();
        assert!(zi.z.is_nan());
        assert_eq!((zi.sigma_bp, zi.cushion_bp, zi.p0, zi.ef), (0.0, 0.0, 100.0, 100.0));
        assert!(zi.h > 0.0);
    }
}

#[test]
fn z_is_nan_not_inf_when_sigma_is_zero_but_the_price_moved() {
    // flat over the sigma window, but the price to beat was lower: a cushion with nothing to measure it by
    let px: Vec<f64> = (0..1301).map(|i| if S - 400 + i < S + 500 { 100.0 } else { 101.0 }).collect();
    let zi = z_score(&ArrayCloses::new(S - 400, px), S, 850.0, Side::Up, &p()).unwrap();
    assert_eq!(zi.sigma_bp, 0.0);
    assert!(zi.cushion_bp > 0.0);
    assert!(zi.z.is_nan());
}

#[test]
fn z_after_the_final_minute_is_over() {
    // nothing is left to estimate: ef is the final average itself, h is 0 and z is nan, not an infinity
    let px = random_closes(S - 400, S + 1000, 37);
    let zi = z_score(&ArrayCloses::new(S - 400, px.clone()), S, 905.0, Side::Up, &p()).unwrap();
    assert_eq!(zi.h, 0.0);
    assert!(zi.z.is_nan());
    let fin = (S + 839..S + 899).map(|q| px[(q - (S - 400)) as usize]).sum::<f64>() / 60.0;
    assert!((zi.ef - fin).abs() < 1e-9);
}

#[test]
fn p0_is_the_mean_of_the_minute_before_the_start() {
    let px = random_closes(S - 400, S + 900, 41);
    let want = (S - 61..=S - 2).map(|q| px[(q - (S - 400)) as usize]).sum::<f64>() / 60.0;
    let got = p0_of(&ArrayCloses::new(S - 400, px), S).unwrap();
    assert!((got - want).abs() < 1e-12);
}
