use std::mem;

use arc_swap::ArcSwap;
use kithara_decode::PcmChunk;
use kithara_events::{AudioEvent, DeferredBus};
use kithara_platform::sync::Arc;
use kithara_stream::{Activity, PlayheadWrite, SeekControl, SeekObserve, SourcePhase, StreamType};
use kithara_test_utils::kithara;
use tracing::{debug, trace, warn};

pub(crate) use crate::pipeline::{
    decode::core::{DecodeCore, DecodeInit, DecoderFactory},
    stream::{offset::OffsetReader, shared::SharedStream},
};
use crate::{
    pipeline::{
        decode::{
            core::{DecodeAction as CoreDecodeAction, DecodeCtx},
            format::{FormatDecision, detect},
            gate::{
                ReadinessGate, post_seek_anchor_offset, recreate_phase,
                source_phase_for_wait_context,
            },
            resume::{ResumeCursor, RouteCtx, seek_position},
            step,
        },
        fetch::Fetch,
        parts::SourceParts,
        rebuild::{
            policy::{classify, observed_seek, record_seek_preempt, superseded},
            port::RebuildPort,
            retire::RetiredDecoders,
        },
        seek::{
            SeekEngine,
            anchor::stale,
            emit::{active_epoch, preempt_target},
            engine::{SeekApplyCtx, SeekTransition},
        },
        track_fsm::{
            ApplyingSeek, AwaitingResume, CurrentFsm, DecoderRebuildComplete, Decoding,
            RebuildState, RebuildingDecoder, RecreateCause, RecreateNext, RecreateOutcome,
            RecreateState, RecreatingDecoder, ResumeState, SeekContext, SeekMode, SeekRequest,
            SeekRequested, Track, TrackFailure, TrackStep, WaitContext, WaitState,
            WaitingForSource, WaitingReason,
        },
    },
    renderer::AudioWorkerSource,
};

/// Audio source for Stream with format change detection.
///
/// Monitors `media_info` changes and recreates decoder at segment boundaries.
/// The old decoder naturally decodes all data from the current segment.
/// When it encounters new segment data (different format), it errors or returns EOF.
/// At that point, we seek to the segment boundary and recreate the decoder.
pub(crate) struct StreamAudioSource<T: StreamType> {
    /// Explicit FSM state — single source of truth for track phase.
    pub(crate) state: CurrentFsm,
    pub(crate) decode: DecodeCore,
    /// Narrow activity handle — set/query the `PLAYING` flag.
    activity: Arc<dyn Activity>,
    seek_engine: SeekEngine,
    /// Narrow mutating playhead handle — committed position and total duration.
    playhead: Arc<dyn PlayheadWrite>,
    /// Narrow seek-control handle — begin / complete / clear-pending.
    seek: Arc<dyn SeekControl>,
    /// Narrow seek-observe handle — read seek state without mutation.
    seek_obs: Arc<dyn SeekObserve>,
    rebuild: RebuildPort<T>,
    /// Absolute content frame offset just past the most recently emitted chunk
    /// (the producer's decode head), tagged with its epoch. A mid-playback
    /// variant-switch recreate continues the new decoder from here — NOT from
    /// the consumer's lagging `committed_position`: the chunks in
    /// `[committed..decode_head]` are already queued in the outlet ring (a
    /// `FormatBoundary` recreate neither flushes it nor bumps the seek epoch),
    /// so resuming at `committed` would re-emit them and rewind content. Stored
    /// as an exact frame plus the sample rate of that produced chunk, then
    /// converted back with `duration_for_frames`; the demuxer quantizes the
    /// seek landing to a sample and `frame_offset_for` rounds to the nearest
    /// frame, so the rebuilt decoder relabels its first chunk at this point. See
    /// `execute_recreation`.
    resume: ResumeCursor,
    /// Deferred sink for FSM lifecycle events ([`AudioEvent`]). The FSM runs on
    /// the produce core, so `emit_event` enqueues lock-free; the scheduler shell
    /// flushes via [`flush_deferred`](AudioWorkerSource::flush_deferred) and on
    /// `Drop`, keeping the cross-thread `broadcast::send` (a `kevent`) off the
    /// forbid path. `None` for sources built without an event bus.
    emit: Option<DeferredBus<AudioEvent>>,
    readiness: ReadinessGate,
    /// `(seek_epoch, target)` of the most recent applied seek.
    /// `committed_position` lags `target` until the seek's first
    /// (trim-aligned) chunk is consumed: the decoder lands at the
    /// containing segment's start and trims forward, so
    /// `commit_seek_landed` records the segment boundary, not the
    /// requested instant. A variant-switch recreate firing inside that
    /// window must resume at the real target, not at the lagging
    /// committed boundary — otherwise playback rewinds to the segment
    /// start. Tagged with the seek epoch so a later seek (especially a
    /// backward one) never resumes against a stale forward target. See
    /// `execute_recreation`.
    /// Decoders displaced on the produce core. They are dropped from
    /// `flush_deferred`, outside the forbid-blocking region.
    retired: RetiredDecoders,
    shared_stream: SharedStream<T>,
}

// Construction, lifecycle, and state access
impl<T: StreamType> StreamAudioSource<T> {
    /// Bounded off-RT retire queue for decoders displaced on the produce core.
    const DECODER_RETIRE_CAPACITY: usize = 4;

    pub(crate) fn new(shared_stream: SharedStream<T>, parts: SourceParts<T>) -> Self {
        let SourceParts {
            activity,
            decode,
            playhead,
            readiness,
            rebuild,
            resume,
            seek,
            seek_engine,
            seek_obs,
        } = parts;
        activity.set_playing(true);
        Self {
            shared_stream,
            decode,
            rebuild,
            seek_engine,
            playhead,
            seek,
            seek_obs,
            activity,
            readiness,
            resume,
            state: CurrentFsm::decoding(),
            emit: None,
            retired: RetiredDecoders::new(Self::DECODER_RETIRE_CAPACITY),
        }
    }

    pub(crate) fn with_emit(mut self, emit: DeferredBus<AudioEvent>) -> Self {
        self.emit = Some(emit);
        self
    }
    /// Publish the current FSM phase to the shared activity flag and assign
    /// the new state.
    ///
    /// `PLAYING` mirrors "audio FSM has an active decode target": every
    /// non-terminal state keeps it set (`Decoding`,
    /// `SeekRequested`, `ApplyingSeek`, `AwaitingResume`,
    /// `WaitingForSource`, `RecreatingDecoder`), while terminal states
    /// (`AtEof`, `Failed`) clear it. The Downloader's peer
    /// `priority()` reads this flag to decide between High and Low
    /// priority slots — keeping PLAYING set through buffering and
    /// mid-seek windows is deliberate, because the listener is still
    /// attached to this track.
    fn update_state(&mut self, new: CurrentFsm) {
        self.activity.set_playing(playing_for_state(&new));
        self.state = new;
    }

    /// Reset the effect chain and discard any armed EOF drain.
    /// Used on seek / decoder recreation, so a buffering effect's stale tail never leaks past
    /// the discontinuity and a seek-after-EOF re-arms the drain from scratch.
    /// Emit an audio event if the bus is set. Runs on the produce core, so it
    /// only *enqueues* (lock-free); the shell publishes via `flush_deferred` /
    /// `Drop` — the `broadcast::send` is a `kevent` the forbid path must not make.
    fn emit_event(&self, event: AudioEvent) {
        if let Some(ref emit) = self.emit {
            emit.enqueue(event);
        }
    }

    fn apply_seek_transition(&mut self, transition: SeekTransition) {
        match transition {
            SeekTransition::Ack { epoch } => {
                self.readiness
                    .finalize_seek_pending(self.seek.as_ref(), epoch);
                self.update_state(CurrentFsm::decoding());
            }
            SeekTransition::Apply(applying) => {
                self.update_state(CurrentFsm::applying_seek(applying));
            }
            SeekTransition::Applied { epoch, resume } => {
                self.seek_engine.commit_decode_epoch(epoch, "seek_applied");
                self.readiness
                    .finalize_seek_pending(self.seek.as_ref(), epoch);
                self.decode.notify_seek();
                self.update_state(CurrentFsm::awaiting_resume(resume));
            }
            SeekTransition::AtEof { epoch } => {
                self.readiness
                    .finalize_seek_pending(self.seek.as_ref(), epoch);
                self.seek_engine
                    .commit_decode_epoch(epoch, "seek_landed_at_eof");
                self.update_state(CurrentFsm::at_eof());
            }
            SeekTransition::Recreate(recreate) => self.start_recreating_decoder(recreate),
            SeekTransition::Resolve(request) => {
                self.update_state(CurrentFsm::seek_requested(request));
            }
            SeekTransition::Reject {
                request,
                error,
                context,
            } => {
                warn!(?error, epoch = request.seek.epoch, ?request.seek.target, "{context}");
                self.emit_event(AudioEvent::SeekRejected {
                    epoch: request.seek.epoch,
                    target: request.seek.target,
                });
                self.seek_engine
                    .commit_decode_epoch(request.seek.epoch, "seek_rejected");
                self.readiness
                    .finalize_seek_pending(self.seek.as_ref(), request.seek.epoch);
                self.update_state(CurrentFsm::decoding());
            }
            SeekTransition::Failed {
                request,
                error,
                context,
            } => {
                warn!(?error, epoch = request.seek.epoch, ?request.seek.target, "{context}");
                self.emit_event(AudioEvent::SeekRejected {
                    epoch: request.seek.epoch,
                    target: request.seek.target,
                });
                self.readiness
                    .finalize_seek_pending(self.seek.as_ref(), request.seek.epoch);
                self.update_state(CurrentFsm::failed(TrackFailure::Decode(error)));
            }
            SeekTransition::Wait { context, reason } => {
                self.update_state(CurrentFsm::waiting(context, reason));
            }
        }
    }
}

// Decoder: rebuild and recreate lifecycle
impl<T: StreamType> StreamAudioSource<T> {
    #[kithara::probe]
    fn start_recreating_decoder(&mut self, state: RecreateState) {
        let pending_seek_target = match &state.next {
            RecreateNext::Seek(req) | RecreateNext::ApplySeek(req) => Some(req.seek.target),
            RecreateNext::Decode => None,
        };
        debug!(
            cause = ?state.cause,
            codec = ?state.media_info.codec,
            container = ?state.media_info.container,
            target_offset = state.offset,
            next = ?mem::discriminant(&state.next),
            ?pending_seek_target,
            committed_position = ?self.playhead.position(),
            stream_pos = self.shared_stream.position(),
            "start_recreating_decoder"
        );
        self.update_state(CurrentFsm::recreating(state));
    }

