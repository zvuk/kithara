/// Minimal xorshift64 PRNG — deterministic and reproducible.
///
/// Use a fixed seed to ensure test results are identical across runs.
pub struct Xorshift64(u64);

/// Bit pattern of `1.0_f64`.
const ONE_BITS: u64 = 0x3FF0_0000_0000_0000;

impl Xorshift64 {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Returns `f64` in `[0, 1)`: the top 52 bits fill the mantissa of a
    /// value in `[1, 2)`.
    pub fn next_f64(&mut self) -> f64 {
        f64::from_bits(ONE_BITS | (self.next_u64() >> 12)) - 1.0
    }

    pub const fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Returns `f64` in `[min, max)`.
    pub fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        (max - min).mul_add(self.next_f64(), min)
    }

    /// Returns `u64` in `[min, max)`.
    pub fn range_u64(&mut self, min: u64, max: u64) -> u64 {
        min + self.next_u64() % (max - min)
    }
}
