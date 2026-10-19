//! Binance over REST: the 1-s klines of the warm-up and of the backfills, and the server time for the
//! clock. Each request tries the addresses in order: api.binance.com, then the mirror
//! data-api.binance.vision (the first answers HTTP 451 from some places).

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

use crate::shared::clock::local_ms;

/// The most klines one request gives.
const PAGE: i64 = 1000;

/// GET `path` with `query` from the first of `bases` that answers with success.
async fn get_json(http: &reqwest::Client, bases: &[String], path: &str, query: &[(&str, String)]) -> Result<Value> {
    let mut why = Vec::new();
    for base in bases {
        match http.get(format!("{base}{path}")).query(query).send().await {
            Ok(r) if r.status().is_success() => return r.json().await.with_context(|| format!("{base}{path}: not JSON")),
            Ok(r) => why.push(format!("{base}{path}: HTTP {}", r.status().as_u16())),
            Err(e) => why.push(format!("{base}{path}: {e}")),
        }
    }
    Err(anyhow!("{}", why.join("; ")))
}

/// `(open ms, close)` of the 1-s klines of the seconds `q0..=q1`, in pages of 1 000. Binance serves a
/// kline only for a second with trades.
pub async fn klines(http: &reqwest::Client, bases: &[String], symbol: &str, q0: i64, q1: i64) -> Result<Vec<(i64, f64)>> {
    let mut rows = Vec::new();
    let mut start = q0;
    while start <= q1 {
        let end = (start + PAGE - 1).min(q1);
        let query = [
            ("symbol", symbol.to_string()),
            ("interval", "1s".to_string()),
            ("startTime", (start * 1000).to_string()),
            ("endTime", (end * 1000 + 999).to_string()),
            ("limit", PAGE.to_string()),
        ];
        let page = get_json(http, bases, "/api/v3/klines", &query).await?;
        for k in page.as_array().ok_or_else(|| anyhow!("klines of {symbol}: not a list"))? {
            let open = k[0].as_i64().ok_or_else(|| anyhow!("klines of {symbol}: no open time"))?;
            let close = k[4].as_str().and_then(|c| c.parse().ok()).ok_or_else(|| anyhow!("klines of {symbol}: no close"))?;
            rows.push((open, close));
        }
        start = end + 1;
    }
    Ok(rows)
}

/// Binance's time and when it was asked and answered by this machine's clock: `(server, sent,
/// received)`, unix ms. The offset of this clock is the server time minus the middle.
pub async fn server_time_ms(http: &reqwest::Client, bases: &[String]) -> Result<(i64, i64, i64)> {
    let sent = local_ms();
    let v = get_json(http, bases, "/api/v3/time", &[]).await?;
    let received = local_ms();
    let server = v["serverTime"].as_i64().ok_or_else(|| anyhow!("/api/v3/time: no serverTime"))?;
    Ok((server, sent, received))
}
