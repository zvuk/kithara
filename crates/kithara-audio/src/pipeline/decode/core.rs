use std::{
    any::Any,
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_bufpool::PcmPool;
use kithara_decode::{
    DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome, GaplessMode,
    PcmChunk, PcmMeta, duration_for_frames,
};
use kithara_events::{DeferredBus, Event};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{MediaInfo, PlayheadWrite, SeekObserve, StreamType};
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use crate::{
    pipeline::{
        decode::{drain::EofDrain, resume::ResumeCursor},
        fetch::Fetch,
        gapless::GaplessStage,
        gapless_blender::GaplessBlender,
        rebuild::RecreateState,
        seek::{ResumeState, SeekEngine, emit::commit_outcome},
        stream::shared::SharedStream,
        track::{TrackFailure, WaitingReason},
    },
    renderer::{apply_effects, reset_effects},
    traits::AudioEffect,
};

/// Decoder and its associated metadata, installed as an atomic unit.
pub(crate) struct DecoderSession {
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) base_offset: u64,
    pub(crate) installed_at_seek_epoch: u64,
    decoder_origin: Duration,
}

/// Factory closure that creates a new decoder from stream, media info, and base offset.
///
/// Production creates a Symphonia decoder via `OffsetReader`; tests may return
/// a mock decoder without real I/O. Interrupted construction remains distinct
/// from a hard decoder or codec error so recreation can wait for source bytes.
pub(crate) type DecoderFactory<T> = Arc<
    dyn Fn(SharedStream<T>, MediaInfo, u64) -> Result<Box<dyn Decoder>, DecodeError> + Send + Sync,
>;

/// Decoder construction state shared by initial installation and later rebuilds.
pub(crate) struct DecodeInit<T: StreamType> {
    pub(crate) decoder: Box<dyn Decoder>,
    pub(crate) decoder_factory: DecoderFactory<T>,
    pub(crate) decoder_backend: kithara_decode::DecoderBackend,
    pub(crate) gapless_mode: GaplessMode,
    pub(crate) pcm_pool: PcmPool,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) media_info: Option<MediaInfo>,
    pub(crate) playback_resampler_backend: &'static str,
    pub(crate) recreate_on_host_rate_change: bool,
}

pub(crate) struct DecodeParts<T: StreamType> {
    pub(crate) core: DecodeCore,
    pub(crate) factory: DecoderFactory<T>,
    pub(crate) host_sample_rate: Arc<AtomicU32>,
    pub(crate) recreate_on_host_rate_change: bool,
    pub(crate) decoder_host_sample_rate: u32,
    pub(crate) decoder_backend: kithara_decode::DecoderBackend,
    pub(crate) playback_resampler_backend: &'static str,
}

impl<T: StreamType> DecodeInit<T> {
    pub(crate) fn build_gapless(&self) -> GaplessStage {
        GaplessStage::build(
            self.decoder.as_ref(),
            self.gapless_mode,
            self.media_info.as_ref(),
        )
    }

    pub(crate) fn decoder_host_sample_rate(&self) -> u32 {
        self.host_sample_rate.load(Ordering::Acquire)
    }

    pub(crate) fn into_parts(
        self,
        effects: Vec<Box<dyn AudioEffect>>,
        installed_at_seek_epoch: u64,
    ) -> DecodeParts<T> {
        let gapless = self.build_gapless();
        let decoder_host_sample_rate = self.decoder_host_sample_rate();
        let Self {
            decoder,
            decoder_factory,
            decoder_backend,
            gapless_mode,
            pcm_pool,
            host_sample_rate,
            media_info,
            playback_resampler_backend,
            recreate_on_host_rate_change,
        } = self;
        DecodeParts {
            core: DecodeCore::new(
                DecoderSession {
                    decoder_origin: decoder_origin(decoder.as_ref()),
                    decoder,
                    base_offset: 0,
                    media_info,
                    installed_at_seek_epoch,
                },
                gapless_mode,
                gapless,
                pcm_pool,
                effects,
            ),
            factory: decoder_factory,
            host_sample_rate,
            recreate_on_host_rate_change,
            decoder_host_sample_rate,
            decoder_backend,
            playback_resampler_backend,
        }
    }
}

pub(crate) struct DecodeCore {
    session: DecoderSession,
    content_origin: Duration,
    gapless_mode: GaplessMode,
    gapless: GaplessStage,
    blender: GaplessBlender,
    effects: Vec<Box<dyn AudioEffect>>,
    drain: EofDrain,
}

