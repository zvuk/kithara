use std::num::NonZeroU32;

use kithara::platform::{sync::Arc, time::Duration};
use kithara_test_fixtures::signal;

/// Default audio parameters for generated WAV test fixtures.
///
/// All stress and integration tests that create synthetic WAV data
/// use these values unless a test-specific override is needed.
pub struct SawWav {
    pub sample_rate: u32,
    pub channels: u16,
    pub segment_size: usize,
}

impl SawWav {
    /// Bytes per mono PCM sample (16-bit signed).
    const BYTES_PER_SAMPLE: usize = 2;

    /// Standard defaults: 44.1 kHz stereo, 200 KB segments (native) / 32 KB (wasm).
    pub const DEFAULT: Self = Self {
        sample_rate: 44100,
        channels: 2,
        #[cfg(not(target_arch = "wasm32"))]
        segment_size: 200_000,
        #[cfg(target_arch = "wasm32")]
        segment_size: 32_000,
    };

    /// Duration of one `segment_size`-byte slice in seconds.
    ///
    /// `bytes / (sample_rate * channels * bytes_per_sample)`. The same
    /// expression is inlined in several HLS fixture builders.
    pub fn segment_duration_secs(&self) -> f64 {
        self.segment_size as f64
            / (f64::from(self.sample_rate)
                * f64::from(self.channels)
                * Self::BYTES_PER_SAMPLE as f64)
    }

    /// Byte count for `segments` consecutive segments of `segment_size`.
    pub const fn total_bytes(&self, segments: usize) -> usize {
        segments * self.segment_size
    }

    /// Generate a WAV blob sized to `segments * segment_size` bytes.
    pub fn build_wav(&self, segments: usize) -> Arc<Vec<u8>> {
        Arc::new(signal::wav_of_size(
            self.sample_rate,
            self.channels,
            self.total_bytes(segments),
            signal::TONE,
        ))
    }
}

/// Frames of interleaved 16-bit PCM that fill `segments` segments of
/// `segment_size` bytes.
#[must_use]
pub const fn frames_in_segments(segments: usize, segment_size: usize, channels: u16) -> usize {
    segments * segment_size / (channels as usize * size_of::<i16>())
}

/// Frames of PCM a packaged HLS variant carries.
///
/// A segment holds whole encoder frames, so the packager rounds the requested
/// segment length up to the next one. A fixture therefore carries more audio
/// than `segments * segment_duration` names, and a test that asks whether a
/// track played to its end has to expect the rounded figure. `None` on
/// overflow.
#[must_use]
pub fn packaged_content_frames(
    requested_segment_frames: usize,
    frame_samples: usize,
    segments: usize,
) -> Option<usize> {
    requested_segment_frames
        .div_ceil(frame_samples)
        .max(1)
        .checked_mul(frame_samples)?
        .checked_mul(segments)
}

impl Default for SawWav {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Cross-module scalar constants.
///
/// Items here were previously duplicated inline (`const SAMPLE_RATE`,
/// `SEGMENT_SIZE`, `EXPECTED_DURATION_SECS`, …) across `kithara_audio`,
/// `kithara_play`, `kithara_file`, and `kithara_hls` tests. Centralising
/// them keeps numeric drift impossible and makes it obvious which values
/// are shared vs. test-local.
///
/// New entries should be added only when the same value recurs in ≥2
/// modules. Genuinely local values stay in their own file.
pub struct Consts;

impl Consts {
    /// Default sample rate for generated WAV / expected streams.
    pub const SAMPLE_RATE: u32 = SawWav::DEFAULT.sample_rate;
    /// Typed form passed explicitly into playback configuration.
    pub const NON_ZERO_SAMPLE_RATE: NonZeroU32 = match NonZeroU32::new(Self::SAMPLE_RATE) {
        Some(sample_rate) => sample_rate,
        None => unreachable!(),
    };
    /// Default channel count.
    pub const CHANNELS: u16 = SawWav::DEFAULT.channels;
    /// Default packaged HLS segment size (bytes).
    pub const SEGMENT_SIZE: usize = SawWav::DEFAULT.segment_size;

    /// Nominal duration of `signal_mp3_track_sine440_187s`, the full-length
    /// MPEG clip, in seconds. The generator renders exactly this many seconds;
    /// decoders report it back within their own priming and tail slack.
    pub const TEST_MP3_DURATION_SECS: f64 = 187.0;

    /// Default soft read timeout for resource/decoder integration tests.
    pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
}
