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

use kithara_bufpool::PcmBuf;
use kithara_platform::time::Duration;
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
    codec::FrameCodec,
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    symphonia::config::SymphoniaConfig,
    types::{DecoderTrackInfo, PcmSpec},
};

const TRACK_ID: u32 = 0;

/// Frame codec backed by a symphonia codec registry decoder.
pub(crate) struct SymphoniaCodec {
    decoder: Box<dyn AudioDecoder>,
    spec: PcmSpec,
    /// Decoder-owned playback contract. Populated from container-level
    /// gapless metadata captured by the demuxer before the codec is
    /// opened; left empty otherwise.
    track_info: DecoderTrackInfo,
}

impl SymphoniaCodec {
    /// Build a [`SymphoniaCodec`] from native Symphonia codec
    /// parameters. Used by the factory when wiring a
    /// [`crate::symphonia::SymphoniaDemuxer`] for PCM/ADPCM tracks where
    /// the generic [`TrackInfo::codec`] field cannot describe the
    /// concrete sample format.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::Backend`] when the Symphonia codec
    /// registry cannot build a decoder for the supplied parameters.
    pub(crate) fn open_native(params: &AudioCodecParameters) -> DecodeResult<Self> {
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
        Ok(Self {
            decoder,
            spec,
            track_info: DecoderTrackInfo::default(),
        })
    }

    /// Whether `SymphoniaCodec::open` can accept this codec via
    /// [`TrackInfo`] alone. `AudioCodec::Pcm` / `AudioCodec::Adpcm` need
    /// bit-depth + endianness which the generic enum does not encode —
    /// the factory routes those through [`Self::open_native`] using the
    /// demuxer's native `AudioCodecParameters` instead.
    #[must_use]
    pub(crate) fn supports(codec: AudioCodec) -> bool {
        !matches!(codec, AudioCodec::Pcm | AudioCodec::Adpcm)
    }

    /// Build a [`SymphoniaCodec`] from `TrackInfo` with extra options
    /// from [`SymphoniaConfig`]. Currently only [`SymphoniaConfig::gapless`]
    /// matters: when set, `AudioDecoderOptions::gapless = true` so the
    /// Symphonia codec emits priming-trimmed output internally for codecs
    /// that report it (FLAC, Opus, Vorbis).
    ///
    /// `FrameCodec::open` keeps its no-config shape so existing
    /// `UniversalDecoder<D, C>` callers don't break.
    ///
    /// # Errors
    ///
    /// Same as [`FrameCodec::open`].
    pub(crate) fn open_with_config(
        track: &TrackInfo,
        config: &SymphoniaConfig,
    ) -> DecodeResult<Self> {
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
        let mut opts = AudioDecoderOptions::default();
        opts.gapless = config.gapless;
        // Gapless source-of-truth precedence:
        //   1. Container/encoder-probed `track.gapless` (factory's
        //      `probe_codec_gapless` from MP4 `udta`/`elst` for AAC,
        //      Xing/Info+LAME for MP3) — used verbatim, no extra fold.
        //      The probed leading already includes the encoder priming.
        //   2. Conservative codec default `codec_priming_frames(codec)` —
        //      fallback when no container metadata was probed (raw
        //      streams, stripped tags). MP3 falls back to LAME default
        //      (1105), Opus to RFC 7845 (312), others to 0.
        // Adding (1) on top of (2) double-counts the priming and trims
        // 1+ second of real audio at track start.
        let track_gapless = track.gapless.or_else(|| {
            let extra = crate::gapless::codec_priming_frames(track.codec);
            (extra > 0).then_some(crate::GaplessInfo {
                leading_frames: extra,
                trailing_frames: 0,
            })
        });
        let decoder = registry
            .make_audio_decoder(&params, &opts)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;

        let spec = PcmSpec {
            channels: track.channels,
            sample_rate: track.sample_rate,
        };
        // Carry container-level gapless info (when the demuxer surfaced
        // `iTunSMPB` / `elst`) into `DecoderTrackInfo` so downstream
        // gapless trim picks it up. Codecs whose priming Symphonia
        // strips internally (Vorbis/Opus via `AudioDecoderOptions::gapless`)
        // leave `track.gapless` as `None` and report nothing here.
        Ok(Self {
            decoder,
            spec,
            track_info: DecoderTrackInfo {
                gapless: track_gapless,
                ..DecoderTrackInfo::default()
            },
        })
    }
}

impl FrameCodec for SymphoniaCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
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
                out.clear();
                return Ok(0);
            }
            Err(SymphoniaError::ResetRequired) => {
                self.decoder.reset();
                out.clear();
                return Ok(0);
            }
            Err(e) => return Err(DecodeError::Backend(Box::new(e))),
        };

        let num_samples = decoded.samples_interleaved();
        if num_samples == 0 {
            out.clear();
            return Ok(0);
        }

        out.ensure_len(num_samples)
            .map_err(|e| DecodeError::Backend(Box::new(e)))?;
        decoded.copy_to_slice_interleaved(&mut out[..num_samples]);
        out.truncate(num_samples);
        Ok(u32::try_from(decoded.frames()).unwrap_or(u32::MAX))
    }

    fn flush(&mut self) {
        self.decoder.reset();
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }

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
        Ok(Self {
            decoder,
            spec,
            track_info: DecoderTrackInfo::default(),
        })
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