    fn start_route_change_recreate_if_needed(&mut self) -> bool {
        let Some(recreate) = self.resume.route_change(&RouteCtx {
            committed: self.playhead.position(),
            seek: &self.seek_engine,
            seek_active: active_epoch(&self.state).is_some(),
            session: self.decode.session(),
            stream: &self.shared_stream,
        }) else {
            return false;
        };
        self.start_recreating_decoder(recreate);
        true
    }
}

enum DecodeStep {
    Produced(Fetch<PcmChunk>),
    Interrupted,
    NotReady(WaitingReason),
    Eof,
    Failed,
}

fn decode_step<T: StreamType>(src: &mut StreamAudioSource<T>) -> DecodeStep {
    let resuming = matches!(src.state, CurrentFsm::AwaitingResume(_));
    let action = {
        let resume = match &mut src.state {
            CurrentFsm::AwaitingResume(handle) => Some(handle.data_mut()),
            _ => None,
        };
        step::tick(
            &mut src.decode,
            DecodeCtx {
                emit: src.emit.as_ref(),
                playhead: src.playhead.as_ref(),
                resume,
                cursor: &mut src.resume,
                seek: &src.seek_engine,
                seek_observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        )
    };
    match action {
        CoreDecodeAction::Produced(fetch) => {
            if resuming {
                src.update_state(CurrentFsm::decoding());
            }
            DecodeStep::Produced(fetch)
        }
        CoreDecodeAction::Pending(reason) => DecodeStep::NotReady(reason),
        CoreDecodeAction::StartRecreate(recreate) => {
            src.start_recreating_decoder(recreate);
            DecodeStep::Interrupted
        }
        CoreDecodeAction::SeekInterrupted => DecodeStep::Interrupted,
        CoreDecodeAction::Eof => {
            src.update_state(CurrentFsm::at_eof());
            DecodeStep::Eof
        }
        CoreDecodeAction::Failed(failure) => {
            src.update_state(CurrentFsm::failed(failure));
            DecodeStep::Failed
        }
    }
}
impl<T: StreamType> StreamAudioSource<T> {
    fn finish_route_change_after_recreate(
        &mut self,
        recreate: &RecreateState,
        request: SeekRequest,
    ) -> TrackStep<PcmChunk> {
        let target = request.seek.target;
        match self
            .decode
            .seek(&self.shared_stream, self.playhead.as_ref(), target)
        {
            Ok(outcome) => {
                self.decode.reset();
                self.seek_engine
                    .record_resume_target(request.seek.epoch, target);
                let landed_at = seek_position(outcome);
                let remaining = target.saturating_sub(landed_at);
                let skip = (!remaining.is_zero()).then_some(remaining);
                self.update_state(CurrentFsm::awaiting_resume(ResumeState {
                    anchor_offset: Some(recreate.offset),
                    anchor_variant_index: recreate
                        .media_info
                        .variant_index
                        .and_then(|variant| usize::try_from(variant).ok()),
                    skip,
                    seek: request.seek,
                }));
                TrackStep::StateChanged
            }
            Err(err) => {
                warn!(
                    ?err,
                    ?target,
                    "route-change recreate: recreated decoder seek failed"
                );
                self.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                    offset: self.decode.session().base_offset,
                }));
                TrackStep::StateChanged
            }
        }
    }

    /// Apply the `RecreateNext` action after a successful recreation.
    fn apply_recreate_next(&mut self, recreate: &RecreateState) -> TrackStep<PcmChunk> {
        match &recreate.next {
            RecreateNext::Decode => {
                self.decode.reset();
                self.update_state(CurrentFsm::decoding());
                TrackStep::StateChanged
            }
            RecreateNext::Seek(request) => {
                self.update_state(CurrentFsm::seek_requested(*request));
                TrackStep::StateChanged
            }
            RecreateNext::ApplySeek(request) if recreate.cause == RecreateCause::RouteChange => {
                self.finish_route_change_after_recreate(recreate, *request)
            }
            RecreateNext::ApplySeek(request) => self.finish_apply_seek_after_recreate(*request),
        }
    }

    fn finish_format_boundary_rebuild(&mut self) -> RecreateOutcome {
        // Continue the new decoder from the producer's decode head, not the
        // consumer's lagging `committed`: chunks in [committed..decode_head]
        // are already queued in the outlet ring (a FormatBoundary recreate
        // neither flushes it nor bumps the seek epoch), so resuming at
        // `committed` re-emits them — duplicated content, a backward phase
        // jump. The decode head is an exact frame; the demuxer quantizes the
        // seek landing to a sample, and `frame_offset_for` rounds that back
        // to the nearest frame (consistent with `frames_to_trim`), so the
        // rebuilt decoder relabels its first chunk at exactly `decode_head`.
        let committed = self.playhead.position();
        let epoch_now = self.seek_engine.epoch();
        // `resume_target` wins only while the target has NOT yet
        // materialized in produced chunks (`target > decode_head`);
        // comparing against the consumer's lagging `committed` mislabels
        // the warmed-up case and re-emits `[target..decode_head)`.
        let target_time =
            self.resume
                .resume_position(epoch_now, committed, self.seek_engine.resume_target());
        debug!(
            ?target_time,
            stream_pos = self.shared_stream.position(),
            stream_len = ?self.shared_stream.len(),
            "execute_recreation: after apply_format_change, about to decoder_seek_safe"
        );
        if !target_time.is_zero()
            && let Err(e) =
                self.decode
                    .seek(&self.shared_stream, self.playhead.as_ref(), target_time)
        {
            warn!(
                ?e,
                ?target_time,
                "Failed to seek decoder to timeline position after cross-codec recreate"
            );
            return classify(&e);
        }
        debug!(
            ?target_time,
            stream_pos_final = self.shared_stream.position(),
            "execute_recreation: FormatBoundary+Decode branch exit"
        );
        RecreateOutcome::Done
    }

    fn finish_recreate_outcome(
        &mut self,
        recreate: RecreateState,
        outcome: RecreateOutcome,
    ) -> TrackStep<PcmChunk> {
        match outcome {
            RecreateOutcome::Done => self.apply_recreate_next(&recreate),
            RecreateOutcome::SoftFailed => {
                self.update_state(CurrentFsm::failed(TrackFailure::RecreateFailed {
                    offset: recreate.offset,
                }));
                TrackStep::Failed
            }
            RecreateOutcome::NeedsSourceWait => self.wait_for_source_on_recreate(recreate),
        }
    }

    fn finish_rebuild(
        &mut self,
        rebuild: RebuildState,
        complete: DecoderRebuildComplete,
    ) -> TrackStep<PcmChunk> {
        if superseded(&self.shared_stream, self.seek_obs.as_ref(), &rebuild) {
            if let Ok(decoder) = complete.result {
                self.retired.retire(decoder);
            }
            return self.transition_after_rebuild_superseded(&rebuild);
        }
        let recreate = rebuild.recreate;
        let decoder = match complete.result {
            Ok(decoder) => decoder,
            Err(outcome) => return self.finish_recreate_outcome(recreate, outcome),
        };
        let duration = decoder.duration();
        let old = self.decode.install(
            decoder,
            recreate.media_info.clone(),
            recreate.offset,
            self.seek_obs.epoch(),
        );
        self.retired.retire(old);
        debug!(
            ?duration,
            offset = recreate.offset,
            "Decoder recreated successfully"
        );
        self.emit_event(AudioEvent::DecoderReady {
            base_offset: recreate.offset,
            variant: recreate.media_info.variant_index,
        });
        let outcome = if recreate_resumes_decode_head(&recreate) {
            self.finish_format_boundary_rebuild()
        } else {
            RecreateOutcome::Done
        };
        self.finish_recreate_outcome(recreate, outcome)
    }

    fn transition_to_seek_request(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
        self.update_state(CurrentFsm::seek_requested(request));
        self.decode.reset();
        self.decode.notify_seek();
        TrackStep::StateChanged
    }

    fn transition_after_rebuild_superseded(
        &mut self,
        rebuild: &RebuildState,
    ) -> TrackStep<PcmChunk> {
        let carried_seek = match &rebuild.recreate.next {
            RecreateNext::Seek(request) | RecreateNext::ApplySeek(request) => Some(*request),
            RecreateNext::Decode => None,
        };
        if let Some(request) = rebuild
            .superseded_seek
            .or_else(|| observed_seek(self.seek_obs.as_ref(), rebuild.started_seek_epoch))
            .or(carried_seek)
        {
            return self.transition_to_seek_request(request);
        }
        if let FormatDecision::Recreate(recreate) = detect(
            &self.shared_stream,
            self.decode.session(),
            self.seek_obs.as_ref(),
        ) {
            self.start_recreating_decoder(recreate);
        } else {
            self.update_state(CurrentFsm::decoding());
        }
        TrackStep::StateChanged
    }

    fn finish_apply_seek_after_recreate(&mut self, request: SeekRequest) -> TrackStep<PcmChunk> {
        debug!(
            target = ?request.seek.target,
            epoch = request.seek.epoch,
            emit_request = request.emit_request,
            committed_position = ?self.playhead.position(),
            stream_pos = self.shared_stream.position(),
            "finish_apply_seek_after_recreate: enter"
        );
        match self.decode.seek(
            &self.shared_stream,
            self.playhead.as_ref(),
            request.seek.target,
        ) {
            Ok(_outcome) => {
                self.decode.reset();
                self.seek_engine
                    .record_resume_target(request.seek.epoch, request.seek.target);
                self.emit_event(AudioEvent::SeekLifecycle {
                    stage: kithara_events::SeekLifecycleStage::SeekApplied,
                    seek_epoch: request.seek.epoch,
                    location: kithara_events::SegmentLocation::new(
                        self.shared_stream
                            .abr_handle()
                            .and_then(|handle| handle.current_variant_index()),
                        None,
                        None,
                        None,
                    ),
                });
                self.apply_seek_transition(SeekTransition::Applied {
                    epoch: request.seek.epoch,
                    resume: ResumeState {
                        seek: request.seek,
                        ..Default::default()
                    },
                });
                TrackStep::StateChanged
            }
            Err(err) => {
                self.apply_seek_transition(SeekTransition::Reject {
                    request,
                    error: err,
                    context: "step_recreating_decoder: recreated decoder seek failed",
                });
                TrackStep::StateChanged
            }
        }
    }

    /// Handle the "source not ready for boundary" branch of
    /// recreate. Transitions to `WaitingForSource` or terminates the
    /// track, depending on the source phase. Owns the `RecreateState`
    /// (moved out of the FSM by the caller) — no `self.state` re-read.
    fn wait_for_source_on_recreate(&mut self, recreate: RecreateState) -> TrackStep<PcmChunk> {
        let phase = recreate_phase(&self.shared_stream, &recreate);
        if let Some(reason) = self.readiness.source_park(&self.shared_stream, phase) {
            self.update_state(CurrentFsm::waiting(
                WaitContext::Recreation(recreate),
                reason,
            ));
            return TrackStep::Blocked(reason);
        }
        if phase == SourcePhase::Cancelled {
            self.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
            return TrackStep::Failed;
        }
        self.update_state(CurrentFsm::recreating(recreate));
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

impl Track<ApplyingSeek> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
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
            src.update_state(CurrentFsm::seek_requested(applying.request));
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
                src.update_state(CurrentFsm::waiting(
                    WaitContext::ApplySeek(applying),
                    reason,
                ));
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            src.update_state(CurrentFsm::applying_seek(applying));
            return TrackStep::Blocked(WaitingReason::Waiting);
        }
        let transition = src.seek_engine.apply(
            applying,
            SeekApplyCtx {
                decode: &mut src.decode,
                emit: src.emit.as_ref(),
                playhead: src.playhead.as_ref(),
                readiness: &src.readiness,
                seek: src.seek.as_ref(),
                observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        );
        src.apply_seek_transition(transition);
        TrackStep::StateChanged
    }
}

