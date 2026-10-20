//! The journal, the restart state and the CSV tables (spec §8): the port of `paper/journal.py` and of
//! the restart rules of `paper/engine.py` (`_reconcile`).

use std::path::Path;

use serde_json::{json, Value};
use v21_core::money::{DemoAccount, DemoPosition};
use v21_core::window::{Coin, Side};
use v21_live::cold::csv::rebuild_tables;
use v21_live::cold::journal::{load_state, save_state, Journal, State, WindowWait};
use v21_live::shared::utc::{fmt_utc, iso_ms};

/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;

fn signal_rec(coin: &str, t_trig: f64, px: f64) -> Value {
    json!({"s": S, "coin": coin, "side": "Up", "t_trig_s": t_trig, "ask_at_trig": 0.8, "px": px, "counted": true,
           "rank": 1, "z": 1.7, "hour_ok": true, "decision": "вход", "reason": "вход",
           "fast_decision": "вход", "fast_reason": "вход", "fast_rank": 1, "fast_z": 1.69, "lag_ms": 40,
           "demo": {"shares": 24.5, "notional_usd": 20.0, "fee_usd": 0.27, "avg_price": 0.8163, "levels_used": 2, "unfilled_usd": 0.0},
           "price_no_latency": 0.81})
}

fn entry_rec(coin: &str, t_trig: f64) -> Value {
    json!({"trade_id": format!("{S}-{coin}"), "s": S, "coin": coin, "side": "Up", "t_trig_s": t_trig, "rank": 1, "z": 1.7,
           "stake_usd": 20.0, "cost_usd": 20.27, "fee_usd": 0.27, "shares": 24.5, "avg_price": 0.8163, "levels_used": 2,
           "unfilled_usd": 0.0, "price_no_latency": 0.81, "balance_after": 79.73})
}

fn settle_rec(coin: &str, payout: f64) -> Value {
    json!({"trade_id": format!("{S}-{coin}"), "outcome": "Up", "payout_usd": payout, "pnl_usd": payout - 20.27,
           "roi": (payout - 20.27) / 20.27, "balance_after": 79.73 + payout, "official_ptb": 61_000.5,
           "official_final": 61_020.0, "settled_utc": "2026-10-02 03:31:05"})
}

fn read_table(path: &Path) -> (Vec<String>, Vec<Vec<String>>) {
    let raw = std::fs::read(path).unwrap();
    assert!(raw.starts_with("\u{feff}".as_bytes()), "{}: a byte order mark for Excel", path.display());
    let text = String::from_utf8(raw[3..].to_vec()).unwrap();
    let mut rows = parse_csv(&text);
    let header = rows.remove(0);
    (header, rows)
}

/// A CSV reader enough for the tables: quoted fields with "" and line breaks, rows ending in \r\n.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field, mut quoted, mut chars) = (vec![], vec![], String::new(), false, text.chars().peekable());
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    rows
}

fn cell<'a>(header: &[String], row: &'a [String], column: &str) -> &'a str {
    &row[header.iter().position(|h| h == column).unwrap_or_else(|| panic!("no column {column}"))]
}

// ---------------------------------------------------------------- the journal

