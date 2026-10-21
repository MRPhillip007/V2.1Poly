//! The exact engine (spec §5.3, §6.2): events put back in the exchange's time order, the decisions of
//! the backtest in its numbering, the demo fills at the matching moment and on the grid, the book check,
//! a drop of both copies — and a whole archive day through it against the replay of milestone 1.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use v21_core::closes::{ArrayCloses, Closes};
use v21_core::decision::Reason;
use v21_core::fill::FeeSchedule;
use v21_core::params::{load_strategy, Strategy};
use v21_core::window::{Coin, Side};
use v21_live::book::BookSide;
use v21_live::cold::exact::{DemoTiming, ExactEngine, ExactOut, ExactSignal, ShotFill, WindowSummary, GRID_MS};
use v21_live::hot::binance::Trade;
use v21_live::hot::items::{read_frame, FeedItem};
use v21_live::hot::wire::Frame;
use v21_live::shared::msgs::{CoinArm, ShotRecord, WindowArm};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC: in the hours of v2.1.
const S: i64 = 1_790_910_900;
const S0: i64 = S * 1000;
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };

fn strategy() -> Strategy {
    load_strategy(V2, V21).unwrap()
}

fn timing() -> DemoTiming {
    DemoTiming { stake_usd: 20.0, limit: 0.88, half_rtt_ms: 0.0, taker_delay_ms: 250.0 }
}

/// Tokens 0..6: BTC Up/Down, ETH Up/Down, SOL Up/Down. ETH's Up id is the smallest, then BTC's, then
/// SOL's: at one t_trig ETH comes first.
fn arm(s: i64, resumed: bool) -> WindowArm {
    let coin = |coin, up: u8, key: u8| CoinArm { coin, up_ix: up, down_ix: up + 1, up_key: [key; 32], fee: FEE, tradable: true, note: String::new() };
    WindowArm {
        s,
        tokens: (0..6).map(|i| format!("t{i}")).collect(),
        coins: vec![coin(Coin::Btc, 0, 2), coin(Coin::Eth, 2, 1), coin(Coin::Sol, 4, 3)],
        missing: vec![],
        resumed,
        traded: vec![],
    }
}

/// 1-s closes with movement, so that z is a number.
fn klines(first: i64, last: i64) -> Vec<(i64, f64)> {
    (first..=last).map(|q| (q * 1000, 100.0 + ((q % 7) as f64) * 0.01)).collect()
}

fn engine_with(hold_ms: i64, closes_until: i64) -> ExactEngine {
    let mut e = ExactEngine::new(strategy(), hold_ms, timing());
    for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
        e.on_klines(coin, &klines(S - 1000, closes_until));
    }
    e.arm(&arm(S, false));
    e
}

fn engine() -> ExactEngine {
    engine_with(500, S + 900)
}

/// An item at `t` ms into the window, arriving at the same moment.
fn at(e: &mut ExactEngine, t: i64, item: FeedItem) {
    e.on_item(S0 + t, item);
}

fn bbo(token: u8, t: i64, bid: f64, ask: f64) -> FeedItem {
    FeedItem::Bbo { token, ts_ms: S0 + t, bid, ask }
}

/// A snapshot: 50 shares at the bid (if any) and the ask levels, then its row.
fn snapshot(e: &mut ExactEngine, token: u8, t: i64, asks: &[(u16, f64)], bid: f64, ask: f64) {
    at(e, t, FeedItem::Snapshot { token, ts_ms: S0 + t });
    if bid > 0.0 {
        at(e, t, FeedItem::SnapLevel { token, side: BookSide::Bid, tick: (bid * 1000.0).round() as u16, size: 50.0 });
    }
    for &(tick, size) in asks {
        at(e, t, FeedItem::SnapLevel { token, side: BookSide::Ask, tick, size });
    }
    at(e, t, bbo(token, t, bid, ask));
}

fn ask_change(token: u8, t: i64, tick: u16, size: f64, best_bid: f64, best_ask: f64) -> FeedItem {
    FeedItem::Level { token, ts_ms: S0 + t, side: BookSide::Ask, tick: Some(tick), size, best_bid, best_ask }
}

fn run(e: &mut ExactEngine, now: i64) -> Vec<ExactOut> {
    let mut out = vec![];
    e.advance(S0 + now, &mut out);
    out
}

fn signals(out: &[ExactOut]) -> Vec<&ExactSignal> {
    out.iter().filter_map(|o| if let ExactOut::Signal(s) = o { Some(s) } else { None }).collect()
}

