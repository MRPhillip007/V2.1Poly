//! The Binance trade stream: its messages, the fallback address on HTTP 451, the gaps a drop leaves
//! and their backfill (the tests of `paper/feeds.BinanceFeed`), the klines and the time over REST.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::SinkExt;
use support::fake_ws::{record, FakeWs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use v21_core::live_closes::LiveCloses;
use v21_core::window::Coin;
use v21_live::cold::binance_rest::{klines, server_time_ms};
use v21_live::cold::http_client;
use v21_live::cold::ntp::clock_offset_ms;
use v21_live::hot::binance::{backfill_rows, parse_agg, BinanceEvent, BinanceStream, GapTracker, Trade};
use v21_live::hot::ws::CopyTimes;
use v21_live::shared::clock::local_ms;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn quick() -> CopyTimes {
    CopyTimes { ping: ms(50), silent: ms(300), retry_first: ms(10), retry_max: ms(40) }
}

/// An aggTrade message of the combined stream.
fn agg(symbol: &str, t: i64, p: &str) -> String {
    let stream = symbol.to_lowercase();
    format!(r#"{{"stream":"{stream}@aggTrade","data":{{"e":"aggTrade","E":{t},"s":"{symbol}","a":1,"p":"{p}","q":"0.1","f":1,"l":1,"T":{t},"m":false,"M":true}}}}"#)
}

fn trade(coin: Coin, ts_ms: i64, price: f64) -> Trade {
    Trade { coin, ts_ms, price }
}

#[test]
fn parse_agg_reads_a_real_message() {
    // the first Binance message of the recording of 2026-10-19 (tests/data/two_copies.jsonl.gz)
    let raw = r#"{"stream":"btcusdt@aggTrade","data":{"e":"aggTrade","E":1792381439493,"s":"BTCUSDT","a":4092924234,"p":"82514.60000000","q":"0.07383000","f":6773699718,"l":6773699718,"T":1792381439493,"m":false,"M":true}}"#;
    assert_eq!(parse_agg(raw.as_bytes()), Some(trade(Coin::Btc, 1_792_381_439_493, 82514.6)));
    // paper's example
    let raw = r#"{"stream": "solusdt@aggTrade", "data": {"e": "aggTrade", "E": 1790906083274, "s": "SOLUSDT", "p": "119.42000000", "q": "49.6", "T": 1790906083274}}"#;
    assert_eq!(parse_agg(raw.as_bytes()), Some(trade(Coin::Sol, 1_790_906_083_274, 119.42)));
    // a pair that is not ours, and garbage
    assert_eq!(parse_agg(br#"{"stream":"xrpusdt@aggTrade","data":{"s":"XRPUSDT","p":"1","T":1}}"#), None);
    assert_eq!(parse_agg(b"{oops"), None);
}

// ---------------------------------------------------------------- the stream and its addresses

/// A server that answers the first `refuse` websocket handshakes with HTTP 451, as stream.binance.com
/// does from some places, and serves the later ones with `send` and nothing more.
async fn refusing(refuse: usize, send: Option<String>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    // a base as Binance's: the streams are asked for in the query of /stream
    let url = format!("ws://{}/stream", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let (n, send) = (count.fetch_add(1, Ordering::SeqCst), send.clone());
            tokio::spawn(async move {
                if n < refuse {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).await;
                    let _ = stream.write_all(b"HTTP/1.1 451 Unavailable For Legal Reasons\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                } else if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                    if let Some(text) = send {
                        let _ = ws.send(Message::text(text)).await;
                    }
                    tokio::time::sleep(ms(2_000)).await;
                }
            });
        }
    });
    url
}

async fn next(s: &mut BinanceStream) -> BinanceEvent {
    timeout(ms(3_000), s.next()).await.expect("an event within 3 s")
}

#[tokio::test]
async fn falls_back_when_the_primary_answers_451() {
    let primary = refusing(usize::MAX, None).await;
    let fallback = FakeWs::start(|n, mut ws, log| async move {
        ws.send(Message::text(agg("BTCUSDT", 1_000, "100.5"))).await.unwrap();
        record(n, &mut ws, &log).await;
    })
    .await;
    let mut s = BinanceStream::with_times([primary, format!("{}/stream", fallback.url)], &[Coin::Btc, Coin::Eth, Coin::Sol], quick());
    assert_eq!(next(&mut s).await, BinanceEvent::Up { fallback: true });
    assert_eq!(next(&mut s).await, BinanceEvent::Trade(trade(Coin::Btc, 1_000, 100.5)));
}

#[tokio::test]
async fn returns_to_the_primary_after_the_fallback_drops() {
    let primary = refusing(1, Some(agg("ETHUSDT", 2_000, "3000.25"))).await;
    // the fallback sends one trade and closes
    let fallback = FakeWs::start(|_, mut ws, _| async move {
        ws.send(Message::text(agg("BTCUSDT", 1_000, "100.5"))).await.unwrap();
        let _ = ws.close(None).await;
    })
    .await;
    let mut s = BinanceStream::with_times([primary, format!("{}/stream", fallback.url)], &[Coin::Btc, Coin::Eth], quick());
    assert_eq!(next(&mut s).await, BinanceEvent::Up { fallback: true });
    assert_eq!(next(&mut s).await, BinanceEvent::Trade(trade(Coin::Btc, 1_000, 100.5)));
    assert!(matches!(next(&mut s).await, BinanceEvent::Down { .. }));
    assert_eq!(next(&mut s).await, BinanceEvent::Up { fallback: false });
    assert_eq!(next(&mut s).await, BinanceEvent::Trade(trade(Coin::Eth, 2_000, 3000.25)));
}

#[tokio::test]
async fn the_stream_asks_for_the_coins_trades() {
    let fake = FakeWs::start(|n, mut ws, log| async move { record(n, &mut ws, &log).await }).await;
    // the combined stream is chosen by the path: the fake server accepts any, so the url is checked
    let base = format!("{}/stream", fake.url);
    let s = BinanceStream::with_times([base.clone(), base.clone()], &[Coin::Btc, Coin::Sol], quick());
    assert_eq!(s.urls()[0], format!("{base}?streams=btcusdt@aggTrade/solusdt@aggTrade"));
}

// ---------------------------------------------------------------- gaps and backfill (paper)

fn closes_get(c: &LiveCloses, q0: i64, q1: i64) -> Option<Vec<f64>> {
    let mut out = vec![];
    c.get(q0, q1, &mut out).then_some(out)
}

/// Feeds a trade to both, as the hot thread does.
fn feed(g: &mut GapTracker, c: &mut LiveCloses, t: Trade) -> Option<(Coin, i64, i64)> {
    c.add_trade(t.ts_ms, t.price);
    g.on_trade(&t)
}

fn disconnect(g: &mut GapTracker, c: &mut LiveCloses, at_ms: i64) -> Vec<(Coin, i64, i64)> {
    let mut gaps = vec![];
    g.on_disconnect(at_ms, &mut gaps);
    for &(_, q0, q1) in &gaps {
        c.mark_gap(q0, q1);
    }
    gaps
}

#[test]
fn a_drop_marks_a_gap_and_the_next_trade_asks_for_a_backfill() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Btc]), LiveCloses::new());
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 100_200, 10.0)), None);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 101_500, 11.0)), None); // a partial close of second 101
    assert_eq!(disconnect(&mut g, &mut c, 103_400), [(Coin::Btc, 101, 103)]);
    assert_eq!(closes_get(&c, 100, 103), None, "lost seconds are refused, not forward-filled");
    // the first trade after the reconnect
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 106_100, 12.0)), Some((Coin::Btc, 101, 105)));
    let mut rows = vec![(101_000, 11.2), (102_000, 11.2), (103_000, 11.5), (104_000, 11.5), (105_000, 11.5)];
    assert!(backfill_rows(&mut rows, 105));
    c.add_klines(&rows);
    assert_eq!(closes_get(&c, 100, 106), Some(vec![10.0, 11.2, 11.2, 11.5, 11.5, 11.5, 12.0]));
}

