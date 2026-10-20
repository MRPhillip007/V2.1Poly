//! `v21` — the command of the strategy v2.1 bot: `v21 replay`, the archive through the core compared
//! with the Python backtest; `v21 run`, the live bot (milestone 2: the demo mode).
//!
//! Exit codes: 0 — done, the replay agrees with the backtest; 1 — the replay does not agree; 2 — bad
//! input (no data folder, a missing or unreadable file, strategy files that are not the frozen ones,
//! a config the bot refuses).

use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use v21_core::params::load_strategy;
use v21_replay::archive::Period;
use v21_replay::compare::{compare, load_reference};
use v21_replay::report::{period_table, write_csv};
use v21_live::shared::config::{Config, Mode};
use v21_live::shared::secrets::Secrets;
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
    /// The demo against the backtest over a period of the journal (spec §8).
    Report {
        /// The journal of the bot.
        #[arg(long)]
        journal: PathBuf,
        /// The config: the folder of the strategy files.
        #[arg(long, default_value = "/etc/v21bot/v21bot.toml")]
        config: PathBuf,
        /// The first UTC day, YYYY-MM-DD.
        #[arg(long)]
        from: Option<String>,
        /// The last UTC day, YYYY-MM-DD.
        #[arg(long)]
        to: Option<String>,
    },
    /// The live windows of the journal against the archive (spec §5.5); the table goes next to the
    /// journal as reconcile_YYYYMMDD.csv.
    Reconcile {
        #[arg(long)]
        journal: PathBuf,
        /// The 15mbacktest folder (default: V21_ROOT or V21_DATA).
        #[arg(long, visible_alias = "data")]
        root: Option<PathBuf>,
    },
    /// Can the bot work from here: the geoblock, the clock, the paths, the feeds, the markets' rules.
    /// Exit code 0 — all is well, 1 — warnings, 2 — the bot cannot work.
    Check {
        #[arg(long, default_value = "/etc/v21bot/v21bot.toml")]
        config: PathBuf,
    },
    /// Run the live bot in the mode its config says (milestone 2: the demo mode only).
    Run {
        /// The config file.
        #[arg(long, default_value = "/etc/v21bot/v21bot.toml")]
        config: PathBuf,
        /// The secrets file: the Telegram token and chat.
        #[arg(long, default_value = "/etc/v21bot/secrets.toml")]
        secrets: PathBuf,
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

/// `v21 run`: the bot in the mode of its config. Milestone 2 runs the demo mode only.
fn run_bot(config: PathBuf, secrets: PathBuf) -> Result<()> {
    let cfg = Config::load(&config)?;
    if cfg.mode == Mode::Live {
        bail!("mode = \"live\" в {}: боевой режим будет в вехе 3, сейчас — только mode = \"demo\"", config.display());
    }
    let secrets = Secrets::load(&secrets)?;
    // Ctrl-C and SIGTERM (systemd's stop) end the bot: it saves its state and its journal first
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed)).context("cannot catch Ctrl-C and SIGTERM")?;
    v21_live::run::run(cfg, secrets, stop)
}

/// Unix seconds of the start of a UTC day written YYYY-MM-DD.
fn day_start(day: &str) -> Result<i64> {
    v21_core::window::parse_utc_ms(&format!("{day}T00:00:00Z")).map(|ms| ms / 1000).ok_or_else(|| anyhow!("{day}: not a day (YYYY-MM-DD)"))
}

fn run_report(journal: PathBuf, config: PathBuf, from: Option<String>, to: Option<String>) -> Result<()> {
    let strategy = Config::load(&config)?.strategy()?;
    let from = from.as_deref().map(day_start).transpose()?;
    let to = to.as_deref().map(|d| day_start(d).map(|s| s + 86_400)).transpose()?;
    let journal = v21_live::cold::journal::Journal::open(&journal)?;
    print_out(&format!("{}\n", v21_live::report::report(&journal, &strategy, from, to)))
}

fn run_reconcile(journal_path: PathBuf, root: Option<PathBuf>) -> Result<()> {
    let root = data_root(root)?;
    let read = |name: &str| std::fs::read_to_string(root.join(name)).with_context(|| format!("{}: cannot read", root.join(name).display()));
    let strategy = load_strategy(&read("strategy_v2.json")?, &read("strategy_v2_1.json")?).map_err(|e| anyhow!("the strategy files: {e}"))?;
    let journal = v21_live::cold::journal::Journal::open(&journal_path)?;
    let rows = v21_live::reconcile::reconcile(&journal, &root, &strategy)?;
    let mut text = v21_live::reconcile::reconcile_summary(&rows);
    if !rows.is_empty() {
        let day = v21_live::shared::utc::fmt_utc(v21_live::shared::clock::local_ms() / 1000)[..10].replace('-', "");
        let path = journal_path.with_file_name(format!("reconcile_{day}.csv"));
        v21_live::reconcile::write_reconcile_csv(&rows, &path).with_context(|| format!("{}", path.display()))?;
        text.push_str(&format!("\nтаблица: {}", path.display()));
    }
    print_out(&format!("{text}\n"))
}

fn run_check(config: PathBuf) -> Result<u8> {
    let cfg = Config::load(&config)?;
    let (text, code) = v21_live::check::check_blocking(&cfg)?;
    print_out(&format!("{text}\n"))?;
    Ok(u8::try_from(code).unwrap_or(2))
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };
    let result = match cli.cmd {
        Cmd::Replay { root, period, out } => run_replay(root, period, out).map(|agrees| if agrees { 0 } else { 1 }),
        Cmd::Run { config, secrets } => run_bot(config, secrets).map(|()| 0),
        Cmd::Report { journal, config, from, to } => run_report(journal, config, from, to).map(|()| 0),
        Cmd::Reconcile { journal, root } => run_reconcile(journal, root).map(|()| 0),
        Cmd::Check { config } => run_check(config),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: {e:#}");
            ExitCode::from(2)
        }
    }
}