fn summary(out: &[ExactOut]) -> Option<&WindowSummary> {
    out.iter().find_map(|o| if let ExactOut::WindowDone(w) = o { Some(w) } else { None })
}

// ---------------------------------------------------------------- the demo fill

#[test]
fn demo_fill_uses_the_book_at_the_match_moment() {
    let mut e = engine();
    // the BTC Up book at 839 s: 100 at 0.80, in the band from 840 s on
    snapshot(&mut e, 0, 839_000, &[(800, 100.0)], 0.18, 0.80);
    // at 840.100 s a trade takes the 0.80 level and 50 appear at 0.85
    at(&mut e, 840_100, ask_change(0, 840_100, 800, 0.0, 0.18, 0.85));
    at(&mut e, 840_100, bbo(0, 840_100, 0.18, 0.85));
    at(&mut e, 840_100, ask_change(0, 840_100, 850, 50.0, 0.18, 0.85));
    at(&mut e, 840_100, bbo(0, 840_100, 0.18, 0.85));
    let out = run(&mut e, 850_000);
    let s = signals(&out);
    assert_eq!(s.len(), 1, "{out:?}");
    let c = &s[0].check;
    assert_eq!((c.coin, c.t_trig, c.side, c.px, c.ok), (Coin::Btc, 840.0, Side::Up, 0.85, true));
    assert_eq!(s[0].send_ms, S0 + 840_000);
    let demo = s[0].demo.as_ref().unwrap();
    assert_eq!(demo.match_ms, S0 + 840_250);
    assert_eq!((demo.no_latency.avg_price, demo.no_latency.shares), (0.80, 25.0), "the book at the band hit");
    assert_eq!((demo.at_match.avg_price, demo.at_match.shares), (0.85, 20.0 / 0.85), "the book at the match");
}

#[test]
fn the_demo_counts_the_dollars_offered_up_to_the_limit() {
    let mut e = engine();
    // the BTC Up book at the band hit: 100 at 0.80 and 50 at 0.85 up to the limit 0.88 ($122.50),
    // 1 000 at 0.90 past it
    snapshot(&mut e, 0, 839_000, &[(800, 100.0), (850, 50.0), (900, 1000.0)], 0.18, 0.80);
    // by the match (840.250 s) a trade has taken the 0.80 level: $42.50
    at(&mut e, 840_100, ask_change(0, 840_100, 800, 0.0, 0.18, 0.85));
    at(&mut e, 840_100, bbo(0, 840_100, 0.18, 0.85));
    let out = run(&mut e, 850_000);
    let demo = signals(&out)[0].demo.clone().unwrap();
    assert!((demo.depth_no_latency_usd - 122.5).abs() < 1e-9, "{demo:?}");
    assert!((demo.depth_at_match_usd - 42.5).abs() < 1e-9, "{demo:?}");
}

#[test]
fn the_delay_grid_has_13_fills_from_the_send() {
    let mut e = engine();
    snapshot(&mut e, 0, 839_000, &[(800, 100.0)], 0.18, 0.80);
    at(&mut e, 840_120, ask_change(0, 840_120, 800, 0.0, 0.18, 0.85));
    at(&mut e, 840_120, ask_change(0, 840_120, 850, 100.0, 0.18, 0.85));
    at(&mut e, 840_120, bbo(0, 840_120, 0.18, 0.85));
    at(&mut e, 840_420, ask_change(0, 840_420, 850, 0.0, 0.18, 0.86));
    at(&mut e, 840_420, ask_change(0, 840_420, 860, 100.0, 0.18, 0.86));
    at(&mut e, 840_420, bbo(0, 840_420, 0.18, 0.86));
    let out = run(&mut e, 850_000);
    let demo = signals(&out)[0].demo.clone().unwrap();
    let grid = demo.by_delay.expect("a signal has the grid");
    assert_eq!(GRID_MS, [0, 50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 550, 600]);
    let prices: Vec<f64> = grid.iter().map(|f| f.avg_price).collect();
    assert_eq!(prices, [0.80, 0.80, 0.80, 0.85, 0.85, 0.85, 0.85, 0.85, 0.85, 0.86, 0.86, 0.86, 0.86]);
}

