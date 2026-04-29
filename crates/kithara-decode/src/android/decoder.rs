#![allow(unsafe_code)]

use std::{
    cell::Cell,
    fmt,
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
    format::supports_codec_at_api,
    source::OwnedMediaDataSource,
};
use crate::{
    DecodeResult,
    backend::BoxedSource,
    error::DecodeError,
    pcm::{
        conversion::{copy_pcm_float_to_pool, decode_pcm16_to_f32},
        timeline::{SeekTrimOutcome, pcm_meta_from_pts_us, seek_trim_for_buffer},
    },
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmChunk, PcmSpec, TrackMetadata},
};

pub(crate) struct AndroidDecoder {
    pcm_encoding: AndroidPcmEncoding,
    byte_len_handle: Arc<AtomicU64>,
    owner_thread: Cell<Option<thread::ThreadId>>,
    duration: Option<Duration>,
    /// Byte offset of the most recently fed input sample, captured from
    /// `AMediaExtractor_getSampleFormat` via `CompressedSample.byte_offset`.
    /// MediaCodec decouples input and output, so we cache this here to
    /// attach it to the next decoded chunk; AAC/MP3/etc are FIFO so the
    /// pairing is faithful for audio (no reorder).
    last_input_byte_offset: Option<u64>,
    pending_seek_target_us: Option<i64>,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    codec: OwnedCodec,
    extractor: OwnedExtractor,
    media_source: OwnedMediaDataSource,
    pool: PcmPool,
    spec: PcmSpec,
    metadata: TrackMetadata,
    input_eos: bool,
    output_eos: bool,
    epoch: u64,
    selected_track: usize,
}

impl fmt::Debug for AndroidDecoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AndroidDecoder")
            .field("spec", &self.spec)
            .field("selected_track", &self.selected_track)
            .finish_non_exhaustive()
    }
}

impl AndroidDecoder {
    const INPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const OUTPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;

    fn assert_owner_thread(&self) -> Result<(), AndroidBackendError> {
        let current = thread::current().id();
        if let Some(owner) = self.owner_thread.get() {
            if owner != current {
                return Err(AndroidBackendError::operation(
                    "thread-affinity",
                    "android decoder must be used from its owner thread",
                ));
            }
        } else {
            self.owner_thread.set(Some(current));
        }

        Ok(())
    }

    pub(super) fn bootstrap(
        source: BoxedSource,
        config: AndroidConfig,
        codec: AudioCodec,
    ) -> Result<Self, DecodeError> {
        if let Err(error) = ensure_current_thread_attached() {
            return Err(error.into_decode_error());
        }

        let api_level = ffi::current_api_level();
        if !supports_codec_at_api(codec, api_level) {
            return Err(AndroidBackendError::api_level_unavailable(api_level).into_decode_error());
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
            Err((_source, error)) => {
                return Err(error.into_decode_error());
            }
        };

        let extractor_bootstrap = match bootstrap_extractor(media_source, codec) {
            Ok(bootstrap) => bootstrap,
            Err((media_source, error)) => {
                return Err(recover_media_source_failure(media_source, error));
            }
        };

        let codec_bootstrap = match bootstrap_codec(
            &extractor_bootstrap.extractor,
            &extractor_bootstrap.selected_track,
        ) {
            Ok(bootstrap) => bootstrap,
            Err(error) => return Err(recover_codec_failure(extractor_bootstrap, error)),
        };

        Ok(Self {
            byte_len_handle,
            pool,
            media_source: extractor_bootstrap.media_source,
            extractor: extractor_bootstrap.extractor,
            codec: codec_bootstrap.codec,
            selected_track: extractor_bootstrap.selected_track.index,
            spec: codec_bootstrap.spec,
            duration: extractor_bootstrap.selected_track.duration,
            metadata: TrackMetadata::default(),
            stream_ctx: config.stream_ctx,
            epoch: config.epoch,
            input_eos: false,
            output_eos: false,
            pending_seek_target_us: None,
            owner_thread: Cell::new(None),
            pcm_encoding: codec_bootstrap.pcm_encoding,
            last_input_byte_offset: None,
        })
    }

