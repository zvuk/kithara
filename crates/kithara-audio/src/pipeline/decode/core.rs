use std::{
    any::Any,
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_decode::{
    BlenderProfile, ChunkRetire, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome,
    DecoderFactory as BackendDecoderFactory, DecoderSeekOutcome, GaplessMode, PcmChunk,
};
use kithara_events::{DeferredBus, Event};
use kithara_platform::sync::Arc;
use kithara_stream::{
    ByteMap, MediaInfo, OpenedReader, PlayheadWrite, ReaderProfile, SeekObserve, StreamType,
};
use kithara_test_utils::kithara;
use tracing::{debug, warn};

#[cfg(test)]
use crate::pipeline::decode::transition::OutgoingFrontier;
use crate::pipeline::{
    blend::PcmBlender,
    decode::{
        drain::EofDrain,
        generation::{DecoderGeneration, StageFailure, StageOutput, StageResult},
        resume::ResumeCursor,
        transition::IncomingDecode,
    },
    fetch::Fetch,
    rebuild::RecreateState,
    seek::{ResumeState, SeekEngine, emit::commit_outcome},
    stream::shared::SharedStream,
    track::{TrackFailure, WaitingReason},
};

type DecoderBuilder =
    dyn Fn(OpenedReader, MediaInfo) -> Result<Box<dyn Decoder>, DecodeError> + Send + Sync;

/// Decoder construction and reader-profile policy for one configured track.
#[derive(Clone)]
pub(crate) struct DecoderFactory {
    builder: Arc<DecoderBuilder>,
    configured_media_info: Option<MediaInfo>,
}

impl DecoderFactory {
    pub(crate) fn new(
        builder: impl Fn(OpenedReader, MediaInfo) -> Result<Box<dyn Decoder>, DecodeError>
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
        media_info: MediaInfo,
    ) -> Result<Box<dyn Decoder>, DecodeError> {
        (self.builder)(reader, media_info)
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

/// Decoder construction state shared by initial installation and later rebuilds.
pub(crate) struct DecodeInit {
    pub(crate) playback_resampler_backend: &'static str,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) decoder_backend: kithara_decode::DecoderBackend,
    pub(crate) decoder_factory: DecoderFactory,
    pub(crate) gapless_mode: GaplessMode,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) recreate_on_host_rate_change: bool,
}

pub(crate) struct DecodeParts {
    pub(crate) playback_resampler_backend: &'static str,
    pub(crate) active: ActiveDecode,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) decoder_backend: kithara_decode::DecoderBackend,
    pub(crate) factory: DecoderFactory,
    pub(crate) recreate_on_host_rate_change: bool,
    pub(crate) decoder_host_sample_rate: u32,
}

impl DecodeInit {
    pub(crate) fn decoder_host_sample_rate(&self) -> u32 {
        self.host_sample_rate.load(Ordering::Acquire)
    }

    pub(crate) fn into_parts(self, installed_at_seek_epoch: u64) -> DecodeParts {
        let decoder_host_sample_rate = self.decoder_host_sample_rate();
        let Self {
            decoder,
            decoder_factory,
            decoder_backend,
            gapless_mode,
            host_sample_rate,
            media_info,
            playback_resampler_backend,
            recreate_on_host_rate_change,
        } = self;
        let active = DecoderGeneration::new(
            decoder,
            media_info,
            0,
            installed_at_seek_epoch,
            None,
            gapless_mode,
        );
        DecodeParts {
            host_sample_rate,
            recreate_on_host_rate_change,
            decoder_host_sample_rate,
            decoder_backend,
            playback_resampler_backend,
            active: ActiveDecode::new(active, gapless_mode),
            factory: decoder_factory,
        }
    }
}

#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct ActiveDecode {
    #[field(get, vis = "pub(crate)")]
    pub(super) active: DecoderGeneration,
    pub(super) incoming: Option<IncomingDecode>,
    pub(super) blender: PcmBlender,
    drain: EofDrain,
    #[field(get, vis = "pub(crate)", copy)]
    gapless_mode: GaplessMode,
    rejected_chunk: Option<PcmChunk>,
    stage_error: Option<DecodeError>,
}

