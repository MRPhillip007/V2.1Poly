//! The money rules of spec §7.1 and the demo account of §6.3. The same rule will decide the stake of
//! the live account in milestone 3.

use serde::{Deserialize, Serialize};

use crate::window::{Coin, Side};

/// What an order can cost above its stake with the worst fee: a trade needs `stake + RESERVE_USD`.
pub const RESERVE_USD: f64 = 0.50;

/// The answer before an order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StakeAnswer {
    /// Trade this many dollars before the fee.
    Stake(f64),
    /// Not enough free money now, but open positions can bring some back: no trade, reason
    /// «мало денег».
    NotEnough,
    /// Not enough free money and nothing open that can bring any back: the account stops (§7.5).
    Stop,
}

/// The stake of the next order (§7.1): the free money must cover `stake_usd + RESERVE_USD`.
pub fn stake_answer(free_usd: f64, stake_usd: f64, open_positions: usize) -> StakeAnswer {
    if free_usd >= stake_usd + RESERVE_USD {
        StakeAnswer::Stake(stake_usd)
    } else if open_positions > 0 {
        StakeAnswer::NotEnough
    } else {
        StakeAnswer::Stop
    }
}

/// A position of the demo account, held until the market's official outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoPosition {
    /// Unique key of the trade: one trade per window and coin, `"{s}-{coin}"`.
    pub id: String,
    /// Window start, unix seconds.
    pub s: i64,
    pub coin: Coin,
    pub side: Side,
    /// Contracts held: each pays 1 if the side won, 0 if it lost, 0.5 on a 50/50 close.
    pub shares: f64,
    /// What they cost, fee included.
    pub cost_usd: f64,
    /// The fee part of `cost_usd`.
    pub fee_usd: f64,
    /// Average price per contract, fee excluded.
    pub avg_price: f64,
}

/// The demo account: free cash and the open positions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoAccount {
    /// Free money: a trade takes its whole cost out at entry, its payout comes back at settlement.
    pub cash: f64,
    pub positions: Vec<DemoPosition>,
    /// Stopped for lack of money (§7.1): it stays so until the user starts a new account.
    pub stopped: bool,
}

impl DemoAccount {
    pub fn new(bank_usd: f64) -> DemoAccount {
        DemoAccount { cash: bank_usd, positions: vec![], stopped: false }
    }

    /// The answer of `stake_answer` for this account; a stop is remembered.
    pub fn stake(&mut self, stake_usd: f64) -> StakeAnswer {
        let answer = stake_answer(self.cash, stake_usd, self.positions.len());
        if answer == StakeAnswer::Stop {
            self.stopped = true;
        }
        answer
    }

    /// Take the cost of the position out of the cash and hold it until it is settled. A trade that
    /// is already open stays as it is: one trade cannot be opened twice.
    pub fn open(&mut self, pos: DemoPosition) {
        if self.positions.iter().any(|p| p.id == pos.id) {
            return;
        }
        self.cash -= pos.cost_usd;
        self.positions.push(pos);
    }

    /// Close a position: each contract pays `payout_per_share` (1 won, 0 lost, 0.5 for a market
    /// closed 50/50) into the cash. Returns (payout, P&L); `None` if no such position is open.
    pub fn settle(&mut self, id: &str, payout_per_share: f64) -> Option<(f64, f64)> {
        let at = self.positions.iter().position(|p| p.id == id)?;
        let pos = self.positions.remove(at);
        let payout = pos.shares * payout_per_share;
        self.cash += payout;
        Some((payout, payout - pos.cost_usd))
    }
}
