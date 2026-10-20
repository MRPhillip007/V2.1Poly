//! The hot path allocates nothing after its warm-up (Global Constraints): the frames of both copies of
//! the recording and its Binance trades go through the dedupe, the reader, the items, the fast engine,
//! the ticks and the queue to the cold runtime under a counting allocator — the way of
//! `pmbot/tests/no_alloc.rs`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{BufRead, BufReader};
use std::time::Instant;

use v21_core::fill::FeeSchedule;
use v21_core::params::load_strategy;
use v21_core::window::Coin;
use v21_live::hot::binance::parse_agg;
use v21_live::hot::items::{read_frame, FeedItem};
use v21_live::hot::wire::Frame;
use v21_live::hot::HotState;
use v21_live::shared::msgs::{token_key, CoinArm, ColdMsg, FastOut, HotMsg, WindowArm};

thread_local! {
    /// Allocations of this thread while it counts: the harness's own threads do not count.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every call is passed on to `System` unchanged; counting only touches thread-locals.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

fn count() {
    if COUNTING.with(Cell::get) {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
    }
}

fn counting(on: bool) {
    COUNTING.with(|c| c.set(on));
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };

/// What comes to the hot thread: a frame of copy 0 or 1, or a Binance message (source 2).
struct Event {
    rx_ms: i64,
    source: u8,
    raw: Vec<u8>,
}

/// The second window of the recording, its events by arrival (the receipt moved to the exchange's
/// clock), and a book of BTC Up in the band at 839.999 s: the 840 tick decides on it.
fn recorded() -> (WindowArm, Vec<Event>) {
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/two_copies.jsonl.gz")).unwrap();
    let mut lines = BufReader::new(flate2::read::GzDecoder::new(file)).lines();
    let meta: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    let ids: Vec<String> = meta["meta"]["tokens"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().to_string()).collect();
    let tokens: Vec<Box<str>> = ids.iter().map(|t| Box::from(t.as_str())).collect();
    let s = meta["meta"]["windows"][1].as_i64().unwrap();
    let mut events = vec![];
    let (mut frame, mut items, mut lag) = (Frame::default(), vec![], vec![]);
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let source = match v["copy"].as_i64() {
            Some(c) => c as u8,
            None => 2,
        };
        let raw = v["raw"].as_str().unwrap().as_bytes().to_vec();
        let rx_ms = v["rx_ns"].as_i64().unwrap() / 1_000_000;
        if source == 0 {
            items.clear();
            read_frame(&raw, &tokens, &mut frame, &mut items);
            lag.extend(items.iter().filter_map(|i| if let FeedItem::Bbo { ts_ms, .. } = i { Some(ts_ms - rx_ms) } else { None }));
        }
        events.push(Event { rx_ms, source, raw });
    }
    lag.sort_unstable();
    let offset = lag[lag.len() / 2] + 50;
    for e in &mut events {
        e.rx_ms += offset;
    }
    let s0 = s * 1000;
    let book = format!(
        r#"{{"market":"0x00", "asset_id":"{}", "bids":[{{"price":"0.18", "size":"100"}}], "asks":[{{"price":"0.8", "size":"100"}}], "hash":"x", "timestamp":"{}", "event_type":"book"}}"#,
        ids[6],
        s0 + 839_999
    );
    for source in [0, 1] {
        events.push(Event { rx_ms: s0 + 839_999, source, raw: book.clone().into_bytes() });
    }
    events.sort_by_key(|e| e.rx_ms);
    let coin = |coin, up: u8| CoinArm { coin, up_ix: up, down_ix: up + 1, up_key: token_key(&ids[usize::from(up)]).unwrap(), fee: FEE, tradable: true, note: String::new() };
    let coins = vec![coin(Coin::Btc, 6), coin(Coin::Eth, 8), coin(Coin::Sol, 10)];
    (WindowArm { s, tokens: ids, coins, missing: vec![], resumed: false, traded: vec![] }, events)
}

#[test]
fn the_hot_path_allocates_nothing_after_warm_up() {
    let (arm, events) = recorded();
    let s0 = arm.s * 1000;
    let (to_cold, mut cold) = rtrb::RingBuffer::<HotMsg>::new(65_536);
    let mut hot = HotState::new(load_strategy(V2, V21).unwrap().v21, 2_000, 2, to_cold);
    // the closes of the warm-up: the price of each coin's first trade, still
    for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
        let price = events.iter().filter(|e| e.source == 2).filter_map(|e| parse_agg(&e.raw)).find(|t| t.coin == coin).map_or(100.0, |t| t.price);
        let rows = (arm.s - 1000..arm.s + 830).map(|q| (q * 1000, price * (1.0 + ((q % 5) as f64) * 1e-5))).collect();
        hot.on_cold(ColdMsg::Klines { coin, rows });
    }

    // the counter counts: a zero below means something only if an allocation is seen here
    counting(true);
    ALLOCATIONS.with(|n| n.set(0));
    let boxed = std::hint::black_box(Box::new(7u64));
    counting(false);
    drop(boxed);
    assert_eq!(ALLOCATIONS.with(Cell::get), 1, "the counter does not count");

    // the first pass warms up; four more are counted, each in a new window (arming allocates, uncounted)
    let (mut frames, mut decisions, mut messages) = (0, 0, 0);
    for pass in 0..5 {
        counting(false);
        hot.on_cold(ColdMsg::Arm(arm.clone()));
        ALLOCATIONS.with(|n| n.set(0));
        counting(pass > 0);
        // the hot thread's timer: every 50 ms from 835 s
        let mut tick = s0 + 835_000;
        for e in &events {
            while tick <= e.rx_ms {
                hot.on_tick(tick);
                tick += 50;
            }
            if e.source == 2 {
                if let Some(t) = parse_agg(&e.raw) {
                    hot.on_trade(&t, e.rx_ms);
                }
            } else {
                hot.on_frame(e.source, &e.raw, e.rx_ms, Instant::now());
                frames += u32::from(pass > 0);
            }
            while let Ok(m) = cold.pop() {
                messages += 1;
                decisions += u32::from(pass > 0 && matches!(m, HotMsg::Fast(FastOut::Shot(_) | FastOut::Skip(_))));
            }
        }
        counting(false);
        assert_eq!(ALLOCATIONS.with(Cell::get), 0, "pass {pass}: {frames} frames, {decisions} decisions so far");
    }
    assert!(frames > 10_000, "{frames}");
    assert!(decisions >= 4, "the decision at 840 s in every counted pass: {decisions}");
    assert!(messages > 10_000, "{messages}");
}
