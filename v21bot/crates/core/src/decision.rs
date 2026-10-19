//! The rules of one window that look across coins: the numbering of the signals and the filters.
//! The port of `paper/strategy.py::WindowEngine`, with the v2.1 rule of the coins that trade.

use crate::detector::Check;
use crate::params::Params;
use crate::pyjson::py_float_repr;
use crate::window::utc_hour;
use crate::zscore::ZInfo;

/// The decision on a signal: trade it ...
pub const ENTER: &str = "вход";
/// ... or not; the reason says why not.
pub const SKIP: &str = "пропуск";

/// Why a signal trades or not: the first rule that stopped it, in the order they are tried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Enter,
    /// The ask `lat` seconds after the signal was above `max_fill`: the signal is not counted.
    FailedCheck,
    /// The signal's number in the window is above `max_coins`.
    ThirdCoin,
    /// The UTC hour of the entry moment is not one of `hours`.
    OutOfHours,
    /// The coin is watched (it counts in the numbering) but does not trade.
    CoinNotTraded,
    /// No z: the closes are not known, or sigma is 0.
    NoBinance,
    /// z below `z_min`.
    ZBelow,
}

impl Reason {
    /// The reason as the journal and the tables write it.
    pub fn label(self, p: &Params) -> String {
        match self {
            Reason::Enter => ENTER.to_string(),
            Reason::FailedCheck => {
                format!("ask > {} через {} с", py_float_repr(p.max_fill), py_float_repr(p.lat))
            }
            Reason::ThirdCoin => "третья монета".to_string(),
            Reason::OutOfHours => "вне часов".to_string(),
            Reason::CoinNotTraded => "монета не торгуется".to_string(),
            Reason::NoBinance => "нет данных Binance".to_string(),
            Reason::ZBelow => format!("z < {}", py_float_repr(p.z_min)),
        }
    }
}

/// What the window's rules decide on one check.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub check: Check,
    /// The check succeeded, so it takes a place in the order of the window's signals.
    pub counted: bool,
    /// That place: 1, 2, 3 ...; `None` if not counted.
    pub rank: Option<u32>,
    /// The distance filter as the caller computed it.
    pub z: Option<ZInfo>,
    /// The entry moment is in one of the allowed UTC hours.
    pub hour_ok: bool,
    pub trade: bool,
    pub reason: Reason,
}

/// The cross-coin rules of one window. Checks must come in the order of their numbers — the
/// backtest numbers the signals by time — since the rank is the order of the calls. The parameters
/// come with each call, so a live engine keeps its own and runs one of these per window.
pub struct WindowEngine {
    /// The window start, unix seconds.
    s: i64,
    /// Successful checks so far.
    counted: u32,
}

impl WindowEngine {
    pub fn new(s: i64) -> Self {
        WindowEngine { s, counted: 0 }
    }

    /// Whether to trade on a check under the parameters `p`, `z` being its distance filter.
    pub fn decide(&mut self, p: &Params, chk: &Check, z: Option<&ZInfo>) -> Decision {
        let rank = if chk.ok {
            self.counted += 1;
            Some(self.counted)
        } else {
            None
        };
        let (reason, hour_ok) = reason_for(p, self.s, chk, rank, z);
        Decision {
            check: *chk,
            counted: chk.ok,
            rank,
            z: z.copied(),
            hour_ok,
            trade: reason == Reason::Enter,
            reason,
        }
    }
}

/// The reason and `hour_ok` of a check whose number in window `s` is known: the order of
/// `WindowEngine::decide` without its count, for a decision taken before every earlier check has
/// settled. `rank` is the number of a successful check; a failed one has none and is always
/// `FailedCheck`.
pub fn reason_for(p: &Params, s: i64, chk: &Check, rank: Option<u32>, z: Option<&ZInfo>) -> (Reason, bool) {
    let hour = utc_hour(s, chk.t_trig);
    let hour_ok = p.hours.iter().any(|&(a, b)| a <= hour && hour < b);
    let reason = match z {
        _ if !chk.ok => Reason::FailedCheck,
        _ if rank.is_some_and(|r| r > p.max_coins) => Reason::ThirdCoin,
        _ if !hour_ok => Reason::OutOfHours,
        _ if !p.trade_coins.contains(&chk.coin) => Reason::CoinNotTraded,
        None => Reason::NoBinance,
        Some(zi) if zi.z.is_nan() => Reason::NoBinance,
        Some(zi) if zi.z < p.z_min => Reason::ZBelow,
        Some(_) => Reason::Enter,
    };
    (reason, hour_ok)
}
