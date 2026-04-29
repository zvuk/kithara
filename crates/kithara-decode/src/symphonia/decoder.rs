//! Symphonia decoder: shared decode/seek lifecycle across all codecs.
//!
//! Codec selection happens inside `SymphoniaDecoder::new` from the
//! config's `container` (direct reader) or via Symphonia's probe chain
//! — no compile-time codec marker needed.

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
use kithara_stream::{PendingReason, StreamContext, StreamSeekPastEof};
use symphonia::core::{
    codecs::{
        CodecParameters,
        audio::{AudioDecoder as SymphoniaAudioDecoder, AudioDecoderOptions},
    },
    errors::{Error as SymphoniaError, SeekErrorKind},
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track, TrackType},
    packet::Packet,
    units::{Time, Timestamp},
};

use super::{
    config::SymphoniaConfig,
    probe::{ReaderBootstrap, new_direct, probe_with_seek},
};
use crate::{
    error::{DecodeError, DecodeResult},
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Outcome of [`SymphoniaDecoder::next_track_packet`].
enum NextPacket {
    Got(Packet),
    Eof,
    Pending(PendingReason),
}

pub(crate) struct SymphoniaDecoder {
    byte_len_handle: Arc<AtomicU64>,
    /// Live read-cursor of the underlying `ReadSeekAdapter`. Updated
    /// on every `Read::read` and `Seek::seek` so we can report the
    /// authoritative byte offset of the next packet body in
    /// `DecoderSeekOutcome::Landed.landed_byte`.
    byte_pos_handle: Arc<AtomicU64>,
    decoder: Box<dyn SymphoniaAudioDecoder>,
    format_reader: Box<dyn FormatReader>,
    position: Duration,
    duration: Option<Duration>,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    pool: PcmPool,
    spec: PcmSpec,
    metadata: TrackMetadata,
    track_id: u32,
    epoch: u64,
    frame_offset: u64,
}

impl SymphoniaDecoder {
    /// Create a new decoder from a Read + Seek source.
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

    fn calculate_duration(track: &Track) -> Option<Duration> {
        let num_frames = track.num_frames?;
        let time_base = track.time_base?;
        let time = time_base.calc_time(Timestamp::new(num_frames as i64))?;
        let (seconds, nanos) = time.parts();
        Some(Duration::new(seconds.cast_unsigned(), nanos))
    }

    fn init_from_bootstrap(
        bootstrap: ReaderBootstrap,
        config: &SymphoniaConfig,
    ) -> DecodeResult<Self> {
        Self::init_from_reader(
            bootstrap.format_reader,
            config,
            bootstrap.byte_len_handle,
            bootstrap.byte_pos_handle,
        )
    }

    fn init_from_reader(
        format_reader: Box<dyn FormatReader>,
        config: &SymphoniaConfig,
        byte_len_handle: Arc<AtomicU64>,
        byte_pos_handle: Arc<AtomicU64>,
    ) -> DecodeResult<Self> {
        /// Default audio channel count (stereo).
        const DEFAULT_CHANNEL_COUNT: u16 = 2;

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
            duration,
            metadata,
            byte_len_handle,
            byte_pos_handle,
            pool,
            position: Duration::ZERO,
            frame_offset: 0,
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
        })
    }

    fn next_chunk_impl(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        self.refresh_duration();
        loop {
            let packet = match self.next_track_packet()? {
                NextPacket::Got(p) => p,
                NextPacket::Eof => return Ok(DecoderChunkOutcome::Eof),
                NextPacket::Pending(reason) => return Ok(DecoderChunkOutcome::Pending(reason)),
            };

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

            #[expect(clippy::cast_possible_truncation)] // audio frames fit u32
            let chunk_frames = decoded.frames() as u32;
            // Compute the end-timestamp for this chunk inside the
            // decoder (own arithmetic) and pass it through `PcmMeta`
            // so the timeline never recomputes it. Same formula
            // already used to advance `self.position` after the chunk
            // is consumed.
            let frame_duration_after = if pcm_spec.sample_rate > 0 {
                Duration::from_secs_f64(f64::from(chunk_frames) / f64::from(pcm_spec.sample_rate))
            } else {
                Duration::ZERO
            };
            let end_timestamp = self.position.saturating_add(frame_duration_after);
            let meta = PcmMeta {
                end_timestamp,
                spec: pcm_spec,
                frame_offset: self.frame_offset,
                timestamp: self.position,
                segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
                variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
                epoch: self.epoch,
                frames: chunk_frames,
                source_bytes: packet.data.len() as u64,
                // Symphonia's public `FormatReader` API does not expose
                // a per-packet byte offset (the underlying
                // `MediaSourceStream::pos()` is private to the format
                // reader implementation). Leave as `None` until/unless
                // we add a tracking shim around our `ReadSeekAdapter`.
                source_byte_offset: None,
            };

            let chunk = PcmChunk::new(meta, pooled);

            // Contract validation: a `Chunk` outcome must carry a
            // non-empty PcmChunk with a sane spec — otherwise downstream
            // would advance the timeline by 0 frames and silently spin.
            debug_assert!(
                chunk.frames() > 0,
                "SymphoniaDecoder::next_chunk Chunk contract violated: frames=0",
            );
            debug_assert!(
                chunk.spec().sample_rate > 0,
                "SymphoniaDecoder::next_chunk Chunk contract violated: sample_rate=0",
            );
            debug_assert!(
                chunk.spec().channels > 0,
                "SymphoniaDecoder::next_chunk Chunk contract violated: channels=0",
            );

            if self.spec.sample_rate > 0 {
                let frames = chunk.frames();
                #[expect(clippy::cast_precision_loss)] // frame count precision loss is acceptable
                let frame_duration =
                    Duration::from_secs_f64(frames as f64 / f64::from(self.spec.sample_rate));
                self.position = self.position.saturating_add(frame_duration);
                let frames_u64 = frames as u64;
                self.frame_offset += frames_u64;
            }

            return Ok(DecoderChunkOutcome::Chunk(chunk));
        }
    }

    /// Fetch the next packet for our track, transparently skipping packets
    /// for other tracks and surfacing `Eof`/`Pending` as typed outcomes.
    fn next_track_packet(&mut self) -> DecodeResult<NextPacket> {
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => return Ok(NextPacket::Eof),
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::IoError(ref e)) if e.kind() == ErrorKind::UnexpectedEof => {
                    tracing::debug!("Treating UnexpectedEof as EOF");
                    return Ok(NextPacket::Eof);
                }
                Err(SymphoniaError::IoError(ref e)) if e.kind() == ErrorKind::Interrupted => {
                    // Pending seek raced the decoder's read — let the worker
                    // apply the seek instead of treating it as a fault.
                    return Ok(NextPacket::Pending(PendingReason::SeekPending));
                }
                Err(e) => return Err(DecodeError::Backend(Box::new(e))),
            };
            if packet.track_id() == self.track_id {
                return Ok(NextPacket::Got(packet));
            }
        }
    }

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
}

