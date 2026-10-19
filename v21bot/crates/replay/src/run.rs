//! The replay of the archive through the core, in the order of `ruslan_v2.book_triggers` and
//! `ruslan_v2.py`, so that it finds the backtest's signals, numbers and trades exactly:
//! - the hourly files are read in name order; in a file, the rows of each Up token (tokens in
//!   ascending byte order, as `groupby` takes them) give at most one signal;
//! - a window's signal from a later file replaces the earlier one but keeps the window's first
//!   position, as a key of a Python `dict` does — that position breaks ties of `t_trig`;
//! - a signal without z (no Binance seconds around the window, or sigma 0) is dropped before the
//!   numbering, as `dropna(subset=['z'])` comes before `rank`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use v21_core::closes::ArrayCloses;
use v21_core::decision::{Decision, WindowEngine};
use v21_core::detector::{Check, CoinDetector};
use v21_core::params::{Params, Strategy};
use v21_core::returns::{trade_return, win_of};
use v21_core::window::{entry_ms, Coin, Side};
use v21_core::zscore::{z_score, ZInfo};

use crate::archive::{bbo_files, load_closes, load_windows, read_bbo_file, BboRow, Incidents, Period, WindowMeta};

/// Seconds of Binance closes the backtest needs before a window start to keep its signal.
const BEFORE_S: i64 = 400;

/// One counted signal of the base rule, with its z, its number in the window and both decisions.
#[derive(Clone, Debug)]
pub struct SignalRow {
    pub slug: String,
    pub coin: Coin,
    pub s0_ms: i64,
    pub t_trig: f64,
    pub side: Side,
    pub px: f64,
    pub z: ZInfo,
    pub rank: u32,
    pub p_up_final: f64,
    pub win: f64,
    pub ret_top: f64,
    pub ret: f64,
    pub entry_ms: i64,
    pub v2: Decision,
    pub v21: Decision,
}

pub struct ReplayOutput {
    pub period: Period,
    pub signals: Vec<SignalRow>,
    pub windows: usize,
    pub files: usize,
}

/// The signal of one token in one file, as `paper/replay_test.py::_signal`: the detector gets the
/// last row at or before t0, then the rows in (t0, t1 + lat], then the clock past t1 + lat; the
/// first successful check is the signal. `rows` are in time order.
pub fn signal_for_token(rows: &[BboRow], coin: Coin, s0_ms: i64, p: &Params) -> Option<Check> {
    let rel = |r: &BboRow| (r.ts_ms - s0_ms) as f64 / 1000.0;
    let j0 = rows.partition_point(|r| rel(r) <= p.t0).checked_sub(1)?;
    let mut det = CoinDetector::new(p, coin, s0_ms);
    let later = rows.iter().filter(|r| {
        let t = rel(r);
        t > p.t0 && t <= p.t1 + p.lat
    });
    for r in std::iter::once(&rows[j0]).chain(later) {
        if let Some(c) = det.update(r.ts_ms, r.bid, r.ask).filter(|c| c.ok) {
            return Some(c);
        }
    }
    let past = s0_ms + ((p.t1 + p.lat) * 1000.0).round_ties_even() as i64 + 1;
    det.flush(past).filter(|c| c.ok)
}

/// The signals of one file: (window index, check), tokens in ascending byte order.
fn file_signals(path: &Path, windows: &[WindowMeta], by_token: &HashMap<[u8; 32], usize>, p: &Params) -> Result<Vec<(usize, Check)>> {
    let tokens: HashSet<[u8; 32]> = by_token.keys().copied().collect();
    let mut groups: BTreeMap<[u8; 32], Vec<BboRow>> = BTreeMap::new();
    for r in read_bbo_file(path, &tokens)? {
        groups.entry(r.token).or_default().push(r);
    }
    let mut out = vec![];
    for (token, mut rows) in groups {
        rows.sort_by_key(|r| r.ts_ms); // stable, as sort_values(kind='stable')
        let i = by_token[&token];
        if let Some(c) = signal_for_token(&rows, windows[i].coin, windows[i].s0_ms, p) {
            out.push((i, c));
        }
    }
    Ok(out)
}

