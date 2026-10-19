//! The fast engine (spec §5.3, §6.1): the decision at the start of a check, on the events as they
//! come — the coin's number from the checks of the other coins, the fast z, the rules of the bot —
//! and the limits of the shots.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use v21_core::closes::ArrayCloses;
use v21_core::decision::Reason;
use v21_core::fill::FeeSchedule;
use v21_core::params::{load_strategy, Params};
use v21_core::window::{Coin, Side};
use v21_live::hot::binance::Trade;
use v21_live::hot::engine::FastEngine;
use v21_live::hot::items::FeedItem;
use v21_live::shared::msgs::{CoinArm, FastOut, FastReason, FastSkip, ShotRecord, WindowArm};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC: in the hours of v2.1.
const S: i64 = 1_790_910_900;
const S0: i64 = S * 1000;
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };
/// No silence pause in the tests that are not about it.
const NEVER: i64 = i64::MAX / 4;

fn params() -> Params {
    load_strategy(V2, V21).unwrap().v21
}

/// Tokens 0..6: BTC Up/Down, ETH Up/Down, SOL Up/Down. ETH's Up id is the smallest, then BTC's, then
/// SOL's: at one t_trig ETH comes first.
fn coin(coin: Coin, up: u8, key: u8) -> CoinArm {
    CoinArm { coin, up_ix: up, down_ix: up + 1, up_key: [key; 32], fee: FEE, tradable: true, note: String::new() }
}

fn arm(s: i64) -> WindowArm {
    WindowArm {
        s,
        tokens: (0..6).map(|i| format!("t{i}")).collect(),
        coins: vec![coin(Coin::Btc, 0, 2), coin(Coin::Eth, 2, 1), coin(Coin::Sol, 4, 3)],
        missing: vec![],
        resumed: false,
        traded: vec![],
    }
}

/// Closes rising 0.06 ¢ a second from `first`: z is far above 1.25 for Up.
fn rising(first: i64, last: i64) -> Vec<(i64, f64)> {
    (first..=last).map(|q| (q * 1000, 100.0 + 0.0006 * (q - first) as f64)).collect()
}

/// Closes falling 0.06 ¢ a second from `first`: z is far above 1.25 for Down.
fn falling(first: i64, last: i64) -> Vec<(i64, f64)> {
    (first..=last).map(|q| (q * 1000, 100.0 - 0.0006 * (q - first) as f64)).collect()
}

/// Closes that only wiggle: z is near 0.
fn flat(first: i64, last: i64) -> Vec<(i64, f64)> {
    (first..=last).map(|q| (q * 1000, 100.0 + ((q % 7) as f64) * 0.01)).collect()
}

fn engine_for(arm: &WindowArm, silent_ms: i64, closes: fn(i64, i64) -> Vec<(i64, f64)>) -> FastEngine {
    let mut e = FastEngine::new(params(), silent_ms);
    for c in [Coin::Btc, Coin::Eth, Coin::Sol] {
        e.on_klines(c, &closes(arm.s - 1000, arm.s + 900));
    }
    e.arm(arm);
    e
}

fn engine() -> FastEngine {
    engine_for(&arm(S), NEVER, rising)
}

fn bbo(token: u8, t: i64, bid: f64, ask: f64) -> FeedItem {
    FeedItem::Bbo { token, ts_ms: S0 + t, bid, ask }
}

/// A row of `token` at `t` ms into the window, arriving at the same moment.
fn row(e: &mut FastEngine, token: u8, t: i64, bid: f64, ask: f64) -> Vec<FastOut> {
    let mut out = vec![];
    e.on_item(S0 + t, &bbo(token, t, bid, ask), &mut out);
    out
}

fn tick(e: &mut FastEngine, t: i64) -> Vec<FastOut> {
    let mut out = vec![];
    e.on_tick(S0 + t, &mut out);
    out
}

fn shots(out: &[FastOut]) -> Vec<ShotRecord> {
    out.iter().filter_map(|o| if let FastOut::Shot(s) = o { Some(*s) } else { None }).collect()
}

fn skips(out: &[FastOut]) -> Vec<FastSkip> {
    out.iter().filter_map(|o| if let FastOut::Skip(s) = o { Some(*s) } else { None }).collect()
}

fn waits(out: &[FastOut]) -> Vec<(Coin, f64)> {
    out.iter().filter_map(|o| if let FastOut::Wait { coin, t_trig, .. } = o { Some((*coin, *t_trig)) } else { None }).collect()
}

