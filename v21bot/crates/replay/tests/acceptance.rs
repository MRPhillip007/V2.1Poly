//! The comparison with the backtest's own tables, and the acceptance of milestone 1: the replay of
//! the whole archive gives the Python backtest's signals, numbers and trades.

use std::collections::BTreeSet;
use std::path::PathBuf;

use v21_core::decision::{Decision, Reason};
use v21_core::detector::Check;
use v21_core::params::{load_strategy, Strategy};
use v21_core::window::{Coin, Side};
use v21_core::zscore::ZInfo;
use v21_replay::archive::Period;
use v21_replay::compare::{compare, load_reference, RefSignal, Reference};
use v21_replay::run::{replay, ReplayOutput, SignalRow};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
const S0: i64 = 1_790_910_900_000;

fn strategy() -> Strategy {
    load_strategy(V2, V21).unwrap()
}

fn reference_row(slug: &str, s0_ms: i64, rank: u32) -> RefSignal {
    RefSignal {
        slug: slug.to_string(),
        coin: Coin::Btc,
        s0_ms,
        t_trig: 840.0,
        side: Side::Up,
        px: 0.80,
        z: 2.0,
        cushion_bp: 5.0,
        sigma_bp: 1.0,
        h: 19.5,
        rank,
        ret_top: 0.2,
        ret: 0.18,
        entry_ms: s0_ms + 840_000,
    }
}

fn decision(c: Check, z: ZInfo, rank: u32, trade: bool) -> Decision {
    Decision {
        check: c,
        counted: true,
        rank: Some(rank),
        z: Some(z),
        hour_ok: true,
        trade,
        reason: if trade { Reason::Enter } else { Reason::ZBelow },
    }
}

/// The replay's row that equals `r`, with the given trade decisions.
fn replay_row(r: &RefSignal, v2_trade: bool, v21_trade: bool) -> SignalRow {
    let c = Check { coin: r.coin, t_trig: r.t_trig, side: r.side, ask_at_trig: r.px, px: r.px, ok: true };
    let z = ZInfo { z: r.z, p0: 100.0, ef: 100.1, sigma_bp: r.sigma_bp, h: r.h, cushion_bp: r.cushion_bp };
    SignalRow {
        slug: r.slug.clone(),
        coin: r.coin,
        s0_ms: r.s0_ms,
        t_trig: r.t_trig,
        side: r.side,
        px: r.px,
        z,
        rank: r.rank,
        p_up_final: 1.0,
        win: 1.0,
        ret_top: r.ret_top,
        ret: r.ret,
        entry_ms: r.entry_ms,
        v2: decision(c, z, r.rank, v2_trade),
        v21: decision(c, z, r.rank, v21_trade),
    }
}

fn output(signals: Vec<SignalRow>) -> ReplayOutput {
    ReplayOutput { period: Period::Main, signals, windows: 0, files: 0 }
}

fn reference(signals: Vec<RefSignal>, v2: &[&str], v21: &[&str]) -> Reference {
    let last_s0_ms = signals.iter().map(|r| r.s0_ms).max().unwrap_or(0);
    Reference {
        signals,
        v2_trades: v2.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        v21_trades: v21.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
        last_s0_ms,
    }
}

#[test]
fn verdict_text() {
    let refs: Vec<RefSignal> = ["a", "b", "c"].iter().enumerate().map(|(k, s)| reference_row(s, S0 + k as i64 * 900_000, 1)).collect();
    let got = output(refs.iter().map(|r| replay_row(r, r.slug == "a", r.slug == "a")).collect());
    let cmp = compare(&got, &reference(refs, &["a"], &["a"]));
    assert!(cmp.problems.is_empty(), "{:?}", cmp.problems);
    assert!(cmp.newer.is_empty());
    assert_eq!(cmp.verdict, "signals 3/3 equal, z max diff < 1e-9, ranks equal, v2 trades 1/1, v2.1 trades 1/1");
}

