use std::num::NonZeroU32;

use kithara_decode::{DecodeError, DecoderSeekOutcome, PcmOrigin};
use kithara_events::DeferredBus;
use kithara_platform::{
    sync::{Arc, mpsc},
    time::Duration,
    tokio::{runtime::Handle, task::spawn_blocking_on},
};
use kithara_signal::AudioChunk;
use kithara_stream::{
    OpenedVariantReader, OutgoingDisposition, PendingReason, PlayheadWrite, SourcePhase,
    StreamType, VariantControl, VariantTransition, WorkerWake,
};
use tracing::warn;

use super::change::{SourceChange, SourceCommand};
use crate::{
    AudioEvent, AudioLaneEvent, AudioSource, DecoderChangeCause, SeekOutcome, TrackFailureKind,
    TrackStep,
    pipeline::{
        decode::{
            core::{ActiveDecode, DecodeAction, DecodeCtx, DecoderFactory},
            event::{GenerationInstalled, enqueue_generation_installed},
            resume::ResumeCursor,
            step::tick,
            transition::OutgoingFrontier,
            transition_promotion::preparation::{
                IncomingPreparationOwner, IncomingPreparationRequest,
            },
        },
        rebuild::RecreateState,
        seek::ResumeTarget,
        stream::shared::SharedStream,
    },
};

pub(in crate::pipeline::source) enum OwnerPhase {
    Decoding,
    Changing(SourceChange),
    Seeking {
        target: ResumeTarget,
        reason: PendingReason,
    },
    AtEof,
    Failed {
        failure: TrackFailureKind,
        error: Option<DecodeError>,
    },
}

pub(in crate::pipeline::source) fn write_failure_diagnostic(
    failure: TrackFailureKind,
    error: Option<&DecodeError>,
) {
    let message = match failure {
        TrackFailureKind::Decode { .. } => "track failed: decode error",
        TrackFailureKind::RecreateFailed { .. } => "track failed: decoder recreation failed",
        TrackFailureKind::SourceCancelled => "track failed: source cancelled",
        TrackFailureKind::ChannelClosed => "track failed: channel closed",
        TrackFailureKind::Render => "track failed: render error",
    };
    warn!(?failure, err = ?error, "{message}");
}

pub(crate) struct StreamAudioSource<T: StreamType> {
    pub(in crate::pipeline::source) decode: ActiveDecode,
    pub(in crate::pipeline::source) factory: DecoderFactory,
    pub(in crate::pipeline::source) runtime: Handle,
    pub(in crate::pipeline::source) host_rate: Option<NonZeroU32>,
    pub(in crate::pipeline::source) decoder_backend: kithara_decode::DecoderBackend,
    pub(in crate::pipeline::source) playback_resampler_backend: &'static str,
    pub(in crate::pipeline::source) playhead: Arc<dyn PlayheadWrite>,
    pub(in crate::pipeline::source) emit: Arc<DeferredBus<AudioLaneEvent>>,
    pub(in crate::pipeline::source) variant_control: Option<Arc<dyn VariantControl>>,
    pub(in crate::pipeline::source) phase: OwnerPhase,
    pub(in crate::pipeline::source) waiting_for_source: bool,
    pub(in crate::pipeline::source) seek_abr: Option<kithara_abr::AbrHandle>,
    pub(in crate::pipeline::source) failure_logged: bool,
    pub(in crate::pipeline::source) resume: ResumeCursor,
    duration_reconciled: bool,
    pub(in crate::pipeline::source) shared_stream: SharedStream<T>,
    pub(in crate::pipeline::source) wake: Arc<dyn WorkerWake>,
}

pub(crate) struct SourceDecoderConfig {
    pub(crate) factory: DecoderFactory,
    pub(crate) runtime: Handle,
    pub(crate) host_rate: Option<NonZeroU32>,
    pub(crate) backend: kithara_decode::DecoderBackend,
    pub(crate) playback_resampler_backend: &'static str,
}

