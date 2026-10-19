//! A local CLOB: `/clob-markets/{condition_id}` and `/book?token_id=` answer from what the test gives,
//! `/time` with the time in seconds, as the venue does. Every request is recorded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

#[derive(Clone, Default)]
struct Shared {
    markets: Arc<Mutex<HashMap<String, Value>>>,
    books: Arc<Mutex<HashMap<String, Value>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

pub struct FakeClob {
    pub url: String,
    shared: Shared,
}

async fn market_route(State(s): State<Shared>, Path(cid): Path<String>) -> Response {
    s.hits.lock().unwrap().push(format!("market {cid}"));
    match s.markets.lock().unwrap().get(&cid) {
        Some(v) => axum::Json(v.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn book_route(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let token = q.get("token_id").cloned().unwrap_or_default();
    s.hits.lock().unwrap().push(format!("book {token}"));
    match s.books.lock().unwrap().get(&token) {
        Some(v) => axum::Json(v.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn time_route(State(s): State<Shared>) -> Response {
    s.hits.lock().unwrap().push("time".into());
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    secs.to_string().into_response()
}

impl FakeClob {
    pub async fn start() -> FakeClob {
        let shared = Shared::default();
        let app = axum::Router::new()
            .route("/clob-markets/{cid}", get(market_route))
            .route("/book", get(book_route))
            .route("/time", get(time_route))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeClob { url, shared }
    }

    pub fn set_market(&self, condition_id: &str, info: Value) {
        self.shared.markets.lock().unwrap().insert(condition_id.to_string(), info);
    }

    pub fn set_book(&self, token: &str, book: Value) {
        self.shared.books.lock().unwrap().insert(token.to_string(), book);
    }

    pub fn hits(&self) -> Vec<String> {
        self.shared.hits.lock().unwrap().clone()
    }
}
