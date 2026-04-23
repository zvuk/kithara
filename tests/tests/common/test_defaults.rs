//! Shared test defaults — single source of truth for constants used across
//! stress tests, integration tests, and multi-instance tests.
//!
//! Packing constants into structs eliminates per-file `#[cfg]` gates and
//! provides a discoverable location for test configuration.

use std::sync::Arc;

use kithara_test_utils::wav::create_test_wav;

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
    /// Standard RIFF/WAVE header size.
    const WAV_HEADER_SIZE: usize = 44;
    /// Bytes per mono PCM sample (16-bit signed).
    const BYTES_PER_SAMPLE: usize = 2;

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

    /// Duration of one `segment_size`-byte slice in seconds.
    ///
    /// `bytes / (sample_rate * channels * bytes_per_sample)`. The same
    /// expression is inlined in several HLS fixture builders.
    pub(crate) fn segment_duration_secs(&self) -> f64 {
        self.segment_size as f64
            / (f64::from(self.sample_rate)
                * f64::from(self.channels)
                * Self::BYTES_PER_SAMPLE as f64)
    }

    /// Byte count for `segments` consecutive segments of `segment_size`.
    pub(crate) fn total_bytes(&self, segments: usize) -> usize {
        segments * self.segment_size
    }

    /// Generate a WAV blob sized to `segments * segment_size` bytes.
    ///
    /// Uses [`create_test_wav`] with a sine wave; frame count is derived so
    /// the header + PCM body fit within the total byte budget.
    pub(crate) fn build_wav(&self, segments: usize) -> Arc<Vec<u8>> {
        let total = self.total_bytes(segments);
        let bytes_per_frame = self.channels as usize * Self::BYTES_PER_SAMPLE;
        let frames = (total - Self::WAV_HEADER_SIZE) / bytes_per_frame;
        Arc::new(create_test_wav(frames, self.sample_rate, self.channels))
    }
}
