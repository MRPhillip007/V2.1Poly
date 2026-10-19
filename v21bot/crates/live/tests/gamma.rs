//! Gamma and the CLOB: the markets of a window and their rules (spec §7.4), the outcomes, the market
//! settings, the book and the round trip. The fixtures are real answers of 2026-10-19.

mod support;

use std::time::Duration;

use serde_json::{json, Value};
use support::fake_clob::FakeClob;
use support::fake_gamma::FakeGamma;
use v21_core::fill::FeeSchedule;
use v21_core::window::Coin;
use v21_live::cold::clob_info::{fetch_book, fetch_market_info, measure_rtt, parse_market_info, rules_note, MarketInfo, Rtt};
use v21_live::cold::gamma::{fetch_outcomes, fetch_window, gamma_events_with, parse_market, parse_outcome, Outcome};
use v21_live::cold::http_client;

const S: i64 = 1_792_439_100;
const CONDITION: &str = "0x0820ab37b6e2339dceff44b8dde9e3b9425353daf456921e8d877880eb13b260";
const UP: &str = "36423724363119883493146801479183012308409299544668963750177597294218304062682";
const DOWN: &str = "73121245879845022079634998356265994621433682788640427759651753924432326649656";

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The event of a Gamma answer.
fn event(name: &str) -> Value {
    fixture(name)[0].clone()
}

/// The event of `name` as another coin's.
fn as_coin(mut e: Value, coin: &str) -> Value {
    let slug = e["slug"].as_str().unwrap().replacen("btc", coin, 1);
    e["slug"] = json!(slug);
    e
}

#[test]
fn a_real_15m_market_parses_with_its_rules() {
    let m = parse_market(&event("gamma_event.json")).unwrap();
    assert_eq!((m.coin, m.s, m.slug.as_str()), (Coin::Btc, S, "btc-updown-15m-1792439100"));
    assert_eq!(m.condition_id, CONDITION);
    assert_eq!((m.up.as_str(), m.down.as_str()), (UP, DOWN));
    assert_eq!(m.fee, FeeSchedule { rate: 0.07, exponent: 1.0 });
    assert!(m.rules_ok, "{}", m.note);
    assert_eq!(m.note, "");
}

#[test]
fn a_changed_rule_is_named_in_the_note() {
    for (part, key, value, word) in [
        ("feeSchedule", "exponent", json!(2), "exponent"),
        ("feeSchedule", "rate", json!(0.1), "0.1"),
        ("feeSchedule", "takerOnly", json!(false), "тейкер"),
        ("cryptoMarketConfig", "twapLookbackSeconds", json!(30), "TWAP"),
        ("cryptoMarketConfig", "twapEnabled", json!(false), "TWAP"),
    ] {
        let mut e = event("gamma_event.json");
        e["markets"][0][part][key] = value;
        let m = parse_market(&e).unwrap();
        assert!(!m.rules_ok && m.note.contains(word), "{part}.{key}: {}", m.note);
    }
    let mut e = event("gamma_event.json");
    e["markets"][0].as_object_mut().unwrap().remove("cryptoMarketConfig");
    let m = parse_market(&e).unwrap();
    assert!(!m.rules_ok && m.note.contains("cryptoMarketConfig"), "{}", m.note);
    // the CLOB's settings count too
    let m = parse_market(&event("gamma_event.json")).unwrap();
    let mut info = parse_market_info(&fixture("clob_market.json")).unwrap();
    assert_eq!(rules_note(&m, Some(&info)), None);
    info.taker_delay = false;
    assert!(rules_note(&m, Some(&info)).unwrap().contains("задержка тейкера"));
    info.taker_delay = true;
    info.fee.exponent = 2.0;
    assert!(rules_note(&m, Some(&info)).unwrap().contains("CLOB"));
    assert!(rules_note(&m, None).unwrap().contains("нет настроек рынка"));
}

#[test]
fn a_market_that_cannot_be_read_is_an_error() {
    let mut e = event("gamma_event.json");
    e["markets"][0]["outcomes"] = json!("[\"Yes\", \"No\"]");
    assert!(parse_market(&e).is_err());
    let mut e = event("gamma_event.json");
    e["slug"] = json!("btc-updown-1h-1792439100");
    assert!(parse_market(&e).is_err());
}

#[test]
fn outcomes_need_a_closed_market_and_final_prices() {
    let closed = event("gamma_closed.json");
    let want = Outcome { payout_up: 0.0, ptb: Some(81914.85950714983), final_price: Some(81835.58325601992) };
    assert_eq!(parse_outcome(&closed), Some(want));
    assert_eq!(parse_outcome(&event("gamma_event.json")), None, "still open");
    let mut half = closed.clone();
    half["markets"][0]["outcomePrices"] = json!("[\"0.5\", \"0.5\"]");
    assert_eq!(parse_outcome(&half).map(|o| o.payout_up), Some(0.5));
    let mut unsettled = closed.clone();
    unsettled["markets"][0]["outcomePrices"] = json!("[\"0.9995\", \"0.0005\"]");
    assert_eq!(parse_outcome(&unsettled), None);
}

