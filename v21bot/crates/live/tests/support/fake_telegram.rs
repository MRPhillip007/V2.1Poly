//! A local Telegram Bot API: `/bot{token}/sendMessage` answers with the HTTP statuses the test lines up,
//! then with 200 `{"ok": true}`; every message and the path it came to are recorded.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{json, Value};

#[derive(Clone, Default)]
struct Shared {
    statuses: Arc<Mutex<VecDeque<u16>>>,
    sent: Arc<Mutex<Vec<Value>>>,
    bots: Arc<Mutex<Vec<String>>>,
}

pub struct FakeTelegram {
    pub url: String,
    shared: Shared,
}

async fn send_route(State(s): State<Shared>, Path(bot): Path<String>, axum::Json(body): axum::Json<Value>) -> Response {
    s.bots.lock().unwrap().push(bot);
    s.sent.lock().unwrap().push(body);
    match s.statuses.lock().unwrap().pop_front().unwrap_or(200) {
        200 => axum::Json(json!({"ok": true, "result": {}})).into_response(),
        code => {
            let status = StatusCode::from_u16(code).unwrap();
            (status, axum::Json(json!({"ok": false, "error_code": code, "description": status.canonical_reason()}))).into_response()
        }
    }
}

impl FakeTelegram {
    pub async fn start() -> FakeTelegram {
        let shared = Shared::default();
        let app = axum::Router::new().route("/{bot}/sendMessage", post(send_route)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeTelegram { url, shared }
    }

    /// The statuses of the next requests, in order; 200 after them.
    pub fn answer(&self, statuses: &[u16]) {
        self.shared.statuses.lock().unwrap().extend(statuses);
    }

    /// The bodies of the requests so far.
    pub fn sent(&self) -> Vec<Value> {
        self.shared.sent.lock().unwrap().clone()
    }

    /// The first part of each request's path: `bot{token}`.
    pub fn bots(&self) -> Vec<String> {
        self.shared.bots.lock().unwrap().clone()
    }
}