impl Decoder for SymphoniaDecoder {
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn metadata(&self) -> TrackMetadata {
        self.metadata.clone()
    }

    /// Decode the next chunk of PCM data.
    ///
    /// Wraps the inner decode loop in `catch_unwind` to convert Symphonia
    /// codec panics into `DecodeError::InvalidData` instead of aborting.
    /// Returns the typed [`DecoderChunkOutcome`] so seek-pending /
    /// backpressure are surfaced as `Pending(reason)` instead of
    /// being squashed into a single `Err(Interrupted)` shape.
    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
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

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        // Note: do NOT short-circuit here on `pos >= duration`. The
        // pipeline relies on `format_reader.seek` reporting the real
        // failure (`SeekFailed`) so `recover_from_decoder_seek_error`
        // can drive a bounded recreate-loop and surface a typed
        // `TrackError` to the player. Returning `Ok(PastEof)` from
        // here would let the pipeline silently mark the seek as
        // successful while the decoder continues from its previous
        // position — see `hls_seek_past_end_terminates_in_bounded_time`.

        let seek_to = SeekTo::Time {
            time: Time::try_new(pos.as_secs() as i64, pos.subsec_nanos()).unwrap_or(Time::ZERO),
            track_id: Some(self.track_id),
        };

        tracing::trace!(
            position_secs = pos.as_secs_f64(),
            track_id = self.track_id,
            "sending seek to symphonia"
        );

