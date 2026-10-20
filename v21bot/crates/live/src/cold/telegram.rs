//! Telegram (spec §8): the texts of the demo mode and their delivery to the user's one chat, as
//! `paper/notify.py` — a minus is the sign U+2212, money is grouped by thousands. Delivery never stops
//! the bot: three attempts, then a line in the log. Without a token or a chat the messages only go to
//! the log. The token is in no log line: every line is masked before it is written.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use v21_core::fill::Fill;
use v21_core::window::{Coin, Side};
use zeroize::Zeroizing;

use crate::shared::secrets::Secrets;
use crate::shared::utc::fmt_utc;

const MINUS: char = '\u{2212}';
const WARN: &str = "\u{26a0}\u{fe0f}";
/// The pauses before the second and the third attempt.
const PAUSES: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(4)];
/// Log lines kept for `log_lines`.
const LOG_KEEP: usize = 100;

/// `$1,234.50`; with `sign`, `+$1.20` or `−$1.20`.
pub fn money(x: f64, sign: bool) -> String {
    let text = format!("{:.2}", x.abs());
    let (whole, cents) = text.split_once('.').unwrap_or((&text, "00"));
    let mut grouped = String::new();
    for (i, d) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(d);
    }
    let head = match (sign, x < 0.0) {
        (false, _) => String::new(),
        (true, true) => MINUS.to_string(),
        (true, false) => "+".to_string(),
    };
    format!("{head}${grouped}.{cents}")
}

/// `$20`, or `$20.50` when there are cents.
pub fn dollars(x: f64) -> String {
    if x.fract() == 0.0 { format!("${x:.0}") } else { money(x, false) }
}

/// `BTC Down`.
pub fn name(coin: Coin, side: Side) -> String {
    let side = match side {
        Side::Up => "Up",
        Side::Down => "Down",
    };
    format!("{} {side}", coin.as_str().to_uppercase())
}

/// `03:15–03:30`.
fn window(s: i64) -> String {
    format!("{}–{}", &fmt_utc(s)[11..16], &fmt_utc(s + 900)[11..16])
}

pub fn fmt_entry(coin: Coin, side: Side, s: i64, fill: &Fill, z: Option<f64>) -> String {
    let z = z.map_or("—".to_string(), |z| format!("{z:.2}"));
    format!(
        "🟢 {} · окно {} · демо: {} по {:.3} → {:.2} шт. · z {z}",
        name(coin, side),
        window(s),
        money(fill.notional_usd, false),
        fill.avg_price,
        fill.shares
    )
}

pub fn fmt_result(coin: Coin, side: Side, payout_per_share: f64, pnl: f64, balance: f64) -> String {
    let head = if payout_per_share >= 1.0 {
        format!("✅ {}", name(coin, side))
    } else if payout_per_share <= 0.0 {
        format!("❌ {}", name(coin, side))
    } else {
        format!("➖ {} 50/50", name(coin, side))
    };
    format!("{head} {} · баланс {}", money(pnl, true), money(balance, false))
}

pub fn fmt_missed(coin: Coin, side: Side, s: i64) -> String {
    format!("⚪ {} · окно {} · сигнал был, демо-исполнения нет", name(coin, side), window(s))
}

pub fn fmt_alarm(text: &str) -> String {
    format!("{WARN} {text}")
}

/// What the day's summary says.
#[derive(Clone, Debug, PartialEq)]
pub struct DaySummary {
    /// The UTC date, `YYYY-MM-DD`.
    pub date: String,
    /// Trades settled, and those won.
    pub n: u32,
    pub wins: u32,
    pub pnl: f64,
    pub balance: f64,
    /// Counted signals of the base rule, each with its demo order.
    pub signals: u32,
    /// The demo order's dollars before the fee.
    pub stake_usd: f64,
    /// The demo orders that filled the whole stake, %.
    pub full_fill_pct: f64,
    pub mean_filled_usd: f64,
    /// The demo price minus the backtest's, cents a contract.
    pub price_vs_backtest_c: Option<f64>,
    pub missed: u32,
    pub fast_ne_exact: u32,
    pub alarms: Vec<String>,
}