pub(in crate::pipeline::source) fn promotion_frontier_for(
    transition: VariantTransition,
    frontier: OutgoingFrontier,
) -> OutgoingFrontier {
    if transition.outgoing_disposition() == OutgoingDisposition::Abandoned
        && frontier == OutgoingFrontier::Awaiting
    {
        OutgoingFrontier::Unavailable
    } else {
        frontier
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    fn apply_decode_action(&mut self, action: DecodeAction) -> TrackStep<AudioChunk> {
        match action {
            DecodeAction::Produced(fetch) => {
                self.refresh_visible_duration();
                TrackStep::Produced(*fetch)
            }
            DecodeAction::Progress => {
                self.wake.wake();
                TrackStep::StateChanged
            }
            DecodeAction::Pending(reason) => {
                self.waiting_for_source = true;
                TrackStep::Blocked(reason)
            }
            DecodeAction::TransitionPending => TrackStep::Blocked(self.transition_wait_reason()),
            DecodeAction::StartRecreate(recreate) => {
                let offset = recreate.offset;
                match self.install_replacement(
                    recreate,
                    self.resume.source_end().map(ResumeTarget::Source),
                ) {
                    Ok(_) => {
                        self.wake.wake();
                        TrackStep::StateChanged
                    }
                    Err(error) => {
                        let failure = TrackFailureKind::RecreateFailed { offset };
                        TrackStep::Failed(self.fail(failure, Some(error)))
                    }
                }
            }
            DecodeAction::ReaderRetired => {
                let target = self.resume_target();
                match self.replace_retired_reader(target) {
                    Ok(_) => {
                        self.wake.wake();
                        TrackStep::StateChanged
                    }
                    Err((failure, error)) => TrackStep::Failed(self.fail(failure, Some(error))),
                }
            }
            DecodeAction::Eof => self.finish_decoding(),
            DecodeAction::Failed(error) => {
                let failure = TrackFailureKind::Decode {
                    kind: crate::map_decode_error_kind(&error),
                };
                TrackStep::Failed(self.fail(failure, Some(error)))
            }
        }
    }

    fn finish_decoding(&mut self) -> TrackStep<AudioChunk> {
        if let Some(duration) = self
                    .decode
                    .active()
                    .visible_origin()
                    .zip(
                        self.resume
                            .decoded_end(self.decode.discontinuity().revision()),
                    )
                    .and_then(|(origin, end)| {
                        let duration = end.checked_sub(origin)?;
                        tracing::debug!(
                            source_revision = self.decode.discontinuity().revision(),
                            visible_origin = ?origin,
                            raw_end = ?end,
                            visible_duration = ?duration,
                            decoder_origin = ?self.decode.active().decoder().initial_pcm_origin().map(PcmOrigin::position),
                            gapless_profile = ?self.decode.active().gapless_profile(),
                            "refined visible duration at natural EOF"
                        );
                        Some(duration)
                    })
                {
                    self.playhead.set_duration(Some(duration));
                    self.duration_reconciled = true;
                }
        self.release_seek_abr();
        if !matches!(self.phase, OwnerPhase::Failed { .. }) {
            self.abandon_incoming();
            self.phase = OwnerPhase::AtEof;
        }
        self.emit.enqueue(AudioEvent::EndOfStream);
        TrackStep::Eof
    }
}

impl<T: StreamType> AudioSource for StreamAudioSource<T> {
    type Chunk = AudioChunk;
    fn commit_source_end(&mut self, end: crate::SourceEnd, meta: kithara_signal::AudioChunkInfo) {
        if meta.source_revision == self.decode.discontinuity().revision()
            && meta.visible_origin == self.decode.active().visible_origin()
            && meta.media_origin == self.decode.media_origin()
        {
            self.resume.commit_source_end(end);
        }
        self.release_seek_abr();
    }
    delegate::delegate! {
        to self.decode.active() {
            fn native_pcm_basis(&self) -> Option<kithara_signal::AudioChunkInfo>;
            fn visible_origin(&self) -> Option<Duration>;
        }
        to self.decode {
            fn media_origin(&self) -> Option<Duration>;
        }
    }

    fn media_end(&self) -> Option<Duration> {
        let decoded_end = if matches!(self.phase, OwnerPhase::AtEof) {
            self.resume
                .decoded_end(self.decode.discontinuity().revision())
        } else {
            None
        };
        self.decode.active().media_end(
            self.decode.gapless_mode(),
            self.playhead.duration(),
            decoded_end,
        )
    }
    fn discontinuity(&self) -> Option<crate::SourceDiscontinuity> {
        Some(self.decode.discontinuity())
    }
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, crate::AudioReadError> {
        self.seek_selected(position, false)
    }
    fn seek_for_render(
        &mut self,
        position: Duration,
    ) -> Result<SeekOutcome, crate::AudioReadError> {
        self.seek_selected(position, true)
    }
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        self.host_rate
    }
    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        if matches!(self.phase, OwnerPhase::Failed { .. }) || self.host_rate == Some(rate) {
            return;
        }
        if self.shared_stream.phase() == SourcePhase::Cancelled {
            self.fail(TrackFailureKind::SourceCancelled, None);
            return;
        }
        let initial_binding = self.host_rate.is_none();
        self.abandon_incoming();
        self.host_rate = Some(rate);
        if initial_binding && self.decode.output_spec().sample_rate == rate {
            return;
        }
        let landing = self.resume_target();
        let apply_seek = matches!(self.phase, OwnerPhase::Seeking { .. })
            || matches!(&self.phase, OwnerPhase::Changing(change) if change.command.applies_seek());
        if let Err(error) = self.request_source_change(SourceCommand::Rate {
            target: landing,
            apply_seek,
        }) {
            let failure = if self.shared_stream.phase() == SourcePhase::Cancelled {
                TrackFailureKind::SourceCancelled
            } else {
                TrackFailureKind::RecreateFailed {
                    offset: self.decode.active().base_offset(),
                }
            };
            self.fail(failure, Some(error));
        }
    }

    fn finish_deferred(&mut self) {
        self.finish_failure_diagnostic();
        if let Some(wake) = self.shared_stream.peer_wake() {
            wake.flush();
        }
        self.emit.flush();
    }
    fn prepare_deferred(&mut self) -> Option<kithara_signal::AudioSpec> {
        if matches!(
            self.phase,
            OwnerPhase::Decoding | OwnerPhase::Seeking { .. } | OwnerPhase::Changing(_)
        ) && self.shared_stream.phase() == SourcePhase::Cancelled
        {
            self.fail(TrackFailureKind::SourceCancelled, None);
        }
        self.progress_variant_transition();
        if matches!(
            self.phase,
            OwnerPhase::Decoding | OwnerPhase::Seeking { .. } | OwnerPhase::Changing(_)
        ) && let Err((failure, error)) = self.prepare_active()
        {
            self.fail(failure, Some(error));
        }
        Some(self.decode.output_spec())
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        if matches!(
            self.phase,
            OwnerPhase::Decoding | OwnerPhase::Seeking { .. } | OwnerPhase::Changing(_)
        ) && self.shared_stream.phase() == SourcePhase::Cancelled
        {
            return TrackStep::Failed(self.fail(TrackFailureKind::SourceCancelled, None));
        }
        match &mut self.phase {
            OwnerPhase::AtEof => return TrackStep::Eof,
            OwnerPhase::Failed { failure, .. } => return TrackStep::Failed(*failure),
            OwnerPhase::Changing(_) => return TrackStep::Blocked(self.pending_wait_reason()),
            OwnerPhase::Decoding => {}
            OwnerPhase::Seeking { reason, .. } => {
                return if *reason == PendingReason::Retry {
                    self.wake.wake();
                    TrackStep::StateChanged
                } else {
                    TrackStep::Blocked(self.pending_wait_reason())
                };
            }
        }
        if let Some(reason) = self.source_wait_reason() {
            return TrackStep::Blocked(reason);
        }
        self.waiting_for_source = false;
        self.refresh_visible_duration();
        let action = tick(
            &mut self.decode,
            DecodeCtx {
                cursor: &mut self.resume,
                physical_duration: self.playhead.duration(),
                stream: &self.shared_stream,
                emit: Some(&self.emit),
            },
        );
        self.apply_decode_action(action)
    }
    fn warm_up(&mut self) {
        let _ = self.shared_stream.len();
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
        self.cancel_source_change();
        self.release_seek_abr();
        self.abandon_incoming();
        self.finish_deferred();
    }
}