#[test]
fn records_are_strict_json_with_kind_and_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out/journal.jsonl");
    let mut j = Journal::open(&path).unwrap();
    let before = iso_ms(v21_live::shared::clock::local_ms() - 1000);
    j.append("signal", json!({"s": S, "coin": "btc", "z": f64::NAN, "px": f64::INFINITY, "kind": "not this", "ts_utc": "nor this",
                              "reason": "вне часов", "nested": {"a": [f64::NAN, 1.5]}}))
        .unwrap();
    j.append("window", json!({"s": S, "note": "x"})).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(!raw.contains("NaN") && !raw.contains("inf"), "{raw}");
    assert!(raw.contains("вне часов"), "Cyrillic as it is");
    assert!(lines[0].starts_with(r#"{"kind":"signal","ts_utc":""#), "{}", lines[0]);
    let rec: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!((rec["kind"].as_str(), rec["s"].as_i64(), &rec["z"], &rec["px"]), (Some("signal"), Some(S), &Value::Null, &Value::Null));
    assert_eq!(rec["nested"], json!({"a": [null, 1.5]}));
    let ts = rec["ts_utc"].as_str().unwrap();
    assert!(ts.ends_with("+00:00") && ts.len() == 29 && ts > before.as_str(), "{ts}");
    // in order, by kind; a new process sees them too
    let again = Journal::open(&path).unwrap();
    assert_eq!(again.records("window").unwrap().len(), 1);
    assert_eq!(again.all().unwrap().iter().map(|r| r["kind"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["signal", "window"]);
}

#[test]
fn a_torn_last_line_is_skipped_and_the_next_record_starts_a_new_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.jsonl");
    let mut j = Journal::open(&path).unwrap();
    j.append("window", json!({"s": S})).unwrap();
    drop(j);
    // the process died in the middle of a record, inside a Cyrillic letter
    let torn = r#"{"kind": "signal", "reason": "пропуск"#.as_bytes();
    let cut = torn.len() - 3;
    let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    std::io::Write::write_all(&mut file, &torn[..cut]).unwrap();
    drop(file);
    let mut j = Journal::open(&path).unwrap();
    assert_eq!(j.all().unwrap().len(), 1);
    j.append("window", json!({"s": S + 900})).unwrap();
    assert_eq!(j.records("window").unwrap().iter().map(|r| r["s"].as_i64().unwrap()).collect::<Vec<_>>(), [S, S + 900]);
}

#[test]
fn utc_times_are_written_like_paper() {
    assert_eq!(fmt_utc(S), "2026-10-02 03:15:00");
    assert_eq!(iso_ms(0), "1970-01-01T00:00:00.000+00:00");
    assert_eq!(iso_ms(S * 1000 + 59_123), "2026-10-02T03:15:59.123+00:00");
    assert_eq!(fmt_utc(951_782_400), "2000-02-29 00:00:00");
}

// ---------------------------------------------------------------- the restart state

fn position(coin: Coin) -> DemoPosition {
    DemoPosition { id: format!("{S}-{}", coin.as_str()), s: S, coin, side: Side::Up, shares: 24.5, cost_usd: 20.27, fee_usd: 0.27, avg_price: 0.8163 }
}

#[test]
fn the_state_goes_and_comes_back_whole() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let path = dir.path().join("state.json");
    let mut account = DemoAccount::new(100.0);
    account.open(position(Coin::Btc));
    let st = State {
        account,
        traded: vec![(S, Coin::Btc)],
        unsettled: [(S - 900, WindowWait { coins: vec![Coin::Btc, Coin::Eth], done: vec![Coin::Eth] })].into(),
        last_summary_date: Some("2026-10-01".into()),
        started_utc: "2026-10-01T00:00:00.000+00:00".into(),
        clock_offset_ms: Some(-1040),
    };
    save_state(&path, &st).unwrap();
    assert!(!dir.path().join("state.json.tmp").exists());
    let back = load_state(&path, &j, 100.0).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&st).unwrap());
    // no state yet: a fresh account
    let fresh = load_state(&dir.path().join("none.json"), &j, 100.0).unwrap();
    assert_eq!((fresh.account, fresh.traded.len()), (DemoAccount::new(100.0), 0));
    // a damaged file is an error, not a fresh start
    std::fs::write(&path, r#"{"account": {"cash": 95."#).unwrap();
    assert!(load_state(&path, &j, 100.0).is_err());
}

#[test]
fn a_trade_in_the_journal_missing_from_the_state_is_restored() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let path = dir.path().join("state.json");
    // the state was saved before the entry (the process died between the journal and state.json)
    let stale = State {
        account: DemoAccount::new(100.0),
        traded: vec![],
        unsettled: Default::default(),
        last_summary_date: None,
        started_utc: "2026-10-01T00:00:00.000+00:00".into(),
        clock_offset_ms: None,
    };
    save_state(&path, &stale).unwrap();
    j.append("entry", entry_rec("btc", 840.0)).unwrap();
    let st = load_state(&path, &j, 100.0).unwrap();
    assert!((st.account.cash - (100.0 - 20.27)).abs() < 1e-9);
    assert_eq!(st.account.positions, [position(Coin::Btc)]);
    assert_eq!(st.traded, [(S, Coin::Btc)]);
    // a state that has it already is left as it is
    save_state(&path, &st).unwrap();
    let again = load_state(&path, &j, 100.0).unwrap();
    assert_eq!(again.account, st.account);
}

