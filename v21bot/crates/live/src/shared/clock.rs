//! The exchange's clock: this machine's clock plus the measured offset, shared by the threads. A
//! home PC can be a second off; the moments of a window (the 840.000 s shot above all) are taken by
//! this clock. The offset is measured by the cold runtime (NTP, else Binance's time).

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// This machine's clock, unix microseconds.
fn local_us() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    i64::try_from(since.as_micros()).unwrap_or(i64::MAX)
}

pub struct ExchangeClock {
    /// Exchange time minus this machine's time, ms.
    offset_ms: AtomicI64,
    /// The offset has been measured.
    known: AtomicBool,
}

impl Default for ExchangeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl ExchangeClock {
    /// A clock whose offset is not known yet: it reads this machine's time.
    pub fn new() -> ExchangeClock {
        ExchangeClock { offset_ms: AtomicI64::new(0), known: AtomicBool::new(false) }
    }

    /// The exchange's time, unix ms.
    pub fn now_ms(&self) -> i64 {
        self.now_us().div_euclid(1000)
    }

    /// The exchange's time, unix µs.
    pub fn now_us(&self) -> i64 {
        local_us() + self.offset_ms.load(Ordering::Relaxed) * 1000
    }

    pub fn set_offset_ms(&self, ms: i64) {
        self.offset_ms.store(ms, Ordering::Relaxed);
        self.known.store(true, Ordering::Release);
    }

    /// The offset, if it has been measured.
    pub fn offset_ms(&self) -> Option<i64> {
        self.known.load(Ordering::Acquire).then(|| self.offset_ms.load(Ordering::Relaxed))
    }
}

/// The pause of spec §7.4 for the clock: `None` if the offset is known and within
/// `max_offset_ms`, else the reason.
pub fn clock_pause(offset_ms: Option<i64>, max_offset_ms: i64) -> Option<&'static str> {
    match offset_ms {
        None => Some("пауза: часы не измерены"),
        Some(ms) if ms.abs() > max_offset_ms => Some("пауза: часы сбиты"),
        Some(_) => None,
    }
}