/// The signals of `windows` found in the book `files`, numbered and decided under v2 and v2.1; one
/// row per signal, ordered by window and number. `closes` holds the 1-s closes of each coin.
/// A coin of `windows` missing from `closes` is an error: its signals cannot get a z.
pub fn replay_with(files: &[PathBuf], windows: &[WindowMeta], closes: &HashMap<Coin, ArrayCloses>, strategy: &Strategy) -> Result<Vec<SignalRow>> {
    if let Some(w) = windows.iter().find(|w| !closes.contains_key(&w.coin)) {
        bail!("no Binance closes for {}", w.coin.as_str());
    }
    // the detector's parameters are the same in v2 and v2.1
    let p = &strategy.v2;
    let by_token: HashMap<[u8; 32], usize> = windows.iter().enumerate().map(|(i, w)| (w.up, i)).collect();
    let per_file: Vec<Result<Vec<(usize, Check)>>> =
        files.par_iter().map(|f| file_signals(f, windows, &by_token, p)).collect();

    // `res[i] = ...` of the backtest: an overwrite keeps the window's first position
    let mut order: Vec<usize> = vec![];
    let mut found: HashMap<usize, Check> = HashMap::new();
    for (file, signals) in files.iter().zip(per_file) {
        for (i, c) in signals.with_context(|| format!("{}", file.display()))? {
            if found.insert(i, c).is_none() {
                order.push(i);
            }
        }
    }

    struct Kept {
        pos: usize,
        i: usize,
        check: Check,
        z: ZInfo,
    }
    let mut by_window: BTreeMap<i64, Vec<Kept>> = BTreeMap::new();
    for (pos, &i) in order.iter().enumerate() {
        let w = &windows[i];
        let check = found[&i];
        let s = w.s0_ms.div_euclid(1000);
        let cl = &closes[&w.coin];
        if !cl.covers(s - BEFORE_S, s + v21_core::window::WINDOW_S) {
            continue;
        }
        match z_score(cl, s, check.t_trig, check.side, p) {
            Some(z) if !z.z.is_nan() => by_window.entry(w.s0_ms).or_default().push(Kept { pos, i, check, z }),
            _ => {}
        }
    }

    let mut rows = vec![];
    for (s0_ms, mut kept) in by_window {
        kept.sort_by(|a, b| a.check.t_trig.total_cmp(&b.check.t_trig).then(a.pos.cmp(&b.pos)));
        let s = s0_ms.div_euclid(1000);
        let (mut e2, mut e21) = (WindowEngine::new(&strategy.v2, s), WindowEngine::new(&strategy.v21, s));
        for (k, kept) in kept.into_iter().enumerate() {
            let w = &windows[kept.i];
            let c = kept.check;
            let win = win_of(c.side, w.p_up_final);
            rows.push(SignalRow {
                slug: w.slug.clone(),
                coin: w.coin,
                s0_ms,
                t_trig: c.t_trig,
                side: c.side,
                px: c.px,
                z: kept.z,
                rank: u32::try_from(k + 1).expect("a few signals per window"),
                p_up_final: w.p_up_final,
                win,
                ret_top: trade_return(c.px, win, 0.0, p.fee),
                ret: trade_return(c.px, win, p.slip, p.fee),
                entry_ms: entry_ms(s0_ms, c.t_trig),
                v2: e2.decide(&c, Some(&kept.z)),
                v21: e21.decide(&c, Some(&kept.z)),
            });
        }
    }
    Ok(rows)
}

/// The whole archive of a period: its windows (without the incident windows for `Main`), its book
/// files and the Binance closes of the watched coins.
pub fn replay(root: &Path, period: Period, strategy: &Strategy) -> Result<ReplayOutput> {
    let p = &strategy.v2;
    let incidents = match period {
        Period::Main => Some(Incidents::load(&root.join("data/INCIDENTS.json"), p.incident_buffer_min)?),
        Period::Pre => None,
    };
    let windows = load_windows(root, period, &p.watch_coins, incidents.as_ref())?;
    let files = bbo_files(root, period)?;
    let closes = p
        .watch_coins
        .iter()
        .map(|&c| Ok((c, load_closes(root, period, c)?)))
        .collect::<Result<HashMap<Coin, ArrayCloses>>>()?;
    let signals = replay_with(&files, &windows, &closes, strategy)?;
    Ok(ReplayOutput { period, signals, windows: windows.len(), files: files.len() })
}
