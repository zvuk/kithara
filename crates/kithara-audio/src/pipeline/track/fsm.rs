use std::mem;

use kithara_decode::{DecodeError, PcmChunk};
use kithara_events::AudioEvent;
use kithara_stream::{SourcePhase, StreamType};
use tracing::warn;

use super::{
    ApplyingSeek, AtEof, AwaitingResume, Decoding, RebuildingDecoder, RecreatingDecoder,
    SeekRequested, WaitState, WaitingForSource, WaitingReason,
    phase::{Track, TrackPhase, sealed},
    start_recreating_decoder, start_route_change_recreate_if_needed,
};
use crate::pipeline::{
    fetch::Fetch,
    seek::{SeekContext, SeekRequest, emit::preempt_target, engine::SeekTransition},
    source::StreamAudioSource,
};

/// Type-erased FSM phase stored by the track coordinator.
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

/// Result of a single track FSM step.
pub enum TrackStep<C> {
    Produced(Fetch<C>),
    Blocked(WaitingReason),
    StateChanged,
    Eof,
    Failed,
}

/// Terminal failure.
pub(crate) struct Failed;

impl sealed::Sealed for Failed {}

impl TrackPhase for Failed {
    type Data = TrackFailure;

    fn erase(track: Track<Self>) -> CurrentFsm {
        CurrentFsm::Failed(track)
    }
}

/// Terminal failure reasons.
#[derive(Debug)]
pub(crate) enum TrackFailure {
    Decode(DecodeError),
    RecreateFailed { offset: u64 },
    SourceCancelled,
}