impl Track<AwaitingResume> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
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
                src.update_state(CurrentFsm::waiting(WaitContext::PostSeek(resume), reason));
                return TrackStep::Blocked(reason);
            }
        }
        // Restore the phase so the decode loop's `resume_state()` /
        // post-seek skip trimming sees the canonical `ResumeState`.
        src.update_state(CurrentFsm::awaiting_resume(resume));
        match decode_step(src) {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            DecodeStep::NotReady(reason) => TrackStep::Blocked(reason),
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

impl Track<Decoding> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let () = self.into_inner();
        if !src.readiness.source_is_ready(&src.shared_stream) {
            if !src.seek_obs.is_pending()
                && let FormatDecision::Recreate(recreate) = detect(
                    &src.shared_stream,
                    src.decode.session(),
                    src.seek_obs.as_ref(),
                )
            {
                src.start_recreating_decoder(recreate);
                return TrackStep::StateChanged;
            }
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(CurrentFsm::waiting(WaitContext::Playback, reason));
                return TrackStep::Blocked(reason);
            }
            if phase == SourcePhase::Cancelled {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            // Stay in Decoding — the dispatcher's sentinel is already
            // `Decoding`, so no restore is needed.
            return TrackStep::Blocked(WaitingReason::Waiting);
        }

        match decode_step(src) {
            DecodeStep::Produced(fetch) => TrackStep::Produced(fetch),
            DecodeStep::Interrupted => TrackStep::StateChanged,
            // The decoder read across the current segment boundary into a
            // not-ready (withheld) byte. Park in `WaitingForSource(Playback)`
            // rather than re-running the full decode every tick: the wait
            // state re-checks the forward read-ahead window cheaply and only
            // re-enters `Decoding` once that window is ready. Staying in
            // `Decoding` here hot-spins the decode loop (`source_is_ready`
            // gates only the current segment, so it never reflects the
            // blocked forward read) — flake F5.
            DecodeStep::NotReady(reason) => {
                src.update_state(CurrentFsm::waiting(WaitContext::Playback, reason));
                TrackStep::Blocked(reason)
            }
            DecodeStep::Eof => TrackStep::Eof,
            DecodeStep::Failed => TrackStep::Failed,
        }
    }
}

impl Track<RecreatingDecoder> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let recreate = self.into_inner();
        if !src
            .readiness
            .source_ready_for_recreate(&src.shared_stream, &recreate)
        {
            return src.wait_for_source_on_recreate(recreate);
        }
        match src.rebuild.prepare(&src.shared_stream, &recreate) {
            Ok((ticket, completion)) => {
                src.update_state(CurrentFsm::rebuilding(RebuildState {
                    ticket,
                    recreate,
                    started_seek_epoch: src.seek_obs.epoch(),
                    completion,
                    superseded_seek: None,
                }));
                TrackStep::StateChanged
            }
            Err(outcome) => src.finish_recreate_outcome(recreate, outcome),
        }
    }
}

impl Track<RebuildingDecoder> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let mut rebuild = self.into_inner();
        record_seek_preempt(&mut rebuild, src.seek_obs.as_ref(), src.seek_engine.epoch());
        while let Some(complete) = rebuild.completion.pop() {
            if complete.ticket == rebuild.ticket {
                return src.finish_rebuild(rebuild, complete);
            }
            if let Ok(decoder) = complete.result {
                src.retired.retire(decoder);
            }
        }
        src.update_state(CurrentFsm::rebuilding(rebuild));
        TrackStep::Blocked(WaitingReason::Waiting)
    }
}

impl Track<SeekRequested> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
        let request = self.into_inner();
        if !src.readiness.source_is_ready(&src.shared_stream) {
            let phase = src.shared_stream.phase();
            if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
                src.update_state(CurrentFsm::waiting(WaitContext::Seek(request), reason));
                return TrackStep::Blocked(reason);
            }
        }
        let transition = src.seek_engine.apply_from_timeline(
            request,
            &SeekApplyCtx {
                decode: &mut src.decode,
                emit: src.emit.as_ref(),
                playhead: src.playhead.as_ref(),
                readiness: &src.readiness,
                seek: src.seek.as_ref(),
                observe: src.seek_obs.as_ref(),
                stream: &src.shared_stream,
            },
        );
        src.apply_seek_transition(transition);
        TrackStep::StateChanged
    }
}

impl Track<WaitingForSource> {
    fn step<T: StreamType>(self, src: &mut StreamAudioSource<T>) -> TrackStep<PcmChunk> {
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
            src.update_state(CurrentFsm::seek_requested(applying.request));
            return TrackStep::StateChanged;
        }
        let phase = source_phase_for_wait_context(&src.shared_stream, &context);

        if let Some(reason) = src.readiness.source_park(&src.shared_stream, phase) {
            // Still waiting — restore the phase with its stored reason.
            src.update_state(CurrentFsm::waiting(context, stored_reason));
            return TrackStep::Blocked(reason);
        }

        match phase {
            SourcePhase::Cancelled => {
                src.update_state(CurrentFsm::failed(TrackFailure::SourceCancelled));
                return TrackStep::Failed;
            }
            SourcePhase::Eof => {
                src.update_state(CurrentFsm::at_eof());
                return TrackStep::Eof;
            }
            _ => {}
        }

        // Source ready — resume into the phase that initiated the wait.
        match context {
            WaitContext::Playback => src.update_state(CurrentFsm::decoding()),
            WaitContext::Seek(ctx) => src.update_state(CurrentFsm::seek_requested(ctx)),
            WaitContext::ApplySeek(applying) => {
                src.update_state(CurrentFsm::applying_seek(applying));
            }
            WaitContext::Recreation(recreate) => src.update_state(CurrentFsm::recreating(recreate)),
            WaitContext::PostSeek(resume) => src.update_state(CurrentFsm::awaiting_resume(resume)),
        }
        TrackStep::StateChanged
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
        // Publish any lifecycle event enqueued on the final produce pass before
        // the terminal node is dropped — `scheduler::run_loop` removes a
        // removable slot via `retain` without another `flush_deferred`, so a
        // terminal `EndOfStream` would otherwise be lost. Runs in the unchecked
        // shell (retain is outside `produce_pass`), off the forbid path.
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
        self.retired.drain();
    }
}

impl<T: StreamType> AudioWorkerSource for StreamAudioSource<T> {
    type Chunk = PcmChunk;

    fn decode_epoch(&self) -> u64 {
        // The epoch the current decode belongs to — stored when a seek is
        // applied (`ApplyingSeek` / `try_apply_seek`), and the same value
        // stamped on produced chunks (`decode_one_step`). It LAGS
        // `timeline().seek_epoch()`, which the consumer bumps the instant it
        // requests a seek, long before the worker applies it. A terminal
        // marker (EOF / failure) must carry this decode epoch so a stale
        // end-of-stream produced for a superseded seek is discarded by the
        // consumer's validator rather than mistaken for the new seek's
        // terminal (the oversubscription false-EOF race).
        self.seek_engine.epoch()
    }

    fn flush_deferred(&mut self) {
        self.decode.flush_reader_signals();
        self.retired.drain();
        self.rebuild.submit();
        // Publish the FSM lifecycle events the produce core enqueued this pass,
        // off the forbid path (the `broadcast::send` is a `kevent`).
        if let Some(ref emit) = self.emit {
            emit.flush();
        }
        // Deliver the peer wake the produce core armed this pass (a blocked
        // `probe_read`, a seek-apply / finalize). The `notify_one` is a
        // cross-thread `kevent` the forbid-blocking core must not make, so it
        // lands here in the shell. Same `Arc<DeferredWake>` the reader drivers
        // and the FSM arm, so one flush covers both. `None` for file streams.
        self.readiness.flush_peer_wake();
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        Arc::clone(&self.seek_obs)
    }

    fn step_track(&mut self) -> TrackStep<PcmChunk> {
        if !matches!(self.state, CurrentFsm::RebuildingDecoder(_))
            && let Some(target) =
                preempt_target(&self.seek_engine, &self.state, self.seek_obs.as_ref())
        {
            self.update_state(CurrentFsm::seek_requested(SeekRequest {
                seek: SeekContext {
                    target,
                    epoch: self.seek_obs.epoch(),
                },
                emit_request: false,
            }));
            self.decode.reset();
            self.decode.notify_seek();
            return TrackStep::StateChanged;
        }
        if !matches!(
            self.state,
            CurrentFsm::RecreatingDecoder(_)
                | CurrentFsm::RebuildingDecoder(_)
                | CurrentFsm::AtEof(_)
                | CurrentFsm::Failed(_)
        ) && self.start_route_change_recreate_if_needed()
        {
            return TrackStep::StateChanged;
        }

        // Move the typed handle out (sentinel = `Decoding`, the unit
        // phase, so the decode hot path that stays in `Decoding` needs
        // no restore). Each `step` either transitions via
        // `update_state` or restores its own phase before returning.
        match mem::replace(&mut self.state, CurrentFsm::decoding()) {
            CurrentFsm::Decoding(handle) => handle.step(self),
            CurrentFsm::SeekRequested(handle) => handle.step(self),
            CurrentFsm::WaitingForSource(handle) => handle.step(self),
            CurrentFsm::ApplyingSeek(handle) => handle.step(self),
            CurrentFsm::RecreatingDecoder(handle) => handle.step(self),
            CurrentFsm::RebuildingDecoder(handle) => handle.step(self),
            CurrentFsm::AwaitingResume(handle) => handle.step(self),
            CurrentFsm::AtEof(handle) => {
                self.state = CurrentFsm::AtEof(handle);
                TrackStep::Eof
            }
            CurrentFsm::Failed(handle) => {
                emit_failure_log(handle.data());
                self.state = CurrentFsm::Failed(handle);
                TrackStep::Failed
            }
        }
    }

