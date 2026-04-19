//! Explicit FSM types for audio track state management.
//!
//! Replaces the implicit state scattered across `StreamAudioSource` fields
//! (`pending_format_change`, `pending_decode_started_epoch`, etc.) with a
//! single `TrackState` enum that is the sole source of truth.

use std::time::Duration;

use kithara_decode::{DecodeError, InnerDecoder};
use kithara_stream::{MediaInfo, SourcePhase, SourceSeekAnchor};

use crate::pipeline::fetch::Fetch;

// TrackState — worker-side FSM

/// Explicit state machine for a single audio track in the worker thread.
///
/// Each variant carries exactly the context needed for that phase.
/// Transitions happen inside `step_track()` — one transition per call.
pub(crate) enum TrackState {
    /// Normal decoding — produce PCM chunks.
    Decoding,

    /// Consumer requested a seek; not yet applied.
    SeekRequested(SeekRequest),

    /// Waiting for the underlying source to become ready.
    WaitingForSource {
        context: WaitContext,
        reason: WaitingReason,
    },

    /// Actively applying a seek to the decoder.
    ApplyingSeek(ApplySeekState),

    /// Recreating the decoder (format boundary, codec change, seek recovery).
    RecreatingDecoder(RecreateState),

    /// Decoder recreated / seek applied; waiting for first valid chunk.
    AwaitingResume(ResumeState),

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

/// Stateful seek request tracked across retries and waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekRequest {
    pub attempt: u8,
    pub seek: SeekContext,
}

/// Seek application mode resolved before touching the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplySeekState {
    pub mode: SeekMode,
    pub request: SeekRequest,
}

/// Resume state after a seek has been applied to the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResumeState {
    pub recover_attempts: u8,
    pub seek: SeekContext,
    pub skip: Option<Duration>,
    /// Anchor byte offset from the seek — used for readiness checks and demand
    /// when the decoder's stream position differs from the `StreamIndex` layout.
    pub anchor_offset: Option<u64>,
}

/// What to do once decoder recreation succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecreateNext {
    /// Continue plain decoding from the new decoder.
    Decode,
    /// Re-run seek resolution on the recreated decoder.
    Seek(SeekRequest),
    /// Finish seek application by seeking the recreated decoder.
    ApplySeek(SeekRequest),
}

/// Decoder recreation task tracked by the FSM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecreateState {
    pub attempt: u8,
    pub cause: RecreateCause,
    pub media_info: MediaInfo,
    pub next: RecreateNext,
    pub offset: u64,
}

/// What caused us to enter `WaitingForSource`.
#[derive(Debug)]
pub(crate) enum WaitContext {
    /// Starvation during normal playback.
    Playback,
    /// Seek-initiated wait (source not ready for seek).
    Seek(SeekRequest),
    /// Anchor/direct seek resolved, waiting for source bytes before `decoder.seek()`.
    ApplySeek(ApplySeekState),
    /// Init bytes unavailable for decoder recreation.
    Recreation(RecreateState),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekMode {
    /// Direct decoder seek (no anchor). When `target_byte` is `Some`, the FSM
    /// gates the readiness check on that byte range so `decoder.seek()` only
    /// runs once the source can answer the read the decoder is about to
    /// issue. `None` keeps the historical "check current read head" gate for
    /// callers that can't estimate the target byte.
    Direct { target_byte: Option<u64> },
    /// Anchor-based seek with segment alignment.
    Anchor(SourceSeekAnchor),
}

/// Why the decoder needs to be recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecreateCause {
    /// Codec boundary detected during playback.
    FormatBoundary,
    /// ABR switch changed the codec or variant.
    VariantSwitch,
}

