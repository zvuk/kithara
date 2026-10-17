use std::num::NonZeroU32;

use kithara_bufpool::SampleBuffer;
use kithara_platform::time::Duration;
use kithara_signal::AudioSpec;
use kithara_stream::AudioCodec;
use kithara_test_utils::kithara;
use symphonia::core::{
    audio::{Channels, GenericAudioBufferRef},
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
    codec::{CodecPriming, FrameCodec, access_unit_frames},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    symphonia::SymphoniaConfig,
    types::{DecoderTrackInfo, checked_audio_spec},
};

/// Module-scoped constants for [`SymphoniaCodec`].
mod consts {
    /// LAME-convention decoder algorithmic delay for the `mpa` MP3 decoder (528
    /// polyphase synthesis filter convergence + 1 sync sample). See
    /// [`crate::codec::FrameCodec::decoder_algo_delay`].
    pub(super) const MP3_DECODER_DELAY: u64 = 528 + 1;

    /// Symphonia packets are unidimensional (single audio track) so
    /// we pin `track_id` = 0 — Symphonia uses the field for routing
    /// across multiplexed streams that we never produce.
    pub(super) const TRACK_ID: u32 = 0;
}

/// Frame codec backed by a symphonia codec registry decoder.
pub(crate) struct SymphoniaCodec {
    spec: AudioSpec,
    decoder: Box<dyn AudioDecoder>,
    /// Decoder-owned playback contract. Populated from container-level
    /// gapless metadata captured by the demuxer before the codec is
    /// opened; left empty otherwise.
    track_info: DecoderTrackInfo,
    codec: Option<AudioCodec>,
    /// One-shot guard for first-frame diagnostic log — compares the
    /// declared [`AudioSpec`] (from container `TrackInfo`) against the
    /// actual `decoded.spec()` returned by the codec. Catches SBR/PS
    /// rate-doubling (HE-AAC v2: container declares core rate, decoder
    /// outputs upsampled rate) without flooding the log.
    logged_first_frame: bool,
    aac_timing: Option<super::timing::Timing>,
    decoded_offset: usize,
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
        let registry: &CodecRegistry = super::registry::get_codecs();
        let opts = AudioDecoderOptions::default();
        let decoder = registry
            .make_audio_decoder(params, &opts)
            .map_err(DecodeError::backend)?;

