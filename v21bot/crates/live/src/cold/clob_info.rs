//! The CLOB: the settings of a market (`GET /clob-markets/{condition_id}`, spec §3), the rules the
//! strategy needs of them (§7.4), a token's book over REST (§5.2) and the round trip to the venue
//! (§6.2).

use std::collections::VecDeque;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use v21_core::fill::FeeSchedule;

use crate::cold::gamma::{Market, FEE};

/// The settings of a market on the exchange.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarketInfo {
    /// `itode`: the taker's order is held before it is matched (the taker delay is on).
    pub taker_delay: bool,
    /// `mts`: the price step.
    pub tick: f64,
    /// `mos`: the smallest order, contracts.
    pub min_size: f64,
    /// `fd.r` and `fd.e`.
    pub fee: FeeSchedule,
    /// `fd.to`.
    pub taker_only: bool,
}

pub fn parse_market_info(v: &Value) -> Result<MarketInfo> {
    let num = |x: &Value, what: &str| x.as_f64().ok_or_else(|| anyhow!("clob market: no {what}"));
    Ok(MarketInfo {
        taker_delay: v["itode"].as_bool().ok_or_else(|| anyhow!("clob market: no itode"))?,
        tick: num(&v["mts"], "mts")?,
        min_size: num(&v["mos"], "mos")?,
        fee: FeeSchedule { rate: num(&v["fd"]["r"], "fd.r")?, exponent: num(&v["fd"]["e"], "fd.e")? },
        taker_only: v["fd"]["to"].as_bool().ok_or_else(|| anyhow!("clob market: no fd.to"))?,
    })
}

/// `None` if the market's rules are the ones of spec §7.4 (TWAP-60, the fee 0.07 with exponent 1 for
/// the taker only, the taker delay on), else what differs.
pub fn rules_note(m: &Market, info: Option<&MarketInfo>) -> Option<String> {
    let mut bad = vec![];
    if !m.rules_ok {
        bad.push(m.note.clone());
    }
    match info {
        None => bad.push("нет настроек рынка в CLOB".to_string()),
        Some(i) => {
            if !i.taker_delay {
                bad.push("задержка тейкера выключена (itode=false)".to_string());
            }
            if i.fee != FEE || !i.taker_only {
                bad.push(format!(
                    "CLOB: комиссия r={}, e={}, to={} (ожидалось {}, {}, true)",
                    i.fee.rate, i.fee.exponent, i.taker_only, FEE.rate, FEE.exponent
                ));
            }
        }
    }
    (!bad.is_empty()).then(|| bad.join("; "))
}

pub async fn fetch_market_info(http: &reqwest::Client, clob: &str, condition_id: &str) -> Result<MarketInfo> {
    let r = http.get(format!("{clob}/clob-markets/{condition_id}")).send().await?;
    if !r.status().is_success() {
        return Err(anyhow!("clob-markets/{condition_id}: HTTP {}", r.status().as_u16()));
    }
    parse_market_info(&r.json().await.context("clob-markets: not JSON")?)
}

/// The levels of a side of a REST book: `[{"price": "0.5", "size": "10"}, ...]`.
fn levels(v: &Value) -> Result<Vec<(f64, f64)>> {
    let num = |x: &Value| x.as_str().and_then(|t| t.parse().ok()).or_else(|| x.as_f64());
    v.as_array()
        .ok_or_else(|| anyhow!("book: a side is not a list"))?
        .iter()
        .map(|l| num(&l["price"]).zip(num(&l["size"])).ok_or_else(|| anyhow!("book: a level cannot be read")))
        .collect()
}

/// A token's book over REST: `(venue ms, bids, asks)`, the levels as the venue lists them.
pub async fn fetch_book(http: &reqwest::Client, clob: &str, token: &str) -> Result<(i64, Vec<(f64, f64)>, Vec<(f64, f64)>)> {
    let r = http.get(format!("{clob}/book")).query(&[("token_id", token)]).send().await?;
    if !r.status().is_success() {
        return Err(anyhow!("book of {token}: HTTP {}", r.status().as_u16()));
    }
    let v: Value = r.json().await.context("book: not JSON")?;
    let ts = v["timestamp"].as_str().and_then(|t| t.parse().ok()).or_else(|| v["timestamp"].as_i64()).ok_or_else(|| anyhow!("book: no timestamp"))?;
    Ok((ts, levels(&v["bids"])?, levels(&v["asks"])?))
}

/// One round trip to the venue over the kept connection: `GET /time`, ms.
pub async fn measure_rtt(http: &reqwest::Client, clob: &str) -> Result<f64> {
    let started = Instant::now();
    let r = http.get(format!("{clob}/time")).send().await?;
    let _ = r.bytes().await?;
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

/// The last 20 round trips.
#[derive(Clone, Debug, Default)]
pub struct Rtt {
    last: VecDeque<f64>,
}

impl Rtt {
    const KEEP: usize = 20;

    pub fn push(&mut self, ms: f64) {
        if self.last.len() == Self::KEEP {
            self.last.pop_front();
        }
        self.last.push_back(ms);
    }

    pub fn median_ms(&self) -> Option<f64> {
        let mut v: Vec<f64> = self.last.iter().copied().collect();
        v.sort_by(f64::total_cmp);
        let n = v.len();
        match n {
            0 => None,
            _ if n % 2 == 1 => Some(v[n / 2]),
            _ => Some((v[n / 2 - 1] + v[n / 2]) / 2.0),
        }
    }
}
