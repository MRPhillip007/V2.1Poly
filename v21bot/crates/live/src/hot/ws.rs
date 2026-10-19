//! The websocket connections of the hot thread (spec §4.2): a copy of the Polymarket market channel
//! and the Binance trade stream. Both stay connected by themselves, as `paper/feeds.py` does: after a
//! drop, or a failed attempt on every address, they connect again after a pause that grows from
//! 0.1 s to 5 s; a connection silent for 30 s is dead. A Polymarket copy subscribes to the window's
//! tokens and sends a text PING every 10 s. Reading never waits for anything else: the venue closes a
//! slow reader with code 1013.
//!
//! `next()` is cancel-safe, so that the hot thread can poll it in a `select!` that drops it whenever
//! another branch is ready: every step lives in the connection itself — the attempt to connect, the
//! message being sent, the timers — and polling again goes on where it stopped.

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

/// The times of a connection. The defaults are the bot's; the tests make them short.
#[derive(Clone, Copy, Debug)]
pub struct CopyTimes {
    /// A text PING this often (Polymarket wants one at least every 10 s; Binance needs none).
    pub ping: Duration,
    /// No message for this long: the connection is dead.
    pub silent: Duration,
    /// The first pause before connecting again, doubled after each failed round ...
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

/// What a connection gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketEvent {
    /// A frame as it came: the bytes of a text or binary message.
    Frame(Bytes),
    /// Connected (and subscribed) to the address with this place in the list.
    Up { url: usize },
    /// The connection ended: a close frame with its code, an error, or silence.
    Down { code: Option<u16>, reason: String },
}

enum State {
    /// The pause before the next attempt.
    Waiting(Pin<Box<Sleep>>),
    Connecting(Attempt),
    Open {
        ws: Box<Ws>,
        /// The message to send next: the subscription, a PING.
        outbox: Option<Message>,
        ping: Option<Pin<Box<Sleep>>>,
        silent: Pin<Box<Sleep>>,
    },
}

/// What one round of an open connection came to.
enum Step {
    Event(SocketEvent),
    Down(Option<u16>, String),
    Again,
}

/// A connection that keeps itself up. A failed attempt goes on to the next address of the list at
/// once; when all have failed, or after a drop, it starts again from the first one after a pause.
pub struct Socket {
    urls: Vec<String>,
    url: usize,
    subscribe: Option<String>,
    ping: Option<Duration>,
    times: CopyTimes,
    retry: Duration,
    state: State,
}

fn attempt(url: &str) -> Attempt {
    let url = url.to_string();
    Box::pin(async move { connect_async(url).await.map(|(ws, _)| ws) })
}

impl Socket {
    /// Connects on the first poll. `subscribe` is sent after each connect; with `ping` a text PING
    /// goes out every `times.ping`.
    pub fn new(urls: Vec<String>, subscribe: Option<String>, ping: bool, times: CopyTimes) -> Socket {
        assert!(!urls.is_empty(), "a socket needs an address");
        let state = State::Connecting(attempt(&urls[0]));
        Socket { urls, url: 0, subscribe, ping: ping.then_some(times.ping), times, retry: times.retry_first, state }
    }

    /// A pause before the next round from the first address; the pause after it is twice as long.
    fn wait(&mut self) {
        self.url = 0;
        self.state = State::Waiting(Box::pin(sleep(self.retry)));
        self.retry = (self.retry * 2).min(self.times.retry_max);
    }