#[test]
fn a_check_without_a_shot_is_sent_at_840_or_at_its_arrival() {
    let mut e = engine();
    // BTC in the band from 839.5 s (it arrived at 839.6): the order would go at 840.000
    at(&mut e, 839_600, bbo(0, 839_500, 0.18, 0.80));
    // ETH enters the band at 842.300 and the event arrives at 842.350
    at(&mut e, 839_000, bbo(2, 839_000, 0.40, 0.60));
    e.on_item(S0 + 842_350, bbo(2, 842_300, 0.18, 0.80));
    let out = run(&mut e, 850_000);
    let sent: Vec<(Coin, i64)> = signals(&out).iter().map(|s| (s.check.coin, s.send_ms - S0)).collect();
    assert_eq!(sent, [(Coin::Btc, 840_000), (Coin::Eth, 842_350)]);
}

#[test]
fn a_shot_sets_the_send_and_gets_its_fill_at_once() {
    let mut e = engine();
    snapshot(&mut e, 0, 839_000, &[(800, 100.0)], 0.18, 0.80);
    let shot = ShotRecord { s: S, coin: Coin::Btc, side: Side::Up, t_trig: 840.0, event_ts_ms: S0 + 839_000, send_ms: S0 + 840_010, rank: 1, z: None, frame_to_decision_ns: 0 };
    e.on_shot(&shot);
    // the fill comes once the match moment has passed: the hot thread waits for it
    let mut out = run(&mut e, 840_300 + 2_000 + 500);
    let fills: Vec<&ShotFill> = out.iter().filter_map(|o| if let ExactOut::ShotFill(f) = o { Some(f) } else { None }).collect();
    assert_eq!(fills.len(), 1, "{out:?}");
    assert_eq!((fills[0].demo.match_ms, fills[0].demo.at_match.avg_price), (S0 + 840_260, 0.80));
    assert!(fills[0].demo.by_delay.is_none());
    assert!(signals(&out).is_empty(), "the check waits for the grid from the shot's send");
    out.extend(run(&mut e, 850_000));
    let s = signals(&out);
    assert!(s[0].shot && s[0].send_ms == S0 + 840_010, "{:?}", s[0]);
    assert_eq!(s[0].demo.as_ref().unwrap().match_ms, S0 + 840_260);
}

#[test]
fn the_book_at_840_is_taken_before_an_event_of_the_next_ms() {
    let mut e = engine();
    snapshot(&mut e, 0, 839_000, &[(800, 100.0)], 0.18, 0.80);
    // the first event after 840 s comes 1 ms after it: the check starts on it, with the book of 840 s
    at(&mut e, 840_001, ask_change(0, 840_001, 800, 0.0, 0.18, 0.85));
    at(&mut e, 840_001, ask_change(0, 840_001, 850, 100.0, 0.18, 0.85));
    at(&mut e, 840_001, bbo(0, 840_001, 0.18, 0.85));
    let out = run(&mut e, 850_000);
    let demo = signals(&out)[0].demo.clone().unwrap();
    assert_eq!(demo.no_latency.avg_price, 0.80);
    assert_eq!(demo.by_delay.unwrap()[0].avg_price, 0.80);
}

#[test]
fn a_shot_that_comes_after_its_check_started_still_sets_the_send() {
    // the exact engine has seen the check start (the line at 840.1 s), or get its price (840.4 s), when
    // the shot comes
    for line in [840_100, 840_400] {
        let mut e = engine();
        snapshot(&mut e, 0, 839_000, &[(800, 100.0)], 0.18, 0.80);
        let mut out = run(&mut e, line + 2_000 + 500);
        assert!(signals(&out).is_empty());
        let shot = ShotRecord { s: S, coin: Coin::Btc, side: Side::Up, t_trig: 840.0, event_ts_ms: S0 + 839_000, send_ms: S0 + 840_010, rank: 1, z: None, frame_to_decision_ns: 0 };
        e.on_shot(&shot);
        out.extend(run(&mut e, 850_000));
        assert_eq!(out.iter().filter(|o| matches!(o, ExactOut::ShotFill(_))).count(), 1, "{line}");
        let s = signals(&out);
        assert!(s[0].shot && s[0].send_ms == S0 + 840_010, "{line}: {:?}", s[0]);
        assert_eq!(s[0].demo.as_ref().unwrap().match_ms, S0 + 840_260, "{line}");
    }
}

// ---------------------------------------------------------------- decisions

