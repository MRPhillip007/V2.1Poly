//! What both engines are armed with, and what the hot thread and the cold runtime say to each other.

use v21_core::fill::FeeSchedule;
use v21_core::window::{Coin, Side};
use v21_core::zscore::ZInfo;

/// A coin of the window, as both engines need it.
#[derive(Clone, Debug, PartialEq)]
pub struct CoinArm {
    pub coin: Coin,
    /// The numbers of its Up and Down tokens: their places in `WindowArm::tokens`.
    pub up_ix: u8,
    pub down_ix: u8,
    /// The Up token's id as 32 bytes, big-endian: the order of checks at one `t_trig`, as the archive's.
    pub up_key: [u8; 32],
    pub fee: FeeSchedule,
    /// The market's rules are those of spec §7.4, so the coin may trade (if the strategy trades it).
    pub tradable: bool,
    /// What differs, when the rules do.
    pub note: String,
}

/// A window, as both engines need it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowArm {
    /// The window start, unix seconds.
    pub s: i64,
    /// The token ids of the subscription, in the order of the token numbers.
    pub tokens: Vec<String>,
    /// The watched coins that have a market, in the order of the coins.
    pub coins: Vec<CoinArm>,
    /// Watched coins without a market: an earlier check of theirs cannot be ruled out.
    pub missing: Vec<Coin>,
    /// The bot started after the 835th second: the window begins as after a drop of both copies.
    pub resumed: bool,
    /// Coins already bought in this window (from state.json after a restart).
    pub traded: Vec<Coin>,
}

/// A shot of the fast engine (in milestone 2: a demo order).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShotRecord {
    pub s: i64,
    pub coin: Coin,
    pub side: Side,
    pub t_trig: f64,
    /// The exchange time of the event the check started on.
    pub event_ts_ms: i64,
    /// When the order would have been written to the socket, by the exchange's clock.
    pub send_ms: i64,
    pub rank: u32,
    pub z: Option<ZInfo>,
    /// From the frame's arrival to the decision, ns (the hot loop fills it in).
    pub frame_to_decision_ns: u64,
}

/// The 32-byte big-endian form of a decimal token id (a uint256), as the archive stores the ids; `None`
/// if the text is not a number that fits.
pub fn token_key(id: &str) -> Option<[u8; 32]> {
    if id.is_empty() {
        return None;
    }
    let mut key = [0u8; 32];
    for c in id.bytes() {
        let mut carry = u32::from(c.checked_sub(b'0').filter(|d| *d < 10)?);
        for byte in key.iter_mut().rev() {
            let v = u32::from(*byte) * 10 + carry;
            *byte = (v & 0xff) as u8;
            carry = v >> 8;
        }
        if carry != 0 {
            return None;
        }
    }
    Some(key)
}
