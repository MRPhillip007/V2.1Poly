//! The hot thread's state (spec §4.2, §5.2, §5.3): the copies deduped, the loss of the feed, the gaps
//! of Binance, the decisions and the counts of the health record — and a full queue that never makes
//! the hot thread wait.

use std::time::{Duration, Instant};

use v21_core::fill::FeeSchedule;
use v21_core::params::load_strategy;
use v21_core::window::{Coin, Side};
use v21_live::hot::binance::Trade;
use v21_live::hot::HotState;
use v21_live::shared::msgs::{CoinArm, ColdMsg, FastOut, HotMsg, WindowArm};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const S0: i64 = S * 1000;
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };

fn arm() -> WindowArm {
    let coin = |coin, up: u8, key: u8| CoinArm { coin, up_ix: up, down_ix: up + 1, up_key: [key; 32], fee: FEE, tradable: true, note: String::new() };
    WindowArm {
        s: S,
        tokens: (0..6).map(|i| format!("{}", 1000 + i)).collect(),
        coins: vec![coin(Coin::Btc, 0, 2), coin(Coin::Eth, 2, 1), coin(Coin::Sol, 4, 3)],
        missing: vec![],
        resumed: false,
        traded: vec![],
    }
}

/// A hot state armed on the window, with rising closes (z far above 1.25 for Up) and a queue of
/// `room` messages.
fn hot(room: usize) -> (HotState, rtrb::Consumer<HotMsg>) {
    let (to_cold, from_hot) = rtrb::RingBuffer::new(room);
    let mut h = HotState::new(load_strategy(V2, V21).unwrap().v21, i64::MAX / 4, 2, to_cold);
    for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
        let rows = (S - 1000..=S + 900).map(|q| (q * 1000, 100.0 + 0.0006 * (q - S + 1000) as f64)).collect();
        h.on_cold(ColdMsg::Klines { coin, rows });
    }
    h.on_cold(ColdMsg::Arm(arm()));
    (h, from_hot)
}

fn drain(q: &mut rtrb::Consumer<HotMsg>) -> Vec<HotMsg> {
    let mut out = vec![];
    while let Ok(m) = q.pop() {
        out.push(m);
    }
    out
}

/// A `book` frame of token number `token` (id `1000 + token`) at `t` ms into the window.
fn book(token: u8, t: i64, bid: &str, ask: &str) -> Vec<u8> {
    format!(
        r#"{{"market":"0x00", "asset_id":"{}", "bids":[{{"price":"{bid}", "size":"100"}}], "asks":[{{"price":"{ask}", "size":"100"}}], "hash":"x", "timestamp":"{}", "event_type":"book"}}"#,
        1000 + u32::from(token),
        S0 + t
    )
    .into_bytes()
}

#[test]
fn a_frame_both_copies_bring_is_applied_once() {
    let (mut h, mut q) = hot(1024);
    let raw = book(0, 839_000, "0.18", "0.8");
    h.on_frame(0, &raw, S0 + 839_050, Instant::now());
    h.on_frame(1, &raw, S0 + 839_060, Instant::now());
    let feed: Vec<HotMsg> = drain(&mut q).into_iter().filter(|m| matches!(m, HotMsg::Feed { .. })).collect();
    // a snapshot, its two levels and its row: once
    assert_eq!(feed.len(), 4, "{feed:?}");
    assert!(matches!(feed[0], HotMsg::Feed { rx_ms, .. } if rx_ms == S0 + 839_050));
    // the 840 tick decides on that book; the shot has no frame behind it
    h.on_tick(S0 + 840_000);
    let shots: Vec<FastOut> = drain(&mut q).into_iter().filter_map(|m| if let HotMsg::Fast(f) = m { Some(f) } else { None }).collect();
    assert!(matches!(shots[..], [FastOut::Shot(s)] if s.coin == Coin::Btc && s.side == Side::Up && s.frame_to_decision_ns == 0), "{shots:?}");
    // the counts of the minute go out with the first tick a minute after the first one
    h.on_tick(S0 + 900_000);
    let health = drain(&mut q).into_iter().find_map(|m| if let HotMsg::Health(h) = m { Some(h) } else { None }).expect("a health record");
    assert_eq!((health.frames, health.dup_frames, health.pm_lag_ms.count(), health.decide_ns.count()), (2, 1, 1, 0));
}

