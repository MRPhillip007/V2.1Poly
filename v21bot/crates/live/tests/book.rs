//! The book of one token on the 0.001 grid: the tests of `paper/book.py` (`BookTests` of
//! `paper/tests/test_book_fills.py`), and the book against a naive model on random sequences (§10).

use std::collections::BTreeMap;

use proptest::prelude::*;
use v21_live::book::{grid_tick, Book, BookSide};

/// A level given by its price, as an event writes it.
fn set(b: &mut Book, side: BookSide, price: f64, size: f64) {
    b.set(side, grid_tick(price).unwrap(), size);
}

/// A snapshot: the book becomes exactly these levels.
fn snapshot(b: &mut Book, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
    b.clear();
    for &(p, s) in bids {
        set(b, BookSide::Bid, p, s);
    }
    for &(p, s) in asks {
        set(b, BookSide::Ask, p, s);
    }
}

fn asks(b: &Book, limit: f64) -> Vec<(f64, f64)> {
    let mut out = vec![(9.0, 9.0)]; // the call clears it first
    b.asks_upto(limit, &mut out);
    out
}

#[test]
fn books_behave_like_paper_book() {
    // snapshot and change
    let mut b = Book::new();
    snapshot(&mut b, &[(0.48, 30.0), (0.47, 10.0)], &[(0.52, 5.0), (0.55, 7.0)]);
    assert_eq!((b.best_bid(), b.best_ask()), (0.48, 0.52));
    set(&mut b, BookSide::Ask, 0.52, 0.0);
    assert_eq!(b.best_ask(), 0.55);
    set(&mut b, BookSide::Bid, 0.50, 3.0);
    assert_eq!(b.best_bid(), 0.50);
    assert_eq!(asks(&b, 0.60), [(0.55, 7.0)]);

    // a change carries the NEW TOTAL size of the level, not a delta
    let mut b = Book::new();
    snapshot(&mut b, &[(0.48, 30.0)], &[(0.52, 5.0), (0.55, 7.0)]);
    set(&mut b, BookSide::Ask, 0.52, 8.0);
    assert_eq!(asks(&b, 0.60), [(0.52, 8.0), (0.55, 7.0)]);
    set(&mut b, BookSide::Ask, 0.52, 2.0);
    assert_eq!(asks(&b, 0.60), [(0.52, 2.0), (0.55, 7.0)]);
    set(&mut b, BookSide::Bid, 0.48, 12.0);
    set(&mut b, BookSide::Bid, 0.48, 0.0);
    assert_eq!(b.best_bid(), 0.0);

    // a snapshot replaces the whole book; its empty levels are no levels
    let mut b = Book::new();
    snapshot(&mut b, &[(0.48, 30.0)], &[(0.52, 5.0)]);
    snapshot(&mut b, &[(0.40, 9.0), (0.41, 0.0)], &[(0.60, 4.0), (0.59, 0.0)]);
    assert_eq!((b.best_bid(), b.best_ask()), (0.40, 0.60));
    assert_eq!(asks(&b, 0.99), [(0.60, 4.0)]);

    // clear
    b.clear();
    assert_eq!((b.best_bid(), b.best_ask()), (0.0, 1.0));
    assert_eq!(asks(&b, 0.99), []);

    // trim: levels taken by a trade come with no change; the server's best prices show they are gone
    let mut b = Book::new();
    snapshot(&mut b, &[(0.24, 5.0), (0.23, 7.0)], &[(0.25, 4.0), (0.26, 9.0)]);
    b.trim(0.23, 0.26);
    assert_eq!((b.best_bid(), b.best_ask()), (0.23, 0.26));
    assert_eq!(asks(&b, 0.99), [(0.26, 9.0)]);
    // levels at or behind the server's best stay
    b.trim(0.23, 0.26);
    assert_eq!((b.best_bid(), b.best_ask()), (0.23, 0.26));
    // empty server sides (bid 0.0, ask 1.0, the archive's encoding) clear them
    b.trim(0.0, 1.0);
    assert_eq!((b.best_bid(), b.best_ask()), (0.0, 1.0));
}

#[test]
fn grid_tick_rounds_like_the_scanner() {
    // one tick is one level, however noisy its float came out: the scanner rounds to 4 decimals
    assert_eq!(grid_tick(0.1 + 0.2), Some(300));
    assert_eq!(grid_tick(0.3), Some(300));
    assert_eq!(grid_tick(0.52000004), Some(520));
    assert_eq!((grid_tick(0.0), grid_tick(1.0)), (Some(0), Some(1000)));
    for off in [0.5004, 1.5, -0.001, f64::NAN] {
        assert_eq!(grid_tick(off), None, "{off}");
    }
}

