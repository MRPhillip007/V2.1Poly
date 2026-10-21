//! The demo account on the fast decisions (spec §6.3, §7.1): a position for each shot that bought
//! something, settled by the market's official outcome; «мало денег» and the stop.

use v21_core::fill::{fee, FeeSchedule, Fill};
use v21_core::money::DemoAccount;
use v21_core::window::{Coin, Side};
use v21_live::cold::demo::{DemoDesk, DeskOut};
use v21_live::cold::exact::{DemoFill, ShotFill};
use v21_live::shared::msgs::ShotRecord;

/// 2026-10-02 03:15 UTC.
const S: i64 = 1_790_910_900;
const FEE: FeeSchedule = FeeSchedule { rate: 0.07, exponent: 1.0 };

/// The demo fill of a $20 shot when `size` contracts rest at `price` (and none below 0.88 else).
fn shot_fill(coin: Coin, side: Side, price: f64, size: f64) -> ShotFill {
    let shares = (20.0 / price).min(size);
    let at_match = if shares > 0.0 {
        Fill { shares, notional_usd: shares * price, fee_usd: fee(shares, price, &FEE), avg_price: price, levels_used: 1, unfilled_usd: 20.0 - shares * price }
    } else {
        Fill { shares: 0.0, notional_usd: 0.0, fee_usd: 0.0, avg_price: f64::NAN, levels_used: 0, unfilled_usd: 20.0 }
    };
    let shot = ShotRecord { s: S, coin, side, t_trig: 840.0, event_ts_ms: S * 1000 + 839_500, send_ms: S * 1000 + 840_000, rank: 1, z: None, frame_to_decision_ns: 0 };
    ShotFill { shot, demo: DemoFill { match_ms: S * 1000 + 840_256, at_match, no_latency: at_match, by_delay: None, depth_no_latency_usd: 100.0, depth_at_match_usd: 100.0 } }
}

#[test]
fn the_desk_opens_a_position_only_for_a_fill_and_stops_when_money_runs_out() {
    let mut d = DemoDesk::new(DemoAccount::new(41.0), 20.0);
    assert_eq!(d.pause(), None);
    // nothing under 0.88: nothing bought
    assert_eq!(d.on_shot_fill(&shot_fill(Coin::Eth, Side::Up, 0.809, 0.0)), DeskOut::Missed);
    assert!(d.account().positions.is_empty());
    let DeskOut::Entry { pos, balance } = d.on_shot_fill(&shot_fill(Coin::Btc, Side::Up, 0.809, 1000.0)) else { panic!("an entry") };
    assert_eq!((pos.id.clone(), pos.s, pos.coin, pos.side), (format!("{S}-btc"), S, Coin::Btc, Side::Up));
    assert!((pos.cost_usd - 20.2674).abs() < 1e-9 && (pos.fee_usd - 0.2674).abs() < 1e-12, "{pos:?}");
    assert!((balance - (41.0 - 20.2674)).abs() < 1e-9);
    assert_eq!(d.pause(), None, "$20.73 still covers $20 and the reserve");
    // only 15 contracts under 0.88: a part of the stake
    let DeskOut::Entry { pos, balance } = d.on_shot_fill(&shot_fill(Coin::Eth, Side::Up, 0.80, 15.0)) else { panic!("an entry") };
    assert_eq!(pos.shares, 15.0);
    assert!((balance - (41.0 - 20.2674 - 12.168)).abs() < 1e-9, "{balance}");
    assert_eq!(d.pause(), Some("мало денег"));
    // both lose: no money and nothing open — the account stops
    assert!(matches!(d.on_outcome(S, Coin::Btc, 0.0)[..], [DeskOut::Settled { .. }]));
    assert_eq!(d.pause(), Some("мало денег"));
    assert!(matches!(d.on_outcome(S, Coin::Eth, 0.0)[..], [DeskOut::Settled { .. }, DeskOut::Stopped]));
    assert_eq!(d.pause(), Some("стоп демо-счёта"));
    assert!(d.account().stopped);
    // a fill after the stop buys nothing
    assert_eq!(d.on_shot_fill(&shot_fill(Coin::Btc, Side::Up, 0.809, 1000.0)), DeskOut::Missed);
    // a stopped account stays stopped after a restart
    assert_eq!(DemoDesk::new(d.account().clone(), 20.0).pause(), Some("стоп демо-счёта"));
}

#[test]
fn a_bank_below_the_stake_stops_at_once() {
    let d = DemoDesk::new(DemoAccount::new(20.40), 20.0);
    assert_eq!(d.pause(), Some("стоп демо-счёта"));
}

#[test]
fn the_desk_settles_by_the_official_outcome() {
    for (side, payout_up, per_share) in [(Side::Up, 1.0, 1.0), (Side::Down, 1.0, 0.0), (Side::Down, 0.0, 1.0), (Side::Up, 0.5, 0.5)] {
        let mut d = DemoDesk::new(DemoAccount::new(100.0), 20.0);
        let DeskOut::Entry { pos, .. } = d.on_shot_fill(&shot_fill(Coin::Btc, side, 0.809, 1000.0)) else { panic!("an entry") };
        // another coin's outcome, or another window's, leaves it open
        assert_eq!(d.on_outcome(S, Coin::Eth, payout_up), []);
        assert_eq!(d.on_outcome(S + 900, Coin::Btc, payout_up), []);
        let payout = pos.shares * per_share;
        let want = DeskOut::Settled { id: pos.id.clone(), coin: Coin::Btc, side, payout, pnl: payout - pos.cost_usd, balance: 100.0 - pos.cost_usd + payout };
        assert_eq!(d.on_outcome(S, Coin::Btc, payout_up), [want], "{side:?} {payout_up}");
        // once only
        assert_eq!(d.on_outcome(S, Coin::Btc, payout_up), []);
    }
}