/// The book of BTC Up is in the band (ask 0.80) and those of ETH and SOL are not.
const IN: (f64, f64) = (0.18, 0.80);
const OUT: (f64, f64) = (0.04, 0.95);

// ---------------------------------------------------------------- when it fires

#[test]
fn the_book_at_840_fires_on_the_840_tick() {
    let mut e = engine();
    row(&mut e, 0, 839_500, IN.0, IN.1);
    assert!(tick(&mut e, 839_999).is_empty());
    let out = tick(&mut e, 840_000);
    let s = shots(&out);
    assert_eq!(s.len(), 1, "{out:?}");
    assert_eq!((s[0].s, s[0].coin, s[0].side, s[0].t_trig, s[0].rank), (S, Coin::Btc, Side::Up, 840.0, 1));
    // sent at the decision; the check started on the book of 839.5 s
    assert_eq!((s[0].send_ms, s[0].event_ts_ms), (S0 + 840_000, S0 + 839_500));
    assert!(s[0].z.unwrap().z > 1.25);
}

#[test]
fn a_band_hit_mid_minute_fires_on_its_event() {
    let mut e = engine();
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    assert!(tick(&mut e, 840_000).is_empty());
    let mut out = vec![];
    e.on_item(S0 + 845_150, &bbo(0, 845_123, IN.0, IN.1), &mut out);
    let s = shots(&out);
    assert_eq!(s.len(), 1, "{out:?}");
    assert_eq!((s[0].t_trig, s[0].event_ts_ms, s[0].send_ms), (845.123, S0 + 845_123, S0 + 845_150));
}

#[test]
fn a_late_first_tick_still_decides_the_book_at_840() {
    // the hot loop was held up: the first tick after 840 s comes after the check's price moment
    let mut e = engine();
    row(&mut e, 0, 839_500, IN.0, IN.1);
    let s = shots(&tick(&mut e, 840_500));
    assert_eq!((s[0].t_trig, s[0].send_ms), (840.0, S0 + 840_500));
}

#[test]
fn a_number_known_only_as_the_check_closes_gives_its_reason() {
    // all three coins in the band at 840 s: SOL's number waits for ETH's and BTC's checks, which close
    // with it — two successes before it make it the third coin
    let mut e = engine();
    for token in [0, 2, 4] {
        row(&mut e, token, 839_000, IN.0, IN.1);
    }
    let out = tick(&mut e, 840_000);
    assert_eq!(shots(&out).iter().map(|s| (s.coin, s.rank)).collect::<Vec<_>>(), [(Coin::Eth, 1), (Coin::Btc, 1)]);
    assert_eq!(waits(&out), [(Coin::Sol, 840.0)]);
    let k = skips(&tick(&mut e, 840_301));
    assert_eq!((k[0].coin, k[0].rank, k[0].reason), (Coin::Sol, Some(3), FastReason::Rule(Reason::ThirdCoin)));
}

// ---------------------------------------------------------------- the number of the coin

#[test]
fn a_third_coin_does_not_fire() {
    let mut e = engine();
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    row(&mut e, 2, 839_000, IN.0, IN.1);
    row(&mut e, 4, 839_000, IN.0, IN.1);
    tick(&mut e, 840_000);
    // ETH's and SOL's checks get their price: both count
    tick(&mut e, 840_301);
    let out = row(&mut e, 0, 841_000, IN.0, IN.1);
    assert!(shots(&out).is_empty());
    let k = skips(&out);
    assert_eq!((k[0].coin, k[0].rank, k[0].reason), (Coin::Btc, Some(3), FastReason::Rule(Reason::ThirdCoin)));
}

#[test]
fn an_open_rank_waits_and_then_fires() {
    let mut e = engine();
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    row(&mut e, 2, 839_000, OUT.0, OUT.1);
    row(&mut e, 4, 839_000, IN.0, IN.1);
    tick(&mut e, 840_000); // SOL: number 1
    row(&mut e, 2, 840_200, IN.0, IN.1); // ETH starts a check
    tick(&mut e, 840_301); // SOL's check counts
    // BTC: SOL counts and ETH's check is open, so BTC is number 2 or 3
    let out = row(&mut e, 0, 840_350, IN.0, IN.1);
    assert!(shots(&out).is_empty() && skips(&out).is_empty(), "{out:?}");
    assert_eq!(waits(&out), [(Coin::Btc, 840.35)]);
    // ETH's ask leaves the band: its check fails at 840.5, so BTC is number 2
    row(&mut e, 2, 840_450, OUT.0, OUT.1);
    let out = tick(&mut e, 840_501);
    let s = shots(&out);
    assert_eq!((s[0].coin, s[0].rank, s[0].send_ms), (Coin::Btc, 2, S0 + 840_501), "{out:?}");
}

