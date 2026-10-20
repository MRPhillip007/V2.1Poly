//! The demo mode end to end (spec §10): the bot against fake servers, on the exchange's clock at
//! s + 830 s of a window in the hours of v2.1. Each scenario takes about 100 s (the outcomes are asked
//! from the 930th second); they run in parallel, and only when asked:
//! `cargo test -p v21-live --test e2e -- --ignored`.

mod support;

use serde_json::Value;
use support::fake_market::Cut;
use support::scenario::{asks, of, Setup, Stand, BTC, ETH, S, S0, SOL};
use v21_live::shared::utc::iso_ms;

/// The asks of an Up token in the band, and out of it.
const IN: &[(f64, f64)] = &[(0.80, 100.0)];
const OUT: &[(f64, f64)] = &[(0.95, 100.0)];

/// (coin, t_trig) of the records of `kind`.
fn checks(recs: &[Value], kind: &str) -> Vec<(String, f64)> {
    of(recs, kind).iter().map(|r| (r["coin"].as_str().unwrap_or("").to_string(), r["t_trig_s"].as_f64().unwrap_or(f64::NAN))).collect()
}

/// The signal record of `coin` whose t_trig is in `t`.
fn signal<'a>(recs: &'a [Value], coin: &str, t: std::ops::Range<f64>) -> &'a Value {
    of(recs, "signal")
        .into_iter()
        .find(|r| r["coin"] == coin && t.contains(&r["t_trig_s"].as_f64().unwrap_or(0.0)))
        .unwrap_or_else(|| panic!("no signal of {coin} in {t:?}: {:?}", checks(recs, "signal")))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 100 s"]