#[test]
fn exact_z_waits_for_the_closed_second() {
    // Binance's closes stop at 838 s: second 839 is open until a later trade, a kline, or 1.5 s pass
    let mut e = engine_with(500, S + 838);
    at(&mut e, 839_000, bbo(0, 839_000, 0.18, 0.80));
    at(&mut e, 840_900, bbo(0, 840_900, 0.18, 0.80));
    at(&mut e, 841_200, bbo(0, 841_200, 0.18, 0.80));
    // the check has settled (event time 840.7 s) and its demo is captured, but 841.25 s is short of 841.5
    assert!(signals(&run(&mut e, 841_250)).is_empty(), "z waits for second 839");
    e.on_trade(&Trade { coin: Coin::Btc, ts_ms: S0 + 839_700, price: 100.5 });
    e.on_trade(&Trade { coin: Coin::Btc, ts_ms: S0 + 840_050, price: 100.6 });
    let out = run(&mut e, 841_300);
    let s = signals(&out);
    assert_eq!(s.len(), 1);
    // z used the close of 839 the late trade made
    let mut want = v21_core::live_closes::LiveCloses::new();
    want.add_klines(&klines(S - 1000, S + 838));
    want.add_trade(S0 + 839_700, 100.5);
    want.add_trade(S0 + 840_050, 100.6);
    let z = v21_core::zscore::z_score(&want, S, 840.0, Side::Up, &strategy().v2).unwrap();
    assert_eq!(s[0].z.unwrap().ef, z.ef);
}

#[test]
fn decisions_come_in_the_backtests_order() {
    // BTC and ETH both in the band at 840 s: ETH (the smaller Up id) is number 1, whatever arrives first;
    // and ETH's z waits for a hole of Binance's to be filled, so BTC's decision waits for ETH's
    let mut e = engine();
    e.on_gap(Coin::Eth, S + 831, S + 845);
    at(&mut e, 839_000, bbo(0, 839_000, 0.18, 0.80));
    at(&mut e, 839_100, bbo(2, 839_100, 0.18, 0.81));
    assert!(signals(&run(&mut e, 846_000)).is_empty(), "BTC waits for ETH's z");
    e.on_klines(Coin::Eth, &klines(S + 831, S + 900));
    let out = run(&mut e, 845_000);
    let got: Vec<(Coin, Option<u32>)> = signals(&out).iter().map(|s| (s.check.coin, s.rank)).collect();
    assert_eq!(got, [(Coin::Eth, Some(1)), (Coin::Btc, Some(2))]);
}

#[test]
fn without_binance_data_by_the_deadline_the_reason_is_no_binance() {
    let mut e = ExactEngine::new(strategy(), 500, timing());
    e.arm(&arm(S, false));
    at(&mut e, 839_000, bbo(0, 839_000, 0.18, 0.80));
    assert!(signals(&run(&mut e, 900_000)).is_empty(), "it waits for a backfill until 1 020 s");
    let out = run(&mut e, 1_020_001);
    let s = signals(&out);
    assert_eq!((s[0].z, s[0].v21.reason, s[0].rank), (None, Reason::NoBinance, Some(1)));
    assert!(summary(&out).is_some_and(|w| !w.binance_ok));
}

// ---------------------------------------------------------------- the book check

fn reloads(out: &[ExactOut]) -> Vec<u8> {
    out.iter().filter_map(|o| if let ExactOut::Reload { token } = o { Some(*token) } else { None }).collect()
}

#[test]
fn a_book_that_differs_from_the_server_asks_a_reload_once_per_5_s() {
    let mut e = engine();
    snapshot(&mut e, 0, 830_000, &[(800, 100.0)], 0.18, 0.80);
    // the server's best ask 0.80 is the book's: no reload
    at(&mut e, 831_000, ask_change(0, 831_000, 820, 10.0, 0.18, 0.80));
    assert_eq!(reloads(&run(&mut e, 834_000)), [0u8; 0]);
    // the server shows 0.79, which the book has not
    at(&mut e, 835_000, ask_change(0, 835_000, 830, 10.0, 0.18, 0.79));
    assert_eq!(reloads(&run(&mut e, 838_000)), [0]);
    at(&mut e, 838_000, ask_change(0, 838_000, 830, 12.0, 0.18, 0.79));
    assert_eq!(reloads(&run(&mut e, 841_000)), [0u8; 0], "not again within 5 s");
    at(&mut e, 840_500, ask_change(0, 840_500, 830, 14.0, 0.18, 0.79));
    assert_eq!(reloads(&run(&mut e, 844_000)), [0], "5.5 s later");
    // the REST book mends it
    e.on_rest_book(0, S0 + 844_000, &[(0.18, 5.0)], &[(0.79, 3.0), (0.80, 100.0)]);
    at(&mut e, 844_500, ask_change(0, 844_500, 830, 14.0, 0.18, 0.79));
    let out = run(&mut e, 852_000);
    assert_eq!(reloads(&out), [0u8; 0]);
}