    fn build_chunk(
        &mut self,
        presentation_time_us: i64,
        pcm: PcmBuf,
        frames: usize,
        source_bytes: u64,
    ) -> Result<Option<PcmChunk>, AndroidBackendError> {
        if pcm.is_empty() {
            return Ok(None);
        }

        #[expect(clippy::cast_possible_truncation)] // audio frames fit u32
        let frames_u32 = frames as u32;
        let meta = pcm_meta_from_pts_us(
            self.spec,
            presentation_time_us,
            self.stream_ctx.as_deref(),
            self.epoch,
            frames_u32,
            source_bytes,
            // Inherit the byte offset captured from the most recent input
            // sample (`AMEDIAFORMAT_KEY_SAMPLE_FILE_OFFSET`), API 28+.
            // `None` on older Android or when the container doesn't
            // surface per-sample offsets — downstream treats that as
            // "byte offset unknown".
            self.last_input_byte_offset,
        )
        .map_err(|error| AndroidBackendError::operation("pcm-meta", error.to_string()))?;

        Ok(Some(PcmChunk::new(meta, pcm)))
    }

    fn convert_output_payload(
        &self,
        bytes: &[u8],
        trim_frames: usize,
    ) -> Result<PcmBuf, AndroidBackendError> {
        let result = match self.pcm_encoding {
            AndroidPcmEncoding::Pcm16 => {
                decode_pcm16_to_f32(&self.pool, bytes, trim_frames, self.spec.channels)
            }
            AndroidPcmEncoding::Float => {
                copy_pcm_float_to_pool(&self.pool, bytes, trim_frames, self.spec.channels)
            }
        };
        result.map_err(|error| AndroidBackendError::operation("pcm-conversion", error.to_string()))
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

        // MediaCodec hands us decoded PCM bytes directly — that's the
        // ground-truth size of the data this chunk consumed from the
        // codec output buffer. We can't trivially map it back to the
        // *encoded* input packet bytes without a PTS→input-bytes table
        // (input/output are decoupled in MediaCodec), so we report the
        // decoded payload size as the chunk's source-byte attribution.
        let source_bytes = output_data.len() as u64;

        let presentation_time_us = if let Some(target_us) = self.pending_seek_target_us {
            return match seek_trim_for_buffer(
                target_us,
                output.presentation_time_us,
                total_frames,
                self.spec.sample_rate,
            ) {
                SeekTrimOutcome::Drop => Ok(None),
                SeekTrimOutcome::Emit(trim) => {
                    let pcm = self.convert_output_payload(output_data, trim.trim_frames)?;
                    self.pending_seek_target_us = None;
                    let kept_frames = total_frames.saturating_sub(trim.trim_frames);
                    self.build_chunk(trim.output_timestamp_us, pcm, kept_frames, source_bytes)
                }
            };
        } else {
            output.presentation_time_us
        };

        let pcm = self.convert_output_payload(output_data, 0)?;
        self.build_chunk(presentation_time_us, pcm, total_frames, source_bytes)
    }