#[test]
fn sol_counts_but_never_fires() {
    let mut e = engine();
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    row(&mut e, 4, 839_000, IN.0, IN.1);
    let out = tick(&mut e, 840_000);
    let k = skips(&out);
    assert_eq!((k[0].coin, k[0].rank, k[0].reason), (Coin::Sol, Some(1), FastReason::Rule(Reason::CoinNotTraded)));
    tick(&mut e, 840_301);
    let s = shots(&row(&mut e, 0, 841_000, IN.0, IN.1));
    assert_eq!((s[0].coin, s[0].rank), (Coin::Btc, 2));
}

#[test]
fn a_coin_without_a_market_lets_only_rank_1_fire() {
    // SOL has no market: it may have had an earlier check that succeeded
    let arm = WindowArm {
        s: S,
        tokens: (0..4).map(|i| format!("t{i}")).collect(),
        coins: vec![coin(Coin::Btc, 0, 2), coin(Coin::Eth, 2, 1)],
        missing: vec![Coin::Sol],
        resumed: false,
        traded: vec![],
    };
    let mut e = engine_for(&arm, NEVER, rising);
    row(&mut e, 0, 839_000, IN.0, IN.1);
    row(&mut e, 2, 839_000, OUT.0, OUT.1);
    let s = shots(&tick(&mut e, 840_000));
    assert_eq!((s[0].coin, s[0].rank), (Coin::Btc, 1));
    tick(&mut e, 840_301);
    // ETH is number 2, or 3 if SOL had one: it waits, and its check closes still not knowing
    let out = row(&mut e, 2, 841_000, IN.0, IN.1);
    assert_eq!(waits(&out), [(Coin::Eth, 841.0)]);
    let out = tick(&mut e, 841_301);
    let k = skips(&out);
    assert_eq!((k[0].coin, k[0].rank, k[0].reason), (Coin::Eth, None, FastReason::RankUnknown), "{out:?}");
}

// ---------------------------------------------------------------- the strategy

#[test]
fn out_of_hours_and_low_z_do_not_fire() {
    // 10:00 UTC: outside 0–7
    let s10 = S + 24_300;
    let mut e = engine_for(&arm(s10), NEVER, rising);
    let mut out = vec![];
    e.on_item(s10 * 1000 + 839_000, &FeedItem::Bbo { token: 0, ts_ms: s10 * 1000 + 839_000, bid: IN.0, ask: IN.1 }, &mut out);
    e.on_tick(s10 * 1000 + 840_000, &mut out);
    assert!(shots(&out).is_empty());
    assert_eq!(skips(&out)[0].reason, FastReason::Rule(Reason::OutOfHours));
    // the price only wiggles: z is low
    let mut e = engine_for(&arm(S), NEVER, flat);
    row(&mut e, 0, 839_000, IN.0, IN.1);
    let out = tick(&mut e, 840_000);
    assert!(shots(&out).is_empty());
    assert_eq!(skips(&out)[0].reason, FastReason::Rule(Reason::ZBelow));
}

// ---------------------------------------------------------------- the feed

#[test]
fn an_old_event_does_not_change_the_state() {
    let mut e = engine();
    row(&mut e, 0, 839_500, OUT.0, OUT.1);
    // a row older than the one applied comes after it: dropped
    let mut out = vec![];
    e.on_item(S0 + 839_600, &bbo(0, 839_400, IN.0, IN.1), &mut out);
    assert!(out.is_empty());
    assert!(tick(&mut e, 840_000).is_empty(), "the book at 840 s is the one of 839.5 s");
}