// ---------------------------------------------------------------- a drop of both copies

#[test]
fn after_coverage_loss_changes_wait_for_a_fresh_snapshot() {
    let mut e = engine();
    snapshot(&mut e, 0, 839_000, &[(950, 100.0)], 0.04, 0.95);
    e.on_coverage_lost(S0 + 841_000);
    // a change in the band before a fresh snapshot: the book behind it is unknown
    at(&mut e, 842_000, ask_change(0, 842_000, 800, 50.0, 0.18, 0.80));
    at(&mut e, 842_000, bbo(0, 842_000, 0.18, 0.80));
    // the copies are back: a snapshot of every token
    snapshot(&mut e, 0, 843_000, &[(960, 100.0)], 0.03, 0.96);
    for token in 1..6 {
        snapshot(&mut e, token, 843_000, &[(600, 10.0)], 0.40, 0.60);
    }
    at(&mut e, 844_000, ask_change(0, 844_000, 800, 50.0, 0.18, 0.80));
    at(&mut e, 844_000, bbo(0, 844_000, 0.18, 0.80));
    let out = run(&mut e, 880_000);
    let s = signals(&out);
    assert_eq!(s.iter().map(|s| s.check.t_trig).collect::<Vec<_>>(), [844.0]);
    let w = summary(&out).expect("the window is done");
    assert!(!w.ws_ok);
    assert_eq!(w.coverage_lost_ms, 2_000);
}

/// A row for every token every second from `from` to `to` ms into the window, all out of the band: the
/// copies are alive.
fn every_second(e: &mut ExactEngine, from: i64, to: i64) {
    for t in (from..=to).step_by(1000) {
        for token in 0..6 {
            let (bid, ask) = if token == 0 { (0.03, 0.96) } else { (0.40, 0.60) };
            at(e, t, bbo(token, t, bid, ask));
        }
    }
}

#[test]
fn a_drop_of_both_copies_spoils_the_minute_only_if_it_reaches_it() {
    let drop = |lost_at: i64, back_at: i64| {
        let mut e = engine();
        every_second(&mut e, 790_000, lost_at - 1000);
        e.on_coverage_lost(S0 + lost_at);
        for token in 0..6 {
            snapshot(&mut e, token, back_at, &[(600, 10.0)], 0.40, 0.60);
        }
        every_second(&mut e, back_at + 1000, 871_000);
        let w = summary(&run(&mut e, 880_000)).expect("the window is done").clone();
        (w.ws_ok, w.coverage_lost_ms)
    };
    assert_eq!(drop(800_000, 802_000), (true, 2_000));
    assert_eq!(drop(841_000, 843_000), (false, 2_000));
}

#[test]
fn a_window_joined_late_is_a_drop_at_its_start() {
    let mut e = ExactEngine::new(strategy(), 500, timing());
    for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
        e.on_klines(coin, &klines(S - 1000, S + 900));
    }
    e.arm(&arm(S, true));
    // a change before any snapshot: the book behind it is unknown
    at(&mut e, 851_000, ask_change(0, 851_000, 800, 50.0, 0.18, 0.80));
    at(&mut e, 851_000, bbo(0, 851_000, 0.18, 0.80));
    snapshot(&mut e, 0, 852_000, &[(800, 100.0)], 0.18, 0.80);
    for token in 1..6 {
        snapshot(&mut e, token, 852_000, &[(600, 10.0)], 0.40, 0.60);
    }
    let out = run(&mut e, 880_000);
    assert_eq!(signals(&out).iter().map(|s| s.check.t_trig).collect::<Vec<_>>(), [852.0]);
    let w = summary(&out).expect("the window is done");
    assert!(!w.ws_ok);
    assert_eq!(w.coverage_lost_ms, 852_000);
}

// ---------------------------------------------------------------- late events

/// Frames (arrival ms into the window, items), `advance` after each, then the clock to 850 s.
fn feed(hold_ms: i64, frames: Vec<(i64, Vec<FeedItem>)>) -> Vec<ExactOut> {
    let mut e = engine_with(hold_ms, S + 900);
    let mut out = vec![];
    for (rx, items) in frames {
        for item in items {
            e.on_item(S0 + rx, item);
        }
        e.advance(S0 + rx, &mut out);
    }
    e.advance(S0 + 850_000, &mut out);
    out
}

