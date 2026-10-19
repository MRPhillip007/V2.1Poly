//! `CoinDetector`, ported with the tests of `paper/tests/test_strategy.py` (DetectorTests,
//! DetectorResetTests, BacktestEquivalenceTests): same inputs, same expected values.

mod common;

use common::{strategy, Rng, S};
use v21_core::detector::{Check, CoinDetector};
use v21_core::numpy::round3;
use v21_core::params::Params;
use v21_core::window::{Coin, Side};
use Side::{Down, Up};

/// A book before t0 far from the band: Up ask 0.50, Down ask 0.55.
const QUIET: (i64, f64, f64) = (830_000, 0.45, 0.50);

fn p() -> Params {
    strategy().v2
}

fn detector() -> CoinDetector {
    CoinDetector::new(&p(), Coin::Btc, 0)
}

fn quiet(det: &mut CoinDetector) {
    det.update(QUIET.0, QUIET.1, QUIET.2);
}

fn check(t_trig: f64, side: Side, ask_at_trig: f64, px: f64, ok: bool) -> Option<Check> {
    Some(Check { coin: Coin::Btc, t_trig, side, ask_at_trig, px, ok })
}

// ---------------------------------------------------------------- DetectorTests

#[test]
fn signal_from_pre840_state() {
    // the book of 839.5 s is still the book at 840 s: the signal is dated t0, found by the next update
    let mut det = detector();
    assert_eq!(det.update(839_500, 0.10, 0.75), None); // Up ask 0.75, Down ask 0.90
    assert_eq!(det.update(840_500, 0.10, 0.70), check(840.0, Up, 0.75, 0.75, true));
    assert!(det.is_done());
}

#[test]
fn down_side_from_up_bid() {
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.update(850_000, 0.19, 0.95), None); // Up ask 0.95 is above the band, Down 0.81
    let got = det.update(851_000, 0.19, 0.95);
    assert_eq!(got, check(850.0, Down, 0.81, 0.81, true));
}

#[test]
fn up_checked_before_down() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.28, 0.75); // both sides in the band: 0.75 and 0.72
    assert_eq!(det.update(851_000, 0.28, 0.75), check(850.0, Up, 0.75, 0.75, true));
}

#[test]
fn failed_check_then_resume() {
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.update(841_000, 0.10, 0.85), None);
    assert_eq!(det.pending(), Some((841.0, Up, 0.85)));
    assert_eq!(det.update(841_200, 0.10, 0.90), None); // 0.2 s after the check: no fill price yet
    // the first update after 841.3 s: the price in force at that moment (0.90) is too high ...
    assert_eq!(det.update(841_400, 0.10, 0.86), check(841.0, Up, 0.85, 0.90, false));
    // ... and the very same update is the first one band-checked again
    assert_eq!(det.pending(), Some((841.4, Up, 0.86)));
    assert!(!det.is_done());
    assert_eq!(det.update(841_800, 0.10, 0.60), check(841.4, Up, 0.86, 0.86, true));
    assert!(det.is_done());
}

#[test]
fn failed_pre840_check_resumes_with_the_first_later_update() {
    let mut det = detector();
    det.update(839_000, 0.10, 0.85); // in the band at 840 s
    assert_eq!(det.update(840_200, 0.10, 0.95), None); // within 0.3 s of t0: still waiting
    assert_eq!(det.update(841_000, 0.10, 0.80), check(840.0, Up, 0.85, 0.95, false));
    assert_eq!(det.pending(), Some((841.0, Up, 0.80)));
    assert_eq!(det.update(842_000, 0.10, 0.80), check(841.0, Up, 0.80, 0.80, true));
}

#[test]
fn px_below_band_accepted() {
    // only the upper limit applies to the fill price: a drop below the band is a better price
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    det.update(850_100, 0.10, 0.55);
    let got = det.update(850_400, 0.10, 0.75);
    assert_eq!(got, check(850.0, Up, 0.75, 0.55, true));
    assert_eq!(got.unwrap().px, 0.55);
}

#[test]
fn fill_price_exactly_at_the_limit_is_accepted() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.80);
    det.update(850_100, 0.10, 0.88);
    assert_eq!(det.update(850_400, 0.10, 0.80), check(850.0, Up, 0.80, 0.88, true));
}

#[test]
fn no_new_signal_after_t1() {
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.update(870_100, 0.10, 0.75), None); // in the band, but 870.1 s is after t1
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(871_000), None);
    assert!(det.is_done());
}

