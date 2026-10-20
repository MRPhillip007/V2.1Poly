//! A local Polymarket: the geoblock answer, and the market channel. A connection that subscribes gets a
//! book of each of its tokens every `every_ms` — the same bytes on every connection in the same slot
//! of time, as the copies of the real channel get them.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use v21_live::shared::clock::local_ms;

pub struct FakePolymarket {
    /// `ws://…/ws/market`.
    pub ws: String,
    /// `http://…/api/geoblock`.
    pub geoblock: String,
    connections: Arc<AtomicUsize>,
}

/// A book of `token` stamped `ts`.
fn book(token: &str, ts: i64) -> String {
    format!(
        r#"{{"market":"0x00", "asset_id":"{token}", "bids":[{{"price":"0.48", "size":"100"}}], "asks":[{{"price":"0.52", "size":"100"}}], "hash":"x", "timestamp":"{ts}", "event_type":"book"}}"#
    )
}

impl FakePolymarket {
    /// `geoblock`: what the geoblock answers, (blocked, country).
    pub async fn start(geoblock: (bool, &'static str), every_ms: i64) -> FakePolymarket {
        let (blocked, country) = geoblock;
        let app = axum::Router::new().route(
            "/api/geoblock",
            get(move || async move { axum::Json(serde_json::json!({"blocked": blocked, "ip": "127.0.0.1", "country": country, "region": "L"})) }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let geoblock = format!("http://{}/api/geoblock", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = format!("ws://{}/ws/market", listener.local_addr().unwrap());
        let connections = Arc::new(AtomicUsize::new(0));
        let counted = connections.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                    // the subscription first
                    let tokens: Vec<String> = loop {
                        match ws.next().await {
                            Some(Ok(Message::Text(t))) => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                                    break v["assets_ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
                                }
                            }
                            Some(Ok(_)) => {}
                            _ => return,
                        }
                    };
                    let mut slot = local_ms().div_euclid(every_ms);
                    loop {
                        let wait = (slot + 1) * every_ms - local_ms();
                        tokio::time::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(0))).await;
                        slot += 1;
                        for t in &tokens {
                            if ws.send(Message::text(book(t, slot * every_ms))).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        FakePolymarket { ws, geoblock, connections }
    }

    /// Connections to the market channel so far.
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}