    fn frame_count_for_payload(&self, bytes_len: usize) -> Result<usize, AndroidBackendError> {
        let channels = usize::from(self.spec.channels);
        if channels == 0 {
            return Err(AndroidBackendError::operation(
                "pcm-frame-count",
                "output format reported zero channels",
            ));
        }

        const BYTES_PER_PCM16_SAMPLE: usize = 2;
        const BYTES_PER_FLOAT_SAMPLE: usize = 4;
        let bytes_per_sample = match self.pcm_encoding {
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

    fn try_drain_output(
        &mut self,
        timeout_us: i64,
    ) -> Result<Option<Option<PcmChunk>>, AndroidBackendError> {
        match self.codec.dequeue_output_buffer(timeout_us)? {
            DequeueOutput::TryAgainLater => Ok(None),
            DequeueOutput::OutputFormatChanged(format) => {
                info!(
                    sample_rate = format.spec.sample_rate,
                    channels = format.spec.channels,
                    pcm_encoding = ?format.pcm_encoding,
                    "Android MediaCodec output format changed"
                );
                self.spec = format.spec;
                self.pcm_encoding = format.pcm_encoding;
                Ok(Some(None))
            }
            DequeueOutput::Output(output) => {
                let eos = (output.flags & ffi::MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM) != 0;
                let buffer_index = output.index;
                let decoded = self.decode_output_chunk(&output);
                let release = self.codec.release_output_buffer(buffer_index);
                if eos {
                    debug!("Android MediaCodec signalled output end-of-stream");
                    self.output_eos = true;
                }
                release?;
                decoded.map(Some)
            }
        }
    }

    fn try_queue_input(&mut self) -> Result<bool, AndroidBackendError> {
        if self.input_eos {
            return Ok(false);
        }

        let Some(input) = self
            .codec
            .dequeue_input_buffer(Self::INPUT_DEQUEUE_TIMEOUT_US)?
        else {
            return Ok(false);
        };
        let mut input = input;
        let input_index = input.index;
        let sample = self
            .extractor
            .read_sample(input.data_mut(), self.selected_track)?;

        match sample {
            Some(sample) => {
                let sample_len = sample.bytes.len();
                let presentation_time_us = sample.presentation_time_us;
                // Capture the extractor's sample byte offset for the
                // *next* output chunk to inherit. MediaCodec is async
                // but FIFO for audio, so the most recently queued
                // input pairs with the next dequeued output.
                self.last_input_byte_offset = sample.byte_offset;
                self.codec
                    .queue_input_buffer(input_index, sample_len, presentation_time_us, 0)?;
                let _ = self.extractor.advance()?;
            }
            None => {
                debug!("Queued Android MediaCodec end-of-stream input buffer");
                self.codec.queue_end_of_stream(input_index)?;
                self.input_eos = true;
            }
        }

        Ok(true)
    }
}

impl Decoder for AndroidDecoder {
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.metadata.clone()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.assert_owner_thread()
            .map_err(AndroidBackendError::into_decode_error)?;
        let _ = &self.media_source;

        loop {
            if let Some(chunk_opt) = self
                .try_drain_output(0)
                .map_err(AndroidBackendError::into_decode_error)?
            {
                if let Some(chunk) = chunk_opt {
                    return Ok(DecoderChunkOutcome::Chunk(chunk));
                }
                if self.output_eos {
                    return Ok(DecoderChunkOutcome::Eof);
                }
                continue;
            }
            if self.output_eos {
                return Ok(DecoderChunkOutcome::Eof);
            }

            let queued_input = self
                .try_queue_input()
                .map_err(AndroidBackendError::into_decode_error)?;

            if let Some(chunk_opt) = self
                .try_drain_output(Self::OUTPUT_DEQUEUE_TIMEOUT_US)
                .map_err(AndroidBackendError::into_decode_error)?
            {
                if let Some(chunk) = chunk_opt {
                    return Ok(DecoderChunkOutcome::Chunk(chunk));
                }
                if self.output_eos {
                    return Ok(DecoderChunkOutcome::Eof);
                }
                continue;
            }
            if self.output_eos {
                return Ok(DecoderChunkOutcome::Eof);
            }

            if !queued_input && !self.input_eos {
                continue;
            }
        }
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        self.assert_owner_thread()
            .map_err(AndroidBackendError::into_decode_error)?;

        // Past-EOF short-circuit so the pipeline routes through the typed
        // PastEof path instead of the recreate-loop. Mirrors Apple/Symphonia.
        if let Some(duration) = self.duration
            && pos >= duration
        {
            return Ok(DecoderSeekOutcome::PastEof { duration });
        }

        let target_us = i64::try_from(pos.as_micros()).map_err(|_| {
            AndroidBackendError::operation("seek", "target position does not fit in i64")
                .into_decode_error()
        })?;
        self.extractor
            .seek_to(target_us)
            .map_err(AndroidBackendError::into_decode_error)?;
        // A seek invalidates any queued codec output from the old position, so
        // the codec must be flushed before decode resumes.
        self.codec
            .flush()
            .map_err(AndroidBackendError::into_decode_error)?;
        self.input_eos = false;
        self.output_eos = false;
        self.pending_seek_target_us = Some(target_us);
        debug!(target_us, "Android MediaCodec decoder seek requested");

        // MediaExtractor parks at the nearest sync sample <= target. The
        // precise landed PTS isn't available synchronously — the next
        // decoded buffer's PTS is the ground truth, and `seek_trim_for_buffer`
        // trims to `target_us`. Report `pos` as the approximate landed
        // position; `landed_byte = None` because MediaCodec doesn't surface
        // a packet-aligned source byte offset until the next dequeue.
        #[expect(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "frame index from non-negative pos and sample rate fits u64"
        )]
        let landed_frame = (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        Ok(DecoderSeekOutcome::Landed {
            landed_frame,
            landed_at: pos,
            landed_byte: None,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn update_byte_len(&self, len: u64) {
        self.byte_len_handle.store(len, Ordering::Release);
    }
}

fn recover_codec_failure(
    bootstrap: super::extractor::ExtractorBootstrap,
    error: AndroidBackendError,
) -> DecodeError {
    drop(bootstrap.extractor);
    recover_media_source_failure(bootstrap.media_source, error)
}

fn recover_media_source_failure(
    media_source: OwnedMediaDataSource,
    error: AndroidBackendError,
) -> DecodeError {
    match media_source.into_source() {
        Ok(_source) => error.into_decode_error(),
        Err(recover_error) => recover_error.into_decode_error(),
    }
}
