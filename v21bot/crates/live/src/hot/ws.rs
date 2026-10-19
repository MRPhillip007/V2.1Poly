//! One copy of the Polymarket market channel (spec §4.2, §5.2), as `paper/feeds.MarketStream`:
//! connect, subscribe to the window's tokens, send a text PING every 10 s, take 30 s of silence for a
//! dead connection, connect again after a pause that grows from 0.1 s to 5 s. Reading never waits for
//! anything else: the venue closes a slow reader with code 1013.
//!
//! `next()` is cancel-safe, so that the hot thread can poll it in a `select!` that drops it whenever
//! another branch is ready: every step lives in `PmCopy` itself — the attempt to connect, the message
//! being sent, the timers — and polling again goes on where it stopped.

use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Sink, Stream};
use tokio::net::TcpStream;
use tokio::time::{sleep, Instant, Sleep};
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Attempt = Pin<Box<dyn Future<Output = Result<Ws, WsError>> + Send>>;

/// The times of a copy. The defaults are the bot's; the tests make them short.
#[derive(Clone, Copy, Debug)]
pub struct CopyTimes {
    /// A text PING this often (Polymarket wants one at least every 10 s).
    pub ping: Duration,
    /// No message for this long: the connection is dead.
    pub silent: Duration,
    /// The first pause before connecting again, doubled after each failed attempt ...
    pub retry_first: Duration,
    /// ... up to this.
    pub retry_max: Duration,
}

