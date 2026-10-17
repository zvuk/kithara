#![forbid(unsafe_code)]

use kithara_events::Event;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

/// Seek lifecycle stage used for end-to-end diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekLifecycleStage {
    SeekRequest,
    SeekApplied,
    DecodeStarted,
    OutputCommitted,
}

/// Position of a seek target inside the source's variant/segment grid.
///
/// All fields are `Option`: callers may know only some coordinates (e.g. a
/// pre-decode `SeekRequest` knows the variant but not the resolved byte
/// range yet). Empty `SegmentLocation::default()` means "no information".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentLocation {
    pub byte_range_end: Option<u64>,
    pub byte_range_start: Option<u64>,
    pub segment_index: Option<u32>,
    pub variant: Option<usize>,
}

impl SegmentLocation {
    #[must_use]
    pub const fn new(
        variant: Option<usize>,
        segment_index: Option<u32>,
        byte_range_start: Option<u64>,
        byte_range_end: Option<u64>,
    ) -> Self {
        Self {
            byte_range_end,
            byte_range_start,
            segment_index,
            variant,
        }
    }
}

/// Events from the audio pipeline.
#[derive(Debug, Clone, Event)]
pub enum AudioEvent {
    /// Audio format detected.
    FormatDetected { spec: AudioSpec },
    /// Audio format changed (ABR switch).
    FormatChanged { old: AudioSpec, new: AudioSpec },
    /// PCM output progress committed by playback sink.
    PlaybackProgress {
        position_ms: u64,
        total_ms: Option<u64>,
        buffered_ms: Option<u64>,
    },
    /// Decoded output became available to a non-blocking reader.
    ///
    /// This is a wake hint, not sink progress: committed playback position
    /// still comes only from [`PlaybackProgress`](Self::PlaybackProgress).
    OutputAvailable,
    /// Seek lifecycle diagnostics.
    SeekLifecycle {
        stage: SeekLifecycleStage,
        location: SegmentLocation,
    },
    /// Seek completed at the first committed post-seek output frame.
    SeekComplete { position: Duration },
    /// The owning-thread seek failed; the decoded source is terminal.
    SeekRejected { target: Duration },
    /// Decoder initialized or recreated (ABR switch, format boundary, host rate).
    DecoderReady {
        base_offset: u64,
        variant: Option<u32>,
    },
    /// Terminal track failure surfaced by the audio FSM.
    TrackFailed { failure: TrackFailureKind },
    /// Consumer crossed from playable output into starvation.
    UnderrunStarted { position_ms: u64 },
    /// Consumer recovered from starvation and resumed playback.
    UnderrunEnded { position_ms: u64 },
    /// Low-rate worker-side view of decoded/buffered progress.
    BufferHealth {
        buffered_ms: u64,
        decoded_frontier_ms: u64,
    },
    /// Low-rate worker-side engine cost snapshot.
    EngineLoad {
        load: f32,
        ms_per_chunk: f32,
        realtime_factor: f32,
    },
    /// Host-rate adaptation selected or reconfigured for playback.
    PlaybackResamplerConfigured {
        backend: PlaybackResamplerKind,
        host_sample_rate: u32,
        source_sample_rate: u32,
        active: bool,
    },
    /// Decoding finished for the open source.
    EndOfStream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackResamplerKind {
    Rubato,
    Glide,
    None,
}

/// Terminal classification carried by a decoded-audio source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TrackFailureKind {
    /// A decoder failure with its concrete kind.
    #[error("decode failure: {kind:?}")]
    Decode {
        /// The decoder's classification.
        kind: super::DecodeErrorKind,
    },
    /// Decoder recreation failed at a source byte offset.
    #[error("decoder recreation failed at offset {offset}")]
    RecreateFailed {
        /// The attempted source byte offset.
        offset: u64,
    },
    /// The source cancelled before natural EOF.
    #[error("source cancelled")]
    SourceCancelled,
    /// The producer closed without a terminal marker.
    #[error("PCM channel closed with no failure marker")]
    ChannelClosed,
    /// The render adapter failed without a more specific upstream cause.
    #[error("audio render failed")]
    Render,
}