    pub fn poll_event(&mut self, cx: &mut Context<'_>) -> Poll<SocketEvent> {
        loop {
            match &mut self.state {
                State::Waiting(pause) => {
                    ready!(pause.as_mut().poll(cx));
                    self.state = State::Connecting(attempt(&self.urls[self.url]));
                }
                State::Connecting(connecting) => match ready!(connecting.as_mut().poll(cx)) {
                    Ok(ws) => {
                        self.retry = self.times.retry_first;
                        self.state = State::Open {
                            ws: Box::new(ws),
                            outbox: self.subscribe.clone().map(Message::text),
                            ping: self.ping.map(|p| Box::pin(sleep(p))),
                            silent: Box::pin(sleep(self.times.silent)),
                        };
                        return Poll::Ready(SocketEvent::Up { url: self.url });
                    }
                    // still down, so no event: the next address at once, else a pause
                    Err(_) if self.url + 1 < self.urls.len() => {
                        self.url += 1;
                        self.state = State::Connecting(attempt(&self.urls[self.url]));
                    }
                    Err(_) => self.wait(),
                },
                State::Open { ws, outbox, ping, silent } => match open_step(cx, ws, outbox, ping, silent, self.ping, self.times.silent) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Step::Again) => {}
                    Poll::Ready(Step::Event(event)) => return Poll::Ready(event),
                    Poll::Ready(Step::Down(code, reason)) => {
                        self.wait();
                        return Poll::Ready(SocketEvent::Down { code, reason });
                    }
                },
            }
        }
    }
}

/// One round of an open connection: send what waits to be sent, mind the timers, read a message.
fn open_step(
    cx: &mut Context<'_>,
    ws: &mut Ws,
    outbox: &mut Option<Message>,
    ping: &mut Option<Pin<Box<Sleep>>>,
    silent: &mut Pin<Box<Sleep>>,
    ping_every: Option<Duration>,
    silent_after: Duration,
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
    if let (Some(timer), Some(every)) = (ping.as_mut(), ping_every)
        && timer.as_mut().poll(cx).is_ready()
    {
        *outbox = Some(Message::text("PING"));
        timer.as_mut().reset(Instant::now() + every);
        return Poll::Ready(Step::Again);
    }
    if silent.as_mut().poll(cx).is_ready() {
        return Poll::Ready(Step::Down(None, format!("silent for {:.1} s", silent_after.as_secs_f64())));
    }
    let step = match ready!(ws.as_mut().poll_next(cx)) {
        Some(Ok(Message::Text(text))) => Step::Event(SocketEvent::Frame(Bytes::from(text))),
        Some(Ok(Message::Binary(raw))) => Step::Event(SocketEvent::Frame(raw)),
        Some(Ok(Message::Close(Some(f)))) => Step::Down(Some(u16::from(f.code)), f.reason.to_string()),
        Some(Ok(Message::Close(None))) => Step::Down(None, "closed".to_string()),
        Some(Ok(_)) => Step::Again, // a ping, a pong, a raw frame: the connection is alive
        Some(Err(e)) => Step::Down(None, e.to_string()),
        None => Step::Down(None, "closed".to_string()),
    };
    if !matches!(step, Step::Down(..)) {
        silent.as_mut().reset(Instant::now() + silent_after);
    }
    Poll::Ready(step)
}

// ---------------------------------------------------------------- a copy of the market channel

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyEvent {
    /// A frame as it came.
    Frame { copy: u8, raw: Bytes },
    /// Connected and subscribed.
    Up { copy: u8 },
    /// The connection ended (1013: a slow reader); the copy connects again by itself.
    Down { copy: u8, code: Option<u16>, reason: String },
}

/// One copy of the Polymarket market channel for the tokens `assets`.
pub struct PmCopy {
    socket: Socket,
    copy: u8,
}

impl PmCopy {
    pub fn new(url: &str, assets: &[String], copy: u8) -> PmCopy {
        PmCopy::with_times(url, assets, copy, CopyTimes::default())
    }

    pub fn with_times(url: &str, assets: &[String], copy: u8, times: CopyTimes) -> PmCopy {
        let subscribe = serde_json::json!({ "assets_ids": assets, "type": "market" }).to_string();
        PmCopy { socket: Socket::new(vec![url.to_string()], Some(subscribe), true, times), copy }
    }

    /// The next event of the copy. Cancel-safe.
    pub async fn next(&mut self) -> CopyEvent {
        let copy = self.copy;
        poll_fn(|cx| {
            self.socket.poll_event(cx).map(|event| match event {
                SocketEvent::Frame(raw) => CopyEvent::Frame { copy, raw },
                SocketEvent::Up { .. } => CopyEvent::Up { copy },
                SocketEvent::Down { code, reason } => CopyEvent::Down { copy, code, reason },
            })
        })
        .await
    }
}
