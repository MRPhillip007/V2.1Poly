//! `LiveCloses`, the live buffer of 1-s Binance closes, ported with `SecondClosesTests` of
//! `paper/tests/test_closes_z.py`; plus its sums and z against `ArrayCloses`, the closed-second rule
//! of the exact z (spec §5.3) and the ring that holds it.

mod common;

use std::collections::BTreeMap;
use std::panic::catch_unwind;

use common::{random_closes, strategy, Rng, S};
use v21_core::closes::{ArrayCloses, Closes};
use v21_core::live_closes::{LiveCloses, CAPACITY_S};
use v21_core::window::Side;
use v21_core::zscore::z_score;

/// `get()` of `paper`: the closes of `q0..=q1`, or `None`.
fn get(c: &LiveCloses, q0: i64, q1: i64) -> Option<Vec<f64>> {
    let mut out = vec![];
    c.get(q0, q1, &mut out).then_some(out)
}

/// A price with two decimals, as `round(rng.uniform(9, 11), 2)`.
fn price(rng: &mut Rng) -> f64 {
    ((9.0 + 2.0 * rng.random()) * 100.0).round() / 100.0
}

// ---------------------------------------------------------------- SecondClosesTests

#[test]
fn forward_fill_quiet_seconds() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_003_700, 11.0);
    assert_eq!(get(&c, 1000, 1003), Some(vec![10.0, 10.0, 10.0, 11.0]));
}

#[test]
fn last_trade_in_second_wins() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_100, 10.0);
    c.add_trade(1_000_900, 12.0);
    assert_eq!(get(&c, 1000, 1000), Some(vec![12.0]));
}

#[test]
fn older_trade_arriving_late_does_not_win() {
    // the last trade is the latest in time, not the latest delivered; a tie goes to the later delivery
    let mut c = LiveCloses::new();
    c.add_trade(1_000_900, 12.0);
    c.add_trade(1_000_200, 11.0);
    assert_eq!(get(&c, 1000, 1000), Some(vec![12.0]));
    c.add_trade(1_000_900, 13.0);
    assert_eq!(get(&c, 1000, 1000), Some(vec![13.0]));
}

#[test]
fn gap_blocks_until_klines() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_003_700, 11.0);
    c.mark_gap(1001, 1002);
    assert_eq!(get(&c, 1000, 1003), None);
    c.add_klines(&[(1_001_000, 10.5), (1_002_000, 10.6)]);
    assert_eq!(get(&c, 1000, 1003), Some(vec![10.0, 10.5, 10.6, 11.0]));
}

#[test]
fn before_first_second_is_none() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    assert_eq!(get(&c, 990, 1000), None);
    // the first second itself is known, and the quiet seconds after it carry its price
    assert_eq!(get(&c, 1000, 1002), Some(vec![10.0, 10.0, 10.0]));
}

#[test]
fn empty_buffer_knows_nothing() {
    let mut c = LiveCloses::new();
    assert_eq!(get(&c, 1000, 1000), None);
    c.prune(1000);
    c.add_klines(&[]);
    c.mark_gap(1000, 1002);
    assert_eq!(get(&c, 1000, 1000), None);
}

#[test]
#[should_panic(expected = "empty range of seconds")]
fn get_rejects_an_inverted_range() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    get(&c, 1001, 1000);
}

#[test]
fn klines_do_not_override_trades() {
    // the trade stream is newer than a REST snapshot, so a kline only fills seconds without a trade
    let mut c = LiveCloses::new();
    c.add_trade(1_000_500, 10.0);
    c.add_klines(&[(1_000_000, 99.0), (1_001_000, 11.0)]);
    assert_eq!(get(&c, 1000, 1001), Some(vec![10.0, 11.0]));
}

#[test]
fn trade_overrides_kline_in_the_same_second() {
    // a kline fetched while its second was still running: the trades that follow are newer
    let mut c = LiveCloses::new();
    c.add_klines(&[(1_000_000, 10.0)]);
    c.add_trade(1_000_700, 10.4);
    assert_eq!(get(&c, 1000, 1000), Some(vec![10.4]));
}

#[test]
fn quiet_seconds_between_kline_rows_are_not_gaps() {
    // Binance serves 1-s klines only for seconds with trades: a second missing inside the response was quiet
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_006_200, 11.0);
    c.mark_gap(1001, 1005);
    c.add_klines(&[(1_001_000, 10.5), (1_004_000, 10.7)]);
    assert_eq!(get(&c, 1001, 1004), Some(vec![10.5, 10.5, 10.5, 10.7]));
}

