use std::{marker::PhantomData, sync::Arc};

use crossbeam_queue::ArrayQueue;
use kithara_decode::{DecodeError, Decoder};
use kithara_platform::time::Duration;
use kithara_stream::{MediaInfo, SourcePhase, SourceSeekAnchor};

use crate::pipeline::fetch::Fetch;

mod sealed {
    pub(crate) trait Sealed {}
}

/// Phase marker for the worker-thread track FSM.
///
/// Each phase owns exactly the data it needs via [`TrackPhase::Data`].
/// Per-phase behaviour lives in `impl Track<Phase>` blocks (in
/// `pipeline/source.rs`, same module as the worker helpers), so an
/// operation only valid in one phase simply does not exist on the other
/// `Track<_>` types — a misuse becomes a compile error instead of a
/// runtime `match self.state` re-check.
pub(crate) trait TrackPhase: sealed::Sealed {
    type Data;
}

/// Normal decoding — produce PCM chunks.
pub(crate) struct Decoding;
/// Consumer requested a seek; not yet applied.
pub(crate) struct SeekRequested;
/// Waiting for the underlying source to become ready.
pub(crate) struct WaitingForSource;
/// Actively applying a seek to the decoder.
pub(crate) struct ApplyingSeek;
/// Recreating the decoder (format boundary, codec change, seek recovery).
pub(crate) struct RecreatingDecoder;
/// Waiting for an off-core decoder rebuild to complete.
pub(crate) struct RebuildingDecoder;
/// Decoder recreated / seek applied; waiting for first valid chunk.
pub(crate) struct AwaitingResume;
/// End of stream reached.
pub(crate) struct AtEof;
/// Terminal failure.
pub(crate) struct Failed;

impl sealed::Sealed for Decoding {}
impl sealed::Sealed for SeekRequested {}
impl sealed::Sealed for WaitingForSource {}
impl sealed::Sealed for ApplyingSeek {}
impl sealed::Sealed for RecreatingDecoder {}
impl sealed::Sealed for RebuildingDecoder {}
impl sealed::Sealed for AwaitingResume {}
impl sealed::Sealed for AtEof {}
impl sealed::Sealed for Failed {}

impl TrackPhase for Decoding {
    type Data = ();
}
impl TrackPhase for SeekRequested {
    type Data = SeekRequest;
}
impl TrackPhase for WaitingForSource {
    type Data = WaitState;
}
impl TrackPhase for ApplyingSeek {
    type Data = ApplySeekState;
}
impl TrackPhase for RecreatingDecoder {
    type Data = RecreateState;
}
impl TrackPhase for RebuildingDecoder {
    type Data = RebuildState;
}
impl TrackPhase for AwaitingResume {
    type Data = ResumeState;
}
impl TrackPhase for AtEof {
    type Data = ();
}
impl TrackPhase for Failed {
    type Data = TrackFailure;
}

/// Phantom-typed handle for a single FSM phase. Owns the phase's data;
/// transitions consume `self` and produce the next phase. Stored,
/// type-erased, in [`CurrentFsm`].
pub(crate) struct Track<S: TrackPhase> {
    data: S::Data,
    _phase: PhantomData<S>,
}

impl<S: TrackPhase> Track<S> {
    pub(crate) fn new(data: S::Data) -> Self {
        Self {
            data,
            _phase: PhantomData,
        }
    }

    pub(crate) fn data(&self) -> &S::Data {
        &self.data
    }

    pub(crate) fn data_mut(&mut self) -> &mut S::Data {
        &mut self.data
    }

    pub(crate) fn into_inner(self) -> S::Data {
        self.data
    }
}

/// Data carried by the [`WaitingForSource`] phase (was the inline
/// `TrackState::WaitingForSource { context, reason }` variant payload).
pub(crate) struct WaitState {
    pub(crate) context: WaitContext,
    pub(crate) reason: WaitingReason,
}

/// Type-erased FSM phase, stored in `StreamAudioSource.state`.
///
/// A field is mono-typed, so a runtime FSM whose phase changes per
/// transition cannot store a `Track<S>` directly — this sum type is the
/// erasure boundary. Phase-specific behaviour stays on the typed
/// `Track<S>` handles reached by matching here.
pub(crate) enum CurrentFsm {
    Decoding(Track<Decoding>),
    SeekRequested(Track<SeekRequested>),
    WaitingForSource(Track<WaitingForSource>),
    ApplyingSeek(Track<ApplyingSeek>),
    RecreatingDecoder(Track<RecreatingDecoder>),
    RebuildingDecoder(Track<RebuildingDecoder>),
    AwaitingResume(Track<AwaitingResume>),
    AtEof(Track<AtEof>),
    Failed(Track<Failed>),
}

impl CurrentFsm {
    pub(crate) fn applying_seek(state: ApplySeekState) -> Self {
        Self::ApplyingSeek(Track::new(state))
    }

    pub(crate) fn at_eof() -> Self {
        Self::AtEof(Track::new(()))
    }

    pub(crate) fn awaiting_resume(state: ResumeState) -> Self {
        Self::AwaitingResume(Track::new(state))
    }

    pub(crate) fn decoding() -> Self {
        Self::Decoding(Track::new(()))
    }

    pub(crate) fn failed(failure: TrackFailure) -> Self {
        Self::Failed(Track::new(failure))
    }

    /// Returns `true` for terminal phases that will never transition.
    ///
    /// `AtEof` is NOT terminal — seek-after-EOF is a valid transition.
    /// Only `Failed` is truly terminal (track will be removed).
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub(crate) fn rebuilding(state: RebuildState) -> Self {
        Self::RebuildingDecoder(Track::new(state))
    }

    pub(crate) fn recreating(state: RecreateState) -> Self {
        Self::RecreatingDecoder(Track::new(state))
    }

