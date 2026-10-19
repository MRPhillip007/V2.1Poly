//! Strategy v2.1 of the Polymarket 15-minute Up/Down markets, without network, clock or files.
//!
//! The archive replay and the live bot call the same functions of this crate, so a replay that
//! reproduces the Python backtest is proof that the live bot decides by the same rules. Time and
//! events come in as arguments.

pub mod closes;
pub mod decision;
pub mod detector;
pub mod numpy;
pub mod params;
pub mod pyjson;
pub mod ranks;
pub mod returns;
pub mod stats;
pub mod window;
pub mod zscore;