impl<T: StreamType> StreamAudioSource<T> {
    pub(crate) fn new(
        shared_stream: SharedStream<T>,
        decode: ActiveDecode,
        config: SourceDecoderConfig,
        emit: Arc<DeferredBus<AudioLaneEvent>>,
        wake: Arc<dyn WorkerWake>,
    ) -> Self {
        let playhead = shared_stream.playhead_write();
        let variant_control = shared_stream.variant_control();
        Self {
            shared_stream,
            wake,
            decode,
            factory: config.factory,
            runtime: config.runtime,
            host_rate: config.host_rate,
            decoder_backend: config.backend,
            playback_resampler_backend: config.playback_resampler_backend,
            playhead,
            emit,
            variant_control,
            phase: OwnerPhase::Decoding,
            waiting_for_source: false,
            seek_abr: None,
            failure_logged: false,
            resume: ResumeCursor::default(),
            duration_reconciled: false,
        }
    }

    fn refresh_visible_duration(&self) {
        if !self.duration_reconciled {
            let active = self.decode.active();
            if let Some(duration) = active.visible_duration(self.decode.gapless_mode())
                && Some(duration) > self.playhead.duration()
            {
                self.playhead.set_duration(Some(duration));
            }
        }
    }

    fn source_wait_reason(&self) -> Option<crate::pipeline::track::WaitingReason> {
        use crate::pipeline::track::WaitingReason;

        let active = self.decode.active();
        if !self.waiting_for_source || active.has_output() || active.decoder().has_prepared_result()
        {
            return None;
        }
        match self.shared_stream.phase() {
            SourcePhase::Waiting => Some(WaitingReason::Waiting),
            SourcePhase::WaitingDemand => Some(WaitingReason::WaitingDemand),
            SourcePhase::WaitingMetadata => Some(WaitingReason::WaitingMetadata),
            _ => None,
        }
    }

