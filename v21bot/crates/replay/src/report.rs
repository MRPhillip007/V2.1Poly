//! What the replay prints and writes: the table of v2, v2.1 and the trades v2.1 drops by period
//! (as `ruslan_v2_1.py::side_by_side`), and the CSV tables of every signal and trade.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use v21_core::decision::{ENTER, SKIP};
use v21_core::params::Strategy;
use v21_core::stats::{half_of, stats, Half, TradeRow};
use v21_core::window::Side;

use crate::run::SignalRow;

fn trade_row(s: &SignalRow) -> TradeRow {
    TradeRow { s0_ms: s.s0_ms, entry_ms: s.entry_ms, px: s.px, win: s.win, ret_top: s.ret_top, ret: s.ret }
}

fn two(x: f64) -> String {
    if x.is_nan() { "nan".to_string() } else { format!("{x:.2}") }
}

/// n, win %, return per trade with slippage %, and its t by window, of a set of trades.
fn cells(rows: &[&SignalRow], s: &Strategy) -> [String; 4] {
    let rows: Vec<TradeRow> = rows.iter().map(|r| trade_row(r)).collect();
    match stats(&rows, s.v2.slip, s.v2.fee) {
        None => ["0".into(), "-".into(), "-".into(), "-".into()],
        Some(st) => [st.trades.to_string(), two(st.win_pct), two(st.roi_slip_pct), two(st.t_slip)],
    }
}

const LABEL_W: usize = 40;

fn line(out: &mut String, label: &str, cells: &[String]) {
    let _ = write!(out, "{label:<LABEL_W$}");
    for c in cells {
        let _ = write!(out, " | {c:>12}");
    }
    out.push('\n');
}

/// One row per period with v2 trades, and a row `all`: n, win %, return % and t of the v2 trades,
/// the v2.1 trades and the v2 trades v2.1 drops. Returns are with slippage, t is clustered by window.
pub fn period_table(signals: &[SignalRow], s: &Strategy, pre: bool) -> String {
    let mut by_half: BTreeMap<Half, Vec<&SignalRow>> = BTreeMap::new();
    for sig in signals.iter().filter(|x| x.v2.trade) {
        by_half.entry(half_of(sig.entry_ms, sig.s0_ms, &s.dates, pre)).or_default().push(sig);
    }
    let mut out = String::new();
    let header: Vec<String> = ["v2", "v2.1", "dropped"]
        .iter()
        .flat_map(|g| ["n", "win%", "roi%", "t"].map(|c| format!("{g} {c}")))
        .collect();
    line(&mut out, "period", &header);
    let row = |out: &mut String, label: &str, v2: Vec<&SignalRow>| {
        let (kept, dropped): (Vec<&SignalRow>, Vec<&SignalRow>) = v2.iter().partition(|x| x.v21.trade);
        let mut c = cells(&v2, s).to_vec();
        c.extend(cells(&kept, s));
        c.extend(cells(&dropped, s));
        line(out, label, &c);
    };
    for (half, sigs) in &by_half {
        row(&mut out, half.label(), sigs.clone());
    }
    row(&mut out, "all", by_half.values().flatten().copied().collect());
    out
}

const COLUMNS: [&str; 18] = [
    "slug", "coin", "s0", "t_trig", "side", "px", "z", "cushion_bp", "sigma_bp", "h", "rank", "v2_decision", "v2_reason",
    "v21_decision", "v21_reason", "win", "ret_top", "ret",
];

fn record(sig: &SignalRow, s: &Strategy) -> Vec<String> {
    let decided = |trade: bool| if trade { ENTER } else { SKIP }.to_string();
    vec![
        sig.slug.clone(),
        sig.coin.as_str().to_string(),
        sig.s0_ms.to_string(),
        sig.t_trig.to_string(),
        match sig.side {
            Side::Up => "Up",
            Side::Down => "Down",
        }
        .to_string(),
        sig.px.to_string(),
        sig.z.z.to_string(),
        sig.z.cushion_bp.to_string(),
        sig.z.sigma_bp.to_string(),
        sig.z.h.to_string(),
        sig.rank.to_string(),
        decided(sig.v2.trade),
        sig.v2.reason.label(&s.v2),
        decided(sig.v21.trade),
        sig.v21.reason.label(&s.v21),
        sig.win.to_string(),
        sig.ret_top.to_string(),
        sig.ret.to_string(),
    ]
}

fn write_table<'a>(path: &Path, rows: impl Iterator<Item = &'a SignalRow>, s: &Strategy) -> Result<()> {
    let mut w = csv::Writer::from_path(path).with_context(|| format!("{}: cannot write", path.display()))?;
    w.write_record(COLUMNS)?;
    for sig in rows {
        w.write_record(record(sig, s))?;
    }
    w.flush().with_context(|| format!("{}: cannot write", path.display()))?;
    Ok(())
}

/// `signals.csv` (every signal of the base rule), `trades_v2.csv` and `trades_v21.csv` in `out_dir`.
pub fn write_csv(out_dir: &Path, signals: &[SignalRow], s: &Strategy) -> Result<()> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("{}: cannot create", out_dir.display()))?;
    write_table(&out_dir.join("signals.csv"), signals.iter(), s)?;
    write_table(&out_dir.join("trades_v2.csv"), signals.iter().filter(|x| x.v2.trade), s)?;
    write_table(&out_dir.join("trades_v21.csv"), signals.iter().filter(|x| x.v21.trade), s)?;
    Ok(())
}