impl CurrentFsm {
    /// Returns true only for phases that can never transition.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

pub(crate) fn map_source_phase(phase: SourcePhase) -> Option<WaitingReason> {
    match phase {
        SourcePhase::Waiting => Some(WaitingReason::Waiting),
        SourcePhase::WaitingDemand => Some(WaitingReason::WaitingDemand),
        SourcePhase::WaitingMetadata => Some(WaitingReason::WaitingMetadata),
        _ => None,
    }
}

fn emit_event<T: StreamType>(src: &StreamAudioSource<T>, event: AudioEvent) {
    if let Some(ref emit) = src.emit {
        emit.enqueue(event);
    }
}

pub(super) fn apply_seek_transition<T: StreamType>(
    src: &mut StreamAudioSource<T>,
    transition: SeekTransition,
) {
    match transition {
        SeekTransition::Ack { epoch } => {
            src.readiness
                .finalize_seek_pending(src.seek.as_ref(), epoch);
            src.update_state(Track::<Decoding>::new(()).erase());
        }
        SeekTransition::Apply(applying) => {
            src.update_state(Track::<ApplyingSeek>::new(applying).erase());
        }
        SeekTransition::Applied { epoch, resume } => {
            src.seek_engine.commit_decode_epoch(epoch, "seek_applied");
            src.readiness
                .finalize_seek_pending(src.seek.as_ref(), epoch);
            src.decode.notify_seek();
            src.update_state(Track::<AwaitingResume>::new(resume).erase());
        }
        SeekTransition::AtEof { epoch } => {
            src.readiness
                .finalize_seek_pending(src.seek.as_ref(), epoch);
            src.seek_engine
                .commit_decode_epoch(epoch, "seek_landed_at_eof");
            src.update_state(Track::<AtEof>::new(()).erase());
        }
        SeekTransition::Recreate(recreate) => start_recreating_decoder(src, recreate),
        SeekTransition::Resolve(request) => {
            src.update_state(Track::<SeekRequested>::new(request).erase());
        }
        SeekTransition::Reject {
            request,
            error,
            context,
        } => {
            warn!(?error, epoch = request.seek.epoch, ?request.seek.target, "{context}");
            emit_event(
                src,
                AudioEvent::SeekRejected {
                    epoch: request.seek.epoch,
                    target: request.seek.target,
                },
            );
            src.seek_engine
                .commit_decode_epoch(request.seek.epoch, "seek_rejected");
            src.readiness
                .finalize_seek_pending(src.seek.as_ref(), request.seek.epoch);
            src.update_state(Track::<Decoding>::new(()).erase());
        }
        SeekTransition::Failed {
            request,
            error,
            context,
        } => {
            warn!(?error, epoch = request.seek.epoch, ?request.seek.target, "{context}");
            emit_event(
                src,
                AudioEvent::SeekRejected {
                    epoch: request.seek.epoch,
                    target: request.seek.target,
                },
            );
            src.readiness
                .finalize_seek_pending(src.seek.as_ref(), request.seek.epoch);
            src.update_state(Track::<Failed>::new(TrackFailure::Decode(error)).erase());
        }
        SeekTransition::Wait { context, reason } => {
            src.update_state(Track::<WaitingForSource>::new(WaitState { context, reason }).erase());
        }
    }
}

fn emit_failure_log(failure: &TrackFailure) {
    match failure {
        TrackFailure::Decode(err) => warn!(?err, "track failed: decode error"),
        TrackFailure::RecreateFailed { offset } => {
            warn!(offset = *offset, "track failed: decoder recreation failed");
        }
        TrackFailure::SourceCancelled => warn!("track failed: source cancelled"),
    }
}

pub(crate) fn dispatch<T: StreamType>(src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
    if !matches!(src.state, CurrentFsm::RebuildingDecoder(_))
        && let Some(target) = preempt_target(&src.seek_engine, &src.state, src.seek_obs.as_ref())
    {
        src.update_state(
            Track::<SeekRequested>::new(SeekRequest {
                seek: SeekContext {
                    target,
                    epoch: src.seek_obs.epoch(),
                },
                emit_request: false,
            })
            .erase(),
        );
        src.decode.reset();
        src.decode.notify_seek();
        return TrackStep::StateChanged;
    }
    if !matches!(
        src.state,
        CurrentFsm::RecreatingDecoder(_)
            | CurrentFsm::RebuildingDecoder(_)
            | CurrentFsm::AtEof(_)
            | CurrentFsm::Failed(_)
    ) && start_route_change_recreate_if_needed(src)
    {
        return TrackStep::StateChanged;
    }

    match mem::replace(&mut src.state, Track::<Decoding>::new(()).erase()) {
        CurrentFsm::Decoding(handle) => handle.step(src),
        CurrentFsm::SeekRequested(handle) => handle.step(src),
        CurrentFsm::WaitingForSource(handle) => handle.step(src),
        CurrentFsm::ApplyingSeek(handle) => handle.step(src),
        CurrentFsm::RecreatingDecoder(handle) => handle.step(src),
        CurrentFsm::RebuildingDecoder(handle) => handle.step(src),
        CurrentFsm::AwaitingResume(handle) => handle.step(src),
        CurrentFsm::AtEof(handle) => {
            src.state = CurrentFsm::AtEof(handle);
            TrackStep::Eof
        }
        CurrentFsm::Failed(handle) => {
            emit_failure_log(handle.data());
            src.state = CurrentFsm::Failed(handle);
            TrackStep::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use crossbeam_queue::ArrayQueue;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::MediaInfo;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::{
        consumer::ConsumerPhase,
        rebuild::{RebuildState, RecreateCause, RecreateNext, RecreateState},
        seek::{ApplySeekState, ResumeState, SeekContext, SeekMode, SeekRequest},
        track::{Track, TrackFailure, WaitContext, WaitState},
    };

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
            Track::<Decoding>::new(()).erase(),
            Track::<SeekRequested>::new(seek_at(5)).erase(),
            Track::<WaitingForSource>::new(WaitState {
                context: WaitContext::Playback,
                reason: WaitingReason::Waiting,
            })
            .erase(),
            Track::<ApplyingSeek>::new(ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_at(5),
            })
            .erase(),
            Track::<RecreatingDecoder>::new(recreate_state()).erase(),
            Track::<RebuildingDecoder>::new(rebuild_state()).erase(),
            Track::<AwaitingResume>::new(ResumeState {
                seek: SeekContext {
                    epoch: 1,
                    target: Duration::from_secs(5),
                },
                anchor_offset: None,
                anchor_variant_index: None,
                skip: None,
            })
            .erase(),
            Track::<AtEof>::new(()).erase(),
        ];
        for (idx, fsm) in non_terminal.iter().enumerate() {
            assert!(!fsm.is_terminal(), "expected non-terminal phase #{idx}");
        }

        assert!(
            Track::<Failed>::new(TrackFailure::SourceCancelled)
                .erase()
                .is_terminal()
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
        let fsm = Track::<AtEof>::new(()).erase();
        assert!(!fsm.is_terminal());
        assert!(matches!(fsm, CurrentFsm::AtEof(_)));
    }
}
