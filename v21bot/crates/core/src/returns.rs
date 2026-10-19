//! The money of one trade as the backtest counts it (`ruslan_v2.returns`).

use crate::window::Side;

/// 1.0 if the side won, 0.0 if it lost: the Up payout for Up, its complement for Down.
pub fn win_of(side: Side, p_up_final: f64) -> f64 {
    match side {
        Side::Up => p_up_final,
        Side::Down => 1.0 - p_up_final,
    }
}

/// The return per dollar spent, fee included: the price plus `slip` (capped at 0.99) costs
/// `p·(1 + fee·(1 − p))` per share, a win pays 1 per share, a loss loses the stake.
pub fn trade_return(px: f64, win: f64, slip: f64, fee: f64) -> f64 {
    let p = (px + slip).min(0.99);
    if win == 1.0 { 1.0 / (p * (1.0 + fee * (1.0 - p))) - 1.0 } else { -1.0 }
}