/// Terminal failure reasons.
#[derive(Debug)]
pub(crate) enum TrackFailure {
    /// Decoder produced an error.
    Decode(DecodeError),
    /// Decoder recreation failed.
    RecreateFailed { offset: u64 },
    /// Source was cancelled.
    SourceCancelled,
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

// TrackState methods

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
            Self::SeekRequested(_) => TrackPhaseTag::SeekRequested,
            Self::WaitingForSource { .. } => TrackPhaseTag::WaitingForSource,
            Self::ApplyingSeek(_) => TrackPhaseTag::ApplyingSeek,
            Self::RecreatingDecoder(_) => TrackPhaseTag::RecreatingDecoder,
            Self::AwaitingResume(_) => TrackPhaseTag::AwaitingResume,
            Self::AtEof => TrackPhaseTag::AtEof,
            Self::Failed(_) => TrackPhaseTag::Failed,
        }
    }
}

// ConsumerPhase methods

impl ConsumerPhase {
    /// Returns `true` for terminal states.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::AtEof | Self::Failed)
    }
}

// SourcePhase to WaitingReason mapping

/// Map a `SourcePhase` to an optional `WaitingReason`.
///
/// Returns `Some(reason)` for wait states (`Waiting`, `WaitingDemand`,
/// `WaitingMetadata`). Returns `None` for non-wait states (`Ready`, `Eof`,
/// `Seeking`, `Cancelled`) — callers handle those separately.
pub(crate) fn map_source_phase(phase: SourcePhase) -> Option<WaitingReason> {
    match phase {
        SourcePhase::Waiting => Some(WaitingReason::Waiting),
        SourcePhase::WaitingDemand => Some(WaitingReason::WaitingDemand),
        SourcePhase::WaitingMetadata => Some(WaitingReason::WaitingMetadata),
        _ => None,
    }
}

// Tests

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicBool};

    use kithara_decode::{PcmSpec, mock::infinite_inner_decoder_loose};
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn is_terminal_for_each_state() {
        let non_terminal = [
            TrackState::Decoding,
            TrackState::SeekRequested(SeekRequest {
                attempt: 0,
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(5),
                },
            }),
            TrackState::WaitingForSource {
                context: WaitContext::Playback,
                reason: WaitingReason::Waiting,
            },
            TrackState::ApplyingSeek(ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: SeekRequest {
                    attempt: 0,
                    seek: SeekContext {
                        epoch: 1,
                        target: Duration::from_secs(5),
                    },
                },
            }),
            TrackState::RecreatingDecoder(RecreateState {
                attempt: 0,
                cause: RecreateCause::FormatBoundary,
                media_info: MediaInfo::default(),
                next: RecreateNext::Decode,
                offset: 0,
            }),
            TrackState::AwaitingResume(ResumeState {
                recover_attempts: 0,
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(5),
                },
                anchor_offset: None,
                skip: None,
            }),
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
            TrackState::SeekRequested(SeekRequest {
                attempt: 0,
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::ZERO,
                },
            })
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
            TrackState::ApplyingSeek(ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: SeekRequest {
                    attempt: 0,
                    seek: SeekContext {
                        epoch: 1,
                        target: Duration::ZERO,
                    },
                },
            })
            .phase_tag(),
            TrackPhaseTag::ApplyingSeek
        );
        assert_eq!(
            TrackState::RecreatingDecoder(RecreateState {
                attempt: 1,
                cause: RecreateCause::VariantSwitch,
                media_info: MediaInfo::default(),
                next: RecreateNext::ApplySeek(SeekRequest {
                    attempt: 1,
                    seek: SeekContext {
                        epoch: 1,
                        target: Duration::from_secs(10),
                    },
                }),
                offset: 100,
            })
            .phase_tag(),
            TrackPhaseTag::RecreatingDecoder
        );
        assert_eq!(
            TrackState::AwaitingResume(ResumeState {
                recover_attempts: 0,
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(10),
                },
                anchor_offset: None,
                skip: None,
            })
            .phase_tag(),
            TrackPhaseTag::AwaitingResume
        );
        assert_eq!(TrackState::AtEof.phase_tag(), TrackPhaseTag::AtEof);
        assert_eq!(
            TrackState::Failed(TrackFailure::SourceCancelled).phase_tag(),
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
        let media_info = MediaInfo::default()
            .with_channels(2)
            .with_sample_rate(44100);
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