#[test]
fn pending_resolved_by_flush() {
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.update(870_000, 0.10, 0.75), None); // exactly t1 still counts
    assert_eq!(det.pending(), Some((870.0, Up, 0.75)));
    assert_eq!(det.flush(870_300), None); // 870.3 is not after 870.0 + 0.3
    assert!(!det.is_done());
    assert_eq!(det.flush(870_301), check(870.0, Up, 0.75, 0.75, true));
    assert!(det.is_done());
}

#[test]
fn one_signal_per_window() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    assert!(det.update(851_000, 0.10, 0.75).is_some());
    for (ts, bid, ask) in [(852_000, 0.10, 0.80), (853_000, 0.10, 0.70), (860_000, 0.10, 0.85), (869_000, 0.10, 0.75)] {
        assert_eq!(det.update(ts, bid, ask), None);
    }
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(880_000), None);
    assert!(det.is_done());
}

#[test]
fn rounding() {
    let mut det = detector();
    det.update(830_000, 0.81, 0.50);
    assert_eq!(det.da(), Some(0.19)); // 1 - 0.81 is 0.18999999999999995 as a float
    assert_eq!(det.ua(), Some(0.50));
}

#[test]
fn prices_are_rounded_to_three_decimals_exactly() {
    let mut det = detector();
    for k in 0..=1000i64 {
        det.update(830_000 + k, k as f64 / 1000.0, k as f64 / 1000.0);
        assert_eq!(det.da(), Some((1000 - k) as f64 / 1000.0), "{k}");
        assert_eq!(det.ua(), Some(k as f64 / 1000.0), "{k}");
    }
}

#[test]
fn band_is_judged_on_the_rounded_price() {
    // 0.6996 is below 0.70 but rounds to it; 0.8804 rounds to 0.88, 0.8806 to 0.881
    for (ask, want) in [
        (0.6996, Some((850.0, Up, 0.70))),
        (0.8804, Some((850.0, Up, 0.88))),
        (0.8806, None),
        (0.6994, None),
    ] {
        let mut det = detector();
        quiet(&mut det);
        det.update(850_000, 0.00, ask); // bid 0: Down ask 1.0, out of the band
        assert_eq!(det.pending(), want, "{ask}");
    }
}

#[test]
fn no_state_before_t0_skips_the_coin() {
    // the backtest drops a token without any update at or before 840 s (j0 < 0)
    let mut det = detector();
    assert_eq!(det.update(841_000, 0.10, 0.75), None);
    assert!(det.is_done());
    assert_eq!(det.update(842_000, 0.10, 0.75), None);
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(850_000), None);
    let mut silent = detector();
    assert_eq!(silent.flush(841_000), None); // no update at all until after t0
    assert!(silent.is_done());
}

#[test]
fn update_at_t0_belongs_to_the_pre840_state() {
    let mut det = detector();
    quiet(&mut det);
    det.update(840_000, 0.10, 0.75);
    assert_eq!(det.update(841_000, 0.10, 0.50), check(840.0, Up, 0.75, 0.75, true));
    // one millisecond later it is an ordinary update: the book at t0 was the quiet one
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.update(840_001, 0.10, 0.75), None);
    assert_eq!(det.pending(), Some((840.001, Up, 0.75)));
    assert_eq!(det.update(841_000, 0.10, 0.50), check(840.001, Up, 0.75, 0.75, true));
}

#[test]
fn last_of_the_updates_at_t0_is_the_book_at_t0() {
    let mut det = detector();
    quiet(&mut det);
    det.update(840_000, 0.10, 0.75); // in the band ...
    det.update(840_000, 0.10, 0.50); // ... replaced in the same millisecond
    assert_eq!(det.update(841_000, 0.10, 0.50), None);
    assert_eq!(det.pending(), None);
    let mut det = detector();
    quiet(&mut det);
    det.update(840_000, 0.10, 0.50);
    det.update(840_000, 0.10, 0.75);
    assert_eq!(det.update(841_000, 0.10, 0.50), check(840.0, Up, 0.75, 0.75, true));
}

#[test]
fn update_exactly_one_latency_after_the_check_sets_the_fill_price() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    assert_eq!(det.update(850_300, 0.10, 0.60), None); // 850.3 is not after 850.0 + 0.3
    assert_eq!(det.update(851_000, 0.10, 0.75), check(850.0, Up, 0.75, 0.60, true));
    // one millisecond later is too late: it resolves the check itself, with the book that was in force
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    assert_eq!(det.update(850_301, 0.10, 0.60), check(850.0, Up, 0.75, 0.75, true));
}

