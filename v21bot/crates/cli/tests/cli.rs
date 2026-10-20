//! The `v21` command as a user runs it: exit codes and messages for bad input.

use std::process::{Command, Stdio};

fn v21(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_v21"))
        .args(args)
        .env_remove("V21_ROOT")
        .env_remove("V21_DATA")
        .output()
        .unwrap()
}

/// A data folder with the frozen strategy files and nothing else.
fn bare_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in ["strategy_v2.json", "strategy_v2_1.json"] {
        let from = format!("{}/../core/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::copy(from, dir.path().join(name)).unwrap();
    }
    dir
}

#[test]
fn bad_root_exits_2_and_names_the_path() {
    let out = v21(&["replay", "--root", "Z:/no/such/dir"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("strategy_v2.json"), "{err}");
}

#[test]
fn no_root_at_all_exits_2_and_says_how_to_give_one() {
    let out = v21(&["replay"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--root") && err.contains("V21_ROOT"), "{err}");
}

#[test]
fn a_valid_root_without_a_data_file_exits_2_and_names_it() {
    let root = bare_root();
    let out = v21(&["replay", "--root", root.path().to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    // the backtest's table is read first, so a missing one stops the command before the long replay
    assert!(err.contains("ruslan_rule_all_triggers.parquet"), "{err}");
}

#[test]
fn data_and_v21_data_are_aliases() {
    // spec §4.4 and §10 name them --data and V21_DATA
    let out = v21(&["replay", "--data", "Z:/no/such/dir"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("strategy_v2.json"), "{err}");
    let out = Command::new(env!("CARGO_BIN_EXE_v21"))
        .arg("replay")
        .env_remove("V21_ROOT")
        .env("V21_DATA", "Z:/no/such/dir")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("strategy_v2.json"), "{err}");
}

#[test]
fn pre_replay_agrees_with_the_backtest_and_exits_0() {
    let Ok(root) = std::env::var("V21_ROOT") else {
        println!("skipped: V21_ROOT is not set");
        return;
    };
    let out_dir = std::env::temp_dir().join(format!("v21-cli-test-{}", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_v21"))
        .args(["replay", "--root", &root, "--period", "pre", "--out"])
        .arg(&out_dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("signals 517/517 equal, z max diff < 1e-9, ranks equal, v2 trades 69/69, v2.1 trades 55/55"), "{text}");
    assert!(text.contains("0) 07.08-18.08 (PRE)"), "{text}");
    for table in ["signals.csv", "trades_v2.csv", "trades_v21.csv"] {
        assert!(out_dir.join(table).exists(), "{table}");
    }
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn a_closed_stdout_is_a_clean_exit() {
    let Ok(root) = std::env::var("V21_ROOT") else {
        println!("skipped: V21_ROOT is not set");
        return;
    };
    let out_dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_v21"))
        .args(["replay", "--root", &root, "--period", "pre", "--out"])
        .arg(out_dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // the reader goes away before the first line, as `v21 replay | head -0` does
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(matches!(out.status.code(), Some(0 | 1)), "{:?}: {err}", out.status);
    assert!(!err.contains("panicked"), "{err}");
    // the tables are still written
    assert!(out_dir.path().join("signals.csv").exists());
}

#[test]
fn live_mode_is_refused_in_milestone_2() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("v21bot.toml");
    std::fs::write(&config, "mode = \"live\"\n").unwrap();
    let secrets = dir.path().join("secrets.toml");
    let out = v21(&["run", "--config", config.to_str().unwrap(), "--secrets", secrets.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("вехе 3"), "{err}");
}

#[test]
fn a_bad_period_is_refused() {
    let out = v21(&["replay", "--root", ".", "--period", "july"]);
    assert_eq!(out.status.code(), Some(2));
}

/// A journal with one window's signal, and a config whose strategy folder is `root`.
fn journal_and_config(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let journal = root.join("journal.jsonl");
    let s = 1_790_910_900;
    std::fs::write(
        &journal,
        format!(
            "{{\"kind\":\"signal\",\"ts_utc\":\"2026-10-02T03:31:00.000+00:00\",\"s\":{s},\"coin\":\"btc\",\"side\":\"Up\",\"t_trig_s\":840.0,\"px\":0.8,\"counted\":true,\"decision\":\"вход\",\"reason\":\"вход\",\"demo\":{{\"shares\":25.0,\"notional_usd\":20.0,\"unfilled_usd\":0.0,\"avg_price\":0.8}},\"price_no_latency\":0.8}}\n"
        ),
    )
    .unwrap();
    let config = root.join("v21bot.toml");
    std::fs::write(&config, format!("strategy_dir = \"{}\"\n", root.to_string_lossy().replace('\\', "/"))).unwrap();
    (journal, config)
}

#[test]
fn report_prints_the_report_of_a_journal() {
    let root = bare_root();
    let (journal, config) = journal_and_config(root.path());
    let out = v21(&["report", "--journal", journal.to_str().unwrap(), "--config", config.to_str().unwrap(), "--from", "2026-10-02", "--to", "2026-10-02"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{text}{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.starts_with("Отчёт v21bot · демо · 02.10.2026 (UTC)\nСигналов базового правила: 1"), "{text}");
    // a day without windows
    let out = v21(&["report", "--journal", journal.to_str().unwrap(), "--config", config.to_str().unwrap(), "--from", "2026-10-03"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("нет записей"));
}

#[test]
fn report_refuses_a_date_that_is_no_date() {
    let root = bare_root();
    let (journal, config) = journal_and_config(root.path());
    let out = v21(&["report", "--journal", journal.to_str().unwrap(), "--config", config.to_str().unwrap(), "--from", "2026-13-01"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("2026-13-01"));
}

#[test]
fn reconcile_without_a_data_folder_exits_2() {
    let root = bare_root();
    let (journal, _) = journal_and_config(root.path());
    let out = v21(&["reconcile", "--journal", journal.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("V21_ROOT"));
}

#[test]
fn check_with_a_config_it_cannot_read_exits_2() {
    let out = v21(&["check", "--config", "/no/such/v21bot.toml"]);
    assert_eq!(out.status.code(), Some(2));
}
