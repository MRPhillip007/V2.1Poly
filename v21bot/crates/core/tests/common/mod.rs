//! Helpers shared by the integration tests of `v21-core`.
#![allow(dead_code)] // each test file uses its own part

use v21_core::params::{load_strategy, Strategy};

pub const V2: &str = include_str!("../data/strategy_v2.json");
pub const V21: &str = include_str!("../data/strategy_v2_1.json");

/// A window start (a multiple of 900): 2026-10-02 03:15 UTC.
pub const S: i64 = 1_790_910_900;

pub fn strategy() -> Strategy {
    load_strategy(V2, V21).unwrap()
}

/// A small deterministic generator (xorshift64*), as `pmbot/src/hot/mod.rs::testing::Rng`.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() >> 33).unwrap() % n
    }

    /// Uniform in [0, 1).
    pub fn random(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn choice<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    /// `randrange(a, b)`: a..b, b excluded.
    pub fn range(&mut self, a: i64, b: i64) -> i64 {
        a + self.below(usize::try_from(b - a).unwrap()) as i64
    }
}

/// 1-s closes of the seconds `first..=last`: a random walk near 100 with steps of up to ±2 bp, and
/// one second in five without trades (its close repeats the previous one).
pub fn random_closes(first: i64, last: i64, seed: u64) -> Vec<f64> {
    let mut rng = Rng::new(seed);
    let mut price = 100.0;
    (first..=last)
        .map(|_| {
            if rng.random() >= 0.2 {
                price *= 1.0 + (rng.random() - 0.5) * 4e-4;
            }
            price
        })
        .collect()
}
