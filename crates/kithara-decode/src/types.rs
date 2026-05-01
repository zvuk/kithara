use std::{fmt, sync::Arc, time::Duration};

use derivative::Derivative;
use kithara_bufpool::{PcmBuf, pcm_pool};

/// Audio track metadata extracted from Symphonia tags.
///
/// Intentionally without `#[non_exhaustive]` — this is a stable POD of
/// optional tag fields, constructed via direct struct literal in
/// downstream test/processor code; future additions go through
/// `Default::default()` spread.
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
///
/// Intentionally without `#[non_exhaustive]`: this is a stable POD pair
/// (`channels`, `sample_rate`) at the heart of every audio API in the
/// workspace, constructed via direct struct literal at >100 call sites.
/// Adding fields would force a workspace-wide migration regardless of
/// non-exhaustiveness, so the marker buys nothing.
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

impl From<&PcmMeta> for kithara_stream::ChunkPosition {
    fn from(meta: &PcmMeta) -> Self {
        Self {
            sample_rate: meta.spec.sample_rate,
            frame_offset: meta.frame_offset,
            frames: u64::from(meta.frames),
            source_bytes: meta.source_bytes,
            source_byte_offset: meta.source_byte_offset,
            end_position_ns: u64::try_from(meta.end_timestamp.as_nanos()).unwrap_or(u64::MAX),
        }
    }
}

/// Timeline metadata for a PCM chunk.
///
/// Combines audio format specification with position on the logical timeline.
/// Each chunk gets unique timeline coordinates; `PcmSpec` is the static part.
///
/// Intentionally without `#[non_exhaustive]`: external crates construct
/// it via `PcmMeta { spec, ..Default::default() }` for fixtures; the
/// pattern survives field additions, and `non_exhaustive` would block
/// the struct-literal idiom altogether.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcmMeta {
    /// Wall-clock position **after** this chunk's frames have played
    /// out, computed by the decoder from its own frame counter. Used
    /// by `Timeline::advance_committed_chunk` to update the playhead
    /// without re-doing `frames * 1e9 / sample_rate` arithmetic on the
    /// consumer side. For frame-based decoders (MP3 / AAC) the last
    /// chunk may legitimately push this a few ms past the rounded
    /// `total_duration`; the timeline clamps to duration on write.
    pub end_timestamp: Duration,
    /// Timestamp of the first frame in this chunk.
    pub timestamp: Duration,
    /// Segment index within playlist (`None` for progressive files).
    pub segment_index: Option<u32>,
    /// Absolute byte offset of this chunk's source data within the input
    /// stream, when the decoder reports it. Apple's `AudioFile` exposes
    /// this via `AudioStreamPacketDescription.mStartOffset`; other
    /// backends (Symphonia, Android `MediaExtractor`) do not surface
    /// per-packet byte offsets through their public API and leave this
    /// `None`. When present, downstream code can pin the chunk to an
    /// exact byte range without recomputing rate × time.
    pub source_byte_offset: Option<u64>,
    /// Variant/quality level index (`None` for progressive files).
    pub variant_index: Option<usize>,
    /// Audio format (channels, sample rate).
    pub spec: PcmSpec,
    /// Number of audio frames this chunk represents (one frame =
    /// `spec.channels` interleaved samples). Decoder fills it from the
    /// output buffer length; consumer-side splits update it in place
    /// when slicing a chunk into consumed/remaining halves.
    pub frames: u32,
    /// Decoder generation — increments on each ABR switch / decoder recreation.
    pub epoch: u64,
    /// Absolute frame offset from the start of the track.
    pub frame_offset: u64,
    /// Number of source-stream bytes that produced this chunk's PCM, as
    /// reported by the underlying decoder packet (e.g. `Packet.data.len()`
    /// for Symphonia, `mDataByteSize` for Apple `AudioConverter`,
    /// `readSampleData` return for Android `MediaExtractor`).
    ///
    /// Lets the consumer correlate chunk frames with the source byte
    /// position without recomputing rate × time externally — the decoder
    /// already knows the exact mapping for variable-bitrate compressed
    /// formats and arbitrary-sized PCM packets. `0` means "unknown" (mock
    /// decoders, post-EOF flush chunks).
    pub source_bytes: u64,
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
#[derive(Debug, Derivative)]
#[derivative(Default)]
pub struct PcmChunk {
    #[derivative(Default(value = "pcm_pool().get()"))]
    pub pcm: PcmBuf,
    pub meta: PcmMeta,
}

