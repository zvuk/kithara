#![forbid(unsafe_code)]

use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;

use crate::SeekEpoch;

/// Seek lifecycle stage used for end-to-end diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
#[derive(Debug, Clone)]
#[non_exhaustive]
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
        seek_epoch: SeekEpoch,
    },
    /// Decoded output became available to a non-blocking reader.
    ///
    /// This is a wake hint, not sink progress: committed playback position
    /// still comes only from [`PlaybackProgress`](Self::PlaybackProgress).
    OutputAvailable,
    /// Seek lifecycle diagnostics.
    SeekLifecycle {
        stage: SeekLifecycleStage,
        seek_epoch: SeekEpoch,
        location: SegmentLocation,
    },
    /// Seek completed at the first committed post-seek output frame.
    SeekComplete {
        position: Duration,
        seek_epoch: SeekEpoch,
    },
    /// Seek could not be applied and playback continues from the current decoder position.
    SeekRejected { epoch: SeekEpoch, target: Duration },
    /// Decoder initialized or recreated (ABR switch, format boundary, recovery).
    DecoderReady {
        base_offset: u64,
        variant: Option<u32>,
    },
    /// Terminal track failure surfaced by the audio FSM.
    TrackFailed {
        failure: TrackFailureKind,
        seek_epoch: SeekEpoch,
    },
    /// Consumer crossed from playable output into starvation.
    UnderrunStarted {
        position_ms: u64,
        seek_epoch: SeekEpoch,
    },
    /// Consumer recovered from starvation and resumed playback.
    UnderrunEnded {
        position_ms: u64,
        seek_epoch: SeekEpoch,
    },
    /// Low-rate worker-side view of decoded/buffered progress.
    BufferHealth {
        buffered_ms: u64,
        decoded_frontier_ms: u64,
        seek_epoch: SeekEpoch,
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
    /// Decoding finished for one seek epoch.
    EndOfStream { seek_epoch: SeekEpoch },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlaybackResamplerKind {
    Rubato,
    Glide,
    None,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum TrackFailureKind {
    Decode,
    RecreateFailed { offset: u64 },
    SourceCancelled,
}
