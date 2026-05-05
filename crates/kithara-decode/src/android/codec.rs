//! `AndroidCodec` — Android `MediaCodec` as a [`FrameCodec`].
//!
//! Container parsing happens upstream in a [`crate::Demuxer`]; this codec
//! consumes already-demuxed frame bytes and produces interleaved f32 PCM.
//! `AMediaFormat` is built from [`TrackInfo`] alone (MIME + sample-rate +
//! channel-count + `csd-0`), with no `AMediaExtractor` and no per-codec
//! container glue.
//!
//! Initial scope: AAC-LC and FLAC (the codecs that flow through
//! [`crate::fmp4::Fmp4SegmentDemuxer`]). MP3 / ALAC follow alongside the
//! file-Symphonia migration when [`TrackInfo`] carries codec-specific
//! extra-data shape.

#![allow(unsafe_code)]

use std::{ffi::c_void, ptr::NonNull, time::Duration};

use kithara_bufpool::PcmBuf;
use kithara_stream::AudioCodec;

use super::{
    aformat::OwnedFormat,
    ensure_current_thread_attached,
    error::AndroidBackendError,
    ffi::{
        self, KEY_CHANNEL_COUNT, KEY_CSD_0, KEY_MIME, KEY_PCM_ENCODING, KEY_SAMPLE_RATE, MIME_AAC,
        MIME_FLAC, PCM_ENCODING_16BIT, PCM_ENCODING_FLOAT,
    },
    media_codec::{AndroidPcmEncoding, DequeueOutput, OwnedCodec, QueueInput},
};
use crate::{
    android::config::AndroidConfig,
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    gapless::probe_mp4_gapless_dyn,
    traits::DecoderInput,
    types::{DecoderTrackInfo, PcmSpec},
};

struct Consts;

impl Consts {
    const INPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const OUTPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const PCM16_SCALE: f32 = 32_768.0;
}

/// Frame-level codec wrapping Android's `AMediaCodec`.
pub(crate) struct AndroidCodec {
    pcm_encoding: AndroidPcmEncoding,
    codec: OwnedCodec,
    spec: PcmSpec,
    /// Decoder-owned playback contract. Populated from container-level
    /// gapless metadata (MP4 udta) when [`AndroidConfig::gapless`] is
    /// set; left empty otherwise. `MediaCodec` itself does not surface
    /// encoder priming, so the contract value comes solely from
    /// container probing.
    track_info: DecoderTrackInfo,
}

