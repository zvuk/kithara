//! Explicit FSM types for audio track state management.
//!
//! Replaces the implicit state scattered across `StreamAudioSource` fields
//! (`pending_format_change`, `pending_decode_started_epoch`, etc.) with a
//! single `TrackState` enum that is the sole source of truth.

use std::time::Duration;

use kithara_decode::{DecodeError, InnerDecoder};
use kithara_stream::{Fetch, MediaInfo, SourcePhase, SourceSeekAnchor};

// ---------------------------------------------------------------------------
// TrackState — worker-side FSM
// ---------------------------------------------------------------------------

/// Explicit state machine for a single audio track in the worker thread.
///
/// Each variant carries exactly the context needed for that phase.
/// Transitions happen inside `step_track()` — one transition per call.
#[expect(dead_code, reason = "variants used progressively across Phase 2-4")]
pub(crate) enum TrackState {
    /// Normal decoding — produce PCM chunks.
    Decoding,

    /// Consumer requested a seek; not yet applied.
    SeekRequested { epoch: u64, target: Duration },

    /// Waiting for the underlying source to become ready.
    WaitingForSource {
        context: WaitContext,
        reason: WaitingReason,
    },

    /// Actively applying a seek to the decoder.
    ApplyingSeek {
        epoch: u64,
        target: Duration,
        mode: SeekMode,
        attempt: u8,
    },

    /// Recreating the decoder (format boundary, codec change, seek recovery).
    RecreatingDecoder {
        cause: RecreateCause,
        seek: Option<SeekContext>,
        offset: u64,
        attempt: u8,
    },

    /// Decoder recreated / seek applied; waiting for first valid chunk.
    AwaitingResume {
        seek: Option<SeekContext>,
        skip: Option<Duration>,
    },

    /// End of stream reached.
    AtEof,

    /// Terminal failure.
    Failed(TrackFailure),
}

/// Context for a pending seek, carried through multiple states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekContext {
    pub epoch: u64,
    pub target: Duration,
}

/// What caused us to enter `WaitingForSource`.
#[derive(Debug)]
#[expect(dead_code, reason = "variants used in Phase 2-4 step methods")]
pub(crate) enum WaitContext {
    /// Starvation during normal playback.
    Playback,
    /// Seek-initiated wait (source not ready for seek).
    Seek(SeekContext),
    /// Init bytes unavailable for decoder recreation.
    Recreation {
        cause: RecreateCause,
        seek: Option<SeekContext>,
        offset: u64,
    },
}

/// Why the source is not ready, mirroring relevant `SourcePhase` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReason {
    /// Generic wait — data not yet available.
    Waiting,
    /// On-demand request already in flight.
    WaitingDemand,
    /// Metadata lookup in progress.
    WaitingMetadata,
}

/// How the seek should be applied.
#[derive(Debug)]
#[expect(dead_code, reason = "used in Phase 2-4 step_applying_seek")]
pub(crate) enum SeekMode {
    /// Direct decoder seek (no anchor).
    Direct,
    /// Anchor-based seek with segment alignment.
    Anchor(SourceSeekAnchor),
}

/// Why the decoder needs to be recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(dead_code, reason = "used in Phase 2-4 step_recreating_decoder")]
pub(crate) enum RecreateCause {
    /// Codec boundary detected during playback.
    FormatBoundary,
    /// Seek failed, recovery via decoder recreation.
    SeekRecovery,
    /// ABR switch changed the codec.
    CodecChange,
}

/// Terminal failure reasons.
#[derive(Debug)]
#[expect(dead_code, reason = "variants used in Phase 2-4 failure paths")]
pub(crate) enum TrackFailure {
    /// Decoder produced an error.
    Decode(DecodeError),
    /// Seek exhausted all retry attempts.
    SeekExhausted {
        epoch: u64,
        target: Duration,
        attempts: u8,
    },
    /// Decoder recreation failed.
    RecreateFailed { offset: u64 },
    /// Source was cancelled.
    SourceCancelled,
    /// Source was stopped.
    SourceStopped,
}

/// Holds the decoder and its associated metadata as an atomic unit.
///
/// Created whole — never partially mutated. On recreation failure
/// the old session remains untouched.
pub(crate) struct DecoderSession {
    pub base_offset: u64,
    pub decoder: Box<dyn InnerDecoder>,
    pub media_info: Option<MediaInfo>,
}

