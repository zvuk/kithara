#![allow(unsafe_code)]

use std::{
    cell::Cell,
    fmt,
    io::Cursor,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use kithara_bufpool::{PcmBuf, PcmPool, pcm_pool};
use kithara_stream::{AudioCodec, StreamContext};
use tracing::{debug, info};

use super::{
    codec::{AndroidPcmEncoding, DequeueOutput, OutputBuffer, OwnedCodec, bootstrap_codec},
    config::AndroidConfig,
    ensure_current_thread_attached,
    error::AndroidBackendError,
    extractor::{OwnedExtractor, bootstrap_extractor},
    ffi,
    format::{
        SeekTrimOutcome, copy_pcm_float_to_pool, decode_pcm16_to_f32, pcm_meta_from_timestamp,
        seek_trim_for_buffer, supports_codec_at_api,
    },
    source::OwnedMediaDataSource,
};
use crate::{
    DecodeResult,
    hardware::{BoxedSource, RecoverableHardwareError, recoverable_hardware_error},
    traits::{Aac, CodecType, Flac, InnerDecoder, Mp3},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

struct AndroidInner {
    media_source: OwnedMediaDataSource,
    extractor: OwnedExtractor,
    codec: OwnedCodec,
    selected_track: usize,
    spec: PcmSpec,
    duration: Option<Duration>,
    metadata: TrackMetadata,
    byte_len_handle: Arc<AtomicU64>,
    pool: PcmPool,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    epoch: u64,
    input_eos: bool,
    output_eos: bool,
    pending_seek_target_us: Option<i64>,
    owner_thread: Cell<Option<thread::ThreadId>>,
    pcm_encoding: AndroidPcmEncoding,
}

pub(crate) struct Android<C: CodecType> {
    inner: AndroidInner,
    _codec: PhantomData<C>,
}

pub(crate) type AndroidAac = Android<Aac>;
pub(crate) type AndroidMp3 = Android<Mp3>;
pub(crate) type AndroidFlac = Android<Flac>;

impl<C: CodecType> fmt::Debug for Android<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Android")
            .field("spec", &self.inner.spec)
            .field("selected_track", &self.inner.selected_track)
            .finish_non_exhaustive()
    }
}

impl<C: CodecType> Android<C> {
    const INPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const OUTPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;

