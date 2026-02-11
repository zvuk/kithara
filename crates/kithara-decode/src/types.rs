use std::{fmt, sync::Arc};

use kithara_bufpool::{PcmBuf, pcm_pool};

/// Audio track metadata extracted from Symphonia tags.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    /// Album name.
    pub album: Option<String>,
    /// Artist name.
    pub artist: Option<String>,
    /// Album artwork (JPEG/PNG bytes).
    pub artwork: Option<Arc<Vec<u8>>>,
    /// Track title.
    pub title: Option<String>,
}

/// PCM specification - core audio format information
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcmSpec {
    pub channels: u16,
    pub sample_rate: u32,
}

impl fmt::Display for PcmSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz, {} channels", self.sample_rate, self.channels)
    }
}

/// PCM chunk containing interleaved audio samples with automatic pool recycling.
///
/// The `pcm` buffer is pool-backed via [`PcmBuf`]: when the chunk is dropped,
/// the buffer returns to the global PCM pool for reuse instead of being deallocated.
///
/// # Invariants
/// - `pcm.len() % channels == 0` (frame-aligned)
/// - `spec.channels > 0` and `spec.sample_rate > 0`
/// - All samples are f32 and interleaved (LRLRLR...)
#[derive(Clone, Debug)]
pub struct PcmChunk {
    pub pcm: PcmBuf,
    pub spec: PcmSpec,
}

impl Default for PcmChunk {
    fn default() -> Self {
        Self {
            pcm: pcm_pool().get(),
            spec: PcmSpec::default(),
        }
    }
}

impl PcmChunk {
    /// Create a new `PcmChunk` from a raw `Vec<f32>`.
    ///
    /// The Vec is attached to the global PCM pool for automatic recycling on drop.
    pub fn new(spec: PcmSpec, pcm: Vec<f32>) -> Self {
        Self {
            pcm: pcm_pool().attach(pcm),
            spec,
        }
    }

    /// Create a new `PcmChunk` from a pool-backed buffer (zero overhead).
    ///
    /// Use this when the buffer was already obtained from the pool
    /// (e.g., via `pcm_pool().get_with()`).
    pub fn with_pooled(spec: PcmSpec, pcm: PcmBuf) -> Self {
        Self { pcm, spec }
    }

    /// Number of audio frames in this chunk
    ///
    /// A frame contains one sample per channel
    pub fn frames(&self) -> usize {
        let channels = self.spec.channels as usize;
        if channels == 0 {
            0
        } else {
            self.pcm.len() / channels
        }
    }