#[test]
fn a_settlement_in_the_journal_is_applied_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let path = dir.path().join("state.json");
    let mut account = DemoAccount::new(100.0);
    account.open(position(Coin::Btc));
    let st = State {
        account,
        traded: vec![(S, Coin::Btc)],
        unsettled: Default::default(),
        last_summary_date: None,
        started_utc: "2026-10-01T00:00:00.000+00:00".into(),
        clock_offset_ms: None,
    };
    save_state(&path, &st).unwrap();
    j.append("entry", entry_rec("btc", 840.0)).unwrap();
    // settled, but state.json missed it
    j.append("settle", settle_rec("btc", 24.5)).unwrap();
    let st = load_state(&path, &j, 100.0).unwrap();
    assert!(st.account.positions.is_empty());
    assert!((st.account.cash - (100.0 - 20.27 + 24.5)).abs() < 1e-9);
    // the next start: the trade is settled in the state too, nothing is opened or paid again
    save_state(&path, &st).unwrap();
    assert_eq!(load_state(&path, &j, 100.0).unwrap().account, st.account);
}

// ---------------------------------------------------------------- the tables

const TRADES: &str = "trade_id,window_start_utc,coin,side,t_trig_s,rank,z,stake_usd,cost_usd,fee_usd,shares,avg_price,levels_used,unfilled_usd,px_backtest,price_no_latency,status,outcome,payout_usd,pnl_usd,roi,balance_after,settled_utc";
const SIGNALS: &str = "window_start_utc,coin,t_trig_s,side,ask_at_trig,px,counted,rank,z,hour_ok,decision,reason,fast_decision,fast_reason,fast_rank,fast_z,lag_ms,demo_filled_usd,demo_shares,demo_avg_price,price_no_latency";
const WINDOWS: &str = "window_start_utc,markets_ok,rules_ok,ws_ok_840_870,binance_ok,n_signals,n_trades,outcome_btc,outcome_eth,outcome_sol,official_ptb_btc,official_ptb_eth,official_ptb_sol,p0_binance_btc,p0_binance_eth,p0_binance_sol,feed_lag_max_ms,late_dropped,book_reloads,coverage_lost_ms,binance_fallback,note";

#[test]
fn tables_have_these_columns_and_a_bom() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    for (name, columns) in [("trades.csv", TRADES), ("signals.csv", SIGNALS), ("windows.csv", WINDOWS)] {
        let (header, rows) = read_table(&dir.path().join(name));
        assert_eq!(header.join(","), columns, "{name}");
        assert!(rows.is_empty());
    }
    j.append("signal", signal_rec("btc", 840.0, 0.83)).unwrap();
    j.append("signal", json!({"s": S, "coin": "eth", "z": 1.5e-5, "rank": null})).unwrap();
    j.append("window", json!({"s": S, "markets_ok": true, "rules_ok": true, "ws_ok_840_870": false, "binance_ok": true,
                              "n_signals": 1, "n_trades": 1, "note": "пропуски данных Polymarket в решающую минуту",
                              "feed_lag_max_ms": 310, "late_dropped": 0, "book_reloads": 2, "coverage_lost_ms": 0,
                              "binance_fallback": false, "p0_binance_btc": 61_000.25, "p0_binance_eth": null}))
        .unwrap();
    // the outcomes come later, in pieces
    j.append("window_result", json!({"s": S, "outcome_btc": "Up", "official_ptb_btc": 61_000.5})).unwrap();
    j.append("window_result", json!({"s": S, "outcome_eth": "Down", "outcome_btc": null})).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    let (h, rows) = read_table(&dir.path().join("signals.csv"));
    let r = &rows[0];
    assert_eq!(
        [cell(&h, r, "window_start_utc"), cell(&h, r, "t_trig_s"), cell(&h, r, "counted"), cell(&h, r, "rank"), cell(&h, r, "reason")],
        ["2026-10-02 03:15:00", "840.0", "True", "1", "вход"]
    );
    assert_eq!([cell(&h, r, "demo_filled_usd"), cell(&h, r, "demo_shares"), cell(&h, r, "demo_avg_price"), cell(&h, r, "lag_ms")], ["20.0", "24.5", "0.8163", "40"]);
    // floats as Python writes them, null as nothing
    assert_eq!([cell(&h, &rows[1], "z"), cell(&h, &rows[1], "rank")], ["1.5e-05", ""]);
    let (h, rows) = read_table(&dir.path().join("windows.csv"));
    let r = &rows[0];
    assert_eq!(
        [cell(&h, r, "ws_ok_840_870"), cell(&h, r, "outcome_btc"), cell(&h, r, "outcome_eth"), cell(&h, r, "official_ptb_btc"), cell(&h, r, "p0_binance_eth"), cell(&h, r, "book_reloads")],
        ["False", "Up", "Down", "61000.5", "", "2"]
    );
    assert_eq!(cell(&h, r, "note"), "пропуски данных Polymarket в решающую минуту");
}

