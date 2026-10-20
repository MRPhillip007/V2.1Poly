//! The CSV tables of the journal (spec §8): `trades.csv`, `signals.csv` and `windows.csv`, rebuilt whole
//! from the journal; they hold nothing it does not. UTF-8 with a byte order mark, which Excel needs for
//! Cyrillic; rows end in CRLF, as Python's `csv` writes them. A table another program holds (Excel) is
//! left as it was until the next rebuild. The port of `paper/journal.py`:
//! - a cell is empty for a missing or null field, `True` / `False` for a boolean, Python's repr for a
//!   float, compact JSON for a list or an object;
//! - `window_start_utc` is made from `s`, unless the record has its own;
//! - `trades.csv` has a row per `entry` (a repeated trade id keeps its first place), joined with the
//!   `settle` of the trade; its `px_backtest` is the `px` of the `signal` of the same window, coin and
//!   `t_trig_s`; its `status` comes from the payout per contract: open, win (1), loss (0), half;
//! - `windows.csv` merges the `window` and `window_result` records of a window: a later value that is
//!   not null wins.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value};
use v21_core::pyjson::py_float_repr;

use crate::cold::journal::{write_atomically, Journal};
use crate::shared::utc::fmt_utc;

pub const TRADES_COLUMNS: [&str; 23] = [
    "trade_id",
    "window_start_utc",
    "coin",
    "side",
    "t_trig_s",
    "rank",
    "z",
    "stake_usd",
    "cost_usd",
    "fee_usd",
    "shares",
    "avg_price",
    "levels_used",
    "unfilled_usd",
    "px_backtest",
    "price_no_latency",
    "status",
    "outcome",
    "payout_usd",
    "pnl_usd",
    "roi",
    "balance_after",
    "settled_utc",
];

pub const SIGNALS_COLUMNS: [&str; 21] = [
    "window_start_utc",
    "coin",
    "t_trig_s",
    "side",
    "ask_at_trig",
    "px",
    "counted",
    "rank",
    "z",
    "hour_ok",
    "decision",
    "reason",
    "fast_decision",
    "fast_reason",
    "fast_rank",
    "fast_z",
    "lag_ms",
    "demo_filled_usd",
    "demo_shares",
    "demo_avg_price",
    "price_no_latency",
];

pub const WINDOWS_COLUMNS: [&str; 22] = [
    "window_start_utc",
    "markets_ok",
    "rules_ok",
    "ws_ok_840_870",
    "binance_ok",
    "n_signals",
    "n_trades",
    "outcome_btc",
    "outcome_eth",
    "outcome_sol",
    "official_ptb_btc",
    "official_ptb_eth",
    "official_ptb_sol",
    "p0_binance_btc",
    "p0_binance_eth",
    "p0_binance_sol",
    "feed_lag_max_ms",
    "late_dropped",
    "book_reloads",
    "coverage_lost_ms",
    "binance_fallback",
    "note",
];

/// The window starts a date can be made of: years 1 to 9999, as Python's `datetime`.
const DATES: std::ops::RangeInclusive<i64> = -62_135_596_800..=253_402_300_799;
/// How near to 1 or 0 a payout per contract is a win or a loss.
const PAYOUT_TOL: f64 = 1e-9;

type Row = Map<String, Value>;

/// Writes the three tables into `dir` again. False if a table could not be written (another program
/// holds it): the others are, and the next call tries again.
pub fn rebuild_tables(journal: &Journal, dir: &Path) -> Result<bool> {
    let recs = journal.all_but_bbo()?;
    let mut ok = true;
    for (name, columns, rows) in [
        ("trades.csv", &TRADES_COLUMNS[..], trade_rows(&recs)),
        ("signals.csv", &SIGNALS_COLUMNS[..], signal_rows(&recs)),
        ("windows.csv", &WINDOWS_COLUMNS[..], window_rows(&recs)),
    ] {
        ok &= write_atomically(&dir.join(name), table(columns, &rows).as_bytes(), false).is_ok();
    }
    Ok(ok)
}

/// Writes a table of plain cells at `path`, as the journal's tables are written.
pub fn write_csv(path: &Path, columns: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut out = String::from("\u{feff}");
    for cells in std::iter::once(columns.iter().map(|c| (*c).to_string()).collect::<Vec<_>>()).chain(rows.iter().cloned()) {
        out.push_str(&cells.iter().map(|c| quote(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    write_atomically(path, out.as_bytes(), false)
}

fn table(columns: &[&str], rows: &[Row]) -> String {
    let mut out = String::from("\u{feff}");
    let mut line = |cells: &mut dyn Iterator<Item = String>| {
        let cells: Vec<String> = cells.map(|c| quote(&c)).collect();
        out.push_str(&cells.join(","));
        out.push_str("\r\n");
    };
    line(&mut columns.iter().map(|c| (*c).to_string()));
    for row in rows {
        line(&mut columns.iter().map(|c| cell(row.get(*c))));
    }
    out
}

/// A field with a comma, a quote or a line break goes in quotes, its quotes doubled.
fn quote(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn cell(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(b)) => if *b { "True" } else { "False" }.to_string(),
        Some(Value::Number(n)) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(x) => py_float_repr(x),
            None => n.to_string(),
        },
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// A whole number, as Python's `_as_int` takes one: not a boolean, a float only if whole.
fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|x| x.fract() == 0.0 && x.abs() < 9.2e18).map(|x| x as i64)),
        _ => None,
    }
}

