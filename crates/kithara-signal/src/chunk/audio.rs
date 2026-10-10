use std::{
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
};

use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;

use crate::{AudioSpec, SegmentId, SourceSpan};

/// Position and provenance facts for one decoded-audio chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioChunkInfo {
    /// Decoded-audio format.
    pub spec: AudioSpec,
    /// Media-timeline position after this chunk's frames have played out.
    pub end_timestamp: Duration,
    /// Media-timeline position of the first frame in this chunk.
    pub timestamp: Duration,
    /// Exact decoded-source mapping of this physical output interval.
    pub source_span: Option<SourceSpan>,
    /// Opaque immutable source/output mapping revision, absent for unmapped PCM.
    pub mapping_revision: Option<NonZeroU64>,
    /// Opaque source segment index reported by the decoder, when available.
    pub segment_index: Option<u32>,
    /// Absolute byte offset reported by the decoder when available.
    pub source_byte_offset: Option<u64>,
    /// Opaque source variant index reported by the decoder, when available.
    pub variant_index: Option<usize>,
    /// Number of interleaved audio frames represented by this chunk.
    pub frames: u32,
    /// Lane segment represented by this chunk.
    pub segment: SegmentId,
    /// First frame of this chunk on the segment's lane timeline.
    pub lane_frame: u64,
    /// Whether this chunk ends the track.
    pub end_of_track: bool,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Opaque producer render revision represented by this chunk.
    pub render_revision: u64,
    /// Source bytes that produced this chunk, or zero when unknown.
    pub source_bytes: u64,
}

impl AudioChunkInfo {
    /// The decoded source frames this chunk carries, `[start, end)`.
    ///
    /// Read off the chunk's own metadata: `frame_offset` is absolute from the
    /// start of the track and a seek landing rewrites it to the landed frame,
    /// so a span never depends on arrival order.
    #[must_use]
    pub const fn frame_range(&self) -> Range<u64> {
        self.frame_offset..self.frame_offset.saturating_add(self.frames as u64)
    }
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
            source_span: None,
            segment_index: None,
            source_byte_offset: None,
            variant_index: None,
            frames: 0,
            segment: SegmentId::FIRST,
            lane_frame: 0,
            end_of_track: false,
            render_revision: 0,
            mapping_revision: None,
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

    use kithara_core_test_fixtures::silence_pcm;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{Pools, pools, sample_buffer};

    fn audio_spec(channels: u16, sample_rate: u32) -> AudioSpec {
        AudioSpec::new(
            channels,
            NonZeroU32::new(sample_rate).expect("test rate must be non-zero"),
        )
    }

    fn chunk(pools: &Pools, spec: AudioSpec, samples: &[f32]) -> AudioChunk {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                ..Default::default()
            },
            sample_buffer(pools, samples),
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
        let silence_pcm = silence_pcm();
        let pools = pools();
        assert_eq!(
            chunk(&pools, audio_spec(2, 44_100), &silence_pcm).frames(),
            3
        );
    }

    #[kithara::test]
    fn zero_channels_report_no_frames() {
        let silence_pcm = silence_pcm();
        let pools = pools();
        assert_eq!(
            chunk(&pools, audio_spec(0, 44_100), &silence_pcm[..4]).frames(),
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
