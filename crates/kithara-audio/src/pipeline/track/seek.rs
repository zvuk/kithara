use kithara_decode::PcmChunk;
use kithara_stream::{SourcePhase, StreamType};
use tracing::trace;

use super::{
    AtEof, CurrentFsm, Decoding, Failed, RecreatingDecoder, TrackFailure, TrackStep,
    decode::{DecodeStep, decode_step},
    fsm::apply_seek_transition,
    phase::{Track, TrackPhase, sealed},
};
use crate::pipeline::{
    decode::gate::{post_seek_anchor_offset, source_phase_for_wait_context},
    rebuild::RecreateState,
    seek::{
        ApplySeekState, ResumeState, SeekMode, SeekRequest, anchor::stale, engine::SeekApplyCtx,
    },
    source::StreamAudioSource,
};

/// Consumer requested a seek; not yet applied.
pub(crate) struct SeekRequested;

impl sealed::Sealed for SeekRequested {}

impl TrackPhase for SeekRequested {
    type Data = SeekRequest;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::SeekRequested(track)
    }
}

impl Track<SeekRequested> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let request = self.into_inner();
        if !src.readiness.source_is_ready(&src.shared_stream) {
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(
                    Track::<WaitingForSource>::new(WaitState {
                        context: WaitContext::Seek(request),
                        reason,
                    })
                    .erase(),
                );
                return TrackStep::Blocked(reason);
            }
        }
        let transition = src.seek_engine.apply_from_timeline(
            request,
            &SeekApplyCtx {
                decode: &mut src.decode,
                emit: src.emit.as_deref(),
                playhead: src.playhead.as_ref(),
                readiness: &src.readiness,
                seek: src.seek.as_ref(),
                observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        );
        apply_seek_transition(src, transition);
        TrackStep::StateChanged
    }
}

/// Actively applying a seek to the decoder.
pub(crate) struct ApplyingSeek;

impl sealed::Sealed for ApplyingSeek {}

impl TrackPhase for ApplyingSeek {
    type Data = ApplySeekState;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::ApplyingSeek(track)
    }
}

impl Track<ApplyingSeek> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let applying = self.into_inner();
        let anchor_variant = match applying.mode {
            SeekMode::Anchor(anchor) => anchor.variant_index,
            SeekMode::Direct { .. } => None,
        };
        let current_variant = src
            .shared_stream
            .abr_handle()
            .and_then(|handle| handle.current_variant_index());
        if stale(anchor_variant, current_variant) {
            trace!(
                ?applying,
                current_variant = ?src
                    .shared_stream
                    .abr_handle()
                    .and_then(|handle| handle.current_variant_index()),
                "apply seek anchor belongs to inactive variant; re-resolving"
            );
            src.update_state(Track::<SeekRequested>::new(applying.request).erase());
            return TrackStep::StateChanged;
        }
        if !src
            .readiness
            .source_is_ready_for_apply_seek(&src.shared_stream, applying)
        {
            let phase = source_phase_for_wait_context(
                &src.shared_stream,
                &WaitContext::ApplySeek(applying),
            );
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(
                    Track::<WaitingForSource>::new(WaitState {
                        context: WaitContext::ApplySeek(applying),
                        reason,
                    })
                    .erase(),
                );
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                src.update_state(Track::<Failed>::new(TrackFailure::SourceCancelled).erase());
                return TrackStep::Failed;
            }
            src.update_state(Self::new(applying).erase());
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        let transition = src.seek_engine.apply(
            applying,
            SeekApplyCtx {
                decode: &mut src.decode,
                emit: src.emit.as_deref(),
                playhead: src.playhead.as_ref(),
                readiness: &src.readiness,
                seek: src.seek.as_ref(),
                observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        );
        apply_seek_transition(src, transition);
        TrackStep::StateChanged
    }
}

/// Decoder recreated / seek applied; waiting for first valid chunk.
pub(crate) struct AwaitingResume;

impl sealed::Sealed for AwaitingResume {}

impl TrackPhase for AwaitingResume {
    type Data = ResumeState;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::AwaitingResume(track)
    }
}

