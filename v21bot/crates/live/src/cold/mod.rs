//! The cold runtime: Binance over REST, Gamma and the CLOB, the clock, the exact engine, the demo
//! account, the journal and Telegram.

use std::time::Duration;

pub mod binance_rest;
pub mod clob_info;
pub mod csv;
pub mod demo;
pub mod exact;
pub mod gamma;
pub mod journal;
pub mod ntp;
pub mod records;
pub mod runtime;
pub mod telegram;

pub use runtime::run_cold;

/// The HTTP client of the cold runtime: rustls on ring, a 10-s timeout per request.
pub fn http_client() -> reqwest::Client {
    crate::shared::tls::init();
    reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("an HTTP client with rustls on ring")
}