pub(crate) struct DecodeCtx<'a, T: StreamType> {
    pub(crate) emit: Option<&'a DeferredBus<Event>>,
    pub(crate) playhead: &'a dyn PlayheadWrite,
    pub(crate) resume: Option<&'a mut ResumeState>,
    pub(crate) cursor: &'a mut ResumeCursor,
    pub(crate) seek: &'a SeekEngine,
    pub(crate) seek_observe: &'a dyn SeekObserve,
    pub(crate) stream: &'a SharedStream<T>,
}

pub(crate) enum DecodeAction {
    Produced(Fetch<PcmChunk>),
    Pending(WaitingReason),
    StartRecreate(RecreateState),
    SeekInterrupted,
    Eof,
    Failed(TrackFailure),
}

impl DecodeCore {
    fn new(
        session: DecoderSession,
        gapless_mode: GaplessMode,
        gapless: GaplessStage,
        pcm_pool: PcmPool,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
        let drain = EofDrain::new(effects.len());
        let blender = GaplessBlender::new(pcm_pool, session.decoder.blend_duration());
        let content_origin = session.decoder_origin;
        Self {
            session,
            content_origin,
            gapless_mode,
            gapless,
            blender,
            effects,
            drain,
        }
    }

    pub(crate) fn session(&self) -> &DecoderSession {
        &self.session
    }

    pub(crate) fn gapless_mode(&self) -> GaplessMode {
        self.gapless_mode
    }

    pub(crate) fn reset(&mut self) {
        reset_effects(&mut self.effects);
        self.drain.reset();
    }

    pub(crate) fn notify_seek(&mut self) {
        self.gapless.notify_seek();
        self.blender.reset();
    }

    pub(crate) fn set_tail_compensation(&mut self) {
        self.gapless
            .set_tail_compensation(self.session.decoder.track_info().gapless_tail);
        self.gapless.flush();
    }

    pub(crate) fn flush_gapless(&mut self) -> DecodeResult<()> {
        self.drain_gapless()?;
        self.blender.flush();
        Ok(())
    }

    pub(crate) fn begin_format_transition(&mut self) -> DecodeResult<Option<Duration>> {
        self.drain_gapless()?;
        self.blender.begin_transition()
    }

    #[must_use]
    pub(crate) fn blend_active(&self) -> bool {
        self.blender.transition_active()
    }

    pub(crate) fn push(&mut self, chunk: PcmChunk) {
        self.gapless.push(chunk);
    }

    pub(crate) fn normalize_timeline(&self, chunk: &mut PcmChunk) {
        normalize_meta(
            &mut chunk.meta,
            self.session.decoder_origin,
            self.content_origin,
        );
    }

    pub(crate) fn track(
        &mut self,
        chunk: &PcmChunk,
        playhead: &dyn PlayheadWrite,
        emit: Option<&DeferredBus<Event>>,
    ) {
        self.drain.track(chunk, playhead, emit);
    }

    pub(crate) fn next_gapless(&mut self) -> DecodeResult<Option<PcmChunk>> {
        loop {
            if let Some(chunk) = self.blender.next()
                && let Some(output) = apply_effects(&mut self.effects, chunk)
            {
                return Ok(Some(output));
            }
            let Some(chunk) = self.gapless.next() else {
                return Ok(None);
            };
            self.blender.push(chunk)?;
        }
    }

    fn drain_gapless(&mut self) -> DecodeResult<()> {
        while let Some(chunk) = self.gapless.next() {
            self.blender.push(chunk)?;
        }
        Ok(())
    }

    pub(crate) fn next_drain(&mut self) -> Option<PcmChunk> {
        self.drain.next(&mut self.effects)
    }

    pub(crate) fn stats(&self) -> (u64, u64) {
        self.drain.stats()
    }