impl Track<AwaitingResume> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let resume = self.into_inner();
        let post_seek_offset = post_seek_anchor_offset(&src.shared_stream, &resume);
        let ready = post_seek_offset.map_or_else(
            || src.readiness.source_is_ready(&src.shared_stream),
            |byte| {
                src.readiness
                    .source_is_ready_for_chunk(&src.shared_stream, byte)
            },
        );
        if !ready {
            let phase =
                source_phase_for_wait_context(&src.shared_stream, &WaitContext::PostSeek(resume));
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(
                    Track::<WaitingForSource>::new(WaitState {
                        context: WaitContext::PostSeek(resume),
                        reason,
                    })
                    .erase(),
                );
                return TrackStep::Blocked(reason);
            }
        }
        // Restore the phase so the decode loop's `resume_state()` /
        // post-seek skip trimming sees the canonical `ResumeState`.
        src.update_state(Self::new(resume).erase());
        match decode_step(src) {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::NotReady(reason) => TrackStep::Blocked(reason),
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

/// Data carried while the source cannot make progress.
pub(crate) struct WaitState {
    pub(crate) context: WaitContext,
    pub(crate) reason: WaitingReason,
}

/// Operation to resume once the source becomes ready.
#[derive(Debug)]
pub(crate) enum WaitContext {
    Playback,
    Seek(SeekRequest),
    ApplySeek(ApplySeekState),
    Recreation(RecreateState),
    PostSeek(ResumeState),
}

/// Why the source is not ready, mirroring relevant `SourcePhase` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitingReason {
    Waiting,
    WaitingDemand,
    WaitingMetadata,
}

/// Waiting for the underlying source to become ready.
pub(crate) struct WaitingForSource;

impl sealed::Sealed for WaitingForSource {}

impl TrackPhase for WaitingForSource {
    type Data = WaitState;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::WaitingForSource(track)
    }
}

impl Track<WaitingForSource> {
    pub(crate) fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let WaitState {
            context,
            reason: stored_reason,
        } = self.into_inner();
        if let WaitContext::ApplySeek(applying) = context
            && stale(
                match applying.mode {
                    SeekMode::Anchor(anchor) => anchor.variant_index,
                    SeekMode::Direct { .. } => None,
                },
                src.shared_stream
                    .abr_handle()
                    .and_then(|handle| handle.current_variant_index()),
            )
        {
            trace!(
                ?applying,
                current_variant = ?src
                    .shared_stream
                    .abr_handle()
                    .and_then(|handle| handle.current_variant_index()),
                "waiting apply seek anchor belongs to inactive variant; re-resolving"
            );
            src.update_state(Track::<SeekRequested>::new(applying.request).erase());
            return TrackStep::StateChanged;
        }
        let phase = source_phase_for_wait_context(&src.shared_stream, &context);

        if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
            // Still waiting — restore the phase with its stored reason.
            src.update_state(
                Self::new(WaitState {
                    context,
                    reason: stored_reason,
                })
                .erase(),
            );
            return TrackStep::Blocked(reason);
        }

        match phase {
            SourcePhase::Cancelled => {
                src.update_state(Track::<Failed>::new(TrackFailure::SourceCancelled).erase());
                return TrackStep::Failed;
            }
            SourcePhase::Eof => {
                src.update_state(Track::<AtEof>::new(()).erase());
                return TrackStep::Eof;
            }
            _ => {}
        }

        // Source ready — resume into the phase that initiated the wait.
        match context {
            WaitContext::Playback => src.update_state(Track::<Decoding>::new(()).erase()),
            WaitContext::Seek(ctx) => src.update_state(Track::<SeekRequested>::new(ctx).erase()),
            WaitContext::ApplySeek(applying) => {
                src.update_state(Track::<ApplyingSeek>::new(applying).erase());
            }
            WaitContext::Recreation(recreate) => {
                src.update_state(Track::<RecreatingDecoder>::new(recreate).erase());
            }
            WaitContext::PostSeek(resume) => {
                src.update_state(Track::<AwaitingResume>::new(resume).erase());
            }
        }
        TrackStep::StateChanged
    }
}