        let seeked = self
            .format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| classify_format_seek_error(&e))?;

        // Resolve the actual landed timestamp from the format reader so
        // the outcome reflects where the decoder is *actually* parked,
        // not where the caller asked it to go. The two diverge for
        // near-EOF / packet-aligned seeks where the underlying format
        // can only land at a packet boundary.
        let track_time_base = self
            .format_reader
            .tracks()
            .iter()
            .find(|t| t.id == self.track_id)
            .and_then(|t| t.time_base);
        let landed_at = track_time_base
            .and_then(|tb| {
                let time = tb.calc_time(seeked.actual_ts)?;
                let (secs, nanos) = time.parts();
                Some(Duration::new(secs.cast_unsigned(), nanos))
            })
            .unwrap_or(pos);

        self.decoder.reset();
        self.position = landed_at;
        #[expect(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        // landed position and sample rate are non-negative; precision loss acceptable
        {
            self.frame_offset = (landed_at.as_secs_f64() * f64::from(self.spec.sample_rate)) as u64;
        }

        // Contract validation: a `Landed` outcome must report a position
        // inside the decoder's known duration window. A landed_at past
        // duration means the format reader silently advanced to EOF —
        // surface that as `PastEof` instead so the pipeline can route
        // through the typed past-EOF path rather than producing zero
        // chunks from a "successful" seek.
        if let Some(duration) = self.duration
            && landed_at >= duration
        {
            return Ok(DecoderSeekOutcome::PastEof { duration });
        }

        debug_assert!(
            self.duration.is_none_or(|dur| landed_at <= dur),
            "SymphoniaDecoder::seek Landed contract violated: landed_at={landed_at:?} > duration={:?}",
            self.duration,
        );

        // After `format_reader.seek` the underlying `MediaSourceStream`
        // is parked at the next packet body's byte offset — read it
        // back from the adapter's live cursor so the pipeline knows
        // the *real* byte target without re-deriving it from frames.
        let landed_byte = Some(self.byte_pos_handle.load(Ordering::Acquire));

        Ok(DecoderSeekOutcome::Landed {
            landed_at,
            landed_byte,
            landed_frame: self.frame_offset,
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn update_byte_len(&self, len: u64) {
        self.byte_len_handle.store(len, Ordering::Release);
    }
}

/// Classify a Symphonia format-reader seek failure into a typed
/// [`DecodeError`] variant, distinguishing caller-side range errors
/// (target invalid for this stream — pipeline must surface, no retry
/// helps) from generic seek failures (decoder corruption, transient,
/// recoverable via recreate).
///
/// Caller-side classes:
/// - [`SymphoniaError::SeekError`] with [`SeekErrorKind::OutOfRange`] —
///   Symphonia's own range check on the indexed track timeline.
/// - [`SymphoniaError::IoError`] wrapping [`StreamSeekPastEof`] —
///   `Stream::seek` rejected an absolute byte target Symphonia
///   computed from a time seek (the production "перемотка не работает"
///   path: sidx-based byte target overflow on fragmented MP4 streams,
///   reported via [`std::io::ErrorKind::InvalidInput`] with the typed
///   [`StreamSeekPastEof`] payload).
pub(crate) fn classify_format_seek_error(err: &SymphoniaError) -> DecodeError {
    match err {
        SymphoniaError::SeekError(SeekErrorKind::OutOfRange) => {
            DecodeError::SeekOutOfRange(err.to_string())
        }
        SymphoniaError::IoError(io_err)
            if io_err.get_ref().is_some_and(
                <dyn std::error::Error + Send + Sync + 'static>::is::<StreamSeekPastEof>,
            ) =>
        {
            DecodeError::SeekOutOfRange(io_err.to_string())
        }
        // `UnexpectedEof` raised inside a `format_reader.seek` is a
        // caller-side range error: the demuxer's atom tables said the
        // sample lives at an offset past the available bytes (e.g.
        // seek target beyond the last fragment's mdat). A fresh
        // decoder reads the same atom tables and will fail
        // identically, so route this to the no-retry path instead of
        // the recreate-loop.
        SymphoniaError::IoError(io_err) if io_err.kind() == ErrorKind::UnexpectedEof => {
            DecodeError::SeekOutOfRange(io_err.to_string())
        }
        // `Stream::read` packages `Pending::NotReady|Retry` as
        // `Interrupted("data not ready")` and `Pending::SeekPending`
        // as `Other` carrying a typed `PendingReason`. Both are
        // transient — the bytes the decoder needs for this seek
        // aren't cached yet but will be. Recreating the decoder
        // burns state without solving the readiness gap and produces
        // a tight recreate-loop; surface as `Interrupted` so the
        // pipeline retries the seek after waiting on the source.
        SymphoniaError::IoError(io_err)
            if io_err.kind() == ErrorKind::Interrupted
                || io_err.get_ref().is_some_and(|src| {
                    src.downcast_ref::<PendingReason>()
                        .is_some_and(|reason| matches!(reason, PendingReason::SeekPending))
                }) =>
        {
            DecodeError::Interrupted
        }
        _ => DecodeError::SeekFailed(err.to_string()),
    }
}