#[test]
fn market_info_reads_itode_tick_min_size_and_fees() {
    let info = parse_market_info(&fixture("clob_market.json")).unwrap();
    let want = MarketInfo { taker_delay: true, tick: 0.001, min_size: 5.0, fee: FeeSchedule { rate: 0.07, exponent: 1.0 }, taker_only: true };
    assert_eq!(info, want);
}

#[tokio::test]
async fn fetch_window_asks_each_coin_with_a_cache_buster() {
    let btc = event("gamma_event.json");
    let gamma = FakeGamma::start(vec![btc.clone(), as_coin(btc, "eth")]).await;
    let http = http_client();
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let got = fetch_window(&http, &gamma.url, S, &coins).await.unwrap();
    assert_eq!(got.iter().map(|m| m.coin).collect::<Vec<_>>(), [Coin::Btc, Coin::Eth], "SOL has no market");
    fetch_window(&http, &gamma.url, S, &coins).await.unwrap();
    let q = gamma.queries();
    let slugs: Vec<&str> = q[0].iter().filter(|(k, _)| k == "slug").map(|(_, v)| v.as_str()).collect();
    assert_eq!(slugs, ["btc-updown-15m-1792439100", "eth-updown-15m-1792439100", "sol-updown-15m-1792439100"]);
    let buster = |i: usize| q[i].iter().find(|(k, _)| k == "_").map(|(_, v)| v.clone()).unwrap();
    assert_ne!(buster(0), buster(1), "the CDN keeps a repeated URL for minutes");
}

#[tokio::test]
async fn gamma_is_asked_three_times_then_gives_up() {
    let gamma = FakeGamma::start(vec![event("gamma_event.json")]).await;
    let (http, slug) = (http_client(), ["btc-updown-15m-1792439100".to_string()]);
    gamma.fail_next(2);
    assert_eq!(gamma_events_with(&http, &gamma.url, &slug, Duration::from_millis(10)).await.unwrap().len(), 1);
    gamma.fail_next(3);
    assert!(gamma_events_with(&http, &gamma.url, &slug, Duration::from_millis(10)).await.is_err());
    assert_eq!(gamma.queries().len(), 6);
}

#[tokio::test]
async fn fetch_outcomes_gives_the_settled_coins_only() {
    let gamma = FakeGamma::start(vec![event("gamma_closed.json"), as_coin(event("gamma_event.json"), "eth")]).await;
    // both events answer for the window of the closed one
    let closed_s = 1_792_431_900;
    let mut eth = as_coin(event("gamma_event.json"), "eth");
    eth["slug"] = json!(format!("eth-updown-15m-{closed_s}"));
    gamma.set_events(vec![event("gamma_closed.json"), eth]);
    let got = fetch_outcomes(&http_client(), &gamma.url, closed_s, &[Coin::Btc, Coin::Eth]).await.unwrap();
    assert_eq!(got.iter().map(|(c, o)| (*c, o.payout_up)).collect::<Vec<_>>(), [(Coin::Btc, 0.0)]);
}

#[tokio::test]
async fn the_clob_gives_market_info_a_book_and_a_round_trip() {
    let clob = FakeClob::start().await;
    clob.set_market(CONDITION, fixture("clob_market.json"));
    clob.set_book(UP, fixture("clob_book.json"));
    let http = http_client();
    assert!(fetch_market_info(&http, &clob.url, CONDITION).await.unwrap().taker_delay);
    assert!(fetch_market_info(&http, &clob.url, "0xnone").await.is_err());
    let (ts, bids, asks) = fetch_book(&http, &clob.url, UP).await.unwrap();
    assert_eq!(ts, 1_792_439_760_641);
    assert_eq!((bids.len(), asks.len()), (2, 117));
    assert_eq!((bids[0], asks[0]), ((0.001, 1063.59), (0.999, 20.0)));
    let rtt = measure_rtt(&http, &clob.url).await.unwrap();
    assert!((0.0..1_000.0).contains(&rtt), "{rtt}");
}

#[test]
fn rtt_median_of_the_last_20() {
    let mut r = Rtt::default();
    assert_eq!(r.median_ms(), None);
    for v in [3.0, 1.0, 2.0] {
        r.push(v);
    }
    assert_eq!(r.median_ms(), Some(2.0));
    let mut r = Rtt::default();
    for k in 1..=25 {
        r.push(f64::from(k));
    }
    assert_eq!(r.median_ms(), Some(15.5), "the last 20: 6..=25");
}