#[test]
fn after_coverage_loss_no_shot_until_a_fresh_snapshot() {
    let mut e = engine();
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    tick(&mut e, 840_000);
    e.on_coverage_lost();
    // a change in the band before a fresh snapshot: the book behind it is unknown
    assert!(row(&mut e, 0, 842_000, IN.0, IN.1).is_empty());
    assert!(tick(&mut e, 842_500).is_empty());
    let mut out = vec![];
    e.on_item(S0 + 843_000, &FeedItem::Snapshot { token: 0, ts_ms: S0 + 843_000 }, &mut out);
    e.on_item(S0 + 843_000, &bbo(0, 843_000, 0.03, 0.96), &mut out);
    assert!(out.is_empty());
    let s = shots(&row(&mut e, 0, 844_000, IN.0, IN.1));
    assert_eq!(s.iter().map(|s| s.t_trig).collect::<Vec<_>>(), [844.0]);

    // a Down check whose token has no fresh snapshot yet
    let mut e = engine_for(&arm(S), NEVER, falling);
    row(&mut e, 0, 839_000, OUT.0, OUT.1);
    tick(&mut e, 840_000);
    e.on_coverage_lost();
    let mut out = vec![];
    e.on_item(S0 + 843_000, &FeedItem::Snapshot { token: 0, ts_ms: S0 + 843_000 }, &mut out);
    // the Up bid 0.20: Down's ask is 0.80, in the band
    e.on_item(S0 + 843_000, &bbo(0, 843_000, 0.20, 0.96), &mut out);
    let k = skips(&out);
    assert_eq!((k[0].side, k[0].reason), (Side::Down, FastReason::StaleBook), "{out:?}");
}

#[test]
fn binance_silence_pauses_the_shots() {
    let mut e = engine_for(&arm(S), 2000, rising);
    // the last trade at 836 s (at the price of its second), Polymarket alive
    let price = 100.0 + 0.0006 * 1836.0;
    e.on_trade(S0 + 836_000, &Trade { coin: Coin::Btc, ts_ms: S0 + 836_000, price });
    row(&mut e, 0, 839_500, IN.0, IN.1);
    row(&mut e, 2, 839_500, OUT.0, OUT.1);
    let out = tick(&mut e, 840_000);
    assert!(shots(&out).is_empty());
    assert_eq!(skips(&out)[0].reason, FastReason::Paused("пауза: Binance молчит"));
    // trades again: ETH's band hit fires
    e.on_trade(S0 + 841_000, &Trade { coin: Coin::Btc, ts_ms: S0 + 841_000, price: price + 0.003 });
    let s = shots(&row(&mut e, 2, 841_500, IN.0, IN.1));
    assert_eq!(s[0].coin, Coin::Eth);
    // Polymarket silent for 2.5 s, Binance alive
    let mut e = engine_for(&arm(S), 2000, rising);
    e.on_trade(S0 + 839_900, &Trade { coin: Coin::Btc, ts_ms: S0 + 839_900, price: 100.0 + 0.0006 * 1839.0 });
    row(&mut e, 0, 837_500, IN.0, IN.1);
    let out = tick(&mut e, 840_000);
    assert_eq!(skips(&out)[0].reason, FastReason::Paused("пауза: лента Polymarket молчит"), "{out:?}");
}

// ---------------------------------------------------------------- the rules of the bot

#[test]
fn a_changed_rule_blocks_the_coin_but_it_still_counts() {
    let mut a = arm(S);
    a.coins[0].tradable = false;
    a.coins[0].note = "окно TWAP 30 с (ожидалось 60)".into();
    let mut e = engine_for(&a, NEVER, rising);
    row(&mut e, 0, 839_000, IN.0, IN.1);
    row(&mut e, 2, 839_000, OUT.0, OUT.1);
    let out = tick(&mut e, 840_000);
    let k = skips(&out);
    assert_eq!((k[0].coin, k[0].rank, k[0].reason), (Coin::Btc, Some(1), FastReason::RulesChanged));
    tick(&mut e, 840_301);
    let s = shots(&row(&mut e, 2, 841_000, IN.0, IN.1));
    assert_eq!((s[0].coin, s[0].rank), (Coin::Eth, 2));
}

#[test]
fn a_pause_from_the_cold_side_blocks_shots() {
    let mut e = engine();
    e.set_pause(Some("пауза: часы не измерены"));
    row(&mut e, 0, 839_000, IN.0, IN.1);
    row(&mut e, 2, 839_000, OUT.0, OUT.1);
    let out = tick(&mut e, 840_000);
    assert!(shots(&out).is_empty());
    assert_eq!(skips(&out)[0].reason, FastReason::Paused("пауза: часы не измерены"));
    e.set_pause(None);
    let s = shots(&row(&mut e, 2, 841_000, IN.0, IN.1));
    assert_eq!(s[0].coin, Coin::Eth);
}

