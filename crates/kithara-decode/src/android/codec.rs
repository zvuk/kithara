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

use kithara_stream::AudioCodec;

use super::{
    aformat::OwnedFormat,
    ensure_current_thread_attached,
    error::AndroidBackendError,
    ffi::{
        self, KEY_CHANNEL_COUNT, KEY_CSD_0, KEY_MIME, KEY_PCM_ENCODING, KEY_SAMPLE_RATE, MIME_AAC,
        MIME_FLAC, PCM_ENCODING_16BIT, PCM_ENCODING_FLOAT,
    },
    media_codec::{AndroidPcmEncoding, DequeueOutput, OwnedCodec},
};
use crate::{
    codec::{DecodedFrame, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::PcmSpec,
};

struct Consts;

impl Consts {
    const INPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const OUTPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    const PCM16_SCALE: f32 = 32_768.0;
}

/// Frame-level codec wrapping Android's `AMediaCodec`.
pub(crate) struct AndroidCodec {
    codec: OwnedCodec,
    spec: PcmSpec,
    pcm_encoding: AndroidPcmEncoding,
}

impl AndroidCodec {
    /// Whether `MediaCodec` accepts this codec at the codec layer alone
    /// (i.e. without an extractor providing per-track metadata).
    /// Initial scope: AAC family + FLAC.
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
    }
}

impl FrameCodec for AndroidCodec {
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
        })
    }

    fn decode_frame(&mut self, frame_data: &[u8], pts: Duration) -> DecodeResult<DecodedFrame> {
        if frame_data.is_empty() {
            return Ok(empty_frame());
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
                    .queue_input_buffer(buf.index, copy_len, pts_us, 0)
                    .map_err(AndroidBackendError::into_decode_error)?;
            }
            None => {
                // Codec backpressure — caller will retry the frame on the
                // next pump. UniversalDecoder loops on zero-frame returns.
                return Ok(empty_frame());
            }
        }

        match self
            .codec
            .dequeue_output_buffer(Consts::OUTPUT_DEQUEUE_TIMEOUT_US)
            .map_err(AndroidBackendError::into_decode_error)?
        {
            DequeueOutput::Output(out) => {
                let bytes = out.data();
                let samples = match self.pcm_encoding {
                    AndroidPcmEncoding::Pcm16 => decode_pcm16(bytes),
                    AndroidPcmEncoding::Float => decode_pcm_float(bytes),
                };
                let index = out.index;
                self.codec
                    .release_output_buffer(index)
                    .map_err(AndroidBackendError::into_decode_error)?;
                let channels = self.spec.channels as usize;
                let frames = if channels == 0 {
                    0
                } else {
                    u32::try_from(samples.len() / channels).unwrap_or(u32::MAX)
                };
                Ok(DecodedFrame { samples, frames })
            }
            DequeueOutput::OutputFormatChanged(new_format) => {
                self.spec = new_format.spec;
                self.pcm_encoding = new_format.pcm_encoding;
                Ok(empty_frame())
            }
            DequeueOutput::TryAgainLater => Ok(empty_frame()),
        }
    }

    fn flush(&mut self) {
        let _ = self.codec.flush();
    }

    fn spec(&self) -> PcmSpec {
        self.spec
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

fn decode_pcm16(bytes: &[u8]) -> Vec<f32> {
    let mut samples = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        samples.push(f32::from(s) / Consts::PCM16_SCALE);
    }
    samples
}

fn decode_pcm_float(bytes: &[u8]) -> Vec<f32> {
    let mut samples = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    samples
}

const fn empty_frame() -> DecodedFrame {
    DecodedFrame {
        samples: Vec::new(),
        frames: 0,
    }
}