#[test]
fn empty_sides_read_as_0_and_1() {
    let b = Book::new();
    assert_eq!((b.best_bid(), b.best_ask()), (0.0, 1.0));
}

#[test]
fn asks_upto_is_cheapest_first_and_stops_at_the_limit() {
    let mut b = Book::new();
    snapshot(&mut b, &[], &[(0.60, 1.0), (0.55, 7.0), (0.52, 5.0), (0.88, 2.0), (0.881, 3.0)]);
    assert_eq!(asks(&b, 0.55), [(0.52, 5.0), (0.55, 7.0)]);
    assert_eq!(asks(&b, 0.51), []);
    assert_eq!(asks(&b, 0.88), [(0.52, 5.0), (0.55, 7.0), (0.60, 1.0), (0.88, 2.0)]);
}

#[test]
fn matches_compares_both_sides_with_the_server() {
    let mut b = Book::new();
    snapshot(&mut b, &[(0.48, 30.0)], &[(0.52, 5.0)]);
    assert!(b.matches(0.48, 0.52));
    assert!(!b.matches(0.48, 0.51), "a level we do not have");
    assert!(!b.matches(0.49, 0.52));
    assert!(Book::new().matches(0.0, 1.0));
    assert!(!b.matches(f64::NAN, 0.52));
}

#[test]
fn a_nan_server_price_trims_its_whole_side() {
    // as `p >= best_ask - 1e-9` is false for NaN in paper
    let mut b = Book::new();
    snapshot(&mut b, &[(0.48, 30.0)], &[(0.52, 5.0)]);
    b.trim(0.48, f64::NAN);
    assert_eq!((b.best_bid(), b.best_ask()), (0.48, 1.0));
    b.trim(f64::NAN, 1.0);
    assert_eq!(b.best_bid(), 0.0);
}

// ---------------------------------------------------------------- against a naive model

#[derive(Clone, Debug)]
enum Op {
    /// (bid side, tick, size) levels of a snapshot.
    Snapshot(Vec<(bool, u16, f64)>),
    Set(bool, u16, f64),
    /// The server's best bid and ask ticks.
    Trim(u16, u16),
}

fn size() -> impl Strategy<Value = f64> {
    prop_oneof![Just(0.0), (1u32..10_000).prop_map(|h| f64::from(h) / 100.0)]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        1 => prop::collection::vec((any::<bool>(), 0u16..=1000, size()), 0..30).prop_map(Op::Snapshot),
        4 => (any::<bool>(), 0u16..=1000, size()).prop_map(|(bid, t, s)| Op::Set(bid, t, s)),
        1 => (0u16..=1000, 0u16..=1000).prop_map(|(b, a)| Op::Trim(b, a)),
    ]
}

fn price(t: u16) -> f64 {
    f64::from(t) / 1000.0
}

proptest! {
    #[test]
    fn book_equals_a_naive_model(ops in prop::collection::vec(op(), 1..60)) {
        let mut book = Book::new();
        let (mut bids, mut asks) = (BTreeMap::<u16, f64>::new(), BTreeMap::<u16, f64>::new());
        let put = |bids: &mut BTreeMap<u16, f64>, asks: &mut BTreeMap<u16, f64>, bid: bool, t: u16, s: f64| {
            let side = if bid { bids } else { asks };
            if s > 0.0 { side.insert(t, s); } else { side.remove(&t); }
        };
        let mut out = vec![];
        for op in ops {
            match op {
                Op::Snapshot(levels) => {
                    book.clear();
                    bids.clear();
                    asks.clear();
                    for (bid, t, s) in levels {
                        book.set(if bid { BookSide::Bid } else { BookSide::Ask }, t, s);
                        put(&mut bids, &mut asks, bid, t, s);
                    }
                }
                Op::Set(bid, t, s) => {
                    book.set(if bid { BookSide::Bid } else { BookSide::Ask }, t, s);
                    put(&mut bids, &mut asks, bid, t, s);
                }
                Op::Trim(b, a) => {
                    book.trim(price(b), price(a));
                    bids.retain(|&t, _| price(t) <= price(b) + 1e-9);
                    asks.retain(|&t, _| price(t) >= price(a) - 1e-9);
                }
            }
            let want_bid = bids.keys().next_back().map_or(0.0, |&t| price(t));
            let want_ask = asks.keys().next().map_or(1.0, |&t| price(t));
            prop_assert_eq!((book.best_bid(), book.best_ask()), (want_bid, want_ask));
            book.asks_upto(0.88, &mut out);
            let want: Vec<(f64, f64)> = asks.iter().filter(|(t, _)| **t <= 880).map(|(&t, &s)| (price(t), s)).collect();
            prop_assert_eq!(&out, &want);
        }
    }
}
