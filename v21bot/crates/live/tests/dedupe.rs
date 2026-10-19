//! The dedupe of the two copies of the market channel (spec §5.2): each frame is applied once, from
//! the copy that brings it first. The recording of 2026-10-19 merged through it gives one copy's frames.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use v21_live::hot::copies::FrameDedupe;

#[test]
fn the_second_copy_of_a_frame_is_dropped() {
    let mut d = FrameDedupe::new(64, 2, 3_000);
    assert!(d.admit(0, b"A", 1_000));
    assert!(!d.admit(1, b"A", 1_010));
    assert!(d.admit(1, b"B", 1_020));
    assert!(!d.admit(0, b"B", 1_030));
    assert_eq!(d.dropped(), 2);
}

#[test]
fn identical_frames_repeated_inside_a_copy_are_kept() {
    // A, A from copy 0 and A, A from copy 1: two frames
    let mut d = FrameDedupe::new(64, 2, 3_000);
    let got: Vec<bool> = [0u8, 0, 1, 1].iter().map(|&c| d.admit(c, b"A", 1_000)).collect();
    assert_eq!(got, [true, true, false, false]);
    // the other order of arrival admits two as well
    let mut d = FrameDedupe::new(64, 2, 3_000);
    let got: Vec<bool> = [0u8, 1, 1, 0].iter().map(|&c| d.admit(c, b"A", 1_000)).collect();
    assert_eq!(got, [true, false, true, false]);
}

#[test]
fn a_frame_only_one_copy_had_is_kept() {
    let mut d = FrameDedupe::new(64, 2, 3_000);
    assert!(d.admit(0, b"A", 1_000));
    assert!(d.admit(0, b"C", 1_001)); // copy 1 lost it in a reconnect
    assert!(!d.admit(1, b"A", 1_002));
    assert_eq!(d.dropped(), 1);
}

#[test]
fn an_entry_older_than_the_ttl_is_forgotten() {
    let mut d = FrameDedupe::new(64, 2, 3_000);
    assert!(d.admit(0, b"A", 1_000));
    assert!(!d.admit(1, b"A", 4_000));
    assert!(d.admit(0, b"B", 1_000));
    assert!(d.admit(1, b"B", 4_001), "3 001 ms later: a new frame");
}

#[test]
fn a_full_table_admits_rather_than_drops() {
    // 16 slots full of live entries: a new frame finds no place and is admitted, and so is its copy
    let mut d = FrameDedupe::new(16, 2, 3_000);
    for k in 0..16u32 {
        assert!(d.admit(0, &k.to_le_bytes(), 1_000), "{k}");
    }
    assert!(d.admit(0, b"new", 1_000));
    assert!(d.admit(1, b"new", 1_001), "no record of it: applied twice, never lost");
    assert!(!d.admit(1, &3u32.to_le_bytes(), 1_002), "the frames held are still known");
}

// ---------------------------------------------------------------- the recording

/// The frames of both copies in the order of receipt: (copy, receipt ns, frame).
fn recording() -> Vec<(u8, i64, String)> {
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/two_copies.jsonl.gz")).unwrap();
    let lines = BufReader::new(flate2::read::GzDecoder::new(file)).lines().skip(1); // the meta line
    lines
        .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
        .filter_map(|v| {
            let copy = u8::try_from(v["copy"].as_u64()?).ok()?; // Binance's lines have "bn"
            Some((copy, v["rx_ns"].as_i64().unwrap(), v["raw"].as_str().unwrap().to_string()))
        })
        .collect()
}

fn market(raw: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(raw).unwrap();
    let first = if v.is_array() { &v[0] } else { &v };
    first["market"].as_str().unwrap_or_default().to_string()
}

fn by_market<'a>(frames: &[&'a String]) -> HashMap<String, Vec<&'a String>> {
    let mut out: HashMap<String, Vec<&String>> = HashMap::new();
    for f in frames {
        out.entry(market(f)).or_default().push(f);
    }
    out
}

#[test]
fn merged_stream_of_the_recording_equals_each_copy_per_market() {
    let frames = recording();
    let mut d = FrameDedupe::new(32_768, 2, 3_000);
    let admitted: Vec<&String> = frames.iter().filter(|(c, rx, raw)| d.admit(*c, raw.as_bytes(), rx / 1_000_000)).map(|x| &x.2).collect();
    let copy_a: Vec<&String> = frames.iter().filter(|x| x.0 == 0).map(|x| &x.2).collect();
    assert!(copy_a.len() > 1000, "{}", copy_a.len());
    let (mut got, mut want) = (admitted.clone(), copy_a.clone());
    got.sort();
    want.sort();
    assert!(got == want, "the merged stream holds each frame of one copy exactly once");
    assert_eq!(by_market(&admitted), by_market(&copy_a), "and in the copy's order for each market");
    assert_eq!(d.dropped() as usize, copy_a.len(), "every frame came twice");
}

// ---------------------------------------------------------------- no allocation

thread_local! {
    /// Allocations made by this thread while it is counting: the test harness allocates on its own
    /// threads, which do not count. Plain `Cell`s need no allocation to reach.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    if COUNTING.with(Cell::get) {
        ALLOCATIONS.with(|n| n.set(n.get() + 1));
    }
}

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

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn no_allocation_after_warm_up() {
    let frames = recording();
    let mut d = FrameDedupe::new(32_768, 2, 3_000);
    let (warm, rest) = frames.split_at(100);
    for (c, rx, raw) in warm {
        d.admit(*c, raw.as_bytes(), rx / 1_000_000);
    }
    COUNTING.with(|c| c.set(true));
    let mut admitted = 0;
    for (c, rx, raw) in rest {
        admitted += usize::from(d.admit(*c, raw.as_bytes(), rx / 1_000_000));
    }
    COUNTING.with(|c| c.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    assert!(admitted > 1000, "{admitted}");
}
