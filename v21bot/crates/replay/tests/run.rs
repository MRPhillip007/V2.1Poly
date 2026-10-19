//! The replay loop: one token's signal from its rows, the order of the backtest's dictionary of
//! signals, the drop of signals without Binance data, and the real archive against the backtest.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int64Type};
use arrow_array::{ArrayRef, BinaryArray, Decimal128Array, RecordBatch, TimestampMillisecondArray};
use arrow_schema::{Field, Schema};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use v21_core::closes::ArrayCloses;
use v21_core::detector::Check;
use v21_core::params::{load_strategy, Strategy};
use v21_core::window::{Coin, Side};
use v21_core::zscore::z_score;
use v21_replay::archive::{bbo_files, load_closes, load_windows, BboRow, Incidents, Period, WindowMeta};
use v21_replay::run::{replay_with, signal_for_token};

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");
/// 2026-10-02 03:15 UTC, in ms.
const S0: i64 = 1_790_910_900_000;
const ETH_UP: [u8; 32] = [1; 32];
const BTC_UP: [u8; 32] = [2; 32];
const SOL_UP: [u8; 32] = [3; 32];

fn strategy() -> Strategy {
    load_strategy(V2, V21).unwrap()
}

fn row(t_ms: i64, bid: f64, ask: f64) -> BboRow {
    BboRow { ts_ms: S0 + t_ms, token: BTC_UP, bid, ask }
}

#[test]
fn no_row_before_t0_gives_no_signal() {
    let rows = [row(841_000, 0.10, 0.75), row(842_000, 0.10, 0.75)];
    assert_eq!(signal_for_token(&rows, Coin::Btc, S0, &strategy().v2), None);
}

#[test]
fn pre840_state_in_the_band_signals_at_840() {
    let rows = [row(839_000, 0.10, 0.75), row(841_000, 0.10, 0.75)];
    assert_eq!(
        signal_for_token(&rows, Coin::Btc, S0, &strategy().v2),
        Some(Check { coin: Coin::Btc, t_trig: 840.0, side: Side::Up, ask_at_trig: 0.75, px: 0.75, ok: true })
    );
    // with no row after it the flush past 870.3 s settles it
    assert_eq!(signal_for_token(&rows[..1], Coin::Btc, S0, &strategy().v2).map(|c| c.t_trig), Some(840.0));
}

#[test]
fn failed_then_ok_check_returns_the_ok_one() {
    let rows = [
        row(830_000, 0.45, 0.50),
        row(841_000, 0.10, 0.85),
        row(841_200, 0.10, 0.90),
        row(841_400, 0.10, 0.86),
        row(841_800, 0.10, 0.60),
    ];
    assert_eq!(
        signal_for_token(&rows, Coin::Btc, S0, &strategy().v2),
        Some(Check { coin: Coin::Btc, t_trig: 841.4, side: Side::Up, ask_at_trig: 0.86, px: 0.86, ok: true })
    );
}

#[test]
fn rows_after_t1_plus_lat_are_not_read() {
    // a band hit at 870.3 s is past the window the backtest reads
    let rows = [row(830_000, 0.45, 0.50), row(870_301, 0.10, 0.75), row(871_000, 0.10, 0.75)];
    assert_eq!(signal_for_token(&rows, Coin::Btc, S0, &strategy().v2), None);
}

// ---------------------------------------------------------------- the whole loop on small files

fn write_bbo(path: &Path, rows: &[(i64, [u8; 32], f64, f64)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let dec = |v: Vec<i128>| -> ArrayRef { Arc::new(Decimal128Array::from(v).with_precision_and_scale(9, 4).unwrap()) };
    let cols: Vec<(&str, ArrayRef)> = vec![
        ("timestamp", Arc::new(TimestampMillisecondArray::from(rows.iter().map(|r| r.0).collect::<Vec<_>>()).with_timezone("UTC"))),
        ("asset_id", Arc::new(BinaryArray::from_iter_values(rows.iter().map(|r| r.1)))),
        ("best_bid", dec(rows.iter().map(|r| (r.2 * 10_000.0).round() as i128).collect())),
        ("best_ask", dec(rows.iter().map(|r| (r.3 * 10_000.0).round() as i128).collect())),
    ];
    let fields: Vec<Field> = cols.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect();
    let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), cols.into_iter().map(|(_, a)| a).collect()).unwrap();
    let mut w = ArrowWriter::try_new(File::create(path).unwrap(), batch.schema(), None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn window(coin: Coin, up: [u8; 32]) -> WindowMeta {
    WindowMeta { slug: format!("{}-updown-15m-{}", coin.as_str(), S0 / 1000), coin, s0_ms: S0, up, p_up_final: 1.0 }
}

/// Closes with movement around the window, so that z is a number.
fn closes_around(first: i64, last: i64) -> ArrayCloses {
    ArrayCloses::new(first, (first..=last).map(|q| 100.0 + ((q % 7) as f64) * 0.01).collect())
}

fn covered() -> HashMap<Coin, ArrayCloses> {
    let s = S0 / 1000;
    [Coin::Btc, Coin::Eth, Coin::Sol].into_iter().map(|c| (c, closes_around(s - 1000, s + 1000))).collect()
}

#[test]
fn ties_at_840_follow_the_backtest_insertion_order() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("2026-10-02T03.parquet");
    // both in the band at 840.000; the file groups tokens by id, so ETH (the smaller id) comes first
    write_bbo(&f, &[(S0 + 839_000, BTC_UP, 0.10, 0.75), (S0 + 839_500, ETH_UP, 0.20, 0.80)]);
    let windows = [window(Coin::Btc, BTC_UP), window(Coin::Eth, ETH_UP)];
    let rows = replay_with(&[f], &windows, &covered(), &strategy()).unwrap();
    let got: Vec<(Coin, f64, u32)> = rows.iter().map(|r| (r.coin, r.t_trig, r.rank)).collect();
    assert_eq!(got, vec![(Coin::Eth, 840.0, 1), (Coin::Btc, 840.0, 2)]);
}