impl AndroidCodec {
    /// Whether `MediaCodec` accepts this codec at the codec layer alone
    /// (i.e. without an extractor providing per-track metadata).
    /// Initial scope: AAC family + FLAC.
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
    }

    /// Build an [`AndroidCodec`] with extra knobs from [`AndroidConfig`].
    /// `MediaCodec` itself does not surface encoder priming, so the
    /// gapless flag here is wired into the factory's container probe
    /// (P7 calls [`Self::probe_track_info`] before opening the codec).
    /// `FrameCodec::open` keeps the no-config shape so existing
    /// `UniversalDecoder<D, C>` callers don't break.
    ///
    /// # Errors
    ///
    /// Same as [`FrameCodec::open`].
    pub(crate) fn open_with_config(
        track: &TrackInfo,
        _config: &AndroidConfig,
    ) -> DecodeResult<Self> {
        // No `AudioDecoderOptions::gapless` analogue on `MediaCodec` —
        // priming/padding numbers come from the demuxer's MP4 udta
        // probe (see `probe_track_info`). The config exists so the
        // factory keeps a uniform call shape across apple/symphonia/android.
        Self::open(track)
    }

    /// Inherent constructor used by [`Self::open_with_config`].
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] for codecs the
    /// `MediaCodec` codec layer doesn't accept; any FFI failure
    /// surfaces as [`DecodeError::Backend`] via
    /// [`AndroidBackendError::into_decode_error`].
    fn open(track: &TrackInfo) -> DecodeResult<Self> {
        ensure_current_thread_attached().map_err(AndroidBackendError::into_decode_error)?;

        let mime = match track.codec {
            AudioCodec::AacLc => MIME_AAC,
            AudioCodec::Flac => MIME_FLAC,
            other => return Err(DecodeError::UnsupportedCodec(other)),
        };

        let format = build_format(mime, track).map_err(AndroidBackendError::into_decode_error)?;
        let codec = OwnedCodec::create_with_format(mime, &format)
            .map_err(AndroidBackendError::into_decode_error)?;
        let (spec, pcm_encoding) =
            read_output_format(&codec).map_err(AndroidBackendError::into_decode_error)?;

        Ok(Self {
            codec,
            spec,
            pcm_encoding,
            track_info: DecoderTrackInfo::default(),
        })
    }

    /// Probe the source for container-level gapless metadata before
    /// opening the codec. Currently only AAC inside MP4 udta carries
    /// useful priming/padding numbers (`iTunSMPB`); other codecs
    /// return `DecoderTrackInfo::default()`.
    ///
    /// # Errors
    ///
    /// Forwards [`DecodeError`] from the MP4 probe.
    pub(crate) fn probe_track_info(
        source: &mut dyn DecoderInput,
        codec: AudioCodec,
        config: &AndroidConfig,
    ) -> DecodeResult<DecoderTrackInfo> {
        let gapless = if config.gapless && codec == AudioCodec::AacLc {
            let info = probe_mp4_gapless_dyn(source)?;
            if let Some(info) = info {
                tracing::debug!(
                    target: "kithara::gapless",
                    codec = ?codec,
                    leading_frames = info.leading_frames,
                    trailing_frames = info.trailing_frames,
                    "captured AAC gapless metadata for Android"
                );
            }
            info
        } else {
            None
        };
        Ok(DecoderTrackInfo {
            gapless,
            ..DecoderTrackInfo::default()
        })
    }
}

impl FrameCodec for AndroidCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
        if frame_data.is_empty() {
            out.clear();
            return Ok(0);
        }

        match self
            .codec
            .dequeue_input_buffer(Consts::INPUT_DEQUEUE_TIMEOUT_US)
            .map_err(AndroidBackendError::into_decode_error)?
        {
            Some(mut buf) => {
                let dst = buf.data_mut();
                let copy_len = dst.len().min(frame_data.len());
                dst[..copy_len].copy_from_slice(&frame_data[..copy_len]);
                let pts_us = i64::try_from(pts.as_micros()).unwrap_or(i64::MAX);
                self.codec
                    .queue_input_buffer(QueueInput {
                        index: buf.index,
                        size: copy_len,
                        presentation_time_us: pts_us,
                        flags: 0,
                    })
                    .map_err(AndroidBackendError::into_decode_error)?;
            }
            None => {
                // Codec backpressure — caller will retry the frame on the
                // next pump. UniversalDecoder loops on zero-frame returns.
                out.clear();
                return Ok(0);
            }
        }

        match self
            .codec
            .dequeue_output_buffer(Consts::OUTPUT_DEQUEUE_TIMEOUT_US)
            .map_err(AndroidBackendError::into_decode_error)?
        {
            DequeueOutput::Output(buffer) => {
                let bytes = buffer.data();
                match self.pcm_encoding {
                    AndroidPcmEncoding::Pcm16 => decode_pcm16_into(bytes, out)?,
                    AndroidPcmEncoding::Float => decode_pcm_float_into(bytes, out)?,
                };
                let index = buffer.index;
                self.codec
                    .release_output_buffer(index)
                    .map_err(AndroidBackendError::into_decode_error)?;
                let channels = self.spec.channels as usize;
                let frames = if channels == 0 {
                    0
                } else {
                    u32::try_from(out.len() / channels).unwrap_or(u32::MAX)
                };
                Ok(frames)
            }
            DequeueOutput::OutputFormatChanged(new_format) => {
                self.spec = new_format.spec;
                self.pcm_encoding = new_format.pcm_encoding;
                out.clear();
                Ok(0)
            }
            DequeueOutput::TryAgainLater => {
                out.clear();
                Ok(0)
            }
        }
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.codec
            .flush()
            .map_err(AndroidBackendError::into_decode_error)
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

