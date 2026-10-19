//! A local websocket server: each connection is handed, with its number, to a script of the test.
//! What the clients send is recorded.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

pub type ServerWs = WebSocketStream<TcpStream>;

/// What the clients sent: (connection number, text).
pub type Log = Arc<Mutex<Vec<(usize, String)>>>;

pub struct FakeWs {
    pub url: String,
    pub log: Log,
}

impl FakeWs {
    /// Serves on a free local port; `script(n, ws, log)` drives the n-th connection (from 0).
    pub async fn start<F, Fut>(script: F) -> FakeWs
    where
        F: Fn(usize, ServerWs, Log) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let log: Log = Arc::default();
        let (script, served) = (Arc::new(script), log.clone());
        tokio::spawn(async move {
            let mut n = 0;
            while let Ok((stream, _)) = listener.accept().await {
                let (script, log) = (script.clone(), served.clone());
                tokio::spawn(async move {
                    if let Ok(ws) = accept_async(stream).await {
                        script(n, ws, log).await;
                    }
                });
                n += 1;
            }
        });
        FakeWs { url, log }
    }

    /// The texts received so far.
    pub fn received(&self) -> Vec<(usize, String)> {
        self.log.lock().unwrap().clone()
    }

    /// Waits up to `within` until `ok` holds for the texts received.
    pub async fn wait_for(&self, within: Duration, ok: impl Fn(&[(usize, String)]) -> bool) -> Vec<(usize, String)> {
        let until = tokio::time::Instant::now() + within;
        loop {
            let got = self.received();
            if ok(&got) || tokio::time::Instant::now() >= until {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Records what connection `n` sends until it closes.
pub async fn record(n: usize, ws: &mut ServerWs, log: &Log) {
    while let Some(Ok(msg)) = ws.next().await {
        if let Message::Text(text) = msg {
            log.lock().unwrap().push((n, text.to_string()));
        }
    }
}
