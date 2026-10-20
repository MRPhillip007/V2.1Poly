//! A local Binance: REST `/api/v3/klines` (a row per second asked) and `/api/v3/time` (this machine's
//! time plus a shift), and the trade stream `/stream?streams=…`, which sends an aggTrade of each coin
//! every `every_ms`, stamped with the shifted time. The prices rise 0.0006% a second from the
//! coin's base: z is far above 1.25 for Up. `refuse`: every request and every connection gets 451.
//! `cut`: in a span of the exchange's clock the stream's connections are dropped and refused.

#![allow(clippy::result_large_err, reason = "the handshake callback's error type is tungstenite's")]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::SinkExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response as WsResponse};
use tokio_tungstenite::tungstenite::Message;
use v21_core::window::Coin;
use v21_live::shared::clock::local_ms;

const COINS: [Coin; 3] = [Coin::Btc, Coin::Eth, Coin::Sol];

/// The price of `coin` at second `q`.
pub fn price(coin: Coin, q: i64) -> f64 {
    let base = match coin {
        Coin::Btc => 61_000.0,
        Coin::Eth => 2_500.0,
        Coin::Sol => 150.0,
    };
    base * (1.0 + 6e-6 * (q % 1_000_000) as f64)
}

#[derive(Clone)]
struct Shared {
    shift_ms: i64,
    refuse: bool,
    asked: Arc<Mutex<Vec<String>>>,
}

/// The span of the exchange's clock (ms) the stream is down in.
type CutSpan = Arc<Mutex<Option<(i64, i64)>>>;

fn in_cut(cut: &CutSpan, now_ms: i64) -> bool {
    cut.lock().unwrap().is_some_and(|(from, until)| (from..until).contains(&now_ms))
}

pub struct FakeBinance {
    /// `http://…`: the REST base.
    pub rest: String,
    /// `ws://…/stream`: the stream base.
    pub ws: String,
    asked: Arc<Mutex<Vec<String>>>,
    cut: CutSpan,
}

async fn klines_route(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    s.asked.lock().unwrap().push(format!("klines {} {} {}", q["symbol"], q["startTime"], q["endTime"]));
    if s.refuse {
        return StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response();
    }
    let coin = COINS.into_iter().find(|c| c.binance_symbol() == q["symbol"]).unwrap();
    let (start, end): (i64, i64) = (q["startTime"].parse().unwrap(), q["endTime"].parse().unwrap());
    let limit: usize = q["limit"].parse().unwrap();
    let rows: Vec<serde_json::Value> = (start.div_euclid(1000)..=end.div_euclid(1000))
        .take(limit)
        .map(|sec| {
            let p = format!("{:.2}", price(coin, sec));
            serde_json::json!([sec * 1000, p, p, p, p, "1", sec * 1000 + 999, "1", 1, "0", "0", "0"])
        })
        .collect();
    axum::Json(rows).into_response()
}

async fn time_route(State(s): State<Shared>) -> Response {
    s.asked.lock().unwrap().push("time".into());
    if s.refuse {
        return StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS.into_response();
    }
    axum::Json(serde_json::json!({ "serverTime": local_ms() + s.shift_ms })).into_response()
}

fn agg(coin: Coin, t: i64, id: u64) -> String {
    let p = format!("{:.2}", price(coin, t.div_euclid(1000)));
    format!(
        r#"{{"stream":"{}@aggTrade","data":{{"e":"aggTrade","E":{t},"s":"{}","a":{id},"p":"{p}","q":"0.1","f":{id},"l":{id},"T":{t},"m":false,"M":true}}}}"#,
        coin.binance_symbol().to_lowercase(),
        coin.binance_symbol()
    )
}

impl FakeBinance {
    pub async fn start(shift_ms: i64, refuse: bool, every_ms: u64) -> FakeBinance {
        let shared = Shared { shift_ms, refuse, asked: Arc::default() };
        let app = axum::Router::new().route("/api/v3/klines", get(klines_route)).route("/api/v3/time", get(time_route)).with_state(shared.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let rest = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = format!("ws://{}/stream", listener.local_addr().unwrap());
        let cut = CutSpan::default();
        let cut_ws = cut.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if in_cut(&cut_ws, local_ms() + shift_ms) {
                    drop(stream);
                    continue;
                }
                let cut = cut_ws.clone();
                tokio::spawn(async move {
                    let check = |_: &Request, resp: WsResponse| -> Result<WsResponse, ErrorResponse> {
                        if refuse {
                            let mut no = ErrorResponse::new(Some("451".into()));
                            *no.status_mut() = StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS;
                            Err(no)
                        } else {
                            Ok(resp)
                        }
                    };
                    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, check).await else { return };
                    let mut id = 0;
                    loop {
                        if in_cut(&cut, local_ms() + shift_ms) {
                            return; // dropped without a close frame
                        }
                        for coin in COINS {
                            id += 1;
                            if ws.send(Message::text(agg(coin, local_ms() + shift_ms, id))).await.is_err() {
                                return;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(every_ms)).await;
                    }
                });
            }
        });
        FakeBinance { rest, ws, asked: shared.asked, cut }
    }

    /// The stream is down from `from_ms` until `until_ms` (the exchange's clock).
    pub fn cut(&self, from_ms: i64, until_ms: i64) {
        *self.cut.lock().unwrap() = Some((from_ms, until_ms));
    }

    /// The REST requests so far.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}
