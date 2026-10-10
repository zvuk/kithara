use std::{any::Any, mem, num::NonZeroU32};

use kithara_bufpool::{HasPool, PoolError, PoolRegion};
use kithara_decode::{
    BlenderProfile, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    DecoderFactory as BackendDecoderFactory, DecoderSeekOutcome, GaplessMode,
};
use kithara_events::DeferredBus;
use kithara_platform::{sync::Arc, time::Duration};
use kithara_signal::AudioChunk;
use kithara_stream::{
    ByteMap, MediaInfo, OpenedReader, PlayheadWrite, ReaderProfile, StreamType, VariantTransition,
};
use tracing::debug;

use crate::{
    AudioLaneEvent, AudioObserver,
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
        seek::emit::commit_outcome,
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
    stage_error: Option<DecodeError>,
    discontinuity_revision: u64,
}

pub(crate) struct DecodeCtx<'a, T: StreamType> {
    pub(crate) cursor: &'a mut ResumeCursor,
    pub(crate) stream: &'a SharedStream<T>,
    pub(crate) playhead: &'a dyn PlayheadWrite,
    pub(crate) emit: Option<&'a DeferredBus<AudioLaneEvent>>,
}

pub(crate) enum DecodeAction {
    Progress,
    Produced(Box<Fetch<AudioChunk>>),
    Pending(WaitingReason),
    TransitionPending,
    StartRecreate(RecreateState),
    Eof,
    Failed(DecodeError),
}

impl ActiveDecode {
    pub(crate) fn new<S>(
        active: DecoderGeneration,
        gapless_mode: GaplessMode,
        observer: Option<Box<dyn AudioObserver>>,
        pools: &PoolRegion<S>,
    ) -> Result<Self, PoolError>
    where
        S: HasPool<f32>,
    {
        let blender = GaplessBlender::new(active.blender_profile(), pools)?;
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

    pub(crate) fn discontinuity(&self) -> crate::SourceDiscontinuity {
        crate::SourceDiscontinuity::new(
            self.discontinuity_revision,
            self.active.blender_profile().spec(),
        )
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
    ) -> DecodeResult<Option<AudioChunk>> {
        self.next_output_inner(cursor, true)
    }

    fn next_output_inner(
        &mut self,
        cursor: &mut ResumeCursor,
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
        crate::pipeline::seek::skip::rebase_source(
            &mut chunk,
            self.active.pending_head_skip_mut().as_deref(),
        )?;
        cursor.record(&chunk);
        if let Some(observer) = &mut self.observer {
            let _observation = observer.try_observe(&chunk);
        }
        Ok(Some(self.blender.process_active(chunk)))
    }

    pub(crate) fn next_output_unheld(
        &mut self,
        cursor: &mut ResumeCursor,
    ) -> DecodeResult<Option<AudioChunk>> {
        self.next_output_inner(cursor, false)
    }

    pub(crate) fn output_spec(&self) -> kithara_signal::AudioSpec {
        self.active.blender_profile().spec()
    }

    pub(crate) fn prepare_deferred(&mut self) {
        self.active.prepare_deferred(true);
        self.active.decoder_mut().flush_reader_signals();
        self.flush_incoming_reader_signals();
    }

    pub(crate) fn prepare_incoming_profile(&mut self, profile: BlenderProfile) {
        if let Err(error) = self.blender.prepare_active(profile) {
            self.stage_error = Some(DecodeError::backend(error));
            return;
        }
        if self.active.blender_profile().spec() != profile.spec() {
            return;
        }
        let result = self
            .active
            .prepare_holdback(profile.spec(), self.blender.join_frame_count());
        if let StageResult::Invalid(failure) = result {
            let error = Self::reject_stage(*failure);
            self.stage_error = Some(error);
        }
    }

    pub(crate) fn prepare_replacement_profile(&mut self, profile: BlenderProfile) {
        if let Err(error) = self.blender.prepare_active(profile) {
            self.stage_error = Some(DecodeError::backend(error));
        }
    }

    pub(crate) fn push(&mut self, chunk: AudioChunk) -> DecodeResult<()> {
        if !self.outgoing_holdback_is_active() {
            self.active.push(chunk);
            return Ok(());
        }
        match self.active.push_holdback(chunk) {
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
        self.blender.replace_active(active.blender_profile());
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
        self.blender.is_steady()
            && self.active.blender_profile().spec() == generation.blender_profile().spec()
    }
    pub(crate) fn notify_seek(&mut self) -> Option<DecoderGeneration> {
        self.active.notify_seek();
        self.reset();
        self.discard_incoming()
    }
    pub(crate) fn seek<T: StreamType>(
        &mut self,
        stream: &SharedStream<T>,
        playhead: &dyn PlayheadWrite,
        position: Duration,
    ) -> DecodeResult<DecoderSeekOutcome> {
        self.active.notify_seek();
        self.reset();
        let before = stream.position();
        let outcome = self.active.seek(position);
        if let Ok(ref outcome) = outcome {
            commit_outcome(&self.active, stream, playhead, outcome);
            if matches!(outcome, DecoderSeekOutcome::Landed { .. }) {
                self.active
                    .trim_to(crate::pipeline::seek::ResumeTarget::Position(position));
            }
        }
        debug!(
            ?position,
            before,
            after = stream.position(),
            ?outcome,
            "decoder seek completed"
        );
        outcome
    }

    pub(crate) fn take_stage_error(&mut self) -> Option<DecodeError> {
        self.stage_error.take()
    }

    pub(crate) fn update_len(&self, len: u64) {
        self.active.decoder().update_byte_len(len);
    }

    delegate::delegate! {
        to self.active {
            #[call(finish)]
            pub(crate) fn finish_active(&mut self);
            pub(crate) fn mark_source_exhausted(&mut self);
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
    use kithara_decode::{BlenderProfile, DecoderSeekOutcome};
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
        pipeline::decode::transition::{IncomingPrime, OutgoingFrontier},
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
                .next_output(&mut cursor)
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
            .next_output(&mut cursor)
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
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
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
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
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
        active.stage(AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: 4,
                ..Default::default()
            },
            sample_buffer(&pools, &decode_quarter[..4 * usize::from(spec.channels)]),
        ));
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
        active.stage(rejected);

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
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });

        decode.prepare_replacement_profile(BlenderProfile::new(spec));
        let allocated_before = pools.stats().allocated_bytes;
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
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
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
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
                .next_output(&mut cursor)
                .expect("held output remains valid")
                .is_none()
        );
        let output = decode
            .next_output_unheld(&mut cursor)
            .expect("unheld output remains valid")
            .expect("EOF drain must release the held outgoing tail");
        assert_eq!(output.meta.frames, 128);
        assert!(
            decode
                .next_output_unheld(&mut cursor)
                .expect("drained output remains valid")
                .is_none()
        );
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
        incoming.stage(AudioChunk::new(
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
        ));
        let mut decode = active_decode(&pools, generation(spec), GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Exact {
                frame: CUT,
                rate: spec.sample_rate.get(),
            },
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
        decode
            .push(AudioChunk::new(
                AudioChunkInfo {
                    spec,
                    frames: OUTGOING_FRAMES,
                    ..Default::default()
                },
                sample_buffer(
                    &pools,
                    &vec![
                        0.25;
                        usize::try_from(OUTGOING_FRAMES)
                            .expect("test frames fit usize")
                            .saturating_mul(usize::from(spec.channels))
                    ],
                ),
            ))
            .expect("valid outgoing PCM enters holdback");

        assert!(decode.transition_holds_output());
        assert!(
            decode.outgoing_holdback_needs_pcm(),
            "PCM that covers the join only from queue-front zero does not cover exact cut {CUT}"
        );
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
        active.stage(make_chunk(0));
        let mut incoming = generation(spec);
        incoming.stage(make_chunk(LANDED));
        let mut decode = active_decode(&pools, active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
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
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });

        assert_eq!(
            decode.prime_incoming(OutgoingFrontier::Awaiting),
            IncomingPrime::Failed
        );
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
        decode.active.stage(AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames,
                ..Default::default()
            },
            sample_buffer(&pools, &active_samples),
        ));
        decode
            .blender
            .prepare_active(BlenderProfile::new(spec))
            .expect("join scratch fits test pools");
        assert!(decode.blender.prepare_join(|outgoing| {
            outgoing.copy_from_slice(&outgoing_samples);
            true
        }));
        decode.blender.commit_join();
        decode.incoming = Some(IncomingDecode::Priming {
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Exact {
                frame: 0,
                rate: spec.sample_rate.get(),
            },
        });
        let mut cursor = ResumeCursor::default();

        assert!(
            !decode.transition_holds_output(),
            "the follow-up transition must not freeze an active join"
        );
        let output = decode
            .next_output(&mut cursor)
            .expect("active join remains decodable");

        assert!(output.is_some(), "the prior join must keep consuming PCM");
        assert!(decode.blender.is_steady(), "the prior join must finish");
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
        decode.active.stage(AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: 64,
                ..Default::default()
            },
            sample_buffer(&pools, &decode_quarter[..64 * usize::from(spec.channels)]),
        ));
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
            .next_output(&mut cursor)
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
        fn duration(&self) -> Option<Duration> {
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