    fn warm_up(&mut self) {
        // The storage committed-read fast path (`MemDriver::committed_len` /
        // `read_committed` behind an `arc_swap::ArcSwapOption`) lazily
        // `Box`-allocates this thread's `arc_swap` debt node on its FIRST load.
        // Left to the produce core, that one-time alloc lands inside the
        // forbid-blocking region (the first committed `len`/`contains_range`/
        // read after the resource opens). The debt node is process-global per
        // thread and shared by every `ArcSwap` regardless of payload type, so a
        // throwaway load here — in the scheduler shell, before any checked
        // `tick` — allocates it off the RT path and warms every real storage
        // read. It is resource-independent, so it works even before this
        // source's resource has been opened (the `len()` path only reaches
        // `committed_len` once the resource is live).
        let warm = ArcSwap::from_pointee(());
        let _ = warm.load();
        let _ = self.shared_stream.len();
    }
}

/// Classify a [`CurrentFsm`] phase for the shared activity `PLAYING` flag.
///
/// The Downloader peers read `Activity::is_playing()` in their
/// `priority()` method. Every non-terminal phase keeps this track
/// "listened to" from the user's perspective — buffering, seek-in-
/// progress, and decoder recreation are all transient windows inside
/// an otherwise-active track. Only `AtEof` (natural end) and `Failed`
/// (terminal error) clear the flag.
fn playing_for_state(state: &CurrentFsm) -> bool {
    !matches!(state, CurrentFsm::AtEof(_) | CurrentFsm::Failed(_))
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

fn recreate_resumes_decode_head(recreate: &RecreateState) -> bool {
    recreate.cause == RecreateCause::FormatBoundary && matches!(recreate.next, RecreateNext::Decode)
}

#[cfg(test)]
mod rebuilding_decoder_tests {
    use std::{
        num::NonZeroU32,
        ops::Range,
        sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    };

    use kithara_bufpool::PcmPool;
    use kithara_decode::{
        DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessMode, PcmMeta,
        PcmSpec, duration_for_frames, frames_for_duration,
    };
    use kithara_platform::{
        sync::{Arc, Mutex},
        time::Duration,
        tokio::runtime::Handle as RuntimeHandle,
    };
    use kithara_storage::WaitOutcome;
    use kithara_stream::{
        Activity, AudioCodec, ChunkPosition, ContainerFormat, MediaInfo, PlayheadRead,
        PlayheadState, PrerollHint, ReadOutcome, SeekState, Source, SourceError, Stream,
        StreamError, StreamResult, VariantControl, WorkerWake,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::rebuild::port::RebuildRuntime;

    struct Consts;

    impl Consts {
        const CHANNELS: u16 = 2;
        const ROUTE_CHUNK_FRAMES: usize = 256;
        const ROUTE_SAMPLE_RATE: u32 = 48_000;
        const SAMPLE_RATE: u32 = 44_100;
        const TONE_HZ: f64 = 440.0;
    }

    struct TestDecoder {
        id: u64,
        drops: Arc<Mutex<Vec<u64>>>,
    }

    impl TestDecoder {
        fn new(id: u64, drops: Arc<Mutex<Vec<u64>>>) -> Self {
            Self { id, drops }
        }
    }

    impl Drop for TestDecoder {
        fn drop(&mut self) {
            self.drops.lock().push(self.id);
        }
    }

    impl Decoder for TestDecoder {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(DecoderChunkOutcome::Eof)
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(DecoderSeekOutcome::Landed {
                landed_at: pos,
                landed_frame: 0,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }

        fn spec(&self) -> PcmSpec {
            PcmSpec::new(2, NonZeroU32::MIN)
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    struct RouteSignalDecoder {
        drops: Arc<Mutex<Vec<u64>>>,
        id: u64,
        next_frame: u64,
        sample_rate: u32,
    }

    impl RouteSignalDecoder {
        fn new(id: u64, sample_rate: u32, drops: Arc<Mutex<Vec<u64>>>) -> Self {
            Self {
                drops,
                id,
                next_frame: 0,
                sample_rate,
            }
        }

        fn pcm_spec(&self) -> PcmSpec {
            PcmSpec::new(
                Consts::CHANNELS,
                NonZeroU32::new(self.sample_rate).unwrap_or(NonZeroU32::MIN),
            )
        }
    }

    impl Drop for RouteSignalDecoder {
        fn drop(&mut self) {
            self.drops.lock().push(self.id);
        }
    }

    impl Decoder for RouteSignalDecoder {
        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(60))
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            let spec = self.pcm_spec();
            let channels = usize::from(Consts::CHANNELS);
            let frames = Consts::ROUTE_CHUNK_FRAMES;
            let mut samples = PcmPool::default().get();
            samples.resize(frames.saturating_mul(channels), 0.0);
            for frame in 0..frames {
                let absolute = self
                    .next_frame
                    .saturating_add(u64::try_from(frame).unwrap_or(u64::MAX));
                let absolute_f64 =
                    num_traits::cast::ToPrimitive::to_f64(&absolute).unwrap_or(f64::MAX);
                let t = absolute_f64 / f64::from(self.sample_rate);
                let sample = (t * Consts::TONE_HZ * std::f64::consts::TAU).sin() * 0.25;
                let sample = num_traits::cast::ToPrimitive::to_f32(&sample).unwrap_or(0.0);
                let base = frame.saturating_mul(channels);
                samples[base] = sample;
                samples[base + 1] = sample;
            }
            let frame_count = u32::try_from(frames).unwrap_or(u32::MAX);
            let start = self.next_frame;
            let end = start.saturating_add(u64::from(frame_count));
            self.next_frame = end;
            Ok(DecoderChunkOutcome::Chunk(PcmChunk::new(
                PcmMeta {
                    spec,
                    timestamp: duration_for_frames(self.sample_rate, start),
                    end_timestamp: duration_for_frames(self.sample_rate, end),
                    frame_offset: start,
                    frames: frame_count,
                    ..Default::default()
                },
                samples,
            )))
        }

        fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            let frame =
                u64::try_from(frames_for_duration(self.sample_rate, pos)).unwrap_or(u64::MAX);
            self.next_frame = frame;
            Ok(DecoderSeekOutcome::Landed {
                landed_at: duration_for_frames(self.sample_rate, frame),
                landed_frame: frame,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
            })
        }

        fn spec(&self) -> PcmSpec {
            self.pcm_spec()
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    struct TestWake;

    impl WorkerWake for TestWake {
        fn wake(&self) {}
    }

    #[derive(Default)]
    struct CountingWake {
        count: AtomicU64,
    }

    impl CountingWake {
        fn count(&self) -> u64 {
            self.count.load(Ordering::Acquire)
        }
    }

    impl WorkerWake for CountingWake {
        fn wake(&self) {
            self.count.fetch_add(1, Ordering::Release);
        }
    }

    struct TestControl {
        media_info: Mutex<Option<MediaInfo>>,
        variant_pending: AtomicBool,
        variant_target: Mutex<Option<usize>>,
        format_range: Mutex<Option<Range<u64>>>,
    }

    impl TestControl {
        fn new(media_info: MediaInfo) -> Self {
            Self {
                media_info: Mutex::new(Some(media_info)),
                variant_pending: AtomicBool::new(false),
                variant_target: Mutex::new(None),
                format_range: Mutex::new(Some(0..32)),
            }
        }

        fn set_media_info(&self, media_info: MediaInfo) {
            *self.media_info.lock() = Some(media_info);
        }

        fn raise_variant_fence(&self, target: usize, media_info: MediaInfo) {
            self.set_media_info(media_info);
            *self.variant_target.lock() = Some(target);
            self.variant_pending.store(true, Ordering::Release);
        }
    }

    impl VariantControl for TestControl {
        fn clear_variant_fence(&self) {
            self.variant_pending.store(false, Ordering::Release);
            *self.variant_target.lock() = None;
        }

        fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
            self.format_range
                .lock()
                .clone()
                .ok_or(StreamError::Source(SourceError::FormatChangeNotApplicable))
        }

        fn has_variant_change_pending(&self) -> bool {
            self.variant_pending.load(Ordering::Acquire)
        }

        fn variant_change_target(&self) -> Option<usize> {
            *self.variant_target.lock()
        }
    }

    struct TestSource {
        control: Arc<TestControl>,
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
    }

    impl TestSource {
        fn new(control: Arc<TestControl>) -> Self {
            Self {
                control,
                playhead: Arc::new(PlayheadState::new()),
                position: Arc::new(AtomicU64::new(0)),
                seek: Arc::new(SeekState::new()),
            }
        }
    }

    impl Source for TestSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn len(&self) -> Option<u64> {
            Some(4096)
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.control.media_info.lock().clone()
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            Ok(ReadOutcome::Eof)
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
            Some(Arc::clone(&self.control) as Arc<dyn VariantControl>)
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            Ok(WaitOutcome::Ready)
        }
    }

    struct TestConfig {
        source: TestSource,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                source: TestSource::new(Arc::new(TestControl::new(media_info(0)))),
            }
        }
    }

    struct TestStream;

    impl StreamType for TestStream {
        type Config = TestConfig;
        type Events = ();
        type Source = TestSource;

        async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
            Ok(config.source)
        }
    }

    fn media_info(variant: u32) -> MediaInfo {
        let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        info.variant_index = Some(variant);
        info
    }

    fn recreate_state(variant: u32) -> RecreateState {
        RecreateState {
            media_info: media_info(variant),
            cause: RecreateCause::FormatBoundary,
            next: RecreateNext::Decode,
            offset: 0,
        }
    }

    struct RebuildFixture {
        control: Arc<TestControl>,
        drops: Arc<Mutex<Vec<u64>>>,
        source: StreamAudioSource<TestStream>,
    }

    struct RouteFixture {
        host_sample_rate: Arc<AtomicU32>,
        source: StreamAudioSource<TestStream>,
    }

    async fn test_source(variant: u32) -> RebuildFixture {
        let control = Arc::new(TestControl::new(media_info(variant)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let stream = match Stream::<TestStream>::new(TestConfig {
            source: TestSource::new(control.clone()),
        })
        .await
        {
            Ok(stream) => stream,
            Err(err) => panic!("test stream construction failed: {err}"),
        };
        let shared_stream = SharedStream::new(stream);
        let factory_drops = drops.clone();
        let decoder_factory: DecoderFactory<TestStream> =
            Arc::new(move |_stream, _info, _offset| {
                Ok(Box::new(TestDecoder::new(99, factory_drops.clone())))
            });
        let runtime_handle = match RuntimeHandle::try_current() {
            Ok(handle) => handle,
            Err(err) => panic!("test requires tokio runtime: {err}"),
        };
        let decode = DecodeInit {
            decoder: Box::new(TestDecoder::new(1, drops.clone())),
            decoder_factory,
            gapless_mode: GaplessMode::Disabled,
            host_sample_rate: Arc::new(AtomicU32::new(Consts::SAMPLE_RATE)),
            media_info: Some(media_info(0)),
            recreate_on_host_rate_change: true,
        }
        .into_parts(Vec::new(), shared_stream.seek_observe().epoch());
        let parts = SourceParts::new(
            &shared_stream,
            decode,
            Arc::new(AtomicU64::new(0)),
            RebuildRuntime {
                handle: runtime_handle,
                wake: Arc::new(TestWake),
            },
        );
        RebuildFixture {
            control,
            drops,
            source: StreamAudioSource::new(shared_stream, parts),
        }
    }

    async fn route_signal_source(initial_host_rate: u32) -> RouteFixture {
        let control = Arc::new(TestControl::new(media_info(0)));
        let drops = Arc::new(Mutex::new(Vec::new()));
        let host_sample_rate = Arc::new(AtomicU32::new(initial_host_rate));
        let stream = match Stream::<TestStream>::new(TestConfig {
            source: TestSource::new(control),
        })
        .await
        {
            Ok(stream) => stream,
            Err(err) => panic!("test stream construction failed: {err}"),
        };
        let shared_stream = SharedStream::new(stream);
        let factory_drops = drops.clone();
        let factory_host_rate = host_sample_rate.clone();
        let decoder_factory: DecoderFactory<TestStream> =
            Arc::new(move |_stream, _info, _offset| {
                let rate = factory_host_rate.load(Ordering::Acquire);
                Ok(Box::new(RouteSignalDecoder::new(
                    99,
                    rate,
                    factory_drops.clone(),
                )))
            });
        let runtime_handle = match RuntimeHandle::try_current() {
            Ok(handle) => handle,
            Err(err) => panic!("test requires tokio runtime: {err}"),
        };
        let decode = DecodeInit {
            decoder: Box::new(RouteSignalDecoder::new(1, Consts::SAMPLE_RATE, drops)),
            decoder_factory,
            gapless_mode: GaplessMode::Disabled,
            host_sample_rate: host_sample_rate.clone(),
            media_info: Some(media_info(0)),
            recreate_on_host_rate_change: true,
        }
        .into_parts(Vec::new(), shared_stream.seek_observe().epoch());
        let parts = SourceParts::new(
            &shared_stream,
            decode,
            Arc::new(AtomicU64::new(0)),
            RebuildRuntime {
                handle: runtime_handle,
                wake: Arc::new(TestWake),
            },
        );
        RouteFixture {
            host_sample_rate,
            source: StreamAudioSource::new(shared_stream, parts),
        }
    }

    fn run_pending_rebuild_inline(source: &mut StreamAudioSource<TestStream>) {
        source.rebuild.run_inline();
    }

    fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
        let channels = usize::from(chunk.meta.spec.channels);
        for frame in 0..chunk.frames() {
            left.push(chunk.samples[frame * channels]);
        }
    }

    fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
        assert!(
            (1..left.len()).contains(&center),
            "first-difference center must be in 1..{}, got {center}",
            left.len(),
        );
        let start = center.saturating_sub(half).max(1);
        let end = center.saturating_add(half).min(left.len() - 1);
        let mut peak = 0.0_f32;
        for i in start..=end {
            peak = peak.max((left[i] - left[i - 1]).abs());
        }
        peak
    }

    fn next_test_chunk(
        source: &mut StreamAudioSource<TestStream>,
        route_recreated: &mut bool,
    ) -> PcmChunk {
        loop {
            run_pending_rebuild_inline(source);
            match source.step_track() {
                TrackStep::Produced(fetch) => return fetch.into_inner(),
                TrackStep::StateChanged => {
                    if matches!(
                        &source.state,
                        CurrentFsm::RecreatingDecoder(handle)
                            if handle.data().cause == RecreateCause::RouteChange
                    ) {
                        *route_recreated = true;
                    }
                }
                TrackStep::Blocked(_) => {}
                TrackStep::Eof => panic!("route test source reached EOF"),
                TrackStep::Failed => panic!("route test source failed"),
            }
        }
    }

    fn enter_rebuilding(
        source: &mut StreamAudioSource<TestStream>,
        ticket: u64,
        recreate: RecreateState,
    ) {
        source.state = CurrentFsm::rebuilding(RebuildState {
            ticket,
            recreate,
            started_seek_epoch: source.seek_obs.epoch(),
            completion: source.rebuild.completion(),
            superseded_seek: None,
        });
    }

    fn push_completion_with_drops(
        source: &StreamAudioSource<TestStream>,
        ticket: u64,
        decoder_id: u64,
        drops: Arc<Mutex<Vec<u64>>>,
    ) {
        let pushed = source.rebuild.completion().push(DecoderRebuildComplete {
            result: Ok(Box::new(TestDecoder::new(decoder_id, drops))),
            ticket,
        });
        assert!(pushed.is_ok());
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_pending_poll_blocks() {
        let RebuildFixture { mut source, .. } = test_source(1).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));

        assert!(matches!(
            source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ));
        assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_completion_installs_once() {
        let RebuildFixture {
            drops, mut source, ..
        } = test_source(1).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        push_completion_with_drops(&source, 7, 2, drops.clone());

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        assert_eq!(
            source
                .decode
                .session()
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(1)
        );
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
        assert_eq!(source.retired.len(), 1);

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[1]);
    }

    #[kithara::test(tokio)]
    async fn route_change_host_rate_delta_starts_decoder_recreate() {
        let RouteFixture {
            host_sample_rate,
            mut source,
        } = route_signal_source(Consts::SAMPLE_RATE).await;

        host_sample_rate.store(48_000, Ordering::Release);

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                let recreate = handle.data();
                assert_eq!(recreate.cause, RecreateCause::RouteChange);
                match &recreate.next {
                    RecreateNext::ApplySeek(request) => {
                        assert_eq!(request.seek.epoch, source.seek_engine.epoch());
                        assert_eq!(request.seek.target, source.playhead.position());
                        assert!(!request.emit_request);
                    }
                    _ => panic!("expected route-change recreate to resume via ApplySeek"),
                }
                assert_eq!(recreate.offset, source.decode.session().base_offset);
                assert_eq!(recreate.media_info.variant_index, Some(0));
            }
            _ => panic!("expected route-change recreate"),
        }
    }

    #[kithara::test(tokio)]
    async fn route_change_recreate_preserves_position_and_output_rate_continuity_metric() {
        let RouteFixture {
            host_sample_rate,
            mut source,
        } = route_signal_source(Consts::SAMPLE_RATE).await;
        let mut left = Vec::new();
        let mut route_recreated = false;

        for _ in 0..8 {
            let chunk = next_test_chunk(&mut source, &mut route_recreated);
            assert_eq!(chunk.meta.spec.sample_rate.get(), Consts::SAMPLE_RATE);
            append_left_channel(&mut left, &chunk);
            source.playhead.advance(&ChunkPosition::from(&chunk.meta));
        }

        let route_frame = left.len();
        let route_position = source.playhead.position();
        host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

        let mut first_route_timestamp = None;
        let mut saw_new_rate = false;
        for _ in 0..8 {
            let chunk = next_test_chunk(&mut source, &mut route_recreated);
            if first_route_timestamp.is_none() {
                first_route_timestamp = Some(chunk.meta.timestamp);
            }
            saw_new_rate |= chunk.meta.spec.sample_rate.get() == Consts::ROUTE_SAMPLE_RATE;
            append_left_channel(&mut left, &chunk);
            source.playhead.advance(&ChunkPosition::from(&chunk.meta));
        }

        assert!(
            route_recreated,
            "route change must enter recreate machinery"
        );
        assert!(
            saw_new_rate,
            "route-change output chunks must report the new host rate"
        );
        assert_eq!(
            source.decode.session().decoder.spec().sample_rate.get(),
            Consts::ROUTE_SAMPLE_RATE
        );
        let first_route_timestamp =
            first_route_timestamp.expect("route change should produce post-route PCM");
        let drift_ns = first_route_timestamp.abs_diff(route_position).as_nanos();
        assert!(
            drift_ns <= 1_000_000,
            "route recreate drifted by {drift_ns} ns from {route_position:?} to {first_route_timestamp:?}",
        );

        let route_peak = peak_first_diff(&left, route_frame, 64);
        let control_peak = peak_first_diff(&left, Consts::ROUTE_CHUNK_FRAMES * 4, 64);
        let ratio = route_peak / control_peak.max(f32::EPSILON);
        println!(
            "S_ROUTE_CONTINUITY route_peak={route_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 2.0,
            "route-change discontinuity {route_peak:.6} is {ratio:.1}x the control boundary {control_peak:.6}",
        );
    }

    #[kithara::test(tokio)]
    async fn equal_host_rate_does_not_start_route_recreate() {
        let RebuildFixture { mut source, .. } = test_source(0).await;

        assert!(!source.start_route_change_recreate_if_needed());
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    }

    #[kithara::test(tokio)]
    async fn first_matching_host_rate_latches_without_route_recreate() {
        let RouteFixture {
            host_sample_rate,
            mut source,
        } = route_signal_source(0).await;

        host_sample_rate.store(Consts::SAMPLE_RATE, Ordering::Release);

        assert!(!source.start_route_change_recreate_if_needed());
        assert_eq!(source.resume.decoder_rate(), Consts::SAMPLE_RATE);
        assert!(matches!(source.state, CurrentFsm::Decoding(_)));
    }

    #[kithara::test(tokio)]
    async fn first_mismatched_host_rate_still_starts_route_recreate() {
        let RouteFixture {
            host_sample_rate,
            mut source,
        } = route_signal_source(0).await;

        host_sample_rate.store(Consts::ROUTE_SAMPLE_RATE, Ordering::Release);

        assert!(source.start_route_change_recreate_if_needed());
        assert_eq!(source.resume.decoder_rate(), Consts::ROUTE_SAMPLE_RATE);
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                assert_eq!(handle.data().cause, RecreateCause::RouteChange);
            }
            _ => panic!("expected route-change recreate"),
        }
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_seek_epoch_supersedes_completion() {
        let RebuildFixture {
            drops, mut source, ..
        } = test_source(1).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        let epoch = source.seek.begin(Duration::from_secs(3));
        push_completion_with_drops(&source, 7, 2, drops.clone());

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::SeekRequested(handle) => {
                assert_eq!(handle.data().seek.epoch, epoch);
                assert_eq!(handle.data().seek.target, Duration::from_secs(3));
            }
            _ => panic!("expected seek request after rebuild supersession"),
        }
        assert_eq!(
            source
                .decode
                .session()
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_variant_fence_supersedes_completion() {
        let RebuildFixture {
            control,
            drops,
            mut source,
        } = test_source(1).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        control.raise_variant_fence(2, media_info(2));
        push_completion_with_drops(&source, 7, 2, drops.clone());

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::RecreatingDecoder(handle) => {
                assert_eq!(handle.data().media_info.variant_index, Some(2));
            }
            _ => panic!("expected fresh recreate after variant supersession"),
        }
        assert_eq!(
            source
                .decode
                .session()
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn rebuilding_decoder_variant_fence_preserves_inflight_seek() {
        let RebuildFixture {
            control,
            drops,
            mut source,
        } = test_source(1).await;
        let target = Duration::from_secs(3);
        let request = SeekRequest {
            seek: SeekContext {
                epoch: source.seek.begin(target),
                target,
            },
            emit_request: false,
        };
        enter_rebuilding(
            &mut source,
            7,
            RecreateState {
                cause: RecreateCause::VariantSwitch,
                next: RecreateNext::Seek(request),
                ..recreate_state(1)
            },
        );
        control.raise_variant_fence(2, media_info(2));
        push_completion_with_drops(&source, 7, 2, drops.clone());

        assert!(matches!(source.step_track(), TrackStep::StateChanged));
        match &source.state {
            CurrentFsm::SeekRequested(handle) => assert_eq!(*handle.data(), request),
            _ => panic!("expected in-flight seek after variant supersession"),
        }
        assert_eq!(
            source
                .decode
                .session()
                .media_info
                .as_ref()
                .and_then(|i| i.variant_index),
            Some(0)
        );
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[2]);
    }

    #[kithara::test(tokio)]
    async fn stale_rebuild_completion_retires_decoder_shell_side() {
        let RebuildFixture {
            drops, mut source, ..
        } = test_source(1).await;
        enter_rebuilding(&mut source, 7, recreate_state(1));
        push_completion_with_drops(&source, 6, 3, drops.clone());

        assert!(matches!(
            source.step_track(),
            TrackStep::Blocked(WaitingReason::Waiting)
        ));
        assert!(matches!(source.state, CurrentFsm::RebuildingDecoder(_)));
        assert!(drops.lock().is_empty());

        source.flush_deferred();
        assert_eq!(drops.lock().as_slice(), &[3]);
    }

    // A decoder factory that panics during construction must not strand the
    // FSM in `RebuildingDecoder` forever. The rebuild port catches the panic,
    // pushes a `SoftFailed` completion, and wakes the worker.
    #[kithara::test(tokio)]
    async fn rebuild_factory_panic_fails_track_without_hang() {
        let RebuildFixture { mut source, .. } = test_source(1).await;

        let wake = Arc::new(CountingWake::default());
        let panicking_factory: DecoderFactory<TestStream> =
            Arc::new(|_stream, _info, _offset| panic!("decoder construction blew up"));
        source.rebuild = RebuildPort::new(
            panicking_factory,
            RebuildRuntime {
                handle: source.rebuild.runtime().clone(),
                wake: Arc::clone(&wake) as Arc<dyn WorkerWake>,
            },
        );
        let recreate = recreate_state(1);
        let (ticket, completion) = source
            .rebuild
            .prepare(&source.shared_stream, &recreate)
            .expect("panic test rebuild must prepare");
        source.state = CurrentFsm::rebuilding(RebuildState {
            ticket,
            recreate,
            started_seek_epoch: source.seek_obs.epoch(),
            completion,
            superseded_seek: None,
        });

        // Run the rebuild job synchronously through the same `run` path the
        // blocking pool uses: a factory panic must be caught by `catch_unwind`,
        // push a `SoftFailed` completion, and wake the worker rather than
        // stranding the FSM in `RebuildingDecoder`.
        source.rebuild.run_inline();
        assert_eq!(wake.count(), 1, "factory panic must wake the worker");

        // The worker's next step must reach the terminal recreate failure,
        // not loop on `Blocked(Waiting)`.
        assert!(matches!(source.step_track(), TrackStep::Failed));
        match &source.state {
            CurrentFsm::Failed(handle) => {
                assert!(matches!(handle.data(), TrackFailure::RecreateFailed { .. }));
            }
            _ => panic!("expected RecreateFailed terminal state after factory panic"),
        }
    }
}

