//! Reading the archive: both schemas of the book files, the market list, the incident list and the
//! Binance seconds; synthetic files are written with the parquet writer, the real archive is read
//! when `V21_ROOT` points at it.

use std::collections::HashSet;
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;

use arrow_array::{
    ArrayRef, BinaryArray, Decimal128Array, FixedSizeBinaryArray, Float64Array, Int64Array, RecordBatch,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray,
};
use arrow_schema::{Field, Schema};
use parquet::arrow::ArrowWriter;
use v21_core::closes::Closes;
use v21_core::window::Coin;
use v21_replay::archive::{
    bbo_files, load_closes, load_windows, read_bbo_file, BboRow, Incidents, Period, MAIN_START_MS,
};

const T1: [u8; 32] = [1; 32];
const T2: [u8; 32] = [2; 32];
const T3: [u8; 32] = [3; 32];

fn write(path: &Path, columns: Vec<(&str, ArrayRef)>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let fields: Vec<Field> = columns.iter().map(|(n, a)| Field::new(*n, a.data_type().clone(), true)).collect();
    let batch = RecordBatch::try_new(
        Arc::new(Schema::new(fields)),
        columns.into_iter().map(|(_, a)| a).collect(),
    )
    .unwrap();
    let mut w = ArrowWriter::try_new(File::create(path).unwrap(), batch.schema(), None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn ms(v: Vec<i64>) -> ArrayRef {
    Arc::new(TimestampMillisecondArray::from(v).with_timezone("UTC"))
}

fn dec(v: Vec<Option<i128>>) -> ArrayRef {
    Arc::new(Decimal128Array::from(v).with_precision_and_scale(9, 4).unwrap())
}

/// A book row: (timestamp ms, asset id, bid and ask in ten-thousandths of a dollar).
type BookRow = (i64, [u8; 32], Option<i128>, Option<i128>);

/// A book file in the schema of `data/bbo`: binary asset ids, Decimal(9, 4) prices.
fn main_bbo(path: &Path, rows: &[BookRow]) {
    write(
        path,
        vec![
            ("timestamp", ms(rows.iter().map(|r| r.0).collect())),
            ("asset_id", Arc::new(BinaryArray::from_iter_values(rows.iter().map(|r| r.1)))),
            ("best_bid", dec(rows.iter().map(|r| r.2).collect())),
            ("best_ask", dec(rows.iter().map(|r| r.3).collect())),
        ],
    );
}

fn tokens(t: &[[u8; 32]]) -> HashSet<[u8; 32]> {
    t.iter().copied().collect()
}

#[test]
fn reads_main_schema_decimal_prices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("2026-10-02T03.parquet");
    main_bbo(&path, &[(1000, T1, Some(1900), Some(8100)), (2000, T1, None, Some(5000))]);
    let rows = read_bbo_file(&path, &tokens(&[T1])).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], BboRow { ts_ms: 1000, token: T1, bid: 0.19, ask: 0.81 });
    assert!(rows[1].bid.is_nan());
    assert_eq!((rows[1].ts_ms, rows[1].ask), (2000, 0.5));
}

#[test]
fn decimal_cast_matches_pyarrow() {
    // pyarrow 25 casts Decimal(9, 4) to float64 as v * 1e-4 (checked on all 10 001 values in [0, 1] on
    // 2026-10-18), and at a 4th-decimal tie the backtest's np.round(x, 3) then rounds up
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ties.parquet");
    main_bbo(&path, &[(1, T1, Some(7000), Some(8805)), (2, T1, Some(1900), Some(7525))]);
    let rows = read_bbo_file(&path, &tokens(&[T1])).unwrap();
    assert_eq!((rows[0].bid, rows[0].ask), (0.7000000000000001, 0.8805000000000001));
    assert_eq!((rows[1].bid, rows[1].ask), (0.19, 0.7525000000000001));
    assert_eq!((v21_core::numpy::round3(rows[0].ask), v21_core::numpy::round3(rows[1].ask)), (0.881, 0.753));
}

