//! The records of the journal (spec §8; the names of `paper/journal.py` where it has them), built from
//! what the engines and the demo account give, and the day's summary built from the journal.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use v21_core::decision::{Decision, Reason, ENTER, SKIP};
use v21_core::money::DemoPosition;
use v21_core::params::{Params, Strategy};
use v21_core::window::{Coin, Side};

use crate::cold::exact::{ExactSignal, ShotFill, WindowSummary, GRID_MS};
use crate::cold::gamma::Outcome;
use crate::cold::telegram::DaySummary;
use crate::shared::msgs::{FastOut, ShotRecord};
use crate::shared::utc::fmt_utc;

/// A delivery lag of the feed worth a word in the window's note.
const LAG_NOTE_MS: i64 = 2_000;
/// A demo order that left less than this unfilled filled the whole stake.
const FULL_FILL_LEFT_USD: f64 = 0.005;
/// How near to 1 a payout per contract is a win.
const PAYOUT_TOL: f64 = 1e-9;

/// The fast decision of a check as the cold runtime got it, and how long it waited for its number
/// or for the answer to the coin's last shot.
#[derive(Clone, Copy, Debug)]
pub struct FastSeen {
    pub out: FastOut,
    pub wait_ms: Option<i64>,
}

/// What a window's record says beyond the exact engine's summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowNotes {
    /// Watched coins without a market.
    pub missing: Vec<Coin>,
    /// Coins whose market's rules differ, and how.
    pub rules: Vec<(Coin, String)>,
    pub binance_fallback: bool,
}

fn side(s: Side) -> &'static str {
    match s {
        Side::Up => "Up",
        Side::Down => "Down",
    }
}

fn decision(d: &Decision, p: &Params) -> (&'static str, String) {
    (if d.trade { ENTER } else { SKIP }, d.reason.label(p))
}

/// The record of an exact signal with the fast decision paired to it.
pub fn signal_record(sig: &ExactSignal, fast: Option<&FastSeen>, strategy: &Strategy) -> Value {
    let c = &sig.check;
    let (dec, reason) = decision(&sig.v21, &strategy.v21);
    let (v2_dec, v2_reason) = decision(&sig.v2, &strategy.v2);
    let (fast_dec, fast_reason, fast_rank, fast_z) = match fast.map(|f| f.out) {
        Some(FastOut::Shot(s)) => (Some(ENTER), Some(Reason::Enter.label(&strategy.v21)), Some(s.rank), s.z.map(|z| z.z)),
        Some(FastOut::Skip(k)) => (Some(SKIP), Some(k.reason.label(&strategy.v21)), k.rank, k.z.map(|z| z.z)),
        Some(FastOut::Wait { .. }) | None => (None, None, None, None),
    };
    let demo = sig.demo.as_ref();
    let grid = demo.and_then(|d| d.by_delay.as_ref()).map(|g| {
        g.iter().zip(GRID_MS).map(|(f, off)| json!([off, f.shares, f.notional_usd, f.avg_price])).collect::<Vec<_>>()
    });
    json!({
        "s": sig.s,
        "coin": c.coin.as_str(),
        "side": side(c.side),
        "t_trig_s": c.t_trig,
        "ask_at_trig": c.ask_at_trig,
        "px": c.px,
        "counted": sig.v21.counted,
        "rank": sig.rank,
        "z": sig.z.map(|z| z.z),
        "cushion_bp": sig.z.map(|z| z.cushion_bp),
        "sigma_bp": sig.z.map(|z| z.sigma_bp),
        "h_s": sig.z.map(|z| z.h),
        "p0_binance": sig.z.map(|z| z.p0),
        "ef_binance": sig.z.map(|z| z.ef),
        "hour_ok": sig.v21.hour_ok,
        "decision": dec,
        "reason": reason,
        "v2_decision": v2_dec,
        "v2_reason": v2_reason,
        "fast_decision": fast_dec,
        "fast_reason": fast_reason,
        "fast_rank": fast_rank,
        "fast_z": fast_z,
        "fast_wait_ms": fast.and_then(|f| f.wait_ms),
        "lag_ms": sig.lag_ms,
        "send_ms": sig.send_ms,
        "shot": sig.shot,
        "demo": demo.map(|d| serde_json::to_value(d.at_match).unwrap_or(Value::Null)),
        "demo_grid": grid,
        "price_no_latency": demo.map(|d| d.no_latency.avg_price),
        "depth_usd_trig": demo.map(|d| d.depth_no_latency_usd),
        "depth_usd_match": demo.map(|d| d.depth_at_match_usd),
    })
}