#[test]
fn gap_after_the_last_kline_row_stays_unknown() {
    // the rows say nothing about seconds after the last one (a long response may have been cut off)
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_006_200, 11.0);
    c.mark_gap(1001, 1005);
    c.add_klines(&[(1_001_000, 10.5), (1_004_000, 10.7)]);
    assert_eq!(get(&c, 1004, 1006), None);
    c.add_klines(&[(1_005_000, 10.7)]); // a closing row for the last covered second
    assert_eq!(get(&c, 1004, 1006), Some(vec![10.7, 10.7, 11.0]));
}

#[test]
fn gap_before_the_first_kline_row_stays_unknown() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_004_200, 11.0);
    c.mark_gap(1001, 1003);
    c.add_klines(&[(1_002_000, 10.5)]);
    assert_eq!(get(&c, 1001, 1001), None);
    assert_eq!(get(&c, 1003, 1003), None);
    assert_eq!(get(&c, 1002, 1002), Some(vec![10.5]));
}

#[test]
fn quiet_second_behind_a_gap_is_unknown() {
    // second 1004 had no trades, but the price it carried was lost with the gap before it
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_005_200, 11.0);
    c.mark_gap(1002, 1003);
    assert_eq!(get(&c, 1004, 1004), None);
    assert_eq!(get(&c, 1005, 1005), Some(vec![11.0]));
}

#[test]
fn trade_fills_a_gap_second() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_003_700, 11.0);
    c.mark_gap(1001, 1002);
    c.add_trade(1_001_500, 10.7);
    assert_eq!(get(&c, 1001, 1001), Some(vec![10.7]));
    assert_eq!(get(&c, 1000, 1003), None); // 1002 is still unknown
}

#[test]
fn mark_gap_forgets_known_seconds() {
    let mut c = LiveCloses::new();
    for q in 1000..1004 {
        c.add_trade(q * 1000 + 100, 10.0 + (q - 1000) as f64);
    }
    c.mark_gap(1001, 1002);
    assert_eq!(get(&c, 1001, 1001), None);
    assert_eq!(get(&c, 1002, 1002), None);
    c.add_klines(&[(1_001_000, 11.5)]);
    assert_eq!(get(&c, 1000, 1001), Some(vec![10.0, 11.5])); // not the old 11.0
    assert_eq!(get(&c, 1002, 1003), None);
    // what the gap made it forget must not come back as a "quiet" second between two later rows
    c.mark_gap(1000, 1003);
    c.add_klines(&[(1_000_000, 20.0), (1_003_000, 23.0)]);
    assert_eq!(get(&c, 1000, 1003), Some(vec![20.0, 20.0, 20.0, 23.0]));
}

#[test]
fn prune_forgets_old_seconds_and_keeps_the_price_in_force() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_010_200, 11.0);
    c.prune(1005);
    assert_eq!(get(&c, 1004, 1006), None);
    assert_eq!(get(&c, 1005, 1011), Some(vec![10.0, 10.0, 10.0, 10.0, 10.0, 11.0, 11.0]));
}

#[test]
fn prune_frees_the_old_seconds() {
    // `paper` counts its dictionaries; the ring holds every second it is asked about, so what is
    // checked is that the seconds before the cut are gone and the ones after it answer as before
    let mut c = LiveCloses::new();
    for q in 1000..3000 {
        c.add_trade(q * 1000 + 100, 10.0);
    }
    c.mark_gap(1500, 1600);
    c.prune(2990);
    assert_eq!(get(&c, 2989, 2990), None);
    assert_eq!(get(&c, 1000, 1000), None);
    assert_eq!(get(&c, 2990, 2999), Some(vec![10.0; 10]));
}

#[test]
fn prune_ignores_data_older_than_the_cut() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_010_200, 11.0);
    c.prune(1005);
    c.add_trade(1_002_000, 99.0);
    assert_eq!(get(&c, 1002, 1002), None);
    c.add_klines(&[(1_003_000, 98.0)]);
    assert_eq!(get(&c, 1003, 1003), None);
    c.prune(1000); // a later cut that is older changes nothing
    c.add_trade(1_004_000, 97.0);
    assert_eq!(get(&c, 1004, 1004), None);
    assert_eq!(get(&c, 1005, 1006), Some(vec![10.0, 10.0]));
    // nothing to remember about seconds that are gone: a gap there blocks nothing after the cut
    c.mark_gap(1001, 1004);
    assert_eq!(get(&c, 1005, 1006), Some(vec![10.0, 10.0]));
}

