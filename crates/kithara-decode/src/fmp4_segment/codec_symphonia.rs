//! Symphonia-based frame codecs for [`super::Fmp4SegmentDecoder`].
//!
//! Each codec wraps a [`symphonia::core::codecs::audio::AudioDecoder`]
//! built from raw codec parameters (codec id + `extra_data`) extracted
//! from the init segment by the demuxer. The codec consumes one
//! [`super::demux::Fmp4Frame`] at a time and emits interleaved PCM via
//! [`super::codec::DecodedFrame`].

use kithara_stream::AudioCodec;
use symphonia::core::{
    audio::Channels,
    codecs::{
        CodecProfile,
        audio::{
            AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions,
            well_known::{CODEC_ID_AAC, CODEC_ID_FLAC, profiles::CODEC_PROFILE_AAC_LC},
        },
        registry::CodecRegistry,
    },
    errors::Error as SymphoniaError,
    packet::Packet,
    units::{Duration as PktDuration, Timestamp},
};

use crate::{
    error::{DecodeError, DecodeResult},
    fmp4_segment::{
        codec::{DecodedFrame, FrameCodec},
        demux::{CodecConfig, Fmp4Frame, Fmp4InitInfo},
    },
    types::PcmSpec,
};

const TRACK_ID: u32 = 0;

fn build_decoder(
    codec_id: AudioCodecId,
    profile: Option<CodecProfile>,
    init: &Fmp4InitInfo,
    extra_data: &[u8],
) -> DecodeResult<(Box<dyn AudioDecoder>, PcmSpec)> {
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(codec_id)
        .with_sample_rate(init.sample_rate);
    if let Some(profile) = profile {
        params.with_profile(profile);
    }
    params.with_channels(Channels::Discrete(init.channels));
    params.with_extra_data(extra_data.to_vec().into_boxed_slice());

    let registry: &CodecRegistry = symphonia::default::get_codecs();
    let opts = AudioDecoderOptions::default();
    let decoder = registry
        .make_audio_decoder(&params, &opts)
        .map_err(|e| DecodeError::Backend(Box::new(e)))?;

    let spec = PcmSpec {
        channels: init.channels,
        sample_rate: init.sample_rate,
    };
    Ok((decoder, spec))
}

fn decode_one_frame(
    decoder: &mut dyn AudioDecoder,
    frame: &Fmp4Frame,
    frame_data: &[u8],
) -> DecodeResult<DecodedFrame> {
    let pts = Timestamp::new(i64::try_from(frame.decode_time).unwrap_or(i64::MAX));
    let dur = PktDuration::new(u64::from(frame.duration));
    let packet = Packet::new(TRACK_ID, pts, dur, frame_data.to_vec());

    let decoded = match decoder.decode(&packet) {
        Ok(d) => d,
        Err(SymphoniaError::DecodeError(err)) => {
            tracing::debug!(error = %err, "fmp4_segment: skipping undecodable frame");
            return Ok(DecodedFrame {
                samples: Vec::new(),
                frames: 0,
            });
        }
        Err(SymphoniaError::ResetRequired) => {
            decoder.reset();
            return Ok(DecodedFrame {
                samples: Vec::new(),
                frames: 0,
            });
        }
        Err(e) => return Err(DecodeError::Backend(Box::new(e))),
    };

    let spec = decoded.spec();
    let channels = spec.channels().count();
    let num_samples = decoded.samples_interleaved();
    if num_samples == 0 {
        return Ok(DecodedFrame {
            samples: Vec::new(),
            frames: 0,
        });
    }

    let mut samples = vec![0.0f32; num_samples];
    decoded.copy_to_slice_interleaved(&mut samples[..]);
    let frames = u32::try_from(decoded.frames()).unwrap_or(u32::MAX);
    let _ = channels;
    Ok(DecodedFrame { samples, frames })
}

/// AAC frame codec backed by Symphonia's AAC decoder.
pub(crate) struct SymphoniaAacCodec {
    decoder: Box<dyn AudioDecoder>,
    spec: PcmSpec,
}

impl FrameCodec for SymphoniaAacCodec {
    fn open(init: &Fmp4InitInfo) -> DecodeResult<Self> {
        if !matches!(init.codec, AudioCodec::AacLc) {
            return Err(DecodeError::UnsupportedCodec(init.codec));
        }
        let CodecConfig::Aac(asc) = &init.config else {
            return Err(DecodeError::InvalidData(
                "SymphoniaAacCodec requires AAC AudioSpecificConfig".into(),
            ));
        };
        let (decoder, spec) = build_decoder(CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_LC), init, asc)?;
        Ok(Self { decoder, spec })
    }

    fn decode_frame(&mut self, frame: &Fmp4Frame, frame_data: &[u8]) -> DecodeResult<DecodedFrame> {
        decode_one_frame(self.decoder.as_mut(), frame, frame_data)
    }

    fn flush(&mut self) {
        self.decoder.reset();
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

/// FLAC frame codec backed by `symphonia-bundle-flac`.
pub(crate) struct SymphoniaFlacCodec {
    decoder: Box<dyn AudioDecoder>,
    spec: PcmSpec,
}

impl FrameCodec for SymphoniaFlacCodec {
    fn open(init: &Fmp4InitInfo) -> DecodeResult<Self> {
        if !matches!(init.codec, AudioCodec::Flac) {
            return Err(DecodeError::UnsupportedCodec(init.codec));
        }
        let CodecConfig::Flac(streaminfo) = &init.config else {
            return Err(DecodeError::InvalidData(
                "SymphoniaFlacCodec requires FLAC STREAMINFO".into(),
            ));
        };
        let (decoder, spec) = build_decoder(CODEC_ID_FLAC, None, init, streaminfo)?;
        Ok(Self { decoder, spec })
    }

    fn decode_frame(&mut self, frame: &Fmp4Frame, frame_data: &[u8]) -> DecodeResult<DecodedFrame> {
        decode_one_frame(self.decoder.as_mut(), frame, frame_data)
    }

    fn flush(&mut self) {
        self.decoder.reset();
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}
