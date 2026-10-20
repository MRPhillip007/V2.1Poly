//! The health of the bot (spec §8): a `health` record a minute — the delivery lags of the feeds, the
//! time from a frame to a decision, the copies up, the frames and their duplicates, the messages the
//! queues lost, the clock, the round trip to the exchange, the CPU.

use std::time::Instant;

use serde_json::{json, Value};

/// Values counted by powers of two: bucket 0 holds 0, bucket k holds 2^(k−1) ..= 2^k − 1. Nothing is
/// allocated: the hot thread keeps these.
#[derive(Clone, Copy, Debug)]
pub struct Hist {
    counts: [u64; 65],
    n: u64,
    max: u64,
}

impl Default for Hist {
    fn default() -> Self {
        Hist { counts: [0; 65], n: 0, max: 0 }
    }
}

impl Hist {
    pub fn add(&mut self, v: u64) {
        self.counts[(u64::BITS - v.leading_zeros()) as usize] += 1;
        self.n += 1;
        self.max = self.max.max(v);
    }

    /// The top of the bucket that holds the value at quantile `q` (no more than the largest value): the
    /// true value is at most that, and more than half of it. 0 when empty.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let rank = ((q * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (k, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                let top = match k {
                    0 => 0,
                    64 => u64::MAX,
                    k => (1u64 << k) - 1,
                };
                return top.min(self.max);
            }
        }
        self.max
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// Values added.
    pub fn count(&self) -> u64 {
        self.n
    }
}

/// What the hot thread counts between two health records.
#[derive(Clone, Copy, Debug, Default)]
pub struct HotHealth {
    pub frames: u64,
    pub dup_frames: u64,
    /// Parts of frames that could not be read.
    pub bad_parts: u64,
    pub trades: u64,
    /// Messages a full queue lost.
    pub dropped_msgs: u64,
    /// From a frame's arrival to the decision on it.
    pub decide_ns: Hist,
    /// Arrival minus exchange time.
    pub pm_lag_ms: Hist,
    pub bn_lag_ms: Hist,
    pub copies_up: u8,
}

/// The `health` record of the journal.
pub fn health_record(h: &HotHealth, clock_offset_ms: Option<i64>, rtt_ms: Option<f64>, cpu_pct: Option<f64>) -> Value {
    json!({
        "pm_lag_ms_p50": h.pm_lag_ms.quantile(0.5),
        "pm_lag_ms_p99": h.pm_lag_ms.quantile(0.99),
        "bn_lag_ms_p50": h.bn_lag_ms.quantile(0.5),
        "bn_lag_ms_p99": h.bn_lag_ms.quantile(0.99),
        "decide_ns_p50": h.decide_ns.quantile(0.5),
        "decide_ns_p99": h.decide_ns.quantile(0.99),
        "decide_ns_max": h.decide_ns.max(),
        "copies_up": h.copies_up,
        "frames": h.frames,
        "dup_frames": h.dup_frames,
        "bad_parts": h.bad_parts,
        "trades": h.trades,
        "dropped_msgs": h.dropped_msgs,
        "clock_offset_ms": clock_offset_ms,
        "rtt_ms": rtt_ms,
        "cpu_pct": cpu_pct,
    })
}

/// Clock ticks a second of `/proc/self/stat` (USER_HZ: 100 on Linux).
const CLK_TCK: f64 = 100.0;

/// The CPU of this process between two samples, from `/proc/self/stat`.
#[derive(Debug, Default)]
pub struct CpuMeter {
    last: Option<(Instant, u64)>,
}

impl CpuMeter {
    /// The CPU used since the last sample, % of one core; none at the first sample and off Linux.
    pub fn sample(&mut self) -> Option<f64> {
        let ticks = proc_ticks()?;
        let now = Instant::now();
        let pct = self.last.map(|(at, before)| {
            let secs = now.duration_since(at).as_secs_f64();
            if secs > 0.0 { ticks.saturating_sub(before) as f64 / CLK_TCK / secs * 100.0 } else { 0.0 }
        });
        self.last = Some((now, ticks));
        pct
    }
}

/// User and system time of this process, clock ticks.
fn proc_ticks() -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // the name of the program is in parentheses and may hold spaces: the fields come after the last one
    let fields: Vec<&str> = stat.get(stat.rfind(')')? + 1..)?.split_whitespace().collect();
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}