#[test]
fn the_buffer_puts_a_change_back_before_a_later_event_of_another_token() {
    // BTC's Up book at 839 s is in the band from 840 s on. Its change of 840.200 comes at 840.450, after
    // ETH's event of 840.400 (which came at 840.410): the moments 840.25 (the match) and 840.3 (the
    // price of the check) see the change only if the events go out in time order
    let frames = || {
        vec![
            (
                839_000,
                vec![
                    FeedItem::Snapshot { token: 0, ts_ms: S0 + 839_000 },
                    FeedItem::SnapLevel { token: 0, side: BookSide::Bid, tick: 180, size: 50.0 },
                    FeedItem::SnapLevel { token: 0, side: BookSide::Ask, tick: 800, size: 100.0 },
                    bbo(0, 839_000, 0.18, 0.80),
                    bbo(2, 839_000, 0.40, 0.60),
                ],
            ),
            (840_410, vec![bbo(2, 840_400, 0.41, 0.59)]),
            (
                840_450,
                vec![
                    ask_change(0, 840_200, 800, 0.0, 0.18, 0.85),
                    bbo(0, 840_200, 0.18, 0.85),
                    ask_change(0, 840_200, 850, 100.0, 0.18, 0.85),
                    bbo(0, 840_200, 0.18, 0.85),
                ],
            ),
        ]
    };
    let price = |out: &[ExactOut]| {
        let s = signals(out);
        assert_eq!(s.len(), 1, "{out:?}");
        (s[0].check.px, s[0].demo.as_ref().unwrap().at_match.avg_price)
    };
    assert_eq!(price(&feed(500, frames())), (0.85, 0.85));
    // without the buffer the check is priced before the change has come
    assert_eq!(price(&feed(0, frames())), (0.80, 0.80));
}

/// The frames of copy a of the recording of 2026-10-19 by arrival, with the receipt moved to the
/// exchange's clock (the PC was about a second behind): (arrival ms, the frame's items).
fn recorded() -> (WindowArm, Vec<(i64, Vec<FeedItem>)>) {
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/two_copies.jsonl.gz")).unwrap();
    let mut lines = BufReader::new(flate2::read::GzDecoder::new(file)).lines();
    let meta: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let ids: Vec<String> = meta["meta"]["tokens"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
    let tokens: Vec<Box<str>> = ids.iter().map(|t| Box::from(t.as_str())).collect();
    let s = meta["meta"]["windows"][1].as_i64().unwrap();
    let (mut frame, mut frames) = (Frame::default(), vec![]);
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        if v["copy"] != 0 {
            continue;
        }
        let mut items = vec![];
        read_frame(v["raw"].as_str().unwrap().as_bytes(), &tokens, &mut frame, &mut items);
        frames.push((v["rx_ns"].as_i64().unwrap() / 1_000_000, items));
    }
    let mut lag: Vec<i64> = frames.iter().flat_map(|(rx, items)| items.iter().filter_map(move |i| ts_of(i).map(|ts| ts - rx))).collect();
    lag.sort_unstable();
    let offset = lag[lag.len() / 2] + 50; // the median, and some delivery
    for (rx, _) in &mut frames {
        *rx += offset;
    }
    // the second window of the recording: tokens 6..12 (BTC, ETH, SOL; Up then Down)
    let coin = |coin, up: u8| CoinArm {
        coin,
        up_ix: up,
        down_ix: up + 1,
        up_key: v21_live::shared::msgs::token_key(&ids[usize::from(up)]).unwrap(),
        fee: FEE,
        tradable: true,
        note: String::new(),
    };
    let coins = vec![coin(Coin::Btc, 6), coin(Coin::Eth, 8), coin(Coin::Sol, 10)];
    let arm = WindowArm { s, tokens: ids, coins, missing: vec![], resumed: false, traded: vec![] };
    (arm, frames)
}

fn ts_of(i: &FeedItem) -> Option<i64> {
    match *i {
        FeedItem::Bbo { ts_ms, .. } | FeedItem::Snapshot { ts_ms, .. } | FeedItem::Level { ts_ms, .. } | FeedItem::TickSize { ts_ms, .. } => Some(ts_ms),
        FeedItem::SnapLevel { .. } => None,
    }
}

/// The frames through the engine (`advance` after each frame), and its window summary.
fn replay_frames(arm: &WindowArm, frames: &[(i64, Vec<FeedItem>)], hold_ms: i64) -> WindowSummary {
    let mut e = ExactEngine::new(strategy(), hold_ms, timing());
    for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
        e.on_klines(coin, &klines(arm.s - 1000, arm.s + 900));
    }
    e.arm(arm);
    let mut out = vec![];
    for (rx, items) in frames {
        for item in items {
            e.on_item(*rx, *item);
        }
        e.advance(*rx, &mut out);
    }
    e.advance(arm.s * 1000 + 1_000_000, &mut out);
    summary(&out).expect("the window is done").clone()
}

