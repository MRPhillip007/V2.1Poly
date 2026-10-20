//! `v21 report` (spec §8): the demo against the backtest over a period of the journal.

use serde_json::{json, Value};
use v21_core::params::load_strategy;
use v21_core::returns::trade_return;
use v21_live::cold::journal::Journal;
use v21_live::report::report;

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const WARN_MINUS: char = '\u{2212}';

#[allow(clippy::too_many_arguments)]
fn signal(s: i64, coin: &str, side: &str, t: f64, px: f64, counted: bool, decision: (&str, &str), fast: (&str, &str), demo: Value, no_latency: Value) -> Value {
    json!({"s": s, "coin": coin, "side": side, "t_trig_s": t, "px": px, "counted": counted, "decision": decision.0, "reason": decision.1,
           "fast_decision": fast.0, "fast_reason": fast.1, "demo": demo, "price_no_latency": no_latency})
}

fn fill(shares: f64, notional: f64, unfilled: f64, price: Value) -> Value {
    json!({"shares": shares, "notional_usd": notional, "fee_usd": 0.0, "avg_price": price, "levels_used": 1, "unfilled_usd": unfilled})
}

/// A signed number with the Unicode minus, as the report writes it.
fn signed(x: f64, digits: usize, unit: &str) -> String {
    let sign = if x < 0.0 { WARN_MINUS } else { '+' };
    format!("{sign}{:.digits$}{unit}", x.abs())
}

#[test]
fn report_of_a_small_journal() {
    let st = load_strategy(V2, V21).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let enter = ("вход", "вход");
    let btc_shares = 20.0 / 0.81;
    j.append("start", json!({"mode": "demo", "stake_usd": 20.0, "demo_bank_usd": 100.0, "taker_delay_ms": 250})).unwrap();
    // 03:15: BTC bought in full and won
    j.append("signal", signal(S, "btc", "Up", 840.0, 0.80, true, enter, enter, fill(btc_shares, 20.0, 0.0, json!(0.81)), json!(0.79))).unwrap();
    j.append("entry", json!({"trade_id": format!("{S}-btc"), "s": S, "shares": btc_shares, "balance_after": 79.73})).unwrap();
    j.append("settle", json!({"trade_id": format!("{S}-btc"), "payout_usd": btc_shares, "pnl_usd": 4.42, "balance_after": 104.42})).unwrap();
    j.append("window_result", json!({"s": S, "outcome_btc": "Up"})).unwrap();
    // 03:30: ETH Down bought in part and lost
    let s2 = S + 900;
    j.append("signal", signal(s2, "eth", "Down", 845.5, 0.84, true, enter, enter, fill(15.0, 12.0, 8.0, json!(0.80)), json!(0.83))).unwrap();
    j.append("entry", json!({"trade_id": format!("{s2}-eth"), "s": s2, "shares": 15.0, "balance_after": 92.26})).unwrap();
    j.append("settle", json!({"trade_id": format!("{s2}-eth"), "payout_usd": 0.0, "pnl_usd": -12.16, "balance_after": 92.26})).unwrap();
    j.append("window_result", json!({"s": s2, "outcome_eth": "Up"})).unwrap();
    // 03:45: SOL counted but not traded; BTC entered by the exact rule, nothing in the demo and the fast
    // engine did not know the number; a failed ETH check the fast engine shot on
    let s3 = S + 1800;
    let skip = ("пропуск", "монета не торгуется");
    j.append("signal", signal(s3, "sol", "Up", 840.0, 0.82, true, skip, skip, fill(20.0 / 0.82, 20.0, 0.0, json!(0.82)), json!(0.82))).unwrap();
    j.append("signal", signal(s3, "btc", "Up", 851.2, 0.86, true, enter, ("пропуск", "номер монеты неизвестен"), fill(0.0, 0.0, 20.0, Value::Null), Value::Null)).unwrap();
    j.append("signal", signal(s3, "eth", "Up", 842.0, 0.91, false, ("пропуск", "ask > 0.88 через 0.3 с"), enter, Value::Null, Value::Null)).unwrap();
    j.append("window_result", json!({"s": s3, "outcome_sol": "Down", "outcome_btc": "Up"})).unwrap();

    let v21 = &st.v21;
    let equivalent = [trade_return(0.80, 1.0, v21.slip, v21.fee), trade_return(0.84, 0.0, v21.slip, v21.fee), trade_return(0.86, 1.0, v21.slip, v21.fee)];
    let mean = equivalent.iter().sum::<f64>() / 3.0 * 100.0;
    let want = [
        "Отчёт v21bot · демо · 02.10.2026 (UTC)".to_string(),
        "Сигналов базового правила: 4 · ордер $20 исполнился полностью в 50% · в среднем $13.00 из $20".to_string(),
        format!("Цена демо − цена бэктеста: {} · без задержки − бэктест: {}", signed(-1.0, 1, "¢"), signed(-0.02 / 3.0 * 100.0, 1, "¢")),
        "Упущенные сделки: 1 · быстрое ≠ точное: 2".to_string(),
        "Задержка тейкера в демо: 250 мс (по документации; живых ордеров ещё нет)".to_string(),
        format!("P&L демо-счёта: {WARN_MINUS}$7.74 · баланс $92.26 · сделок 2, выигрышей 1"),
        format!("Бэктест-эквивалент v2.1: сделок 3 · доход на сделку {} (проскальзывание {:.1}¢)", signed(mean, 1, "%"), v21.slip * 100.0),
        "Проверка на 100 сделках: 2/100 · вин-рейт 50%".to_string(),
        "Быстрое ≠ точное:".to_string(),
        "02.10 03:45 BTC Up 851.2 с: быстрое — пропуск (номер монеты неизвестен), точное — вход (вход)".to_string(),
        "02.10 03:45 ETH Up 842.0 с: быстрое — вход (вход), точное — пропуск (ask > 0.88 через 0.3 с)".to_string(),
    ];
    assert_eq!(report(&j, &st, None, None), want.join("\n"));

    // a period is a range of window starts: the first window alone
    let one = report(&j, &st, Some(S), Some(S + 900));
    assert!(one.starts_with("Отчёт v21bot · демо · 02.10.2026 (UTC)\nСигналов базового правила: 1 · ордер $20 исполнился полностью в 100%"), "{one}");
    assert!(one.contains("сделок 1, выигрышей 1"), "{one}");
    assert!(one.contains("Проверка на 100 сделках: 2/100"), "the check counts every trade: {one}");
}

#[test]
fn an_empty_journal_reports_nothing_found() {
    let st = load_strategy(V2, V21).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    assert_eq!(report(&j, &st, None, None), "Отчёт v21bot · демо · нет записей");
}
