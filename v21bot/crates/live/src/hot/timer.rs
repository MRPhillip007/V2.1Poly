//! The schedule of a window by the exchange's clock (spec §5.3, the Global Constraints of milestone 2):
//! which window to watch, and when the hot thread ticks.

/// In the decision minute the hot thread ticks every this many ms (840.000 is one of them).
const TICK_MS: i64 = 50;
/// Outside it, the queue from the cold runtime is read this often.
const IDLE_TICK_MS: i64 = 100;
/// The decision minute of the ticks: these ms of the window.
const MINUTE_FROM_MS: i64 = 835_000;
const MINUTE_TO_MS: i64 = 872_000;
/// Gamma is asked from this second of the window.
const GAMMA_AT_S: i64 = 540;
/// A window can still be joined before this second of it.
const JOIN_BEFORE_S: i64 = 870;
/// A start after this ms of the window is a late one.
const LATE_AFTER_MS: i64 = 835_000;

/// The moments of a window, exchange ms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowPlan {
    pub s: i64,
    pub gamma_at_ms: i64,
    pub connect_at_ms: i64,
    pub t840_ms: i64,
    pub close_at_ms: i64,
    /// The bot starts after the 835th second: the window begins as after a drop of the feed.
    pub resumed: bool,
}

/// The current window, if there is still time before its 870th second, else the next one; the window
/// `last_s` (and those before it) are not taken again. The copies connect at `open_s` and close at
/// `close_s` seconds of the window.
pub fn window_plan(now_ms: i64, last_s: Option<i64>, open_s: i64, close_s: i64) -> WindowPlan {
    let current = now_ms.div_euclid(900_000) * 900;
    let mut s = if now_ms < (current + JOIN_BEFORE_S) * 1000 { current } else { current + 900 };
    if let Some(last) = last_s
        && s <= last
    {
        s = last + 900;
    }
    let s0 = s * 1000;
    WindowPlan {
        s,
        gamma_at_ms: s0 + GAMMA_AT_S * 1000,
        connect_at_ms: s0 + open_s * 1000,
        t840_ms: s0 + 840_000,
        close_at_ms: s0 + close_s * 1000,
        resumed: now_ms > s0 + LATE_AFTER_MS,
    }
}

/// The hot thread's next tick after `now_ms` for the window `s`: every 50 ms in 835–872 s of it, on
/// the multiples of 50 ms (840.000 among them); else every 100 ms, but 835.000 itself.
pub fn next_tick_ms(now_ms: i64, s: Option<i64>) -> i64 {
    let Some(s) = s else { return now_ms + IDLE_TICK_MS };
    let (from, to) = (s * 1000 + MINUTE_FROM_MS, s * 1000 + MINUTE_TO_MS);
    if now_ms < from {
        (now_ms + IDLE_TICK_MS).min(from)
    } else if now_ms < to {
        (now_ms.div_euclid(TICK_MS) + 1) * TICK_MS
    } else {
        now_ms + IDLE_TICK_MS
    }
}