        let raw_rate = params.sample_rate.ok_or_else(|| DecodeError::InvalidData {
            detail: "symphonia native params missing sample rate",
        })?;
        let channels = params
            .channels
            .as_ref()
            .map_or(2, |c| u16::try_from(c.count()).unwrap_or(2));
        let spec = checked_audio_spec(channels, raw_rate, "symphonia.codec.native")?;
        Ok(Self {
            decoder,
            spec,
            codec: None,
            track_info: DecoderTrackInfo::default(),
            logged_first_frame: false,
            aac_timing: None,
            decoded_offset: 0,
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

        let registry: &CodecRegistry = super::registry::get_codecs();
        let mut opts = AudioDecoderOptions::default();
        opts.gapless = config.gapless;
        let decoder = registry
            .make_audio_decoder(&params, &opts)
            .map_err(DecodeError::backend)?;
        let algo_delay = symphonia_decoder_algo_delay(track.codec);
        let track_gapless = track
            .gapless
            .map(|info| apply_decoder_algo_delay(info, algo_delay));

        let prepared_rate = decoder
            .codec_params()
            .sample_rate
            .unwrap_or(track.sample_rate);
        let spec = checked_audio_spec(track.channels, prepared_rate, "symphonia.codec.track")?;
        Ok(Self {
            decoder,
            spec,
            codec: Some(track.codec),
            track_info: DecoderTrackInfo {
                gapless: track_gapless,
                ..DecoderTrackInfo::default()
            },
            logged_first_frame: false,
            aac_timing: matches!(
                track.codec,
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2
            )
            .then(super::timing::Timing::default),
            decoded_offset: 0,
        })
    }

    /// Whether `SymphoniaCodec::open` can accept this codec via
    /// [`TrackInfo`] alone. `AudioCodec::Pcm` / `AudioCodec::Adpcm` need
    /// bit-depth + endianness which the generic enum does not encode —
    /// the factory routes those through [`Self::open_native`] using the
    /// demuxer's native `AudioCodecParameters` instead.
    #[must_use]
    pub(crate) const fn supports(codec: AudioCodec) -> bool {
        !matches!(codec, AudioCodec::Pcm | AudioCodec::Adpcm)
    }

    fn take_aac_output(
        &mut self,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let timing = self.aac_timing.as_mut().ok_or(DecodeError::InvalidData {
            detail: "AAC output has no packet timing owner",
        })?;
        let decoded = self.decoder.last_decoded();
        let total_frames = decoded.frames();
        let channels = usize::from(self.spec.channels);
        while self.decoded_offset < total_frames {
            let available = u64::try_from(total_frames - self.decoded_offset)?;
            match timing.take(available, self.spec)? {
                super::timing::Output::Padding(frames) => {
                    self.decoded_offset += usize::try_from(frames)?;
                }
                super::timing::Output::Samples { frames, metadata } => {
                    let frames = usize::try_from(frames)?;
                    let samples = decoded.samples_interleaved();
                    out.ensure_len(samples)?;
                    decoded.copy_to_slice_interleaved(&mut out[..samples]);
                    let start = self.decoded_offset * channels;
                    let end = (self.decoded_offset + frames) * channels;
                    out.copy_within(start..end, 0);
                    out.truncate(frames * channels);
                    self.decoded_offset += frames;
                    return Ok(crate::codec::DecodedPacket::new(
                        u32::try_from(frames)?,
                        metadata,
                    ));
                }
            }
        }
        out.clear();
        Ok(crate::codec::DecodedPacket::new(
            0,
            crate::demuxer::PacketMetadata::default(),
        ))
    }

    fn handle_decode_error(
        &mut self,
        error: SymphoniaError,
        out: &mut SampleBuffer,
        metadata: crate::demuxer::PacketMetadata,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        match error {
            SymphoniaError::DecodeError(err) => {
                if self.aac_timing.is_some() {
                    return Err(DecodeError::backend(SymphoniaError::DecodeError(err)));
                }
                tracing::debug!(error = %err, "SymphoniaCodec: skipping undecodable frame");
                out.clear();
                Ok(crate::codec::DecodedPacket::new(0, metadata))
            }
            SymphoniaError::ResetRequired => {
                if self.aac_timing.is_some() {
                    return Err(DecodeError::backend(SymphoniaError::ResetRequired));
                }
                self.decoder.reset();
                out.clear();
                Ok(crate::codec::DecodedPacket::new(0, metadata))
            }
            error => Err(DecodeError::backend(error)),
        }
    }
}

fn update_decoded_spec(
    spec: &mut AudioSpec,
    logged_first_frame: &mut bool,
    decoded: &GenericAudioBufferRef<'_>,
    pts_ticks: u64,
) {
    let num_samples = decoded.samples_interleaved();
    let actual = decoded.spec();
    let actual_rate = actual.rate();
    let actual_channels = u16::try_from(actual.channels().count()).unwrap_or(spec.channels);
    if !*logged_first_frame {
        tracing::info!(
            target: "kithara_decode::symphonia::codec",
            declared_rate = spec.sample_rate.get(),
            declared_channels = spec.channels,
            actual_rate,
            actual_channels,
            decoded_frames = decoded.frames(),
            num_samples_interleaved = num_samples,
            pts_ticks,
            "SymphoniaCodec::decode_frame — first frame snapshot"
        );
        *logged_first_frame = true;
    }
    if let Some(nz_actual_rate) = NonZeroU32::new(actual_rate)
        && (spec.sample_rate != nz_actual_rate || spec.channels != actual_channels)
    {
        tracing::debug!(
            target: "kithara_decode::symphonia::codec",
            old_rate = spec.sample_rate.get(),
            old_channels = spec.channels,
            new_rate = actual_rate,
            new_channels = actual_channels,
            "SymphoniaCodec: live spec update from decoder output"
        );
        *spec = AudioSpec::new(actual_channels, nz_actual_rate);
    }
}

fn copy_decoded(
    decoded: &GenericAudioBufferRef<'_>,
    out: &mut SampleBuffer,
    spec: AudioSpec,
    metadata: crate::demuxer::PacketMetadata,
) -> DecodeResult<crate::codec::DecodedPacket> {
    let num_samples = decoded.samples_interleaved();
    if num_samples == 0 {
        out.clear();
        return Ok(crate::codec::DecodedPacket::new(0, metadata));
    }

    out.ensure_len(num_samples)?;
    kithara::measure_block!("symphonia::output::copy_interleaved", {
        decoded.copy_to_slice_interleaved(&mut out[..num_samples]);
    });
    out.truncate(num_samples);
    let frames = u32::try_from(decoded.frames()).unwrap_or(u32::MAX);
    out.truncate(usize::try_from(frames)? * usize::from(spec.channels));
    Ok(crate::codec::DecodedPacket::new(frames, metadata))
}

impl FrameCodec for SymphoniaCodec {
    #[kithara::measure(label = "decode.symphonia.codec")]
    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        if self.has_output() {
            return Err(DecodeError::InvalidData {
                detail: "buffered codec output must be consumed before another packet",
            });
        }
        let crate::demuxer::Frame {
            pts,
            packet_desc: _packet_desc,
            ..
        } = frame;
        let pts_ticks = self.spec.frame_at(pts)?;
        let packet_pts = Timestamp::new(i64::try_from(pts_ticks).unwrap_or(i64::MAX));
        let packet_ref = PacketRef::new(
            consts::TRACK_ID,
            packet_pts,
            PktDuration::new(self.spec.frame_at(frame.duration)?),
            frame.data,
        );

