//! `v21` — the command of the strategy v2.1 bot. Milestone 1: `v21 replay`, the archive through the
//! core, compared with the Python backtest.
//!
//! Exit codes: 0 — the replay agrees with the backtest; 1 — it does not; 2 — bad input (no data
//! folder, a missing or unreadable file, strategy files that are not the frozen ones).

use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use v21_core::params::load_strategy;
use v21_replay::archive::Period;
use v21_replay::compare::{compare, load_reference};
use v21_replay::report::{period_table, write_csv};
use v21_replay::run::replay;

/// Problem lines (and lines of newer signals) printed at most; the rest are counted.
const SHOWN: usize = 40;

#[derive(Parser)]
#[command(name = "v21", about = "Strategy v2.1 of the Polymarket 15m Up/Down markets")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Replay the archive through the strategy core and compare it with the Python backtest.
    Replay {
        /// The 15mbacktest folder: strategy_v2.json, strategy_v2_1.json, data/ and data_pre/
        /// (default: the V21_ROOT or V21_DATA environment variable).
        #[arg(long, visible_alias = "data")]
        root: Option<PathBuf>,
        /// main: from 2026-08-18 (data/); pre: 2026-08-07..18 (data_pre/).
        #[arg(long, value_enum, default_value = "main")]
        period: PeriodArg,
        /// Where the CSV tables go.
        #[arg(long, default_value = "replay_out")]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum PeriodArg {
    Main,
    Pre,
}

/// The data folder: the option, else V21_ROOT, else V21_DATA (the spec's name for it).
fn data_root(root: Option<PathBuf>) -> Result<PathBuf> {
    root.or_else(|| std::env::var_os("V21_ROOT").or_else(|| std::env::var_os("V21_DATA")).map(PathBuf::from))
        .ok_or_else(|| anyhow!("no data folder: pass --root DIR (or --data DIR) or set V21_ROOT (or V21_DATA)"))
}

/// Prints to stdout. A reader that went away (`v21 replay | head`) is not an error: the rest of the
/// output is dropped and the command finishes its work.
fn print_out(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    match stdout.write_all(text.as_bytes()).and_then(|()| stdout.flush()) {
        Err(e) if e.kind() != ErrorKind::BrokenPipe => Err(e).context("cannot write to stdout"),
        _ => Ok(()),
    }
}

fn run_replay(root: Option<PathBuf>, period: PeriodArg, out: PathBuf) -> Result<bool> {
    let root = data_root(root)?;
    let read = |name: &str| {
        let path = root.join(name);
        std::fs::read_to_string(&path).with_context(|| format!("{}: cannot read", path.display()))
    };
    let strategy = load_strategy(&read("strategy_v2.json")?, &read("strategy_v2_1.json")?)
        .context("the strategy files")?;
    let period = match period {
        PeriodArg::Main => Period::Main,
        PeriodArg::Pre => Period::Pre,
    };
    // the backtest's tables first: a missing one stops the command now, not after the replay
    let reference = load_reference(&root, period, &strategy.v21)?;
    let started = Instant::now();
    let got = replay(&root, period, &strategy)?;
    let cmp = compare(&got, &reference);
    let seconds = started.elapsed().as_secs_f64();

    let mut text = format!("{} windows, {} book files, {} signals of the base rule\n", got.windows, got.files, got.signals.len());
    for line in cmp.lines(SHOWN) {
        text.push_str(&line);
        text.push('\n');
    }
    text.push('\n');
    text.push_str(&period_table(&got.signals, &strategy, period == Period::Pre));
    print_out(&text)?;
    write_csv(&out, &got.signals, &strategy)?;
    print_out(&format!("\ntables: {}\nreplay and comparison: {seconds:.1} s\n", out.display()))?;
    Ok(cmp.problems.is_empty())
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    match cli.cmd {
        Cmd::Replay { root, period, out } => match run_replay(root, period, out) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(1),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "error: {e:#}");
                ExitCode::from(2)
            }
        },
    }
}
