//! Gamma: the markets of a window, their rules, and their outcomes — the port of `paper/feeds.py`
//! (`parse_market`, `parse_outcome`, `gamma_events`, `fetch_markets`, `fetch_outcomes`), with the fee's
//! exponent checked too (spec §7.4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use v21_core::fill::FeeSchedule;
use v21_core::window::{Coin, TWAP_S};

use crate::shared::clock::local_ms;

/// The fee of the markets the strategy was tested on: 0.07 · p(1 − p), taker only.
pub const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };

/// A market of a window, as Gamma gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Market {
    pub coin: Coin,
    /// The window start, unix seconds.
    pub s: i64,
    pub slug: String,
    pub condition_id: String,
    /// The token ids of the outcomes.
    pub up: String,
    pub down: String,
    pub fee: FeeSchedule,
    /// The rules are the ones the strategy was tested on (TWAP-60, the fee); `note` says what differs.
    pub rules_ok: bool,
    pub note: String,
}

/// The settled result of a market.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    /// What an Up contract pays: 1, 0 or 0.5.
    pub payout_up: f64,
    pub ptb: Option<f64>,
    pub final_price: Option<f64>,
}

/// A list Gamma writes as JSON text inside the JSON (`"[\"Up\", \"Down\"]"`).
fn text_list(v: &Value, what: &str) -> Result<Vec<String>> {
    let text = v.as_str().ok_or_else(|| anyhow!("{what}: not a text"))?;
    serde_json::from_str(text).with_context(|| format!("{what}: not a list"))
}

/// `{coin}-updown-15m-{start}`.
fn parse_slug(slug: &str) -> Option<(Coin, i64)> {
    let mut parts = slug.splitn(4, '-');
    let coin = Coin::parse(parts.next()?)?;
    if (parts.next()?, parts.next()?) != ("updown", "15m") {
        return None;
    }
    Some((coin, parts.next()?.parse().ok()?))
}

/// The tokens of the window and whether its rules are the ones the strategy was tested on.
pub fn parse_market(e: &Value) -> Result<Market> {
    let slug = e["slug"].as_str().ok_or_else(|| anyhow!("an event without a slug"))?;
    let (coin, s) = parse_slug(slug).ok_or_else(|| anyhow!("{slug}: not a 15-minute Up/Down market"))?;
    let m = &e["markets"][0];
    let outcomes = text_list(&m["outcomes"], "outcomes")?;
    let ids = text_list(&m["clobTokenIds"], "clobTokenIds")?;
    let token = |name: &str| {
        let at = outcomes.iter().position(|o| o == name).ok_or_else(|| anyhow!("{slug}: no {name} outcome"))?;
        ids.get(at).cloned().ok_or_else(|| anyhow!("{slug}: no token of {name}"))
    };
    let (up, down) = (token("Up")?, token("Down")?);
    let condition_id = m["conditionId"].as_str().ok_or_else(|| anyhow!("{slug}: no conditionId"))?.to_string();

    let mut bad = vec![];
    match m.get("cryptoMarketConfig").filter(|c| c.is_object()) {
        None => bad.push("нет cryptoMarketConfig".to_string()),
        Some(cfg) => {
            if cfg["twapEnabled"] != Value::Bool(true) {
                bad.push(format!("TWAP выключен (twapEnabled={})", cfg["twapEnabled"]));
            }
            if cfg["twapLookbackSeconds"].as_i64() != Some(TWAP_S) {
                bad.push(format!("окно TWAP {} с (ожидалось {TWAP_S})", cfg["twapLookbackSeconds"]));
            }
        }
    }
    let mut fee = FEE;
    match m.get("feeSchedule").filter(|f| f.is_object()) {
        None => bad.push("нет feeSchedule".to_string()),
        Some(fs) => {
            fee = FeeSchedule { rate: fs["rate"].as_f64().unwrap_or(f64::NAN), exponent: fs["exponent"].as_f64().unwrap_or(f64::NAN) };
            if fee.rate != FEE.rate {
                bad.push(format!("комиссия: ставка {} (ожидалось {})", fs["rate"], FEE.rate));
            }
            if fee.exponent != FEE.exponent {
                bad.push(format!("комиссия: степень (exponent) {} (ожидалось 1)", fs["exponent"]));
            }
            if fs["takerOnly"] != Value::Bool(true) {
                bad.push(format!("комиссия не только для тейкера (takerOnly={})", fs["takerOnly"]));
            }
        }
    }
    Ok(Market { coin, s, slug: slug.to_string(), condition_id, up, down, fee, rules_ok: bad.is_empty(), note: bad.join("; ") })
}