#[test]
fn a_later_file_replaces_the_signal_but_keeps_the_first_position() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("2026-10-02T02.parquet");
    let b = dir.path().join("2026-10-02T03.parquet");
    write_bbo(&a, &[(S0 + 839_000, BTC_UP, 0.10, 0.75)]);
    write_bbo(&b, &[(S0 + 839_000, BTC_UP, 0.10, 0.80), (S0 + 839_500, ETH_UP, 0.20, 0.79)]);
    let windows = [window(Coin::Btc, BTC_UP), window(Coin::Eth, ETH_UP)];
    let rows = replay_with(&[a, b], &windows, &covered(), &strategy()).unwrap();
    let got: Vec<(Coin, f64, u32)> = rows.iter().map(|r| (r.coin, r.px, r.rank)).collect();
    // BTC keeps the first place of the dictionary and takes file B's check
    assert_eq!(got, vec![(Coin::Btc, 0.80, 1), (Coin::Eth, 0.79, 2)]);
}

#[test]
fn an_earlier_files_signal_survives_when_the_windows_own_hour_has_none() {
    // `res[i] = ...` only overwrites: a later file without rows of the window leaves file A's signal alone
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("2026-10-02T02.parquet");
    let b = dir.path().join("2026-10-02T03.parquet");
    write_bbo(&a, &[(S0 + 839_000, BTC_UP, 0.10, 0.75)]);
    write_bbo(&b, &[(S0 + 839_500, ETH_UP, 0.20, 0.79)]);
    let windows = [window(Coin::Btc, BTC_UP), window(Coin::Eth, ETH_UP)];
    let rows = replay_with(&[a, b], &windows, &covered(), &strategy()).unwrap();
    let got: Vec<(Coin, f64, u32)> = rows.iter().map(|r| (r.coin, r.px, r.rank)).collect();
    assert_eq!(got, vec![(Coin::Btc, 0.75, 1), (Coin::Eth, 0.79, 2)]);
}

#[test]
fn a_coin_missing_from_closes_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("2026-10-02T03.parquet");
    write_bbo(&f, &[(S0 + 839_000, BTC_UP, 0.10, 0.75), (S0 + 839_500, SOL_UP, 0.20, 0.79)]);
    let mut closes = covered();
    closes.remove(&Coin::Sol);
    let windows = [window(Coin::Btc, BTC_UP), window(Coin::Sol, SOL_UP)];
    let err = replay_with(&[f], &windows, &closes, &strategy()).unwrap_err();
    assert!(format!("{err:#}").contains("no Binance closes for sol"), "{err:#}");
}

#[test]
fn signals_without_binance_coverage_are_dropped_before_ranking() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("2026-10-02T03.parquet");
    write_bbo(&f, &[(S0 + 830_000, BTC_UP, 0.45, 0.50), (S0 + 841_000, BTC_UP, 0.10, 0.75),
                    (S0 + 830_000, ETH_UP, 0.45, 0.50), (S0 + 842_000, ETH_UP, 0.10, 0.75)]);
    let s = S0 / 1000;
    let mut closes = covered();
    closes.insert(Coin::Btc, closes_around(s - 300, s + 1000)); // does not reach s - 400
    let rows = replay_with(&[f], &[window(Coin::Btc, BTC_UP), window(Coin::Eth, ETH_UP)], &closes, &strategy()).unwrap();
    let got: Vec<(Coin, u32)> = rows.iter().map(|r| (r.coin, r.rank)).collect();
    assert_eq!(got, vec![(Coin::Eth, 1)]);
}

