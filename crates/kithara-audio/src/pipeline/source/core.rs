use std::{
    io::SeekFrom,
    num::NonZeroU32,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_decode::{DecodeError, DecoderSeekOutcome};
use kithara_events::DeferredBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioChunk;
use kithara_stream::{
    MediaInfo, OpenedReader, OpenedVariantReader, OutgoingDisposition, PlayheadWrite, StreamType,
    VariantControl, VariantTransition, WorkerWake,
};
use tracing::warn;

use crate::{
    AudioEvent, AudioLaneEvent, AudioSource, DecoderChangeCause, SeekOutcome, TrackFailureKind,
    TrackStep,
    pipeline::{
        decode::{
            DecoderGeneration,
            core::{ActiveDecode, DecodeAction, DecodeCtx, DecoderFactory, panic_message},
            event::{GenerationInstalled, enqueue_generation_installed},
            format::{FormatDecision, detect},
            resume::ResumeCursor,
            step::tick,
            transition::OutgoingFrontier,
        },
        rebuild::{RecreateCause, RecreateState},
        seek::{ResumeTarget, emit::commit_outcome},
        stream::shared::SharedStream,
    },
};

pub(in crate::pipeline::source) enum OwnerPhase {
    Decoding,
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
    pub(in crate::pipeline::source) host_rate: Option<NonZeroU32>,
    pub(in crate::pipeline::source) decoder_backend: kithara_decode::DecoderBackend,
    pub(in crate::pipeline::source) playback_resampler_backend: &'static str,
    pub(in crate::pipeline::source) playhead: Arc<dyn PlayheadWrite>,
    pub(in crate::pipeline::source) emit: Arc<DeferredBus<AudioLaneEvent>>,
    pub(in crate::pipeline::source) variant_control: Option<Arc<dyn VariantControl>>,
    pub(in crate::pipeline::source) phase: OwnerPhase,
    pub(in crate::pipeline::source) failure_logged: bool,
    pub(in crate::pipeline::source) resume: ResumeCursor,
    pub(in crate::pipeline::source) shared_stream: SharedStream<T>,
    pub(in crate::pipeline::source) wake: Arc<dyn WorkerWake>,
}

pub(crate) struct SourceDecoderConfig {
    pub(crate) factory: DecoderFactory,
    pub(crate) host_rate: Option<NonZeroU32>,
    pub(crate) backend: kithara_decode::DecoderBackend,
    pub(crate) playback_resampler_backend: &'static str,
}

pub(in crate::pipeline::source) fn promotion_frontier_for(
    transition: VariantTransition,
    frontier: OutgoingFrontier,
) -> OutgoingFrontier {
    if transition.outgoing_disposition() == OutgoingDisposition::Abandoned {
        OutgoingFrontier::Unavailable
    } else {
        frontier
    }
}
pub(in crate::pipeline::source) fn initial_promotion_frontier(
    transition: VariantTransition,
) -> OutgoingFrontier {
    if transition.outgoing_disposition() == OutgoingDisposition::Abandoned {
        OutgoingFrontier::Unavailable
    } else {
        OutgoingFrontier::Awaiting
    }
}

impl<T: StreamType> AudioSource for StreamAudioSource<T> {
    type Chunk = AudioChunk;
    fn commit_source_end(&mut self, end: crate::SourceEnd, _meta: kithara_signal::AudioChunkInfo) {
        self.resume.commit_source_end(end);
    }
    fn discontinuity(&self) -> Option<crate::SourceDiscontinuity> {
        Some(self.decode.discontinuity())
    }
    fn seek(&mut self, position: Duration) -> Result<SeekOutcome, crate::AudioReadError> {
        if self.shared_stream.phase() == kithara_stream::SourcePhase::Cancelled {
            self.fail(TrackFailureKind::SourceCancelled, None);
        }
        if let OwnerPhase::Failed { failure, .. } = self.phase {
            return Err(crate::AudioReadError::Stream {
                what: "seek decoded source",
                source: crate::FailureSource::ProducerAfterSeek { failure },
            });
        }
        self.emit.enqueue(AudioEvent::SeekLifecycle {
            stage: crate::SeekLifecycleStage::SeekRequest,
            location: crate::SegmentLocation::default(),
        });
        let result = self.seek_owned(position);
        match &result {
            Ok(_) => self.emit.enqueue(AudioEvent::SeekLifecycle {
                stage: crate::SeekLifecycleStage::SeekApplied,
                location: crate::SegmentLocation::default(),
            }),
            Err(error) => {
                let failure = TrackFailureKind::from(error);
                self.fail(failure, None);
                self.emit
                    .enqueue(AudioEvent::SeekRejected { target: position });
            }
        }
        result
    }
    fn host_sample_rate(&self) -> Option<NonZeroU32> {
        self.host_rate
    }
    fn set_host_sample_rate(&mut self, rate: NonZeroU32) {
        if matches!(self.phase, OwnerPhase::Failed { .. }) || self.host_rate == Some(rate) {
            return;
        }
        let initial_binding = self.host_rate.is_none();
        self.host_rate = Some(rate);
        if initial_binding && self.decode.output_spec().sample_rate == rate {
            return;
        }
        let landing = self.resume.source_end().map_or_else(
            || {
                let spec = self.decode.output_spec();
                spec.frame_at(self.playhead.position())
                    .map(|frame| crate::SourceEnd::new(frame, spec.sample_rate))
                    .map_err(DecodeError::from)
            },
            Ok,
        );
        let result = landing.and_then(|landing| {
            self.shared_stream
                .seek_time_anchor(kithara_decode::DecodeResult::from(ResumeTarget::Source(
                    landing,
                ))?)
                .map_err(|source| DecodeError::Io { source })
                .and_then(|_| {
                    let media_info = self
                        .decode
                        .active()
                        .media_info()
                        .cloned()
                        .or_else(|| self.shared_stream.media_info());
                    self.install_replacement(
                        RecreateState {
                            media_info,
                            offset: self.decode.active().base_offset(),
                            cause: RecreateCause::HostRateChange,
                        },
                        Some(landing),
                    )
                })
        });
        if let Err(error) = result {
            self.fail(
                TrackFailureKind::RecreateFailed {
                    offset: self.decode.active().base_offset(),
                },
                Some(error),
            );
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
        self.progress_variant_transition();
        if matches!(self.phase, OwnerPhase::Decoding) {
            self.decode.prepare_deferred();
        }
        Some(self.decode.output_spec())
    }
    fn step_track(&mut self) -> TrackStep<AudioChunk> {
        match &mut self.phase {
            OwnerPhase::AtEof => return TrackStep::Eof,
            OwnerPhase::Failed { failure, .. } => return TrackStep::Failed(*failure),
            OwnerPhase::Decoding => {}
        }
        if self.shared_stream.phase() == kithara_stream::SourcePhase::Cancelled {
            return TrackStep::Failed(self.fail(TrackFailureKind::SourceCancelled, None));
        }
        let action = tick(
            &mut self.decode,
            DecodeCtx {
                cursor: &mut self.resume,
                stream: &self.shared_stream,
                playhead: self.playhead.as_ref(),
                emit: Some(&self.emit),
            },
        );
        match action {
            DecodeAction::Produced(fetch) => TrackStep::Produced(*fetch),
            DecodeAction::Progress => {
                self.wake.wake();
                TrackStep::StateChanged
            }
            DecodeAction::Pending(reason) => TrackStep::Blocked(reason),
            DecodeAction::TransitionPending => TrackStep::Blocked(self.transition_wait_reason()),
            DecodeAction::StartRecreate(recreate) => {
                let offset = recreate.offset;
                match self.install_replacement(recreate, self.resume.source_end()) {
                    Ok(()) => {
                        self.wake.wake();
                        TrackStep::StateChanged
                    }
                    Err(error) => {
                        let failure = TrackFailureKind::RecreateFailed { offset };
                        TrackStep::Failed(self.fail(failure, Some(error)))
                    }
                }
            }
            DecodeAction::Eof => {
                if !matches!(self.phase, OwnerPhase::Failed { .. }) {
                    self.phase = OwnerPhase::AtEof;
                }
                self.emit.enqueue(AudioEvent::EndOfStream);
                TrackStep::Eof
            }
            DecodeAction::Failed(error) => {
                let failure = TrackFailureKind::Decode {
                    kind: crate::map_decode_error_kind(&error),
                };
                TrackStep::Failed(self.fail(failure, Some(error)))
            }
        }
    }
    fn warm_up(&mut self) {
        let _ = self.shared_stream.len();
    }
}

impl<T: StreamType> Drop for StreamAudioSource<T> {
    fn drop(&mut self) {
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
            host_rate: config.host_rate,
            decoder_backend: config.backend,
            playback_resampler_backend: config.playback_resampler_backend,
            playhead,
            emit,
            variant_control,
            phase: OwnerPhase::Decoding,
            failure_logged: false,
            resume: ResumeCursor::default(),
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
        self.discard_local_incoming();
        let _ = control.abort_variant(transition);
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
        let landing = Some(ResumeTarget::Position(plan.landing_time()));
        let info = Some(plan.media_info().clone());
        match self.build_generation(reader, info, 0, landing) {
            Ok(generation) => {
                drop(self.decode.install_incoming(transition, generation));
                self.wake.wake();
            }
            Err(error) => {
                warn!(?error, ?transition, "incoming decoder build failed");
                self.abort_local_incoming(control, transition);
            }
        }
    }

    pub(super) fn build_generation(
        &self,
        reader: OpenedReader,
        info: Option<MediaInfo>,
        offset: u64,
        landing: Option<ResumeTarget>,
    ) -> Result<DecoderGeneration, DecodeError> {
        let gate = reader.construction_gate();
        if let Some(gate) = &gate {
            gate.arm();
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let decoder = self.factory.create(reader, info.clone(), self.host_rate)?;
            let mut generation = DecoderGeneration::new(
                decoder,
                info,
                offset,
                gate.clone(),
                None,
                self.decode.gapless_mode(),
            );
            if let Some(target) = landing {
                generation.notify_seek();
                match generation.seek(kithara_decode::DecodeResult::from(target)?)? {
                    DecoderSeekOutcome::Landed { .. } => generation.trim_to(target),
                    DecoderSeekOutcome::PastEof { .. } => generation.finish(),
                }
            }
            Ok(generation)
        }));
        if let Some(gate) = &gate {
            gate.disarm();
        }
        result.map_err(|payload| {
            warn!(panic = %panic_message(payload), "decoder factory panicked");
            DecodeError::InvalidData {
                detail: "decoder factory panicked",
            }
        })?
    }

    pub(super) fn install_replacement(
        &mut self,
        recreate: RecreateState,
        landing: Option<crate::SourceEnd>,
    ) -> Result<(), DecodeError> {
        self.abandon_incoming();
        self.shared_stream
            .probe_seek(SeekFrom::Start(recreate.offset))
            .map_err(|source| DecodeError::Io { source })?;
        let reader = self.shared_stream.open_rebuild_reader(recreate.offset);
        let generation = self.build_generation(
            reader,
            recreate.media_info,
            recreate.offset,
            landing.map(ResumeTarget::Source),
        )?;
        let old_spec = self.decode.output_spec();
        self.decode
            .prepare_replacement_profile(generation.blender_profile());
        if let Some(error) = self.decode.take_stage_error() {
            return Err(error);
        }
        drop(self.decode.replace_active(generation));
        self.decode.reset();
        self.resume.clear();
        if let Some(landing) = landing {
            self.resume.rebase(landing);
        }
        if !matches!(self.phase, OwnerPhase::Failed { .. }) {
            self.phase = if self.decode.active().is_finished() {
                OwnerPhase::AtEof
            } else {
                OwnerPhase::Decoding
            };
        }
        let new_spec = self.decode.output_spec();
        if old_spec != new_spec {
            self.emit.enqueue(AudioEvent::FormatChanged {
                old: old_spec,
                new: new_spec,
            });
        }
        self.publish_generation(match recreate.cause {
            RecreateCause::FormatBoundary => DecoderChangeCause::FormatBoundary,
            RecreateCause::HostRateChange => DecoderChangeCause::HostRateChange,
            RecreateCause::VariantSwitch => DecoderChangeCause::VariantSwitch,
        });
        Ok(())
    }

    pub(super) fn publish_generation(&self, cause: DecoderChangeCause) {
        enqueue_generation_installed(
            &self.emit,
            &GenerationInstalled {
                backend: self.decoder_backend,
                cause,
                generation: self.decode.active(),
                host_sample_rate: self.host_rate.map_or(0, NonZeroU32::get),
                playback_resampler_backend: self.playback_resampler_backend,
                recreates_on_route: true,
            },
        );
    }

    pub(super) fn seek_owned(
        &mut self,
        position: Duration,
    ) -> Result<SeekOutcome, crate::AudioReadError> {
        self.discard_local_incoming();
        drop(self.decode.notify_seek());
        self.decode.reset();
        self.resume.clear();
        if let Some(duration) = self
            .playhead
            .duration()
            .filter(|duration| position >= *duration)
        {
            let outcome = DecoderSeekOutcome::PastEof { duration };
            commit_outcome(
                self.decode.active(),
                &self.shared_stream,
                self.playhead.as_ref(),
                &outcome,
            );
            if !matches!(self.phase, OwnerPhase::Failed { .. }) {
                self.phase = OwnerPhase::AtEof;
            }
            return Ok(SeekOutcome::PastEof {
                target: position,
                duration,
            });
        }
        let _anchor = self
            .shared_stream
            .seek_time_anchor(position)
            .map_err(|source| DecodeError::Io { source })?;
        if let FormatDecision::Recreate(recreate) =
            detect(&self.shared_stream, self.decode.active())
        {
            let offset = recreate.offset;
            self.install_replacement(recreate, None).map_err(|_| {
                crate::AudioReadError::Stream {
                    what: "seek decoder recreation",
                    source: crate::FailureSource::Producer {
                        failure: TrackFailureKind::RecreateFailed { offset },
                    },
                }
            })?;
        }
        if let Some(len) = self.shared_stream.len() {
            self.decode
                .update_len(len.saturating_sub(self.decode.active().base_offset()));
        }
        let outcome = self
            .decode
            .seek(&self.shared_stream, self.playhead.as_ref(), position)?;
        match outcome {
            DecoderSeekOutcome::Landed { landed_at, .. } => {
                if !matches!(self.phase, OwnerPhase::Failed { .. }) {
                    self.phase = OwnerPhase::Decoding;
                }
                Ok(SeekOutcome::Landed {
                    target: position,
                    landed_at,
                })
            }
            DecoderSeekOutcome::PastEof { duration } => {
                if !matches!(self.phase, OwnerPhase::Failed { .. }) {
                    self.phase = OwnerPhase::AtEof;
                }
                Ok(SeekOutcome::PastEof {
                    target: position,
                    duration,
                })
            }
        }
    }
}
