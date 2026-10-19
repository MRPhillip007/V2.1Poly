//! The Binance trade stream (spec §4.2): the aggTrade of the watched coins on one connection, from
//! stream.binance.com, else from its mirror (stream.binance.com answers HTTP 451 from some places).
//! A drop leaves a gap in the closes, repaired by REST klines once trades come again — the rules of
//! `paper/feeds.BinanceFeed`.

use std::future::poll_fn;
use std::task::{ready, Poll};

use serde::Deserialize;
use v21_core::window::Coin;

use crate::hot::ws::{CopyTimes, Socket, SocketEvent};

/// A trade of a watched coin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trade {
    pub coin: Coin,
    /// The trade time, unix ms (Binance's clock).
    pub ts_ms: i64,
    pub price: f64,
}

#[derive(Deserialize)]
struct Combined<'a> {
    #[serde(borrow)]
    data: Agg<'a>,
}

#[derive(Deserialize)]
struct Agg<'a> {
    s: &'a str,
    p: &'a str,
    #[serde(rename = "T")]
    t: i64,
}

fn coin_of(symbol: &str) -> Option<Coin> {
    [Coin::Btc, Coin::Eth, Coin::Sol].into_iter().find(|c| c.binance_symbol() == symbol)
}

/// The trade of an aggTrade message of the combined stream (`data.s`, `data.T`, `data.p`); `None`
/// for anything else. The fields are borrowed from the message: nothing is allocated.
pub fn parse_agg(raw: &[u8]) -> Option<Trade> {
    let msg: Combined = serde_json::from_slice(raw).ok()?;
    Some(Trade { coin: coin_of(msg.data.s)?, ts_ms: msg.data.t, price: msg.data.p.parse().ok()? })
}

#[derive(Clone, Debug, PartialEq)]
pub enum BinanceEvent {
    Trade(Trade),
    /// Connected; `fallback` — to the mirror.
    Up { fallback: bool },
    /// The connection ended; the stream connects again by itself, to the primary address first.
    Down { reason: String },
}

/// The trade stream of the coins: the primary address first, the mirror if it fails, and the
/// primary again after the mirror drops.
pub struct BinanceStream {
    socket: Socket,
    urls: [String; 2],
}

impl BinanceStream {
    /// `urls` are the primary and the mirror bases (`wss://.../stream`).
    pub fn new(urls: [String; 2], coins: &[Coin]) -> BinanceStream {
        BinanceStream::with_times(urls, coins, CopyTimes::default())
    }

    pub fn with_times(bases: [String; 2], coins: &[Coin], times: CopyTimes) -> BinanceStream {
        let streams: Vec<String> = coins.iter().map(|c| format!("{}@aggTrade", c.binance_symbol().to_lowercase())).collect();
        let urls = bases.map(|base| format!("{base}?streams={}", streams.join("/")));
        BinanceStream { socket: Socket::new(urls.to_vec(), None, false, times), urls }
    }

    /// The addresses connected to: the primary and the mirror, with the streams asked.
    pub fn urls(&self) -> &[String; 2] {
        &self.urls
    }

    /// The next trade or change of the connection. Cancel-safe; messages that are not trades of the
    /// coins are skipped.
    pub async fn next(&mut self) -> BinanceEvent {
        poll_fn(|cx| loop {
            match ready!(self.socket.poll_event(cx)) {
                SocketEvent::Frame(raw) => {
                    if let Some(t) = parse_agg(&raw) {
                        return Poll::Ready(BinanceEvent::Trade(t));
                    }
                }
                SocketEvent::Up { url } => return Poll::Ready(BinanceEvent::Up { fallback: url > 0 }),
                SocketEvent::Down { code, reason } => {
                    let reason = code.map_or(reason.clone(), |c| format!("{c}: {reason}"));
                    return Poll::Ready(BinanceEvent::Down { reason });
                }
            }
        })
        .await
    }
}

fn slot(coin: Coin) -> usize {
    match coin {
        Coin::Btc => 0,
        Coin::Eth => 1,
        Coin::Sol => 2,
    }
}

/// The gaps a drop of the stream leaves in the closes, and the backfills they need: the port of
/// `paper/feeds.BinanceFeed`'s `on_trade` / `on_disconnect`. The caller marks the gaps in its closes
/// and fetches the backfills.
pub struct GapTracker {
    coins: Vec<Coin>,
    /// The latest trade time of each coin.
    last_ms: [Option<i64>; 3],
    /// The first second of the gap of each coin, while one is open.
    gap_start: [Option<i64>; 3],
}

impl GapTracker {
    pub fn new(coins: &[Coin]) -> GapTracker {
        GapTracker { coins: coins.to_vec(), last_ms: [None; 3], gap_start: [None; 3] }
    }

    /// A trade came: if it is the first one after a drop, the seconds `q0..=q1` to fill from klines
    /// (the second of the last trade before the drop up to the one before this trade).
    pub fn on_trade(&mut self, t: &Trade) -> Option<(Coin, i64, i64)> {
        if !self.coins.contains(&t.coin) {
            return None;
        }
        let i = slot(t.coin);
        let q = t.ts_ms.div_euclid(1000);
        let backfill = self.gap_start[i].take().filter(|&start| q > start).map(|start| (t.coin, start, q - 1));
        self.last_ms[i] = Some(self.last_ms[i].map_or(t.ts_ms, |last| last.max(t.ts_ms)));
        backfill
    }

    /// The stream dropped at `at_ms`: for each coin that has traded, the gap from the second of its
    /// last trade (its close may be partial) to the drop's second goes into `out`. A drop while a gap
    /// is open extends that gap.
    pub fn on_disconnect(&mut self, at_ms: i64, out: &mut Vec<(Coin, i64, i64)>) {
        for &coin in &self.coins {
            let i = slot(coin);
            let Some(last) = self.last_ms[i] else { continue };
            let start = self.gap_start[i].unwrap_or(last.div_euclid(1000));
            out.push((coin, start, at_ms.div_euclid(1000)));
            self.gap_start[i] = Some(start);
        }
    }
}

/// Makes the rows of a klines answer for the seconds up to `q1` ready for `LiveCloses::add_klines`:
/// in time order, and closed through `q1` (the price in force carries to the end of the hole, as
/// `paper`'s `backfill` does). False if there are none: the hole stays, to be asked again.
pub fn backfill_rows(rows: &mut Vec<(i64, f64)>, q1: i64) -> bool {
    rows.sort_by_key(|r| r.0);
    let Some(&(open_ms, close)) = rows.last() else { return false };
    if open_ms.div_euclid(1000) < q1 {
        rows.push((q1 * 1000, close));
    }
    true
}
