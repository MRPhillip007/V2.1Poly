//! The demo account on the fast decisions (spec §6.3): a position for every shot whose demo fill bought
//! something, settled by the market's official outcome. The money rules of §7.1 pause the shots:
//! «мало денег» while open positions can still bring money back, «стоп демо-счёта» when nothing can.

use v21_core::money::{stake_answer, DemoAccount, DemoPosition, StakeAnswer};
use v21_core::window::{Coin, Side};

use crate::cold::exact::ShotFill;

/// The pauses of the demo account (the hot thread's `FastReason::Paused`).
pub const LOW_MONEY: &str = "мало денег";
pub const STOPPED: &str = "стоп демо-счёта";

#[derive(Clone, Debug, PartialEq)]
pub enum DeskOut {
    Entry { pos: DemoPosition, balance: f64 },
    /// The shot bought nothing (or the account could not pay for it).
    Missed,
    Settled { id: String, coin: Coin, side: Side, payout: f64, pnl: f64, balance: f64 },
    /// The money ran out with nothing open: the account stops until the user starts a new one.
    Stopped,
}

pub struct DemoDesk {
    account: DemoAccount,
    stake_usd: f64,
}

impl DemoDesk {
    /// The desk of `account` (a stop it has earned is remembered at once).
    pub fn new(account: DemoAccount, stake_usd: f64) -> DemoDesk {
        let mut desk = DemoDesk { account, stake_usd };
        desk.answer();
        desk
    }

    /// The answer of §7.1 now; a stop is kept in the account.
    fn answer(&mut self) -> StakeAnswer {
        if self.account.stopped { StakeAnswer::Stop } else { self.account.stake(self.stake_usd) }
    }

    /// The demo fill of a shot: a position if it bought anything and the account can pay.
    pub fn on_shot_fill(&mut self, f: &ShotFill) -> DeskOut {
        let fill = &f.demo.at_match;
        if fill.shares <= 0.0 || !matches!(self.answer(), StakeAnswer::Stake(_)) {
            return DeskOut::Missed;
        }
        let shot = &f.shot;
        let pos = DemoPosition {
            id: format!("{}-{}", shot.s, shot.coin.as_str()),
            s: shot.s,
            coin: shot.coin,
            side: shot.side,
            shares: fill.shares,
            cost_usd: fill.notional_usd + fill.fee_usd,
            fee_usd: fill.fee_usd,
            avg_price: fill.avg_price,
        };
        self.account.open(pos.clone());
        self.answer();
        DeskOut::Entry { pos, balance: self.account.cash }
    }

    /// The official outcome of coin `coin` in window `s`: what an Up contract pays (1, 0 or 0.5).
    pub fn on_outcome(&mut self, s: i64, coin: Coin, payout_up: f64) -> Vec<DeskOut> {
        let id = format!("{s}-{}", coin.as_str());
        let Some(side) = self.account.positions.iter().find(|p| p.id == id).map(|p| p.side) else { return vec![] };
        let per_share = match side {
            Side::Up => payout_up,
            Side::Down => 1.0 - payout_up,
        };
        let was_stopped = self.account.stopped;
        let Some((payout, pnl)) = self.account.settle(&id, per_share) else { return vec![] };
        let mut out = vec![DeskOut::Settled { id, coin, side, payout, pnl, balance: self.account.cash }];
        if self.answer() == StakeAnswer::Stop && !was_stopped {
            out.push(DeskOut::Stopped);
        }
        out
    }

    /// The pause the account puts on the shots, if any.
    pub fn pause(&self) -> Option<&'static str> {
        if self.account.stopped {
            return Some(STOPPED);
        }
        match stake_answer(self.account.cash, self.stake_usd, self.account.positions.len()) {
            StakeAnswer::Stake(_) => None,
            StakeAnswer::NotEnough => Some(LOW_MONEY),
            StakeAnswer::Stop => Some(STOPPED),
        }
    }

    pub fn account(&self) -> &DemoAccount {
        &self.account
    }
}
