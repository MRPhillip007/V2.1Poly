//! The money rules of spec §7.1 and the demo account of §6.3.

use v21_core::money::{stake_answer, DemoAccount, DemoPosition, StakeAnswer};
use v21_core::window::{Coin, Side};

fn position(id: &str, shares: f64, cost_usd: f64) -> DemoPosition {
    DemoPosition {
        id: id.to_string(),
        s: 1_790_910_900,
        coin: Coin::Btc,
        side: Side::Up,
        shares,
        cost_usd,
        fee_usd: 0.28,
        avg_price: 0.80,
    }
}

#[test]
fn stake_rules() {
    // a trade needs the stake and 0.50 for the worst fee
    assert_eq!(stake_answer(20.49, 20.0, 1), StakeAnswer::NotEnough);
    assert_eq!(stake_answer(20.49, 20.0, 0), StakeAnswer::Stop);
    assert_eq!(stake_answer(20.50, 20.0, 0), StakeAnswer::Stake(20.0));
    assert_eq!(stake_answer(100.0, 20.0, 3), StakeAnswer::Stake(20.0));
}

#[test]
fn a_bank_of_100_takes_four_stakes() {
    let mut a = DemoAccount::new(100.0);
    for k in 0..4 {
        assert_eq!(a.stake(20.0), StakeAnswer::Stake(20.0), "stake {k}");
        a.open(position(&format!("t{k}"), 25.0, 20.28));
    }
    assert!((a.cash - (100.0 - 4.0 * 20.28)).abs() < 1e-9, "{}", a.cash);
    // 18.88 left, but open positions can bring money back
    assert_eq!(a.stake(20.0), StakeAnswer::NotEnough);
    assert!(!a.stopped);
}

#[test]
fn no_money_and_no_positions_stops_the_account() {
    let mut a = DemoAccount::new(20.40);
    assert_eq!(a.stake(20.0), StakeAnswer::Stop);
    assert!(a.stopped);
}

#[test]
fn settle_pays_1_0_or_half() {
    for (payout_per_share, want_payout, want_pnl) in [(1.0, 25.0, 4.72), (0.0, 0.0, -20.28), (0.5, 12.5, -7.78)] {
        let mut a = DemoAccount::new(100.0);
        a.open(position("t", 25.0, 20.28));
        let (payout, pnl) = a.settle("t", payout_per_share).unwrap();
        assert!((payout - want_payout).abs() < 1e-9 && (pnl - want_pnl).abs() < 1e-9, "{payout} {pnl}");
        assert!((a.cash - (100.0 + want_pnl)).abs() < 1e-9, "cash {}", a.cash);
        assert!(a.positions.is_empty());
        assert_eq!(a.settle("t", payout_per_share), None, "settled once");
    }
}

#[test]
fn opening_the_same_trade_twice_changes_nothing() {
    let mut a = DemoAccount::new(100.0);
    a.open(position("t", 25.0, 20.28));
    a.open(position("t", 25.0, 20.28));
    assert_eq!(a.positions.len(), 1);
    assert!((a.cash - 79.72).abs() < 1e-9, "{}", a.cash);
}

#[test]
fn the_account_round_trips_through_json_with_the_journal_names() {
    let mut a = DemoAccount::new(100.0);
    a.open(position("1790910900-btc", 25.0, 20.28));
    let text = serde_json::to_string(&a).unwrap();
    assert!(text.contains(r#""coin":"btc""#) && text.contains(r#""side":"Up""#), "{text}");
    let back: DemoAccount = serde_json::from_str(&text).unwrap();
    assert_eq!(back, a);
}