#[test]
fn updates_with_the_same_time_are_taken_in_order() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.45, 0.50); // same ms: the later one is the book ...
    det.update(850_000, 0.10, 0.75);
    assert_eq!(det.pending(), Some((850.0, Up, 0.75))); // ... but each is band-checked itself
    // and of several updates at the fill moment the last one is the price in force
    det.update(850_300, 0.10, 0.60);
    assert_eq!(det.update(850_300, 0.10, 0.95), None);
    assert_eq!(det.update(851_000, 0.10, 0.75), check(850.0, Up, 0.75, 0.95, false));
}

#[test]
fn updates_inside_the_latency_window_are_not_band_checked() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75); // a check at 850.0
    det.update(850_100, 0.10, 0.80); // in the band, but a check waits
    assert_eq!(det.pending(), Some((850.0, Up, 0.75)));
    det.update(850_300, 0.10, 0.95); // the price in force at 850.3: too high
    // the next update fails the check; the ones in between are skipped, this one is the first checked again
    assert_eq!(det.update(850_400, 0.10, 0.50), check(850.0, Up, 0.75, 0.95, false));
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(880_000), None);
    assert!(det.is_done());
}

#[test]
fn flush_resolves_the_pre840_check() {
    let mut det = detector();
    det.update(839_000, 0.10, 0.75);
    assert_eq!(det.flush(839_900), None); // before t0 nothing is decided
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(840_300), None);
    assert_eq!(det.pending(), Some((840.0, Up, 0.75)));
    assert_eq!(det.flush(840_301), check(840.0, Up, 0.75, 0.75, true));
}

#[test]
fn flush_does_not_finish_the_coin_before_t1_is_over() {
    let mut det = detector();
    quiet(&mut det);
    assert_eq!(det.flush(870_000), None);
    assert!(!det.is_done());
    assert_eq!(det.update(870_000, 0.10, 0.75), None); // an update at t1 itself still counts
    assert_eq!(det.pending(), Some((870.0, Up, 0.75)));
}

#[test]
fn flush_finishes_a_failed_check_after_t1() {
    let mut det = detector();
    quiet(&mut det);
    det.update(869_900, 0.10, 0.75);
    assert_eq!(det.update(870_100, 0.10, 0.95), None); // the price in force at 870.2 will be 0.95
    assert_eq!(det.flush(870_150), None);
    assert!(!det.is_done());
    assert_eq!(det.flush(870_250), check(869.9, Up, 0.75, 0.95, false));
    assert!(det.is_done()); // t1 is over, no check waits: nothing more
}

#[test]
fn times_are_relative_to_the_window_start() {
    let s0_ms = S * 1000;
    let mut det = CoinDetector::new(&p(), Coin::Btc, s0_ms);
    det.update(s0_ms + 830_000, 0.45, 0.50);
    det.update(s0_ms + 850_000, 0.10, 0.75);
    assert_eq!(det.update(s0_ms + 851_000, 0.10, 0.75), check(850.0, Up, 0.75, 0.75, true));
}

#[test]
fn coin_is_reported_on_the_check() {
    let mut det = CoinDetector::new(&p(), Coin::Sol, 0);
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    assert_eq!(det.update(851_000, 0.10, 0.75).unwrap().coin, Coin::Sol);
}

// ---------------------------------------------------------------- DetectorResetTests

#[test]
fn reset_drops_a_check_in_flight() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    det.reset();
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(851_000), None); // the old book cannot decide the price
    assert!(!det.is_done());
    det.update(852_000, 0.10, 0.80); // a fresh book starts a new check
    assert_eq!(det.pending(), Some((852.0, Up, 0.80)));
}

#[test]
fn detection_goes_on_after_a_reset_with_the_fresh_book() {
    let mut det = detector();
    quiet(&mut det);
    det.update(845_000, 0.45, 0.50);
    det.reset();
    assert_eq!(det.flush(855_000), None); // nothing is known until a new book
    assert_eq!(det.pending(), None);
    assert!(!det.is_done());
    assert_eq!(det.update(856_000, 0.10, 0.50), None); // the fresh book is out of the band
    assert_eq!(det.pending(), None);
    det.update(857_000, 0.10, 0.75);
    assert_eq!(det.pending(), Some((857.0, Up, 0.75)));
}