#[test]
fn trades_csv_takes_px_backtest_from_the_signal() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    j.append("signal", signal_rec("eth", 840.0, 0.86)).unwrap();
    j.append("signal", signal_rec("btc", 845.5, 0.83)).unwrap();
    j.append("entry", entry_rec("btc", 845.5)).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    let (h, rows) = read_table(&dir.path().join("trades.csv"));
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!([cell(&h, r, "trade_id"), cell(&h, r, "px_backtest"), cell(&h, r, "status"), cell(&h, r, "payout_usd")], [&format!("{S}-btc")[..], "0.83", "open", ""]);
    // settled: won, lost, half
    for (payout, status) in [(24.5, "win"), (0.0, "loss"), (12.25, "half")] {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
        j.append("entry", entry_rec("btc", 845.5)).unwrap();
        j.append("settle", settle_rec("btc", payout)).unwrap();
        assert!(rebuild_tables(&j, dir.path()).unwrap());
        let (h, rows) = read_table(&dir.path().join("trades.csv"));
        assert_eq!(cell(&h, &rows[0], "status"), status);
        assert_eq!(cell(&h, &rows[0], "balance_after"), pyfloat(79.73 + payout), "the settlement's balance");
    }
}

fn pyfloat(x: f64) -> String {
    v21_core::pyjson::py_float_repr(x)
}

#[test]
fn a_cell_with_a_comma_a_quote_or_a_line_break_is_quoted() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    j.append("signal", json!({"s": S, "reason": "with, a comma and \"quotes\"\nand a line break"})).unwrap();
    j.append("signal", json!({"s": 1_000_000_000_000_000_000i64})).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    let (h, rows) = read_table(&dir.path().join("signals.csv"));
    assert_eq!(cell(&h, &rows[0], "reason"), "with, a comma and \"quotes\"\nand a line break");
    assert_eq!(cell(&h, &rows[1], "window_start_utc"), "", "no such date");
}

#[test]
fn a_locked_csv_does_not_stop_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    j.append("entry", entry_rec("btc", 840.0)).unwrap();
    // something holds trades.csv: here a folder of that name, which no file can replace
    std::fs::create_dir(dir.path().join("trades.csv")).unwrap();
    assert!(!rebuild_tables(&j, dir.path()).unwrap());
    assert!(dir.path().join("signals.csv").is_file() && dir.path().join("windows.csv").is_file(), "the other tables are written");
    assert!(!dir.path().join("trades.csv.tmp").exists());
    j.append("signal", signal_rec("btc", 840.0, 0.83)).unwrap();
    assert_eq!(j.all().unwrap().len(), 2);
    // free again: the next rebuild writes it
    std::fs::remove_dir(dir.path().join("trades.csv")).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    let (_, rows) = read_table(&dir.path().join("trades.csv"));
    assert_eq!(rows.len(), 1);
}

#[cfg(windows)]
#[test]
fn a_csv_open_in_excel_is_written_at_the_next_rebuild() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    j.append("entry", entry_rec("btc", 840.0)).unwrap();
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    j.append("settle", settle_rec("btc", 24.5)).unwrap();
    {
        // Excel opens a table without sharing it
        let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(dir.path().join("trades.csv")).unwrap();
        assert!(!rebuild_tables(&j, dir.path()).unwrap());
    }
    let (h, rows) = read_table(&dir.path().join("trades.csv"));
    assert_eq!(cell(&h, &rows[0], "status"), "open", "the held table is as it was");
    assert!(rebuild_tables(&j, dir.path()).unwrap());
    let (h, rows) = read_table(&dir.path().join("trades.csv"));
    assert_eq!(cell(&h, &rows[0], "status"), "win");
}