/// The record as a row, with its `window_start_utc` made from `s`.
fn with_start(mut row: Row) -> Row {
    if row.get("window_start_utc").is_none_or(Value::is_null)
        && let Some(s) = row.get("s").and_then(as_int).filter(|s| DATES.contains(s))
    {
        row.insert("window_start_utc".into(), Value::String(fmt_utc(s)));
    }
    row
}

fn object(v: &Value) -> Row {
    v.as_object().cloned().unwrap_or_default()
}

fn status(entry: &Row, settle: Option<&Row>) -> &'static str {
    let Some(settle) = settle else { return "open" };
    let shares = entry.get("shares").and_then(Value::as_f64).filter(|s| *s != 0.0);
    let payout = settle.get("payout_usd").and_then(Value::as_f64);
    let (Some(shares), Some(payout)) = (shares, payout) else { return "" };
    let per_share = payout / shares;
    if per_share >= 1.0 - PAYOUT_TOL {
        "win"
    } else if per_share <= PAYOUT_TOL {
        "loss"
    } else {
        "half"
    }
}

/// (s, coin, t_trig_s) of a record.
fn check_key(r: &Value) -> Option<(i64, String, u64)> {
    Some((as_int(&r["s"])?, r["coin"].as_str()?.to_string(), r["t_trig_s"].as_f64()?.to_bits()))
}

fn trade_rows(recs: &[Value]) -> Vec<Row> {
    let px: HashMap<(i64, String, u64), Value> =
        recs.iter().filter(|r| r["kind"] == "signal").filter_map(|r| Some((check_key(r)?, r["px"].clone()))).collect();
    // entries by trade id, in the order of their first record; a record without an id is a row of its own
    let mut entries: Vec<Row> = vec![];
    let mut place: HashMap<String, usize> = HashMap::new();
    let mut settles: HashMap<String, Row> = HashMap::new();
    for r in recs {
        if r["kind"] == "entry" {
            match r["trade_id"].as_str() {
                Some(id) if place.contains_key(id) => entries[place[id]] = object(r),
                Some(id) => {
                    place.insert(id.to_string(), entries.len());
                    entries.push(object(r));
                }
                None => entries.push(object(r)),
            }
        } else if r["kind"] == "settle"
            && let Some(id) = r["trade_id"].as_str()
        {
            settles.insert(id.to_string(), object(r));
        }
    }
    entries
        .into_iter()
        .map(|entry| {
            let settle = entry.get("trade_id").and_then(Value::as_str).and_then(|id| settles.get(id));
            let status = status(&entry, settle);
            let backtest = check_key(&Value::Object(entry.clone())).and_then(|k| px.get(&k).cloned());
            let mut row = entry;
            row.extend(settle.cloned().unwrap_or_default());
            if let Some(px) = backtest {
                row.insert("px_backtest".into(), px);
            }
            row.insert("status".into(), Value::String(status.into()));
            with_start(row)
        })
        .collect()
}

fn signal_rows(recs: &[Value]) -> Vec<Row> {
    recs.iter()
        .filter(|r| r["kind"] == "signal")
        .map(|r| {
            let mut row = object(r);
            if let Some(demo) = r["demo"].as_object() {
                for (column, field) in [("demo_filled_usd", "notional_usd"), ("demo_shares", "shares"), ("demo_avg_price", "avg_price")] {
                    if let Some(v) = demo.get(field) {
                        row.insert(column.into(), v.clone());
                    }
                }
            }
            with_start(row)
        })
        .collect()
}

fn window_rows(recs: &[Value]) -> Vec<Row> {
    let mut merged: BTreeMap<i64, Row> = BTreeMap::new();
    for r in recs.iter().filter(|r| r["kind"] == "window" || r["kind"] == "window_result") {
        if let Some(s) = as_int(&r["s"]) {
            let row = merged.entry(s).or_default();
            for (k, v) in object(r).into_iter().filter(|(_, v)| !v.is_null()) {
                row.insert(k, v);
            }
        }
    }
    merged.into_values().map(with_start).collect()
}
