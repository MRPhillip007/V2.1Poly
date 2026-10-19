//! The number of a coin for the fast decision (spec §5.3): earlier checks of other coins may still be
//! running when a coin's own check starts.

use v21_core::ranks::{rank_state, Earlier, RankState};

#[test]
fn no_earlier_check_is_rank_1() {
    assert_eq!(rank_state(&[], 2), RankState::Rank(1));
}

#[test]
fn two_settled_ok_make_a_third_coin() {
    assert_eq!(rank_state(&[Earlier::Ok, Earlier::Ok], 2), RankState::Rank(3));
}

#[test]
fn two_ok_and_a_pending_are_still_a_third_coin() {
    assert_eq!(rank_state(&[Earlier::Ok, Earlier::Pending, Earlier::Ok], 2), RankState::Rank(3));
}

#[test]
fn one_ok_and_one_pending_must_wait() {
    // the pending check decides between number 2 and number 3
    assert_eq!(rank_state(&[Earlier::Ok, Earlier::Pending], 2), RankState::Wait);
}

#[test]
fn one_pending_alone_cannot_make_a_third_coin() {
    assert_eq!(rank_state(&[Earlier::Pending], 2), RankState::Rank(1));
}

#[test]
fn two_pending_must_wait() {
    assert_eq!(rank_state(&[Earlier::Pending, Earlier::Pending], 2), RankState::Wait);
}

#[test]
fn failed_checks_do_not_count() {
    assert_eq!(rank_state(&[Earlier::Failed, Earlier::Ok], 2), RankState::Rank(2));
    assert_eq!(rank_state(&[Earlier::Failed, Earlier::Failed, Earlier::Pending], 2), RankState::Rank(1));
}