#[test]
fn an_empty_fill_after_a_failed_check_rearms_the_coin() {
    for (filled, again) in [(false, true), (true, false)] {
        let mut e = engine();
        row(&mut e, 0, 839_000, IN.0, IN.1);
        assert_eq!(shots(&tick(&mut e, 840_000)).len(), 1);
        // the ask leaves the band: the check fails at 840.3
        row(&mut e, 0, 840_100, OUT.0, OUT.1);
        tick(&mut e, 840_301);
        e.on_fill(Coin::Btc, filled);
        let out = row(&mut e, 0, 841_000, IN.0, IN.1);
        assert_eq!(shots(&out).len() == 1, again, "filled {filled}: {out:?}");
        if !again {
            assert_eq!(skips(&out)[0].reason, FastReason::ShotLimit);
        }
    }
}

#[test]
fn a_band_hit_waits_for_the_answer_to_the_previous_shot() {
    let mut e = engine();
    row(&mut e, 0, 839_000, IN.0, IN.1);
    assert_eq!(shots(&tick(&mut e, 840_000)).len(), 1);
    row(&mut e, 0, 840_100, OUT.0, OUT.1);
    tick(&mut e, 840_301);
    // back in the band before the answer to the shot
    let out = row(&mut e, 0, 840_500, IN.0, IN.1);
    assert_eq!(waits(&out), [(Coin::Btc, 840.5)]);
    assert!(tick(&mut e, 840_550).is_empty());
    e.on_fill(Coin::Btc, false);
    let s = shots(&tick(&mut e, 840_600));
    assert_eq!((s[0].t_trig, s[0].send_ms), (840.5, S0 + 840_600));
}

#[test]
fn two_shots_at_most_in_a_window() {
    // under a rule of three coins a window, all three in the band at 840 s: the third is not shot
    let mut p = load_strategy(V2, V21).unwrap().v2;
    p.max_coins = 3;
    let mut e = FastEngine::new(p, NEVER);
    for c in [Coin::Btc, Coin::Eth, Coin::Sol] {
        e.on_klines(c, &rising(S - 1000, S + 900));
    }
    e.arm(&arm(S));
    for token in [0, 2, 4] {
        row(&mut e, token, 839_000, IN.0, IN.1);
    }
    let out = tick(&mut e, 840_000);
    assert_eq!(shots(&out).iter().map(|s| s.coin).collect::<Vec<_>>(), [Coin::Eth, Coin::Btc]);
    assert_eq!(skips(&out).iter().map(|k| (k.coin, k.reason)).collect::<Vec<_>>(), [(Coin::Sol, FastReason::ShotLimit)]);
}

#[test]
fn a_coin_bought_before_a_restart_does_not_fire_again() {
    let mut a = arm(S);
    a.traded = vec![Coin::Eth];
    let mut e = engine_for(&a, NEVER, rising);
    row(&mut e, 0, 839_000, IN.0, IN.1);
    row(&mut e, 2, 839_000, IN.0, IN.1);
    let out = tick(&mut e, 840_000);
    let s = shots(&out);
    assert_eq!(s.iter().map(|s| s.coin).collect::<Vec<_>>(), [Coin::Btc]);
    assert_eq!(skips(&out).iter().map(|k| (k.coin, k.reason)).collect::<Vec<_>>(), [(Coin::Eth, FastReason::ShotLimit)]);
}

// ---------------------------------------------------------------- an archive day

fn root() -> Option<PathBuf> {
    match std::env::var("V21_ROOT").or_else(|_| std::env::var("V21_DATA")) {
        Ok(r) => Some(PathBuf::from(r)),
        Err(_) => {
            println!("skipped: V21_ROOT is not set");
            None
        }
    }
}