#[test]
fn a_short_backfill_is_closed_through_q1() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Eth]), LiveCloses::new());
    feed(&mut g, &mut c, trade(Coin::Eth, 199_100, 4.0));
    disconnect(&mut g, &mut c, 210_500);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Eth, 214_000, 6.0)), Some((Coin::Eth, 199, 213)));
    let mut rows = vec![(200_000, 5.0), (199_000, 4.0)]; // any order
    assert!(backfill_rows(&mut rows, 213));
    c.add_klines(&rows);
    let mut want = vec![4.0];
    want.extend([5.0; 14]);
    want.push(6.0);
    assert_eq!(closes_get(&c, 199, 214), Some(want));
}

#[test]
fn an_empty_backfill_keeps_the_gap() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Eth]), LiveCloses::new());
    feed(&mut g, &mut c, trade(Coin::Eth, 199_100, 4.0));
    disconnect(&mut g, &mut c, 200_500);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Eth, 204_000, 6.0)), Some((Coin::Eth, 199, 203)));
    let mut rows = vec![];
    assert!(!backfill_rows(&mut rows, 203), "nothing to add: the caller asks again");
    assert_eq!(closes_get(&c, 199, 204), None);
}

#[test]
fn a_repeated_disconnect_extends_the_same_gap() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Btc]), LiveCloses::new());
    feed(&mut g, &mut c, trade(Coin::Btc, 100_200, 10.0));
    assert_eq!(disconnect(&mut g, &mut c, 102_000), [(Coin::Btc, 100, 102)]);
    // the reconnect failed, no trade in between
    assert_eq!(disconnect(&mut g, &mut c, 105_000), [(Coin::Btc, 100, 105)]);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 107_000, 12.0)), Some((Coin::Btc, 100, 106)));
}

