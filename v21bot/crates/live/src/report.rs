//! `v21 report` (spec §8): the demo against the backtest over a period of the journal — the demo orders
//! of the base rule's signals, the prices against the backtest's, the missed trades, the fast decisions
//! that differ from the exact ones, the demo account, the backtest equivalent of v2.1 and the progress
//! to the check on 100 trades.

use std::collections::HashMap;

use serde_json::Value;
use v21_core::decision::ENTER;
use v21_core::params::Strategy;
use v21_core::pyjson::py_float_repr;
use v21_core::returns::{trade_return, win_of};
use v21_core::window::{Coin, Side};

use crate::cold::journal::Journal;
use crate::cold::telegram::{dollars, money, name};
use crate::shared::utc::fmt_utc;

/// A demo order that left less than this unfilled filled the whole stake.
const FULL_FILL_LEFT_USD: f64 = 0.005;
const PAYOUT_TOL: f64 = 1e-9;
const MINUS: char = '\u{2212}';
/// Until a `start` record says otherwise.
const STAKE_USD: f64 = 20.0;
const TAKER_DELAY_MS: i64 = 250;

/// `+1.4¢`, `−0.2%`.
fn signed(x: f64, unit: &str) -> String {
    let sign = if x < 0.0 { MINUS } else { '+' };
    format!("{sign}{:.1}{unit}", x.abs())
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// `15.10–19.10.2026`, one date alone, or both years when they differ.
fn period(first: i64, last: i64) -> String {
    let (a, b) = (fmt_utc(first), fmt_utc(last));
    let dm = |t: &str| format!("{}.{}", &t[8..10], &t[5..7]);
    if a[..10] == b[..10] {
        format!("{}.{}", dm(&a), &a[..4])
    } else if a[..4] == b[..4] {
        format!("{}–{}.{}", dm(&a), dm(&b), &b[..4])
    } else {
        format!("{}.{}–{}.{}", dm(&a), &a[..4], dm(&b), &b[..4])
    }
}

/// The report of the windows whose start is in `from..to` (unix seconds; open ends when `None`).
pub fn report(journal: &Journal, strategy: &Strategy, from: Option<i64>, to: Option<i64>) -> String {
    let recs = match journal.all() {
        Ok(r) => r,
        Err(e) => return format!("Отчёт v21bot · журнал не читается: {e:#}"),
    };
    let in_period = |r: &Value| r["s"].as_i64().is_some_and(|s| from.is_none_or(|f| s >= f) && to.is_none_or(|t| s < t));
    let of = |kind: &'static str| recs.iter().filter(move |r| r["kind"] == kind);
    let signals: Vec<&Value> = of("signal").filter(|r| in_period(r)).collect();
    let entries: Vec<&Value> = of("entry").filter(|r| in_period(r)).collect();
    let starts: Vec<i64> = signals.iter().chain(&entries).chain(of("window").filter(|r| in_period(r)).collect::<Vec<_>>().iter()).filter_map(|r| r["s"].as_i64()).collect();
    let (Some(&first), Some(&last)) = (starts.iter().min(), starts.iter().max()) else { return "Отчёт v21bot · демо · нет записей".to_string() };
    let start = of("start").next_back();
    let stake = start.and_then(|r| r["stake_usd"].as_f64()).unwrap_or(STAKE_USD);
    let taker = start.and_then(|r| r["taker_delay_ms"].as_i64()).unwrap_or(TAKER_DELAY_MS);
    let mut lines = vec![format!("Отчёт v21bot · демо · {} (UTC)", period(first, last))];

    // the demo orders of the base rule's counted signals
    let counted: Vec<&Value> = signals.iter().copied().filter(|r| r["counted"] == true).collect();
    let bought = |r: &Value| r["demo"]["shares"].as_f64().is_some_and(|s| s > 0.0);
    if counted.is_empty() {
        lines.push("Сигналов базового правила: 0".to_string());
    } else {
        let full = counted.iter().filter(|r| bought(r) && r["demo"]["unfilled_usd"].as_f64().is_some_and(|u| u <= FULL_FILL_LEFT_USD)).count();
        let filled: f64 = counted.iter().map(|r| r["demo"]["notional_usd"].as_f64().unwrap_or(0.0)).sum();
        lines.push(format!(
            "Сигналов базового правила: {} · ордер {} исполнился полностью в {:.0}% · в среднем {} из {}",
            counted.len(),
            dollars(stake),
            full as f64 * 100.0 / counted.len() as f64,
            money(filled / counted.len() as f64, false),
            dollars(stake)
        ));
    }
    let gap = |price: &dyn Fn(&Value) -> Option<f64>| {
        let v: Vec<f64> = counted.iter().filter_map(|r| Some((price(r)? - r["px"].as_f64()?) * 100.0)).collect();
        mean(&v).map_or("—".to_string(), |m| signed(m, "¢"))
    };
    let demo_gap = gap(&|r: &Value| if bought(r) { r["demo"]["avg_price"].as_f64() } else { None });
    let no_latency_gap = gap(&|r: &Value| r["price_no_latency"].as_f64());
    lines.push(format!("Цена демо − цена бэктеста: {demo_gap} · без задержки − бэктест: {no_latency_gap}"));
    let differ: Vec<&Value> = signals.iter().copied().filter(|r| r["fast_decision"].is_string() && r["fast_decision"] != r["decision"]).collect();
    let missed = counted.iter().filter(|r| r["decision"] == ENTER && !bought(r)).count();
    lines.push(format!("Упущенные сделки: {missed} · быстрое ≠ точное: {}", differ.len()));
    lines.push(format!("Задержка тейкера в демо: {taker} мс (по документации; живых ордеров ещё нет)"));

    // the demo account
    let settles: HashMap<&str, &Value> = of("settle").filter_map(|r| Some((r["trade_id"].as_str()?, r))).collect();
    let tally = |entries: &[&Value]| {
        let (mut n, mut wins, mut pnl) = (0u32, 0u32, 0.0);
        for e in entries {
            let Some(st) = e["trade_id"].as_str().and_then(|id| settles.get(id)) else { continue };
            n += 1;
            pnl += st["pnl_usd"].as_f64().unwrap_or(0.0);
            let per_share = st["payout_usd"].as_f64().unwrap_or(0.0) / e["shares"].as_f64().unwrap_or(f64::NAN);
            wins += u32::from(per_share >= 1.0 - PAYOUT_TOL);
        }
        (n, wins, pnl)
    };
    let (n, wins, pnl) = tally(&entries);
    let balance = recs.iter().filter(|r| r["kind"] == "entry" || r["kind"] == "settle").filter_map(|r| r["balance_after"].as_f64()).next_back();
    let balance = balance.or_else(|| start.and_then(|r| r["demo_bank_usd"].as_f64())).map_or("—".to_string(), |b| money(b, false));
    lines.push(format!("P&L демо-счёта: {} · баланс {balance} · сделок {n}, выигрышей {wins}", money(pnl, true)));

    // the exact decisions of v2.1 to enter whose outcome is known
    let mut outcome: HashMap<(i64, String), f64> = HashMap::new();
    for r in of("window_result") {
        let Some(s) = r["s"].as_i64() else { continue };
        for coin in [Coin::Btc, Coin::Eth, Coin::Sol] {
            let p_up = match r[format!("outcome_{}", coin.as_str())].as_str() {
                Some("Up") => 1.0,
                Some("Down") => 0.0,
                _ => continue,
            };
            outcome.insert((s, coin.as_str().to_string()), p_up);
        }
    }
    let v21 = &strategy.v21;
    let returns: Vec<f64> = signals
        .iter()
        .filter(|r| r["decision"] == ENTER)
        .filter_map(|r| {
            let p_up = outcome.get(&(r["s"].as_i64()?, r["coin"].as_str()?.to_string()))?;
            let side = if r["side"] == "Down" { Side::Down } else { Side::Up };
            Some(trade_return(r["px"].as_f64()?, win_of(side, *p_up), v21.slip, v21.fee))
        })
        .collect();
    lines.push(match mean(&returns) {
        Some(m) => format!("Бэктест-эквивалент v2.1: сделок {} · доход на сделку {} (проскальзывание {:.1}¢)", returns.len(), signed(m * 100.0, "%"), v21.slip * 100.0),
        None => "Бэктест-эквивалент v2.1: сделок 0".to_string(),
    });

    // every settled trade counts towards the check of the forward rule
    let all: Vec<&Value> = of("entry").collect();
    let (n_all, wins_all, _) = tally(&all);
    let rate = if n_all == 0 { "—".to_string() } else { format!("{:.0}%", f64::from(wins_all) * 100.0 / f64::from(n_all)) };
    lines.push(format!("Проверка на {0} сделках: {n_all}/{0} · вин-рейт {rate}", strategy.forward.stop_after_trades));

    if !differ.is_empty() {
        lines.push("Быстрое ≠ точное:".to_string());
        for r in differ {
            let when = r["s"].as_i64().map(fmt_utc).unwrap_or_default();
            let coin = r["coin"].as_str().and_then(Coin::parse);
            let side = if r["side"] == "Down" { Side::Down } else { Side::Up };
            let what = coin.map_or_else(|| "?".to_string(), |c| name(c, side));
            let t = r["t_trig_s"].as_f64().map(py_float_repr).unwrap_or_default();
            let text = |k: &str| r[k].as_str().unwrap_or("").to_string();
            lines.push(format!(
                "{}.{} {} {what} {t} с: быстрое — {} ({}), точное — {} ({})",
                when.get(8..10).unwrap_or(""),
                when.get(5..7).unwrap_or(""),
                when.get(11..16).unwrap_or(""),
                text("fast_decision"),
                text("fast_reason"),
                text("decision"),
                text("reason")
            ));
        }
    }
    lines.join("\n")
}