    #[kithara::rtsan_allow_blocking]
    pub(crate) fn next_chunk(&mut self, stream_position: u64) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = match catch_unwind(AssertUnwindSafe(|| self.session.decoder.next_chunk())) {
            Ok(result) => result,
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during next_chunk");
                Err(DecodeError::InvalidData {
                    detail: "decoder panicked during next_chunk",
                })
            }
        };
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

    #[kithara::rtsan_allow_blocking]
    pub(crate) fn seek<T: StreamType>(
        &mut self,
        stream: &SharedStream<T>,
        playhead: &dyn PlayheadWrite,
        position: Duration,
    ) -> DecodeResult<DecoderSeekOutcome> {
        let before = stream.position();
        let decoder_position = shift(position, self.content_origin, self.session.decoder_origin);
        let outcome = match catch_unwind(AssertUnwindSafe(|| {
            self.session.decoder.seek(decoder_position)
        })) {
            Ok(result) => result.map(|outcome| {
                normalize_seek(
                    outcome,
                    self.session.decoder.spec().sample_rate.get(),
                    self.session.decoder_origin,
                    self.content_origin,
                )
            }),
            Err(payload) => {
                warn!(panic = %panic_message(payload), "decoder panicked during seek");
                return Err(DecodeError::InvalidData {
                    detail: "decoder panicked during seek",
                });
            }
        };
        if let Ok(ref outcome) = outcome {
            commit_outcome(&self.session, stream, playhead, outcome);
        }
        debug!(
            ?position,
            ?decoder_position,
            before,
            after = stream.position(),
            ?outcome,
            "decoder seek completed"
        );
        outcome
    }

    pub(crate) fn update_len(&self, len: u64) {
        self.session.decoder.update_byte_len(len);
    }

    pub(crate) fn install(
        &mut self,
        decoder: Box<dyn Decoder>,
        media_info: MediaInfo,
        offset: u64,
        seek_epoch: u64,
    ) -> Box<dyn Decoder> {
        self.blender.set_duration(decoder.blend_duration());
        let mut gapless =
            GaplessStage::build(decoder.as_ref(), self.gapless_mode, Some(&media_info));
        gapless.notify_seek();
        self.gapless = gapless;
        let session = DecoderSession {
            decoder_origin: decoder_origin(&*decoder),
            decoder,
            media_info: Some(media_info),
            base_offset: offset,
            installed_at_seek_epoch: seek_epoch,
        };
        let old = mem::replace(&mut self.session, session);
        old.decoder
    }

    pub(crate) fn flush_reader_signals(&mut self) {
        self.session.decoder.flush_reader_signals();
    }
}

fn decoder_origin(decoder: &dyn Decoder) -> Duration {
    let frames = decoder
        .track_info()
        .gapless
        .map_or(0, |gapless| gapless.leading_frames);
    duration_for_frames(decoder.spec().sample_rate.get(), frames)
}

fn normalize_meta(meta: &mut PcmMeta, decoder_origin: Duration, content_origin: Duration) {
    if decoder_origin == content_origin {
        return;
    }
    let sample_rate = meta.spec.sample_rate.get();
    meta.timestamp = shift(meta.timestamp, decoder_origin, content_origin);
    meta.end_timestamp = shift(meta.end_timestamp, decoder_origin, content_origin);
    meta.frame_offset = shift_frames(
        meta.frame_offset,
        frames_at(sample_rate, decoder_origin),
        frames_at(sample_rate, content_origin),
    );
}

fn normalize_seek(
    outcome: DecoderSeekOutcome,
    sample_rate: u32,
    decoder_origin: Duration,
    content_origin: Duration,
) -> DecoderSeekOutcome {
    if decoder_origin == content_origin {
        return outcome;
    }
    match outcome {
        DecoderSeekOutcome::Landed {
            landed_at,
            landed_frame,
            landed_byte,
            preroll,
        } => DecoderSeekOutcome::Landed {
            landed_at: shift(landed_at, decoder_origin, content_origin),
            landed_frame: shift_frames(
                landed_frame,
                frames_at(sample_rate, decoder_origin),
                frames_at(sample_rate, content_origin),
            ),
            landed_byte,
            preroll,
        },
        DecoderSeekOutcome::PastEof { duration } => DecoderSeekOutcome::PastEof {
            duration: shift(duration, decoder_origin, content_origin),
        },
    }
}

fn shift(value: Duration, from: Duration, to: Duration) -> Duration {
    if to >= from {
        value.saturating_add(to.saturating_sub(from))
    } else {
        value.saturating_sub(from.saturating_sub(to))
    }
}

fn frames_at(sample_rate: u32, at: Duration) -> u64 {
    let seconds = at.as_secs();
    let subsecond = u64::from(at.subsec_nanos())
        .saturating_mul(u64::from(sample_rate))
        .saturating_add(500_000_000)
        / 1_000_000_000;
    seconds
        .saturating_mul(u64::from(sample_rate))
        .saturating_add(subsecond)
}

fn shift_frames(value: u64, from: u64, to: u64) -> u64 {
    if to >= from {
        value.saturating_add(to.saturating_sub(from))
    } else {
        value.saturating_sub(from.saturating_sub(to))
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
