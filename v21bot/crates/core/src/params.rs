//! The parameters of v2 and v2.1, read only from the two frozen files.
//!
//! v2.1 is v2 with two changes (`strategy_v2_1.json`: "every parameter of v2 applies unless changed
//! below"). Before anything is read, `strategy_v2.json` must be the file v2.1 was frozen on top of:
//! its canonical sha256 is checked against `base.sha256_canonical`, as `ruslan_v2_1.py` does.

use serde_json::Value;

use crate::pyjson::canonical_sha256;
use crate::window::{parse_utc_ms, Coin};

/// The rule of one version of the strategy.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Coins whose markets are watched: their signals count in the per-window coin rule.
    pub watch_coins: Vec<Coin>,
    /// Coins that may trade.
    pub trade_coins: Vec<Coin>,
    /// Price band of the favourite: `lo <= ask <= hi`.
    pub lo: f64,
    pub hi: f64,
    /// Monitoring runs from `t0` to `t1` seconds after the window start.
    pub t0: f64,
    pub t1: f64,
    /// Fill latency of the backtest, s: the ask this long after the signal is the fill price.
    pub lat: f64,
    /// Highest price an order may fill at.
    pub max_fill: f64,
    /// Minimum distance, in sigmas.
    pub z_min: f64,
    /// Coins traded per window at most: the coin with a later counted signal does not trade.
    pub max_coins: u32,
    /// UTC hours `[from, to)` of the entry moment that may trade.
    pub hours: Vec<(u32, u32)>,
    /// Sigma lookback, s.
    pub sig_lb: i64,
    /// Taker fee rate: `shares * fee * p * (1 - p)`.
    pub fee: f64,
    /// Backtest slippage per share.
    pub slip: f64,
    /// Windows this many minutes around a venue incident are left out of the backtest.
    pub incident_buffer_min: i64,
}

/// The forward-test rule of v2.1: after `stop_after_trades` trades, a win rate below
/// `stop_if_win_below` means there is no edge.
#[derive(Clone, Debug, PartialEq)]
pub struct ForwardRules {
    pub stop_after_trades: u32,
    pub stop_if_win_below: f64,
}

/// The dates that split the backtest into its periods, unix ms.
#[derive(Clone, Debug, PartialEq)]
pub struct Dates {
    pub research_split_ms: i64,
    pub rule_selected_ms: i64,
    pub v2_forward_ms: i64,
    pub v21_forward_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Strategy {
    pub v2: Params,
    pub v21: Params,
    pub forward: ForwardRules,
    pub dates: Dates,
}

#[derive(Debug, thiserror::Error)]
pub enum ParamsError {
    #[error("bad json: {0}")]
    Json(String),
    #[error("strategy_v2.json differs from the one v2.1 was frozen with: expected {expected}, got {got}")]
    BaseHash { expected: String, got: String },
    #[error("missing or bad field {0}")]
    Field(&'static str),
}

fn at<'a>(v: &'a Value, path: &'static str) -> Result<&'a Value, ParamsError> {
    path.split('.').try_fold(v, |v, key| v.get(key)).ok_or(ParamsError::Field(path))
}

fn float(v: &Value, path: &'static str) -> Result<f64, ParamsError> {
    at(v, path)?.as_f64().ok_or(ParamsError::Field(path))
}

fn int(v: &Value, path: &'static str) -> Result<i64, ParamsError> {
    at(v, path)?.as_i64().ok_or(ParamsError::Field(path))
}

fn time_ms(v: &Value, path: &'static str) -> Result<i64, ParamsError> {
    at(v, path)?.as_str().and_then(parse_utc_ms).ok_or(ParamsError::Field(path))
}

fn coins(v: &Value, path: &'static str) -> Result<Vec<Coin>, ParamsError> {
    let list = at(v, path)?.as_array().ok_or(ParamsError::Field(path))?;
    list.iter()
        .map(|c| c.as_str().and_then(Coin::parse).ok_or(ParamsError::Field(path)))
        .collect()
}

fn hours(v: &Value, path: &'static str) -> Result<Vec<(u32, u32)>, ParamsError> {
    let list = at(v, path)?.as_array().ok_or(ParamsError::Field(path))?;
    list.iter()
        .map(|pair| {
            let hour = |k: usize| {
                pair.get(k).and_then(Value::as_u64).and_then(|h| u32::try_from(h).ok())
            };
            match (pair.as_array().map(Vec::len), hour(0), hour(1)) {
                (Some(2), Some(a), Some(b)) => Ok((a, b)),
                _ => Err(ParamsError::Field(path)),
            }
        })
        .collect()
}

fn parse(text: &str) -> Result<Value, ParamsError> {
    serde_json::from_str(text).map_err(|e| ParamsError::Json(e.to_string()))
}

/// The strategy from the texts of `strategy_v2.json` and `strategy_v2_1.json`.
pub fn load_strategy(v2_json: &str, v21_json: &str) -> Result<Strategy, ParamsError> {
    let v2 = parse(v2_json)?;
    let v21 = parse(v21_json)?;
    let expected = at(&v21, "base.sha256_canonical")?
        .as_str()
        .ok_or(ParamsError::Field("base.sha256_canonical"))?
        .to_string();
    let got = canonical_sha256(v2_json).map_err(|e| ParamsError::Json(e.to_string()))?;
    if got != expected {
        return Err(ParamsError::BaseHash { expected, got });
    }

    let band = at(&v2, "entry.price_band")?
        .as_array()
        .filter(|b| b.len() == 2)
        .ok_or(ParamsError::Field("entry.price_band"))?;
    let edge = |k: usize| band[k].as_f64().ok_or(ParamsError::Field("entry.price_band"));
    let watch = coins(&v2, "markets.coins")?;
    let base = Params {
        watch_coins: watch.clone(),
        trade_coins: watch,
        lo: edge(0)?,
        hi: edge(1)?,
        t0: float(&v2, "entry.monitor_from_s")?,
        t1: float(&v2, "entry.monitor_to_s")?,
        lat: float(&v2, "entry.fill_latency_s")?,
        max_fill: float(&v2, "entry.max_fill_price")?,
        z_min: float(&v2, "filters.distance_min_sigma")?,
        max_coins: u32::try_from(int(&v2, "filters.max_coins_per_window")?)
            .map_err(|_| ParamsError::Field("filters.max_coins_per_window"))?,
        hours: hours(&v2, "filters.utc_hours")?,
        sig_lb: int(&v2, "filters.sigma_lookback_s")?,
        fee: float(&v2, "costs.taker_fee_rate")?,
        slip: float(&v2, "costs.slippage_per_share")?,
        incident_buffer_min: int(&v2, "filters.skip_incident_windows_buffer_min")?,
    };
    let v21_params = Params {
        trade_coins: coins(&v21, "changes.trade_coins")?,
        hours: hours(&v21, "changes.utc_hours")?,
        ..base.clone()
    };
    Ok(Strategy {
        v2: base,
        v21: v21_params,
        forward: ForwardRules {
            stop_after_trades: u32::try_from(int(&v21, "forward_rules.stop_after_trades")?)
                .map_err(|_| ParamsError::Field("forward_rules.stop_after_trades"))?,
            stop_if_win_below: float(&v21, "forward_rules.stop_if_win_below")?,
        },
        dates: Dates {
            research_split_ms: time_ms(&v2, "research_split_utc")?,
            rule_selected_ms: time_ms(&v2, "rule_selected_utc")?,
            v2_forward_ms: time_ms(&v2, "forward_start_utc")?,
            v21_forward_ms: time_ms(&v21, "forward_start_utc")?,
        },
    })
}
