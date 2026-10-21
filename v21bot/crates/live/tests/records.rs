//! The records of the journal (spec §8, the table of task 14), built from what the engines give, and
//! the day's summary built from the journal.

use serde_json::{json, Value};
use v21_core::decision::{Reason, WindowEngine};
use v21_core::detector::Check;
use v21_core::fill::Fill;
use v21_core::money::DemoPosition;
use v21_core::params::load_strategy;
use v21_core::window::{Coin, Side};
use v21_core::zscore::ZInfo;
use v21_live::cold::exact::{DemoFill, ExactSignal, ShotFill, WindowSummary, GRID_MS};
use v21_live::cold::gamma::Outcome;
use v21_live::cold::journal::Journal;
use v21_live::cold::records::{
    bbo_records, day_summary, demo_record, entry_record, settle_record, shot_record, signal_record, window_record, window_result_record, FastSeen, WindowNotes,
};
use v21_live::shared::msgs::{FastOut, FastReason, FastSkip, ShotRecord};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const S0: i64 = S * 1000;

fn z(z: f64) -> ZInfo {
    ZInfo { z, p0: 61_000.0, ef: 61_050.0, sigma_bp: 1.2, h: 19.5, cushion_bp: 8.2 }
}

fn fill(shares: f64, price: f64) -> Fill {
    Fill { shares, notional_usd: shares * price, fee_usd: 0.27, avg_price: price, levels_used: 1, unfilled_usd: 20.0 - shares * price }
}

fn demo(price: f64) -> DemoFill {
    DemoFill {
        match_ms: S0 + 840_250,
        at_match: fill(20.0 / price, price),
        no_latency: fill(25.0, 0.80),
        by_delay: Some(std::array::from_fn(|k| fill(20.0, 0.80 + k as f64 * 0.001))),
        depth_no_latency_usd: 122.5,
        depth_at_match_usd: 42.5,
    }
}

/// The exact signal of a successful BTC check at 840.0 s, decided by v2 and v2.1.
fn signal(px: f64, z_value: Option<f64>) -> ExactSignal {
    let st = load_strategy(V2, V21).unwrap();
    let check = Check { coin: Coin::Btc, t_trig: 840.0, side: Side::Up, ask_at_trig: 0.80, px, ok: px <= 0.88 };
    let zi = z_value.map(z);
    let v2 = WindowEngine::new(S).decide(&st.v2, &check, zi.as_ref());
    let v21 = WindowEngine::new(S).decide(&st.v21, &check, zi.as_ref());
    ExactSignal { s: S, check, rank: v21.rank, z: zi, v2, v21, lag_ms: 40, send_ms: S0 + 840_000, shot: true, demo: Some(demo(0.81)) }
}

fn shot() -> ShotRecord {
    ShotRecord { s: S, coin: Coin::Btc, side: Side::Up, t_trig: 840.0, event_ts_ms: S0 + 839_500, send_ms: S0 + 840_000, rank: 1, z: Some(z(1.69)), frame_to_decision_ns: 4_200 }
}

#[test]
fn a_signal_record_has_both_decisions_the_fast_one_and_the_demo() {
    let st = load_strategy(V2, V21).unwrap();
    let r = signal_record(&signal(0.85, Some(1.7)), Some(&FastSeen { out: FastOut::Shot(shot()), wait_ms: Some(120) }), &st);
    assert_eq!(
        [&r["s"], &r["coin"], &r["side"], &r["t_trig_s"], &r["ask_at_trig"], &r["px"], &r["counted"], &r["rank"], &r["z"], &r["hour_ok"]],
        [&json!(S), &json!("btc"), &json!("Up"), &json!(840.0), &json!(0.8), &json!(0.85), &json!(true), &json!(1), &json!(1.7), &json!(true)]
    );
    assert_eq!([&r["cushion_bp"], &r["sigma_bp"], &r["h_s"], &r["p0_binance"], &r["ef_binance"]], [&json!(8.2), &json!(1.2), &json!(19.5), &json!(61_000.0), &json!(61_050.0)]);
    assert_eq!([&r["decision"], &r["reason"], &r["v2_decision"], &r["v2_reason"]], [&json!("вход"); 4]);
    assert_eq!(
        [&r["fast_decision"], &r["fast_reason"], &r["fast_rank"], &r["fast_z"], &r["fast_wait_ms"]],
        [&json!("вход"), &json!("вход"), &json!(1), &json!(1.69), &json!(120)]
    );
    assert_eq!([&r["lag_ms"], &r["send_ms"], &r["shot"]], [&json!(40), &json!(S0 + 840_000), &json!(true)]);
    assert_eq!(r["demo"]["avg_price"], json!(0.81));
    assert_eq!(r["price_no_latency"], json!(0.8));
    // the dollars on the asks up to the limit when the band was hit and at the match
    assert_eq!([&r["depth_usd_trig"], &r["depth_usd_match"]], [&json!(122.5), &json!(42.5)]);
    let grid = r["demo_grid"].as_array().unwrap();
    assert_eq!(grid.len(), 13);
    let price = 0.80 + 12.0 * 0.001;
    assert_eq!(grid[12], json!([GRID_MS[12], 20.0, 20.0 * price, price]));
}

