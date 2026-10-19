//! The replay against the backtest's own tables: every signal with its z, number and returns, and
//! the sets of v2 and v2.1 trades — the acceptance of milestone 1 (spec §5.4).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use v21_core::params::Params;
use v21_core::window::{Coin, Side};

use crate::archive::{float_column, int_column, read_columns, texts, Period};
use crate::run::{ReplayOutput, SignalRow};

/// z, cushion, sigma and h must agree to this.
const Z_TOL: f64 = 1e-9;
/// The returns of a trade must agree to this.
const RET_TOL: f64 = 1e-12;

/// One signal as the backtest stored it.
#[derive(Clone, Debug)]
pub struct RefSignal {
    pub slug: String,
    pub coin: Coin,
    pub s0_ms: i64,
    pub t_trig: f64,
    pub side: Side,
    pub px: f64,
    pub z: f64,
    pub cushion_bp: f64,
    pub sigma_bp: f64,
    pub h: f64,
    pub rank: u32,
    pub ret_top: f64,
    pub ret: f64,
    pub entry_ms: i64,
}

/// The backtest's tables of one archive.
pub struct Reference {
    pub signals: Vec<RefSignal>,
    /// Slugs of the v2 trades (`ruslan_v2_trades.parquet`).
    pub v2_trades: BTreeSet<String>,
    /// Slugs of the v2.1 trades: the v2 trades `ruslan_v2_1.is_v21` keeps.
    pub v21_trades: BTreeSet<String>,
    /// The latest window start of the signals: the replay's later windows are newer than the tables.
    pub last_s0_ms: i64,
}

pub struct Comparison {
    /// One line per discrepancy; empty when everything agrees.
    pub problems: Vec<String>,
    /// The replay's signals in windows after the reference's last one: the tables are older than the
    /// archive, not wrong.
    pub newer: Vec<String>,
    /// One line, e.g. `signals 2748/2748 equal, z max diff < 1e-9, ranks equal, v2 trades 315/315, ...`.
    pub verdict: String,
}

impl Comparison {
    /// What the command prints: the verdict, then at most `shown` problem lines and at most `shown`
    /// lines of newer signals, each list closed by a count of the lines left out.
    pub fn lines(&self, shown: usize) -> Vec<String> {
        let mut out = vec![self.verdict.clone()];
        out.extend(self.problems.iter().take(shown).map(|p| format!("  {p}")));
        if self.problems.len() > shown {
            out.push(format!("  ... and {} more", self.problems.len() - shown));
        }
        out.extend(self.newer.iter().take(shown).map(|n| format!("  {n}")));
        if self.newer.len() > shown {
            out.push(format!("  ... and {} more newer than the reference", self.newer.len() - shown));
        }
        out
    }
}

fn reference_paths(root: &Path, period: Period) -> (PathBuf, PathBuf) {
    match period {
        Period::Main => (root.join("data/ruslan_rule_all_triggers.parquet"), root.join("data/ruslan_v2_trades.parquet")),
        Period::Pre => (root.join("data_pre/ruslan_rule_pre_triggers.parquet"), root.join("data_pre/ruslan_v2_pre_trades.parquet")),
    }
}

fn parse_side(s: Option<&str>) -> Option<Side> {
    match s? {
        "Up" => Some(Side::Up),
        "Down" => Some(Side::Down),
        _ => None,
    }
}