    pub(crate) fn seek_requested(request: SeekRequest) -> Self {
        Self::SeekRequested(Track::new(request))
    }

    pub(crate) fn waiting(context: WaitContext, reason: WaitingReason) -> Self {
        Self::WaitingForSource(Track::new(WaitState { context, reason }))
    }
}

/// Context for a pending seek, carried through multiple states.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SeekContext {
    pub(crate) target: Duration,
    pub(crate) epoch: u64,
}

/// Stateful seek request carried across waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekRequest {
    pub(crate) seek: SeekContext,
    pub(crate) emit_request: bool,
}

impl Default for SeekRequest {
    fn default() -> Self {
        Self {
            seek: SeekContext::default(),
            emit_request: true,
        }
    }
}

/// Seek application mode resolved before touching the decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApplySeekState {
    pub(crate) mode: SeekMode,
    pub(crate) request: SeekRequest,
}

/// Resume state after a seek has been applied to the decoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ResumeState {
    /// Anchor byte offset from the seek — used for readiness checks and demand
    /// when the decoder's stream position differs from the `StreamIndex` layout.
    pub(crate) anchor_offset: Option<u64>,
    /// Variant that owns `anchor_offset`. An HLS manual switch can commit after
    /// anchor resolution; then the old offset is not meaningful in the new
    /// variant's byte space and must not drive post-seek demand.
    pub(crate) anchor_variant_index: Option<usize>,
    pub(crate) skip: Option<Duration>,
    pub(crate) seek: SeekContext,
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
    pub(crate) media_info: MediaInfo,
    pub(crate) cause: RecreateCause,
    pub(crate) next: RecreateNext,
    pub(crate) offset: u64,
}

pub(crate) struct RebuildState {
    pub(crate) completion: Arc<ArrayQueue<DecoderRebuildComplete>>,
    pub(crate) superseded_seek: Option<SeekRequest>,
    pub(crate) recreate: RecreateState,
    pub(crate) started_seek_epoch: u64,
    pub(crate) ticket: u64,
}

pub(crate) struct DecoderRebuildComplete {
    pub(crate) result: Result<Box<dyn Decoder>, RecreateOutcome>,
    pub(crate) ticket: u64,
}

/// Outcome of one `execute_recreation` call.
///
/// `NeedsSourceWait` exists for the post-VariantChange WAV-ABR / fMP4
/// case where the decoder factory's probe reads `[0..PROBE)` of the
/// freshly-switched variant *before* the HLS scheduler has buffered
/// those bytes — the probe surfaces an `ErrorClass::Interrupted`
/// (`StreamPending(WaitBudgetExhausted)`). Treating that as a hard
/// `RecreateFailed` deadlocks the audio worker (Cluster C/D/E/F in
/// `pure-dancing-porcupine.md`, Wave 2.A); routing back through
/// `wait_for_source_on_recreate` lets the source catch up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecreateOutcome {
    Done,
    SoftFailed,
    NeedsSourceWait,
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
    /// `decoder.seek()` already succeeded and the FSM was in
    /// `AwaitingResume` when the source stopped producing chunks.
    /// Carries the `ResumeState` so the wait loop can demand the
    /// anchor byte (instead of the stale pre-seek read head) and
    /// then transition back to `AwaitingResume` once data arrives.
    PostSeek(ResumeState),
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
    /// Host audio route changed the device sample rate.
    RouteChange,
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
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) base_offset: u64,
    /// Seek epoch at which this session was installed. Used by
    /// `detect_format_change` to suppress a redundant recreate when
    /// the in-flight seek epoch still matches the epoch that produced
    /// the session — the decoder is already aligned with the seek's
    /// landing variant.
    pub(crate) installed_at_seek_epoch: u64,
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
    /// Terminal failure — details available via `CurrentFsm::Failed`.
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

impl ConsumerPhase {
    /// Returns `true` for terminal states.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::AtEof | Self::Failed)
    }
}

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossbeam_queue::ArrayQueue;
    use kithara_test_utils::kithara;

    use super::*;

    fn seek_at(secs: u64) -> SeekRequest {
        SeekRequest {
            seek: SeekContext {
                epoch: 1,
                target: Duration::from_secs(secs),
            },
            ..Default::default()
        }
    }

    fn recreate_state() -> RecreateState {
        RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        }
    }

    fn rebuild_state() -> RebuildState {
        RebuildState {
            ticket: 1,
            recreate: recreate_state(),
            started_seek_epoch: 0,
            completion: Arc::new(ArrayQueue::new(1)),
            superseded_seek: None,
        }
    }

    #[kithara::test]
    fn is_terminal_for_each_phase() {
        let non_terminal = [
            CurrentFsm::decoding(),
            CurrentFsm::seek_requested(seek_at(5)),
            CurrentFsm::waiting(WaitContext::Playback, WaitingReason::Waiting),
            CurrentFsm::applying_seek(ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_at(5),
            }),
            CurrentFsm::recreating(recreate_state()),
            CurrentFsm::rebuilding(rebuild_state()),
            CurrentFsm::awaiting_resume(ResumeState {
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(5),
                },
                anchor_offset: None,
                anchor_variant_index: None,
                skip: None,
            }),
            CurrentFsm::at_eof(),
        ];
        for (idx, fsm) in non_terminal.iter().enumerate() {
            assert!(!fsm.is_terminal(), "expected non-terminal phase #{idx}");
        }

        assert!(CurrentFsm::failed(TrackFailure::SourceCancelled).is_terminal());
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
        let fsm = CurrentFsm::at_eof();
        assert!(!fsm.is_terminal());
        assert!(matches!(fsm, CurrentFsm::AtEof(_)));
    }
}