    pub(super) fn pending_seek_target(&self) -> Option<ResumeTarget> {
        match &self.phase {
            OwnerPhase::Seeking { target, .. } => Some(*target),
            OwnerPhase::Changing(change) if change.command.applies_seek() => {
                change.command.target()
            }
            _ => None,
        }
    }

    pub(super) fn fail(
        &mut self,
        failure: TrackFailureKind,
        error: Option<DecodeError>,
    ) -> TrackFailureKind {
        if let OwnerPhase::Failed { failure, .. } = self.phase {
            return failure;
        }
        if let Some(target) = self.pending_seek_target() {
            match target {
                ResumeTarget::Position(target) | ResumeTarget::Render(target) => {
                    self.emit.enqueue(AudioEvent::SeekRejected { target });
                }
                ResumeTarget::Source(_) => {}
            }
        }
        self.cancel_source_change();
        self.abandon_incoming();
        self.release_seek_abr();
        self.phase = OwnerPhase::Failed { failure, error };
        self.emit.enqueue(AudioEvent::TrackFailed { failure });
        failure
    }

    pub(super) fn finish_failure_diagnostic(&mut self) {
        if self.failure_logged {
            return;
        }
        let OwnerPhase::Failed { failure, error } = &mut self.phase else {
            return;
        };
        write_failure_diagnostic(*failure, error.take().as_ref());
        self.failure_logged = true;
    }