#[cfg(test)]
mod splice_continuity_tests {
    use std::{
        num::NonZeroUsize,
        ops::Range,
        path::Path,
        sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    };

    use kithara_bufpool::PcmPool;
    use kithara_decode::{DecoderConfig, DecoderFactory as DecodeFactory, GaplessMode};
    use kithara_platform::{
        sync::{Arc, Mutex},
        time::Duration,
        tokio::runtime::Handle as RuntimeHandle,
    };
    use kithara_storage::WaitOutcome;
    use kithara_stream::{
        Activity, AudioCodec, ByteMap, ChunkPosition, ContainerFormat, MediaInfo, PlayheadRead,
        PlayheadState, PlayheadWrite, ReadOutcome, SeekState, SegmentDescriptor, Source,
        SourceError, SourceSeekAnchor, Stream, StreamError, StreamResult, VariantControl,
        WorkerWake,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::rebuild::port::RebuildRuntime;

    struct Consts;

    impl Consts {
        const CHANNELS: usize = 2;
        const SAMPLE_RATE: u32 = 44_100;
        const SEGMENT_DURATION_SECS: u64 = 6;
        const SPLICE_SEGMENT: u32 = 3;
        const TOTAL_SEGMENTS: usize = 7;
        const CAPTURE_END_SEGMENT: u64 = 6;
        const SLQ_VARIANT: usize = 0;
        const SMQ_VARIANT: usize = 1;
    }

    struct VariantLayout {
        blob: Vec<u8>,
        init_range: Range<u64>,
        segments: Vec<SegmentDescriptor>,
    }

    struct SpliceState {
        active: AtomicUsize,
        media_info: Mutex<Option<MediaInfo>>,
        pending_variant_change: AtomicBool,
        target_variant: Mutex<Option<usize>>,
        variants: Vec<VariantLayout>,
        warmup_landing: Mutex<Option<SegmentDescriptor>>,
    }

    impl SpliceState {
        fn new(variants: Vec<VariantLayout>) -> Self {
            Self {
                active: AtomicUsize::new(Consts::SLQ_VARIANT),
                media_info: Mutex::new(Some(media_info(Consts::SLQ_VARIANT))),
                pending_variant_change: AtomicBool::new(false),
                target_variant: Mutex::new(None),
                variants,
                warmup_landing: Mutex::new(None),
            }
        }

        fn active_index(&self) -> usize {
            self.active
                .load(Ordering::Acquire)
                .min(self.variants.len().saturating_sub(1))
        }

        fn active_layout(&self) -> &VariantLayout {
            &self.variants[self.active_index()]
        }

        fn switch_to(&self, variant: usize) {
            self.active.store(variant, Ordering::Release);
            *self.media_info.lock() = Some(media_info(variant));
            *self.target_variant.lock() = Some(variant);
            self.pending_variant_change.store(true, Ordering::Release);
        }

        fn warmup_landing(&self) -> Option<SegmentDescriptor> {
            self.warmup_landing.lock().clone()
        }
    }

    impl VariantControl for SpliceState {
        fn clear_variant_fence(&self) {
            self.pending_variant_change.store(false, Ordering::Release);
            *self.target_variant.lock() = None;
        }

        fn format_change_segment_range(&self) -> StreamResult<Range<u64>> {
            let range = self.active_layout().init_range.clone();
            if range.is_empty() {
                Err(StreamError::Source(SourceError::FormatChangeNotApplicable))
            } else {
                Ok(range)
            }
        }

        fn has_variant_change_pending(&self) -> bool {
            self.pending_variant_change.load(Ordering::Acquire)
        }

        fn variant_change_target(&self) -> Option<usize> {
            *self.target_variant.lock()
        }
    }

    impl ByteMap for SpliceState {
        fn anchor_at_time(&self, position: Duration) -> StreamResult<Option<SourceSeekAnchor>> {
            Ok(self.segment_at_time(position).map(|segment| {
                SourceSeekAnchor::builder()
                    .segment_start(segment.decode_time)
                    .segment_end(segment.decode_time.saturating_add(segment.duration))
                    .segment_index(segment.segment_index)
                    .variant_index(segment.variant_index)
                    .byte_offset(segment.byte_range.start)
                    .build()
            }))
        }

        fn init_segment_range(&self) -> Range<u64> {
            self.active_layout().init_range.clone()
        }

        fn len(&self) -> Option<u64> {
            Some(u64::try_from(self.active_layout().blob.len()).expect("blob length fits u64"))
        }

        fn segment_after_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .iter()
                .find(|segment| segment.byte_range.start >= byte_offset)
                .cloned()
        }

        fn segment_at_byte(&self, byte_offset: u64) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .iter()
                .find(|segment| segment.byte_range.contains(&byte_offset))
                .cloned()
        }

