//! The schedule of a window by the exchange's clock: which window to watch, and the hot thread's
//! ticks (spec §5.3: every 50 ms in 835–872 s and exactly at 840.000).

use v21_live::hot::timer::{next_tick_ms, window_plan, Ticker, WindowPlan};

/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const S0: i64 = S * 1000;

#[test]
fn window_plan_times() {
    let p = window_plan(S0 + 300_000, None, 600, 880);
    let want = WindowPlan { s: S, gamma_at_ms: S0 + 540_000, connect_at_ms: S0 + 600_000, t840_ms: S0 + 840_000, close_at_ms: S0 + 880_000, resumed: false };
    assert_eq!(p, want);
    // a start in the decision minute: the same window, as after a drop of the feed
    let p = window_plan(S0 + 851_000, None, 600, 880);
    assert_eq!((p.s, p.resumed), (S, true));
    assert!(window_plan(S0 + 835_500, None, 600, 880).resumed);
    assert!(!window_plan(S0 + 835_000, None, 600, 880).resumed);
    // no time left before 870 s: the next window
    let p = window_plan(S0 + 870_000, None, 600, 880);
    assert_eq!((p.s, p.resumed), (S + 900, false));
    // a window done is not taken again
    let p = window_plan(S0 + 300_000, Some(S), 600, 880);
    assert_eq!((p.s, p.gamma_at_ms), (S + 900, S0 + 1_440_000));
}

#[test]
fn the_hot_timer_ticks_every_50_ms_in_the_minute_and_at_840_000() {
    // before 835 s: every 100 ms, but 835.000 itself
    assert_eq!(next_tick_ms(S0 + 500_000, Some(S)), S0 + 500_100);
    assert_eq!(next_tick_ms(S0 + 834_950, Some(S)), S0 + 835_000);
    // in 835–872 s: the next multiple of 50 ms, 840.000 among them
    assert_eq!(next_tick_ms(S0 + 835_000, Some(S)), S0 + 835_050);
    assert_eq!(next_tick_ms(S0 + 839_987, Some(S)), S0 + 840_000);
    assert_eq!(next_tick_ms(S0 + 840_000, Some(S)), S0 + 840_050);
    assert_eq!(next_tick_ms(S0 + 871_990, Some(S)), S0 + 872_000);
    // after 872 s, or without a window: every 100 ms
    assert_eq!(next_tick_ms(S0 + 872_000, Some(S)), S0 + 872_100);
    assert_eq!(next_tick_ms(S0 + 1, None), S0 + 101);
}

#[test]
fn frames_do_not_put_off_the_tick() {
    // the hot loop wakes for every frame (2 500 a second at the busiest); the tick it waits for stays
    // where it was, so a frame every millisecond cannot put it off (the home run of 20.10: the health
    // records of the minutes after the copies connected came up to 40 s late)
    let mut t = Ticker::default();
    assert_eq!(t.wait_ms(S0 + 500_000, Some(S)), 100);
    for now in S0 + 500_001..S0 + 500_100 {
        assert!(t.wait_ms(now, Some(S)) > 0);
    }
    assert_eq!(t.wait_ms(S0 + 500_100, Some(S)), 0);
    // a loop kept busy past it is owed the tick at once
    assert_eq!(t.wait_ms(S0 + 500_250, Some(S)), 0);
    // the tick taken, the next one is counted from then
    t.ticked();
    assert_eq!(t.wait_ms(S0 + 500_250, Some(S)), 100);
    // in the minute: its grid
    t.ticked();
    assert_eq!(t.wait_ms(S0 + 839_987, Some(S)), 13);
}

#[test]
fn a_clock_set_back_does_not_hold_the_tick() {
    // the clock's offset comes after the start: the exchange's clock can move back by days (the
    // end-to-end stand) or by the drift of a PC; the tick waited for is counted again from the new time
    let mut t = Ticker::default();
    assert_eq!(t.wait_ms(S0 + 500_000, Some(S)), 100);
    assert_eq!(t.wait_ms(S0 + 100_000, Some(S)), 100);
}
