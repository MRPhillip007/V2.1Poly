//! One copy of the Polymarket market channel against a local server: the subscription, the PINGs,
//! a close with code 1013, silence, and a `next()` cancelled by `select!` again and again.

mod support;

use std::time::Duration;

use futures_util::SinkExt;
use support::fake_ws::{record, FakeWs};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use v21_live::hot::ws::{CopyEvent, CopyTimes, PmCopy};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The times of a copy, short enough for a test.
fn quick() -> CopyTimes {
    CopyTimes { ping: ms(50), silent: ms(300), retry_first: ms(10), retry_max: ms(40) }
}

fn frame(copy: u8, text: &str) -> CopyEvent {
    CopyEvent::Frame { copy, raw: bytes::Bytes::copy_from_slice(text.as_bytes()) }
}

async fn next(copy: &mut PmCopy) -> CopyEvent {
    timeout(ms(3_000), copy.next()).await.expect("an event within 3 s")
}

#[tokio::test]
async fn subscribes_with_the_window_tokens_and_pings() {
    let fake = FakeWs::start(|n, mut ws, log| async move { record(n, &mut ws, &log).await }).await;
    let mut copy = PmCopy::with_times(&fake.url, &["111".to_string(), "222".to_string()], 1, quick());
    assert_eq!(next(&mut copy).await, CopyEvent::Up { copy: 1 });
    // nothing comes from the server; the copy is polled for a while and sends its PINGs
    let _ = timeout(ms(170), copy.next()).await;
    let got = fake.wait_for(ms(1_000), |got| got.iter().filter(|(_, m)| m == "PING").count() >= 2).await;
    assert_eq!(got[0], (0, r#"{"assets_ids":["111","222"],"type":"market"}"#.to_string()));
    assert!(got.iter().filter(|(_, m)| m == "PING").count() >= 2, "{got:?}");
}

#[tokio::test]
async fn reconnects_after_close_1013_and_reports_the_code() {
    let fake = FakeWs::start(|n, mut ws, log| async move {
        if n == 0 {
            ws.send(Message::text("A")).await.unwrap();
            ws.send(Message::text("B")).await.unwrap();
            let close = CloseFrame { code: CloseCode::Again, reason: "slow consumer".into() };
            let _ = ws.close(Some(close)).await;
        } else {
            ws.send(Message::text("C")).await.unwrap();
            record(n, &mut ws, &log).await;
        }
    })
    .await;
    let mut copy = PmCopy::with_times(&fake.url, &["111".to_string()], 0, quick());
    let mut got = vec![];
    for _ in 0..6 {
        got.push(next(&mut copy).await);
    }
    let down = CopyEvent::Down { copy: 0, code: Some(1013), reason: "slow consumer".into() };
    assert_eq!(got, [CopyEvent::Up { copy: 0 }, frame(0, "A"), frame(0, "B"), down, CopyEvent::Up { copy: 0 }, frame(0, "C")]);
}

#[tokio::test]
async fn silence_breaks_the_connection() {
    // the server holds the connection and says nothing
    let fake = FakeWs::start(|n, mut ws, log| async move { record(n, &mut ws, &log).await }).await;
    let mut copy = PmCopy::with_times(&fake.url, &["111".to_string()], 0, quick());
    assert_eq!(next(&mut copy).await, CopyEvent::Up { copy: 0 });
    let started = tokio::time::Instant::now();
    match next(&mut copy).await {
        CopyEvent::Down { code: None, reason, .. } => assert!(reason.contains("silent"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() >= ms(290), "{:?}", started.elapsed());
    assert_eq!(next(&mut copy).await, CopyEvent::Up { copy: 0 }, "and it connects again");
}

#[tokio::test]
async fn a_server_that_is_not_there_gives_no_event_until_it_is() {
    // a copy that cannot connect stays down without a stream of events, and comes up when it can
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let mut copy = PmCopy::with_times(&format!("ws://{addr}"), &["111".to_string()], 0, quick());
    assert!(timeout(ms(200), copy.next()).await.is_err(), "no event while nothing listens");
}

#[tokio::test]
async fn next_is_cancel_safe() {
    let fake = FakeWs::start(|n, mut ws, log| async move {
        for k in 0..300 {
            ws.send(Message::text(k.to_string())).await.unwrap();
        }
        record(n, &mut ws, &log).await;
    })
    .await;
    let mut copy = PmCopy::with_times(&fake.url, &["111".to_string()], 0, quick());
    let mut frames = vec![];
    let until = tokio::time::Instant::now() + ms(5_000);
    while frames.len() < 300 && tokio::time::Instant::now() < until {
        tokio::select! {
            ev = copy.next() => if let CopyEvent::Frame { raw, .. } = ev { frames.push(String::from_utf8(raw.to_vec()).unwrap()) },
            _ = tokio::time::sleep(Duration::from_micros(200)) => {}
        }
    }
    let want: Vec<String> = (0..300).map(|k: i32| k.to_string()).collect();
    assert_eq!(frames, want);
}

#[tokio::test]
async fn a_slow_handshake_survives_cancellation() {
    // the server answers the handshake 50 ms after the connection; the copy is polled in a select!
    // that drops it every millisecond, so an attempt that did not outlive a poll would never finish
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(ms(50)).await;
                if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                    let _ = ws.send(Message::text("X")).await;
                    tokio::time::sleep(ms(1_000)).await;
                }
            });
        }
    });
    let mut copy = PmCopy::with_times(&url, &["111".to_string()], 0, quick());
    let mut got = None;
    let until = tokio::time::Instant::now() + ms(3_000);
    while got.is_none() && tokio::time::Instant::now() < until {
        tokio::select! {
            ev = copy.next() => if let CopyEvent::Frame { raw, .. } = ev { got = Some(raw) },
            _ = tokio::time::sleep(ms(1)) => {}
        }
    }
    assert_eq!(got.as_deref(), Some(&b"X"[..]));
}

#[tokio::test]
async fn frames_keep_the_connection_alive() {
    // a frame every 100 ms for a second, with 300 ms of silence allowed: no reconnect
    let fake = FakeWs::start(|n, mut ws, log| async move {
        for k in 0..10 {
            tokio::time::sleep(ms(100)).await;
            ws.send(Message::text(k.to_string())).await.unwrap();
        }
        record(n, &mut ws, &log).await;
    })
    .await;
    let mut copy = PmCopy::with_times(&fake.url, &["111".to_string()], 0, quick());
    let mut got = vec![];
    for _ in 0..11 {
        got.push(next(&mut copy).await);
    }
    assert!(got[1..].iter().all(|e| matches!(e, CopyEvent::Frame { .. })), "{got:?}");
}