        fn segment_at_index(&self, segment_index: u32) -> Option<SegmentDescriptor> {
            self.active_layout()
                .segments
                .get(usize::try_from(segment_index).ok()?)
                .cloned()
        }

        fn segment_at_time(&self, t: Duration) -> Option<SegmentDescriptor> {
            let found = self
                .active_layout()
                .segments
                .iter()
                .find(|segment| t < segment.decode_time.saturating_add(segment.duration))
                .or_else(|| self.active_layout().segments.last())
                .cloned();
            if self.active_index() == Consts::SMQ_VARIANT
                && t < Duration::from_secs(120)
                && let Some(segment) = found.as_ref()
            {
                *self.warmup_landing.lock() = Some(segment.clone());
            }
            found
        }

        fn segment_count(&self) -> Option<u32> {
            u32::try_from(self.active_layout().segments.len()).ok()
        }
    }

    struct SpliceSource {
        playhead: Arc<PlayheadState>,
        position: Arc<AtomicU64>,
        seek: Arc<SeekState>,
        state: Arc<SpliceState>,
    }

    impl SpliceSource {
        fn new(state: Arc<SpliceState>) -> Self {
            Self {
                playhead: Arc::new(PlayheadState::new()),
                position: Arc::new(AtomicU64::new(0)),
                seek: Arc::new(SeekState::new()),
                state,
            }
        }
    }

    impl Source for SpliceSource {
        fn activity(&self) -> Arc<dyn Activity> {
            Arc::clone(&self.seek) as Arc<dyn Activity>
        }

        fn advance(&self, n: u64) {
            self.position.fetch_add(n, Ordering::AcqRel);
        }

        fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
            Some(Arc::clone(&self.state) as Arc<dyn ByteMap>)
        }

        fn len(&self) -> Option<u64> {
            Some(
                u64::try_from(self.state.active_layout().blob.len()).expect("blob length fits u64"),
            )
        }

        fn media_info(&self) -> Option<MediaInfo> {
            self.state.media_info.lock().clone()
        }

        fn phase_at(&self, _range: Range<u64>) -> SourcePhase {
            SourcePhase::Ready
        }

        fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadRead>
        }

        fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
            Arc::clone(&self.playhead) as Arc<dyn PlayheadWrite>
        }

        fn position(&self) -> u64 {
            self.position.load(Ordering::Acquire)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome> {
            let blob = &self.state.active_layout().blob;
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start >= blob.len() {
                return Ok(ReadOutcome::Eof);
            }
            let n = (blob.len() - start).min(buf.len());
            buf[..n].copy_from_slice(&blob[start..start + n]);
            Ok(ReadOutcome::Bytes(
                NonZeroUsize::new(n).expect("non-empty read must produce nonzero bytes"),
            ))
        }

        fn seek_control(&self) -> Arc<dyn SeekControl> {
            Arc::clone(&self.seek) as Arc<dyn SeekControl>
        }

        fn seek_observe(&self) -> Arc<dyn SeekObserve> {
            Arc::clone(&self.seek) as Arc<dyn SeekObserve>
        }

        fn set_position(&self, pos: u64) {
            self.position.store(pos, Ordering::Release);
        }

        fn variant_control(&self) -> Option<Arc<dyn VariantControl>> {
            Some(Arc::clone(&self.state) as Arc<dyn VariantControl>)
        }

        fn wait_range(
            &mut self,
            _range: Range<u64>,
            _timeout: Option<Duration>,
        ) -> StreamResult<WaitOutcome> {
            Ok(WaitOutcome::Ready)
        }
    }

    struct SpliceConfig {
        state: Arc<SpliceState>,
    }

    impl Default for SpliceConfig {
        fn default() -> Self {
            Self {
                state: Arc::new(SpliceState::new(vec![
                    build_variant_layout("slq", Consts::SLQ_VARIANT),
                    build_variant_layout("smq", Consts::SMQ_VARIANT),
                ])),
            }
        }
    }

    struct SpliceStream;

    impl StreamType for SpliceStream {
        type Config = SpliceConfig;
        type Events = ();
        type Source = SpliceSource;

        async fn create(config: Self::Config) -> Result<Self::Source, SourceError> {
            Ok(SpliceSource::new(config.state))
        }
    }

    struct TestWake;

    impl WorkerWake for TestWake {
        fn wake(&self) {}
    }

    fn asset_bytes(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|err| panic!("read {path:?}: {err}"))
    }

    fn build_variant_layout(label: &str, variant_index: usize) -> VariantLayout {
        let init = asset_bytes(&format!("init-{label}-a1.mp4"));
        let init_len = u64::try_from(init.len()).expect("init length fits u64");
        let mut blob = init;
        let mut byte_cursor = init_len;
        let mut segments = Vec::new();
        for segment_number in 1..=Consts::TOTAL_SEGMENTS {
            let bytes = asset_bytes(&format!("segment-{segment_number}-{label}-a1.m4s"));
            let start = byte_cursor;
            let end = start + u64::try_from(bytes.len()).expect("segment length fits u64");
            let segment_index = u32::try_from(segment_number - 1).expect("segment index fits u32");
            segments.push(SegmentDescriptor::new(
                start..end,
                Duration::from_secs(u64::from(segment_index) * Consts::SEGMENT_DURATION_SECS),
                Duration::from_secs(Consts::SEGMENT_DURATION_SECS),
                segment_index,
                variant_index,
            ));
            blob.extend_from_slice(&bytes);
            byte_cursor = end;
        }
        VariantLayout {
            blob,
            init_range: 0..init_len,
            segments,
        }
    }

    fn media_info(variant: usize) -> MediaInfo {
        let mut info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        info.variant_index = Some(u32::try_from(variant).expect("variant fits u32"));
        info
    }

    fn decoder_backend() -> kithara_decode::DecoderBackend {
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        {
            kithara_decode::DecoderBackend::Apple
        }
        #[cfg(not(all(feature = "apple", any(target_os = "macos", target_os = "ios"))))]
        {
            kithara_decode::DecoderBackend::Symphonia
        }
    }

    fn decoder_config<T: StreamType>(
        stream: &SharedStream<T>,
        backend: kithara_decode::DecoderBackend,
        byte_len: Arc<AtomicU64>,
    ) -> DecoderConfig<kithara_resampler::NoResamplerBackend> {
        byte_len.store(stream.len().unwrap_or(0), Ordering::Release);
        DecoderConfig::builder()
            .backend(backend)
            .byte_len_handle(byte_len)
            .maybe_byte_map(stream.byte_map())
            .gapless(false)
            .build()
    }

    struct SpliceFixture {
        source: StreamAudioSource<SpliceStream>,
        state: Arc<SpliceState>,
    }

    async fn splice_source(variants: Vec<VariantLayout>) -> SpliceFixture {
        let state = Arc::new(SpliceState::new(variants));
        let stream = Stream::<SpliceStream>::new(SpliceConfig {
            state: state.clone(),
        })
        .await
        .expect("create in-memory splice stream");
        let shared_stream = SharedStream::new(stream);
        let backend = decoder_backend();
        let initial_byte_len = Arc::new(AtomicU64::new(0));
        let initial_decoder = DecodeFactory::create_from_media_info(
            shared_stream.clone(),
            &media_info(Consts::SLQ_VARIANT),
            decoder_config(&shared_stream, backend, initial_byte_len),
        )
        .expect("create initial slq fMP4 decoder");
        let initial_spec = initial_decoder.spec();
        let host_sample_rate = Arc::new(AtomicU32::new(Consts::SAMPLE_RATE));
        let pcm_pool = PcmPool::default().clone();
        let effects =
            crate::pipeline::config::create_effects(initial_spec, None, &pcm_pool, Vec::new());
        let factory_byte_len = Arc::new(AtomicU64::new(0));
        let decoder_factory: DecoderFactory<SpliceStream> =
            Arc::new(move |stream, info, base_offset| {
                let byte_len = stream
                    .len()
                    .map_or(0, |len| len.saturating_sub(base_offset));
                factory_byte_len.store(byte_len, Ordering::Release);
                let config: DecoderConfig<kithara_resampler::NoResamplerBackend> =
                    DecoderConfig::builder()
                        .backend(backend)
                        .byte_len_handle(factory_byte_len.clone())
                        .maybe_byte_map(stream.byte_map())
                        .gapless(false)
                        .build();
                let input = OffsetReader::new(stream, base_offset);
                let decoder = DecodeFactory::create_from_media_info(input, &info, config)?;
                decoder.update_byte_len(byte_len);
                Ok(decoder)
            });
        let decode = DecodeInit {
            decoder: initial_decoder,
            decoder_factory,
            gapless_mode: GaplessMode::Disabled,
            host_sample_rate,
            media_info: Some(media_info(Consts::SLQ_VARIANT)),
            recreate_on_host_rate_change: false,
        }
        .into_parts(effects, shared_stream.seek_observe().epoch());
        let parts = SourceParts::new(
            &shared_stream,
            decode,
            Arc::new(AtomicU64::new(0)),
            RebuildRuntime {
                handle: RuntimeHandle::try_current().expect("test requires tokio runtime"),
                wake: Arc::new(TestWake),
            },
        );
        SpliceFixture {
            source: StreamAudioSource::new(shared_stream, parts),
            state,
        }
    }

    fn run_pending_rebuild_inline(source: &mut StreamAudioSource<SpliceStream>) {
        source.rebuild.run_inline();
    }

    fn append_left_channel(left: &mut Vec<f32>, chunk: &PcmChunk) {
        let channels = usize::from(chunk.meta.spec.channels);
        assert_eq!(channels, Consts::CHANNELS, "AAC fixture should be stereo");
        for frame in 0..chunk.frames() {
            left.push(chunk.samples[frame * channels]);
        }
    }

    fn peak_first_diff(left: &[f32], center: usize, half: usize) -> f32 {
        assert!(
            (1..left.len()).contains(&center),
            "first-difference center must be in 1..{}, got {center}",
            left.len(),
        );
        let start = center.saturating_sub(half).max(1);
        let end = center.saturating_add(half).min(left.len() - 1);
        let mut peak = 0.0_f32;
        for i in start..=end {
            let diff = (left[i] - left[i - 1]).abs();
            peak = peak.max(diff);
        }
        peak
    }

    fn segment_boundary_frame(segment: usize) -> usize {
        let seconds = u64::try_from(segment)
            .unwrap_or(u64::MAX)
            .saturating_mul(Consts::SEGMENT_DURATION_SECS);
        let frames = seconds.saturating_mul(u64::from(Consts::SAMPLE_RATE));
        usize::try_from(frames).unwrap_or(usize::MAX)
    }

    // splice-continuity contract: RED = audible click on variant switch
    // (see .docs/plans/2026-07-03-resampler-native-src-design.md, S-Click)
    #[kithara::test(tokio)]
    async fn hls_aac_lc_abr_variant_switch_splice_continuity_metric() {
        let SpliceFixture { mut source, state } = splice_source(vec![
            build_variant_layout("slq", Consts::SLQ_VARIANT),
            build_variant_layout("smq", Consts::SMQ_VARIANT),
        ])
        .await;
        let splice_time =
            Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
        let capture_frames = usize::try_from(
            Consts::CAPTURE_END_SEGMENT
                .saturating_mul(Consts::SEGMENT_DURATION_SECS)
                .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
        )
        .expect("capture frame count fits usize");
        let mut left = Vec::with_capacity(capture_frames);
        let mut switched = false;
        let mut splice_frame = None;

        while left.len() < capture_frames {
            run_pending_rebuild_inline(&mut source);
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = fetch.into_inner();
                    append_left_channel(&mut left, &chunk);
                    source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                    if !switched
                        && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                        && chunk.meta.end_timestamp >= splice_time
                    {
                        state.switch_to(Consts::SMQ_VARIANT);
                        switched = true;
                        splice_frame = Some(left.len());
                    }
                }
                TrackStep::StateChanged | TrackStep::Blocked(_) => {}
                TrackStep::Eof => break,
                TrackStep::Failed => panic!("splice source failed before metric collection"),
            }
        }

        assert!(switched, "test must trigger the slq -> smq splice");
        let splice_frame = splice_frame.expect("splice frame should be captured");
        if let Some(landing) = state.warmup_landing() {
            println!(
                "SPLICE_WARMUP variant={} segment={}",
                landing.variant_index, landing.segment_index
            );
        }
        let switch_peak = peak_first_diff(&left, splice_frame, 64);
        let mut control_peak = 0.0_f32;
        let mut control_count = 0usize;
        for k in 1..Consts::TOTAL_SEGMENTS {
            let boundary = segment_boundary_frame(k);
            if boundary >= left.len() || boundary.abs_diff(splice_frame) <= 4096 {
                continue;
            }
            control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
            control_count += 1;
        }
        assert!(
            control_count >= 2,
            "splice continuity metric needs at least two same-variant control boundaries, got {control_count}",
        );
        let ratio = switch_peak / control_peak.max(f32::EPSILON);
        println!(
            "SPLICE_CONTINUITY switch_peak={switch_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 3.0,
            "variant-switch stitch discontinuity {switch_peak:.6} is {ratio:.1}x the worst same-variant segment boundary {control_peak:.6} — audible click at the splice",
        );
    }

    #[kithara::test(tokio)]
    async fn hls_aac_lc_same_variant_recreate_continuity_metric() {
        let SpliceFixture { mut source, state } =
            splice_source(vec![build_variant_layout("slq", Consts::SLQ_VARIANT)]).await;
        let recreate_after =
            Duration::from_secs(u64::from(Consts::SPLICE_SEGMENT) * Consts::SEGMENT_DURATION_SECS);
        let capture_frames = usize::try_from(
            Consts::CAPTURE_END_SEGMENT
                .saturating_mul(Consts::SEGMENT_DURATION_SECS)
                .saturating_mul(u64::from(Consts::SAMPLE_RATE)),
        )
        .expect("capture frame count fits usize");
        let mut left = Vec::with_capacity(capture_frames);
        let mut recreated = false;
        let mut recreate_frame = None;

        while left.len() < capture_frames {
            run_pending_rebuild_inline(&mut source);
            match source.step_track() {
                TrackStep::Produced(fetch) => {
                    let chunk = fetch.into_inner();
                    append_left_channel(&mut left, &chunk);
                    source.playhead.advance(&ChunkPosition::from(&chunk.meta));
                    if !recreated
                        && chunk.meta.segment_index == Some(Consts::SPLICE_SEGMENT - 1)
                        && chunk.meta.end_timestamp >= recreate_after
                    {
                        let active = state.active_index();
                        assert_eq!(
                            active,
                            Consts::SLQ_VARIANT,
                            "same-variant recreate test must stay on the SLQ variant",
                        );
                        source.start_recreating_decoder(RecreateState {
                            cause: RecreateCause::VariantSwitch,
                            media_info: media_info(active),
                            next: RecreateNext::ApplySeek(SeekRequest {
                                seek: SeekContext {
                                    epoch: source.seek_engine.epoch(),
                                    target: chunk.meta.end_timestamp,
                                },
                                emit_request: false,
                            }),
                            offset: state.active_layout().init_range.start,
                        });
                        recreated = true;
                        recreate_frame = Some(left.len());
                    }
                }
                TrackStep::StateChanged | TrackStep::Blocked(_) => {}
                TrackStep::Eof => break,
                TrackStep::Failed => {
                    panic!("same-variant recreate source failed before metric collection");
                }
            }
        }

        assert!(recreated, "test must trigger the same-variant recreate");
        assert_eq!(
            state.active_index(),
            Consts::SLQ_VARIANT,
            "same-variant recreate must not change the active variant",
        );
        let recreate_frame = recreate_frame.expect("recreate frame should be captured");
        let recreate_peak = peak_first_diff(&left, recreate_frame, 64);
        let mut control_peak = 0.0_f32;
        let mut control_count = 0usize;
        for k in 1..Consts::TOTAL_SEGMENTS {
            let boundary = segment_boundary_frame(k);
            if boundary >= left.len() || boundary.abs_diff(recreate_frame) <= 4096 {
                continue;
            }
            control_peak = control_peak.max(peak_first_diff(&left, boundary, 64));
            control_count += 1;
        }
        assert!(
            control_count >= 2,
            "recreate continuity metric needs at least two same-content control boundaries, got {control_count}",
        );
        let ratio = recreate_peak / control_peak.max(f32::EPSILON);
        println!(
            "RECREATE_CONTINUITY recreate_peak={recreate_peak:.6} control_peak={control_peak:.6} ratio={ratio:.3}"
        );
        assert!(
            ratio < 3.0,
            "same-variant recreate discontinuity {recreate_peak:.6} is {ratio:.1}x the worst same-content segment boundary {control_peak:.6}",
        );
    }
}