#[test]
fn a_skip_without_z_and_a_check_the_fast_engine_never_saw() {
    let st = load_strategy(V2, V21).unwrap();
    let skip = FastSkip { s: S, coin: Coin::Btc, side: Side::Up, t_trig: 840.0, rank: None, z: None, reason: FastReason::RankUnknown };
    let r = signal_record(&signal(0.85, None), Some(&FastSeen { out: FastOut::Skip(skip), wait_ms: None }), &st);
    assert_eq!([&r["decision"], &r["reason"]], [&json!("пропуск"), &json!(Reason::NoBinance.label(&st.v21))]);
    assert_eq!([&r["z"], &r["cushion_bp"]], [&Value::Null, &Value::Null]);
    assert_eq!([&r["fast_decision"], &r["fast_reason"], &r["fast_rank"], &r["fast_z"]], [&json!("пропуск"), &json!("номер монеты неизвестен"), &Value::Null, &Value::Null]);
    let r = signal_record(&signal(0.90, Some(1.7)), None, &st);
    assert_eq!([&r["counted"], &r["rank"], &r["fast_decision"]], [&json!(false), &Value::Null, &Value::Null]);
    assert_eq!(r["reason"], json!("ask > 0.88 через 0.3 с"));
}

#[test]
fn the_records_of_a_shot_its_fill_and_its_entry() {
    let r = shot_record(&shot());
    assert_eq!(
        [&r["s"], &r["coin"], &r["side"], &r["t_trig_s"], &r["event_ts_ms"], &r["send_ms"], &r["rank"], &r["z"], &r["frame_to_decision_ns"]],
        [&json!(S), &json!("btc"), &json!("Up"), &json!(840.0), &json!(S0 + 839_500), &json!(S0 + 840_000), &json!(1), &json!(1.69), &json!(4_200)]
    );
    let f = ShotFill { shot: shot(), demo: DemoFill { by_delay: None, ..demo(0.809) } };
    let r = demo_record(&f);
    assert_eq!([&r["match_ms"], &r["avg_price"], &r["levels_used"], &r["price_no_latency"]], [&json!(S0 + 840_250), &json!(0.809), &json!(1), &json!(0.8)]);
    let pos = DemoPosition { id: format!("{S}-btc"), s: S, coin: Coin::Btc, side: Side::Up, shares: 24.72, cost_usd: 20.27, fee_usd: 0.27, avg_price: 0.809 };
    let r = entry_record(&pos, &f, 20.0, 79.73);
    assert_eq!(
        [&r["trade_id"], &r["s"], &r["coin"], &r["side"], &r["t_trig_s"], &r["rank"], &r["z"], &r["stake_usd"], &r["cost_usd"], &r["shares"], &r["balance_after"]],
        [&json!(format!("{S}-btc")), &json!(S), &json!("btc"), &json!("Up"), &json!(840.0), &json!(1), &json!(1.69), &json!(20.0), &json!(20.27), &json!(24.72), &json!(79.73)]
    );
}

#[test]
fn a_window_record_names_what_was_wrong() {
    let w = WindowSummary {
        s: S,
        ws_ok: false,
        binance_ok: false,
        n_signals: 2,
        lag_max_ms: 2_500,
        late_dropped: 3,
        book_reloads: 1,
        coverage_lost_ms: 11_000,
        p0: vec![(Coin::Btc, Some(61_000.0)), (Coin::Eth, None), (Coin::Sol, Some(150.0))],
        bbo: vec![(Coin::Btc, vec![(S0 + 840_000, 0.18, 0.80)]), (Coin::Eth, vec![])],
    };
    let notes = WindowNotes { missing: vec![Coin::Sol], rules: vec![(Coin::Eth, "окно TWAP 30 с (ожидалось 60)".into())], binance_fallback: true };
    let r = window_record(&w, &notes, 1);
    assert_eq!(
        [&r["markets_ok"], &r["rules_ok"], &r["ws_ok_840_870"], &r["binance_ok"], &r["n_signals"], &r["n_trades"], &r["binance_fallback"]],
        [&json!(false), &json!(false), &json!(false), &json!(false), &json!(2), &json!(1), &json!(true)]
    );
    assert_eq!([&r["feed_lag_max_ms"], &r["late_dropped"], &r["book_reloads"], &r["coverage_lost_ms"]], [&json!(2_500), &json!(3), &json!(1), &json!(11_000)]);
    assert_eq!([&r["p0_binance_btc"], &r["p0_binance_eth"], &r["p0_binance_sol"]], [&json!(61_000.0), &Value::Null, &json!(150.0)]);
    assert_eq!(
        r["note"],
        json!("нет рынка: sol; eth: окно TWAP 30 с (ожидалось 60); пропуски данных Polymarket в решающую минуту; пропуски данных Binance; задержка данных Polymarket до 2.5 с")
    );
    let bbo = bbo_records(&w);
    assert_eq!(bbo, [json!({"s": S, "coin": "btc", "rows": [[S0 + 840_000, 0.18, 0.80]]}), json!({"s": S, "coin": "eth", "rows": []})]);
}

