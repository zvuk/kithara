use kithara_android::{
    AndroidBackendError,
    media::{
        AndroidPcmEncoding, DequeueOutput, InputBuffer, OutputFormat, OwnedCodec, OwnedFormat,
        QueueInput,
        sys::{
            KEY_CHANNEL_COUNT, KEY_CSD_0, KEY_MIME, KEY_PCM_ENCODING, KEY_SAMPLE_RATE,
            MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM, MIME_AAC, MIME_ALAC, MIME_FLAC, MIME_MP3,
            MIME_RAW, PCM_ENCODING_16BIT,
        },
    },
};
use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;
use kithara_stream::AudioCodec;

use super::output_spec;
use crate::{
    codec::{CodecPriming, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::DecoderTrackInfo,
};

mod consts {
    pub(super) const DRAIN_DEQUEUE_TIMEOUT_US: i64 = 1_000_000;
    pub(super) const INPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    /// Wait for a free input buffer before the codec counts as stuck.
    pub(super) const INPUT_WAIT_BUDGET_US: i64 = 1_000_000;
    pub(super) const NO_WAIT_US: i64 = 0;
    pub(super) const OUTPUT_DEQUEUE_TIMEOUT_US: i64 = 10_000;
    pub(super) const PCM16_SCALE: f32 = 32_768.0;
}

#[derive(Default)]
enum DrainState {
    #[default]
    Feeding,
    Draining,
    Finished,
}

/// Frame-level codec wrapping Android's `AMediaCodec`.
///
/// fMP4 uses demuxer-provided gapless metadata. Standalone files retain the
/// extractor's complete format when configuring `MediaCodec`, including PCM
/// encoding and codec delay/padding. Output timestamps and EOS come from the
/// codec's output queue.
pub(crate) struct AndroidCodec {
    pcm_encoding: AndroidPcmEncoding,
    spec: AudioSpec,
    track_info: DecoderTrackInfo,
    drain: DrainState,
    decoded_pts: Duration,
    codec: OwnedCodec,
}

impl AndroidCodec {
    /// Configure a frame codec from the demuxer's track metadata, retaining
    /// its gapless contract for the audio pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] for codecs the
    /// `MediaCodec` codec layer doesn't accept; any FFI failure
    /// surfaces as [`DecodeError::Backend`] via [`DecodeError::from`].
    pub(crate) fn open_with_config(track: &TrackInfo) -> DecodeResult<Self> {
        let format = build_format(codec_mime(track.codec)?, track)?;
        Self::open_with_format(track, &format)
    }

    pub(crate) fn open_with_format(track: &TrackInfo, format: &OwnedFormat) -> DecodeResult<Self> {
        let input_mime = format.get_str(KEY_MIME).ok_or(DecodeError::InvalidData {
            detail: "track format has no input MIME",
        })?;
        if input_mime != codec_mime(track.codec)?
            && !(track.codec == AudioCodec::Flac && input_mime == MIME_RAW)
        {
            return Err(DecodeError::InvalidData {
                detail: "extractor track codec disagrees with declared media information",
            });
        }
        let codec = OwnedCodec::create_with_format(format)?;
        let output = OutputFormat::read(&codec.output_format()?)?;
        let spec = output_spec(&output)?;
        let pcm_encoding = output.pcm_encoding;

        Ok(Self {
            codec,
            spec,
            pcm_encoding,
            drain: DrainState::Feeding,
            decoded_pts: Duration::ZERO,
            track_info: DecoderTrackInfo {
                gapless: track.gapless,
                ..DecoderTrackInfo::default()
            },
        })
    }

    /// Whether `MediaCodec` accepts this codec at the codec layer alone
    /// (i.e. without an extractor providing per-track metadata).
    ///
    /// Container support is decided separately by the decoder factory.
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        codec_mime(codec).is_ok()
    }
}

