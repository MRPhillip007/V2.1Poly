//! The Telegram messages of spec §8 in the demo mode, their delivery, and the health record.

mod support;

use std::time::Duration;

use serde_json::{json, Value};
use support::fake_telegram::FakeTelegram;
use v21_core::fill::{fee, FeeSchedule, Fill};
use v21_core::window::{Coin, Side};
use v21_live::cold::telegram::{fmt_alarm, fmt_entry, fmt_missed, fmt_result, fmt_summary, DaySummary, Telegram};
use v21_live::health::{health_record, CpuMeter, Hist, HotHealth};
use v21_live::shared::secrets::Secrets;

/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };
const WARN: &str = "\u{26a0}\u{fe0f}";

/// $20 at 0.809: 24.72188 contracts.
fn fill() -> Fill {
    let shares = 20.0 / 0.809;
    Fill { shares, notional_usd: 20.0, fee_usd: fee(shares, 0.809, &FEE), avg_price: 0.809, levels_used: 1, unfilled_usd: 0.0 }
}

fn secrets(token: Option<&str>) -> Secrets {
    Secrets { telegram_token: token.map(str::to_string), telegram_chat_id: Some("42".into()) }
}

#[test]
fn message_texts_are_exact() {
    let f = fill();
    assert_eq!(f.fee_usd, 0.2674);
    assert_eq!(fmt_entry(Coin::Btc, Side::Down, S, &f, Some(1.7)), "🟢 BTC Down · окно 03:15–03:30 · демо: $20.00 по 0.809 → 24.72 шт. · z 1.70");
    assert_eq!(fmt_entry(Coin::Eth, Side::Up, S, &f, None), "🟢 ETH Up · окно 03:15–03:30 · демо: $20.00 по 0.809 → 24.72 шт. · z —");
    // the position cost $20.27 of a $100 balance
    let cost = f.notional_usd + f.fee_usd;
    let result = |per_share: f64| {
        let payout = f.shares * per_share;
        fmt_result(Coin::Btc, Side::Down, per_share, payout - cost, 100.0 - cost + payout)
    };
    assert_eq!(result(1.0), "✅ BTC Down +$4.45 · баланс $104.45");
    assert_eq!(result(0.0), "❌ BTC Down −$20.27 · баланс $79.73");
    assert_eq!(result(0.5), "➖ BTC Down 50/50 −$7.91 · баланс $92.09");
    assert_eq!(fmt_missed(Coin::Eth, Side::Up, S), "⚪ ETH Up · окно 03:15–03:30 · сигнал был, демо-исполнения нет");
    assert_eq!(fmt_alarm("обе копии ленты недоступны 12 с"), format!("{WARN} обе копии ленты недоступны 12 с"));
    // the window of 23:45 ends at 00:00; thousands are grouped
    assert_eq!(fmt_result(Coin::Sol, Side::Up, 1.0, 1234.5, 12_345.678), "✅ SOL Up +$1,234.50 · баланс $12,345.68");
    assert!(fmt_missed(Coin::Btc, Side::Up, S + 20 * 3600 + 30 * 60).contains("окно 23:45–00:00"));

    let day = DaySummary {
        date: "2026-10-19".into(),
        n: 3,
        wins: 3,
        pnl: 12.4,
        balance: 112.4,
        signals: 41,
        stake_usd: 20.0,
        full_fill_pct: 85.0,
        mean_filled_usd: 17.9,
        price_vs_backtest_c: Some(0.4),
        missed: 1,
        fast_ne_exact: 0,
        alarms: vec!["обе копии ленты недоступны 12 с".into()],
    };
    assert_eq!(
        fmt_summary(&day),
        format!(
            "📊 Сводка за 19.10 (UTC) · демо\n\
             Сделок: 3 · выигрыши 3 / проигрыши 0 · P&L +$12.40 · баланс $112.40\n\
             Сигналов: 41 · ордер $20 исполнился полностью в 85% · в среднем $17.90 из $20\n\
             Цена демо − цена бэктеста: +0.4¢\n\
             Упущенные: 1 · быстрое ≠ точное: 0\n\
             {WARN} обе копии ленты недоступны 12 с"
        )
    );
    // a quiet day
    let quiet = DaySummary { n: 0, wins: 0, pnl: 0.0, balance: 100.0, signals: 0, price_vs_backtest_c: None, missed: 0, alarms: vec![], ..day };
    assert_eq!(
        fmt_summary(&quiet),
        "📊 Сводка за 19.10 (UTC) · демо\n\
         Сделок: 0 · выигрыши 0 / проигрыши 0 · P&L +$0.00 · баланс $100.00\n\
         Сигналов: 0\n\
         Цена демо − цена бэктеста: —\n\
         Упущенные: 0 · быстрое ≠ точное: 0"
    );
}