#[test]
fn each_kind_of_mismatch_is_one_problem_line() {
    let at = |k: i64| S0 + k * 900_000;
    let refs: Vec<RefSignal> = ["a", "b", "c", "d", "e", "f", "g"].iter().enumerate().map(|(k, s)| reference_row(s, at(k as i64), 1)).collect();
    let mut rows: Vec<SignalRow> = refs.iter().filter(|r| r.slug != "b").map(|r| replay_row(r, false, false)).collect();
    let by = |rows: &mut Vec<SignalRow>, slug: &str| rows.iter().position(|x| x.slug == slug).unwrap();
    let c = by(&mut rows, "c");
    rows[c].px = 0.81; // differs
    let d = by(&mut rows, "d");
    rows[d].z.z += 1e-6; // z differs
    let e = by(&mut rows, "e");
    rows[e].rank = 2; // rank differs
    let f = by(&mut rows, "f");
    rows[f].ret += 1e-9; // return differs
    // "g" is a v2 trade of the backtest and not of the replay
    let mut h = replay_row(&reference_row("h", at(2), 2), false, false); // only in the replay, inside the reference
    h.slug = "h".into();
    rows.push(h);
    let newer = replay_row(&reference_row("i", at(100), 1), false, false); // after the reference's last window
    rows.push(newer);
    let cmp = compare(&output(rows), &reference(refs, &["g"], &[]));
    assert_eq!(cmp.problems.len(), 7, "{:#?}", cmp.problems);
    for (prefix, slug) in [
        ("only in the backtest", "b"),
        ("only in the replay", "h"),
        ("differs", "c"),
        ("z differs", "d"),
        ("rank differs", "e"),
        ("return differs", "f"),
        ("v2 trade only in the backtest", "g"),
    ] {
        assert!(
            cmp.problems.iter().any(|p| p.starts_with(prefix) && p.contains(&format!(": {slug}"))),
            "{prefix} {slug}: {:#?}",
            cmp.problems
        );
    }
    assert_eq!(cmp.newer.len(), 1);
    assert!(cmp.newer[0].contains(": i"), "{:?}", cmp.newer);
    assert!(cmp.verdict.contains("1 newer than the reference — rerun ruslan_v2.py"), "{}", cmp.verdict);
}

#[test]
fn signals_after_the_reference_go_to_newer_not_to_problems() {
    let refs = vec![reference_row("a", S0, 1)];
    let mut rows = vec![replay_row(&refs[0], true, true)];
    rows.push(replay_row(&reference_row("z", S0 + 900_000, 1), true, true)); // a newer window that trades
    let cmp = compare(&output(rows), &reference(refs, &["a"], &["a"]));
    assert!(cmp.problems.is_empty(), "{:?}", cmp.problems);
    assert_eq!(cmp.newer.len(), 1);
    assert!(cmp.verdict.starts_with("signals 1/1 equal"), "{}", cmp.verdict);
}

fn root() -> Option<PathBuf> {
    match std::env::var("V21_ROOT") {
        Ok(r) => Some(PathBuf::from(r)),
        Err(_) => {
            println!("skipped: V21_ROOT is not set");
            None
        }
    }
}

#[test]
fn main_archive_matches_python() {
    let Some(root) = root() else { return };
    let started = std::time::Instant::now();
    let got = replay(&root, Period::Main, &strategy()).unwrap();
    let r = load_reference(&root, Period::Main, &strategy().v21).unwrap();
    let cmp = compare(&got, &r);
    println!("main: {} windows, {} files, {} signals in {:.1} s", got.windows, got.files, got.signals.len(), started.elapsed().as_secs_f64());
    println!("main: {}", cmp.verdict);
    for p in cmp.problems.iter().take(40) {
        println!("  {p}");
    }
    assert!(cmp.problems.is_empty(), "{} problems", cmp.problems.len());
    let trades = |f: fn(&SignalRow) -> bool| got.signals.iter().filter(|s| s.s0_ms <= r.last_s0_ms && f(s)).count();
    assert_eq!(trades(|s| s.v2.trade), r.v2_trades.len());
    assert_eq!(trades(|s| s.v21.trade), r.v21_trades.len());
}

#[test]
fn pre_archive_matches_python() {
    let Some(root) = root() else { return };
    let got = replay(&root, Period::Pre, &strategy()).unwrap();
    let r = load_reference(&root, Period::Pre, &strategy().v21).unwrap();
    let cmp = compare(&got, &r);
    println!("pre: {}", cmp.verdict);
    for p in cmp.problems.iter().take(40) {
        println!("  {p}");
    }
    assert!(cmp.problems.is_empty(), "{} problems", cmp.problems.len());
    assert_eq!((r.signals.len(), r.v2_trades.len(), r.v21_trades.len()), (517, 69, 55));
    assert_eq!(cmp.verdict, "signals 517/517 equal, z max diff < 1e-9, ranks equal, v2 trades 69/69, v2.1 trades 55/55");
}
