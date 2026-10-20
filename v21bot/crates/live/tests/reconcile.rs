//! `v21 reconcile` (spec §5.5): the live windows of the journal against the archive replayed by the
//! code of milestone 1 — the signals and both decisions, and the book rows of 835–871 s.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use v21_core::closes::{ArrayCloses, Closes};
use v21_core::fill::FeeSchedule;
use v21_core::params::{load_strategy, Strategy};
use v21_core::window::Coin;
use v21_live::cold::exact::{DemoTiming, ExactEngine, ExactOut};
use v21_live::cold::journal::Journal;
use v21_live::cold::records::{bbo_records, signal_record, FastSeen};
use v21_live::hot::items::FeedItem;
use v21_live::reconcile::{reconcile, reconcile_summary};
use v21_live::shared::msgs::{CoinArm, FastOut, FastReason, FastSkip, ShotRecord, WindowArm};
use v21_replay::archive::{bbo_files, load_closes, load_windows, read_bbo_file, Period};

const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };
/// 2026-10-15 UTC, the day of the exact engine's test: the first windows of it.
const DAY0_MS: i64 = 1_792_022_400_000;
const UNTIL_MS: i64 = DAY0_MS + 8 * 3_600_000;

fn root() -> Option<PathBuf> {
    match std::env::var("V21_ROOT").or_else(|_| std::env::var("V21_DATA")) {
        Ok(r) => Some(PathBuf::from(r)),
        Err(_) => {
            println!("skipped: V21_ROOT is not set");
            None
        }
    }
}

fn strategy(root: &Path) -> Strategy {
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap();
    load_strategy(&read("strategy_v2.json"), &read("strategy_v2_1.json")).unwrap()
}

/// The records a live day would have left: the exact engine on the archive's rows of the first eight
/// hours of 15.10, a fast decision the same as the exact one for every check, and the book rows.
fn journal_records(root: &Path, st: &Strategy) -> Vec<(String, Value)> {
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let files: Vec<PathBuf> = bbo_files(root, Period::Main)
        .unwrap()
        .into_iter()
        .filter(|f| ("2026-10-14T23.parquet"..="2026-10-15T09.parquet").contains(&f.file_name().unwrap().to_string_lossy().as_ref()))
        .collect();
    let windows: Vec<_> = load_windows(root, Period::Main, &coins, None).unwrap().into_iter().filter(|w| (DAY0_MS..UNTIL_MS).contains(&w.s0_ms)).collect();
    let closes: HashMap<Coin, ArrayCloses> = coins.iter().map(|&c| (c, load_closes(root, Period::Main, c).unwrap())).collect();
    let tokens: HashSet<[u8; 32]> = windows.iter().map(|w| w.up).collect();
    let mut rows: HashMap<[u8; 32], Vec<(i64, f64, f64)>> = HashMap::new();
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
    let timing = DemoTiming { stake_usd: 20.0, limit: 0.88, half_rtt_ms: 0.0, taker_delay_ms: 250.0 };
    let mut out = vec![];
    for (&s0, metas) in &by_window {
        let s = s0 / 1000;
        let mut e = ExactEngine::new(st.clone(), 500, timing);
        for &c in &coins {
            let cl = &closes[&c];
            e.on_klines(c, &((s - 1000).max(cl.t0())..=s + 900).filter_map(|q| Some((q * 1000, cl.close(q)?))).collect::<Vec<_>>());
        }
        let n = metas.len() as u8;
        let arms = metas.iter().enumerate().map(|(i, m)| CoinArm { coin: m.coin, up_ix: i as u8, down_ix: n + i as u8, up_key: m.up, fee: FEE, tradable: true, note: String::new() }).collect();
        let missing = coins.iter().copied().filter(|c| !metas.iter().any(|m| m.coin == *c)).collect();
        e.arm(&WindowArm { s, tokens: (0..2 * n).map(|i| format!("t{i}")).collect(), coins: arms, missing, resumed: false, traded: vec![] });
        let mut merged: Vec<(i64, u8, f64, f64)> = vec![];
        for (i, m) in metas.iter().enumerate() {
            merged.extend(rows.get(&m.up).map(Vec::as_slice).unwrap_or_default().iter().map(|r| (r.0, i as u8, r.1, r.2)));
        }
        merged.sort_by_key(|r| (r.0, r.1));
        let mut got = vec![];
        for &(ts, token, bid, ask) in &merged {
            e.on_item(ts, FeedItem::Bbo { token, ts_ms: ts, bid, ask });
            e.advance(ts, &mut got);
        }
        e.advance(s0 + 1_100_000, &mut got);
        for o in got {
            match o {
                ExactOut::Signal(sig) => {
                    // the fast engine decided as the exact one did
                    let c = &sig.check;
                    let fast = if sig.v21.trade {
                        FastOut::Shot(ShotRecord { s, coin: c.coin, side: c.side, t_trig: c.t_trig, event_ts_ms: 0, send_ms: 0, rank: sig.rank.unwrap_or(0), z: sig.z, frame_to_decision_ns: 0 })
                    } else {
                        FastOut::Skip(FastSkip { s, coin: c.coin, side: c.side, t_trig: c.t_trig, rank: sig.rank, z: sig.z, reason: FastReason::Rule(sig.v21.reason) })
                    };
                    out.push(("signal".to_string(), signal_record(&sig, Some(&FastSeen { out: fast, wait_ms: None }), st)));
                }
                ExactOut::WindowDone(w) => out.extend(bbo_records(&w).into_iter().map(|r| ("bbo".to_string(), r))),
                _ => {}
            }
        }
    }
    out
}

