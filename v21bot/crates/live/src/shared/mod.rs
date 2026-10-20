//! What both threads of the bot use: the configuration, the secrets and the exchange's clock.

pub mod clock;
pub mod config;
pub mod msgs;
pub mod secrets;
pub mod tls;
pub mod utc;