impl Default for CopyTimes {
    fn default() -> Self {
        CopyTimes {
            ping: Duration::from_secs(10),
            silent: Duration::from_secs(30),
            retry_first: Duration::from_millis(100),
            retry_max: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyEvent {
    /// A frame as it came: the bytes of a text or binary message.
    Frame { copy: u8, raw: Bytes },
    /// Connected and subscribed.
    Up { copy: u8 },
    /// The connection ended: a close frame with its code (1013: a slow reader), an error, or silence.
    /// The copy connects again by itself.
    Down { copy: u8, code: Option<u16>, reason: String },
}

enum State {
    /// The pause before the next attempt.
    Waiting(Pin<Box<Sleep>>),
    Connecting(Attempt),
    Open {
        ws: Box<Ws>,
        /// The message to send next: the subscription, a PING.
        outbox: Option<Message>,
        ping: Pin<Box<Sleep>>,
        silent: Pin<Box<Sleep>>,
    },
}

/// What one round of an open connection came to.
enum Step {
    Event(CopyEvent),
    Down(Option<u16>, String),
    Again,
}

/// One copy of the market channel for the tokens `assets`.
pub struct PmCopy {
    url: String,
    subscribe: String,
    copy: u8,
    times: CopyTimes,
    /// The pause before the next attempt after a failed one.
    retry: Duration,
    state: State,
}

fn attempt(url: &str) -> Attempt {
    let url = url.to_string();
    Box::pin(async move { connect_async(url).await.map(|(ws, _)| ws) })
}

impl PmCopy {
    pub fn new(url: &str, assets: &[String], copy: u8) -> PmCopy {
        PmCopy::with_times(url, assets, copy, CopyTimes::default())
    }

    /// Connects at once (on the first poll) and keeps connecting until dropped.
    pub fn with_times(url: &str, assets: &[String], copy: u8, times: CopyTimes) -> PmCopy {
        PmCopy {
            url: url.to_string(),
            subscribe: serde_json::json!({ "assets_ids": assets, "type": "market" }).to_string(),
            copy,
            times,
            retry: times.retry_first,
            state: State::Connecting(attempt(url)),
        }
    }

    /// The next event of the copy. Cancel-safe.
    pub async fn next(&mut self) -> CopyEvent {
        poll_fn(|cx| self.poll_event(cx)).await
    }

    /// A pause before the next attempt; the pause after that is twice as long, up to `retry_max`.
    fn wait(&mut self) {
        self.state = State::Waiting(Box::pin(sleep(self.retry)));
        self.retry = (self.retry * 2).min(self.times.retry_max);
    }

    fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<CopyEvent> {
        loop {
            match &mut self.state {
                State::Waiting(pause) => {
                    ready!(pause.as_mut().poll(cx));
                    self.state = State::Connecting(attempt(&self.url));
                }
                State::Connecting(connecting) => match ready!(connecting.as_mut().poll(cx)) {
                    Ok(ws) => {
                        self.retry = self.times.retry_first;
                        self.state = State::Open {
                            ws: Box::new(ws),
                            outbox: Some(Message::text(self.subscribe.clone())),
                            ping: Box::pin(sleep(self.times.ping)),
                            silent: Box::pin(sleep(self.times.silent)),
                        };
                        return Poll::Ready(CopyEvent::Up { copy: self.copy });
                    }
                    // still down: no event, another attempt after a pause
                    Err(_) => self.wait(),
                },
                State::Open { ws, outbox, ping, silent } => {
                    match open_step(cx, ws, outbox, ping, silent, self.copy, &self.times) {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Step::Again) => {}
                        Poll::Ready(Step::Event(event)) => return Poll::Ready(event),
                        Poll::Ready(Step::Down(code, reason)) => {
                            self.wait();
                            return Poll::Ready(CopyEvent::Down { copy: self.copy, code, reason });
                        }
                    }
                }
            }
        }
    }
}

/// One round of an open connection: send what waits to be sent, mind the timers, read a message.
fn open_step(
    cx: &mut Context<'_>,
    ws: &mut Ws,
    outbox: &mut Option<Message>,
    ping: &mut Pin<Box<Sleep>>,
    silent: &mut Pin<Box<Sleep>>,
    copy: u8,
    times: &CopyTimes,
) -> Poll<Step> {
    let mut ws = Pin::new(ws);
    if let Some(msg) = outbox.take() {
        match ws.as_mut().poll_ready(cx) {
            Poll::Ready(Ok(())) => {
                if let Err(e) = ws.as_mut().start_send(msg) {
                    return Poll::Ready(Step::Down(None, e.to_string()));
                }
            }
            Poll::Ready(Err(e)) => return Poll::Ready(Step::Down(None, e.to_string())),
            Poll::Pending => *outbox = Some(msg),
        }
    }
    // whatever is buffered goes out: our messages, and the pongs the library answers pings with
    if let Poll::Ready(Err(e)) = ws.as_mut().poll_flush(cx) {
        return Poll::Ready(Step::Down(None, e.to_string()));
    }
    if ping.as_mut().poll(cx).is_ready() {
        *outbox = Some(Message::text("PING"));
        ping.as_mut().reset(Instant::now() + times.ping);
        return Poll::Ready(Step::Again);
    }
    if silent.as_mut().poll(cx).is_ready() {
        return Poll::Ready(Step::Down(None, format!("silent for {:.1} s", times.silent.as_secs_f64())));
    }
    let step = match ready!(ws.as_mut().poll_next(cx)) {
        Some(Ok(Message::Text(text))) => Step::Event(CopyEvent::Frame { copy, raw: Bytes::from(text) }),
        Some(Ok(Message::Binary(raw))) => Step::Event(CopyEvent::Frame { copy, raw }),
        Some(Ok(Message::Close(Some(f)))) => Step::Down(Some(u16::from(f.code)), f.reason.to_string()),
        Some(Ok(Message::Close(None))) => Step::Down(None, "closed".to_string()),
        Some(Ok(_)) => Step::Again, // a ping, a pong, a raw frame: the connection is alive
        Some(Err(e)) => Step::Down(None, e.to_string()),
        None => Step::Down(None, "closed".to_string()),
    };
    if !matches!(step, Step::Down(..)) {
        silent.as_mut().reset(Instant::now() + times.silent);
    }
    Poll::Ready(step)
}