    fn bootstrap(
        source: BoxedSource,
        config: AndroidConfig,
    ) -> Result<Self, RecoverableHardwareError> {
        if let Err(error) = ensure_current_thread_attached() {
            return Err(recoverable_hardware_error(
                source,
                error.into_decode_error(),
            ));
        }

        let api_level = ffi::current_api_level();
        if !supports_codec_at_api(C::CODEC, api_level) {
            return Err(recoverable_hardware_error(
                source,
                AndroidBackendError::api_level_unavailable(api_level).into_decode_error(),
            ));
        }

        let byte_len_handle = config
            .byte_len_handle
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicU64::new(0)));
        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| pcm_pool().clone());

        let media_source = match OwnedMediaDataSource::new(source, Arc::clone(&byte_len_handle)) {
            Ok(media_source) => media_source,
            Err((source, error)) => {
                return Err(recoverable_hardware_error(
                    source,
                    error.into_decode_error(),
                ));
            }
        };

        let extractor_bootstrap = match bootstrap_extractor(media_source, C::CODEC) {
            Ok(bootstrap) => bootstrap,
            Err((media_source, error)) => {
                return Err(recover_media_source_failure(media_source, error));
            }
        };

        // Bootstrap is staged so each step can hand ownership back to the
        // Symphonia fallback if Android setup fails partway through.
        let codec_bootstrap = match bootstrap_codec(
            &extractor_bootstrap.extractor,
            &extractor_bootstrap.selected_track,
        ) {
            Ok(bootstrap) => bootstrap,
            Err(error) => return Err(recover_codec_failure(extractor_bootstrap, error)),
        };

        Ok(Self {
            inner: AndroidInner {
                media_source: extractor_bootstrap.media_source,
                extractor: extractor_bootstrap.extractor,
                codec: codec_bootstrap.codec,
                selected_track: extractor_bootstrap.selected_track.index,
                spec: codec_bootstrap.spec,
                duration: extractor_bootstrap.selected_track.duration,
                metadata: TrackMetadata::default(),
                byte_len_handle,
                pool,
                stream_ctx: config.stream_ctx,
                epoch: config.epoch,
                input_eos: false,
                output_eos: false,
                pending_seek_target_us: None,
                owner_thread: Cell::new(None),
                pcm_encoding: codec_bootstrap.pcm_encoding,
            },
            _codec: PhantomData,
        })
    }

    fn assert_owner_thread(&self) -> Result<(), AndroidBackendError> {
        let current = thread::current().id();
        if let Some(owner) = self.inner.owner_thread.get() {
            if owner != current {
                return Err(AndroidBackendError::operation(
                    "thread-affinity",
                    "android decoder must be used from its owner thread",
                ));
            }
        } else {
            self.inner.owner_thread.set(Some(current));
        }

        Ok(())
    }

    fn try_queue_input(&mut self) -> Result<bool, AndroidBackendError> {
        if self.inner.input_eos {
            return Ok(false);
        }

        let Some(input) = self
            .inner
            .codec
            .dequeue_input_buffer(Self::INPUT_DEQUEUE_TIMEOUT_US)?
        else {
            return Ok(false);
        };
        let mut input = input;
        let input_index = input.index;
        let sample = self
            .inner
            .extractor
            .read_sample(input.data_mut(), self.inner.selected_track)?;

        match sample {
            Some(sample) => {
                let sample_len = sample.bytes.len();
                let presentation_time_us = sample.presentation_time_us;
                self.inner.codec.queue_input_buffer(
                    input_index,
                    sample_len,
                    presentation_time_us,
                    0,
                )?;
                let _ = self.inner.extractor.advance()?;
            }
            None => {
                debug!("Queued Android MediaCodec end-of-stream input buffer");
                self.inner.codec.queue_end_of_stream(input_index)?;
                self.inner.input_eos = true;
            }
        }

        Ok(true)
    }

    fn try_drain_output(
        &mut self,
        timeout_us: i64,
    ) -> Result<Option<Option<PcmChunk>>, AndroidBackendError> {
        match self.inner.codec.dequeue_output_buffer(timeout_us)? {
            DequeueOutput::TryAgainLater => Ok(None),
            DequeueOutput::OutputFormatChanged(format) => {
                info!(
                    sample_rate = format.spec.sample_rate,
                    channels = format.spec.channels,
                    pcm_encoding = ?format.pcm_encoding,
                    "Android MediaCodec output format changed"
                );
                self.inner.spec = format.spec;
                self.inner.pcm_encoding = format.pcm_encoding;
                Ok(Some(None))
            }
            DequeueOutput::Output(output) => {
                let eos = (output.flags & ffi::MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM) != 0;
                let buffer_index = output.index;
                let decoded = self.decode_output_chunk(&output);
                let release = self.inner.codec.release_output_buffer(buffer_index);
                if eos {
                    debug!("Android MediaCodec signalled output end-of-stream");
                    self.inner.output_eos = true;
                }
                release?;
                decoded.map(Some)
            }
        }
    }

    fn decode_output_chunk(
        &mut self,
        output: &OutputBuffer,
    ) -> Result<Option<PcmChunk>, AndroidBackendError> {
        let output_data = output.data();
        let total_frames = self.frame_count_for_payload(output_data.len())?;
        if total_frames == 0 {
            return Ok(None);
        }

        let presentation_time_us = if let Some(target_us) = self.inner.pending_seek_target_us {
            return match seek_trim_for_buffer(
                target_us,
                output.presentation_time_us,
                total_frames,
                self.inner.spec.sample_rate,
            ) {
                SeekTrimOutcome::Drop => Ok(None),
                SeekTrimOutcome::Emit(trim) => {
                    let pcm = self.convert_output_payload(output_data, trim.trim_frames)?;
                    self.inner.pending_seek_target_us = None;
                    self.build_chunk(trim.output_timestamp_us, pcm)
                }
            };
        } else {
            output.presentation_time_us
        };

        let pcm = self.convert_output_payload(output_data, 0)?;
        self.build_chunk(presentation_time_us, pcm)
    }

    fn convert_output_payload(
        &self,
        bytes: &[u8],
        trim_frames: usize,
    ) -> Result<PcmBuf, AndroidBackendError> {
        let result = match self.inner.pcm_encoding {
            AndroidPcmEncoding::Pcm16 => decode_pcm16_to_f32(
                &self.inner.pool,
                bytes,
                trim_frames,
                self.inner.spec.channels,
            ),
            AndroidPcmEncoding::Float => copy_pcm_float_to_pool(
                &self.inner.pool,
                bytes,
                trim_frames,
                self.inner.spec.channels,
            ),
        };
        result.map_err(|error| AndroidBackendError::operation("pcm-conversion", error.to_string()))
    }

    fn build_chunk(
        &mut self,
        presentation_time_us: i64,
        pcm: PcmBuf,
    ) -> Result<Option<PcmChunk>, AndroidBackendError> {
        if pcm.is_empty() {
            return Ok(None);
        }

        let meta = pcm_meta_from_timestamp(
            self.inner.spec,
            presentation_time_us,
            self.inner.stream_ctx.as_deref(),
            self.inner.epoch,
        )
        .map_err(|error| AndroidBackendError::operation("pcm-meta", error.to_string()))?;

        Ok(Some(PcmChunk::new(meta, pcm)))
    }

    fn frame_count_for_payload(&self, bytes_len: usize) -> Result<usize, AndroidBackendError> {
        let channels = usize::from(self.inner.spec.channels);
        if channels == 0 {
            return Err(AndroidBackendError::operation(
                "pcm-frame-count",
                "output format reported zero channels",
            ));
        }

        const BYTES_PER_PCM16_SAMPLE: usize = 2;
        const BYTES_PER_FLOAT_SAMPLE: usize = 4;
        let bytes_per_sample = match self.inner.pcm_encoding {
            AndroidPcmEncoding::Pcm16 => BYTES_PER_PCM16_SAMPLE,
            AndroidPcmEncoding::Float => BYTES_PER_FLOAT_SAMPLE,
        };
        if bytes_len % bytes_per_sample != 0 {
            return Err(AndroidBackendError::operation(
                "pcm-frame-count",
                format!(
                    "payload length {bytes_len} is not aligned to sample size {bytes_per_sample}"
                ),
            ));
        }

        let sample_count = bytes_len / bytes_per_sample;
        if sample_count % channels != 0 {
            return Err(AndroidBackendError::operation(
                "pcm-frame-count",
                format!("sample count {sample_count} is not aligned to {channels} channels"),
            ));
        }

        Ok(sample_count / channels)
    }
}