async fn shot_at_840_and_a_mid_minute_signal() {
    let stand = Stand::start(Setup { script: vec![asks(BTC, 835_000, IN), asks(ETH, 842_300, IN)], ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("two settlements", |r| of(r, "settle").len() == 2).await;
    let shots = checks(&recs, "shot");
    assert_eq!(shots.len(), 2, "{shots:?}");
    assert_eq!(shots[0], ("btc".to_string(), 840.0));
    assert!(shots[1].0 == "eth" && (842.3..842.4).contains(&shots[1].1), "{shots:?}");
    for (coin, t) in [("btc", 840.0..840.01), ("eth", 842.3..842.4)] {
        let s = signal(&recs, coin, t);
        assert_eq!((&s["decision"], &s["fast_decision"], &s["shot"]), (&Value::from("вход"), &Value::from("вход"), &Value::from(true)), "{s}");
        assert!(s["demo"]["shares"].as_f64().unwrap() > 24.0, "{s}");
    }
    assert_eq!(of(&recs, "demo").len(), 2);
    assert_eq!(of(&recs, "entry").len(), 2);
    let (header, rows) = stand.table("trades.csv");
    let status = header.iter().position(|h| h == "status").unwrap();
    assert_eq!(rows.iter().map(|r| r[status].as_str()).collect::<Vec<_>>(), ["win", "win"]);
    let texts: Vec<String> = stand.tg.sent().iter().filter_map(|m| m["text"].as_str().map(str::to_string)).collect();
    assert!(texts.iter().any(|t| t.starts_with("🟢 BTC Up · окно 03:15–03:30 · демо: $20.00 по 0.800")), "{texts:?}");
    assert!(texts.iter().any(|t| t.starts_with("🟢 ETH Up")), "{texts:?}");
    assert_eq!(texts.iter().filter(|t| t.starts_with("✅ ")).count(), 2, "{texts:?}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 100 s"]
async fn failed_check_then_a_second_shot() {
    let script = vec![asks(BTC, 841_000, IN), asks(BTC, 841_200, &[(0.90, 100.0)]), asks(BTC, 843_000, IN)];
    let stand = Stand::start(Setup { script, ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("an entry", |r| !of(r, "entry").is_empty()).await;
    let demos = of(&recs, "demo");
    assert_eq!(demos.len(), 2, "{demos:?}");
    assert_eq!(demos[0]["shares"].as_f64(), Some(0.0), "the first fill bought nothing");
    assert!(demos[1]["shares"].as_f64().unwrap() > 24.0);
    let entry = of(&recs, "entry")[0];
    assert!((843.0..843.1).contains(&entry["t_trig_s"].as_f64().unwrap()), "{entry}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn third_coin_and_waiting_for_the_rank() {
    let script = vec![asks(SOL, 835_000, IN), asks(BTC, 840_100, IN), asks(ETH, 840_200, IN)];
    let stand = Stand::start(Setup { script, ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("the decisions on all three", |r| of(r, "signal").iter().filter(|s| s["counted"] == true).count() == 3).await;
    let shots = of(&recs, "shot");
    assert_eq!(shots.len(), 1);
    assert_eq!((&shots[0]["coin"], &shots[0]["rank"]), (&Value::from("btc"), &Value::from(1)));
    let eth = signal(&recs, "eth", 840.2..840.3);
    assert_eq!((&eth["decision"], &eth["reason"], &eth["rank"]), (&Value::from("пропуск"), &Value::from("третья монета"), &Value::from(3)));
    assert_eq!((&eth["fast_decision"], &eth["fast_reason"]), (&Value::from("пропуск"), &Value::from("третья монета")));
    assert!(eth["fast_wait_ms"].as_i64().is_some_and(|w| w > 0), "{eth}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn one_copy_drops_mid_minute() {
    let cut = Cut { at_ms: 845_000, until_ms: 845_000, all: false, code: 1013 };
    let stand = Stand::start(Setup { script: vec![asks(BTC, 846_000, IN)], cuts: vec![cut], ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("the window's record", |r| !of(r, "window").is_empty()).await;
    let shots = checks(&recs, "shot");
    assert!(shots.len() == 1 && (846.0..846.1).contains(&shots[0].1), "{shots:?}");
    assert_eq!(of(&recs, "window")[0]["ws_ok_840_870"], Value::from(true), "{}", of(&recs, "window")[0]);
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn both_copies_drop_for_11_s() {
    let cut = Cut { at_ms: 845_000, until_ms: 856_000, all: true, code: 1001 };
    let stand = Stand::start(Setup { script: vec![asks(BTC, 850_000, IN)], cuts: vec![cut], ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("the window's record", |r| !of(r, "window").is_empty()).await;
    for (_, t) in checks(&recs, "shot") {
        assert!(t >= 856.0, "no shot before a fresh snapshot: {t}");
    }
    let w = of(&recs, "window")[0];
    assert_eq!(w["ws_ok_840_870"], Value::from(false), "{w}");
    assert!(w["coverage_lost_ms"].as_i64().is_some_and(|ms| ms >= 11_000), "{w}");
    let alarms: Vec<&str> = of(&recs, "alarm").iter().filter_map(|a| a["text"].as_str()).collect();
    assert!(alarms.iter().any(|a| a.contains("дольше 10 с")), "{alarms:?}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn partial_fill_and_a_missed_trade() {
    // BTC: $12 under 0.88; ETH: in the band at 840 s and at its price moment, out of it around the match
    let script = vec![
        asks(BTC, 835_000, &[(0.80, 15.0), (0.90, 100.0)]),
        asks(ETH, 835_000, &[(0.85, 100.0)]),
        asks(ETH, 840_200, &[(0.90, 100.0)]),
        asks(ETH, 840_280, &[(0.85, 100.0)]),
    ];
    let stand = Stand::start(Setup { script, ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("both signals", |r| of(r, "signal").iter().filter(|s| s["counted"] == true).count() == 2).await;
    let entries = of(&recs, "entry");
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!((&entries[0]["coin"], entries[0]["shares"].as_f64()), (&Value::from("btc"), Some(15.0)));
    assert!((entries[0]["unfilled_usd"].as_f64().unwrap() - 8.0).abs() < 1e-9);
    let eth = signal(&recs, "eth", 840.0..840.01);
    assert_eq!((&eth["decision"], eth["demo"]["shares"].as_f64()), (&Value::from("вход"), Some(0.0)), "{eth}");
    let texts: Vec<String> = stand.tg.sent().iter().filter_map(|m| m["text"].as_str().map(str::to_string)).collect();
    assert!(texts.iter().any(|t| t == "⚪ ETH Up · окно 03:15–03:30 · сигнал был, демо-исполнения нет"), "{texts:?}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn money_below_the_stake_stops_the_demo_account() {
    let stand = Stand::start(Setup { script: vec![asks(BTC, 835_000, IN)], bank_usd: 20.40, ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("the window's record", |r| !of(r, "window").is_empty()).await;
    assert!(of(&recs, "shot").is_empty());
    let btc = signal(&recs, "btc", 840.0..840.01);
    assert_eq!((&btc["fast_decision"], &btc["fast_reason"]), (&Value::from("пропуск"), &Value::from("стоп демо-счёта")), "{btc}");
    let alarms: Vec<&str> = of(&recs, "alarm").iter().filter_map(|a| a["text"].as_str()).collect();
    assert!(alarms.iter().any(|a| a.contains("стоп демо-счёта")), "{alarms:?}");
    let texts: Vec<String> = stand.tg.sent().iter().filter_map(|m| m["text"].as_str().map(str::to_string)).collect();
    assert!(texts.iter().any(|t| t.contains("стоп демо-счёта")), "{texts:?}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 60 s"]
async fn binance_primary_451_uses_the_fallback() {
    let stand = Stand::start(Setup { script: vec![asks(BTC, 835_000, IN)], primary_451: true, ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("the window's record", |r| !of(r, "window").is_empty()).await;
    assert_eq!(checks(&recs, "shot"), [("btc".to_string(), 840.0)]);
    let btc = signal(&recs, "btc", 840.0..840.01);
    assert!(btc["z"].as_f64().is_some_and(|z| z > 1.25), "{btc}");
    assert_eq!(of(&recs, "window")[0]["binance_fallback"], Value::from(true));
    assert!(stand.primary.asked().iter().all(|a| !a.is_empty()), "the primary was asked first");
    bot.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 100 s"]
async fn restart_mid_window_restores_from_the_journal() {
    let script = vec![asks(BTC, 835_000, IN), asks(BTC, 845_000, OUT), asks(BTC, 860_000, IN), asks(ETH, 862_000, IN)];
    let stand = Stand::start(Setup { script, ..Setup::default() }).await;
    let first = stand.bot();
    stand.wait_for("the first entry", |r| !of(r, "entry").is_empty()).await;
    stand.until(850_000).await;
    first.stop().unwrap();
    let second = stand.bot();
    let recs = stand.wait_for("two settlements", |r| of(r, "settle").len() == 2).await;
    let shots = checks(&recs, "shot");
    assert_eq!(shots.iter().filter(|(c, _)| c == "btc").count(), 1, "{shots:?}");
    assert!(shots.iter().any(|(c, t)| c == "eth" && (862.0..862.1).contains(t)), "{shots:?}");
    assert_eq!(of(&recs, "entry").len(), 2);
    let btc = of(&recs, "signal").into_iter().find(|r| r["coin"] == "btc" && r["t_trig_s"].as_f64().is_some_and(|t| t >= 860.0)).expect("the BTC check of 860 s");
    assert_eq!(btc["fast_reason"], Value::from("монета уже в сделке или 2 выстрела в окне"), "{btc}");
    assert!(of(&recs, "start").len() == 2 && S > 0);
    second.stop().unwrap();
    stand.assert_no_errors();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "end to end: about 100 s"]
async fn the_exact_decision_waits_for_the_seconds_a_drop_of_binance_lost() {
    // Binance drops at 838.5 s and is back from 846 s; BTC's check starts at 840 s and its z needs the
    // close of 839 s. As in paper, the drop's seconds are a gap until the first trade after it, and only
    // then are their klines asked: none at the drop, whose second is still open
    let stand = Stand::start(Setup { script: vec![asks(BTC, 835_000, IN)], binance_cut: Some((838_500, 846_000)), ..Setup::default() }).await;
    let bot = stand.bot();
    let recs = stand.wait_for("BTC's signal", |r| of(r, "signal").iter().any(|s| s["coin"] == "btc")).await;
    let s = signal(&recs, "btc", 840.0..840.01);
    let back = iso_ms(S0 + 846_000 - stand.shift_ms);
    assert!(s["ts_utc"].as_str().unwrap() >= back.as_str(), "decided before Binance was back ({back}): {s}");
    assert_eq!(s["decision"], "вход", "{s}");
    // in the minute, one backfill: from the drop's second to the one before the first trade back
    let spans: Vec<Vec<i64>> = stand
        .primary
        .asked()
        .iter()
        .filter(|a| a.starts_with("klines BTCUSDT"))
        .map(|a| a.split(' ').skip(2).map(|x| x.parse().unwrap()).collect::<Vec<i64>>())
        .filter(|span| span[0] >= S0 + 835_000)
        .collect();
    assert!(matches!(spans[..], [ref span] if span[0] == S0 + 838_000 && span[1] >= S0 + 845_999), "{spans:?}");
    bot.stop().unwrap();
    stand.assert_no_errors();
}