fn write(dir: &Path, records: &[(String, Value)]) -> Journal {
    let mut j = Journal::open(&dir.join("journal.jsonl")).unwrap();
    for (kind, rec) in records {
        j.append(kind, rec.clone()).unwrap();
    }
    j
}

#[test]
fn reconcile_of_a_journal_made_from_the_archive_is_clean() {
    let Some(root) = root() else { return };
    let st = strategy(&root);
    let records = journal_records(&root, &st);
    let dir = tempfile::tempdir().unwrap();
    let j = write(dir.path(), &records);
    let rows = reconcile(&j, &root, &st).unwrap();
    // 32 windows of three coins
    assert!(rows.len() >= 90, "{}", rows.len());
    let both = rows.iter().filter(|r| r.status == "both").count();
    assert!(both > 5, "{both}");
    for r in &rows {
        assert!(r.status == "both" || r.status == "none", "{r:?}");
        assert!(r.status == "none" || (r.side_eq && r.rank_eq && r.exact_eq && r.fast_eq), "{r:?}");
        assert_eq!((r.bbo_live_only, r.bbo_archive_only), (0, 0), "{r:?}");
        assert!(r.dt_trig_ms.is_none_or(|d| d == 0.0) && r.dpx.is_none_or(|d| d == 0.0), "{r:?}");
    }
    let same: u32 = rows.iter().map(|r| r.bbo_same).sum();
    assert!(same > 1_000, "{same}");
    println!("bbo rows matched: {same}; both {both}; rows {}", rows.len());
    let text = reconcile_summary(&rows);
    assert!(text.contains(&format!("вместе {both}")) && text.contains("только живые 0") && text.contains("только архив 0"), "{text}");
}

#[test]
fn reconcile_names_a_changed_decision_and_a_missing_signal() {
    let Some(root) = root() else { return };
    let st = strategy(&root);
    let mut records = journal_records(&root, &st);
    let counted: Vec<usize> = (0..records.len()).filter(|&i| records[i].0 == "signal" && records[i].1["counted"] == true).collect();
    assert!(counted.len() >= 2);
    // the live bot decided otherwise on one, and missed another
    let (changed, gone) = (counted[0], counted[1]);
    let flip = if records[changed].1["decision"] == "вход" { ("пропуск", "z < 1.25") } else { ("вход", "вход") };
    records[changed].1["decision"] = json!(flip.0);
    records[changed].1["reason"] = json!(flip.1);
    let (s, coin) = (records[gone].1["s"].as_i64().unwrap(), records[gone].1["coin"].as_str().unwrap().to_string());
    records.remove(gone);
    // and a book row the archive does not have; in another window, a row the journal has twice
    let mut books = records.iter_mut().filter(|(k, r)| k == "bbo" && r["rows"].as_array().is_some_and(|a| !a.is_empty()));
    let bbo = books.next().unwrap();
    bbo.1["rows"][0][1] = json!(0.123);
    let twice = books.next().unwrap();
    let row = twice.1["rows"][0].clone();
    twice.1["rows"].as_array_mut().unwrap().push(row);
    let dir = tempfile::tempdir().unwrap();
    let j = write(dir.path(), &records);
    let rows = reconcile(&j, &root, &st).unwrap();
    let decided = rows.iter().filter(|r| r.status == "both" && !r.exact_eq).count();
    assert_eq!(decided, 1);
    let missing: Vec<_> = rows.iter().filter(|r| r.status == "archive_only").collect();
    assert_eq!(missing.len(), 1);
    assert_eq!((missing[0].s, missing[0].coin.as_str()), (s, coin.as_str()));
    let mut odd: Vec<(u32, u32)> = rows.iter().map(|r| (r.bbo_live_only, r.bbo_archive_only)).filter(|p| *p != (0, 0)).collect();
    odd.sort_unstable();
    assert_eq!(odd, [(1, 0), (1, 1)]);
    let text = reconcile_summary(&rows);
    assert!(text.contains("только архив 1"), "{text}");
}
