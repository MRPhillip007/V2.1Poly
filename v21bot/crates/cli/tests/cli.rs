//! The `v21` command as a user runs it: exit codes and messages for bad input.

use std::process::Command;

fn v21(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_v21")).args(args).env_remove("V21_ROOT").output().unwrap()
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
fn a_bad_period_is_refused() {
    let out = v21(&["replay", "--root", ".", "--period", "july"]);
    assert_eq!(out.status.code(), Some(2));
}
