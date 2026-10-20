//! A scripted market channel of Polymarket, run by the exchange's clock (this machine's clock plus a
//! shift). Each token has a book; at the moments of the script the asks of a token are set anew, and
//! every subscribed connection gets the change — the same bytes on each, as the copies of the real
//! channel get them. Every 500 ms a far bid level of every token changes, so that no coin's feed falls
//! silent. A subscription first gets a snapshot of each of its tokens. Connections can be dropped with
//! a close code, and new ones refused for a while.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use v21_live::shared::clock::local_ms;

/// (price, size) of the levels of one side of a book.
pub type Levels = Vec<(f64, f64)>;

/// The asks of a token from a moment of the window on.
#[derive(Clone, Debug)]
pub struct Asks {
    /// Ms into the window.
    pub at_ms: i64,
    pub token: usize,
    /// (price, size), all the asks the token has from then on.
    pub levels: Vec<(f64, f64)>,
}

/// Connections dropped at `at_ms` (ms into the window) with `code`: the oldest one, or all; new ones are
/// refused until `until_ms`.
#[derive(Clone, Debug)]
pub struct Cut {
    pub at_ms: i64,
    pub until_ms: i64,
    pub all: bool,
    pub code: u16,
}

#[derive(Clone, Debug, Default)]
struct Book {
    bids: BTreeMap<u32, f64>,
    asks: BTreeMap<u32, f64>,
}

fn tick(p: f64) -> u32 {
    (p * 1000.0).round() as u32
}

fn price(t: u32) -> String {
    let p = f64::from(t) / 1000.0;
    format!("{p}")
}

impl Book {
    fn best(&self) -> (String, String) {
        let bid = self.bids.iter().rev().find(|(_, s)| **s > 0.0).map_or("0".to_string(), |(t, _)| price(*t));
        let ask = self.asks.iter().find(|(_, s)| **s > 0.0).map_or("1".to_string(), |(t, _)| price(*t));
        (bid, ask)
    }