#[test]
fn the_records_of_the_outcomes() {
    let o = Outcome { payout_up: 1.0, ptb: Some(61_000.5), final_price: Some(61_020.0) };
    let r = settle_record(&format!("{S}-btc"), &o, 24.72, 4.45, 20.27, 104.45, "2026-10-02 03:31:05");
    assert_eq!(
        [&r["trade_id"], &r["outcome"], &r["payout_usd"], &r["pnl_usd"], &r["balance_after"], &r["official_ptb"], &r["official_final"], &r["settled_utc"]],
        [&json!(format!("{S}-btc")), &json!("Up"), &json!(24.72), &json!(4.45), &json!(104.45), &json!(61_000.5), &json!(61_020.0), &json!("2026-10-02 03:31:05")]
    );
    assert_eq!(r["roi"], json!(4.45 / 20.27));
    let half = Outcome { payout_up: 0.5, ptb: None, final_price: None };
    let r = window_result_record(S, &[(Coin::Btc, o), (Coin::Sol, half)]);
    assert_eq!(
        r,
        json!({"s": S, "outcome_btc": "Up", "official_ptb_btc": 61_000.5, "official_final_btc": 61_020.0, "outcome_sol": "50/50", "official_ptb_sol": null, "official_final_sol": null})
    );
}

#[test]
fn the_days_summary_comes_from_the_journal() {
    let st = load_strategy(V2, V21).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::open(&dir.path().join("journal.jsonl")).unwrap();
    let fast = |out| Some(FastSeen { out, wait_ms: None });
    // BTC traded and won; ETH: the exact rule entered, the demo bought nothing (missed), the fast one
    // skipped; a failed check does not count
    j.append("signal", signal_record(&signal(0.85, Some(1.7)), fast(FastOut::Shot(shot())).as_ref(), &st)).unwrap();
    let mut eth = signal(0.85, Some(1.7));
    eth.check.coin = Coin::Eth;
    eth.demo = Some(DemoFill { at_match: Fill { shares: 0.0, notional_usd: 0.0, fee_usd: 0.0, avg_price: f64::NAN, levels_used: 0, unfilled_usd: 20.0 }, ..demo(0.81) });
    let skip = FastSkip { s: S, coin: Coin::Eth, side: Side::Up, t_trig: 840.0, rank: Some(2), z: None, reason: FastReason::Rule(Reason::ThirdCoin) };
    j.append("signal", signal_record(&eth, fast(FastOut::Skip(skip)).as_ref(), &st)).unwrap();
    j.append("signal", signal_record(&signal(0.90, Some(1.7)), None, &st)).unwrap();
    let f = ShotFill { shot: shot(), demo: demo(0.81) };
    let pos = DemoPosition { id: format!("{S}-btc"), s: S, coin: Coin::Btc, side: Side::Up, shares: 20.0 / 0.81, cost_usd: 20.27, fee_usd: 0.27, avg_price: 0.81 };
    j.append("entry", entry_record(&pos, &f, 20.0, 79.73)).unwrap();
    let o = Outcome { payout_up: 1.0, ptb: None, final_price: None };
    j.append("settle", settle_record(&pos.id, &o, pos.shares, pos.shares - 20.27, 20.27, 79.73 + pos.shares, "2026-10-02 03:31:05")).unwrap();
    j.append("alarm", json!({"at_ms": S0 + 845_000, "text": "обе копии ленты недоступны 12 с"})).unwrap();
    j.append("alarm", json!({"at_ms": S0 + 86_400_000, "text": "завтрашняя"})).unwrap();
    // another day's records do not count
    j.append("signal", signal_record(&ExactSignal { s: S + 86_400, ..signal(0.85, Some(1.7)) }, None, &st)).unwrap();

    let d = day_summary(&j.all().unwrap(), "2026-10-02", 104.42, 20.0);
    assert_eq!((d.date.as_str(), d.n, d.wins, d.balance, d.signals, d.missed, d.fast_ne_exact), ("2026-10-02", 1, 1, 104.42, 2, 1, 1));
    assert!((d.pnl - (20.0 / 0.81 - 20.27)).abs() < 1e-9);
    assert_eq!(d.full_fill_pct, 50.0);
    assert!((d.mean_filled_usd - 10.0).abs() < 1e-9);
    assert!((d.price_vs_backtest_c.unwrap() - (0.81 - 0.85) * 100.0).abs() < 1e-9);
    assert_eq!(d.stake_usd, 20.0);
    assert_eq!(d.alarms, ["обе копии ленты недоступны 12 с"], "the alarms of the day, by the exchange's clock");
}
