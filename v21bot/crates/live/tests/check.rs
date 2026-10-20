//! `v21 check` (spec §4.4, the part without keys): the geoblock, the clock, the path to the CLOB,
//! Binance (both addresses), the copies of the market channel and the rules of the current window's
//! markets — against local fake servers.

mod support;

use std::time::Duration;

use serde_json::{json, Value};
use support::fake_binance::FakeBinance;
use support::fake_clob::FakeClob;
use support::fake_gamma::FakeGamma;
use support::fake_polymarket::FakePolymarket;
use v21_live::check::check_with;
use v21_live::shared::clock::local_ms;
use v21_live::shared::config::Config;

const CONDITION: &str = "0x0820ab37b6e2339dceff44b8dde9e3b9425353daf456921e8d877880eb13b260";

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The markets of the current window: the real event of the fixture under each coin's slug.
fn events() -> Vec<Value> {
    let s = local_ms().div_euclid(900_000) * 900;
    ["btc", "eth", "sol"]
        .iter()
        .map(|coin| {
            let mut e = fixture("gamma_event.json")[0].clone();
            e["slug"] = json!(format!("{coin}-updown-15m-{s}"));
            e
        })
        .collect()
}

async fn servers(blocked: bool, binance_refuses: bool) -> (Config, Vec<Box<dyn std::any::Any>>) {
    let pm = FakePolymarket::start(blocked, 100).await;
    let primary = FakeBinance::start(0, binance_refuses, 100).await;
    let mirror = FakeBinance::start(0, false, 100).await;
    let clob = FakeClob::start().await;
    clob.set_market(CONDITION, fixture("clob_market.json"));
    let gamma = FakeGamma::start(events()).await;
    let mut cfg = Config::default();
    cfg.risk.ntp_server = String::new();
    cfg.feeds.binance_ws = primary.ws.clone();
    cfg.feeds.binance_ws_fallback = mirror.ws.clone();
    cfg.feeds.binance_rest = primary.rest.clone();
    cfg.feeds.binance_rest_fallback = mirror.rest.clone();
    cfg.urls.gamma = gamma.url.clone();
    cfg.urls.clob = clob.url.clone();
    cfg.urls.polymarket_ws = pm.ws.clone();
    cfg.urls.geoblock = pm.geoblock.clone();
    (cfg, vec![Box::new(pm), Box::new(primary), Box::new(mirror), Box::new(clob), Box::new(gamma)])
}

#[tokio::test(flavor = "multi_thread")]
async fn check_against_fake_servers_is_0_and_a_geoblock_is_2() {
    let (cfg, _keep) = servers(false, false).await;
    let (text, code) = check_with(&cfg, Duration::from_millis(1500)).await;
    assert_eq!(code, 0, "{text}");
    for line in ["геоблок: открыто (IE)", "часы: Binance", "путь до CLOB: медиана", "Binance основной: первая сделка", "Binance запасной: первая сделка", "Polymarket: копия 1 —", "правила в порядке", "итог: можно работать"] {
        assert!(text.contains(line), "{line}:\n{text}");
    }

    let (cfg, _keep) = servers(true, false).await;
    let (text, code) = check_with(&cfg, Duration::from_millis(500)).await;
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("геоблок: закрыто (US)") && text.contains("итог: работать нельзя"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_primary_binance_that_answers_451_is_a_warning() {
    let (cfg, _keep) = servers(false, true).await;
    let (text, code) = check_with(&cfg, Duration::from_millis(500)).await;
    assert_eq!(code, 1, "{text}");
    assert!(text.contains("Binance основной: недоступен") && text.contains("Binance запасной: первая сделка"), "{text}");
}