/// The tables `ruslan_v2.py` (or `ruslan_v2_pre.py`) wrote for an archive. `v21` selects the v2.1
/// trades among the v2 ones, as `ruslan_v2_1.is_v21` does: its coins and the UTC hour of the entry.
pub fn load_reference(root: &Path, period: Period, v21: &Params) -> Result<Reference> {
    let (sig_path, trades_path) = reference_paths(root, period);
    let names = [
        "slug", "coin", "s0", "t_trig", "side", "px", "z", "cushion_bp", "sigma_bp", "h", "rank_in_window", "ret_top", "ret", "entry",
    ];
    let mut signals = vec![];
    for cols in read_columns(&sig_path, &names)? {
        let p = &sig_path;
        let slug = texts(&cols[0], p, "slug")?;
        let coin = texts(&cols[1], p, "coin")?;
        let s0 = int_column(&cols[2], p, "s0")?;
        let side = texts(&cols[4], p, "side")?;
        let f = |k: usize| float_column(&cols[k], p, names[k]);
        let (t_trig, px, z, cushion, sigma, h) = (f(3)?, f(5)?, f(6)?, f(7)?, f(8)?, f(9)?);
        let rank = int_column(&cols[10], p, "rank_in_window")?;
        let (ret_top, ret) = (f(11)?, f(12)?);
        let entry = int_column(&cols[13], p, "entry")?;
        for i in 0..s0.len() {
            let slug = slug[i].clone().ok_or_else(|| anyhow!("{}: row {i}: no slug", p.display()))?;
            signals.push(RefSignal {
                coin: coin[i].as_deref().and_then(Coin::parse).ok_or_else(|| anyhow!("{}: {slug}: bad coin", p.display()))?,
                side: parse_side(side[i].as_deref()).ok_or_else(|| anyhow!("{}: {slug}: bad side", p.display()))?,
                rank: u32::try_from(rank[i]).map_err(|_| anyhow!("{}: {slug}: bad rank", p.display()))?,
                slug,
                s0_ms: s0[i],
                t_trig: t_trig[i],
                px: px[i],
                z: z[i],
                cushion_bp: cushion[i],
                sigma_bp: sigma[i],
                h: h[i],
                ret_top: ret_top[i],
                ret: ret[i],
                entry_ms: entry[i],
            });
        }
    }
    let (mut v2_trades, mut v21_trades) = (BTreeSet::new(), BTreeSet::new());
    for cols in read_columns(&trades_path, &["slug", "coin", "entry"])? {
        let p = &trades_path;
        let slug = texts(&cols[0], p, "slug")?;
        let coin = texts(&cols[1], p, "coin")?;
        let entry = int_column(&cols[2], p, "entry")?;
        for i in 0..entry.len() {
            let slug = slug[i].clone().ok_or_else(|| anyhow!("{}: row {i}: no slug", p.display()))?;
            let hour = u32::try_from(entry[i].div_euclid(3_600_000).rem_euclid(24)).expect("an hour of the day");
            let coin = coin[i].as_deref().and_then(Coin::parse);
            if coin.is_some_and(|c| v21.trade_coins.contains(&c)) && v21.hours.iter().any(|&(a, b)| a <= hour && hour < b) {
                v21_trades.insert(slug.clone());
            }
            v2_trades.insert(slug);
        }
    }
    let last_s0_ms = signals.iter().map(|s| s.s0_ms).max().unwrap_or(i64::MIN);
    Ok(Reference { signals, v2_trades, v21_trades, last_s0_ms })
}

fn side_name(s: Side) -> &'static str {
    match s {
        Side::Up => "Up",
        Side::Down => "Down",
    }
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() < tol // false for a NaN
}

