//! A local Gamma: `/events` answers with the events whose slugs are asked, from a list the test gives
//! and may change; it can fail the next requests with HTTP 500. Every query is recorded.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

/// The query pairs of each request.
type Queries = Arc<Mutex<Vec<Vec<(String, String)>>>>;

#[derive(Clone, Default)]
struct Shared {
    events: Arc<Mutex<Vec<Value>>>,
    queries: Queries,
    fail: Arc<AtomicUsize>,
}

pub struct FakeGamma {
    pub url: String,
    shared: Shared,
}

async fn events_route(State(s): State<Shared>, Query(q): Query<Vec<(String, String)>>) -> Response {
    s.queries.lock().unwrap().push(q.clone());
    if s.fail.load(Ordering::SeqCst) > 0 {
        s.fail.fetch_sub(1, Ordering::SeqCst);
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let slugs: Vec<&str> = q.iter().filter(|(k, _)| k == "slug").map(|(_, v)| v.as_str()).collect();
    let found: Vec<Value> = s.events.lock().unwrap().iter().filter(|e| slugs.contains(&e["slug"].as_str().unwrap_or_default())).cloned().collect();
    axum::Json(found).into_response()
}

impl FakeGamma {
    pub async fn start(events: Vec<Value>) -> FakeGamma {
        let shared = Shared::default();
        *shared.events.lock().unwrap() = events;
        let app = axum::Router::new().route("/events", get(events_route)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeGamma { url, shared }
    }

    /// The events served from now on.
    pub fn set_events(&self, events: Vec<Value>) {
        *self.shared.events.lock().unwrap() = events;
    }

    /// The next `n` requests get HTTP 500.
    pub fn fail_next(&self, n: usize) {
        self.shared.fail.store(n, Ordering::SeqCst);
    }

    /// The query pairs of each request so far.
    pub fn queries(&self) -> Vec<Vec<(String, String)>> {
        self.shared.queries.lock().unwrap().clone()
    }
}