#[test]
fn stale_book_is_not_the_book_at_t0_after_a_reset() {
    let mut det = detector();
    det.update(839_000, 0.10, 0.75); // in the band, then the connection broke
    det.reset();
    assert_eq!(det.update(841_000, 0.10, 0.50), None); // the new book is out of the band
    assert_eq!(det.pending(), None);
    assert_eq!(det.flush(880_000), None);
    assert!(det.is_done()); // t1 is over without a signal
}

#[test]
fn reset_requires_a_fresh_book_and_then_allows_a_signal() {
    // a stale book in the band, a reset at 839 s, no new book until 845 s
    let mut det = detector();
    det.update(720_000, 0.10, 0.80);
    det.reset();
    assert_eq!(det.flush(841_000), None); // the stale book signals nothing ...
    assert_eq!(det.pending(), None);
    assert!(!det.is_done()); // ... and no book at t0 keeps the coin
    assert_eq!(det.update(845_000, 0.10, 0.80), None); // a fresh book in the band starts a check
    assert_eq!(det.pending(), Some((845.0, Up, 0.80)));
    assert_eq!(det.update(846_000, 0.10, 0.80), check(845.0, Up, 0.80, 0.80, true));
}

#[test]
fn fresh_book_after_reset_before_t0_is_the_book_at_t0() {
    let mut det = detector();
    quiet(&mut det);
    det.reset();
    det.update(839_000, 0.10, 0.75);
    assert_eq!(det.update(841_000, 0.10, 0.50), check(840.0, Up, 0.75, 0.75, true));
}

#[test]
fn reset_does_not_revive_a_finished_coin() {
    let mut det = detector();
    quiet(&mut det);
    det.update(850_000, 0.10, 0.75);
    assert!(det.update(851_000, 0.10, 0.75).is_some());
    det.reset();
    assert!(det.is_done());
    assert_eq!(det.update(852_000, 0.10, 0.75), None);
    assert_eq!(det.pending(), None);
}

// ---------------------------------------------------------------- BacktestEquivalenceTests

/// One check of the backtest: (t_trig, side, ask_at_trig, px, ok).
type Rec = (f64, Side, f64, f64, bool);

/// The inner loop of `ruslan_v2.book_triggers`, transcribed on the arrays of one token, as
/// `backtest_checks` in the Python test: the failed checks it steps over are recorded as well.
fn backtest_checks(p: &Params, rel: &[f64], bid: &[f64], ask: &[f64]) -> Vec<Rec> {
    let j0 = rel.partition_point(|&r| r <= p.t0) as i64 - 1;
    if j0 < 0 {
        return vec![];
    }
    let mut sel = vec![j0 as usize];
    sel.extend((0..rel.len()).filter(|&i| rel[i] > p.t0 && rel[i] <= p.t1 + p.lat));
    let mut tt = vec![p.t0];
    tt.extend(sel[1..].iter().map(|&i| rel[i]));
    let ua: Vec<f64> = sel.iter().map(|&i| round3(ask[i])).collect();
    let da: Vec<f64> = sel.iter().map(|&i| round3(1.0 - bid[i])).collect();
    let mut out = vec![];
    let mut k = 0;
    while k < tt.len() && tt[k] <= p.t1 {
        let side = if p.lo <= ua[k] && ua[k] <= p.hi {
            Some(Up)
        } else if p.lo <= da[k] && da[k] <= p.hi {
            Some(Down)
        } else {
            None
        };
        if let Some(side) = side {
            let m = tt.partition_point(|&x| x <= tt[k] + p.lat) - 1;
            let (pf, at) = if side == Up { (ua[m], ua[k]) } else { (da[m], da[k]) };
            if pf <= p.max_fill {
                out.push((tt[k], side, at, pf, true));
                break;
            }
            out.push((tt[k], side, at, pf, false));
            k = m + 1;
            continue;
        }
        k += 1;
    }
    out
}