        let decoded = match kithara::measure_block!(
            match self.codec {
                Some(AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2) => {
                    "symphonia::decoder::aac"
                }
                Some(AudioCodec::Mp3) => "symphonia::decoder::mp3",
                Some(AudioCodec::Flac) => "symphonia::decoder::flac",
                Some(AudioCodec::Vorbis) => "symphonia::decoder::vorbis",
                Some(AudioCodec::Opus) => "symphonia::decoder::opus",
                Some(AudioCodec::Alac) => "symphonia::decoder::alac",
                Some(AudioCodec::Pcm) => "symphonia::decoder::pcm",
                Some(AudioCodec::Adpcm) => "symphonia::decoder::adpcm",
                Some(AudioCodec::Ape) => "symphonia::decoder::ape",
                None => "symphonia::decoder::native",
            },
            self.decoder.decode_ref(&packet_ref)
        ) {
            Ok(d) => d,
            Err(error) => return self.handle_decode_error(error, out, metadata),
        };

        update_decoded_spec(
            &mut self.spec,
            &mut self.logged_first_frame,
            &decoded,
            pts_ticks,
        );
        self.decoded_offset = 0;
        let decoded_frames = decoded.frames();
        if let Some(timing) = &mut self.aac_timing {
            if !frame.data.is_empty() {
                let block = self
                    .decoder
                    .codec_params()
                    .max_frames_per_packet
                    .unwrap_or_else(|| {
                        self.codec
                            .map_or(0, |codec| u64::from(access_unit_frames(codec)))
                    });
                timing.push(frame, metadata, self.spec, block)?;
            }
            if frame.data.is_empty() && decoded_frames == 0 && !timing.is_complete() {
                return Err(DecodeError::InvalidData {
                    detail: "AAC decoder ended before all supplied PCM was recovered",
                });
            }
            return self.take_aac_output(out);
        }
        copy_decoded(&decoded, out, self.spec, metadata)
    }