/// Result of a single `step_track()` call.
pub enum TrackStep<C> {
    /// Produced a chunk ready for the consumer.
    Produced(Fetch<C>),
    /// Source is not ready — cannot make progress.
    Blocked(WaitingReason),
    /// Internal state changed — caller should call `step_track()` again.
    StateChanged,
    /// End of stream.
    Eof,
    /// Terminal failure — details available via `TrackState::Failed`.
    Failed,
}

/// Fieldless discriminant of [`TrackState`] for external phase queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackPhaseTag {
    Decoding,
    SeekRequested,
    WaitingForSource,
    ApplyingSeek,
    RecreatingDecoder,
    AwaitingResume,
    AtEof,
    Failed,
}

/// Consumer-side phase for `Audio<S>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(dead_code, reason = "used in Phase 3 to replace eof:bool in Audio")]
pub(crate) enum ConsumerPhase {
    /// Initial state — waiting for first chunk.
    Buffering,
    /// Normal playback.
    Playing,
    /// Seek in progress — waiting for chunks with matching epoch.
    SeekPending { epoch: u64 },
    /// End of stream reached.
    AtEof,
    /// Unrecoverable failure.
    Failed,
}

// ---------------------------------------------------------------------------
// TrackState methods
// ---------------------------------------------------------------------------

impl TrackState {
    /// Returns `true` for terminal states that will never transition.
    ///
    /// `AtEof` is NOT terminal — seek-after-EOF is a valid transition.
    /// Only `Failed` is truly terminal (track will be removed).
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    /// Fieldless discriminant for external phase queries.
    pub(crate) fn phase_tag(&self) -> TrackPhaseTag {
        match self {
            Self::Decoding => TrackPhaseTag::Decoding,
            Self::SeekRequested { .. } => TrackPhaseTag::SeekRequested,
            Self::WaitingForSource { .. } => TrackPhaseTag::WaitingForSource,
            Self::ApplyingSeek { .. } => TrackPhaseTag::ApplyingSeek,
            Self::RecreatingDecoder { .. } => TrackPhaseTag::RecreatingDecoder,
            Self::AwaitingResume { .. } => TrackPhaseTag::AwaitingResume,
            Self::AtEof => TrackPhaseTag::AtEof,
            Self::Failed(_) => TrackPhaseTag::Failed,
        }
    }
}

// ---------------------------------------------------------------------------
// ConsumerPhase methods
// ---------------------------------------------------------------------------

#[expect(dead_code, reason = "used in Phase 3")]
impl ConsumerPhase {
    /// Returns `true` for terminal states.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::AtEof | Self::Failed)
    }
}

// ---------------------------------------------------------------------------
// SourcePhase → WaitingReason mapping
// ---------------------------------------------------------------------------