/// The replay `got` against the backtest's tables `r`.
pub fn compare(got: &ReplayOutput, r: &Reference) -> Comparison {
    let mut problems = vec![];
    let mut newer = vec![];
    let mut seen = HashSet::new();
    for g in &got.signals {
        if !seen.insert(g.slug.as_str()) {
            problems.push(format!("duplicate in the replay: {}", g.slug));
        }
    }
    let got_by: HashMap<&str, &SignalRow> = got.signals.iter().map(|g| (g.slug.as_str(), g)).collect();
    let ref_by: HashMap<&str, &RefSignal> = r.signals.iter().map(|s| (s.slug.as_str(), s)).collect();
    for s in &r.signals {
        if !got_by.contains_key(s.slug.as_str()) {
            problems.push(format!("only in the backtest: {} (t_trig {}, {}, px {})", s.slug, s.t_trig, side_name(s.side), s.px));
        }
    }
    for g in &got.signals {
        if !ref_by.contains_key(g.slug.as_str()) {
            let line = format!("{} (t_trig {}, {}, px {})", g.slug, g.t_trig, side_name(g.side), g.px);
            if g.s0_ms > r.last_s0_ms {
                newer.push(format!("newer than the reference: {line}"));
            } else {
                problems.push(format!("only in the replay: {line}"));
            }
        }
    }

    let (mut equal, mut z_max, mut bad_z, mut bad_rank) = (0usize, 0.0f64, 0usize, 0usize);
    for s in &r.signals {
        let Some(g) = got_by.get(s.slug.as_str()) else { continue };
        if g.t_trig == s.t_trig && g.side == s.side && g.px == s.px {
            equal += 1;
        } else {
            problems.push(format!(
                "differs: {}: backtest t_trig {}, {}, px {}; replay t_trig {}, {}, px {}",
                s.slug, s.t_trig, side_name(s.side), s.px, g.t_trig, side_name(g.side), g.px
            ));
        }
        let fields = [
            ("z", g.z.z, s.z),
            ("cushion_bp", g.z.cushion_bp, s.cushion_bp),
            ("sigma_bp", g.z.sigma_bp, s.sigma_bp),
            ("h", g.z.h, s.h),
        ];
        for (_, a, b) in fields {
            let d = (a - b).abs();
            // a NaN stays: f64::max would drop it at the next number
            z_max = if z_max.is_nan() || d.is_nan() { f64::NAN } else { z_max.max(d) };
        }
        let bad: Vec<&str> = fields.iter().filter(|(_, a, b)| !close(*a, *b, Z_TOL)).map(|(name, _, _)| *name).collect();
        if !bad.is_empty() {
            bad_z += 1;
            problems.push(format!("z differs: {}: {} (backtest z {:?}, replay z {:?})", s.slug, bad.join(", "), s.z, g.z.z));
        }
        if g.rank != s.rank {
            bad_rank += 1;
            problems.push(format!("rank differs: {}: backtest {}, replay {}", s.slug, s.rank, g.rank));
        }
        if !close(g.ret_top, s.ret_top, RET_TOL) || !close(g.ret, s.ret, RET_TOL) {
            problems.push(format!(
                "return differs: {}: backtest {:?} / {:?}, replay {:?} / {:?}",
                s.slug, s.ret_top, s.ret, g.ret_top, g.ret
            ));
        }
    }

    // the trades of the replay in the windows the tables cover
    let traded = |pick: fn(&SignalRow) -> bool| -> BTreeSet<String> {
        got.signals.iter().filter(|g| g.s0_ms <= r.last_s0_ms && pick(g)).map(|g| g.slug.clone()).collect()
    };
    let (have_v2, have_v21) = (traded(|g| g.v2.trade), traded(|g| g.v21.trade));
    for (name, want, have) in [("v2", &r.v2_trades, &have_v2), ("v2.1", &r.v21_trades, &have_v21)] {
        for s in want.difference(have) {
            problems.push(format!("{name} trade only in the backtest: {s}"));
        }
        for s in have.difference(want) {
            problems.push(format!("{name} entry only in the replay: {s}"));
        }
    }

    let mut parts = vec![format!("signals {equal}/{} equal", r.signals.len())];
    parts.push(if bad_z == 0 { "z max diff < 1e-9".to_string() } else { format!("z max diff {z_max:.3e}") });
    parts.push(if bad_rank == 0 { "ranks equal".to_string() } else { format!("{bad_rank} ranks differ") });
    parts.push(format!("v2 trades {}/{}", r.v2_trades.intersection(&have_v2).count(), r.v2_trades.len()));
    parts.push(format!("v2.1 trades {}/{}", r.v21_trades.intersection(&have_v21).count(), r.v21_trades.len()));
    if !newer.is_empty() {
        parts.push(format!("{} newer than the reference — rerun ruslan_v2.py", newer.len()));
    }
    Comparison { problems, newer, verdict: parts.join(", ") }
}