/// The rows of the Up tokens in 835–871 s: (token, exchange ms), by arrival.
fn up_rows(arm: &WindowArm, frames: &[(i64, Vec<FeedItem>)]) -> Vec<(u8, i64)> {
    let ups: Vec<u8> = arm.coins.iter().map(|c| c.up_ix).collect();
    let window = arm.s * 1000 + 835_000..=arm.s * 1000 + 871_000;
    frames
        .iter()
        .flat_map(|(_, items)| items)
        .filter_map(|i| match *i {
            FeedItem::Bbo { token, ts_ms, .. } if ups.contains(&token) && window.contains(&ts_ms) => Some((token, ts_ms)),
            _ => None,
        })
        .collect()
}

#[test]
fn late_events_are_put_back_in_time_order() {
    let (arm, frames) = recorded();
    // the events of a token come in order, but often after a later event of another token: within
    // the buffer, never dropped
    let mut newest = i64::MIN;
    let mut behind = vec![];
    for ts in frames.iter().flat_map(|(_, items)| items.iter().filter_map(ts_of)) {
        if ts < newest {
            behind.push(newest - ts);
        }
        newest = newest.max(ts);
    }
    assert!(!behind.is_empty(), "the stretch was chosen to hold late events");
    assert!(behind.iter().all(|&b| b < 500), "{behind:?}");
    let w = replay_frames(&arm, &frames, 500);
    assert_eq!(w.late_dropped, 0);
    // every row of the Up tokens in 835–871 s is kept, in time order
    let arrived = up_rows(&arm, &frames);
    assert_eq!(w.bbo.iter().map(|(_, rows)| rows.len()).sum::<usize>(), arrived.len());
    for (coin, rows) in &w.bbo {
        assert!(rows.windows(2).all(|p| p[0].0 <= p[1].0), "{coin:?}: in time order");
    }
    // an Up row that comes 2 s late, behind later rows of its token, is dropped
    let (k, j) = frames
        .iter()
        .enumerate()
        .find_map(|(k, (_, items))| {
            let j = items.iter().position(|i| matches!(*i, FeedItem::Bbo { token: 6, ts_ms, .. } if ts_ms >= arm.s * 1000 + 845_000))?;
            Some((k, j))
        })
        .unwrap();
    let mut delayed = frames.clone();
    let row = delayed[k].1.remove(j);
    delayed.push((frames[k].0 + 2_000, vec![row]));
    delayed.sort_by_key(|(rx, _)| *rx);
    let w = replay_frames(&arm, &delayed, 500);
    assert_eq!(w.late_dropped, 1);
    assert_eq!(w.bbo.iter().map(|(_, rows)| rows.len()).sum::<usize>(), arrived.len() - 1);
}

// ---------------------------------------------------------------- an archive day

fn root() -> Option<PathBuf> {
    match std::env::var("V21_ROOT").or_else(|_| std::env::var("V21_DATA")) {
        Ok(r) => Some(PathBuf::from(r)),
        Err(_) => {
            println!("skipped: V21_ROOT is not set");
            None
        }
    }
}