#[cfg(test)]
mod playing_flag_tests {
    use crossbeam_queue::ArrayQueue;
    use kithara_decode::DecodeError;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::MediaInfo;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::pipeline::track_fsm::{
        ApplySeekState, RebuildState, RecreateCause, RecreateNext, RecreateState, ResumeState,
        SeekContext, SeekMode, SeekRequest, TrackFailure, WaitContext, WaitingReason,
    };

    fn seek_ctx() -> SeekContext {
        SeekContext {
            epoch: 1,
            target: Duration::from_secs(5),
        }
    }

    fn seek_req() -> SeekRequest {
        SeekRequest {
            seek: seek_ctx(),
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
    fn playing_for_state_active_states_are_true() {
        assert!(playing_for_state(&CurrentFsm::decoding()));
        assert!(playing_for_state(&CurrentFsm::seek_requested(seek_req())));
        assert!(playing_for_state(&CurrentFsm::applying_seek(
            ApplySeekState {
                mode: SeekMode::Direct { target_byte: None },
                request: seek_req(),
            }
        )));
        assert!(playing_for_state(&CurrentFsm::waiting(
            WaitContext::Playback,
            WaitingReason::Waiting,
        )));
        assert!(playing_for_state(&CurrentFsm::recreating(RecreateState {
            cause: RecreateCause::FormatBoundary,
            media_info: MediaInfo::default(),
            next: RecreateNext::Decode,
            offset: 0,
        })));
        assert!(playing_for_state(&CurrentFsm::rebuilding(rebuild_state())));
        assert!(playing_for_state(&CurrentFsm::awaiting_resume(
            ResumeState {
                seek: seek_ctx(),
                ..Default::default()
            }
        )));
    }

    #[kithara::test]
    fn playing_for_state_terminal_states_are_false() {
        assert!(!playing_for_state(&CurrentFsm::at_eof()));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::SourceCancelled
        )));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::RecreateFailed { offset: 0 }
        )));
        assert!(!playing_for_state(&CurrentFsm::failed(
            TrackFailure::Decode(DecodeError::Interrupted)
        )));
    }

    #[kithara::test]
    fn playing_matrix_covers_every_transition_endpoint() {
        let transitions: &[(CurrentFsm, bool)] = &[
            (CurrentFsm::decoding(), true),
            (CurrentFsm::seek_requested(seek_req()), true),
            (
                CurrentFsm::applying_seek(ApplySeekState {
                    mode: SeekMode::Direct { target_byte: None },
                    request: seek_req(),
                }),
                true,
            ),
            (
                CurrentFsm::waiting(WaitContext::Playback, WaitingReason::Waiting),
                true,
            ),
            (
                CurrentFsm::recreating(RecreateState {
                    cause: RecreateCause::VariantSwitch,
                    media_info: MediaInfo::default(),
                    next: RecreateNext::Decode,
                    offset: 0,
                }),
                true,
            ),
            (CurrentFsm::rebuilding(rebuild_state()), true),
            (
                CurrentFsm::awaiting_resume(ResumeState {
                    seek: seek_ctx(),
                    ..Default::default()
                }),
                true,
            ),
            (CurrentFsm::at_eof(), false),
            (CurrentFsm::failed(TrackFailure::SourceCancelled), false),
        ];
        for (state, expected) in transitions {
            assert_eq!(playing_for_state(state), *expected);
        }
    }

    #[kithara::test]
    fn no_spurious_flip_across_100_decoding_transitions() {
        for _ in 0..100 {
            assert!(
                playing_for_state(&CurrentFsm::decoding()),
                "PLAYING must stay true across a long Decoding → Decoding loop"
            );
        }
    }
}

