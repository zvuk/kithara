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
}

impl SawWav {
    /// Saw period in PCM frames (matches generated saw-tooth cycle length).
    pub(crate) const SAW_PERIOD: usize = 65536;

    /// Standard defaults: 44.1 kHz stereo, 200 KB segments (native) / 32 KB (wasm).
    pub(crate) const DEFAULT: Self = Self {
        sample_rate: 44100,
        channels: 2,
        #[cfg(not(target_arch = "wasm32"))]
        segment_size: 200_000,
        #[cfg(target_arch = "wasm32")]
        segment_size: 32_000,
    };

    pub(crate) fn default() -> Self {
        Self::DEFAULT
    }
}