pub fn shot_record(shot: &ShotRecord) -> Value {
    json!({
        "s": shot.s,
        "coin": shot.coin.as_str(),
        "side": side(shot.side),
        "t_trig_s": shot.t_trig,
        "event_ts_ms": shot.event_ts_ms,
        "send_ms": shot.send_ms,
        "rank": shot.rank,
        "z": shot.z.map(|z| z.z),
        "frame_to_decision_ns": shot.frame_to_decision_ns,
    })
}

/// The demo fill of a shot.
pub fn demo_record(f: &ShotFill) -> Value {
    let (shot, fill) = (&f.shot, &f.demo.at_match);
    json!({
        "s": shot.s,
        "coin": shot.coin.as_str(),
        "side": side(shot.side),
        "t_trig_s": shot.t_trig,
        "match_ms": f.demo.match_ms,
        "shares": fill.shares,
        "notional_usd": fill.notional_usd,
        "fee_usd": fill.fee_usd,
        "avg_price": fill.avg_price,
        "levels_used": fill.levels_used,
        "unfilled_usd": fill.unfilled_usd,
        "price_no_latency": f.demo.no_latency.avg_price,
    })
}

/// A trade of the demo account.
pub fn entry_record(pos: &DemoPosition, f: &ShotFill, stake_usd: f64, balance: f64) -> Value {
    let fill = &f.demo.at_match;
    json!({
        "trade_id": pos.id,
        "s": pos.s,
        "coin": pos.coin.as_str(),
        "side": side(pos.side),
        "t_trig_s": f.shot.t_trig,
        "rank": f.shot.rank,
        "z": f.shot.z.map(|z| z.z),
        "stake_usd": stake_usd,
        "cost_usd": pos.cost_usd,
        "fee_usd": pos.fee_usd,
        "shares": pos.shares,
        "avg_price": pos.avg_price,
        "levels_used": fill.levels_used,
        "unfilled_usd": fill.unfilled_usd,
        "price_no_latency": f.demo.no_latency.avg_price,
        "balance_after": balance,
    })
}

/// The window's record: the exact engine's summary, the markets and the trades.
pub fn window_record(w: &WindowSummary, notes: &WindowNotes, n_trades: u32) -> Value {
    let mut why = vec![];
    if !notes.missing.is_empty() {
        why.push(format!("нет рынка: {}", notes.missing.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ")));
    }
    why.extend(notes.rules.iter().map(|(c, n)| format!("{}: {n}", c.as_str())));
    if !w.ws_ok {
        why.push("пропуски данных Polymarket в решающую минуту".to_string());
    }
    if !w.binance_ok {
        why.push("пропуски данных Binance".to_string());
    }
    if w.lag_max_ms > LAG_NOTE_MS {
        why.push(format!("задержка данных Polymarket до {:.1} с", w.lag_max_ms as f64 / 1000.0));
    }
    let mut r = json!({
        "s": w.s,
        "markets_ok": notes.missing.is_empty(),
        "rules_ok": notes.rules.is_empty(),
        "ws_ok_840_870": w.ws_ok,
        "binance_ok": w.binance_ok,
        "n_signals": w.n_signals,
        "n_trades": n_trades,
        "note": why.join("; "),
        "feed_lag_max_ms": w.lag_max_ms,
        "late_dropped": w.late_dropped,
        "book_reloads": w.book_reloads,
        "coverage_lost_ms": w.coverage_lost_ms,
        "binance_fallback": notes.binance_fallback,
    });
    if let Value::Object(m) = &mut r {
        for (coin, p0) in &w.p0 {
            m.insert(format!("p0_binance_{}", coin.as_str()), json!(p0));
        }
    }
    r
}

/// The rows of each coin's Up token in 835–871 s: a `bbo` record per coin.
pub fn bbo_records(w: &WindowSummary) -> Vec<Value> {
    w.bbo
        .iter()
        .map(|(coin, rows)| json!({"s": w.s, "coin": coin.as_str(), "rows": rows.iter().map(|&(ts, bid, ask)| json!([ts, bid, ask])).collect::<Vec<_>>()}))
        .collect()
}

/// `Up`, `Down` or `50/50`.
fn outcome_label(payout_up: f64) -> &'static str {
    if payout_up >= 1.0 {
        "Up"
    } else if payout_up <= 0.0 {
        "Down"
    } else {
        "50/50"
    }
}

/// The settlement of a trade.
pub fn settle_record(trade_id: &str, o: &Outcome, payout: f64, pnl: f64, cost: f64, balance: f64, settled_utc: &str) -> Value {
    json!({
        "trade_id": trade_id,
        "outcome": outcome_label(o.payout_up),
        "payout_usd": payout,
        "pnl_usd": pnl,
        "roi": pnl / cost,
        "balance_after": balance,
        "official_ptb": o.ptb,
        "official_final": o.final_price,
        "settled_utc": settled_utc,
    })
}

/// The outcomes of the coins of a window that came now.
pub fn window_result_record(s: i64, outcomes: &[(Coin, Outcome)]) -> Value {
    let mut m = Map::new();
    m.insert("s".into(), json!(s));
    for (coin, o) in outcomes {
        let c = coin.as_str();
        m.insert(format!("outcome_{c}"), json!(outcome_label(o.payout_up)));
        m.insert(format!("official_ptb_{c}"), json!(o.ptb));
        m.insert(format!("official_final_{c}"), json!(o.final_price));
    }
    Value::Object(m)
}

/// The records of `kind`.
fn of<'a>(recs: &'a [Value], kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    recs.iter().filter(move |r| r["kind"] == kind)
}