    /// Duration of this chunk in seconds
    pub fn duration_secs(&self) -> f64 {
        if self.spec.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.spec.sample_rate as f64
        }
    }

    /// Get reference to raw samples.
    pub fn samples(&self) -> &[f32] {
        &self.pcm
    }

    /// Consume chunk and return the pooled buffer.
    pub fn into_samples(self) -> PcmBuf {
        self.pcm
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    // PcmSpec Tests

    #[rstest]
    #[case(44100, 2, "44100 Hz, 2 channels")]
    #[case(48000, 1, "48000 Hz, 1 channels")]
    #[case(96000, 6, "96000 Hz, 6 channels")]
    #[case(192000, 8, "192000 Hz, 8 channels")]
    #[case(0, 0, "0 Hz, 0 channels")] // Edge case
    fn test_pcm_spec_display(
        #[case] sample_rate: u32,
        #[case] channels: u16,
        #[case] expected: &str,
    ) {
        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        assert_eq!(format!("{}", spec), expected);
    }

    #[test]
    fn test_pcm_spec_clone() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let cloned = spec;
        assert_eq!(spec, cloned);
    }

    #[rstest]
    #[case(44100, 2, 44100, 2, true)]
    #[case(44100, 2, 48000, 2, false)]
    #[case(44100, 2, 44100, 1, false)]
    #[case(0, 0, 0, 0, true)]
    fn test_pcm_spec_partial_eq(
        #[case] sr1: u32,
        #[case] ch1: u16,
        #[case] sr2: u32,
        #[case] ch2: u16,
        #[case] should_equal: bool,
    ) {
        let spec1 = PcmSpec {
            channels: ch1,
            sample_rate: sr1,
        };
        let spec2 = PcmSpec {
            channels: ch2,
            sample_rate: sr2,
        };
        assert_eq!(spec1 == spec2, should_equal);
    }

    #[test]
    fn test_pcm_spec_debug() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let debug_str = format!("{:?}", spec);
        assert!(debug_str.contains("PcmSpec"));
        assert!(debug_str.contains("44100"));
        assert!(debug_str.contains("2"));
    }

    #[rstest]
    #[case(44100, 2)]
    #[case(48000, 1)]
    #[case(96000, 6)]
    fn test_pcm_spec_copy_trait(#[case] sample_rate: u32, #[case] channels: u16) {
        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        let copied = spec; // Copy, not move
        assert_eq!(spec, copied);
    }

    // PcmChunk Tests

    #[test]
    fn test_pcm_chunk_new() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1f32, 0.2, 0.3, 0.4];
        let chunk = PcmChunk::new(spec, pcm.clone());

        assert_eq!(chunk.spec, spec);
        assert_eq!(chunk.samples(), &pcm[..]);
    }

    #[rstest]
    #[case(vec![0.0, 1.0, 2.0, 3.0], 2, 2)] // 4 samples, 2 channels = 2 frames
    #[case(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0], 2, 3)] // 6 samples, 2 channels = 3 frames
    #[case(vec![0.0], 1, 1)] // 1 sample, 1 channel = 1 frame
    #[case(vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0], 6, 1)] // 6 samples, 6 channels = 1 frame
    #[case(vec![], 2, 0)] // Empty PCM = 0 frames
    fn test_frames_calculation(
        #[case] pcm: Vec<f32>,
        #[case] channels: u16,
        #[case] expected_frames: usize,
    ) {
        let spec = PcmSpec {
            channels,
            sample_rate: 44100,
        };
        let chunk = PcmChunk::new(spec, pcm);
        assert_eq!(chunk.frames(), expected_frames);
    }

    #[test]
    fn test_frames_zero_channels() {
        let spec = PcmSpec {
            channels: 0,
            sample_rate: 44100,
        };
        let chunk = PcmChunk::new(spec, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(chunk.frames(), 0);
    }

    #[rstest]
    #[case(vec![0.0; 44100 * 2], 44100, 2, 1.0)] // 1 second stereo
    #[case(vec![0.0; 48000], 48000, 1, 1.0)] // 1 second mono
    #[case(vec![0.0; 88200], 44100, 2, 1.0)] // 1 second stereo (88200 samples / 2 ch)
    #[case(vec![0.0; 44100], 44100, 2, 0.5)] // 0.5 seconds stereo
    #[case(vec![], 44100, 2, 0.0)] // Empty = 0 duration
    fn test_duration_secs(
        #[case] pcm: Vec<f32>,
        #[case] sample_rate: u32,
        #[case] channels: u16,
        #[case] expected_duration: f64,
    ) {
        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        let chunk = PcmChunk::new(spec, pcm);
        let duration = chunk.duration_secs();
        assert!((duration - expected_duration).abs() < 1e-6);
    }

    #[test]
    fn test_duration_secs_zero_sample_rate() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 0,
        };
        let chunk = PcmChunk::new(spec, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(chunk.duration_secs(), 0.0);
    }

    #[test]
    fn test_samples_access() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = PcmChunk::new(spec, pcm.clone());

        let samples = chunk.samples();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples, &pcm[..]);
    }

    #[test]
    fn test_into_samples() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = PcmChunk::new(spec, pcm.clone());

        let extracted = chunk.into_samples();
        assert_eq!(extracted.len(), 4);
        assert_eq!(&extracted[..], &pcm[..]);
    }

    #[test]
    fn test_pcm_chunk_clone() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = PcmChunk::new(spec, pcm);
        let cloned = chunk.clone();

        assert_eq!(cloned.spec, chunk.spec);
        assert_eq!(cloned.pcm, chunk.pcm);
    }

    #[test]
    fn test_pcm_chunk_debug() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1f32, 0.2];
        let chunk = PcmChunk::new(spec, pcm);
        let debug_str = format!("{:?}", chunk);

        assert!(debug_str.contains("PcmChunk"));
    }
}
