//! Shared test defaults — single source of truth for constants used across
//! stress tests, integration tests, and multi-instance tests.
//!
//! Packing constants into structs eliminates per-file `#[cfg]` gates and
//! provides a discoverable location for test configuration.

/// Default audio parameters for generated WAV test fixtures.
///
/// All stress and integration tests that create synthetic WAV data
/// use these values unless a test-specific override is needed.
pub(crate) struct SawWav {
    pub sample_rate: u32,
    pub channels: u16,
    pub segment_size: usize,
    pub saw_period: usize,
}

impl SawWav {
    /// Standard defaults: 44.1 kHz stereo, 200 KB segments (native) / 32 KB (wasm).
    pub const DEFAULT: Self = Self {
        sample_rate: 44100,
        channels: 2,
        #[cfg(not(target_arch = "wasm32"))]
        segment_size: 200_000,
        #[cfg(target_arch = "wasm32")]
        segment_size: 32_000,
        saw_period: 65536,
    };

    /// Total bytes for a given segment count.
    pub const fn total_bytes(&self, segment_count: usize) -> usize {
        segment_count * self.segment_size
    }
}