#[test]
fn reads_pre_schema_double_prices() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("2026-08-07T00.parquet");
    write(
        &path,
        vec![
            ("timestamp", ms(vec![1000, 2000])),
            ("asset_id", Arc::new(FixedSizeBinaryArray::try_from_iter([T1, T2].into_iter()).unwrap())),
            ("best_bid", Arc::new(Float64Array::from(vec![0.19, 0.2]))),
            ("best_ask", Arc::new(Float64Array::from(vec![0.81, 0.82]))),
        ],
    );
    let rows = read_bbo_file(&path, &tokens(&[T1, T2])).unwrap();
    assert_eq!(
        rows,
        vec![
            BboRow { ts_ms: 1000, token: T1, bid: 0.19, ask: 0.81 },
            BboRow { ts_ms: 2000, token: T2, bid: 0.2, ask: 0.82 },
        ]
    );
}

#[test]
fn keeps_file_order_and_filters_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.parquet");
    main_bbo(
        &path,
        &[
            (5, T1, Some(1), Some(10)),
            (3, T2, Some(2), Some(20)),
            (4, T3, Some(3), Some(30)),
            (1, T1, Some(4), Some(40)),
            (2, T3, Some(5), Some(50)),
        ],
    );
    let got: Vec<(i64, [u8; 32])> =
        read_bbo_file(&path, &tokens(&[T1, T3])).unwrap().iter().map(|r| (r.ts_ms, r.token)).collect();
    assert_eq!(got, vec![(5, T1), (4, T3), (1, T1), (2, T3)]);
}

#[test]
fn empty_bbo_file_gives_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.parquet");
    main_bbo(&path, &[]);
    assert!(read_bbo_file(&path, &tokens(&[T1])).unwrap().is_empty());
}

#[test]
fn missing_column_names_file_and_column() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("no_ask.parquet");
    write(
        &path,
        vec![
            ("timestamp", ms(vec![1])),
            ("asset_id", Arc::new(BinaryArray::from_iter_values([T1]))),
            ("best_bid", dec(vec![Some(1)])),
        ],
    );
    let err = format!("{:#}", read_bbo_file(&path, &tokens(&[T1])).unwrap_err());
    assert!(err.contains("best_ask") && err.contains("no_ask.parquet"), "{err}");
}

fn hex(t: [u8; 32]) -> String {
    t.iter().map(|b| format!("{b:02x}")).collect()
}

/// Window rows: (slug, coin, start in ms, up token, p_up_final).
type MetaRow = (&'static str, &'static str, i64, [u8; 32], Option<f64>);

fn meta(path: &Path, rows: &[MetaRow], micros: bool) {
    let start: ArrayRef = if micros {
        Arc::new(TimestampMicrosecondArray::from(rows.iter().map(|r| r.2 * 1000).collect::<Vec<_>>()).with_timezone("UTC"))
    } else {
        ms(rows.iter().map(|r| r.2).collect())
    };
    write(
        path,
        vec![
            ("slug", Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.0)))),
            ("coin", Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.1)))),
            ("start", start),
            ("up", Arc::new(StringArray::from_iter_values(rows.iter().map(|r| hex(r.3))))),
            ("p_up_final", Arc::new(Float64Array::from(rows.iter().map(|r| r.4).collect::<Vec<_>>()))),
        ],
    );
}

#[test]
fn meta_start_in_us_and_ms_give_the_same_s0() {
    let dir = tempfile::tempdir().unwrap();
    let w = 1_790_910_900_000;
    let rows: Vec<MetaRow> = vec![
        ("btc-updown-15m-1790910900", "btc", w, T1, Some(1.0)),
        ("eth-updown-15m-1790910900", "eth", w, T2, Some(0.5)), // not settled 0/1: dropped
        ("xrp-updown-15m-1790910900", "xrp", w, T3, Some(0.0)), // not a coin of the strategy
        ("sol-updown-15m-1790910900", "sol", w, T3, None),      // no outcome: dropped
        ("btc-updown-15m-1786924800", "btc", 1_786_924_800_000, T2, Some(0.0)), // before 18.08 06:00
    ];
    meta(&dir.path().join("data/meta_15m.parquet"), &rows, true);
    meta(&dir.path().join("data/meta_15m_pre.parquet"), &rows, false);
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let main = load_windows(dir.path(), Period::Main, &coins, None).unwrap();
    assert_eq!(main.len(), 1);
    assert_eq!((main[0].slug.as_str(), main[0].coin, main[0].s0_ms, main[0].up, main[0].p_up_final),
               ("btc-updown-15m-1790910900", Coin::Btc, w, T1, 1.0));
    assert_eq!(MAIN_START_MS, v21_core::window::parse_utc_ms("2026-08-18T06:00Z").unwrap());
    let pre = load_windows(dir.path(), Period::Pre, &coins, None).unwrap();
    assert_eq!(pre.iter().map(|m| m.s0_ms).collect::<Vec<_>>(), vec![w, 1_786_924_800_000]);
}