impl<C: CodecType> InnerDecoder for Android<C> {
    fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.assert_owner_thread()
            .map_err(AndroidBackendError::into_decode_error)?;
        let _ = &self.inner.media_source;

        loop {
            if let Some(chunk_opt) = self
                .try_drain_output(0)
                .map_err(AndroidBackendError::into_decode_error)?
            {
                if let Some(chunk) = chunk_opt {
                    return Ok(Some(chunk));
                }
                if self.inner.output_eos {
                    return Ok(None);
                }
                continue;
            }
            if self.inner.output_eos {
                return Ok(None);
            }

            let queued_input = self
                .try_queue_input()
                .map_err(AndroidBackendError::into_decode_error)?;

            if let Some(chunk_opt) = self
                .try_drain_output(Self::OUTPUT_DEQUEUE_TIMEOUT_US)
                .map_err(AndroidBackendError::into_decode_error)?
            {
                if let Some(chunk) = chunk_opt {
                    return Ok(Some(chunk));
                }
                if self.inner.output_eos {
                    return Ok(None);
                }
                continue;
            }
            if self.inner.output_eos {
                return Ok(None);
            }

            if !queued_input && !self.inner.input_eos {
                continue;
            }
        }
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        self.assert_owner_thread()
            .map_err(AndroidBackendError::into_decode_error)?;

        let target_us = i64::try_from(pos.as_micros()).map_err(|_| {
            AndroidBackendError::operation("seek", "target position does not fit in i64")
                .into_decode_error()
        })?;
        self.inner
            .extractor
            .seek_to(target_us)
            .map_err(AndroidBackendError::into_decode_error)?;
        // A seek invalidates any queued codec output from the old position, so
        // the codec must be flushed before decode resumes.
        self.inner
            .codec
            .flush()
            .map_err(AndroidBackendError::into_decode_error)?;
        self.inner.input_eos = false;
        self.inner.output_eos = false;
        self.inner.pending_seek_target_us = Some(target_us);
        debug!(target_us, "Android MediaCodec decoder seek requested");
        Ok(())
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.byte_len_handle.store(len, Ordering::Release);
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata.clone()
    }
}

pub(crate) fn try_create_android_decoder(
    codec: AudioCodec,
    source: BoxedSource,
    config: AndroidConfig,
) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
    let _ = config.container;
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            AndroidAac::bootstrap(source, config)
                .map(|decoder| Box::new(decoder) as Box<dyn InnerDecoder>)
        }
        AudioCodec::Mp3 => AndroidMp3::bootstrap(source, config)
            .map(|decoder| Box::new(decoder) as Box<dyn InnerDecoder>),
        AudioCodec::Flac => AndroidFlac::bootstrap(source, config)
            .map(|decoder| Box::new(decoder) as Box<dyn InnerDecoder>),
        unsupported => Err(recoverable_hardware_error(
            source,
            AndroidBackendError::UnsupportedCodec { codec: unsupported }.into_decode_error(),
        )),
    }
}

fn recover_codec_failure(
    bootstrap: super::extractor::ExtractorBootstrap,
    error: AndroidBackendError,
) -> RecoverableHardwareError {
    drop(bootstrap.extractor);
    recover_media_source_failure(bootstrap.media_source, error)
}

fn recover_media_source_failure(
    media_source: OwnedMediaDataSource,
    error: AndroidBackendError,
) -> RecoverableHardwareError {
    match media_source.into_source() {
        Ok(source) => recoverable_hardware_error(source, error.into_decode_error()),
        Err(recover_error) => RecoverableHardwareError {
            source: Box::new(Cursor::new(Vec::<u8>::new())),
            error: recover_error.into_decode_error(),
        },
    }
}