#[test]
fn closes_after_a_warm_up_without_the_stream_wait_for_its_first_trade() {
    // the warm-up's klines came before the stream's first trade (paper warmed up only once the stream
    // was up): the seconds after them are a gap until that trade, which asks for their klines
    let (mut g, mut c) = (GapTracker::new(&[Coin::Btc]), LiveCloses::new());
    let warm = [(98_000, 9.0), (99_000, 10.0)];
    c.add_klines(&warm);
    assert_eq!(g.on_warm_up(Coin::Btc, &warm), Some((Coin::Btc, 100, 100)));
    c.mark_gap(100, 100);
    assert_eq!(closes_get(&c, 99, 103), None, "not carried over the time without the stream");
    // a trade from before the gap leaves it open
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 99_500, 10.5)), None);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Btc, 104_100, 12.0)), Some((Coin::Btc, 100, 103)));
}

#[test]
fn a_drop_before_the_first_trade_after_a_warm_up_starts_at_the_last_trade() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Btc]), LiveCloses::new());
    assert_eq!(g.on_warm_up(Coin::Btc, &[(99_000, 10.0)]), Some((Coin::Btc, 100, 100)));
    feed(&mut g, &mut c, trade(Coin::Btc, 99_500, 10.5));
    assert_eq!(disconnect(&mut g, &mut c, 101_200), [(Coin::Btc, 99, 101)]);
}

#[test]
fn a_warm_up_after_the_first_trade_leaves_no_gap() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Btc]), LiveCloses::new());
    feed(&mut g, &mut c, trade(Coin::Btc, 100_200, 10.0));
    assert_eq!(g.on_warm_up(Coin::Btc, &[(99_000, 10.0)]), None);
    // nor do the klines of a backfill
    assert_eq!(g.on_warm_up(Coin::Btc, &[(101_000, 10.0)]), None);
}

#[test]
fn a_disconnect_before_any_trade_marks_nothing() {
    let (mut g, mut c) = (GapTracker::new(&[Coin::Sol]), LiveCloses::new());
    assert_eq!(disconnect(&mut g, &mut c, 50_000), []);
    assert_eq!(feed(&mut g, &mut c, trade(Coin::Sol, 60_000, 1.0)), None);
}

// ---------------------------------------------------------------- REST