    fn levels(side: &BTreeMap<u32, f64>) -> String {
        side.iter().filter(|(_, s)| **s > 0.0).map(|(t, s)| format!(r#"{{"price":"{}", "size":"{s}"}}"#, price(*t))).collect::<Vec<_>>().join(", ")
    }
}

struct Shared {
    tokens: Vec<String>,
    books: Mutex<Vec<Book>>,
    frames: broadcast::Sender<Arc<String>>,
    /// Connection id → the close code it is to be closed with.
    closing: Mutex<HashMap<u64, u16>>,
    live: Mutex<Vec<u64>>,
    refuse_until: Mutex<i64>,
    shift_ms: i64,
    hash: AtomicU64,
}

impl Shared {
    fn now(&self) -> i64 {
        local_ms() + self.shift_ms
    }

    fn snapshot(&self, token: usize) -> String {
        let books = self.books.lock().unwrap();
        let b = &books[token];
        format!(
            r#"{{"market":"0x{token:02}", "asset_id":"{}", "bids":[{}], "asks":[{}], "hash":"s{}", "timestamp":"{}", "event_type":"book"}}"#,
            self.tokens[token],
            Book::levels(&b.bids),
            Book::levels(&b.asks),
            self.hash.fetch_add(1, Ordering::Relaxed),
            self.now()
        )
    }

    /// Applies changes `(side, tick, size)` to a token's book and sends them as one event stamped `ts`
    /// (the moment of the script, not when the timer woke).
    fn change(&self, token: usize, ts: i64, changes: &[(&str, u32, f64)]) {
        let mut books = self.books.lock().unwrap();
        let b = &mut books[token];
        for &(side, t, size) in changes {
            let levels = if side == "BUY" { &mut b.bids } else { &mut b.asks };
            levels.insert(t, size);
        }
        let (bid, ask) = b.best();
        let id = &self.tokens[token];
        let parts: Vec<String> = changes
            .iter()
            .map(|&(side, t, size)| {
                format!(
                    r#"{{"asset_id":"{id}", "price":"{}", "size":"{size}", "side":"{side}", "hash":"h{}", "best_bid":"{bid}", "best_ask":"{ask}"}}"#,
                    price(t),
                    self.hash.fetch_add(1, Ordering::Relaxed)
                )
            })
            .collect();
        let frame = format!(r#"{{"market":"0x{token:02}", "price_changes":[{}], "timestamp":"{ts}", "event_type":"price_change"}}"#, parts.join(", "));
        let _ = self.frames.send(Arc::new(frame));
    }

    /// The asks of a token become `levels`: the old ones go, the new ones come, in one event.
    fn set_asks(&self, token: usize, ts: i64, levels: &[(f64, f64)]) {
        let old: Vec<u32> = self.books.lock().unwrap()[token].asks.iter().filter(|(_, s)| **s > 0.0).map(|(t, _)| *t).collect();
        let mut changes: Vec<(&str, u32, f64)> = old.into_iter().filter(|t| !levels.iter().any(|(p, _)| tick(*p) == *t)).map(|t| ("SELL", t, 0.0)).collect();
        changes.extend(levels.iter().map(|&(p, s)| ("SELL", tick(p), s)));
        self.change(token, ts, &changes);
    }
}

pub struct FakeMarket {
    /// `ws://…/ws/market`.
    pub ws: String,
    shared: Arc<Shared>,
}

impl FakeMarket {
    /// The channel of the window `s` for `tokens`, with each token's first book `(bids, asks)`.
    pub async fn start(s: i64, shift_ms: i64, tokens: Vec<String>, books: Vec<(Levels, Levels)>, script: Vec<Asks>, cuts: Vec<Cut>) -> FakeMarket {
        let books = books
            .into_iter()
            .map(|(bids, asks)| Book { bids: bids.iter().map(|&(p, s)| (tick(p), s)).collect(), asks: asks.iter().map(|&(p, s)| (tick(p), s)).collect() })
            .collect();
        let (frames, _) = broadcast::channel(16_384);
        let shared = Arc::new(Shared {
            tokens,
            books: Mutex::new(books),
            frames,
            closing: Mutex::default(),
            live: Mutex::default(),
            refuse_until: Mutex::new(i64::MIN),
            shift_ms,
            hash: AtomicU64::new(0),
        });

        // the script, the heartbeats and the cuts, in time order
        enum Step {
            Asks(Asks),
            Beat(u64),
            Cut(Cut),
        }
        let s0 = s * 1000;
        let mut steps: Vec<(i64, Step)> = script.into_iter().map(|a| (s0 + a.at_ms, Step::Asks(a))).collect();
        steps.extend(cuts.into_iter().map(|c| (s0 + c.at_ms, Step::Cut(c))));
        let from = shared.now().div_euclid(500) * 500 + 500;
        steps.extend((0..).map(|k| from + k * 500).take_while(|t| *t <= s0 + 900_000).enumerate().map(|(k, t)| (t, Step::Beat(k as u64))));
        steps.sort_by_key(|(t, _)| *t);
        let run = shared.clone();
        tokio::spawn(async move {
            for (at, step) in steps {
                let wait = at - run.now();
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait as u64)).await;
                }
                match step {
                    Step::Asks(a) => run.set_asks(a.token, at, &a.levels),
                    Step::Beat(k) => {
                        for token in 0..run.tokens.len() {
                            run.change(token, at, &[("BUY", 10, 10.0 + (k % 2) as f64)]);
                        }
                    }
                    Step::Cut(c) => {
                        let live = run.live.lock().unwrap().clone();
                        let mut closing = run.closing.lock().unwrap();
                        for id in if c.all { live } else { live.into_iter().take(1).collect() } {
                            closing.insert(id, c.code);
                        }
                        *run.refuse_until.lock().unwrap() = s0 + c.until_ms;
                    }
                }
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws = format!("ws://{}/ws/market", listener.local_addr().unwrap());
        let serve = shared.clone();
        tokio::spawn(async move {
            let mut next_id = 0u64;
            while let Ok((stream, _)) = listener.accept().await {
                if serve.now() < *serve.refuse_until.lock().unwrap() {
                    drop(stream);
                    continue;
                }
                let (shared, id) = (serve.clone(), next_id);
                next_id += 1;
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else { return };
                    let mut frames = shared.frames.subscribe();
                    // the subscription
                    let tokens: Vec<usize> = loop {
                        match ws.next().await {
                            Some(Ok(Message::Text(t))) => {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                                    let ids: Vec<&str> = v["assets_ids"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
                                    break (0..shared.tokens.len()).filter(|k| ids.contains(&shared.tokens[*k].as_str())).collect();
                                }
                            }
                            Some(Ok(_)) => {}
                            _ => return,
                        }
                    };
                    shared.live.lock().unwrap().push(id);
                    for &k in &tokens {
                        if ws.send(Message::text(shared.snapshot(k))).await.is_err() {
                            return;
                        }
                    }
                    let mut check = tokio::time::interval(Duration::from_millis(20));
                    loop {
                        tokio::select! {
                            got = frames.recv() => match got {
                                Ok(frame) => {
                                    if ws.send(Message::text(frame.as_str())).await.is_err() {
                                        break;
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => {}
                                Err(_) => break,
                            },
                            msg = ws.next() => match msg {
                                Some(Ok(Message::Text(t))) if t.as_str() == "PING" => {
                                    let _ = ws.send(Message::text("PONG")).await;
                                }
                                Some(Ok(_)) => {}
                                _ => break,
                            },
                            _ = check.tick() => {
                                let code = shared.closing.lock().unwrap().remove(&id);
                                if let Some(code) = code {
                                    let _ = ws.close(Some(CloseFrame { code: CloseCode::from(code), reason: "".into() })).await;
                                    break;
                                }
                            }
                        }
                    }
                    shared.live.lock().unwrap().retain(|x| *x != id);
                });
            }
        });
        FakeMarket { ws, shared }
    }

    /// Connections alive now.
    pub fn live(&self) -> usize {
        self.shared.live.lock().unwrap().len()
    }
}