#[test]
fn signals_whose_z_is_nan_are_dropped_before_ranking() {
    // closes flat over the sigma window give sigma 0 and z NaN: the backtest's dropna(subset=['z'])
    // removes the signal before rank(), so the next coin of the window is number 1, not 2
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("2026-10-02T03.parquet");
    write_bbo(&f, &[(S0 + 830_000, BTC_UP, 0.45, 0.50), (S0 + 841_000, BTC_UP, 0.10, 0.75),
                    (S0 + 830_000, ETH_UP, 0.45, 0.50), (S0 + 842_000, ETH_UP, 0.10, 0.75)]);
    let s = S0 / 1000;
    let mut closes = covered();
    closes.insert(Coin::Btc, ArrayCloses::new(s - 1000, vec![100.0; 2001]));
    let rows = replay_with(&[f], &[window(Coin::Btc, BTC_UP), window(Coin::Eth, ETH_UP)], &closes, &strategy()).unwrap();
    let got: Vec<(Coin, u32)> = rows.iter().map(|r| (r.coin, r.rank)).collect();
    assert_eq!(got, vec![(Coin::Eth, 1)]);
}

// ---------------------------------------------------------------- the real archive

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
fn golden_z_on_the_real_archive() {
    let Some(root) = root() else { return };
    let p = strategy().v2;
    for (coin, s, t_trig, z, cushion_bp, sigma_bp, h) in [
        (Coin::Btc, 1_792_029_600, 840.0, 3.8125930024497108, 10.732760574468532, 0.6374450583311702, 19.502777777777776),
        (Coin::Eth, 1_792_115_100, 841.716, 1.8981123947449212, 3.8567995888642668, 0.4719533673912547, 18.535833333333333),
    ] {
        let closes = load_closes(&root, Period::Main, coin).unwrap();
        let got = z_score(&closes, s, t_trig, Side::Down, &p).unwrap();
        for (name, g, w) in [("z", got.z, z), ("cushion_bp", got.cushion_bp, cushion_bp), ("sigma_bp", got.sigma_bp, sigma_bp), ("h", got.h, h)] {
            assert!((g - w).abs() <= 1e-9, "{coin:?} {name}: got {g:?}, want {w:?}");
        }
    }
}

/// (slug, s0, t_trig, side, px, z, rank) of the backtest's signals.
fn reference_signals(path: &Path) -> Vec<(String, i64, f64, String, f64, f64, i64)> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    let mut out = vec![];
    for batch in builder.build().unwrap() {
        let b = batch.unwrap();
        let col = |n: &str| b.column_by_name(n).unwrap().clone();
        let (slug, side) = (col("slug"), col("side"));
        let (slug, side) = (slug.as_string::<i64>(), side.as_string::<i64>());
        let (s0, rank) = (col("s0"), col("rank_in_window"));
        let (t, px, z) = (col("t_trig"), col("px"), col("z"));
        for i in 0..b.num_rows() {
            out.push((
                slug.value(i).to_string(),
                s0.as_primitive::<Int64Type>().value(i),
                t.as_primitive::<Float64Type>().value(i),
                side.value(i).to_string(),
                px.as_primitive::<Float64Type>().value(i),
                z.as_primitive::<Float64Type>().value(i),
                rank.as_primitive::<Int64Type>().value(i),
            ));
        }
    }
    out
}

#[test]
fn one_day_matches_the_reference() {
    let Some(root) = root() else { return };
    let s = strategy();
    let (day0, day1) = (1_792_022_400_000, 1_792_108_800_000); // 2026-10-15 UTC
    let files: Vec<PathBuf> = bbo_files(&root, Period::Main)
        .unwrap()
        .into_iter()
        .filter(|f| {
            let name = f.file_name().unwrap().to_string_lossy().to_string();
            ("2026-10-14T23.parquet"..="2026-10-16T00.parquet").contains(&name.as_str())
        })
        .collect();
    assert_eq!(files.len(), 26);
    let inc = Incidents::load(&root.join("data/INCIDENTS.json"), 30).unwrap();
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let windows: Vec<WindowMeta> = load_windows(&root, Period::Main, &coins, Some(&inc))
        .unwrap()
        .into_iter()
        .filter(|w| (day0..day1).contains(&w.s0_ms))
        .collect();
    let closes: HashMap<Coin, ArrayCloses> =
        coins.iter().map(|&c| (c, load_closes(&root, Period::Main, c).unwrap())).collect();
    let got = replay_with(&files, &windows, &closes, &s).unwrap();
    let mut want: Vec<_> = reference_signals(&root.join("data/ruslan_rule_all_triggers.parquet"))
        .into_iter()
        .filter(|r| (day0..day1).contains(&r.1))
        .collect();
    want.sort_by_key(|r| (r.1, r.6));
    assert!(want.len() > 20, "{}", want.len());
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(&want) {
        let side = match g.side {
            Side::Up => "Up",
            Side::Down => "Down",
        };
        assert_eq!((g.slug.as_str(), g.s0_ms, g.t_trig, side, g.px, i64::from(g.rank)),
                   (w.0.as_str(), w.1, w.2, w.3.as_str(), w.4, w.6), "{}", w.0);
        assert!((g.z.z - w.5).abs() < 1e-9, "{}: z {} vs {}", w.0, g.z.z, w.5);
    }
}