#[test]
fn fast_equals_exact_on_the_archive_for_successful_checks() {
    use v21_replay::archive::{bbo_files, load_closes, load_windows, read_bbo_file, Incidents, Period};
    use v21_replay::run::replay_with;
    let Some(root) = root() else { return };
    let strategy = load_strategy(V2, V21).unwrap();
    // 2026-10-15 UTC, the day of the exact engine's test
    let (day0, day1) = (1_792_022_400_000, 1_792_108_800_000);
    let files: Vec<PathBuf> = bbo_files(&root, Period::Main)
        .unwrap()
        .into_iter()
        .filter(|f| ("2026-10-14T23.parquet"..="2026-10-16T00.parquet").contains(&f.file_name().unwrap().to_string_lossy().as_ref()))
        .collect();
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let inc = Incidents::load(&root.join("data/INCIDENTS.json"), 30).unwrap();
    let windows: Vec<_> = load_windows(&root, Period::Main, &coins, Some(&inc)).unwrap().into_iter().filter(|w| (day0..day1).contains(&w.s0_ms)).collect();
    let closes: HashMap<Coin, ArrayCloses> = coins.iter().map(|&c| (c, load_closes(&root, Period::Main, c).unwrap())).collect();
    let want = replay_with(&files, &windows, &closes, &strategy).unwrap();

    let tokens: HashSet<[u8; 32]> = windows.iter().map(|w| w.up).collect();
    type Rows = HashMap<[u8; 32], Vec<(i64, f64, f64)>>;
    let mut rows: Rows = HashMap::new();
    for f in &files {
        for r in read_bbo_file(f, &tokens).unwrap() {
            rows.entry(r.token).or_default().push((r.ts_ms, r.bid, r.ask));
        }
    }
    for list in rows.values_mut() {
        list.sort_by_key(|r| r.0);
    }
    let mut by_window: BTreeMap<i64, Vec<&v21_replay::archive::WindowMeta>> = BTreeMap::new();
    for w in &windows {
        by_window.entry(w.s0_ms).or_default().push(w);
    }
    // (s0, coin, t_trig) → (rank, the reason of v2.1 as the fast engine gave it)
    let mut fast: HashMap<(i64, Coin, u64), (Option<u32>, FastReason)> = HashMap::new();
    for (&s0, metas) in &by_window {
        let s = s0 / 1000;
        let mut e = FastEngine::new(strategy.v21.clone(), NEVER);
        for &c in &coins {
            let cl = &closes[&c];
            let first = (s - 1000).max(cl.t0());
            e.on_klines(c, &(first..=s + 900).filter_map(|q| Some((q * 1000, v21_core::closes::Closes::close(cl, q)?))).collect::<Vec<_>>());
        }
        let n = metas.len() as u8;
        let arms: Vec<CoinArm> = metas
            .iter()
            .enumerate()
            .map(|(i, m)| CoinArm { coin: m.coin, up_ix: i as u8, down_ix: n + i as u8, up_key: m.up, fee: FEE, tradable: true, note: String::new() })
            .collect();
        let missing = coins.iter().copied().filter(|c| !metas.iter().any(|m| m.coin == *c)).collect();
        e.arm(&WindowArm { s, tokens: (0..2 * n).map(|i| format!("t{i}")).collect(), coins: arms, missing, resumed: false, traded: vec![] });
        let mut merged: Vec<(i64, u8, f64, f64)> = vec![];
        for (i, m) in metas.iter().enumerate() {
            merged.extend(rows.get(&m.up).map(Vec::as_slice).unwrap_or_default().iter().map(|r| (r.0, i as u8, r.1, r.2)));
        }
        merged.sort_by_key(|r| (r.0, r.1));
        let mut out = vec![];
        let mut step = |e: &mut FastEngine, out: &mut Vec<FastOut>| {
            for o in out.drain(..) {
                match o {
                    FastOut::Shot(shot) => {
                        fast.insert((s0, shot.coin, shot.t_trig.to_bits()), (Some(shot.rank), FastReason::Rule(Reason::Enter)));
                        // the demo answer: nothing bought
                        e.on_fill(shot.coin, false);
                    }
                    FastOut::Skip(k) => {
                        fast.insert((s0, k.coin, k.t_trig.to_bits()), (k.rank, k.reason));
                    }
                    FastOut::Wait { .. } => {}
                }
            }
        };
        for &(ts, token, bid, ask) in &merged {
            e.on_item(ts, &FeedItem::Bbo { token, ts_ms: ts, bid, ask }, &mut out);
            e.on_tick(ts, &mut out);
            step(&mut e, &mut out);
        }
        e.on_tick(s0 + 1_000_000, &mut out);
        step(&mut e, &mut out);
    }
    assert!(want.len() > 20);
    let max_coins = strategy.v21.max_coins;
    for w in &want {
        let (rank, reason) = fast.get(&(w.s0_ms, w.coin, w.t_trig.to_bits())).unwrap_or_else(|| panic!("{}: no fast decision", w.slug));
        assert_eq!(*reason, FastReason::Rule(w.v21.reason), "{}", w.slug);
        // a check of another coin still open at the start can only raise the number within max_coins
        let r = rank.unwrap_or_else(|| panic!("{}: no number", w.slug));
        assert!(r == w.rank || (r < w.rank && w.rank <= max_coins), "{}: number {r} vs {}", w.slug, w.rank);
    }
}
