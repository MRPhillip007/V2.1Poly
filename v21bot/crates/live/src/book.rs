//! The order book of one token on the 0.001 grid, both sides (spec §5.2; §9: prices are whole
//! ticks). The shares at each tick live in an array indexed by the tick and each side's best level
//! is kept as an index: `pmbot/src/hot/book.rs::AskBook`, mirrored for the bids. The rules are those
//! of `paper/book.py`: a size is the new total of its level (0 removes it), and `trim` drops the
//! levels that the server's best prices show a trade has taken.

use std::cmp::Ordering;

/// The top tick: prices are `0..=1` in steps of 0.001.
pub const MAX_TICK: u16 = 1000;

const SLOTS: usize = MAX_TICK as usize + 1;

/// No level on that side.
const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSide {
    /// BUY orders.
    Bid,
    /// SELL orders: what a buy takes.
    Ask,
}

/// The tick of a price as the scanner's `Book` keeps it: the price rounded to 4 decimals. `None` if
/// that is not a tick of the 0.001 grid inside `0..=1`, or not a number.
pub fn grid_tick(price: f64) -> Option<u16> {
    // in ten-thousandths of a dollar, which is what `round(price, 4)` keeps
    let ten_thousandths = (price * 10_000.0).round();
    // NaN is in no range; a tick of the grid is ten of them
    if (0.0..=10_000.0).contains(&ten_thousandths) && ten_thousandths % 10.0 == 0.0 {
        Some((ten_thousandths / 10.0) as u16)
    } else {
        None
    }
}

fn tick_price(t: usize) -> f64 {
    t as f64 / 1000.0
}

/// The book of one token.
pub struct Book {
    /// Shares at the price of each tick; 0.0 where nothing rests.
    bids: Box<[f64; SLOTS]>,
    asks: Box<[f64; SLOTS]>,
    /// The highest bid tick with shares, or `NONE`.
    best_bid: usize,
    /// The lowest ask tick with shares, or `NONE`.
    best_ask: usize,
}

impl Default for Book {
    fn default() -> Self {
        Self::new()
    }
}

impl Book {
    pub fn new() -> Book {
        Book { bids: Box::new([0.0; SLOTS]), asks: Box::new([0.0; SLOTS]), best_bid: NONE, best_ask: NONE }
    }

    /// Both sides empty; also the start of a snapshot, whose levels then come through `set`.
    pub fn clear(&mut self) {
        self.bids.fill(0.0);
        self.asks.fill(0.0);
        self.best_bid = NONE;
        self.best_ask = NONE;
    }

    /// The first ask tick with shares at or above `from`, or `NONE`.
    fn ask_from(&self, from: usize) -> usize {
        self.asks.get(from..).and_then(|rest| rest.iter().position(|&s| s > 0.0)).map_or(NONE, |at| from + at)
    }

    /// The last bid tick with shares below `below`, or `NONE`.
    fn bid_below(&self, below: usize) -> usize {
        self.bids[..below].iter().rposition(|&s| s > 0.0).unwrap_or(NONE)
    }

    /// Sets the shares at a tick to `size`: above zero sets the level, anything else (zero,
    /// negative, NaN) removes it. A tick beyond the grid is ignored.
    pub fn set(&mut self, side: BookSide, tick: u16, size: f64) {
        let at = usize::from(tick);
        if at >= SLOTS {
            return;
        }
        let keep = size > 0.0;
        match side {
            BookSide::Ask => {
                self.asks[at] = if keep { size } else { 0.0 };
                if keep {
                    self.best_ask = self.best_ask.min(at);
                } else if at == self.best_ask {
                    self.best_ask = self.ask_from(at + 1);
                }
            }
            BookSide::Bid => {
                self.bids[at] = if keep { size } else { 0.0 };
                if keep {
                    self.best_bid = if self.best_bid == NONE { at } else { self.best_bid.max(at) };
                } else if at == self.best_bid {
                    self.best_bid = self.bid_below(at);
                }
            }
        }
    }

    /// Drops the levels better than the server's best prices by more than 1e-9: bids above
    /// `best_bid`, asks below `best_ask` (an empty side comes as bid 0.0 / ask 1.0). A NaN drops the
    /// whole side, as in `paper`.
    pub fn trim(&mut self, best_bid: f64, best_ask: f64) {
        // a level stays if it is at or behind the server's price; a NaN compares with nothing
        let floor = best_ask - 1e-9;
        while self.best_ask != NONE && !tick_price(self.best_ask).partial_cmp(&floor).is_some_and(Ordering::is_ge) {
            self.asks[self.best_ask] = 0.0;
            self.best_ask = self.ask_from(self.best_ask + 1);
        }
        let ceiling = best_bid + 1e-9;
        while self.best_bid != NONE && !tick_price(self.best_bid).partial_cmp(&ceiling).is_some_and(Ordering::is_le) {
            self.bids[self.best_bid] = 0.0;
            self.best_bid = self.bid_below(self.best_bid);
        }
    }

    /// The best bid, 0.0 if there is none.
    pub fn best_bid(&self) -> f64 {
        if self.best_bid == NONE { 0.0 } else { tick_price(self.best_bid) }
    }

    /// The best ask, 1.0 if there is none.
    pub fn best_ask(&self) -> f64 {
        if self.best_ask == NONE { 1.0 } else { tick_price(self.best_ask) }
    }

    /// The ask levels `(price, shares)` priced at or below `limit`, cheapest first, into `out`
    /// (cleared first).
    pub fn asks_upto(&self, limit: f64, out: &mut Vec<(f64, f64)>) {
        out.clear();
        if self.best_ask == NONE {
            return;
        }
        for t in self.best_ask..SLOTS {
            let p = tick_price(t);
            if p > limit {
                break;
            }
            if self.asks[t] > 0.0 {
                out.push((p, self.asks[t]));
            }
        }
    }

    /// The check of spec §5.2: the best levels of this book are the server's `best_bid` and
    /// `best_ask`, to 1e-9.
    pub fn matches(&self, best_bid: f64, best_ask: f64) -> bool {
        (self.best_bid() - best_bid).abs() <= 1e-9 && (self.best_ask() - best_ask).abs() <= 1e-9
    }
}
