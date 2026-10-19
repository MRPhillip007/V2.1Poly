//! The events of the market channel as the bot uses them (spec §5.2): the rows of the archive's bbo
//! table, which the detectors read, and the level changes the books are built from. `wire::read`
//! reads a frame; `frame_items` turns its events into items, in their order.

use crate::book::{grid_tick, BookSide};
use crate::hot::wire::{self, Change, Event, Frame, Kind, Level, List, Side, Token};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeedItem {
    /// A row of the archive's bbo table: after a snapshot its best prices (bid 0.0 / ask 1.0 for an
    /// empty side), after a change the server's `best_bid` / `best_ask` — as `paper/feeds.parse_ws`.
    Bbo { token: u8, ts_ms: i64, bid: f64, ask: f64 },
    /// A snapshot: the token's book becomes the `SnapLevel` items that follow.
    Snapshot { token: u8, ts_ms: i64 },
    SnapLevel { token: u8, side: BookSide, tick: u16, size: f64 },
    /// A change of a level: its new total size. `tick: None` is a price off the 0.001 grid — the
    /// level is skipped, the trim by the best prices stays.
    Level { token: u8, ts_ms: i64, side: BookSide, tick: Option<u16>, size: f64, best_bid: f64, best_ask: f64 },
    /// The market's new price step.
    TickSize { token: u8, ts_ms: i64, tick: f64 },
}

/// Adds the items of `frame`'s events to `out`, in their order. Returns how many parts could not be
/// read, as pmbot's feed counts them: an event without the fields it needs, a change with numbers or
/// a side that cannot be read, a price off the grid.
pub fn frame_items(frame: &Frame, out: &mut Vec<FeedItem>) -> u32 {
    let mut bad = 0;
    for event in &frame.events {
        bad += match event.kind {
            Kind::Book => book(event, &frame.rows, out),
            Kind::PriceChange => price_change(event, &frame.changes, out),
            Kind::TickSize => {
                tick_size(event, out);
                0
            }
            Kind::Other => 0,
        };
    }
    bad
}

/// Reads one websocket frame and adds its items to `out`: `wire::read`, then `frame_items`. Returns
/// how many parts could not be read; a frame that is not JSON is one, unless it is a PONG or empty.
pub fn read_frame(raw: &[u8], tokens: &[Box<str>], frame: &mut Frame, out: &mut Vec<FeedItem>) -> u32 {
    match wire::read(raw, tokens, frame) {
        Ok(()) => frame_items(frame, out),
        Err(_) => u32::from(!is_idle(raw)),
    }
}

/// A PONG (the answer to our text PING) or an empty frame: not JSON, and not a broken frame either.
fn is_idle(raw: &[u8]) -> bool {
    let text = raw.trim_ascii();
    text.is_empty() || text.eq_ignore_ascii_case(b"PONG")
}

/// The rows of one side of a `book`, or `None` if they cannot be read.
fn side_rows<'r>(list: &List, rows: &'r [Option<Level>]) -> Option<&'r [Option<Level>]> {
    match list {
        List::Unreadable => None,
        List::Empty => Some(&[]),
        List::Items(range) => Some(&rows[range.clone()]),
    }
}

/// The levels of a side that hold shares (a readable list holds readable rows only).
fn live(rows: &[Option<Level>]) -> impl Iterator<Item = &Level> {
    rows.iter().flatten().filter(|l| l.size > 0.0)
}

fn book(event: &Event, rows: &[Option<Level>], out: &mut Vec<FeedItem>) -> u32 {
    let token = match event.token {
        Token::Ours(ix) => ix,
        Token::Other => return 0,
        Token::Unreadable => return 1,
    };
    let Some(ts_ms) = event.ts else { return 1 };
    let (Some(bids), Some(asks)) = (side_rows(&event.bids, rows), side_rows(&event.asks, rows)) else { return 1 };
    let bid = live(bids).map(|l| l.price).reduce(f64::max).unwrap_or(0.0);
    let ask = live(asks).map(|l| l.price).reduce(f64::min).unwrap_or(1.0);
    // a snapshot is taken whole or not at all: a price off the grid has no cell
    let bad = if live(bids).chain(live(asks)).all(|l| grid_tick(l.price).is_some()) {
        out.push(FeedItem::Snapshot { token, ts_ms });
        for (side, levels) in [(BookSide::Bid, bids), (BookSide::Ask, asks)] {
            for l in live(levels) {
                if let Some(tick) = grid_tick(l.price) {
                    out.push(FeedItem::SnapLevel { token, side, tick, size: l.size });
                }
            }
        }
        0
    } else {
        1
    };
    out.push(FeedItem::Bbo { token, ts_ms, bid, ask });
    bad
}

fn price_change(event: &Event, changes: &[Change], out: &mut Vec<FeedItem>) -> u32 {
    let Some(ts_ms) = event.ts else { return 1 };
    let changes = match &event.changes {
        List::Unreadable => return 1,
        List::Empty => return 0,
        List::Items(range) => &changes[range.clone()],
    };
    let mut bad = 0;
    for c in changes {
        let token = match c.token {
            Token::Ours(ix) => ix,
            Token::Other => continue,
            Token::Unreadable => {
                bad += 1;
                continue;
            }
        };
        let (Some(price), Some(size)) = (c.price, c.size) else {
            bad += 1;
            continue;
        };
        let side = match c.side {
            Side::Buy => BookSide::Bid,
            Side::Sell => BookSide::Ask,
            Side::Invalid => {
                bad += 1;
                continue;
            }
        };
        let tick = grid_tick(price);
        bad += u32::from(tick.is_none());
        out.push(FeedItem::Level { token, ts_ms, side, tick, size, best_bid: c.best_bid, best_ask: c.best_ask });
        out.push(FeedItem::Bbo { token, ts_ms, bid: c.best_bid, ask: c.best_ask });
    }
    bad
}

fn tick_size(event: &Event, out: &mut Vec<FeedItem>) {
    let Token::Ours(token) = event.token else { return };
    if let (Some(ts_ms), Some(tick)) = (event.ts, event.tick.filter(|t| t.is_finite() && *t > 0.0)) {
        out.push(FeedItem::TickSize { token, ts_ms, tick });
    }
}
