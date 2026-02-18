use std::{fmt, sync::Arc, time::Duration};

use derivative::Derivative;
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

/// Timeline metadata for a PCM chunk.
///
/// Combines audio format specification with position on the logical timeline.
/// Each chunk gets unique timeline coordinates; `PcmSpec` is the static part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcmMeta {
    /// Audio format (channels, sample rate).
    pub spec: PcmSpec,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Timestamp of the first frame in this chunk.
    pub timestamp: Duration,
    /// Segment index within playlist (`None` for progressive files).
    pub segment_index: Option<u32>,
    /// Variant/quality level index (`None` for progressive files).
    pub variant_index: Option<usize>,
    /// Decoder generation — increments on each ABR switch / decoder recreation.
    pub epoch: u64,
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
#[derive(Clone, Debug, Derivative)]
#[derivative(Default)]
pub struct PcmChunk {
    #[derivative(Default(value = "pcm_pool().get()"))]
    pub pcm: PcmBuf,
    pub meta: PcmMeta,
}

impl PcmChunk {
    /// Create a new `PcmChunk` from a pool-backed buffer.
    #[must_use]
    pub fn new(meta: PcmMeta, pcm: PcmBuf) -> Self {
        Self { pcm, meta }
    }

    /// Audio format specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.meta.spec
    }

    /// Number of audio frames in this chunk.
    ///
    /// A frame contains one sample per channel.
    #[must_use]
    pub fn frames(&self) -> usize {
        let channels = self.meta.spec.channels as usize;
        self.pcm.len().checked_div(channels).unwrap_or(0)
    }

    /// Get reference to raw samples.
    #[must_use]
    pub fn samples(&self) -> &[f32] {
        &self.pcm
    }
}

#[cfg(test)]
mod tests {
    use rstest::*;

    use super::*;

    fn test_chunk(spec: PcmSpec, pcm: Vec<f32>) -> PcmChunk {
        PcmChunk::new(
            PcmMeta {
                spec,
                ..Default::default()
            },
            pcm_pool().attach(pcm),
        )
    }

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

    // PcmMeta Tests

    #[test]
    fn test_pcm_meta_default() {
        let meta = PcmMeta::default();
        assert_eq!(meta.spec, PcmSpec::default());
        assert_eq!(meta.frame_offset, 0);
        assert_eq!(meta.timestamp, Duration::ZERO);
        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);
        assert_eq!(meta.epoch, 0);
    }

    #[test]
    fn test_pcm_meta_copy() {
        let meta = PcmMeta {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            frame_offset: 1000,
            timestamp: Duration::from_millis(22),
            segment_index: Some(5),
            variant_index: Some(2),
            epoch: 3,
        };
        let copied = meta;
        assert_eq!(meta, copied);
    }

    #[test]
    fn test_pcm_meta_with_spec() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 48000,
        };
        let meta = PcmMeta {
            spec,
            ..Default::default()
        };
        assert_eq!(meta.spec, spec);
        assert_eq!(meta.frame_offset, 0);
    }

    #[test]
    fn test_pcm_meta_partial_eq() {
        let a = PcmMeta {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            frame_offset: 100,
            timestamp: Duration::from_millis(2),
            segment_index: Some(1),
            variant_index: Some(0),
            epoch: 1,
        };
        let mut b = a;
        assert_eq!(a, b);
        b.frame_offset = 200;
        assert_ne!(a, b);
    }

    // PcmChunk Tests

    #[test]
    fn test_pcm_chunk_new() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1f32, 0.2, 0.3, 0.4];
        let chunk = test_chunk(spec, pcm.clone());

        assert_eq!(chunk.spec(), spec);
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
        let chunk = test_chunk(spec, pcm);
        assert_eq!(chunk.frames(), expected_frames);
    }

    #[test]
    fn test_frames_zero_channels() {
        let spec = PcmSpec {
            channels: 0,
            sample_rate: 44100,
        };
        let chunk = test_chunk(spec, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(chunk.frames(), 0);
    }

    #[test]
    fn test_samples_access() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = test_chunk(spec, pcm.clone());

        let samples = chunk.samples();
        assert_eq!(samples.len(), 4);
        assert_eq!(samples, &pcm[..]);
    }

    #[test]
    fn test_pcm_chunk_clone() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = test_chunk(spec, pcm);
        let cloned = chunk.clone();

        assert_eq!(cloned.spec(), chunk.spec());
        assert_eq!(cloned.pcm, chunk.pcm);
    }

    #[test]
    fn test_pcm_chunk_debug() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1f32, 0.2];
        let chunk = test_chunk(spec, pcm);
        let debug_str = format!("{:?}", chunk);

        assert!(debug_str.contains("PcmChunk"));
    }
}
