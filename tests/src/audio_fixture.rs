//! Test fixtures for decode tests.
//!
//! Provides deterministic local fixtures for decode tests (no external network).
//! Includes tiny MP3/AAC test assets embedded in the test binary.

/// Embedded audio data for tests that don't need HTTP
pub struct EmbeddedAudio {
    /// WAV data (0.1 seconds of silence)
    wav: &'static [u8],
    /// MP3 data (test audio clip)
    mp3: &'static [u8],
}

impl EmbeddedAudio {
    /// A tiny WAV file (0.1 seconds of silence, 44.1kHz, stereo)
    /// This is a minimal valid WAV file for testing.
    const TINY_WAV_BYTES: &'static [u8] = include_bytes!("../../assets/silence_1s.wav");

    /// A test MP3 file (short audio clip)
    const TEST_MP3_BYTES: &'static [u8] = include_bytes!("../../assets/test.mp3");

    /// Get the embedded audio data
    pub fn get() -> Self {
        Self {
            wav: Self::TINY_WAV_BYTES,
            mp3: Self::TEST_MP3_BYTES,
        }
    }

    /// Get WAV data
    pub fn wav(&self) -> &'static [u8] {
        self.wav
    }

    /// Get MP3 data
    pub fn mp3(&self) -> &'static [u8] {
        self.mp3
    }
}
