//! `v21 reconcile` (spec §5.5): the live windows of the journal against the archive, replayed by the
//! code of milestone 1 — the port of `paper/reconcile.py`. For each window the archive has and each
//! coin: the signals, sides, numbers and both decisions, the differences of t_trig, price and z, and
//! the book rows of 835–871 s. Windows near a venue incident (`data/INCIDENTS.json`) are marked.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;
use v21_core::closes::ArrayCloses;
use v21_core::decision::{ENTER, SKIP};
use v21_core::params::Strategy;
use v21_core::window::{parse_utc_ms, Coin, Side};
use v21_replay::archive::{bbo_files, load_closes, load_windows, read_bbo_file, Incidents, Period, WindowMeta};
use v21_replay::run::{replay_with, SignalRow};

use crate::cold::csv::write_csv;
use crate::shared::utc::fmt_utc;

const HOUR_MS: i64 = 3_600_000;
/// The book rows compared: these ms of the window, both ends in.
const BBO_FROM_MS: i64 = 835_000;
const BBO_TO_MS: i64 = 871_000;

/// A window and a coin, the journal against the archive.
#[derive(Clone, Debug, PartialEq)]
pub struct ReconcileRow {
    pub s: i64,
    pub coin: Coin,
    /// `both`, `live_only`, `archive_only`, or `none` (no signal on either side).
    pub status: &'static str,
    pub side_eq: bool,
    pub rank_eq: bool,
    /// The exact decision of v2.1 (and its reason) is the archive's.
    pub exact_eq: bool,
    /// The fast decision is the archive's decision of v2.1.
    pub fast_eq: bool,
    /// Live minus archive.
    pub dt_trig_ms: Option<f64>,
    pub dpx: Option<f64>,
    pub dz: Option<f64>,
    /// Book rows of 835–871 s in both, in the journal only, in the archive only.
    pub bbo_same: u32,
    pub bbo_live_only: u32,
    pub bbo_archive_only: u32,
    pub incident: bool,
}

/// A row of the bbo table as it can be compared: time and the bits of the prices.
type Row = (i64, u64, u64);

/// The hour of a book file, from its name (`2026-10-15T09.parquet`).
fn hour_ms(path: &Path) -> Option<i64> {
    let name = path.file_name()?.to_str()?;
    parse_utc_ms(&format!("{}:00Z", name.get(..13)?))
}

/// How many rows of `a` and `b` match, one for one, and how many are left in each.
fn compare(a: &[Row], b: &[Row]) -> (u32, u32, u32) {
    let mut left: HashMap<Row, u32> = HashMap::new();
    for r in b {
        *left.entry(*r).or_default() += 1;
    }
    let mut same = 0;
    for r in a {
        if let Some(n) = left.get_mut(r).filter(|n| **n > 0) {
            *n -= 1;
            same += 1;
        }
    }
    (same, a.len() as u32 - same, b.len() as u32 - same)
}

fn decision_of(row: &SignalRow, strategy: &Strategy) -> (&'static str, String) {
    (if row.v21.trade { ENTER } else { SKIP }, row.v21.reason.label(&strategy.v21))
}

