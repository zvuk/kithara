//! Deterministic PRNG for reproducible stress tests.

/// Minimal xorshift64 PRNG — deterministic and reproducible.
///
/// Use a fixed seed to ensure test results are identical across runs.
pub struct Xorshift64(u64);

impl Xorshift64 {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Returns `f64` in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Returns `f64` in `[min, max)`.
    pub fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.next_f64()
    }

    /// Returns `u64` in `[min, max)`.
    pub fn range_u64(&mut self, min: u64, max: u64) -> u64 {
        min + ((max - min) as f64 * self.next_f64()) as u64
    }
}