impl FrameCodec for AndroidCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        _packet_desc: &[u8],
        out: &mut SampleBuffer,
    ) -> DecodeResult<u32> {
        out.clear();
        if matches!(self.drain, DrainState::Finished) {
            return Ok(0);
        }
        if matches!(self.drain, DrainState::Feeding) {
            let mut buf = self.await_input_buffer(out)?;
            let dst = buf.data_mut();
            if frame_data.len() > dst.len() {
                return Err(DecodeError::InvalidData {
                    detail: "encoded packet exceeds MediaCodec input capacity",
                });
            }
            dst[..frame_data.len()].copy_from_slice(frame_data);
            let end_of_stream = frame_data.is_empty();
            self.codec.queue_input_buffer(QueueInput {
                index: buf.index,
                size: frame_data.len(),
                presentation_time_us: i64::try_from(pts.as_micros()).unwrap_or(i64::MAX),
                flags: if end_of_stream {
                    MEDIA_CODEC_BUFFER_FLAG_END_OF_STREAM
                } else {
                    0
                },
            })?;
            if end_of_stream {
                self.drain = DrainState::Draining;
            }
        }
        let timeout = if matches!(self.drain, DrainState::Draining) {
            consts::DRAIN_DEQUEUE_TIMEOUT_US
        } else {
            consts::OUTPUT_DEQUEUE_TIMEOUT_US
        };
        self.read_output(out, timeout)
    }

    fn decoded_pts(&self) -> Option<Duration> {
        Some(self.decoded_pts)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.codec.flush()?;
        self.drain = DrainState::Feeding;
        self.decoded_pts = Duration::ZERO;
        Ok(())
    }

    fn needs_eof_drain(&self, _source_sample_rate: u32) -> bool {
        true
    }

    fn priming(&self, codec: AudioCodec) -> CodecPriming {
        match codec {
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => CodecPriming {
                packets: 2,
                ..CodecPriming::default()
            },
            _ => CodecPriming::default(),
        }
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

impl AndroidCodec {
    /// The codec frees input only as its output pool drains, so each miss
    /// takes the finished output before polling again.
    fn await_input_buffer(&mut self, out: &mut SampleBuffer) -> DecodeResult<InputBuffer> {
        let mut waited_us = 0;
        loop {
            if let Some(buf) = self
                .codec
                .dequeue_input_buffer(consts::INPUT_DEQUEUE_TIMEOUT_US)?
            {
                return Ok(buf);
            }
            waited_us += consts::INPUT_DEQUEUE_TIMEOUT_US;
            if waited_us >= consts::INPUT_WAIT_BUDGET_US {
                return Err(AndroidBackendError::operation(
                    "codec-input-backpressure",
                    "input packet was not consumed",
                )
                .into());
            }
            self.read_output(out, consts::NO_WAIT_US)?;
        }
    }

    fn quantized_pts(&self, presentation_time_us: i64) -> DecodeResult<Duration> {
        let timestamp = Duration::from_micros(
            u64::try_from(presentation_time_us).map_err(DecodeError::backend)?,
        );
        let frame = self
            .spec
            .frame_at(timestamp)
            .map_err(DecodeError::backend)?;
        self.spec.duration_for(frame).map_err(DecodeError::backend)
    }

    /// Appends every finished output buffer to `out`; only the first
    /// dequeue waits. Returns the frames `out` holds.
    fn read_output(&mut self, out: &mut SampleBuffer, timeout_us: i64) -> DecodeResult<u32> {
        let draining = matches!(self.drain, DrainState::Draining);
        let mut timeout = timeout_us;
        loop {
            match self.codec.dequeue_output_buffer(timeout)? {
                DequeueOutput::Output(buffer) => {
                    let start = out.len();
                    let result = match self.pcm_encoding {
                        AndroidPcmEncoding::Pcm16 => append_pcm16(buffer.data(), out),
                        AndroidPcmEncoding::Float => append_pcm_float(buffer.data(), out),
                    };
                    let presentation_time_us = buffer.presentation_time_us;
                    if buffer.end_of_stream {
                        self.drain = DrainState::Finished;
                    }
                    self.codec.release_output_buffer(buffer.index)?;
                    result?;
                    if start == 0 && !out.is_empty() {
                        self.decoded_pts = self.quantized_pts(presentation_time_us)?;
                    }
                    if matches!(self.drain, DrainState::Finished) {
                        break;
                    }
                    timeout = consts::NO_WAIT_US;
                }
                DequeueOutput::OutputFormatChanged(format) => {
                    self.spec = output_spec(&format)?;
                    self.pcm_encoding = format.pcm_encoding;
                }
                DequeueOutput::TryAgainLater => {
                    if draining && timeout != consts::NO_WAIT_US && out.is_empty() {
                        return Err(AndroidBackendError::operation(
                            "codec-drain",
                            "timed out before end of output",
                        )
                        .into());
                    }
                    break;
                }
            }
        }
        u32::try_from(out.len() / usize::from(self.spec.channels)).map_err(DecodeError::backend)
    }
}

fn codec_mime(codec: AudioCodec) -> DecodeResult<&'static std::ffi::CStr> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Ok(MIME_AAC),
        AudioCodec::Flac => Ok(MIME_FLAC),
        AudioCodec::Pcm => Ok(MIME_RAW),
        AudioCodec::Mp3 => Ok(MIME_MP3),
        AudioCodec::Alac => Ok(MIME_ALAC),
        codec => Err(DecodeError::UnsupportedCodec { codec }),
    }
}

fn build_format(
    mime: &std::ffi::CStr,
    track: &TrackInfo,
) -> Result<OwnedFormat, AndroidBackendError> {
    let mut format = OwnedFormat::new()?;
    format.set_str(KEY_MIME, mime);

    let sample_rate = i32::try_from(track.sample_rate).map_err(|_| {
        AndroidBackendError::operation(
            "media-format-sample-rate",
            format!("rate={} out of range", track.sample_rate),
        )
    })?;
    let channels = i32::from(track.channels);
    format.set_i32(KEY_SAMPLE_RATE, sample_rate);
    format.set_i32(KEY_CHANNEL_COUNT, channels);
    format.set_i32(KEY_PCM_ENCODING, PCM_ENCODING_16BIT);

    if !track.extra_data.is_empty() {
        let flac_config;
        let config = if track.codec == AudioCodec::Flac {
            let streaminfo: &[u8; 34] = track.extra_data.as_slice().try_into().map_err(|_| {
                AndroidBackendError::operation("flac-config", "expected a 34-byte STREAMINFO")
            })?;
            flac_config = [b"fLaC\x80\x00\x00\x22".as_slice(), streaminfo].concat();
            flac_config.as_slice()
        } else {
            track.extra_data.as_slice()
        };
        format.set_buffer(KEY_CSD_0, config);
    }

    Ok(format)
}

fn append_pcm16(bytes: &[u8], out: &mut SampleBuffer) -> DecodeResult<()> {
    let start = out.len();
    let end = start + bytes.len() / 2;
    out.ensure_len(end)?;
    for (dst, chunk) in out[start..end].iter_mut().zip(bytes.chunks_exact(2)) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        *dst = f32::from(s) / consts::PCM16_SCALE;
    }
    out.truncate(end);
    Ok(())
}

fn append_pcm_float(bytes: &[u8], out: &mut SampleBuffer) -> DecodeResult<()> {
    let start = out.len();
    let end = start + bytes.len() / 4;
    out.ensure_len(end)?;
    out[start..end]
        .iter_mut()
        .zip(bytes.chunks_exact(4))
        .for_each(|(dst, chunk)| {
            *dst = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        });
    out.truncate(end);
    Ok(())
}
