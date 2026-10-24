use std::{
    any::Any,
    mem,
    num::NonZeroU32,
    panic::{AssertUnwindSafe, catch_unwind},
};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    DecoderFactory as BackendDecoderFactory, DecoderSeekOutcome, GaplessMode, LandingClock,
};
use kithara_events::DeferredBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::{AudioChunk, AudioSpec};
use kithara_stream::{
    ByteMap, MediaInfo, OpenedReader, ReaderProfile, StreamType, VariantTransition,
};
use tracing::{debug, warn};

use crate::{
    AudioLaneEvent, AudioObserver,
    audio::normalize_geometry,
    pipeline::{
        blend::GaplessBlender,
        decode::{
            generation::{DecoderGeneration, StageFailure, StageOutput, StageResult},
            output::DecodedOutput,
            resume::ResumeCursor,
            transition::IncomingDecode,
        },
        fetch::Fetch,
        rebuild::RecreateState,
        seek::ResumeTarget,
        stream::shared::SharedStream,
        track::WaitingReason,
    },
};

type DecoderBuilder = dyn Fn(OpenedReader, Option<MediaInfo>, Option<NonZeroU32>) -> Result<Box<dyn Decoder>, DecodeError>
    + Send
    + Sync;

/// Decoder construction and reader-profile policy for one configured track.
#[derive(Clone)]
pub(crate) struct DecoderFactory {
    builder: Arc<DecoderBuilder>,
    configured_media_info: Option<MediaInfo>,
}

impl DecoderFactory {
    pub(crate) fn new(
        builder: impl Fn(
            OpenedReader,
            Option<MediaInfo>,
            Option<NonZeroU32>,
        ) -> Result<Box<dyn Decoder>, DecodeError>
        + Send
        + Sync
        + 'static,
        configured_media_info: Option<MediaInfo>,
    ) -> Self {
        Self {
            configured_media_info,
            builder: Arc::new(builder),
        }
    }

    pub(crate) fn create(
        &self,
        reader: OpenedReader,
        media_info: Option<MediaInfo>,
        host_rate: Option<NonZeroU32>,
    ) -> Result<Box<dyn Decoder>, DecodeError> {
        (self.builder)(reader, media_info, host_rate)
    }