#[tokio::test]
async fn send_retries_and_gives_up_without_stopping() {
    let fake = FakeTelegram::start().await;
    let t = Telegram::new(&secrets(Some("1:tok")), &fake.url).with_pauses([Duration::from_millis(10); 2]);
    fake.answer(&[500, 500]);
    assert!(t.send("первое").await);
    assert_eq!(fake.sent().len(), 3);
    assert_eq!(fake.sent()[2], json!({"chat_id": "42", "text": "первое"}));
    assert_eq!(fake.bots()[0], "bot1:tok");
    fake.answer(&[500, 500, 500]);
    assert!(!t.send("второе").await);
    assert_eq!(fake.sent().len(), 6);
    // two at once go in order, even when the first needs a second attempt
    fake.answer(&[500]);
    let (a, b) = tokio::join!(t.send("третье"), t.send("четвёртое"));
    assert!(a && b);
    let texts: Vec<Value> = fake.sent().iter().map(|m| m["text"].clone()).collect();
    assert_eq!(texts[6..], [json!("третье"), json!("третье"), json!("четвёртое")]);
}

#[tokio::test]
async fn the_token_never_reaches_the_log() {
    // a server that drops every connection: the request fails, and the error names the URL, token and all
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            drop(sock);
        }
    });
    let t = Telegram::new(&secrets(Some("123456:SECRET-token")), &url).with_pauses([Duration::ZERO; 2]);
    assert!(!t.send("проверка").await);
    let log = t.log_lines().join("\n");
    assert!(log.contains("bot***/sendMessage"), "{log}");
    assert!(!log.contains("SECRET"), "{log}");
}

#[tokio::test]
async fn without_a_token_messages_only_go_to_the_log() {
    let fake = FakeTelegram::start().await;
    let t = Telegram::new(&secrets(None), &fake.url);
    assert!(!t.send("вход").await);
    assert!(fake.sent().is_empty());
    assert_eq!(t.log_lines(), ["[telegram off] вход"]);
    // a token without a chat: nowhere to write
    let t = Telegram::new(&Secrets { telegram_token: Some("1:tok".into()), telegram_chat_id: None }, &fake.url);
    assert!(!t.enabled());
    assert!(!t.send("вход").await);
    assert!(fake.sent().is_empty());
}

// ---------------------------------------------------------------- health

#[test]
fn hist_quantiles() {
    let mut h = Hist::default();
    assert_eq!((h.quantile(0.5), h.quantile(0.99), h.max()), (0, 0, 0));
    for v in 1..=1000 {
        h.add(v);
    }
    // by powers of two: the 500th value is in 256..=511, the 990th in 512..=1023 — no more than the largest
    assert_eq!((h.quantile(0.5), h.quantile(0.99), h.max()), (511, 1000, 1000));
    h.add(u64::MAX);
    h.add(0);
    assert_eq!(h.max(), u64::MAX, "the largest, not the last");
    assert_eq!(h.quantile(0.0), 0);
}

#[test]
fn health_record_has_the_spec_fields() {
    let mut h = HotHealth { frames: 1000, dup_frames: 990, bad_parts: 1, trades: 5000, dropped_msgs: 2, copies_up: 2, ..HotHealth::default() };
    for v in [40, 50, 60, 70] {
        h.pm_lag_ms.add(v);
        h.bn_lag_ms.add(v / 10);
    }
    h.decide_ns.add(3_000);
    let r = health_record(&h, Some(-12), Some(41.5), None);
    assert_eq!(
        (&r["pm_lag_ms_p50"], &r["pm_lag_ms_p99"], &r["bn_lag_ms_p50"], &r["bn_lag_ms_p99"]),
        (&json!(63), &json!(70), &json!(7), &json!(7))
    );
    assert_eq!((&r["decide_ns_p50"], &r["decide_ns_p99"], &r["decide_ns_max"]), (&json!(3000), &json!(3000), &json!(3000)));
    assert_eq!(
        (&r["copies_up"], &r["frames"], &r["dup_frames"], &r["bad_parts"], &r["trades"], &r["dropped_msgs"]),
        (&json!(2), &json!(1000), &json!(990), &json!(1), &json!(5000), &json!(2))
    );
    assert_eq!((&r["clock_offset_ms"], &r["rtt_ms"], &r["cpu_pct"]), (&json!(-12), &json!(41.5), &Value::Null));
}

#[test]
fn cpu_is_read_from_proc_on_linux_only() {
    let mut m = CpuMeter::default();
    let first = m.sample();
    let mut x = 0u64;
    for i in 0..2_000_000u64 {
        x = x.wrapping_mul(31).wrapping_add(i);
    }
    std::hint::black_box(x);
    std::thread::sleep(Duration::from_millis(20));
    let second = m.sample();
    if cfg!(target_os = "linux") {
        assert!(first.is_none() && second.is_some_and(|c| c >= 0.0), "{first:?} {second:?}");
    } else {
        assert_eq!((first, second), (None, None));
    }
}
