//! The number of a coin's check for the fast decision (spec §5.3). The fast decision is taken when
//! the check starts, while earlier checks of other coins may still be waiting for their price; the
//! number is known as soon as no outcome of those can change the decision.

/// A check of another coin of the window whose key (`t_trig`, Up token id) is below ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Earlier {
    /// Settled and successful: it holds a number.
    Ok,
    /// Settled and failed: it holds none.
    Failed,
    /// Still waiting for its price (or a coin of the window whose market is unknown).
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankState {
    /// The number the decision is taken with. A pending check can still raise the true number,
    /// but only within `max_coins`, so the decision stays the same.
    Rank(u32),
    /// The pending checks decide whether the number is within `max_coins`: wait for them.
    Wait,
}

/// With `k` settled successful and `n` pending earlier checks: `max_coins` or more successful
/// ones make the check a third coin whatever the pending ones give; fewer than `max_coins`
/// together keep it within `max_coins`; anything else waits.
pub fn rank_state(earlier: &[Earlier], max_coins: u32) -> RankState {
    let count = |kind: Earlier| u32::try_from(earlier.iter().filter(|&&e| e == kind).count()).unwrap_or(u32::MAX);
    let (k, n) = (count(Earlier::Ok), count(Earlier::Pending));
    if k >= max_coins || k.saturating_add(n) < max_coins {
        RankState::Rank(k + 1)
    } else {
        RankState::Wait
    }
}
