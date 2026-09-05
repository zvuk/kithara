use std::num::NonZeroU32;

use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;

use crate::AudioSpec;

/// Position and provenance facts for one decoded-audio chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioChunkInfo {
    /// Decoded-audio format.
    pub spec: AudioSpec,
    /// Media-timeline position after this chunk's frames have played out.
    pub end_timestamp: Duration,
    /// Media-timeline position of the first frame in this chunk.
    pub timestamp: Duration,
    /// Opaque source segment index reported by the decoder, when available.
    pub segment_index: Option<u32>,
    /// Absolute byte offset reported by the decoder when available.
    pub source_byte_offset: Option<u64>,
    /// Opaque source variant index reported by the decoder, when available.
    pub variant_index: Option<usize>,
    /// Number of interleaved audio frames represented by this chunk.
    pub frames: u32,
    /// Decoder generation, incremented on decoder recreation.
    pub epoch: u64,
    /// Opaque producer render revision represented by this chunk.
    pub render_revision: u64,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Source bytes that produced this chunk, or zero when unknown.
    pub source_bytes: u64,
}

impl Default for AudioChunkInfo {
    fn default() -> Self {
        const PLACEHOLDER_RATE: NonZeroU32 = match NonZeroU32::new(48_000) {
            Some(rate) => rate,
            None => unreachable!(),
        };

        Self {
            spec: AudioSpec::new(0, PLACEHOLDER_RATE),
            end_timestamp: Duration::ZERO,
            timestamp: Duration::ZERO,
            segment_index: None,
            source_byte_offset: None,
            variant_index: None,
            frames: 0,
            epoch: 0,
            render_revision: 0,
            frame_offset: 0,
            source_bytes: 0,
        }
    }
}

/// One owning chunk of interleaved decoded samples and timeline information.
#[derive(Debug)]
pub struct AudioChunk {
    pub meta: AudioChunkInfo,
    pub samples: SampleBuffer,
}

impl AudioChunk {
    #[must_use]
    pub const fn new(meta: AudioChunkInfo, samples: SampleBuffer) -> Self {
        Self { meta, samples }
    }

    /// Number of complete audio frames in this chunk.
    #[must_use]
    pub fn frames(&self) -> usize {
        let channels = self.meta.spec.channels as usize;
        self.samples.len().checked_div(channels).unwrap_or(0)
    }

    #[must_use]
    pub const fn spec(&self) -> AudioSpec {
        self.meta.spec
    }
}

impl AsRef<[f32]> for AudioChunk {
    fn as_ref(&self) -> &[f32] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{Pools, pools, sample_buffer};

    fn audio_spec(channels: u16, sample_rate: u32) -> AudioSpec {
        AudioSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("test rate must be non-zero"),
        )
    }

    fn chunk(pools: &Pools, spec: AudioSpec, samples: Vec<f32>) -> AudioChunk {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                ..Default::default()
            },
            sample_buffer(pools, &samples),
        )
    }

    #[kithara::test]
    #[case(44_100, 2, "44100 Hz, 2 channels")]
    #[case(48_000, 1, "48000 Hz, 1 channels")]
    fn audio_spec_display(#[case] sample_rate: u32, #[case] channels: u16, #[case] expected: &str) {
        assert_eq!(audio_spec(channels, sample_rate).to_string(), expected);
    }

    #[kithara::test]
    fn chunk_reports_complete_frames() {
        let pools = pools();
        assert_eq!(
            chunk(&pools, audio_spec(2, 44_100), vec![0.0; 6]).frames(),
            3
        );
    }

    #[kithara::test]
    fn zero_channels_report_no_frames() {
        let pools = pools();
        assert_eq!(
            chunk(&pools, audio_spec(0, 44_100), vec![0.0; 4]).frames(),
            0
        );
    }

    #[kithara::test]
    fn metadata_default_preserves_placeholder_contract() {
        let info = AudioChunkInfo::default();
        assert_eq!(info.spec.sample_rate.get(), 48_000);
        assert_eq!(info.spec.channels, 0);
        assert_eq!(info.frame_offset, 0);
        assert_eq!(info.timestamp, Duration::ZERO);
    }
}