    pub(crate) fn build_generation(
        &self,
        reader: OpenedReader,
        info: Option<MediaInfo>,
        offset: u64,
        landing: Option<ResumeTarget>,
        host_rate: Option<NonZeroU32>,
        gapless_mode: GaplessMode,
    ) -> Result<(DecoderGeneration, Option<DecoderSeekOutcome>), DecodeError> {
        let gate = reader.construction_gate();
        if let Some(gate) = &gate {
            gate.arm();
        }
        let result = catch_unwind(AssertUnwindSafe(|| {
            let decoder = self.create(reader, info.clone(), host_rate)?;
            let mut generation =
                DecoderGeneration::new(decoder, info, offset, gate.clone(), None, gapless_mode);
            let outcome = if let Some(target) = landing {
                generation.seal_visible_origin(gapless_mode);
                generation.notify_seek();
                let position = DecodeResult::from(target)?;
                let outcome = match target {
                    ResumeTarget::Render(_) => generation.seek_with_wait(
                        position,
                        gapless_mode,
                        true,
                        kithara_decode::PrepareWait::Wait,
                    ),
                    ResumeTarget::Position(_) | ResumeTarget::Source(_) => generation
                        .seek_with_wait(
                            position,
                            gapless_mode,
                            false,
                            kithara_decode::PrepareWait::Wait,
                        ),
                }?;
                match outcome {
                    DecoderSeekOutcome::Landed { .. } | DecoderSeekOutcome::Pending(_) => {
                        generation.trim_to(target);
                    }
                    DecoderSeekOutcome::PastEof { .. } => generation.finish()?,
                }
                Some(outcome)
            } else {
                None
            };
            Ok((generation, outcome))
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

    pub(crate) fn reader_profile(
        &self,
        media_info: &MediaInfo,
        byte_map: Option<&dyn ByteMap>,
    ) -> ReaderProfile {
        let mut resolved = media_info.clone();
        if let Some(configured) = &self.configured_media_info {
            if configured.codec.is_some() {
                resolved.codec = configured.codec;
            }
            if configured.container.is_some() {
                resolved.container = configured.container;
            }
        }
        BackendDecoderFactory::reader_profile(&resolved, byte_map)
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ActiveDecode {
    #[field(get, vis = "pub(crate)")]
    pub(super) active: DecoderGeneration,
    pub(super) blender: GaplessBlender,
    /// Which transition already announced `DecoderEvent::TransitionHold`,
    /// so a held pass is reported once instead of per tick.
    pub(super) announced_hold: Option<VariantTransition>,
    pub(super) incoming: Option<IncomingDecode>,
    output: DecodedOutput,
    #[field(get, vis = "pub(crate)", copy)]
    gapless_mode: GaplessMode,
    observer: Option<Box<dyn AudioObserver>>,
    pub(super) stage_error: Option<DecodeError>,
    pub(super) discontinuity_revision: u64,
}

pub(crate) struct DecodeCtx<'a, T: StreamType> {
    pub(crate) cursor: &'a mut ResumeCursor,
    pub(crate) physical_duration: Option<Duration>,
    pub(crate) stream: &'a SharedStream<T>,
    pub(crate) emit: Option<&'a DeferredBus<AudioLaneEvent>>,
}

pub(crate) enum DecodeAction {
    ReaderRetired,
    Progress,
    Produced(Box<Fetch<AudioChunk>>),
    Pending(WaitingReason),
    TransitionPending,
    StartRecreate(RecreateState),
    Eof,
    Failed(DecodeError),
}

impl ActiveDecode {
    pub(crate) fn release_input(&mut self) -> kithara_decode::InputState {
        self.active.decoder_mut().release_input()
    }

    pub(crate) fn new<S>(
        active: DecoderGeneration,
        gapless_mode: GaplessMode,
        observer: Option<Box<dyn AudioObserver>>,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let blender = GaplessBlender::new(active.spec(), pools)?;
        Ok(Self {
            active,
            gapless_mode,
            blender,
            observer,
            output: DecodedOutput::default(),
            discontinuity_revision: 0,
            incoming: None,
            announced_hold: None,
            stage_error: None,
        })
    }

    pub(crate) fn seal_visible_origin(&mut self) {
        self.active.seal_visible_origin(self.gapless_mode);
    }

    delegate::delegate! {
        to self.active {
            #[call(finish)]
            pub(crate) fn finish_active(&mut self) -> DecodeResult<()>;
            pub(crate) fn mark_source_exhausted(&mut self);
            #[expr(crate::SourceDiscontinuity::new(self.discontinuity_revision, $))]
            #[call(spec)]
            pub(crate) fn discontinuity(&self) -> crate::SourceDiscontinuity;
            #[call(spec)]
            pub(crate) fn output_spec(&self) -> AudioSpec;
        }
        to self.output {
            pub(crate) const fn stats(&self) -> (u64, u64);
            pub(crate) fn track(
                &mut self,
                chunk: &AudioChunk,
                emit: Option<&DeferredBus<AudioLaneEvent>>,
            );
        }
        to self.blender {
            #[cfg(test)]
            #[call(is_steady)]
            pub(crate) fn blender_is_steady(&self) -> bool;
        }
    }

    pub(crate) fn next_chunk(&mut self, stream_position: u64) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = self.active.next_chunk();
        let (chunks, samples) = self.stats();
        match &outcome {
            Ok(DecoderChunkOutcome::Eof) => {
                debug!(
                    chunks,
                    samples,
                    pos = stream_position,
                    "decoder returned EOF"
                );
            }
            Err(error) => {
                debug!(error_class = ?error.classify(), %error, chunks, samples, pos = stream_position, "decoder returned error");
            }
            Ok(DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Pending(_)) => {}
        }
        outcome
    }

    pub(crate) fn next_output(
        &mut self,
        cursor: &mut ResumeCursor,
        physical_duration: Option<Duration>,
    ) -> DecodeResult<Option<AudioChunk>> {
        self.next_output_inner(cursor, physical_duration, true)
    }

    fn next_output_inner(
        &mut self,
        cursor: &mut ResumeCursor,
        physical_duration: Option<Duration>,
        allow_holdback: bool,
    ) -> DecodeResult<Option<AudioChunk>> {
        if allow_holdback && self.transition_holds_output() {
            return Ok(None);
        }
        let holdback =
            allow_holdback && !self.active.is_finished() && self.outgoing_holdback_is_active();
        let next = if holdback {
            match self.active.next_with_holdback() {
                StageOutput::Output(chunk) => chunk,
                StageOutput::Invalid(failure) => return Err(Self::reject_stage(failure)),
            }
        } else {
            self.active.next()
        };
        let Some(mut chunk) = next else {
            return Ok(None);
        };
        normalize_geometry(&mut chunk.meta)?;
        let mut native = chunk.meta;
        native.source_revision = self.discontinuity_revision;
        let native_basis = self.active.observe_native_pcm(native);
        crate::pipeline::seek::skip::rebase_source(
            &mut chunk,
            self.active.pending_head_skip_mut().as_deref(),
            native_basis,
        )?;
        self.active.observe_visible_origin(&chunk);
        chunk.meta.visible_origin = self.active.visible_origin();
        chunk.meta.media_origin = self.media_origin();
        chunk.meta.media_end = self
            .active
            .media_end(self.gapless_mode, physical_duration, None);
        chunk.meta.source_revision = self.discontinuity_revision;
        cursor.record(&chunk, native.end_timestamp);
        if let Some(observer) = &mut self.observer {
            let _observation = observer.try_observe(&chunk);
        }
        Ok(Some(self.blender.process_active(chunk)))
    }

    pub(crate) fn next_output_unheld(
        &mut self,
        cursor: &mut ResumeCursor,
        physical_duration: Option<Duration>,
    ) -> DecodeResult<Option<AudioChunk>> {
        self.next_output_inner(cursor, physical_duration, false)
    }

    pub(crate) fn prepare_deferred(
        &mut self,
        prepare_input: bool,
    ) -> DecodeResult<Option<DecoderSeekOutcome>> {
        let result = self
            .active
            .prepare_deferred(prepare_input, self.gapless_mode);
        self.active.decoder_mut().flush_reader_signals();
        self.flush_incoming_reader_signals();
        if let Ok(Some(outcome)) = &result {
            self.observe_landing(outcome);
        }
        result
    }

    /// A landing that restarted the decoder's output clock re-decodes PCM no
    /// earlier pass produced at the same output frame, so it opens a new
    /// source revision.
    fn observe_landing(&mut self, outcome: &DecoderSeekOutcome) {
        if let DecoderSeekOutcome::Landed {
            clock: LandingClock::Restarted,
            ..
        } = outcome
        {
            self.discontinuity_revision = self.discontinuity_revision.wrapping_add(1);
        }
    }

    pub(crate) fn prepare_incoming_profile(&mut self, profile: AudioSpec) {
        if let Err(error) = self.blender.prepare_active(profile) {
            self.stage_error = Some(DecodeError::backend(error));
            return;
        }
        if self.active.spec() != profile {
            return;
        }
        let result = self
            .active
            .prepare_holdback(profile, self.blender.join_frame_count());
        if let StageResult::Invalid(failure) = result {
            let error = Self::reject_stage(*failure);
            self.stage_error = Some(error);
        }
    }

    pub(crate) fn prepare_replacement_profile(&mut self, profile: AudioSpec) {
        if let Err(error) = self.blender.prepare_active(profile) {
            self.stage_error = Some(DecodeError::backend(error));
        }
    }

    pub(crate) fn push(&mut self, chunk: AudioChunk) -> DecodeResult<()> {
        if !self.outgoing_holdback_is_active() {
            return self.active.push(chunk);
        }
        let cut = self.outgoing_holdback_cut();
        match self.active.push_holdback(chunk, cut)? {
            StageResult::Ready | StageResult::NeedMore => Ok(()),
            StageResult::Invalid(failure) => Err(Self::reject_stage(*failure)),
        }
    }

    fn reject_stage(failure: StageFailure) -> DecodeError {
        let StageFailure { chunk, error } = failure;
        drop(chunk);
        error
    }

    pub(crate) fn replace_active(&mut self, active: DecoderGeneration) -> DecoderGeneration {
        self.discontinuity_revision = self.discontinuity_revision.wrapping_add(1);
        self.blender.replace_active(active.spec());
        mem::replace(&mut self.active, active)
    }

    pub(crate) fn reset(&mut self) {
        self.stage_error = None;
        self.blender.reset();
    }

    fn outgoing_holdback_is_active(&self) -> bool {
        let Some(IncomingDecode::Priming { generation, .. }) = self.incoming.as_ref() else {
            return false;
        };
        self.blender.is_steady() && self.active.spec() == generation.spec()
    }
    pub(crate) fn notify_seek(&mut self) -> Option<DecoderGeneration> {
        self.active.notify_seek();
        self.reset();
        self.discard_incoming()
    }
    /// Content zero of the active generation under this pipeline's gapless mode.
    pub(crate) fn media_origin(&self) -> Option<Duration> {
        self.active.content_origin(self.gapless_mode)
    }

    pub(crate) fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(position, false)
    }

    pub(crate) fn seek_for_render(
        &mut self,
        position: Duration,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.seek_selected(position, true)
    }

    fn seek_selected(
        &mut self,
        position: Duration,
        for_render: bool,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.active.notify_seek();
        self.reset();
        let outcome = if for_render {
            self.active.seek_for_render(position, self.gapless_mode)
        } else {
            self.active.seek(position, self.gapless_mode)
        };
        if let Ok(ref outcome) = outcome {
            self.observe_landing(outcome);
        }
        if let Ok(ref outcome) = outcome
            && matches!(
                outcome,
                DecoderSeekOutcome::Landed { .. } | DecoderSeekOutcome::Pending(_)
            )
        {
            let target = if for_render {
                ResumeTarget::Render(position)
            } else {
                ResumeTarget::Position(position)
            };
            self.active.trim_to(target);
        }
        debug!(?position, ?outcome, "decoder seek outcome");
        outcome
    }

    pub(crate) fn take_stage_error(&mut self) -> Option<DecodeError> {
        self.stage_error.take()
    }

    pub(crate) fn update_len(&self, len: u64) {
        self.active.decoder().update_byte_len(len);
    }
}

/// Text of a caught decoder panic, for the warning that replaces the crash.
pub(crate) fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload.downcast::<&'static str>().map_or_else(
            |_| "unknown panic payload".to_string(),
            |message| (*message).to_string(),
        ),
    }
}
#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use kithara_abr::{AbrMode, AbrReason, AbrState, VariantIndex};
    use kithara_decode::DecoderSeekOutcome;
    use kithara_platform::time::Duration;
    use kithara_signal::{AudioChunkInfo, AudioSpec};
    use kithara_stream::{
        AudioCodec, ContainerFormat, PendingReason, PrerollHint, ReaderInput, VariantTransition,
        VariantTransitionId,
    };
    use kithara_test_fixtures::unit_fixtures::{
        cursor_half, decode_negative_quarter, decode_quarter,
    };
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        pipeline::{
            decode::transition::{IncomingPrime, OutgoingFrontier, PromotionJoin},
            seek::ResumeTarget,
        },
        test_pools::{Pools, pools, sample_buffer},
        traits::{AudioObserveError, AudioObserverMock},
    };

    fn active_decode(
        pools: &Pools,
        active: DecoderGeneration,
        gapless_mode: GaplessMode,
    ) -> ActiveDecode {
        active_decode_with_observer(pools, active, gapless_mode, None)
    }

    fn active_decode_with_observer(
        pools: &Pools,
        active: DecoderGeneration,
        gapless_mode: GaplessMode,
        observer: Option<Box<dyn AudioObserver>>,
    ) -> ActiveDecode {
        ActiveDecode::new(active, gapless_mode, observer, pools)
            .expect("blender scratch fits test pools")
    }

    #[kithara::test]
    fn configured_container_selects_the_incoming_reader_profile() {
        let factory = DecoderFactory::new(
            |_reader, _media_info, _rate| -> Result<Box<dyn Decoder>, DecodeError> {
                panic!("reader-profile test must not construct a decoder")
            },
            Some(
                MediaInfo::builder()
                    .maybe_codec(Some(AudioCodec::Pcm))
                    .maybe_container(Some(ContainerFormat::Wav))
                    .build(),
            ),
        );
        let playlist_info = MediaInfo::builder()
            .maybe_codec(None)
            .maybe_container(Some(ContainerFormat::Fmp4))
            .build();

        let profile = factory.reader_profile(&playlist_info, None);

        assert_eq!(profile.input(), ReaderInput::InitOnly);
    }

    #[kithara::test]
    fn steady_output_bypasses_transition_staging(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        let initial_capacity = decode.active().staged_capacity();
        let mut cursor = ResumeCursor::default();
        decode
            .push(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 64,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..64 * usize::from(spec.channels)]),
            ))
            .expect("steady fixture PCM is valid");

        assert_eq!(decode.active().staged_capacity(), initial_capacity);
        assert!(decode.active().staged_span().is_none());
        assert!(
            decode
                .next_output(&mut cursor, None)
                .expect("steady output remains valid")
                .is_some()
        );
        assert_eq!(decode.active().staged_capacity(), initial_capacity);
    }

    #[kithara::test]
    #[case(AudioObserveError::Full)]
    #[case(AudioObserveError::Closed)]
    fn rejected_pcm_observation_sees_decoder_output_without_rejecting_playback(
        #[case] rejection: AudioObserveError,
        decode_quarter: Vec<f32>,
    ) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let observer = Unimock::new(
            AudioObserverMock::try_observe
                .next_call(matching!((chunk) if
                    chunk.spec()
                        == AudioSpec::new(
                            2,
                            NonZeroU32::new(44_100).expect("test rate"),
                        )
                        && chunk.samples.first() == Some(&0.25)
                ))
                .returns(Err(rejection)),
        );
        let mut decode = active_decode_with_observer(
            &pools,
            generation(spec),
            GaplessMode::Disabled,
            Some(Box::new(observer)),
        );
        decode
            .push(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 64,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..64 * usize::from(spec.channels)]),
            ))
            .expect("fixture PCM is valid");
        let mut cursor = ResumeCursor::default();

        let output = decode
            .next_output(&mut cursor, None)
            .expect("observer saturation does not fail playback")
            .expect("observer saturation does not consume playback PCM");

        assert_eq!(output.frames(), 64);
        assert_eq!(output.samples.first(), Some(&0.25));
    }

    #[kithara::test]
    fn invalid_holdback_pcm_is_retained_for_shell_retirement(decode_quarter: Vec<f32>) {
        let config = kithara_bufpool::PoolConfig::builder()
            .max_buffers(32)
            .max_retained_capacity(1)
            .build();
        let pools = crate::test_pools::pools_with(1024 * 1024, config, config);
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let active = generation(spec);
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(spec);
        let make_chunk = |offset| {
            AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frame_offset: offset,
                    frames: 4,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..4 * usize::from(spec.channels)]),
            )
        };
        decode
            .push(make_chunk(0))
            .expect("first holdback chunk is valid");

        let rejected = make_chunk(5);
        let rejected_bytes = rejected.samples.capacity() * size_of::<f32>();
        let allocated_before = pools.stats().allocated_bytes;
        assert!(decode.push(rejected).is_err());
        assert_eq!(
            pools.stats().allocated_bytes,
            allocated_before - rejected_bytes
        );
        assert_eq!(
            decode.active().staged_span().map(|(_, end, _)| end),
            Some(4)
        );
    }

    #[kithara::test]
    fn a_seek_retires_the_transition_join_it_invalidated(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let active = generation(spec);
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(spec);
        let make_chunk = |offset| {
            AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frame_offset: offset,
                    frames: 4,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..4 * usize::from(spec.channels)]),
            )
        };
        decode
            .push(make_chunk(0))
            .expect("first holdback chunk is valid");

        let invalidated = decode.notify_seek();

        assert!(
            invalidated.is_some(),
            "a seek that retires the join must hand back the incoming half claiming it"
        );
        assert!(decode.incoming_transition().is_none());
        decode
            .push(make_chunk(1_443_179))
            .expect("PCM resumed by a seek must not be judged against a join the seek retired");
    }

    #[kithara::test]
    fn reset_clears_prepare_error_but_retains_rejected_pcm_for_shell_retirement(
        decode_quarter: Vec<f32>,
        cursor_half: Vec<f32>,
    ) {
        let config = kithara_bufpool::PoolConfig::builder()
            .max_buffers(32)
            .max_retained_capacity(1)
            .build();
        let pools = crate::test_pools::pools_with(1024 * 1024, config, config);
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut active = generation(spec);
        active
            .stage(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 4,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..4 * usize::from(spec.channels)]),
            ))
            .expect("valid gapless PCM");
        let rejected = AudioChunk::new(
            AudioChunkInfo {
                spec,
                frame_offset: 5,
                frames: 4,
                ..Default::default()
            },
            sample_buffer(&pools, &cursor_half[..4 * usize::from(spec.channels)]),
        );
        let rejected_bytes = rejected.samples.capacity() * size_of::<f32>();
        active.stage(rejected).expect("valid gapless PCM");

        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });

        decode.prepare_replacement_profile(spec);
        let allocated_before = pools.stats().allocated_bytes;
        decode.prepare_incoming_profile(spec);
        assert!(
            decode.stage_error.is_some(),
            "pre-existing discontinuous PCM must arm a prepare-time error"
        );

        decode.reset();

        assert!(
            decode.take_stage_error().is_none(),
            "reset must not leak the invalidated transition error into the next lifecycle"
        );
        assert_eq!(
            pools.stats().allocated_bytes,
            allocated_before - rejected_bytes
        );
        assert_eq!(
            decode.active().staged_span().map(|(_, end, _)| end),
            Some(4)
        );
        decode.reset();
        assert_eq!(
            pools.stats().allocated_bytes,
            allocated_before - rejected_bytes
        );
    }

    #[kithara::test]
    fn unheld_output_drains_pcm_parked_for_a_priming_join(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let active = generation(spec);
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(spec);
        let samples = decode_quarter[..128 * usize::from(spec.channels)].to_vec();
        decode
            .push(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 128,
                    ..Default::default()
                },
                sample_buffer(&pools, &samples),
            ))
            .expect("valid fixture PCM enters prepared holdback");
        let mut cursor = ResumeCursor::default();

        assert!(
            decode
                .next_output(&mut cursor, None)
                .expect("held output remains valid")
                .is_none()
        );
        let output = decode
            .next_output_unheld(&mut cursor, None)
            .expect("unheld output remains valid")
            .expect("EOF drain must release the held outgoing tail");
        assert_eq!(output.meta.frames, 128);
        assert!(
            decode
                .next_output_unheld(&mut cursor, None)
                .expect("drained output remains valid")
                .is_none()
        );

        for (
            old_rate,
            new_rate,
            source_frame,
            first_frame,
            cut,
            origin,
            incoming_cut,
            timestamp_ns,
        ) in [
            (
                44_100,
                48_000,
                44_101,
                48_001,
                48_002,
                0,
                48_002,
                1_000_022_675,
            ),
            (191_999, 191_997, 3, 2, 3, 1, 2, 15_625),
        ] {
            let spec = AudioSpec::new(1, NonZeroU32::new(new_rate).expect("new route rate"));
            let source_end = crate::SourceEnd::new(
                source_frame,
                NonZeroU32::new(old_rate).expect("old route rate"),
            );
            let mut active = generation(spec);
            active.retain_visible_origin(
                None,
                Some(
                    spec.duration_for(origin)
                        .expect("outgoing comparison origin"),
                ),
            );
            active.trim_to(ResumeTarget::Source(source_end));
            let mut incoming = generation(spec);
            incoming.retain_visible_origin(None, None);
            let mut incoming_samples = vec![0.5; 16_384];
            incoming_samples[1] = 0.75;
            incoming
                .stage(AudioChunk::new(
                    AudioChunkInfo {
                        spec,
                        frame_offset: first_frame,
                        frames: 16_384,
                        ..Default::default()
                    },
                    sample_buffer(&pools, &incoming_samples),
                ))
                .expect("incoming PCM covers the converted cut");
            let mut cursor = ResumeCursor::default();
            cursor.rebase(source_end);
            let (frame, rate) = cursor.decode_head().expect("source resume frontier");
            let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
            decode.incoming = Some(IncomingDecode::Priming {
                preparation:
                    crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
                transition,
                generation: incoming,
                frontier: OutgoingFrontier::Exact { frame, rate },
            });
            decode.prepare_incoming_profile(spec);
            assert_eq!(decode.outgoing_holdback_cut(), Some(cut));
            for (frame, marker) in [(first_frame, 0.125), (first_frame + 1, 0.25)] {
                decode
                    .push(AudioChunk::new(
                        AudioChunkInfo {
                            spec,
                            frame_offset: frame,
                            frames: 1,
                            ..Default::default()
                        },
                        sample_buffer(&pools, &[marker]),
                    ))
                    .expect("valid resumed PCM enters holdback");
            }
            assert!(decode.prepare_promotion().is_none());
            let incoming = decode
                .discard_incoming()
                .expect("the join is still pending");
            for (frame, marker) in [(first_frame, 0.125), (first_frame + 1, 0.25)] {
                let output = decode
                    .next_output(&mut cursor, None)
                    .expect("cancelled promotion keeps resumed PCM valid")
                    .expect("cancelled promotion must retain the predecessor");
                assert_eq!(output.meta.frame_offset, frame);
                assert_eq!(output.samples.as_ref(), &[marker]);
                if frame == first_frame {
                    assert_eq!(output.meta.timestamp, Duration::from_nanos(timestamp_ns));
                }
            }

            let mut active = generation(spec);
            active.retain_visible_origin(
                None,
                Some(
                    spec.duration_for(origin)
                        .expect("outgoing comparison origin"),
                ),
            );
            active
                .stage(AudioChunk::new(
                    AudioChunkInfo {
                        spec,
                        frame_offset: first_frame,
                        frames: 16_384,
                        ..Default::default()
                    },
                    sample_buffer(&pools, &vec![0.25; 16_384]),
                ))
                .expect("outgoing PCM covers the complete join");
            let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
            decode.incoming = Some(IncomingDecode::Priming {
                preparation:
                    crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
                transition,
                generation: incoming,
                frontier: OutgoingFrontier::Exact { frame, rate },
            });
            decode.prepare_incoming_profile(spec);
            let prepared = decode
                .prepare_promotion()
                .expect("both native sides cover the exact join");
            assert!(matches!(
                prepared.join,
                PromotionJoin::Blend { outgoing_first, .. } if outgoing_first == cut
            ));
            let first = prepared
                .generation
                .staged
                .front()
                .expect("incoming join PCM");
            assert_eq!(first.meta.frame_offset, incoming_cut);
            let trimmed = usize::try_from(incoming_cut - first_frame).expect("trim count");
            assert_eq!(first.samples.as_ref(), &incoming_samples[trimmed..]);
            let _retired = decode.commit_prepared_promotion(prepared);
            let output = decode
                .next_output_unheld(&mut ResumeCursor::default(), None)
                .expect("committed native join remains valid")
                .expect("committed native join produces audio");
            assert_eq!(output.meta.frame_offset, incoming_cut);
            assert_eq!(output.samples.first(), Some(&0.25));
            assert_eq!(output.samples.last(), Some(&0.5));
        }
    }

    #[kithara::test]
    fn holdback_coverage_is_measured_from_the_exact_promotion_cut() {
        const CUT: u64 = 256;
        const OUTGOING_FRAMES: u32 = 2_000;
        const INCOMING_FRAMES: u32 = 2_500;

        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut incoming = generation(spec);
        incoming
            .stage(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: INCOMING_FRAMES,
                    ..Default::default()
                },
                sample_buffer(
                    &pools,
                    &vec![
                        0.5;
                        usize::try_from(INCOMING_FRAMES)
                            .expect("test frames fit usize")
                            .saturating_mul(usize::from(spec.channels))
                    ],
                ),
            ))
            .expect("valid gapless PCM");
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Exact {
                frame: CUT,
                rate: spec.sample_rate.get(),
            },
        });
        decode.prepare_incoming_profile(spec);
        let capacity = decode.active().staged_capacity();
        for (offset, frames) in [(0, 64), (64, OUTGOING_FRAMES - 64)] {
            decode
                .push(AudioChunk::new(
                    AudioChunkInfo {
                        spec,
                        frame_offset: offset,
                        frames,
                        ..Default::default()
                    },
                    sample_buffer(
                        &pools,
                        &vec![
                            0.25;
                            usize::try_from(frames)
                                .expect("test frames fit usize")
                                .saturating_mul(usize::from(spec.channels))
                        ],
                    ),
                ))
                .expect("valid outgoing PCM enters holdback");
        }

        assert!(decode.transition_holds_output());
        assert!(
            decode.outgoing_holdback_needs_pcm(),
            "PCM that covers the join only from queue-front zero does not cover exact cut {CUT}"
        );
        decode
            .push(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frame_offset: u64::from(OUTGOING_FRAMES),
                    frames: 32,
                    ..Default::default()
                },
                sample_buffer(&pools, &vec![0.25; 32 * usize::from(spec.channels)]),
            ))
            .expect("outgoing PCM must complete the exact promotion cut");
        assert!(!decode.outgoing_holdback_needs_pcm());
        assert_eq!(decode.active().staged_capacity(), capacity);
        let prepared = decode
            .prepare_promotion()
            .expect("the exact join is covered");
        let _retired = decode.commit_prepared_promotion(prepared);
        let mut cursor = ResumeCursor::default();
        let output = decode
            .next_output_unheld(&mut cursor, None)
            .expect("promoted PCM remains valid")
            .expect("promotion must produce audio");
        assert_eq!(output.samples[0], 0.25);
        assert_eq!(output.samples[output.samples.len() - 1], 0.5);
    }

    #[kithara::test]
    fn incoming_landed_ahead_retargets_to_the_observed_outgoing_frontier() {
        const OLD_CUT: u64 = 256;
        const LANDED: u64 = 512;
        const FRAMES: u32 = 2_500;

        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let make_chunk = |offset| {
            AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frame_offset: offset,
                    frames: FRAMES,
                    ..Default::default()
                },
                sample_buffer(
                    &pools,
                    &vec![
                        0.25;
                        usize::try_from(FRAMES)
                            .expect("test frames fit usize")
                            .saturating_mul(usize::from(spec.channels))
                    ],
                ),
            )
        };
        let mut active = generation(spec);
        active.stage(make_chunk(0)).expect("valid gapless PCM");
        let mut incoming = generation(spec);
        incoming
            .stage(make_chunk(LANDED))
            .expect("valid gapless PCM");
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Exact {
                frame: OLD_CUT,
                rate: spec.sample_rate.get(),
            },
        });

        let outcome = decode.prime_incoming(OutgoingFrontier::Exact {
            frame: LANDED,
            rate: spec.sample_rate.get(),
        });

        assert_eq!(outcome, IncomingPrime::Ready);
        assert_eq!(
            decode.incoming_frontier(),
            Some(OutgoingFrontier::Exact {
                frame: LANDED,
                rate: spec.sample_rate.get(),
            })
        );
    }

    #[kithara::test]
    fn incoming_staged_from_its_priming_promotes_at_the_outgoing_frontier() {
        const FRONTIER: u64 = 4_096;
        const FRAMES: u32 = 8_192;

        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let aac_generation = || {
            DecoderGeneration::new(
                Box::new(TerminalDecoder::new(spec, TerminalOutcome::Eof)),
                Some(
                    MediaInfo::builder()
                        .maybe_codec(Some(AudioCodec::AacLc))
                        .build(),
                ),
                0,
                None,
                None,
                GaplessMode::MediaOnly,
            )
        };
        let head_chunk = || {
            AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: FRAMES,
                    ..Default::default()
                },
                sample_buffer(
                    &pools,
                    &vec![
                        0.25;
                        usize::try_from(FRAMES)
                            .expect("test frames fit usize")
                            .saturating_mul(usize::from(spec.channels))
                    ],
                ),
            )
        };
        let mut active = aac_generation();
        active.stage(head_chunk()).expect("valid gapless PCM");
        let mut incoming = aac_generation();
        incoming.stage(head_chunk()).expect("valid gapless PCM");
        let mut decode = active_decode(&pools, active, GaplessMode::MediaOnly);
        let frontier = OutgoingFrontier::Exact {
            frame: FRONTIER,
            rate: spec.sample_rate.get(),
        };
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier,
        });

        assert_eq!(decode.prime_incoming(frontier), IncomingPrime::Ready);
    }

    #[kithara::test]
    #[kithara::hang_watchdog]
    fn incoming_variant_change_invalidates_the_stale_generation() {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let incoming = DecoderGeneration::new(
            Box::new(TerminalDecoder::new(spec, TerminalOutcome::VariantChange)),
            None,
            0,
            None,
            None,
            GaplessMode::Disabled,
        );
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });

        assert_eq!(
            decode.prime_incoming(OutgoingFrontier::Awaiting),
            IncomingPrime::Advanced
        );
        let (transition, prepared) = loop {
            hang_tick!();
            if let Some(completed) = decode.poll_incoming_build() {
                break completed;
            }
            kithara_platform::thread::yield_now();
        };
        let prepared = prepared.expect("incoming preparation completes");
        assert!(
            decode
                .install_incoming(transition, prepared.generation)
                .is_none()
        );
        decode.finish_incoming_preparation(prepared.outcome);
        assert!(matches!(
            decode.incoming,
            Some(IncomingDecode::Failed {
                transition: failed,
                ..
            }) if failed == transition
        ));
    }

    #[kithara::test]
    fn active_join_drains_before_a_latched_follow_up_transition_holds_output(
        decode_quarter: Vec<f32>,
        decode_negative_quarter: Vec<f32>,
    ) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        let frames = u32::try_from(decode.blender.join_frame_count()).expect("test join fits u32");
        let samples = usize::try_from(frames)
            .expect("test join fits usize")
            .saturating_mul(usize::from(spec.channels));
        let active_samples = decode_quarter
            .iter()
            .copied()
            .cycle()
            .take(samples)
            .collect::<Vec<_>>();
        let outgoing_samples = decode_negative_quarter
            .iter()
            .copied()
            .cycle()
            .take(samples)
            .collect::<Vec<_>>();
        incoming
            .stage(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames,
                    ..Default::default()
                },
                sample_buffer(&pools, &active_samples),
            ))
            .expect("the follow-up transition already has join PCM");
        decode
            .active
            .stage(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames,
                    ..Default::default()
                },
                sample_buffer(&pools, &active_samples),
            ))
            .expect("valid gapless PCM");
        decode
            .blender
            .prepare_active(spec)
            .expect("join scratch fits test pools");
        assert!(decode.blender.prepare_join(|outgoing| {
            outgoing.copy_from_slice(&outgoing_samples);
            true
        }));
        decode.blender.commit_join();
        decode.incoming = Some(IncomingDecode::Priming {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Exact {
                frame: 0,
                rate: spec.sample_rate.get(),
            },
        });
        let mut cursor = ResumeCursor::default();

        let frontier = OutgoingFrontier::Exact {
            frame: 0,
            rate: spec.sample_rate.get(),
        };
        let target = decode
            .incoming_preparation(frontier)
            .expect("the follow-up transition has a preparation target");
        let Some(IncomingDecode::Priming { generation, .. }) = decode.incoming.as_ref() else {
            panic!("the follow-up transition must remain owned by the decoder");
        };
        assert!(matches!(
            crate::pipeline::decode::transition::incoming_readiness(generation, target, 0),
            crate::pipeline::decode::transition::PromotionReadiness::Ready(_)
        ));
        for _ in 0..3 {
            assert_eq!(
                decode.prime_incoming(frontier),
                IncomingPrime::Pending,
                "the active join needs consumption, not repeated incoming preparation"
            );
            assert!(matches!(
                decode.incoming,
                Some(IncomingDecode::Priming { .. })
            ));
            assert!(decode.prepare_promotion().is_none());
        }
        assert!(
            !decode.transition_holds_output(),
            "the follow-up transition must not freeze an active join"
        );
        let output = decode
            .next_output(&mut cursor, None)
            .expect("active join remains decodable");

        assert!(output.is_some(), "the prior join must keep consuming PCM");
        assert!(decode.blender.is_steady(), "the prior join must finish");
        decode.mark_source_exhausted();
        decode.finish_active().expect("finish the outgoing source");
        assert_eq!(decode.prime_incoming(frontier), IncomingPrime::Ready);
        let prepared = decode
            .prepare_promotion()
            .expect("the follow-up transition can promote after the prior join");
        let _retired = decode.commit_prepared_promotion(prepared);
        let promoted = decode
            .next_output(&mut cursor, None)
            .expect("promoted PCM remains valid")
            .expect("the follow-up transition produces its staged PCM");
        assert_eq!(promoted.samples.as_ref(), active_samples.as_slice());
    }

    #[kithara::test]
    fn promotion_resets_render_geometry_only_when_decoder_coordinates_change() {
        use crate::pipeline::decode::transition::{PreparedPromotion, PromotionJoin};

        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut outgoing = generation(spec);
        outgoing.retain_visible_origin(Some(Duration::ZERO), None);
        let mut decode = active_decode(&pools, outgoing, GaplessMode::MediaOnly);
        let initial = decode.discontinuity().revision();
        let mut incoming = generation(spec);
        incoming.retain_visible_origin(None, None);
        assert_eq!(decode.active().visible_origin(), Some(Duration::ZERO));
        assert_eq!(incoming.visible_origin(), None);
        assert_eq!(
            decode.active().comparison_origin(GaplessMode::MediaOnly),
            incoming.comparison_origin(GaplessMode::MediaOnly),
        );
        drop(decode.commit_prepared_promotion(PreparedPromotion {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            generation: incoming,
            transition,
            join: PromotionJoin::HardCut,
            resume_head: crate::SourceEnd::new(0, spec.sample_rate),
        }));
        assert_eq!(decode.active().visible_origin(), None);
        assert_eq!(decode.discontinuity().revision(), initial);

        let mut incoming = generation(spec);
        incoming.align_timeline_gap(1_024);
        drop(decode.commit_prepared_promotion(PreparedPromotion {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            generation: incoming,
            transition,
            join: PromotionJoin::HardCut,
            resume_head: crate::SourceEnd::new(0, spec.sample_rate),
        }));
        assert_ne!(decode.discontinuity().revision(), initial);
        let shifted = decode.discontinuity().revision();
        let incoming_spec = AudioSpec::new(1, NonZeroU32::new(48_000).expect("incoming rate"));
        drop(decode.commit_prepared_promotion(PreparedPromotion {
            preparation:
                crate::pipeline::decode::transition_promotion::preparation::fixtures::preparation_owner(),
            generation: generation(incoming_spec),
            transition,
            join: PromotionJoin::HardCut,
            resume_head: crate::SourceEnd::new(0, incoming_spec.sample_rate),
        }));
        assert_eq!(decode.discontinuity().spec(), &incoming_spec);
        assert_ne!(decode.discontinuity().revision(), shifted);
    }

    #[kithara::test]
    fn latched_incoming_preparation_keeps_outgoing_pcm_running(decode_quarter: Vec<f32>) {
        let pools = pools();
        let spec = AudioSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket()),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        decode
            .active
            .stage(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: 64,
                    ..Default::default()
                },
                sample_buffer(&pools, &decode_quarter[..64 * usize::from(spec.channels)]),
            ))
            .expect("valid gapless PCM");
        decode.incoming = Some(IncomingDecode::Preparing {
            transition,
            frontier: OutgoingFrontier::Exact {
                frame: 0,
                rate: spec.sample_rate.get(),
            },
        });
        let mut cursor = ResumeCursor::default();

        assert!(!decode.transition_holds_output());
        let output = decode
            .next_output(&mut cursor, None)
            .expect("preparing transition keeps active output valid");

        assert!(
            output.is_some(),
            "preparing an incoming reader must not stall playback"
        );
    }

    #[derive(Clone, Copy)]
    enum TerminalOutcome {
        Eof,
        VariantChange,
    }

    struct TerminalDecoder {
        spec: AudioSpec,
        outcome: TerminalOutcome,
    }

    impl TerminalDecoder {
        const fn new(spec: AudioSpec, outcome: TerminalOutcome) -> Self {
            Self { spec, outcome }
        }
    }

    impl Decoder for TerminalDecoder {
        fn duration(&self) -> Option<kithara_decode::DecoderDuration> {
            None
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(match self.outcome {
                TerminalOutcome::Eof => DecoderChunkOutcome::Eof,
                TerminalOutcome::VariantChange => {
                    DecoderChunkOutcome::Pending(PendingReason::VariantChange)
                }
            })
        }

        fn seek(&mut self, position: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(DecoderSeekOutcome::Landed {
                landed_at: position,
                landed_frame: 0,
                landed_byte: None,
                preroll: PrerollHint::NotNeeded,
                clock: LandingClock::Kept,
            })
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    fn generation(spec: AudioSpec) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(TerminalDecoder::new(spec, TerminalOutcome::Eof)),
            None,
            0,
            None,
            None,
            GaplessMode::Disabled,
        )
    }
}