#[test]
fn archive_day_through_the_exact_engine_equals_the_replay() {
    use v21_replay::archive::{bbo_files, load_closes, load_windows, read_bbo_file, Incidents, Period};
    use v21_replay::run::replay_with;
    let Some(root) = root() else { return };
    let strategy = strategy();
    // 2026-10-15 UTC, the first day of the forward test
    let (day0, day1) = (1_792_022_400_000, 1_792_108_800_000);
    let files: Vec<PathBuf> = bbo_files(&root, Period::Main)
        .unwrap()
        .into_iter()
        .filter(|f| ("2026-10-14T23.parquet"..="2026-10-16T00.parquet").contains(&f.file_name().unwrap().to_string_lossy().as_ref()))
        .collect();
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let inc = Incidents::load(&root.join("data/INCIDENTS.json"), 30).unwrap();
    let windows: Vec<_> = load_windows(&root, Period::Main, &coins, Some(&inc)).unwrap().into_iter().filter(|w| (day0..day1).contains(&w.s0_ms)).collect();
    let closes: HashMap<Coin, ArrayCloses> = coins.iter().map(|&c| (c, load_closes(&root, Period::Main, c).unwrap())).collect();
    let want = replay_with(&files, &windows, &closes, &strategy).unwrap();

    // the rows of each Up token, from every file, in time order (the order of the archive)
    let tokens: HashSet<[u8; 32]> = windows.iter().map(|w| w.up).collect();
    // (file, exchange ms, bid, ask) by Up token
    type Rows = HashMap<[u8; 32], Vec<(usize, i64, f64, f64)>>;
    let mut rows: Rows = HashMap::new();
    for (k, f) in files.iter().enumerate() {
        for r in read_bbo_file(f, &tokens).unwrap() {
            rows.entry(r.token).or_default().push((k, r.ts_ms, r.bid, r.ask));
        }
    }
    for list in rows.values_mut() {
        list.sort_by_key(|r| r.1);
    }
    let mut by_window: BTreeMap<i64, Vec<&v21_replay::archive::WindowMeta>> = BTreeMap::new();
    for w in &windows {
        by_window.entry(w.s0_ms).or_default().push(w);
    }
    type Got = (i64, Coin, f64, Side, f64, Option<u32>, Reason, Reason, f64);
    let mut got: Vec<Got> = vec![];
    let mut two_files = vec![];
    for (&s0, metas) in &by_window {
        let s = s0 / 1000;
        let mut e = ExactEngine::new(strategy.clone(), 500, timing());
        for &c in &coins {
            let cl = &closes[&c];
            let first = (s - 1000).max(cl.t0());
            e.on_klines(c, &(first..=s + 900).filter_map(|q| Some((q * 1000, cl.close(q)?))).collect::<Vec<_>>());
        }
        // tokens: the Up tokens of the coins, then a Down token for each (the archive has no Down rows)
        let n = metas.len() as u8;
        let arms: Vec<CoinArm> = metas
            .iter()
            .enumerate()
            .map(|(i, m)| CoinArm { coin: m.coin, up_ix: i as u8, down_ix: n + i as u8, up_key: m.up, fee: FEE, tradable: true, note: String::new() })
            .collect();
        let missing = coins.iter().copied().filter(|c| !metas.iter().any(|m| m.coin == *c)).collect();
        let tokens = (0..2 * n).map(|i| format!("t{i}")).collect();
        e.arm(&WindowArm { s, tokens, coins: arms, missing, resumed: false, traded: vec![] });
        let mut merged: Vec<(i64, u8, f64, f64)> = vec![];
        for (i, m) in metas.iter().enumerate() {
            let list = rows.get(&m.up).map(Vec::as_slice).unwrap_or_default();
            let minute: HashSet<usize> = list.iter().filter(|r| (s0 + 835_000..=s0 + 870_300).contains(&r.1)).map(|r| r.0).collect();
            if minute.len() > 1 {
                two_files.push((s0, m.coin));
            }
            merged.extend(list.iter().map(|r| (r.1, i as u8, r.2, r.3)));
        }
        merged.sort_by_key(|r| (r.0, r.1));
        let mut out = vec![];
        for &(ts, token, bid, ask) in &merged {
            e.on_item(ts, FeedItem::Bbo { token, ts_ms: ts, bid, ask });
            e.advance(ts, &mut out);
        }
        e.advance(s0 + 1_100_000, &mut out);
        assert!(summary(&out).is_some(), "window {s0} is done");
        for sig in signals(&out).into_iter().filter(|s| s.check.ok) {
            let c = &sig.check;
            got.push((s0, c.coin, c.t_trig, c.side, c.px, sig.rank, sig.v2.reason, sig.v21.reason, sig.z.map_or(f64::NAN, |z| z.z)));
        }
    }
    assert!(two_files.is_empty(), "windows with decision-minute rows in two hourly files {two_files:?}: take another day");
    assert_eq!(got.len(), want.len(), "signals of the day");
    assert!(got.len() > 20, "{}", got.len());
    for (g, w) in got.iter().zip(&want) {
        let want_row = (w.s0_ms, w.coin, w.t_trig, w.side, w.px, Some(w.rank), w.v2.reason, w.v21.reason);
        assert_eq!((g.0, g.1, g.2, g.3, g.4, g.5, g.6, g.7), want_row, "{}", w.slug);
        // the backtest sums a window as a difference of running sums over the whole archive: where
        // sigma is a fraction of a basis point that moves z by up to ~2e-6; the live closes sum the
        // window itself
        assert!((g.8 - w.z.z).abs() <= 1e-5 * w.z.z.abs().max(1.0), "{}: z {} vs {}", w.slug, g.8, w.z.z);
    }
}
