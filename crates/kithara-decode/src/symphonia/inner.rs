//! Symphonia inner state — shared decode/seek lifecycle across all codecs.

use std::{
    io::{ErrorKind, Read, Seek},
    panic::{self, AssertUnwindSafe},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_bufpool::PcmPool;
use kithara_stream::StreamContext;
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions},
    },
    errors::Error as SymphoniaError,
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType},
    units::{Time, Timestamp},
};

use super::{
    config::SymphoniaConfig,
    probe::{ReaderBootstrap, new_direct, probe_with_seek},
};
use crate::{
    error::{DecodeError, DecodeResult},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Default audio channel count (stereo).
const DEFAULT_CHANNEL_COUNT: u16 = 2;

/// Inner implementation shared across all Symphonia codecs.
pub(crate) struct SymphoniaInner {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn SymphoniaAudioDecoder>,
    track_id: u32,
    pub(crate) spec: PcmSpec,
    position: Duration,
    frame_offset: u64,
    pub(crate) duration: Option<Duration>,
    pub(crate) metadata: TrackMetadata,
    byte_len_handle: Arc<AtomicU64>,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    epoch: u64,
    pool: PcmPool,
}

impl SymphoniaInner {
    fn refresh_duration(&mut self) {
        let Some(track) = self
            .format_reader
            .tracks()
            .iter()
            .find(|track| track.id == self.track_id)
        else {
            return;
        };
        let fresh = Self::calculate_duration(track);
        // Update duration when a new (larger) value becomes available.
        // Streaming sources may report a short initial estimate based on
        // buffered data; the format reader corrects it as more data arrives.
        match (self.duration, fresh) {
            (None, _) => self.duration = fresh,
            (Some(old), Some(new)) if new > old => self.duration = Some(new),
            _ => {}
        }
    }

    /// Create a new inner decoder from a Read + Seek source.
    pub(crate) fn new<R>(source: R, config: &SymphoniaConfig) -> DecodeResult<Self>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let format_opts = FormatOptions::default();

        // Seek stays off for the whole initialization window: this is a
        // streaming player, and Symphonia's probe/direct readers otherwise
        // seek-to-end to validate container sizes, which either stalls
        // waiting for bytes that haven't arrived or walks off a short
        // variant buffer after an ABR hop. Every container we support
        // places its headers at the front of the stream, so linear reads
        // during init are sufficient. Seek is re-enabled by the bootstrap
        // routine as soon as the reader is built.
        let bootstrap = if let Some(container) = config.container {
            new_direct(source, config, container, format_opts)?
        } else {
            probe_with_seek(source, config, format_opts, false)?
        };

        Self::init_from_bootstrap(bootstrap, config)
    }

    fn init_from_bootstrap(
        bootstrap: ReaderBootstrap,
        config: &SymphoniaConfig,
    ) -> DecodeResult<Self> {
        Self::init_from_reader(bootstrap.format_reader, config, bootstrap.byte_len_handle)
    }

    fn init_from_reader(
        format_reader: Box<dyn FormatReader>,
        config: &SymphoniaConfig,
        byte_len_handle: Arc<AtomicU64>,
    ) -> DecodeResult<Self> {
        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecodeError::ProbeFailed)?
            .clone();

        let track_id = track.id;

        let codec_params = match &track.codec_params {
            Some(CodecParameters::Audio(params)) => params.clone(),
            _ => return Err(DecodeError::ProbeFailed),
        };

        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| DecodeError::InvalidData("No sample rate".to_string()))?;
        #[expect(clippy::cast_possible_truncation)] // audio channel count always fits u16
        let channels = codec_params
            .channels
            .as_ref()
            .map_or(DEFAULT_CHANNEL_COUNT, |c| c.count() as u16);
        let spec = PcmSpec {
            channels,
            sample_rate,
        };

        let mut decoder_opts = AudioDecoderOptions::default();
        decoder_opts.verify = config.verify;
        decoder_opts.gapless = config.gapless;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&codec_params, &decoder_opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        // Calculate duration from track metadata.
        // Fallback: when num_frames is unavailable, try track.duration field.
        #[expect(
            clippy::cast_possible_wrap,
            reason = "duration in timebase ticks fits i64"
        )]
        let duration = Self::calculate_duration(&track).or_else(|| {
            let dur = track.duration?;
            let tb = track.time_base?;
            let time = tb.calc_time(Timestamp::new(dur.get() as i64))?;
            let (seconds, nanos) = time.parts();
            Some(Duration::new(seconds.cast_unsigned(), nanos))
        });

        let metadata = TrackMetadata::default();

        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            spec,
            position: Duration::ZERO,
            frame_offset: 0,
            duration,
            metadata,
            byte_len_handle,
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
            pool,
        })
    }

    fn calculate_duration(track: &Track) -> Option<Duration> {
        let num_frames = track.num_frames?;
        let time_base = track.time_base?;
        let time = time_base.calc_time(Timestamp::new(num_frames as i64))?;
        let (seconds, nanos) = time.parts();
        Some(Duration::new(seconds.cast_unsigned(), nanos))
    }

    /// Decode the next chunk of PCM data.
    ///
    /// Wraps the inner decode loop in `catch_unwind` to convert Symphonia
    /// codec panics into `DecodeError::InvalidData` instead of aborting.
    pub(crate) fn next_chunk(&mut self) -> DecodeResult<Option<PcmChunk>> {
        match panic::catch_unwind(AssertUnwindSafe(|| self.next_chunk_impl())) {
            Ok(result) => result,
            Err(payload) => {
                let msg = match payload.downcast::<String>() {
                    Ok(s) => *s,
                    Err(payload) => payload
                        .downcast::<&str>()
                        .map_or_else(|_| "unknown panic".to_string(), |s| (*s).to_string()),
                };
                Err(DecodeError::InvalidData(format!("symphonia panic: {msg}")))
            }
        }
    }

    fn next_chunk_impl(&mut self) -> DecodeResult<Option<PcmChunk>> {
        self.refresh_duration();
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(None), // EOF
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(ref e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    tracing::debug!("Treating UnexpectedEof as EOF");
                    return Ok(None);
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphoniaError::DecodeError(err)) => {
                    tracing::debug!(error = %err, "Skipping undecodable packet");
                    continue;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };

            let spec = decoded.spec();
            let channels = spec.channels().count();
            let num_samples = decoded.samples_interleaved();

            if num_samples == 0 {
                continue;
            }

            let mut pooled = self.pool.get();
            pooled
                .ensure_len(num_samples)
                .map_err(|e| DecodeError::Backend(Box::new(e)))?;
            decoded.copy_to_slice_interleaved(&mut *pooled);

            #[expect(clippy::cast_possible_truncation)] // audio channel count always fits u16
            let pcm_spec = PcmSpec {
                channels: channels as u16,
                sample_rate: spec.rate(),
            };

            let meta = PcmMeta {
                spec: pcm_spec,
                frame_offset: self.frame_offset,
                timestamp: self.position,
                segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
                variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
                epoch: self.epoch,
            };

            let chunk = PcmChunk::new(meta, pooled);

            if self.spec.sample_rate > 0 {
                let frames = chunk.frames();
                #[expect(clippy::cast_precision_loss)] // frame count precision loss is acceptable
                let frame_duration =
                    Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate));
                self.position = self.position.saturating_add(frame_duration);
                let frames_u64 = frames as u64;
                self.frame_offset += frames_u64;
            }

            return Ok(Some(chunk));
        }
    }

    pub(crate) fn seek(&mut self, pos: Duration) -> DecodeResult<()> {
        let seek_to = SeekTo::Time {
            time: Time::try_new(pos.as_secs() as i64, pos.subsec_nanos()).unwrap_or(Time::ZERO),
            track_id: Some(self.track_id),
        };

        tracing::trace!(
            position_secs = pos.as_secs_f64(),
            track_id = self.track_id,
            "sending seek to symphonia"
        );

        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| DecodeError::SeekFailed(e.to_string()))?;

        self.decoder.reset();
        self.position = pos;
        #[expect(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        // seek position and sample rate are non-negative; precision loss acceptable
        {
            self.frame_offset = (pos.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        }
        Ok(())
    }

    pub(crate) fn update_byte_len(&self, len: u64) {
        self.byte_len_handle.store(len, Ordering::Release);
    }
}