fn bn1s(path: &Path, t: Vec<i64>, c: Vec<Option<f64>>) {
    write(path, vec![("t", Arc::new(Int64Array::from(t))), ("c", Arc::new(Float64Array::from(c)))]);
}

#[test]
fn closes_reject_hole_null_and_empty() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data/bn1s/BTCUSDT.parquet");
    let t0 = 1_786_924_800;
    bn1s(&file, (0..5).map(|i| (t0 + i) * 1000).collect(), (0..5).map(|i| Some(10.0 + i as f64)).collect());
    let a = load_closes(dir.path(), Period::Main, Coin::Btc).unwrap();
    assert_eq!((a.t0(), a.len(), a.close(t0 + 4)), (t0, 5, Some(14.0)));
    for (t, c) in [
        (vec![t0 * 1000, (t0 + 2) * 1000], vec![Some(1.0), Some(2.0)]), // a hole
        (vec![t0 * 1000, (t0 + 1) * 1000], vec![Some(1.0), None]),     // a missing close
        (vec![], vec![]),                                              // empty
    ] {
        bn1s(&file, t, c);
        let err = format!("{:#}", load_closes(dir.path(), Period::Main, Coin::Btc).unwrap_err());
        assert!(err.contains("BTCUSDT.parquet"), "{err}");
    }
}

#[test]
fn incident_buffer_edges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("INCIDENTS.json");
    fs::write(
        &path,
        r#"{"incidents": [{"id": "x", "started_utc": "2026-08-19T04:00Z", "ended_utc": "2026-08-19T05:30Z"}]}"#,
    )
    .unwrap();
    // the incident 04:00-05:30 with a 30-minute buffer is [03:30, 06:00)
    let inc = Incidents::load(&path, 30).unwrap();
    assert_eq!(inc.len(), 1);
    let at = |s: i64| s * 1000;
    assert!(!inc.overlaps(at(1_787_109_300))); // 03:15: the window ends at 03:30
    assert!(inc.overlaps(at(1_787_110_200))); // 03:30
    assert!(inc.overlaps(at(1_787_118_300))); // 05:45
    assert!(!inc.overlaps(at(1_787_119_200))); // 06:00
}

#[test]
fn real_archive_facts() {
    let Ok(root) = std::env::var("V21_ROOT") else {
        println!("skipped: V21_ROOT is not set");
        return;
    };
    let root = Path::new(&root);
    let coins = [Coin::Btc, Coin::Eth, Coin::Sol];
    let inc = Incidents::load(&root.join("data/INCIDENTS.json"), 30).unwrap();
    assert!(inc.len() >= 26);
    let main = load_windows(root, Period::Main, &coins, Some(&inc)).unwrap();
    let per = |c: Coin| main.iter().filter(|m| m.coin == c).count();
    assert!(per(Coin::Btc) >= 5577, "{}", per(Coin::Btc));
    assert_eq!((per(Coin::Btc), per(Coin::Btc)), (per(Coin::Eth), per(Coin::Sol)));
    assert_eq!(load_windows(root, Period::Pre, &coins, None).unwrap().len(), 3240);
    let files = bbo_files(root, Period::Main).unwrap();
    assert!(files.len() >= 1476);
    assert_eq!(files[0].file_name().unwrap(), "2026-08-18T06.parquet");
    assert_eq!(bbo_files(root, Period::Pre).unwrap().len(), 270);
    let btc = load_closes(root, Period::Main, Coin::Btc).unwrap();
    assert_eq!((btc.t0(), btc.close(btc.t0())), (1_786_924_800, Some(62900.0)));
    let eth = load_closes(root, Period::Main, Coin::Eth).unwrap();
    assert_eq!(eth.close(eth.t0()), Some(1876.01));
    let sol = load_closes(root, Period::Main, Coin::Sol).unwrap();
    assert_eq!(sol.close(sol.t0()), Some(74.62));
    assert_eq!((btc.len(), btc.len()), (eth.len(), sol.len()));
}