pub(crate) struct DecodeCtx<'a, T: StreamType> {
    pub(crate) cursor: &'a mut ResumeCursor,
    pub(crate) seek: &'a SeekEngine,
    pub(crate) stream: &'a SharedStream<T>,
    pub(crate) playhead: &'a dyn PlayheadWrite,
    pub(crate) seek_observe: &'a dyn SeekObserve,
    pub(crate) emit: Option<&'a DeferredBus<Event>>,
    pub(crate) resume: Option<&'a mut ResumeState>,
}

pub(crate) enum DecodeAction {
    Produced(Fetch<PcmChunk>),
    Pending(WaitingReason),
    TransitionPending,
    StartRecreate(RecreateState),
    SeekInterrupted,
    Eof,
    Failed(TrackFailure),
}

impl ActiveDecode {
    fn new(active: DecoderGeneration, gapless_mode: GaplessMode) -> Self {
        let drain = EofDrain::new();
        let blender = PcmBlender::new(active.blender_profile());
        Self {
            active,
            gapless_mode,
            blender,
            drain,
            incoming: None,
            rejected_chunk: None,
            stage_error: None,
        }
    }

    pub(crate) fn flush_reader_signals(&mut self) {
        self.active.decoder_mut().flush_reader_signals();
        self.flush_incoming_reader_signals();
    }

