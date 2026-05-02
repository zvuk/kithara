//! Shared test defaults — single source of truth for constants used across
//! stress tests, integration tests, and multi-instance tests.
//!
//! Two namespaces:
//!
//! * [`SawWav`] — audio defaults (`sample_rate`, `channels`, `segment_size`)
//!   plus helpers for derived values (segment duration, WAV blob size).
//! * [`Consts`] — scalar constants that were duplicated across modules
//!   (segment counts, read timeouts, MP3 fixture sizes). Consumers use
//!   `Consts::FOO` so there is one editable source of truth.
//!
//! Packing constants into structs eliminates per-file `#[cfg]` gates and
//! provides a discoverable location for test configuration.

// Shared test fixture: items below are used by some test binaries but not all.
// `cargo build --all-targets` flags them as dead per-binary.
#![allow(dead_code)]

use std::sync::Arc;

use kithara_integration_tests::audio_fixture::EmbeddedAudio;
use kithara_platform::time::Duration;
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

/// Cross-module scalar constants.
///
/// Items here were previously duplicated inline (`const SAMPLE_RATE`,
/// `TEST_MP3_BYTES`, `EXPECTED_DURATION_SECS`, …) across `kithara_audio`,
/// `kithara_play`, `kithara_file`, and `kithara_hls` tests. Centralising
/// them keeps numeric drift impossible and makes it obvious which values
/// are shared vs. test-local.
///
/// New entries should be added only when the same value recurs in ≥2
/// modules. Genuinely local values stay in their own file.
pub(crate) struct Consts;

impl Consts {
    // --- Audio format (mirror of `SawWav::DEFAULT`) -----------------------

    /// Default sample rate for generated WAV / expected streams.
    pub(crate) const SAMPLE_RATE: u32 = SawWav::DEFAULT.sample_rate;
    /// Default channel count.
    pub(crate) const CHANNELS: u16 = SawWav::DEFAULT.channels;
    /// Default packaged HLS segment size (bytes).
    pub(crate) const SEGMENT_SIZE: usize = SawWav::DEFAULT.segment_size;

    // --- Embedded MP3 fixture --------------------------------------------

    /// Bytes of the `assets/test.mp3` fixture.
    pub(crate) const TEST_MP3_BYTES: &'static [u8] = EmbeddedAudio::TEST_MP3_BYTES;
    /// Nominal duration of [`Self::TEST_MP3_BYTES`] in seconds.
    pub(crate) const TEST_MP3_DURATION_SECS: f64 = EmbeddedAudio::MP3_EXPECTED_DURATION_SECS;

    // --- Common test budgets ---------------------------------------------

    /// Default soft read timeout for resource/decoder integration tests.
    pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(5);
    /// Block size used by offline render harnesses (audio block budget).
    pub(crate) const OFFLINE_BLOCK_FRAMES: usize = 512;
}