#[test]
fn a_copy_seconds_behind_the_other_is_still_applied_once() {
    // the copies drift apart by seconds (the recording of 20.10: 4.6% of the frames came over 3 s
    // after the other copy's, the slowest within 30 s): the frame must still be known
    let (mut h, mut q) = hot(1024);
    let raw = book(0, 829_900, "0.18", "0.8");
    h.on_frame(0, &raw, S0 + 830_000, Instant::now());
    h.on_frame(1, &raw, S0 + 838_000, Instant::now());
    let feed = drain(&mut q).into_iter().filter(|m| matches!(m, HotMsg::Feed { .. })).count();
    // a snapshot, its two levels and its row: once
    assert_eq!(feed, 4);
}

#[test]
fn a_shot_on_a_frame_carries_the_time_from_the_frame() {
    let (mut h, mut q) = hot(1024);
    h.on_frame(0, &book(0, 839_000, "0.04", "0.95"), S0 + 839_050, Instant::now());
    h.on_tick(S0 + 840_000);
    let rx = Instant::now();
    std::thread::sleep(Duration::from_millis(2));
    h.on_frame(0, &book(0, 845_123, "0.18", "0.8"), S0 + 845_150, rx);
    let shot = drain(&mut q).into_iter().find_map(|m| if let HotMsg::Fast(FastOut::Shot(s)) = m { Some(s) } else { None }).expect("a shot");
    assert_eq!((shot.t_trig, shot.send_ms), (845.123, S0 + 845_150));
    assert!(shot.frame_to_decision_ns >= 2_000_000, "{}", shot.frame_to_decision_ns);
}

#[test]
fn the_loss_of_the_last_copy_is_a_loss_of_the_feed() {
    let (mut h, mut q) = hot(1024);
    h.on_copy(0, true, None, S0 + 600_000);
    h.on_copy(1, true, None, S0 + 600_010);
    // one copy drops: the feed goes on
    h.on_copy(0, false, Some(1013), S0 + 845_000);
    let coverage = |msgs: Vec<HotMsg>| msgs.into_iter().filter_map(|m| if let HotMsg::Coverage { lost, at_ms } = m { Some((lost, at_ms)) } else { None }).collect::<Vec<_>>();
    assert_eq!(coverage(drain(&mut q)), []);
    // the other one too: lost; back: found
    h.on_copy(1, false, None, S0 + 846_000);
    h.on_copy(0, true, None, S0 + 857_000);
    assert_eq!(coverage(drain(&mut q)), [(true, S0 + 846_000), (false, S0 + 857_000)]);
    // lost again at the end of the window, and closed then: the next window's copies are not a return
    h.on_copy(0, false, None, S0 + 870_000);
    assert_eq!(coverage(drain(&mut q)), [(true, S0 + 870_000)]);
    h.copies_closed();
    h.on_copy(1, true, None, S0 + 1_500_000);
    assert_eq!(coverage(drain(&mut q)), []);
}

#[test]
fn a_drop_of_binance_marks_the_gap_and_asks_for_the_backfill() {
    let (mut h, mut q) = hot(1024);
    h.on_trade(&Trade { coin: Coin::Btc, ts_ms: S0 + 830_400, price: 100.5 }, S0 + 830_450);
    h.on_binance(false, false, S0 + 832_500);
    h.on_binance(true, true, S0 + 833_000);
    h.on_trade(&Trade { coin: Coin::Btc, ts_ms: S0 + 834_200, price: 100.6 }, S0 + 834_250);
    let gaps: Vec<(Coin, i64, i64)> = drain(&mut q).into_iter().filter_map(|m| if let HotMsg::Gap { coin, q0, q1 } = m { Some((coin, q0, q1)) } else { None }).collect();
    assert_eq!(gaps, [(Coin::Btc, S + 830, S + 832), (Coin::Btc, S + 830, S + 833)]);
}

#[test]
fn a_full_queue_drops_the_message_and_counts_it() {
    let (mut h, mut q) = hot(4);
    for k in 0..10 {
        h.on_trade(&Trade { coin: Coin::Btc, ts_ms: S0 + 830_000 + k, price: 100.5 }, S0 + 830_050 + k);
    }
    assert_eq!(drain(&mut q).len(), 4);
    h.on_tick(S0 + 830_100);
    h.on_tick(S0 + 890_100);
    let health = drain(&mut q).into_iter().find_map(|m| if let HotMsg::Health(h) = m { Some(h) } else { None }).expect("a health record");
    assert_eq!((health.trades, health.dropped_msgs), (10, 6));
}