    pub(super) fn discard_local_incoming(&mut self) {
        drop(self.decode.discard_incoming());
    }

    pub(super) fn abort_local_incoming(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
    ) {
        let _ = control.abort_variant(transition);
        self.discard_local_incoming();
    }

    pub(super) fn abandon_incoming(&mut self) {
        if let Some(transition) = self.decode.incoming_transition()
            && let Some(control) = self.variant_control.clone()
        {
            self.abort_local_incoming(control.as_ref(), transition);
        } else {
            self.discard_local_incoming();
        }
    }

    pub(super) fn start_incoming_build(
        &mut self,
        control: &dyn VariantControl,
        transition: VariantTransition,
        reader: OpenedVariantReader,
    ) {
        let (plan, reader) = reader.split();
        reader.control().set_worker_wake(self.wake.clone());
        let landing = Some(ResumeTarget::Position(plan.landing_time()));
        let info = Some(plan.media_info().clone());
        let frontier = self
            .resume
            .decode_head()
            .map_or(OutgoingFrontier::Awaiting, |(frame, rate)| {
                OutgoingFrontier::Exact { frame, rate }
            });
        let Some(target) = self
            .decode
            .incoming_preparation(promotion_frontier_for(transition, frontier))
        else {
            self.abort_local_incoming(control, transition);
            return;
        };
        let (completed, completion) = mpsc::channel();
        let (requests, requested) = mpsc::channel::<IncomingPreparationRequest>();
        let preparation = IncomingPreparationOwner {
            requests,
            completion,
        };
        if !self.decode.begin_incoming_build(transition, preparation) {
            let _ = control.abort_variant(transition);
            return;
        }
        let factory = self.factory.clone();
        let host_rate = self.host_rate;
        let gapless_mode = self.decode.gapless_mode();
        let wake = self.wake.clone();
        drop(spawn_blocking_on(&self.runtime, move || {
            let result = factory
                .build_generation(reader, info, 0, landing, host_rate, gapless_mode)
                .map(|(generation, _)| IncomingPreparationRequest { generation, target }.prepare());
            if completed.send(result).is_err() {
                return;
            }
            wake.wake();
            while let Ok(request) = requested.recv() {
                if completed.send(Ok(request.prepare())).is_err() {
                    return;
                }
                wake.wake();
            }
        }));
    }

    pub(super) fn install_replacement(
        &mut self,
        recreate: RecreateState,
        landing: Option<ResumeTarget>,
    ) -> Result<Option<DecoderSeekOutcome>, DecodeError> {
        self.decode.seal_visible_origin();
        self.abandon_incoming();
        let apply_seek = matches!(self.phase, OwnerPhase::Seeking { .. });
        self.request_source_change(SourceCommand::Recreate {
            recreate,
            landing,
            apply_seek,
        })?;
        Ok(landing.map(|_| DecoderSeekOutcome::Pending(PendingReason::Retry)))
    }

    pub(super) fn publish_generation(&self, cause: DecoderChangeCause) {
        enqueue_generation_installed(
            &self.emit,
            &GenerationInstalled {
                backend: self.decoder_backend,
                cause,
                generation: self.decode.active(),
                gapless_mode: self.decode.gapless_mode(),
                host_sample_rate: self.host_rate.map_or(0, NonZeroU32::get),
                playback_resampler_backend: self.playback_resampler_backend,
                recreates_on_route: true,
            },
        );
    }

    fn pending_wait_reason(&self) -> crate::pipeline::track::WaitingReason {
        use crate::pipeline::track::WaitingReason;
        match self.shared_stream.phase() {
            SourcePhase::WaitingDemand => WaitingReason::WaitingDemand,
            SourcePhase::WaitingMetadata => WaitingReason::WaitingMetadata,
            _ => WaitingReason::Waiting,
        }
    }
}
