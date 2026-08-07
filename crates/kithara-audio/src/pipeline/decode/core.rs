use std::{
    any::Any,
    mem,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicU32, Ordering},
};

use kithara_decode::{
    ChunkRetire, DecodeError, DecodeResult, Decoder, DecoderChunkOutcome, DecoderSeekOutcome,
    GaplessMode, PcmChunk,
};
use kithara_events::{DeferredBus, Event};
use kithara_platform::sync::Arc;
use kithara_stream::{
    ByteMap, MediaInfo, OpenedReader, PlayheadWrite, ReaderProfile, SeekObserve, StreamType,
};
use kithara_test_utils::kithara;
use tracing::{debug, warn};

use crate::{
    pipeline::{
        blend::PcmBlender,
        decode::{
            drain::EofDrain, generation::DecoderGeneration, resume::ResumeCursor,
            transition::IncomingDecode,
        },
        fetch::Fetch,
        rebuild::RecreateState,
        seek::{ResumeState, SeekEngine, emit::commit_outcome},
        stream::shared::SharedStream,
        track::{TrackFailure, WaitingReason},
    },
    renderer::{apply_effects, reset_effects},
    traits::AudioEffect,
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
        kithara_decode::DecoderFactory::reader_profile(&resolved, byte_map)
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

    pub(crate) fn into_parts(
        self,
        effects: Vec<Box<dyn AudioEffect>>,
        installed_at_seek_epoch: u64,
    ) -> DecodeParts {
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
            active: ActiveDecode::new(active, gapless_mode, effects),
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
    effects: Vec<Box<dyn AudioEffect>>,
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
    StartRecreate(RecreateState),
    SeekInterrupted,
    Eof,
    Failed(TrackFailure),
}

impl ActiveDecode {
    fn new(
        active: DecoderGeneration,
        gapless_mode: GaplessMode,
        effects: Vec<Box<dyn AudioEffect>>,
    ) -> Self {
        let drain = EofDrain::new(effects.len());
        let blender = PcmBlender::new(active.blender_profile());
        Self {
            active,
            gapless_mode,
            blender,
            effects,
            drain,
            incoming: None,
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

    pub(crate) fn next_drain(&mut self) -> Option<PcmChunk> {
        self.drain.next(&mut self.effects)
    }

    pub(crate) fn next_output(&mut self) -> Option<PcmChunk> {
        while let Some(chunk) = self.active.next() {
            let chunk = self.blender.process_active(chunk);
            if let Some(output) = apply_effects(&mut self.effects, chunk) {
                return Some(output);
            }
        }
        None
    }

    delegate::delegate! {
        to self.active {
            pub(crate) fn notify_seek(&mut self, retire: &dyn ChunkRetire);
            pub(crate) fn push(&mut self, chunk: PcmChunk);
            #[call(finish)]
            pub(crate) fn set_tail_compensation(&mut self);
        }
        to self.drain {
            pub(crate) fn stats(&self) -> (u64, u64);
            pub(crate) fn track(
                &mut self,
                chunk: &PcmChunk,
                playhead: &dyn PlayheadWrite,
                emit: Option<&DeferredBus<Event>>,
            );
        }
    }

    pub(crate) fn replace_active(&mut self, active: DecoderGeneration) -> DecoderGeneration {
        self.blender.replace_active(active.blender_profile());
        mem::replace(&mut self.active, active)
    }

    pub(crate) fn reset(&mut self) {
        reset_effects(&mut self.effects);
        self.drain.reset();
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
    use kithara_stream::{AudioCodec, ContainerFormat, ReaderInput};

    use super::*;

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
}