/// Map a `SourcePhase` to an optional `WaitingReason`.
///
/// Returns `Some(reason)` for wait states (`Waiting`, `WaitingDemand`,
/// `WaitingMetadata`). Returns `None` for non-wait states (`Ready`, `Eof`,
/// `Seeking`, `Cancelled`, `Stopped`) — callers handle those separately.
pub(crate) fn map_source_phase(phase: SourcePhase) -> Option<WaitingReason> {
    match phase {
        SourcePhase::Waiting => Some(WaitingReason::Waiting),
        SourcePhase::WaitingDemand => Some(WaitingReason::WaitingDemand),
        SourcePhase::WaitingMetadata => Some(WaitingReason::WaitingMetadata),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn is_terminal_for_each_state() {
        let non_terminal = [
            TrackState::Decoding,
            TrackState::SeekRequested {
                epoch: 1,
                target: Duration::from_secs(5),
            },
            TrackState::WaitingForSource {
                context: WaitContext::Playback,
                reason: WaitingReason::Waiting,
            },
            TrackState::ApplyingSeek {
                epoch: 1,
                target: Duration::from_secs(5),
                mode: SeekMode::Direct,
                attempt: 0,
            },
            TrackState::RecreatingDecoder {
                cause: RecreateCause::FormatBoundary,
                seek: None,
                offset: 0,
                attempt: 0,
            },
            TrackState::AwaitingResume {
                seek: None,
                skip: None,
            },
            // AtEof is NOT terminal — seek-after-EOF is valid
            TrackState::AtEof,
        ];
        for state in &non_terminal {
            assert!(
                !state.is_terminal(),
                "expected non-terminal for {:?}",
                state.phase_tag()
            );
        }

        // Only Failed is truly terminal
        assert!(TrackState::Failed(TrackFailure::SourceCancelled).is_terminal());
    }

    #[kithara::test]
    fn phase_tag_preserves_discriminant() {
        assert_eq!(TrackState::Decoding.phase_tag(), TrackPhaseTag::Decoding);
        assert_eq!(
            TrackState::SeekRequested {
                epoch: 1,
                target: Duration::ZERO
            }
            .phase_tag(),
            TrackPhaseTag::SeekRequested
        );
        assert_eq!(
            TrackState::WaitingForSource {
                context: WaitContext::Playback,
                reason: WaitingReason::WaitingDemand,
            }
            .phase_tag(),
            TrackPhaseTag::WaitingForSource
        );
        assert_eq!(
            TrackState::ApplyingSeek {
                epoch: 1,
                target: Duration::ZERO,
                mode: SeekMode::Direct,
                attempt: 0,
            }
            .phase_tag(),
            TrackPhaseTag::ApplyingSeek
        );
        assert_eq!(
            TrackState::RecreatingDecoder {
                cause: RecreateCause::SeekRecovery,
                seek: None,
                offset: 100,
                attempt: 1,
            }
            .phase_tag(),
            TrackPhaseTag::RecreatingDecoder
        );
        assert_eq!(
            TrackState::AwaitingResume {
                seek: Some(SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(10)
                }),
                skip: None,
            }
            .phase_tag(),
            TrackPhaseTag::AwaitingResume
        );
        assert_eq!(TrackState::AtEof.phase_tag(), TrackPhaseTag::AtEof);
        assert_eq!(
            TrackState::Failed(TrackFailure::SourceStopped).phase_tag(),
            TrackPhaseTag::Failed
        );
    }

    #[kithara::test]
    fn map_source_phase_table() {
        assert_eq!(
            map_source_phase(SourcePhase::Waiting),
            Some(WaitingReason::Waiting)
        );
        assert_eq!(
            map_source_phase(SourcePhase::WaitingDemand),
            Some(WaitingReason::WaitingDemand)
        );
        assert_eq!(
            map_source_phase(SourcePhase::WaitingMetadata),
            Some(WaitingReason::WaitingMetadata)
        );

        assert_eq!(map_source_phase(SourcePhase::Ready), None);
        assert_eq!(map_source_phase(SourcePhase::Eof), None);
        assert_eq!(map_source_phase(SourcePhase::Seeking), None);
        assert_eq!(map_source_phase(SourcePhase::Cancelled), None);
        assert_eq!(map_source_phase(SourcePhase::Stopped), None);
    }

    #[kithara::test]
    fn consumer_phase_terminal() {
        assert!(!ConsumerPhase::Buffering.is_terminal());
        assert!(!ConsumerPhase::Playing.is_terminal());
        assert!(!ConsumerPhase::SeekPending { epoch: 1 }.is_terminal());
        assert!(ConsumerPhase::AtEof.is_terminal());
        assert!(ConsumerPhase::Failed.is_terminal());
    }

    #[kithara::test]
    fn seek_context_copy_and_eq() {
        let ctx = SeekContext {
            epoch: 42,
            target: Duration::from_millis(500),
        };
        let copy = ctx;
        assert_eq!(ctx, copy);
        assert_eq!(copy.epoch, 42);
        assert_eq!(copy.target, Duration::from_millis(500));
    }

    #[kithara::test]
    fn at_eof_allows_seek_transition() {
        // AtEof is not terminal — seek-after-EOF must work
        let state = TrackState::AtEof;
        assert!(!state.is_terminal());
        assert_eq!(state.phase_tag(), TrackPhaseTag::AtEof);
    }

    #[kithara::test]
    fn decoder_session_construction() {
        use std::sync::{Arc, atomic::AtomicBool};

        use kithara_decode::{PcmSpec, mock::infinite_inner_decoder_loose};

        let media_info = MediaInfo {
            channels: Some(2),
            codec: None,
            container: None,
            sample_rate: Some(44100),
            variant_index: None,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (decoder, _logs) = infinite_inner_decoder_loose(PcmSpec::default(), stop);
        let session = DecoderSession {
            base_offset: 1024,
            decoder,
            media_info: Some(media_info),
        };
        assert_eq!(session.base_offset, 1024);
        assert!(session.media_info.is_some());
    }
}