/// Random best bid/ask updates of one Up token as (ts_ms, bid, ask) in time order, as
/// `random_stream` in the Python test: times crowd around t0, t0 + lat, t1 and t1 + lat; prices
/// around the edges of the band, with four-decimal values that round across them.
fn random_stream(rng: &mut Rng, s0_ms: i64) -> Vec<(i64, f64, f64)> {
    const EDGES: [i64; 12] =
        [839_999, 840_000, 840_001, 840_299, 840_300, 840_301, 869_999, 870_000, 870_001, 870_299, 870_300, 870_301];
    const TICKS: [f64; 24] = [
        0.0, 0.05, 0.1, 0.11, 0.12, 0.1195, 0.1205, 0.2995, 0.3, 0.3005, 0.31, 0.45, 0.5, 0.69, 0.6995, 0.7, 0.7005,
        0.75, 0.8795, 0.88, 0.8805, 0.89, 0.95, 1.0,
    ];
    const IN_BAND: [f64; 7] = [0.6996, 0.7, 0.75, 0.8, 0.85, 0.8804, 0.88];
    const TOO_HIGH: [f64; 5] = [0.8806, 0.89, 0.9, 0.95, 1.0];
    const GAPS: [i64; 9] = [0, 1, 100, 299, 300, 301, 450, 1000, 5000];
    let price = |rng: &mut Rng| {
        if rng.random() < 0.7 { rng.choice(&TICKS) } else { rng.range(0, 10_001) as f64 / 10_000.0 }
    };
    let mut events = vec![];
    if rng.random() < 0.85 {
        // a book before t0; the backtest drops a window without one
        let ts = s0_ms + rng.range(-60_000, 840_001);
        let (b, a) = (price(rng), price(rng));
        events.push((ts, b, a));
    }
    if rng.random() < 0.5 {
        for _ in 0..rng.choice(&[0, 1, 2, 3, 4, 6, 10, 20]) {
            let r = rng.random();
            let ms = if r < 0.3 {
                rng.choice(&EDGES)
            } else if r < 0.8 {
                rng.range(838_000, 872_000)
            } else {
                rng.range(-60_000, 900_000)
            };
            let (b, a) = (price(rng), price(rng));
            events.push((s0_ms + ms, b, a));
        }
    } else {
        let mut ms = rng.range(838_000, 868_000);
        for _ in 0..rng.range(1, 9) {
            let (b, a) = (rng.choice(&TICKS), rng.choice(&IN_BAND));
            events.push((s0_ms + ms, b, a));
            ms += if rng.random() < 0.75 { rng.choice(&[0, 1, 100, 299, 300]) } else { rng.choice(&[301, 450, 1000]) };
            let follow: &[f64] = if rng.random() < 0.75 { &TOO_HIGH } else { &IN_BAND };
            let (b, a) = (rng.choice(&TICKS), rng.choice(follow));
            events.push((s0_ms + ms, b, a));
            ms += rng.choice(&GAPS);
        }
    }
    events.sort_by_key(|e| e.0); // stable: ties keep their order
    events
}

#[test]
fn detector_matches_book_triggers_on_random_streams() {
    let p = p();
    let mut rng = Rng::new(20_261_002);
    let (mut ok, mut ended_failed, mut no_check, mut no_book, mut more) = (0, 0, 0, 0, 0);
    for n in 0..6000 {
        let s0_ms = rng.choice(&[S, S + 900]) * 1000;
        let events = random_stream(&mut rng, s0_ms);
        let rel: Vec<f64> = events.iter().map(|e| (e.0 - s0_ms) as f64 / 1000.0).collect();
        let bid: Vec<f64> = events.iter().map(|e| e.1).collect();
        let ask: Vec<f64> = events.iter().map(|e| e.2).collect();
        let want = backtest_checks(&p, &rel, &bid, &ask);
        let mut det = CoinDetector::new(&p, Coin::Btc, s0_ms);
        let mut got: Vec<Rec> = vec![];
        for &(ts, b, a) in &events {
            if let Some(c) = det.update(ts, b, a) {
                got.push((c.t_trig, c.side, c.ask_at_trig, c.px, c.ok));
            }
        }
        if let Some(c) = det.flush(s0_ms + 1_000_000) {
            got.push((c.t_trig, c.side, c.ask_at_trig, c.px, c.ok));
        }
        assert_eq!(got, want, "stream {n}: {events:?}");
        assert!(det.is_done(), "stream {n}");
        match want.last() {
            None if events.iter().any(|e| e.0 <= s0_ms + 840_000) => no_check += 1,
            None => no_book += 1,
            Some(last) => {
                if last.4 { ok += 1 } else { ended_failed += 1 }
                if want.len() > 1 {
                    more += 1;
                }
            }
        }
    }
    // the generator must keep every kind of stream in play, or the equality above proves little
    let seen = format!("ok {ok}, ended failed {ended_failed}, no check {no_check}, no book {no_book}, more {more}");
    assert!(ok >= 1000 && ended_failed >= 100 && no_check >= 300 && no_book >= 300 && more >= 300, "{seen}");
}
