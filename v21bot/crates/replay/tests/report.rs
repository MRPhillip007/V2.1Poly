//! The period table (as `ruslan_v2_1.py::side_by_side`) and the CSV tables of the replay.

use v21_core::decision::{Decision, Reason};
use v21_core::detector::Check;
use v21_core::params::{load_strategy, Strategy};
use v21_core::returns::trade_return;
use v21_core::window::{Coin, Side};
use v21_core::zscore::ZInfo;
use v21_replay::report::{period_table, write_csv};
use v21_replay::run::SignalRow;

const V2: &str = include_str!("../../core/tests/data/strategy_v2.json");
const V21: &str = include_str!("../../core/tests/data/strategy_v2_1.json");

fn strategy() -> Strategy {
    load_strategy(V2, V21).unwrap()
}

fn decision(c: Check, z: ZInfo, rank: u32, trade: bool) -> Decision {
    Decision {
        check: c,
        counted: true,
        rank: Some(rank),
        z: Some(z),
        hour_ok: true,
        trade,
        reason: if trade { Reason::Enter } else { Reason::CoinNotTraded },
    }
}

/// A signal of `coin` in the window `s0_ms`, entered 840 s in; `v2` and `v21` say whether it trades.
fn signal(coin: Coin, s0_ms: i64, px: f64, win: f64, v2: bool, v21: bool) -> SignalRow {
    let c = Check { coin, t_trig: 840.0, side: Side::Up, ask_at_trig: px, px, ok: true };
    let z = ZInfo { z: 2.0, p0: 100.0, ef: 100.1, sigma_bp: 1.0, h: 19.5, cushion_bp: 5.0 };
    SignalRow {
        slug: format!("{}-updown-15m-{}", coin.as_str(), s0_ms / 1000),
        coin,
        s0_ms,
        t_trig: 840.0,
        side: Side::Up,
        px,
        z,
        rank: 1,
        p_up_final: win,
        win,
        ret_top: trade_return(px, win, 0.0, 0.07),
        ret: trade_return(px, win, 0.014, 0.07),
        entry_ms: s0_ms + 840_000,
        v2: decision(c, z, 1, v2),
        v21: decision(c, z, 1, v21),
    }
}

/// 2026-08-20 03:00 UTC (period 1) and 2026-10-16 03:00 UTC (the v2.1 forward).
const P1: i64 = 1_787_194_800_000;
const FWD: i64 = 1_792_119_600_000;

fn sample() -> Vec<SignalRow> {
    vec![
        signal(Coin::Btc, P1, 0.80, 1.0, true, true),
        signal(Coin::Sol, P1 + 900_000, 0.80, 0.0, true, false), // a v2 trade v2.1 drops
        signal(Coin::Eth, P1 + 1_800_000, 0.85, 1.0, false, false), // no trade at all
        signal(Coin::Btc, FWD, 0.75, 1.0, true, true),
    ]
}

#[test]
fn period_table_has_a_row_per_period_and_the_three_column_groups() {
    let table = period_table(&sample(), &strategy(), false);
    let lines: Vec<&str> = table.lines().collect();
    let header = lines[0];
    for col in ["period", "v2 n", "v2 win%", "v2 roi%", "v2 t", "v2.1 n", "v2.1 win%", "v2.1 roi%", "v2.1 t",
                "dropped n", "dropped win%", "dropped roi%", "dropped t"] {
        assert!(header.contains(col), "{col}: {header}");
    }
    let row = |label: &str| *lines.iter().find(|l| l.starts_with(label)).unwrap_or_else(|| panic!("{label}: {table}"));
    let cells = |label: &str| row(label).split('|').map(str::trim).collect::<Vec<_>>();
    // period 1: v2 has 2 trades (one won), v2.1 keeps 1, 1 is dropped
    let p1 = cells("1) 18.08-06.09");
    assert_eq!((p1[1], p1[2], p1[5], p1[6], p1[9], p1[10]), ("2", "50.00", "1", "100.00", "1", "0.00"));
    let fwd = cells("5) FORWARD v2.1 from 15.10");
    assert_eq!((fwd[1], fwd[5], fwd[9]), ("1", "1", "0"));
    let all = cells("all");
    assert_eq!((all[1], all[5], all[9]), ("3", "2", "1"));
    assert!(!table.contains("2) 07.09-01.10 10:00"), "periods without trades are left out: {table}");
}

#[test]
fn csv_header_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    write_csv(dir.path(), &sample(), &strategy()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("signals.csv")).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next().unwrap(),
        "slug,coin,s0,t_trig,side,px,z,cushion_bp,sigma_bp,h,rank,v2_decision,v2_reason,v21_decision,v21_reason,win,ret_top,ret"
    );
    assert_eq!(lines.count(), 4);
    let trades = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap().lines().count() - 1;
    assert_eq!((trades("trades_v2.csv"), trades("trades_v21.csv")), (3, 2));
    let first = text.lines().nth(1).unwrap();
    assert!(first.starts_with("btc-updown-15m-1787194800,btc,1787194800000,840,Up,0.8,2,5,1,19.5,1,вход,вход,вход,вход,1,"), "{first}");
}