/// The windows of the journal that the archive at `root` has, against it.
pub fn reconcile(journal: &Journal, root: &Path, strategy: &Strategy) -> Result<Vec<ReconcileRow>> {
    let recs = journal.all()?;
    let key = |r: &Value| Some((r["s"].as_i64()?, Coin::parse(r["coin"].as_str()?)?));
    let live: BTreeMap<(i64, Coin), &Value> = recs.iter().filter(|r| r["kind"] == "signal" && r["counted"] == true).filter_map(|r| Some((key(r)?, r))).collect();
    let mut live_bbo: HashMap<(i64, Coin), Vec<Row>> = HashMap::new();
    for r in recs.iter().filter(|r| r["kind"] == "bbo") {
        let Some(k) = key(r) else { continue };
        let rows = r["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
        live_bbo.entry(k).or_default().extend(rows.iter().filter_map(|x| Some((x[0].as_i64()?, x[1].as_f64()?.to_bits(), x[2].as_f64()?.to_bits()))));
    }
    let starts: BTreeSet<i64> = live.keys().chain(live_bbo.keys()).map(|(s, _)| *s).collect();
    if starts.is_empty() {
        return Ok(vec![]);
    }
    let coins = strategy.v2.watch_coins.clone();
    let metas: Vec<WindowMeta> = load_windows(root, Period::Main, &coins, None)?.into_iter().filter(|w| starts.contains(&(w.s0_ms / 1000))).collect();
    let (Some(lo), Some(hi)) = (metas.iter().map(|w| w.s0_ms).min(), metas.iter().map(|w| w.s0_ms).max()) else { return Ok(vec![]) };
    let (from, to) = ((lo.div_euclid(HOUR_MS) - 1) * HOUR_MS, ((hi + 900_000 - 1).div_euclid(HOUR_MS) + 1) * HOUR_MS);
    let files: Vec<PathBuf> = bbo_files(root, Period::Main)?.into_iter().filter(|f| hour_ms(f).is_some_and(|h| (from..=to).contains(&h))).collect();
    let closes: HashMap<Coin, ArrayCloses> = coins.iter().map(|&c| Ok((c, load_closes(root, Period::Main, c)?))).collect::<Result<_>>()?;
    let want = replay_with(&files, &metas, &closes, strategy)?;
    let archive: HashMap<(i64, Coin), &SignalRow> = want.iter().map(|w| ((w.s0_ms / 1000, w.coin), w)).collect();

    // the archive's book rows of 835–871 s of each window's Up tokens
    let by_token: HashMap<[u8; 32], (i64, Coin)> = metas.iter().map(|w| (w.up, (w.s0_ms, w.coin))).collect();
    let tokens: HashSet<[u8; 32]> = by_token.keys().copied().collect();
    let mut archive_bbo: HashMap<(i64, Coin), Vec<Row>> = HashMap::new();
    for f in &files {
        for r in read_bbo_file(f, &tokens)? {
            let (s0, coin) = by_token[&r.token];
            if (s0 + BBO_FROM_MS..=s0 + BBO_TO_MS).contains(&r.ts_ms) {
                archive_bbo.entry((s0 / 1000, coin)).or_default().push((r.ts_ms, r.bid.to_bits(), r.ask.to_bits()));
            }
        }
    }
    let incidents = Incidents::load(&root.join("data/INCIDENTS.json"), strategy.v2.incident_buffer_min)?;

    let keys: BTreeSet<(i64, Coin)> = metas.iter().map(|w| (w.s0_ms / 1000, w.coin)).collect();
    Ok(keys
        .into_iter()
        .map(|k| {
            let (l, a) = (live.get(&k), archive.get(&k));
            let status = match (l, a) {
                (Some(_), Some(_)) => "both",
                (Some(_), None) => "live_only",
                (None, Some(_)) => "archive_only",
                (None, None) => "none",
            };
            let pair = l.zip(a);
            let side = |s: Side| if s == Side::Up { "Up" } else { "Down" };
            let (same, live_only, archive_only) = compare(live_bbo.get(&k).map(Vec::as_slice).unwrap_or_default(), archive_bbo.get(&k).map(Vec::as_slice).unwrap_or_default());
            ReconcileRow {
                s: k.0,
                coin: k.1,
                status,
                side_eq: pair.is_some_and(|(l, a)| l["side"] == side(a.side)),
                rank_eq: pair.is_some_and(|(l, a)| l["rank"].as_u64() == Some(u64::from(a.rank))),
                exact_eq: pair.is_some_and(|(l, a)| {
                    let (d, r) = decision_of(a, strategy);
                    l["decision"] == d && l["reason"] == r.as_str()
                }),
                fast_eq: pair.is_some_and(|(l, a)| l["fast_decision"] == decision_of(a, strategy).0),
                dt_trig_ms: pair.and_then(|(l, a)| Some((l["t_trig_s"].as_f64()? - a.t_trig) * 1000.0)),
                dpx: pair.and_then(|(l, a)| Some(l["px"].as_f64()? - a.px)),
                dz: pair.and_then(|(l, a)| Some(l["z"].as_f64()? - a.z.z)),
                bbo_same: same,
                bbo_live_only: live_only,
                bbo_archive_only: archive_only,
                incident: incidents.overlaps(k.0 * 1000),
            }
        })
        .collect())
}

/// The rows' summary for the screen.
pub fn reconcile_summary(rows: &[ReconcileRow]) -> String {
    if rows.is_empty() {
        return "архив ещё не покрывает окна журнала: сверить нечего".to_string();
    }
    let count = |st: &str| rows.iter().filter(|r| r.status == st).count();
    let windows: BTreeSet<i64> = rows.iter().map(|r| r.s).collect();
    let both: Vec<&ReconcileRow> = rows.iter().filter(|r| r.status == "both").collect();
    let n = both.len();
    let eq = |f: fn(&ReconcileRow) -> bool| both.iter().filter(|r| f(r)).count();
    let mut lines = vec![format!(
        "окон {} · сигналы: вместе {n}, только живые {}, только архив {}, без сигнала {} · в окнах сбоев {}",
        windows.len(),
        count("live_only"),
        count("archive_only"),
        count("none"),
        rows.iter().filter(|r| r.incident).map(|r| r.s).collect::<BTreeSet<_>>().len()
    )];
    if n > 0 {
        let mut dt: Vec<f64> = both.iter().filter_map(|r| r.dt_trig_ms).collect();
        dt.sort_by(f64::total_cmp);
        let max = |v: Vec<f64>| v.into_iter().map(f64::abs).fold(0.0, f64::max);
        lines.push(format!(
            "та же сторона {}/{n}, тот же номер {}/{n}, точное решение {}/{n}, быстрое решение {}/{n}",
            eq(|r| r.side_eq),
            eq(|r| r.rank_eq),
            eq(|r| r.exact_eq),
            eq(|r| r.fast_eq)
        ));
        lines.push(format!(
            "t_trig живой − архив: медиана {:+.0} мс, max |{:.0}| мс · px max |{:.3}| · z max |{:.2e}|",
            dt.get(dt.len() / 2).copied().unwrap_or(0.0),
            max(dt.clone()),
            max(both.iter().filter_map(|r| r.dpx).collect()),
            max(both.iter().filter_map(|r| r.dz).collect())
        ));
    }
    let sum = |f: fn(&ReconcileRow) -> u32| rows.iter().map(f).sum::<u32>();
    lines.push(format!(
        "события стакана 835–871 с: совпали {}, только живые {}, только архив {}",
        sum(|r| r.bbo_same),
        sum(|r| r.bbo_live_only),
        sum(|r| r.bbo_archive_only)
    ));
    lines.join("\n")
}

/// The rows as `reconcile_YYYYMMDD.csv`.
pub fn write_reconcile_csv(rows: &[ReconcileRow], path: &Path) -> std::io::Result<()> {
    let opt = |x: Option<f64>| x.map(|v| v.to_string()).unwrap_or_default();
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                fmt_utc(r.s),
                r.coin.as_str().to_string(),
                r.status.to_string(),
                r.side_eq.to_string(),
                r.rank_eq.to_string(),
                r.exact_eq.to_string(),
                r.fast_eq.to_string(),
                opt(r.dt_trig_ms),
                opt(r.dpx),
                opt(r.dz),
                r.bbo_same.to_string(),
                r.bbo_live_only.to_string(),
                r.bbo_archive_only.to_string(),
                r.incident.to_string(),
            ]
        })
        .collect();
    let columns = [
        "window_start_utc",
        "coin",
        "status",
        "side_eq",
        "rank_eq",
        "exact_eq",
        "fast_eq",
        "dt_trig_ms",
        "dpx",
        "dz",
        "bbo_same",
        "bbo_live_only",
        "bbo_archive_only",
        "incident",
    ];
    write_csv(path, &columns, &cells)
}

use crate::cold::journal::Journal;