fn build_format(
    mime: &std::ffi::CStr,
    track: &TrackInfo,
) -> Result<OwnedFormat, AndroidBackendError> {
    // SAFETY: AMediaFormat_new returns a freshly allocated AMediaFormat
    // we own; non-null on success.
    let raw = NonNull::new(unsafe { ffi::AMediaFormat_new() })
        .ok_or_else(|| AndroidBackendError::operation("media-format-new", "returned null"))?;
    let mut format = OwnedFormat::from_raw(raw);

    // SAFETY: format is live; key/value are static null-terminated CStrs.
    unsafe {
        ffi::AMediaFormat_setString(format.raw(), KEY_MIME.as_ptr(), mime.as_ptr());
    }

    let sample_rate = i32::try_from(track.sample_rate).map_err(|_| {
        AndroidBackendError::operation(
            "media-format-sample-rate",
            format!("rate={} out of range", track.sample_rate),
        )
    })?;
    let channels = i32::from(track.channels);
    if !format.set_i32(KEY_SAMPLE_RATE, sample_rate) {
        return Err(AndroidBackendError::operation(
            "media-format-set-sample-rate",
            format!("rate={sample_rate}"),
        ));
    }
    if !format.set_i32(KEY_CHANNEL_COUNT, channels) {
        return Err(AndroidBackendError::operation(
            "media-format-set-channels",
            format!("channels={channels}"),
        ));
    }
    // Non-fatal: codec may negotiate a different output PCM encoding via
    // OUTPUT_FORMAT_CHANGED later.
    let _ = format.set_i32(KEY_PCM_ENCODING, PCM_ENCODING_16BIT);

    if !track.extra_data.is_empty() {
        // SAFETY: format is live; extra_data is a readable byte slice.
        unsafe {
            ffi::AMediaFormat_setBuffer(
                format.raw(),
                KEY_CSD_0.as_ptr(),
                track.extra_data.as_ptr() as *const c_void,
                track.extra_data.len(),
            );
        }
    }

    Ok(format)
}

fn read_output_format(
    codec: &OwnedCodec,
) -> Result<(PcmSpec, AndroidPcmEncoding), AndroidBackendError> {
    let format = codec.output_format()?;
    let sample_rate = format.get_u32(KEY_SAMPLE_RATE)?.ok_or_else(|| {
        AndroidBackendError::operation("codec-output-format", "missing sample-rate")
    })?;
    let channels = format.get_u16(KEY_CHANNEL_COUNT)?.ok_or_else(|| {
        AndroidBackendError::operation("codec-output-format", "missing channel-count")
    })?;
    let pcm_encoding = match format.get_i32(KEY_PCM_ENCODING) {
        None | Some(PCM_ENCODING_16BIT) => AndroidPcmEncoding::Pcm16,
        Some(PCM_ENCODING_FLOAT) => AndroidPcmEncoding::Float,
        Some(other) => return Err(AndroidBackendError::UnsupportedPcmEncoding { encoding: other }),
    };
    Ok((
        PcmSpec {
            sample_rate,
            channels,
        },
        pcm_encoding,
    ))
}

fn decode_pcm16_into(bytes: &[u8], out: &mut PcmBuf) -> DecodeResult<()> {
    let count = bytes.len() / 2;
    out.ensure_len(count)
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;
    for (dst, chunk) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        *dst = f32::from(s) / Consts::PCM16_SCALE;
    }
    out.truncate(count);
    Ok(())
}

fn decode_pcm_float_into(bytes: &[u8], out: &mut PcmBuf) -> DecodeResult<()> {
    let count = bytes.len() / 4;
    out.ensure_len(count)
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;
    for (dst, chunk) in out.iter_mut().zip(bytes.chunks_exact(4)) {
        *dst = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    out.truncate(count);
    Ok(())
}
