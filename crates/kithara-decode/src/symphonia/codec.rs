use std::num::NonZeroU32;

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
    packet::PacketRef,
    units::{Duration as PktDuration, Timestamp},
};

use crate::{
    GaplessInfo,
    codec::{CodecPriming, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    symphonia::config::SymphoniaConfig,
    types::{DecoderTrackInfo, PcmSpec},
};

/// Module-scoped constants for [`SymphoniaCodec`].
struct Consts;

impl Consts {
    /// LAME-convention decoder algorithmic delay for the `mpa` MP3
    /// decoder (528 polyphase synthesis filter convergence + 1 sync
    /// sample). See [`crate::codec::FrameCodec::decoder_algo_delay`]
    /// + `kithara-decode/README.md` "Gapless probe contract".
    const MP3_DECODER_DELAY: u64 = 528 + 1;

    /// Symphonia packets are unidimensional (single audio track) so
    /// we pin `track_id` = 0 — Symphonia uses the field for routing
    /// across multiplexed streams that we never produce.
    const TRACK_ID: u32 = 0;
}

/// Frame codec backed by a symphonia codec registry decoder.
pub(crate) struct SymphoniaCodec {
    decoder: Box<dyn AudioDecoder>,
    /// Decoder-owned playback contract. Populated from container-level
    /// gapless metadata captured by the demuxer before the codec is
    /// opened; left empty otherwise.
    track_info: DecoderTrackInfo,
    spec: PcmSpec,
    /// One-shot guard for first-frame diagnostic log — compares the
    /// declared [`PcmSpec`] (from container `TrackInfo`) against the
    /// actual `decoded.spec()` returned by the codec. Catches SBR/PS
    /// rate-doubling (HE-AAC v2: container declares core rate, decoder
    /// outputs upsampled rate) without flooding the log.
    logged_first_frame: bool,
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
        let registry: &CodecRegistry = crate::symphonia::registry::get_codecs();
        let opts = AudioDecoderOptions::default();
        let decoder = registry
            .make_audio_decoder(params, &opts)
            .map_err(DecodeError::backend)?;

        let raw_rate = params.sample_rate.ok_or_else(|| {
            DecodeError::InvalidData("symphonia native params missing sample rate".into())
        })?;
        let channels = params
            .channels
            .as_ref()
            .map_or(2, |c| u16::try_from(c.count()).unwrap_or(2));
        let spec = PcmSpec::checked(channels, raw_rate, "symphonia.codec.native")?;
        Ok(Self {
            decoder,
            spec,
            track_info: DecoderTrackInfo::default(),
            logged_first_frame: false,
        })
    }

    /// Build a [`SymphoniaCodec`] from `TrackInfo` with extra options
    /// from [`SymphoniaConfig`]. Currently only [`SymphoniaConfig::gapless`]
    /// matters: when set, `AudioDecoderOptions::gapless = true` so the
    /// Symphonia codec emits priming-trimmed output internally for codecs
    /// that report it (FLAC, Opus, Vorbis).
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::UnsupportedCodec`] when Symphonia has no
    /// registered decoder for the track's codec id, and `DecodeError::Backend`
    /// when codec instantiation fails.
    pub(crate) fn open_with_config(
        track: &TrackInfo,
        config: &SymphoniaConfig,
    ) -> DecodeResult<Self> {
        let (codec_id, profile) = map_codec(track.codec)?;
        tracing::info!(
            target: "kithara_decode::symphonia::codec",
            codec = ?track.codec,
            sample_rate = track.sample_rate,
            channels = track.channels,
            extra_data_len = track.extra_data.len(),
            gapless_cfg = config.gapless,
            "SymphoniaCodec::open_with_config — TrackInfo"
        );
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

        let registry: &CodecRegistry = crate::symphonia::registry::get_codecs();
        let mut opts = AudioDecoderOptions::default();
        opts.gapless = config.gapless;
        let decoder = registry
            .make_audio_decoder(&params, &opts)
            .map_err(DecodeError::backend)?;
        let algo_delay = symphonia_decoder_algo_delay(track.codec);
        let track_gapless = track
            .gapless
            .map(|info| apply_decoder_algo_delay(info, algo_delay));

        let spec = PcmSpec::checked(track.channels, track.sample_rate, "symphonia.codec.track")?;
        Ok(Self {
            decoder,
            spec,
            track_info: DecoderTrackInfo {
                gapless: track_gapless,
                ..DecoderTrackInfo::default()
            },
            logged_first_frame: false,
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
}

impl FrameCodec for SymphoniaCodec {
    fn decode_frame(
        &mut self,
        frame_data: &[u8],
        pts: Duration,
        _packet_desc: &[u8],
        out: &mut PcmBuf,
    ) -> DecodeResult<u32> {
        let pts_ticks = duration_to_ticks(pts, self.spec.sample_rate.get());
        let packet_pts = Timestamp::new(i64::try_from(pts_ticks).unwrap_or(i64::MAX));
        let packet_ref = PacketRef::new(
            Consts::TRACK_ID,
            packet_pts,
            PktDuration::new(0),
            frame_data,
        );

        let decoded = match self.decoder.decode_ref(&packet_ref) {
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
            Err(e) => return Err(DecodeError::backend(e)),
        };

        let num_samples = decoded.samples_interleaved();
        let actual = decoded.spec();
        let actual_rate = actual.rate();
        let actual_channels =
            u16::try_from(actual.channels().count()).unwrap_or(self.spec.channels);
        if !self.logged_first_frame {
            tracing::info!(
                target: "kithara_decode::symphonia::codec",
                declared_rate = self.spec.sample_rate.get(),
                declared_channels = self.spec.channels,
                actual_rate,
                actual_channels,
                decoded_frames = decoded.frames(),
                num_samples_interleaved = num_samples,
                pts_ticks,
                "SymphoniaCodec::decode_frame — first frame snapshot"
            );
            self.logged_first_frame = true;
        }
        if let Some(nz_actual_rate) = NonZeroU32::new(actual_rate)
            && (self.spec.sample_rate != nz_actual_rate || self.spec.channels != actual_channels)
        {
            tracing::debug!(
                target: "kithara_decode::symphonia::codec",
                old_rate = self.spec.sample_rate.get(),
                old_channels = self.spec.channels,
                new_rate = actual_rate,
                new_channels = actual_channels,
                "SymphoniaCodec: live spec update from decoder output"
            );
            self.spec = PcmSpec::new(actual_channels, nz_actual_rate);
        }
        if num_samples == 0 {
            out.clear();
            return Ok(0);
        }

        out.ensure_len(num_samples)?;
        decoded.copy_to_slice_interleaved(&mut out[..num_samples]);
        out.truncate(num_samples);
        Ok(u32::try_from(decoded.frames()).unwrap_or(u32::MAX))
    }

    fn decoder_algo_delay(&self, codec: AudioCodec) -> u64 {
        symphonia_decoder_algo_delay(codec)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.decoder.reset();
        Ok(())
    }

    fn priming(&self, codec: AudioCodec) -> CodecPriming {
        // WHY: AAC (incl. HE-AAC seen as AacLc) requests 2 AU of SBR/PS QMF pre-roll after flush (README "Seek pre-roll and trim").
        match codec {
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => CodecPriming {
                packets: 2,
                ..CodecPriming::default()
            },
            _ => CodecPriming::default(),
        }
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

fn symphonia_decoder_algo_delay(codec: AudioCodec) -> u64 {
    match codec {
        AudioCodec::Mp3 => Consts::MP3_DECODER_DELAY,
        _ => 0,
    }
}

fn apply_decoder_algo_delay(info: GaplessInfo, algo_delay: u64) -> GaplessInfo {
    if algo_delay == 0 {
        return info;
    }
    GaplessInfo {
        leading_frames: info.leading_frames.saturating_add(algo_delay),
        trailing_frames: info.trailing_frames.saturating_sub(algo_delay),
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
    let secs = d.as_secs();
    let subsec_nanos = u64::from(d.subsec_nanos());
    secs.saturating_mul(u64::from(sample_rate))
        .saturating_add(subsec_nanos.saturating_mul(u64::from(sample_rate)) / 1_000_000_000)
}

#[cfg(test)]
mod tests {
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::SymphoniaCodec;
    use crate::{
        codec::{CodecPriming, FrameCodec},
        demuxer::TrackInfo,
        error::DecodeError,
        symphonia::config::SymphoniaConfig,
    };

    fn mp3_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::Mp3,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    fn zero_rate_track() -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::Mp3,
            sample_rate: 0,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless: None,
        }
    }

    /// A zero sample rate in `TrackInfo` must be rejected with
    /// `DecodeError::InvalidSampleRate` instead of silently building a
    /// `PcmSpec { sample_rate: 0 }`.
    #[kithara::test]
    fn open_with_config_zero_rate_returns_invalid_sample_rate() {
        match SymphoniaCodec::open_with_config(&zero_rate_track(), &SymphoniaConfig::default()) {
            Err(DecodeError::InvalidSampleRate { .. }) => {}
            Err(other) => panic!("expected InvalidSampleRate, got {other:?}"),
            Ok(_) => panic!("zero sample rate must be rejected"),
        }
    }

    #[kithara::test]
    fn symphonia_priming_warms_aac_and_defaults_elsewhere() {
        let codec = SymphoniaCodec::open_with_config(&mp3_track(), &SymphoniaConfig::default())
            .expect("BUG: MP3 codec open");
        // WHY: AAC (incl. HE-AAC seen as AacLc) requests 2-AU SBR pre-roll (README "Seek pre-roll and trim").
        for c in [AudioCodec::AacLc, AudioCodec::AacHe, AudioCodec::AacHeV2] {
            assert_eq!(
                codec.priming(c),
                CodecPriming {
                    packets: 2,
                    ..CodecPriming::default()
                },
                "{c:?} must request SBR pre-roll"
            );
        }
        // WHY: codecs Symphonia primes internally keep the empty contract.
        for c in [AudioCodec::Mp3, AudioCodec::Flac, AudioCodec::Pcm] {
            assert_eq!(
                codec.priming(c),
                CodecPriming::default(),
                "symphonia handles {c:?} priming internally"
            );
        }
    }
}