/// The summary of the UTC day `date` (`YYYY-MM-DD`) from the journal's records: the trades of its
/// windows that are settled, the counted signals of its windows with their demo orders, and the
/// alarms raised that day by the exchange's clock.
pub fn day_summary(recs: &[Value], date: &str, balance: f64, stake_usd: f64) -> DaySummary {
    let day_of_ms = |ms: i64| fmt_utc(ms.div_euclid(1000))[..10].to_string();
    let on_day = |r: &Value| r["s"].as_i64().is_some_and(|s| fmt_utc(s)[..10] == *date);
    let settles: HashMap<&str, &Value> = of(recs, "settle").filter_map(|r| Some((r["trade_id"].as_str()?, r))).collect();
    let (mut n, mut wins, mut pnl) = (0, 0, 0.0);
    for e in of(recs, "entry").filter(|e| on_day(e)) {
        let Some(st) = e["trade_id"].as_str().and_then(|id| settles.get(id)) else { continue };
        n += 1;
        pnl += st["pnl_usd"].as_f64().unwrap_or(0.0);
        let per_share = st["payout_usd"].as_f64().unwrap_or(0.0) / e["shares"].as_f64().unwrap_or(f64::NAN);
        wins += u32::from(per_share >= 1.0 - PAYOUT_TOL);
    }
    let signals: Vec<&Value> = of(recs, "signal").filter(|r| on_day(r)).collect();
    let counted: Vec<&Value> = signals.iter().copied().filter(|r| r["counted"] == true).collect();
    let bought = |r: &Value| r["demo"]["shares"].as_f64().is_some_and(|s| s > 0.0);
    let full = counted.iter().filter(|r| bought(r) && r["demo"]["unfilled_usd"].as_f64().is_some_and(|u| u <= FULL_FILL_LEFT_USD)).count();
    let filled: f64 = counted.iter().map(|r| r["demo"]["notional_usd"].as_f64().unwrap_or(0.0)).sum();
    let slips: Vec<f64> = counted
        .iter()
        .filter(|r| bought(r))
        .filter_map(|r| Some((r["demo"]["avg_price"].as_f64()? - r["px"].as_f64()?) * 100.0))
        .collect();
    let share = |k: usize| if counted.is_empty() { 0.0 } else { k as f64 * 100.0 / counted.len() as f64 };
    DaySummary {
        date: date.to_string(),
        n,
        wins,
        pnl,
        balance,
        signals: counted.len() as u32,
        stake_usd,
        full_fill_pct: share(full),
        mean_filled_usd: if counted.is_empty() { 0.0 } else { filled / counted.len() as f64 },
        price_vs_backtest_c: (!slips.is_empty()).then(|| slips.iter().sum::<f64>() / slips.len() as f64),
        missed: counted.iter().filter(|r| r["decision"] == ENTER && !bought(r)).count() as u32,
        fast_ne_exact: signals.iter().filter(|r| !r["fast_decision"].is_null() && r["fast_decision"] != r["decision"]).count() as u32,
        alarms: of(recs, "alarm")
            .filter(|r| r["at_ms"].as_i64().map(day_of_ms).as_deref() == Some(date))
            .filter_map(|r| r["text"].as_str().map(str::to_string))
            .collect(),
    }
}