pub fn fmt_summary(d: &DaySummary) -> String {
    let date = match (d.date.get(8..10), d.date.get(5..7)) {
        (Some(day), Some(month)) => format!("{day}.{month}"),
        _ => d.date.clone(),
    };
    let signals = if d.signals == 0 {
        "Сигналов: 0".to_string()
    } else {
        format!(
            "Сигналов: {} · ордер {} исполнился полностью в {:.0}% · в среднем {} из {}",
            d.signals,
            dollars(d.stake_usd),
            d.full_fill_pct,
            money(d.mean_filled_usd, false),
            dollars(d.stake_usd)
        )
    };
    let price = d.price_vs_backtest_c.map_or("—".to_string(), |c| format!("{}{:.1}¢", if c < 0.0 { MINUS } else { '+' }, c.abs()));
    let mut lines = vec![
        format!("📊 Сводка за {date} (UTC) · демо"),
        format!(
            "Сделок: {} · выигрыши {} / проигрыши {} · P&L {} · баланс {}",
            d.n,
            d.wins,
            d.n.saturating_sub(d.wins),
            money(d.pnl, true),
            money(d.balance, false)
        ),
        signals,
        format!("Цена демо − цена бэктеста: {price}"),
        format!("Упущенные: {} · быстрое ≠ точное: {}", d.missed, d.fast_ne_exact),
    ];
    lines.extend(d.alarms.iter().map(|a| fmt_alarm(a)));
    lines.join("\n")
}

/// The error and its causes, one after another.
fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        text.push_str(": ");
        text.push_str(&c.to_string());
        cause = c.source();
    }
    text
}

/// The user's chat. Messages go one at a time, in the order they were given.
pub struct Telegram {
    http: reqwest::Client,
    /// `…/bot{token}/sendMessage`; none without a token or a chat.
    url: Option<Zeroizing<String>>,
    token: Option<Zeroizing<String>>,
    chat_id: Option<String>,
    pauses: [Duration; 2],
    turn: tokio::sync::Mutex<()>,
    log: Mutex<VecDeque<String>>,
}

impl Telegram {
    /// The chat of `secrets` through the Bot API at `api` (`https://api.telegram.org`).
    pub fn new(secrets: &Secrets, api: &str) -> Telegram {
        let token = secrets.telegram_token.clone().map(Zeroizing::new);
        let chat_id = secrets.telegram_chat_id.clone();
        let url = token.as_ref().filter(|_| chat_id.is_some()).map(|t| Zeroizing::new(format!("{api}/bot{}/sendMessage", t.as_str())));
        Telegram { http: crate::cold::http_client(), url, token, chat_id, pauses: PAUSES, turn: tokio::sync::Mutex::new(()), log: Mutex::new(VecDeque::new()) }
    }

    /// Other pauses between the attempts (the tests).
    pub fn with_pauses(mut self, pauses: [Duration; 2]) -> Telegram {
        self.pauses = pauses;
        self
    }

    /// A token and a chat are known.
    pub fn enabled(&self) -> bool {
        self.url.is_some()
    }

    /// The last lines this client wrote to the log.
    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().map(|l| l.iter().cloned().collect()).unwrap_or_default()
    }

    fn note(&self, line: String) {
        let line = match &self.token {
            Some(t) if !t.is_empty() => line.replace(t.as_str(), "***"),
            _ => line,
        };
        eprintln!("telegram: {line}");
        if let Ok(mut log) = self.log.lock() {
            if log.len() == LOG_KEEP {
                log.pop_front();
            }
            log.push_back(line);
        }
    }

    /// Sends `text`; false if it was not delivered after three attempts, or Telegram is off.
    pub async fn send(&self, text: &str) -> bool {
        let Some(url) = &self.url else {
            self.note(format!("[telegram off] {text}"));
            return false;
        };
        let _turn = self.turn.lock().await;
        let body = json!({"chat_id": self.chat_id, "text": text});
        for attempt in 1..=3 {
            if attempt > 1 {
                tokio::time::sleep(self.pauses[attempt - 2]).await;
            }
            match self.http.post(url.as_str()).json(&body).send().await {
                Ok(r) => {
                    let status = r.status();
                    match r.json::<Value>().await {
                        Ok(v) if status.is_success() && v["ok"] == true => return true,
                        Ok(v) => self.note(format!("attempt {attempt} refused: HTTP {}: {}", status.as_u16(), v["description"])),
                        Err(e) => self.note(format!("attempt {attempt} refused: HTTP {}: {}", status.as_u16(), chain(&e))),
                    }
                }
                Err(e) => self.note(format!("attempt {attempt} failed: {}", chain(&e))),
            }
        }
        self.note(format!("not delivered after 3 attempts: {text}"));
        false
    }
}
