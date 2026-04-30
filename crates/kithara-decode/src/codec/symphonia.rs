//! `SymphoniaCodec` — generic [`FrameCodec`] over `symphonia`'s codec
//! registry.
//!
//! Wraps `Box<dyn symphonia::core::codecs::audio::AudioDecoder>` built
//! from a [`TrackInfo`]. Codec id is selected from
//! [`TrackInfo::codec`]; codec-specific extra data
//! (`AudioSpecificConfig` for AAC, `STREAMINFO` for FLAC, ESDS cookie
//! for ALAC, etc.) flows through `TrackInfo::extra_data`.
//!
//! PCM and ADPCM tracks need the specific bit-depth / endian variant
//! which our generic [`AudioCodec`] enum does not encode. For those
//! cases, the factory uses [`SymphoniaCodec::open_native`], passing the
//! original `AudioCodecParameters` straight through from
//! [`crate::demuxer::SymphoniaDemuxer::native_params`].

use std::time::Duration;

use kithara_stream::AudioCodec;
use symphonia::core::{
    audio::Channels,
    codecs::{
        CodecProfile,
        audio::{
            AudioCodecId, AudioCodecParameters, AudioDecoder, AudioDecoderOptions,
            well_known::{
                CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_OPUS,
                CODEC_ID_VORBIS,
                profiles::{CODEC_PROFILE_AAC_HE, CODEC_PROFILE_AAC_HE_V2, CODEC_PROFILE_AAC_LC},
            },
        },
        registry::CodecRegistry,
    },
    errors::Error as SymphoniaError,
    packet::Packet,
    units::{Duration as PktDuration, Timestamp},
};

use crate::{
    codec::{DecodedFrame, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::PcmSpec,
};

const TRACK_ID: u32 = 0;

/// Frame codec backed by a symphonia codec registry decoder.
pub struct SymphoniaCodec {
    decoder: Box<dyn AudioDecoder>,
    spec: PcmSpec,
}

impl SymphoniaCodec {
    /// Whether `SymphoniaCodec::open` can accept this codec via
    /// [`TrackInfo`] alone. `AudioCodec::Pcm` / `AudioCodec::Adpcm` need
    /// bit-depth + endianness which the generic enum does not encode —
    /// the factory routes those through [`Self::open_native`] using the
    /// demuxer's native `AudioCodecParameters` instead.
    #[must_use]
    pub fn supports(codec: AudioCodec) -> bool {
        !matches!(codec, AudioCodec::Pcm | AudioCodec::Adpcm)
    }

    /// Build a [`SymphoniaCodec`] from native Symphonia codec
    /// parameters. Used by the factory when wiring a
    /// [`crate::demuxer::SymphoniaDemuxer`] for PCM/ADPCM tracks where
    /// the generic [`TrackInfo::codec`] field cannot describe the
    /// concrete sample format.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Backend`] when the Symphonia codec
    /// registry cannot build a decoder for the supplied parameters.
    pub fn open_native(params: &AudioCodecParameters) -> DecodeResult<Self> {
        let registry: &CodecRegistry = symphonia::default::get_codecs();
        let opts = AudioDecoderOptions::default();
        let decoder = registry
            .make_audio_decoder(params, &opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let sample_rate = params.sample_rate.ok_or_else(|| {
            DecodeError::InvalidData("symphonia native params missing sample rate".into())
        })?;
        let channels = params
            .channels
            .as_ref()
            .map_or(2, |c| u16::try_from(c.count()).unwrap_or(2));
        let spec = PcmSpec {
            channels,
            sample_rate,
        };
        Ok(Self { decoder, spec })
    }
}

impl FrameCodec for SymphoniaCodec {
    fn open(track: &TrackInfo) -> DecodeResult<Self> {
        let (codec_id, profile) = map_codec(track.codec)?;
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(codec_id)
            .with_sample_rate(track.sample_rate);
        if let Some(profile) = profile {
            params.with_profile(profile);
        }
        params.with_channels(Channels::Discrete(track.channels));
        if !track.extra_data.is_empty() {
            params.with_extra_data(track.extra_data.clone().into_boxed_slice());
        }

        let registry: &CodecRegistry = symphonia::default::get_codecs();
        let opts = AudioDecoderOptions::default();
        let decoder = registry
            .make_audio_decoder(&params, &opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let spec = PcmSpec {
            channels: track.channels,
            sample_rate: track.sample_rate,
        };
        Ok(Self { decoder, spec })
    }

    fn decode_frame(&mut self, frame_data: &[u8], pts: Duration) -> DecodeResult<DecodedFrame> {
        let pts_ticks = duration_to_ticks(pts, self.spec.sample_rate);
        let packet_pts = Timestamp::new(i64::try_from(pts_ticks).unwrap_or(i64::MAX));
        let packet = Packet::new(
            TRACK_ID,
            packet_pts,
            PktDuration::new(0),
            frame_data.to_vec(),
        );

        let decoded = match self.decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(err)) => {
                tracing::debug!(error = %err, "SymphoniaCodec: skipping undecodable frame");
                return Ok(DecodedFrame {
                    samples: Vec::new(),
                    frames: 0,
                });
            }
            Err(SymphoniaError::ResetRequired) => {
                self.decoder.reset();
                return Ok(DecodedFrame {
                    samples: Vec::new(),
                    frames: 0,
                });
            }
            Err(e) => return Err(DecodeError::Backend(Box::new(e))),
        };

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
        Ok(DecodedFrame { samples, frames })
    }

    fn flush(&mut self) {
        self.decoder.reset();
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }
}

fn map_codec(codec: AudioCodec) -> DecodeResult<(AudioCodecId, Option<CodecProfile>)> {
    match codec {
        AudioCodec::AacLc => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_LC))),
        AudioCodec::AacHe => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_HE))),
        AudioCodec::AacHeV2 => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_HE_V2))),
        AudioCodec::Flac => Ok((CODEC_ID_FLAC, None)),
        AudioCodec::Mp3 => Ok((CODEC_ID_MP3, None)),
        AudioCodec::Alac => Ok((CODEC_ID_ALAC, None)),
        AudioCodec::Opus => Ok((CODEC_ID_OPUS, None)),
        AudioCodec::Vorbis => Ok((CODEC_ID_VORBIS, None)),
        AudioCodec::Pcm | AudioCodec::Adpcm => Err(DecodeError::UnsupportedCodec(codec)),
    }
}

fn duration_to_ticks(d: Duration, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let secs = d.as_secs();
    let subsec_nanos = u64::from(d.subsec_nanos());
    secs.saturating_mul(u64::from(sample_rate))
        .saturating_add(subsec_nanos.saturating_mul(u64::from(sample_rate)) / 1_000_000_000)
}