#[test]
fn prune_cannot_carry_a_price_across_a_gap() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_200, 10.0);
    c.add_trade(1_010_200, 11.0);
    c.mark_gap(1002, 1003);
    assert_eq!(get(&c, 1005, 1005), None);
    c.prune(1005);
    assert_eq!(get(&c, 1005, 1005), None);
    assert_eq!(get(&c, 1010, 1010), Some(vec![11.0]));
}

#[test]
fn prune_does_not_change_answers_for_later_seconds() {
    let mut rng = Rng::new(5);
    let mut c = LiveCloses::new();
    for q in 1000..1400 {
        if rng.random() < 0.6 {
            let ts = q * 1000 + rng.range(0, 1000);
            c.add_trade(ts, price(&mut rng));
        }
    }
    c.mark_gap(1100, 1103);
    c.add_klines(&[(1_100_000, 10.1), (1_102_000, 10.2)]); // 1101 is quiet, 1103 stays unknown
    let cuts: Vec<(i64, i64)> = (1100..1420).step_by(7).flat_map(|q0| [(q0, q0), (q0, q0 + 3), (q0, q0 + 40)]).collect();
    let before: Vec<Option<Vec<f64>>> = cuts.iter().map(|&(q0, q1)| get(&c, q0, q1)).collect();
    c.prune(1100);
    for (&(q0, q1), b) in cuts.iter().zip(&before) {
        assert_eq!(&get(&c, q0, q1), b, "{q0}..{q1}");
    }
    assert!(before.iter().any(Option::is_some) && before.iter().any(Option::is_none));
    assert_eq!(get(&c, 1099, 1100), None);
}

#[test]
fn random_trades_match_a_naive_model() {
    let mut rng = Rng::new(7);
    let mut trades = vec![];
    for q in 1000..1300 {
        if rng.random() < 0.6 {
            // 40% of the seconds are quiet
            for _ in 0..rng.range(1, 5) {
                let ts = q * 1000 + rng.range(0, 1000);
                trades.push((ts, price(&mut rng)));
            }
        }
    }
    // delivered in a random order
    for i in (1..trades.len()).rev() {
        trades.swap(i, rng.below(i + 1));
    }
    let mut c = LiveCloses::new();
    let mut last: BTreeMap<i64, ((i64, usize), f64)> = BTreeMap::new();
    for (n, &(ts, px)) in trades.iter().enumerate() {
        c.add_trade(ts, px);
        let q = ts.div_euclid(1000);
        if last.get(&q).is_none_or(|(key, _)| (ts, n) > *key) {
            last.insert(q, ((ts, n), px));
        }
    }
    let (first, end) = (*last.keys().next().unwrap(), *last.keys().next_back().unwrap());
    for _ in 0..300 {
        let q0 = rng.range(first - 5, end + 11);
        let q1 = q0 + rng.range(0, 31);
        let got = get(&c, q0, q1);
        if q0 < first {
            assert_eq!(got, None, "{q0}..{q1}");
            continue;
        }
        let mut want = vec![];
        let mut cur = None;
        for q in first..=q1 {
            cur = last.get(&q).map(|v| v.1).or(cur);
            if q >= q0 {
                want.push(cur.unwrap());
            }
        }
        assert_eq!(got, Some(want), "{q0}..{q1}");
    }
}

// ---------------------------------------------------------------- against the archive's closes

/// A live buffer of the closes `px` of seconds `first..`: a trade in each second whose close
/// changed (the others are quiet), as `paper`'s `live_buffer` feeds one.
fn live_buffer(px: &[f64], first: i64) -> LiveCloses {
    let mut c = LiveCloses::new();
    for (i, &p) in px.iter().enumerate() {
        if i == 0 || p != px[i - 1] {
            let i = i as i64;
            c.add_trade((first + i) * 1000 + 250 + (i * 37) % 700, p);
        }
    }
    c
}

fn rel_close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs())
}