#[cfg(test)]
mod resolve_format_change_target_tests {
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_utils::kithara;

    use crate::pipeline::decode::format::resolve_target;

    fn info(
        codec: Option<AudioCodec>,
        container: Option<ContainerFormat>,
        variant: Option<u32>,
    ) -> MediaInfo {
        let mut info = MediaInfo::new(codec, container);
        info.variant_index = variant;
        info
    }

    #[kithara::test]
    fn no_change_when_variant_index_matches() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn same_codec_fmp4_variant_change_recreates_boundary() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target = resolve_target(Some(&cached), &current)
            .expect("same-codec fMP4 variant change must re-prime the demuxer");
        assert_eq!(target.variant_index, Some(1));
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn same_codec_wav_variant_change_is_byte_continuity() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(1));
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn variant_change_keeps_cached_codec_and_container_when_current_disagrees() {
        let cached = info(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav), Some(0));
        let current = info(None, Some(ContainerFormat::Fmp4), Some(1));
        let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Pcm));
        assert_eq!(target.container, Some(ContainerFormat::Wav));
        assert_eq!(target.variant_index, Some(1));
    }

    #[kithara::test]
    fn variant_change_falls_back_to_current_when_cached_lacks_codec_or_container() {
        let cached = info(None, None, Some(0));
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(2),
        );
        let target = resolve_target(Some(&cached), &current).expect("variant change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::AacLc));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
        assert_eq!(target.variant_index, Some(2));
    }

    #[kithara::test]
    fn no_cached_uses_current_directly() {
        let current = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(1),
        );
        let target =
            resolve_target(None, &current).expect("None cached + Some(variant) must trigger");
        assert_eq!(target, current);
    }

    #[kithara::test]
    fn explicit_codec_change_takes_current_codec() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::Flac), Some(ContainerFormat::Fmp4), None);
        let target = resolve_target(Some(&cached), &current).expect("codec change must trigger");
        assert_eq!(target.codec, Some(AudioCodec::Flac));
        assert_eq!(target.container, Some(ContainerFormat::Fmp4));
    }

    #[kithara::test]
    fn current_codec_none_is_not_a_codec_change() {
        let cached = info(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Fmp4),
            Some(0),
        );
        let current = info(None, Some(ContainerFormat::Fmp4), Some(0));
        assert!(resolve_target(Some(&cached), &current).is_none());
    }

    #[kithara::test]
    fn no_change_when_neither_side_has_variant() {
        let cached = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        let current = info(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4), None);
        assert!(resolve_target(Some(&cached), &current).is_none());
    }
}