impl Clone for PcmChunk {
    /// Clone creates a new pool-backed buffer with copied samples.
    ///
    /// Each clone gets its own [`PcmBuf`] from the global pool,
    /// so both original and clone recycle independently on drop.
    fn clone(&self) -> Self {
        let mut new_pcm = pcm_pool().get();
        new_pcm.extend_from_slice(&self.pcm);
        Self {
            pcm: new_pcm,
            meta: self.meta,
        }
    }
}

impl PcmChunk {
    /// Create a new `PcmChunk` from a pool-backed buffer.
    #[must_use]
    pub fn new(meta: PcmMeta, pcm: PcmBuf) -> Self {
        Self { pcm, meta }
    }

    /// Number of audio frames in this chunk.
    ///
    /// A frame contains one sample per channel.
    #[must_use]
    pub fn frames(&self) -> usize {
        let channels = self.meta.spec.channels as usize;
        self.pcm.len().checked_div(channels).unwrap_or(0)
    }

    /// Audio format specification.
    #[must_use]
    pub fn spec(&self) -> PcmSpec {
        self.meta.spec
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

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

    #[kithara::test]
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

    #[kithara::test]
    fn test_pcm_spec_clone() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let cloned = spec;
        assert_eq!(spec, cloned);
    }

    #[kithara::test]
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

    #[kithara::test]
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

    #[kithara::test]
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

    #[kithara::test]
    fn test_pcm_meta_default() {
        let meta = PcmMeta::default();
        assert_eq!(meta.spec, PcmSpec::default());
        assert_eq!(meta.frame_offset, 0);
        assert_eq!(meta.timestamp, Duration::ZERO);
        assert_eq!(meta.segment_index, None);
        assert_eq!(meta.variant_index, None);
        assert_eq!(meta.epoch, 0);
    }

    #[kithara::test]
    fn test_pcm_meta_copy() {
        let meta = PcmMeta {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            frame_offset: 1000,
            timestamp: Duration::from_millis(22),
            end_timestamp: Duration::from_millis(22),
            segment_index: Some(5),
            variant_index: Some(2),
            epoch: 3,
            frames: 0,
            source_bytes: 0,
            source_byte_offset: None,
        };
        let copied = meta;
        assert_eq!(meta, copied);
    }

    #[kithara::test]
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

    #[kithara::test]
    fn test_pcm_meta_partial_eq() {
        let a = PcmMeta {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44100,
            },
            frame_offset: 100,
            timestamp: Duration::from_millis(2),
            end_timestamp: Duration::from_millis(2),
            segment_index: Some(1),
            variant_index: Some(0),
            epoch: 1,
            frames: 0,
            source_bytes: 0,
            source_byte_offset: None,
        };
        let mut b = a;
        assert_eq!(a, b);
        b.frame_offset = 200;
        assert_ne!(a, b);
    }

    // PcmChunk Tests

    #[kithara::test]
    fn test_pcm_chunk_new() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1f32, 0.2, 0.3, 0.4];
        let chunk = test_chunk(spec, pcm.clone());

        assert_eq!(chunk.spec(), spec);
        assert_eq!(&chunk.pcm[..], &pcm[..]);
    }

    #[kithara::test]
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

    #[kithara::test]
    fn test_frames_zero_channels() {
        let spec = PcmSpec {
            channels: 0,
            sample_rate: 44100,
        };
        let chunk = test_chunk(spec, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(chunk.frames(), 0);
    }

    #[kithara::test]
    fn test_samples_access() {
        let spec = PcmSpec {
            channels: 2,
            sample_rate: 44100,
        };
        let pcm = vec![0.1, 0.2, 0.3, 0.4];
        let chunk = test_chunk(spec, pcm.clone());

        let samples: &[f32] = &chunk.pcm;
        assert_eq!(samples.len(), 4);
        assert_eq!(samples, &pcm[..]);
    }

    #[kithara::test]
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

    #[kithara::test]
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