    fn decoder_algo_delay(&self, codec: AudioCodec) -> u64 {
        symphonia_decoder_algo_delay(codec)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        self.decoder.reset();
        self.decoded_offset = 0;
        if let Some(timing) = &mut self.aac_timing {
            timing.clear();
        }
        Ok(())
    }

    fn has_output(&self) -> bool {
        self.aac_timing.is_some() && self.decoded_offset < self.decoder.last_decoded().frames()
    }

    fn take_output(&mut self, out: &mut SampleBuffer) -> DecodeResult<crate::codec::DecodedPacket> {
        self.take_aac_output(out)
    }

    fn decoded_pts(&self) -> Option<Duration> {
        if let Some(timing) = &self.aac_timing {
            return timing.decoded_pts();
        }
        None
    }

    fn needs_eof_drain(&self, source_sample_rate: u32) -> bool {
        #[cfg(all(not(target_arch = "wasm32"), feature = "fdk-aac"))]
        if self.aac_timing.is_some() {
            return true;
        }
        self.spec.sample_rate.get() != source_sample_rate
    }

    fn prepare_output(&self, out: &mut SampleBuffer) -> DecodeResult<()> {
        let frames = self
            .decoder
            .codec_params()
            .max_frames_per_packet
            .unwrap_or_else(|| {
                self.codec
                    .map_or(0, |codec| u64::from(access_unit_frames(codec)))
            });
        let samples = usize::try_from(frames)?
            .checked_mul(usize::from(self.spec.channels))
            .ok_or(DecodeError::InvalidData {
                detail: "codec output sample count overflow",
            })?;
        out.ensure_len(samples)?;
        out.clear();
        Ok(())
    }

    /// AAC (including HE-AAC seen as `AacLc`) requests 2 access units of SBR/PS QMF pre-roll after
    /// flush.
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

const fn symphonia_decoder_algo_delay(codec: AudioCodec) -> u64 {
    match codec {
        AudioCodec::Mp3 => consts::MP3_DECODER_DELAY,
        _ => 0,
    }
}

const fn apply_decoder_algo_delay(info: GaplessInfo, algo_delay: u64) -> GaplessInfo {
    if algo_delay == 0 {
        return info;
    }
    GaplessInfo {
        leading_frames: info.leading_frames.saturating_add(algo_delay),
        trailing_frames: info.trailing_frames.saturating_sub(algo_delay),
    }
}

const fn map_codec(codec: AudioCodec) -> DecodeResult<(AudioCodecId, Option<CodecProfile>)> {
    match codec {
        AudioCodec::AacLc => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_LC))),
        AudioCodec::AacHe => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_HE))),
        AudioCodec::AacHeV2 => Ok((CODEC_ID_AAC, Some(CODEC_PROFILE_AAC_HE_V2))),
        AudioCodec::Flac => Ok((CODEC_ID_FLAC, None)),
        AudioCodec::Mp3 => Ok((CODEC_ID_MP3, None)),
        AudioCodec::Alac => Ok((CODEC_ID_ALAC, None)),
        AudioCodec::Opus => Ok((CODEC_ID_OPUS, None)),
        AudioCodec::Vorbis => Ok((CODEC_ID_VORBIS, None)),
        AudioCodec::Pcm | AudioCodec::Adpcm | AudioCodec::Ape => {
            Err(DecodeError::UnsupportedCodec { codec })
        }
    }
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
        symphonia::SymphoniaConfig,
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
    /// `AudioSpec { sample_rate: 0 }`.
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
        // WHY: AAC (incl. HE-AAC seen as AacLc) requests 2-AU SBR pre-roll.
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