/// The settled result, or `None` while the market is open or its prices are not final (1/0 or 0.5/0.5).
pub fn parse_outcome(e: &Value) -> Option<Outcome> {
    let m = &e["markets"][0];
    if m["closed"] != Value::Bool(true) {
        return None;
    }
    let outcomes = text_list(&m["outcomes"], "outcomes").ok()?;
    let prices = text_list(&m["outcomePrices"], "outcomePrices").ok()?;
    if prices.len() != outcomes.len() {
        return None;
    }
    let up: f64 = prices[outcomes.iter().position(|o| o == "Up")?].parse().ok()?;
    if ![0.0, 0.5, 1.0].contains(&up) {
        return None;
    }
    let em = &e["eventMetadata"];
    Some(Outcome { payout_up: up, ptb: em["priceToBeat"].as_f64(), final_price: em["finalPrice"].as_f64() })
}

/// A value for the `_` parameter that no other request of this process has had: Gamma answers through
/// a CDN that keeps a repeated URL for minutes.
fn cache_buster() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", local_ms(), N.fetch_add(1, Ordering::Relaxed))
}

/// The Gamma events of `slugs`: three attempts, `pause` and then twice `pause` apart.
pub async fn gamma_events_with(http: &reqwest::Client, gamma: &str, slugs: &[String], pause: Duration) -> Result<Vec<Value>> {
    let mut why = String::new();
    for attempt in 0..3u32 {
        let mut query: Vec<(&str, String)> = slugs.iter().map(|s| ("slug", s.clone())).collect();
        query.push(("limit", "500".to_string()));
        query.push(("_", cache_buster()));
        match http.get(format!("{gamma}/events")).query(&query).send().await {
            Ok(r) if r.status().is_success() => match r.json::<Vec<Value>>().await {
                Ok(events) => return Ok(events),
                Err(e) => why = format!("not a list of events: {e}"),
            },
            Ok(r) => why = format!("HTTP {}", r.status().as_u16()),
            Err(e) => why = e.to_string(),
        }
        if attempt < 2 {
            tokio::time::sleep(pause * (attempt + 1)).await;
        }
    }
    bail!("gamma: no answer after 3 attempts: {why}")
}

async fn window_events(http: &reqwest::Client, gamma: &str, s: i64, coins: &[Coin]) -> Result<Vec<Value>> {
    let slugs: Vec<String> = coins.iter().map(|c| format!("{}-updown-15m-{s}", c.as_str())).collect();
    let events = gamma_events_with(http, gamma, &slugs, Duration::from_secs(2)).await?;
    Ok(events.into_iter().filter(|e| slugs.iter().any(|s| e["slug"] == *s.as_str()) && e["markets"][0].is_object()).collect())
}

/// The markets of window `s`, only for the coins Gamma knows, in the order of the coins.
pub async fn fetch_window(http: &reqwest::Client, gamma: &str, s: i64, coins: &[Coin]) -> Result<Vec<Market>> {
    let mut markets = window_events(http, gamma, s, coins).await?.iter().map(parse_market).collect::<Result<Vec<_>>>()?;
    markets.sort_by_key(|m| coins.iter().position(|c| *c == m.coin));
    Ok(markets)
}

/// The settled outcomes of window `s`; the coins still open are left out.
pub async fn fetch_outcomes(http: &reqwest::Client, gamma: &str, s: i64, coins: &[Coin]) -> Result<Vec<(Coin, Outcome)>> {
    let mut out: Vec<(Coin, Outcome)> = window_events(http, gamma, s, coins)
        .await?
        .iter()
        .filter_map(|e| Some((parse_slug(e["slug"].as_str()?)?.0, parse_outcome(e)?)))
        .collect();
    out.sort_by_key(|(c, _)| coins.iter().position(|x| x == c));
    Ok(out)
}