/// An HTTP server on a free port: `/api/v3/klines` serves a row per second asked (close = the
/// second), `/api/v3/time` the server time 1 234; every request is recorded. With `refuse` every
/// request gets 451.
async fn binance_rest(refuse: bool) -> (String, Arc<Mutex<Vec<String>>>) {
    use axum::extract::{Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use std::collections::HashMap;

    type Asked = Arc<Mutex<Vec<String>>>;
    async fn klines_route(State((asked, refuse)): State<(Asked, bool)>, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
        asked.lock().unwrap().push(format!("klines {} {} {}", q["symbol"], q["startTime"], q["endTime"]));
        if refuse {
            return StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response();
        }
        let (start, end): (i64, i64) = (q["startTime"].parse().unwrap(), q["endTime"].parse().unwrap());
        let rows: Vec<serde_json::Value> = (start / 1000..=end / 1000)
            .take(q["limit"].parse().unwrap())
            .map(|s| serde_json::json!([s * 1000, "1", "1", "1", format!("{s}.0"), "0", s * 1000 + 999, "0", 1, "0", "0", "0"]))
            .collect();
        axum::Json(rows).into_response()
    }
    async fn time_route(State((asked, refuse)): State<(Asked, bool)>) -> axum::response::Response {
        asked.lock().unwrap().push("time".into());
        if refuse {
            return StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response();
        }
        axum::Json(serde_json::json!({ "serverTime": 1234 })).into_response()
    }
    let asked: Asked = Arc::default();
    let app = axum::Router::new()
        .route("/api/v3/klines", get(klines_route))
        .route("/api/v3/time", get(time_route))
        .with_state((asked.clone(), refuse));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, asked)
}

#[tokio::test]
async fn klines_page_by_1000_and_fall_back_on_451() {
    let (refused, refused_asked) = binance_rest(true).await;
    let (base, asked) = binance_rest(false).await;
    let rows = klines(&http_client(), &[refused, base], "BTCUSDT", 1_000, 3_499).await.unwrap();
    assert_eq!(rows.len(), 2_500);
    assert_eq!((rows[0], rows[2_499]), ((1_000_000, 1_000.0), (3_499_000, 3_499.0)));
    assert_eq!(
        *asked.lock().unwrap(),
        ["klines BTCUSDT 1000000 1999999", "klines BTCUSDT 2000000 2999999", "klines BTCUSDT 3000000 3499999"]
    );
    assert_eq!(refused_asked.lock().unwrap().len(), 3, "each page tried the first address first");
}

#[tokio::test]
async fn server_time_tries_the_fallback() {
    let (refused, _) = binance_rest(true).await;
    let (base, _) = binance_rest(false).await;
    let (server, sent, received) = server_time_ms(&http_client(), &[refused, base]).await.unwrap();
    assert_eq!(server, 1234);
    assert!(sent <= received);
    let (refused, _) = binance_rest(true).await;
    assert!(server_time_ms(&http_client(), &[refused]).await.is_err());
}

/// A Binance REST whose `/api/v3/time` is this machine's clock, over keep-alive connections: a new
/// connection is served `connect_ms` after it opens (as a TLS handshake far away takes), and every
/// answer has `status` (200 or 451) after `answer_ms`.
async fn clock_rest(connect_ms: u64, status: u16, answer_ms: u64) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(ms(connect_ms)).await;
                let (mut buf, mut have) = (vec![0u8; 8192], 0);
                loop {
                    let n = match sock.read(&mut buf[have..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    have += n;
                    while let Some(end) = buf[..have].windows(4).position(|w| w == b"\r\n\r\n") {
                        tokio::time::sleep(ms(answer_ms)).await;
                        let body = if status == 200 { format!(r#"{{"serverTime":{}}}"#, local_ms()) } else { String::new() };
                        let head = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n", body.len());
                        if sock.write_all(format!("{head}{body}").as_bytes()).await.is_err() {
                            return;
                        }
                        buf.copy_within(end + 4..have, 0);
                        have -= end + 4;
                    }
                }
            });
        }
    });
    base
}

#[tokio::test]
async fn the_server_time_is_timed_on_the_address_that_answers() {
    // the primary refuses after 300 ms (from the PC at home: Binance +390 ms against NTP −23 ms)
    let refused = clock_rest(0, 451, 300).await;
    let mirror = clock_rest(0, 200, 0).await;
    let (server, sent, received) = server_time_ms(&http_client(), &[refused, mirror]).await.unwrap();
    let offset = clock_offset_ms(server, sent, received);
    assert!(offset.abs() <= 20, "offset {offset} ms");
}

#[tokio::test]
async fn the_server_time_is_timed_over_a_connection_already_open() {
    // a new connection takes 300 ms to serve: the time is not taken over it
    let slow = clock_rest(300, 200, 0).await;
    let (server, sent, received) = server_time_ms(&http_client(), &[slow]).await.unwrap();
    let offset = clock_offset_ms(server, sent, received);
    assert!(offset.abs() <= 20, "offset {offset} ms");
}