    #[kithara::rtsan_allow_blocking]
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
                debug!(error_class = ?error.classify(), chunks, samples, pos = stream_position, "decoder returned error");
            }
            Ok(DecoderChunkOutcome::Chunk(_) | DecoderChunkOutcome::Pending(_)) => {}
        }
        outcome
    }

    pub(crate) fn next_output(
        &mut self,
        cursor: &mut ResumeCursor,
        epoch: u64,
    ) -> DecodeResult<Option<PcmChunk>> {
        self.next_output_inner(cursor, epoch, true)
    }

    pub(crate) fn next_output_unheld(
        &mut self,
        cursor: &mut ResumeCursor,
        epoch: u64,
    ) -> DecodeResult<Option<PcmChunk>> {
        self.next_output_inner(cursor, epoch, false)
    }

    fn next_output_inner(
        &mut self,
        cursor: &mut ResumeCursor,
        epoch: u64,
        allow_holdback: bool,
    ) -> DecodeResult<Option<PcmChunk>> {
        if allow_holdback && self.transition_holds_output() {
            return Ok(None);
        }
        let holdback =
            allow_holdback && !self.active.is_finished() && self.outgoing_holdback_is_active();
        let next = if holdback {
            match self.active.next_with_holdback() {
                StageOutput::Output(chunk) => chunk,
                StageOutput::Invalid(failure) => return Err(self.reject_stage(failure)),
            }
        } else {
            self.active.next()
        };
        let Some(chunk) = next else {
            return Ok(None);
        };
        cursor.record(&chunk, epoch);
        Ok(Some(self.blender.process_active(chunk)))
    }

    fn outgoing_holdback_is_active(&self) -> bool {
        let Some(IncomingDecode::Priming { generation, .. }) = self.incoming.as_ref() else {
            return false;
        };
        self.blender.is_steady()
            && self.active.blender_profile().spec() == generation.blender_profile().spec()
    }

    delegate::delegate! {
        to self.active {
            #[call(finish)]
            pub(crate) fn finish_active(&mut self);
            pub(crate) fn notify_seek(&mut self, retire: &dyn ChunkRetire);
        }
        to self.drain {
            pub(crate) const fn stats(&self) -> (u64, u64);
            pub(crate) fn track(
                &mut self,
                chunk: &PcmChunk,
                playhead: &dyn PlayheadWrite,
                emit: Option<&DeferredBus<Event>>,
            );
        }
    }

    pub(crate) fn push(&mut self, chunk: PcmChunk) -> DecodeResult<()> {
        if !self.outgoing_holdback_is_active() {
            self.active.push(chunk);
            return Ok(());
        }
        match self.active.push_holdback(chunk) {
            StageResult::Ready | StageResult::NeedMore => Ok(()),
            StageResult::Invalid(failure) => Err(self.reject_stage(failure)),
        }
    }

    pub(crate) fn prepare_incoming_profile(&mut self, profile: BlenderProfile) {
        self.blender.prepare_active(profile);
        if self.active.blender_profile().spec() != profile.spec() {
            return;
        }
        let result = self
            .active
            .prepare_holdback(profile.spec(), self.blender.join_frame_count());
        if let StageResult::Invalid(failure) = result {
            let error = self.reject_stage(failure);
            self.stage_error = Some(error);
        }
    }

    pub(crate) fn prepare_replacement_profile(&mut self, profile: BlenderProfile) {
        self.blender.prepare_active(profile);
    }

    pub(crate) fn take_rejected_chunk(&mut self) -> Option<PcmChunk> {
        self.rejected_chunk.take()
    }

    pub(crate) fn take_stage_error(&mut self) -> Option<DecodeError> {
        self.stage_error.take()
    }

    fn reject_stage(&mut self, failure: StageFailure) -> DecodeError {
        let StageFailure { chunk, error } = failure;
        debug_assert!(self.rejected_chunk.is_none());
        self.rejected_chunk = Some(chunk);
        error
    }

    pub(crate) fn replace_active(&mut self, active: DecoderGeneration) -> DecoderGeneration {
        self.blender.replace_active(active.blender_profile());
        mem::replace(&mut self.active, active)
    }

    pub(crate) fn reset(&mut self) {
        self.stage_error = None;
        self.blender.reset();
    }

    #[kithara::rtsan_allow_blocking]
    pub(crate) fn seek<T: StreamType>(
        &mut self,
        stream: &SharedStream<T>,
        playhead: &dyn PlayheadWrite,
        position: kithara_platform::time::Duration,
    ) -> DecodeResult<DecoderSeekOutcome> {
        let before = stream.position();
        let outcome = match catch_unwind(AssertUnwindSafe(|| {
            self.active.decoder_mut().seek(position)
        })) {
            Ok(result) => result,
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during seek");
                return Err(DecodeError::InvalidData {
                    detail: "decoder panicked during seek",
                });
            }
        };
        if let Ok(ref outcome) = outcome {
            commit_outcome(&self.active, stream, playhead, outcome);
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

    pub(crate) fn update_len(&self, len: u64) {
        self.active.decoder().update_byte_len(len);
    }
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
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
    use kithara_bufpool::PcmPool;
    use kithara_decode::{BlenderProfile, DecoderSeekOutcome, PcmMeta, PcmSpec};
    use kithara_platform::time::Duration;
    use kithara_stream::{
        AudioCodec, ContainerFormat, PendingReason, PrerollHint, ReaderInput, VariantTransition,
        VariantTransitionId,
    };

    use super::*;
    use crate::pipeline::decode::transition::IncomingPrime;

    #[kithara::test]
    fn configured_container_selects_the_incoming_reader_profile() {
        let factory = DecoderFactory::new(
            |_reader, _media_info| -> Result<Box<dyn Decoder>, DecodeError> {
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
    fn steady_output_bypasses_transition_staging() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut decode = ActiveDecode::new(generation(spec), GaplessMode::Disabled);
        let initial_capacity = decode.active().staged_capacity();
        let mut cursor = ResumeCursor::new(
            Arc::new(AtomicU32::new(spec.sample_rate.get())),
            false,
            spec.sample_rate.get(),
        );
        decode
            .push(PcmChunk::new(
                PcmMeta {
                    spec,
                    frames: 64,
                    ..Default::default()
                },
                PcmPool::default().attach(vec![0.25; 64 * usize::from(spec.channels)]),
            ))
            .expect("steady fixture PCM is valid");

        assert_eq!(decode.active().staged_capacity(), initial_capacity);
        assert!(decode.active().staged_span().is_none());
        assert!(
            decode
                .next_output(&mut cursor, 0)
                .expect("steady output remains valid")
                .is_some()
        );
        assert_eq!(decode.active().staged_capacity(), initial_capacity);
    }

    #[kithara::test]
    fn invalid_holdback_pcm_is_retained_for_shell_retirement() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let active = generation(spec);
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = ActiveDecode::new(active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
        let make_chunk = |offset| {
            PcmChunk::new(
                PcmMeta {
                    spec,
                    frame_offset: offset,
                    frames: 4,
                    ..Default::default()
                },
                PcmPool::default().attach(vec![0.25; 4 * usize::from(spec.channels)]),
            )
        };
        decode
            .push(make_chunk(0))
            .expect("first holdback chunk is valid");

        assert!(decode.push(make_chunk(5)).is_err());
        let rejected = decode
            .take_rejected_chunk()
            .expect("invalid pooled PCM remains owned until the shell retires it");
        assert_eq!(rejected.meta.frame_offset, 5);
        assert_eq!(
            decode.active().staged_span().map(|(_, end, _)| end),
            Some(4)
        );
    }

    #[kithara::test]
    fn reset_clears_prepare_error_but_retains_rejected_pcm_for_shell_retirement() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let mut active = generation(spec);
        active.stage(PcmChunk::new(
            PcmMeta {
                spec,
                frames: 4,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.25; 4 * usize::from(spec.channels)]),
        ));
        let rejected = PcmChunk::new(
            PcmMeta {
                spec,
                frame_offset: 5,
                frames: 4,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.5; 4 * usize::from(spec.channels)]),
        );
        let rejected_samples = rejected.samples.as_ptr();
        active.stage(rejected);

        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = ActiveDecode::new(active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });

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
        let rejected = decode
            .take_rejected_chunk()
            .expect("reset must retain rejected PCM for shell retirement");
        assert_eq!(rejected.samples.as_ptr(), rejected_samples);
        assert_eq!(rejected.meta.frame_offset, 5);
        assert!(decode.take_rejected_chunk().is_none());
    }

    #[kithara::test]
    fn unheld_output_drains_pcm_parked_for_a_priming_join() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let active = generation(spec);
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = ActiveDecode::new(active, GaplessMode::Disabled);
        decode.incoming = Some(IncomingDecode::Priming {
            transition,
            generation: incoming,
            frontier: OutgoingFrontier::Awaiting,
        });
        decode.prepare_incoming_profile(BlenderProfile::new(spec));
        let samples = vec![0.25; 128 * usize::from(spec.channels)];
        decode
            .push(PcmChunk::new(
                PcmMeta {
                    spec,
                    frames: 128,
                    ..Default::default()
                },
                PcmPool::default().attach(samples),
            ))
            .expect("valid fixture PCM enters prepared holdback");
        let mut cursor = ResumeCursor::new(
            Arc::new(AtomicU32::new(spec.sample_rate.get())),
            false,
            spec.sample_rate.get(),
        );

        assert!(
            decode
                .next_output(&mut cursor, 0)
                .expect("held output remains valid")
                .is_none()
        );
        let output = decode
            .next_output_unheld(&mut cursor, 0)
            .expect("unheld output remains valid")
            .expect("EOF drain must release the held outgoing tail");
        assert_eq!(output.meta.frames, 128);
        assert!(
            decode
                .next_output_unheld(&mut cursor, 0)
                .expect("drained output remains valid")
                .is_none()
        );
    }

    #[kithara::test]
    fn holdback_coverage_is_measured_from_the_exact_promotion_cut() {
        const CUT: u64 = 256;
        const OUTGOING_FRAMES: u32 = 1_000;
        const INCOMING_FRAMES: u32 = 1_500;

        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut incoming = generation(spec);
        incoming.stage(PcmChunk::new(
            PcmMeta {
                spec,
                frames: INCOMING_FRAMES,
                ..Default::default()
            },
            PcmPool::default().attach(vec![
                0.5;
                usize::try_from(INCOMING_FRAMES)
                    .expect("test frames fit usize")
                    .saturating_mul(usize::from(spec.channels))
            ]),
        ));
        let mut decode = ActiveDecode::new(generation(spec), GaplessMode::Disabled);
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
            .push(PcmChunk::new(
                PcmMeta {
                    spec,
                    frames: OUTGOING_FRAMES,
                    ..Default::default()
                },
                PcmPool::default().attach(vec![
                    0.25;
                    usize::try_from(OUTGOING_FRAMES)
                        .expect("test frames fit usize")
                        .saturating_mul(usize::from(spec.channels))
                ]),
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
        const FRAMES: u32 = 1_500;

        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let make_chunk = |offset| {
            PcmChunk::new(
                PcmMeta {
                    spec,
                    frame_offset: offset,
                    frames: FRAMES,
                    ..Default::default()
                },
                PcmPool::default().attach(vec![
                    0.25;
                    usize::try_from(FRAMES)
                        .expect("test frames fit usize")
                        .saturating_mul(usize::from(spec.channels))
                ]),
            )
        };
        let mut active = generation(spec);
        active.stage(make_chunk(0));
        let mut incoming = generation(spec);
        incoming.stage(make_chunk(LANDED));
        let mut decode = ActiveDecode::new(active, GaplessMode::Disabled);
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
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let incoming = DecoderGeneration::new(
            Box::new(TerminalDecoder::new(spec, TerminalOutcome::VariantChange)),
            None,
            0,
            0,
            None,
            GaplessMode::Disabled,
        );
        let mut decode = ActiveDecode::new(generation(spec), GaplessMode::Disabled);
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
    fn active_join_drains_before_a_latched_follow_up_transition_holds_output() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let incoming = generation(spec);
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = ActiveDecode::new(generation(spec), GaplessMode::Disabled);
        let frames = u32::try_from(decode.blender.join_frame_count()).expect("test join fits u32");
        let samples = usize::try_from(frames)
            .expect("test join fits usize")
            .saturating_mul(usize::from(spec.channels));
        decode.active.stage(PcmChunk::new(
            PcmMeta {
                spec,
                frames,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.25; samples]),
        ));
        decode.blender.prepare_active(BlenderProfile::new(spec));
        assert!(decode.blender.prepare_join(|outgoing| {
            outgoing.fill(-0.25);
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
        let mut cursor = ResumeCursor::new(
            Arc::new(AtomicU32::new(spec.sample_rate.get())),
            false,
            spec.sample_rate.get(),
        );

        assert!(
            !decode.transition_holds_output(),
            "the follow-up transition must not freeze an active join"
        );
        let output = decode
            .next_output(&mut cursor, 0)
            .expect("active join remains decodable");

        assert!(output.is_some(), "the prior join must keep consuming PCM");
        assert!(decode.blender.is_steady(), "the prior join must finish");
    }

    #[kithara::test]
    fn latched_incoming_preparation_keeps_outgoing_pcm_running() {
        let spec = PcmSpec::new(2, NonZeroU32::new(44_100).expect("test rate"));
        let abr = AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0))));
        abr.request_target(VariantIndex::new(1), AbrReason::ManualOverride);
        let claim = abr
            .claim_pending_decision(VariantIndex::new(0))
            .expect("test transition claim");
        let transition = VariantTransition::new(
            VariantTransitionId::new(claim.ticket(), 0),
            VariantIndex::new(0),
            VariantIndex::new(1),
        );
        let mut decode = ActiveDecode::new(generation(spec), GaplessMode::Disabled);
        decode.active.stage(PcmChunk::new(
            PcmMeta {
                spec,
                frames: 64,
                ..Default::default()
            },
            PcmPool::default().attach(vec![0.25; 64 * usize::from(spec.channels)]),
        ));
        decode.incoming = Some(IncomingDecode::Preparing {
            transition,
            frontier: OutgoingFrontier::Exact {
                frame: 0,
                rate: spec.sample_rate.get(),
            },
        });
        let mut cursor = ResumeCursor::new(
            Arc::new(AtomicU32::new(spec.sample_rate.get())),
            false,
            spec.sample_rate.get(),
        );

        assert!(!decode.transition_holds_output());
        let output = decode
            .next_output(&mut cursor, 0)
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
        outcome: TerminalOutcome,
        spec: PcmSpec,
    }

    impl TerminalDecoder {
        const fn new(spec: PcmSpec, outcome: TerminalOutcome) -> Self {
            Self { outcome, spec }
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

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    fn generation(spec: PcmSpec) -> DecoderGeneration {
        DecoderGeneration::new(
            Box::new(TerminalDecoder::new(spec, TerminalOutcome::Eof)),
            None,
            0,
            0,
            None,
            GaplessMode::Disabled,
        )
    }
}