#[test]
fn sums_agree_with_array_closes() {
    let first = S - 400;
    let px = random_closes(first, S + 900, 3);
    let (live, arch) = (live_buffer(&px, first), ArrayCloses::new(first, px.clone()));
    for (q0, q1) in [(S - 61, S - 2), (S + 839, S + 860), (S + 500, S + 800), (S - 399, S - 100)] {
        let (a, b) = (live.sum_closes(q0, q1).unwrap(), arch.sum_closes(q0, q1).unwrap());
        assert!(rel_close(a, b), "sum_closes {q0}..{q1}: {a} vs {b}");
        let (a, b) = (live.sum_sq_returns(q0, q1).unwrap(), arch.sum_sq_returns(q0, q1).unwrap());
        assert!(rel_close(a, b), "sum_sq_returns {q0}..{q1}: {a} vs {b}");
    }
    assert_eq!(live.sum_closes(S + 5, S + 5), Some(px[405]));
    // unknown ranges: before the data, and a return needs the bar before the first second asked
    assert_eq!(live.sum_closes(S - 500, S - 400), None);
    assert_eq!(live.sum_sq_returns(S - 400, S - 300), None);
    assert!(live.sum_sq_returns(S - 399, S - 300).is_some());
    assert!(catch_unwind(|| live.sum_closes(S, S - 1)).is_err());
    assert!(catch_unwind(|| live.sum_sq_returns(S, S - 1)).is_err());
}

#[test]
fn z_on_live_and_array_closes_agree() {
    let p = strategy().v2;
    let first = S - 1000;
    let px = random_closes(first, S + 900, 11);
    let (live, arch) = (live_buffer(&px, first), ArrayCloses::new(first, px));
    for k in 0..=300 {
        let t_trig = 840.0 + f64::from(k) * 0.1;
        for side in [Side::Up, Side::Down] {
            let (a, b) = (z_score(&live, S, t_trig, side, &p).unwrap(), z_score(&arch, S, t_trig, side, &p).unwrap());
            assert!((a.z - b.z).abs() <= 1e-6, "t_trig {t_trig} {side:?}: z {} vs {}", a.z, b.z);
            assert!(rel_close(a.p0, b.p0) && rel_close(a.ef, b.ef) && rel_close(a.sigma_bp, b.sigma_bp), "t_trig {t_trig}");
            assert_eq!(a.h, b.h, "t_trig {t_trig}");
        }
    }
}

// ---------------------------------------------------------------- the closed second (spec §5.3)

#[test]
fn a_second_is_final_after_a_later_trade_a_kline_or_1_5_s() {
    let mut c = LiveCloses::new();
    c.add_trade(1_000_300, 10.0);
    assert!(!c.is_final(1000, 1_000_400));
    assert!(!c.is_final(1000, 1_002_499));
    assert!(c.is_final(1000, 1_002_500), "the clock is past q + 1.5 s");
    c.add_trade(1_001_000, 10.1); // a trade of the next second closes 1000
    assert!(c.is_final(1000, 1_000_400));
    assert!(!c.is_final(1001, 1_001_100));
    c.add_klines(&[(1_001_000, 10.1)]); // Binance serves a kline only for a second that is over
    assert!(c.is_final(1001, 1_001_100));
    assert!(!c.is_final(1002, 1_001_100));
    assert_eq!(c.latest_trade_ms(), Some(1_001_000));
}

#[test]
fn an_open_second_answers_with_the_last_trade() {
    // the fast z (spec §5.3): until second q closes, its close is the last trade that has come
    let mut c = LiveCloses::new();
    c.add_trade(999_500, 9.0);
    c.add_trade(1_000_300, 10.0);
    assert_eq!(c.close(1000), Some(10.0));
    assert!(!c.is_final(1000, 1_000_900));
    c.add_trade(1_000_800, 10.5);
    assert_eq!(c.close(1000), Some(10.5));
    assert_eq!(c.close(1001), Some(10.5)); // a second without trades yet carries it on
}

// ---------------------------------------------------------------- the ring

#[test]
fn a_wrap_of_the_ring_answers_like_prune() {
    // three turns of the ring, delivered in time order: the seconds still held answer as a buffer
    // without a limit does, and the ones the ring gave up are unknown, as after prune()
    let mut rng = Rng::new(13);
    let first = 1_000_000;
    let end = first + 3 * CAPACITY_S;
    let mut c = LiveCloses::new();
    let mut model: BTreeMap<i64, f64> = BTreeMap::new();
    for q in first..end {
        if q == first || rng.random() < 0.6 {
            let px = price(&mut rng);
            c.add_trade(q * 1000 + rng.range(0, 1000), px);
            model.insert(q, px);
        }
    }
    let forward = |q: i64| *model.range(..=q).next_back().unwrap().1;
    for _ in 0..200 {
        let q0 = rng.range(end - 4000, end);
        let q1 = (q0 + rng.range(0, 300)).min(end - 1);
        let want: Vec<f64> = (q0..=q1).map(forward).collect();
        assert_eq!(get(&c, q0, q1), Some(want), "{q0}..{q1}");
    }
    assert_eq!(get(&c, end - CAPACITY_S - 10, end - CAPACITY_S - 5), None);
}
