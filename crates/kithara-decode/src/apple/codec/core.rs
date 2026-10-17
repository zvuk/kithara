use std::mem::size_of;

use kithara_apple::audio_toolbox::{
    AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, AudioConverter, AudioConverterPrimeInfo,
    AudioStreamPacketDescription, AudioToolboxError, SingleAudioBufferList, pod_from_prefix,
};
use kithara_bufpool::SampleBuffer;
use kithara_platform::time::{Duration, ThreadTime, WallInstant};
use kithara_signal::AudioSpec;
use kithara_stream::AudioCodec;

#[cfg(test)]
use super::super::flac;
#[cfg(test)]
use super::output::ceil_resampled_frames;
use super::{
    super::{
        consts,
        converter::{
            ConverterInputState, gapless_info_from_prime_info, log_gapless_prime_info,
            prime_info_from_converter,
        },
        demuxer::AppleAudioFileDemuxer,
    },
    input::{AppleInputFormat, build_input_format},
    output::{
        build_pcm_output_format, output_frame_capacity, output_sample_capacity,
        resolve_output_sample_rate,
    },
};
use crate::{
    GaplessTailCompensation,
    codec::{CodecPriming, FrameCodec},
    demuxer::TrackInfo,
    error::{DecodeError, DecodeResult},
    types::{DecoderTrackInfo, checked_audio_spec},
};

/// Frame-level codec wrapping Apple's `AudioConverter`.
pub(crate) struct AppleCodec {
    codec: AudioCodec,
    converter: AudioConverter,
    spec: AudioSpec,
    input_state: Box<ConverterInputState>,
    /// Decoder-owned playback contract. Populated with the captured
    /// [`crate::GaplessInfo`] when `gapless` was requested in
    /// [`AppleCodec::open_with_config`].
    track_info: DecoderTrackInfo,
    /// Last `kAudioConverterPrimeInfo` snapshot. Used to detect when
    /// the post-first-chunk refresh changes from the init query (AAC
    /// reports priming only after consuming one input packet).
    last_prime_info: Option<AudioConverterPrimeInfo>,
    /// True once the converter has emitted the remaining PCM after source EOF.
    eof_drained: bool,
    /// Whether gapless capture was requested in
    /// [`AppleCodec::open_with_config`].
    gapless_enabled: bool,
    /// Set to `true` when `gapless_enabled` is on and the init query
    /// did not yet yield priming numbers; cleared after the first
    /// post-decode refresh.
    prime_info_refresh_pending: bool,
    tail_compensation_enabled: bool,
    /// `AudioConverter`'s expected output packets-per-callback. Used to
    /// pre-grow the caller's `out` buffer before invoking the FFI so the
    /// converter writes directly into pool memory — no internal scratch
    /// buffer needed.
    frames_per_packet: u32,
    /// Input ASBD `bytes_per_packet` snapshot, copied at open. Non-zero
    /// for CBR codecs (`LinearPCM`); zero for VBR (AAC/MP3/ALAC/FLAC).
    /// `decode_frame` uses this to size the `AudioConverter` output to
    /// match the actual input packet count when the demuxer batched
    /// multiple packets into one `Frame`.
    input_bytes_per_packet: u32,
    /// Source-domain sample rate from `TrackInfo`; `spec.sample_rate`
    /// is the actual output/device-domain rate.
    source_sample_rate: u32,
    source_frames_seen: u64,
    output_frames_seen: u64,
    output_origin: Option<Duration>,
    decoded_pts: Option<Duration>,
}

impl AppleCodec {
    fn drain_eof(&mut self, out: &mut SampleBuffer) -> DecodeResult<u32> {
        if !self.needs_eof_drain(self.source_sample_rate) || self.eof_drained {
            out.clear();
            return Ok(0);
        }

        self.input_state.finish();
        let frames = self.fill_converter(out, self.eof_flush_frame_capacity()?)?;
        if frames == 0 {
            self.eof_drained = true;
            self.input_state.clear();
        }
        Ok(frames)
    }

    fn eof_flush_frame_capacity(&self) -> DecodeResult<u32> {
        let total = u128::from(self.source_frames_seen) * u128::from(self.spec.sample_rate.get());
        let total = total.div_ceil(u128::from(self.source_sample_rate));
        let remaining = total.saturating_sub(u128::from(self.output_frames_seen));
        let capacity = output_frame_capacity(
            self.frames_per_packet.max(consts::AAC_FRAMES_PER_PACKET),
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        )?;
        Ok(u32::try_from(remaining.min(u128::from(capacity)))?)
    }

    fn fill_converter(&mut self, out: &mut SampleBuffer, target_frames: u32) -> DecodeResult<u32> {
        if target_frames == 0 {
            out.clear();
            return Ok(0);
        }

        let decoded_pts =
            self.output_origin
                .map(|origin| {
                    let start = self.spec.frame_at(origin)?;
                    let end = start.checked_add(self.output_frames_seen).ok_or(
                        DecodeError::InvalidData {
                            detail: "Apple PCM output frame offset overflows",
                        },
                    )?;
                    let elapsed = self.spec.duration_for(end)? - self.spec.duration_for(start)?;
                    origin.checked_add(elapsed).ok_or(DecodeError::InvalidData {
                        detail: "Apple PCM output timestamp overflows",
                    })
                })
                .transpose()?;
        let channels = usize::from(self.spec.channels);
        let needed_samples = output_sample_capacity(target_frames, channels)?;
        out.ensure_len(needed_samples)?;

        let mut output_packets = target_frames;
        let mut buffer_list =
            SingleAudioBufferList::interleaved_f32(u32::from(self.spec.channels), out)
                .map_err(DecodeError::backend)?;
        let status = self.converter.fill_complex_buffer(
            self.input_state.as_mut(),
            1,
            &mut output_packets,
            &mut buffer_list,
        );

        if status != consts::NO_ERR
            && status != consts::CONVERTER_ERR_NO_DATA_NOW
            && output_packets == 0
        {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverterFillComplexBuffer",
            });
        }

        let frames = output_packets;
        if frames != 0 {
            self.decoded_pts = decoded_pts;
        }
        self.output_frames_seen = self.output_frames_seen.saturating_add(u64::from(frames));
        let samples_len = output_sample_capacity(frames, channels)?;
        out.truncate(samples_len);
        Ok(frames)
    }

    /// Open the converter and capture container or backend gapless metadata when requested.
    ///
    /// AAC can publish priming after its first packet, so missing metadata arms one refresh.
    ///
    /// # Errors
    ///
    /// Returns the backend error if the input/output formats or magic cookie are rejected.
    pub(crate) fn open_with_config(
        track: &TrackInfo,
        gapless: bool,
        target_output_rate: Option<u32>,
    ) -> DecodeResult<Self> {
        let AppleInputFormat {
            asbd: input_format,
            frames_per_packet,
            cookie,
        } = build_input_format(track)?;
        let input_bytes_per_packet = input_format.bytes_per_packet;
        let output_sample_rate = resolve_output_sample_rate(track.sample_rate, target_output_rate);
        let spec = checked_audio_spec(track.channels, output_sample_rate, "apple.codec.output")?;
        let output_format =
            build_pcm_output_format(track.sample_rate, track.channels, target_output_rate);

        tracing::debug!(operation = "AudioConverterNew", phase = "begin");
        let converter_cpu_started =
            tracing::enabled!(tracing::Level::DEBUG).then(ThreadTime::try_now);
        let converter_started = WallInstant::now();
        let created = AudioConverter::new(&input_format, &output_format);
        let converter_elapsed_us = converter_started.elapsed().as_micros();
        let (converter_cpu_elapsed_us, converter_cpu_error) = match converter_cpu_started
            .map(|started| started.and_then(|start| start.try_elapsed()))
        {
            Some(Ok(elapsed)) => (Some(elapsed.as_micros()), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        tracing::debug!(
            operation = "AudioConverterNew",
            phase = "end",
            elapsed_us = converter_elapsed_us,
            cpu_elapsed_us = converter_cpu_elapsed_us,
            cpu_clock_error = ?converter_cpu_error,
            succeeded = created.is_ok(),
        );
        let mut converter = created.map_err(|err| DecodeError::BackendStatus {
            code: audio_toolbox_status(&err),
            op: "AudioConverterNew",
        })?;

        if let Some(cookie) = cookie.as_ref().filter(|c| !c.is_empty()) {
            let status =
                converter.set_property_bytes(AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, cookie);
            if status != consts::NO_ERR {
                return Err(DecodeError::BackendStatus {
                    code: status,
                    op: "AudioConverterSetProperty(MagicCookie)",
                });
            }
        }

        let (prime_info, resolved_gapless) = if gapless {
            tracing::debug!(
                operation = "AudioConverterGetProperty(PrimeInfo)",
                phase = "begin",
            );
            let prime_started = WallInstant::now();
            let prime_info = prime_info_from_converter(&converter);
            let prime_elapsed_us = prime_started.elapsed().as_micros();
            tracing::debug!(
                operation = "AudioConverterGetProperty(PrimeInfo)",
                phase = "end",
                elapsed_us = prime_elapsed_us,
                available = prime_info.is_some(),
            );
            let probed_gapless = prime_info.and_then(gapless_info_from_prime_info);
            log_gapless_prime_info("init", prime_info, probed_gapless);
            (prime_info, track.gapless.or(probed_gapless))
        } else {
            (None, None)
        };

        Ok(Self {
            codec: track.codec,
            converter,
            spec,
            frames_per_packet,
            input_bytes_per_packet,
            source_sample_rate: track.sample_rate,
            input_state: Box::new(ConverterInputState::default()),
            track_info: DecoderTrackInfo {
                gapless: resolved_gapless,
                ..DecoderTrackInfo::default()
            },
            last_prime_info: prime_info,
            gapless_enabled: gapless,
            prime_info_refresh_pending: gapless && resolved_gapless.is_none(),
            eof_drained: false,
            tail_compensation_enabled: output_sample_rate != track.sample_rate,
            source_frames_seen: 0,
            output_frames_seen: 0,
            output_origin: None,
            decoded_pts: None,
        })
    }

    /// AAC's converter populates `kAudioConverterPrimeInfo` only after
    /// at least one input packet is consumed; FLAC fills it at init.
    /// We arm a one-shot refresh during `open_with_config` and run it
    /// after the first `decode_frame` to capture priming on AAC without
    /// a separate API.
    fn refresh_gapless_after_first_chunk(&mut self) {
        if !self.gapless_enabled || !self.prime_info_refresh_pending {
            return;
        }
        self.prime_info_refresh_pending = false;
        let prime_info = prime_info_from_converter(&self.converter);
        let gapless = prime_info.and_then(gapless_info_from_prime_info);
        log_gapless_prime_info("post_first_chunk", prime_info, gapless);
        if let Some(prime_info) = prime_info {
            self.last_prime_info = Some(prime_info);
            self.track_info.gapless = gapless;
        }
    }

    fn refresh_tail_compensation(&mut self) {
        if !self.tail_compensation_enabled {
            return;
        }
        self.track_info.gapless_tail = GaplessTailCompensation::for_source_frames(
            self.source_frames_seen,
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        );
    }

    /// Whether the Apple `AudioConverter` accepts this codec at the
    /// codec layer alone (i.e. without an external container parser).
    ///
    /// Scope: AAC-LC and FLAC over fMP4 (HLS), standalone MP3 behind the
    /// MPEG audio demuxer, plus standalone WAV/PCM and ALAC paired with
    /// [`AppleAudioFileDemuxer`]. PCM
    /// requires the demuxer to stash the source ASBD as a serialized
    /// 40-byte blob in `TrackInfo.extra_data`; ALAC requires the magic
    /// cookie in the same field.
    #[must_use]
    pub(crate) const fn supports(codec: AudioCodec) -> bool {
        matches!(
            codec,
            AudioCodec::AacLc
                | AudioCodec::AacHe
                | AudioCodec::AacHeV2
                | AudioCodec::Flac
                | AudioCodec::Pcm
                | AudioCodec::Mp3
                | AudioCodec::Alac
        )
    }
}

impl FrameCodec for AppleCodec {
    /// Equal-rate AAC already emits complete packets; conversion and other codecs need a tail drain.
    fn needs_eof_drain(&self, _source_rate: u32) -> bool {
        self.spec.sample_rate.get() != self.source_sample_rate
            || !matches!(
                self.codec,
                AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2
            )
    }

    fn decode_frame(
        &mut self,
        frame: crate::demuxer::Frame<'_>,
        metadata: crate::demuxer::PacketMetadata,
        out: &mut SampleBuffer,
    ) -> DecodeResult<crate::codec::DecodedPacket> {
        let crate::demuxer::Frame {
            data: frame_data,
            pts,
            packet_desc,
            ..
        } = frame;
        if frame_data.is_empty() {
            return self
                .drain_eof(out)
                .map(|frames| crate::codec::DecodedPacket::new(frames, metadata));
        }

        let desc = if packet_desc.len() == size_of::<AudioStreamPacketDescription>() {
            pod_from_prefix(packet_desc).ok_or(DecodeError::InvalidData {
                detail: "packet descriptor has invalid Apple ABI shape",
            })?
        } else {
            let frame_bytes = u32::try_from(frame_data.len())?;
            AudioStreamPacketDescription {
                start_offset: 0,
                variable_frames_in_packet: 0,
                data_byte_size: frame_bytes,
            }
        };
        self.eof_drained = false;
        let status = self.input_state.set(frame_data, desc);
        if status != consts::NO_ERR {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverter input packet",
            });
        }

        if self.codec == AudioCodec::Mp3 && self.output_origin.is_none() {
            self.output_origin = Some(pts);
            self.decoded_pts = Some(pts);
        }
        let input_frames = if self.input_bytes_per_packet > 0 {
            let packets = frame_data.len() / usize::try_from(self.input_bytes_per_packet)?;
            let frames =
                u64::try_from(packets)?.saturating_mul(u64::from(self.frames_per_packet.max(1)));
            u32::try_from(frames)?
        } else if desc.variable_frames_in_packet > 0 {
            desc.variable_frames_in_packet
        } else {
            self.frames_per_packet.max(1)
        };
        self.source_frames_seen = self
            .source_frames_seen
            .saturating_add(u64::from(input_frames));
        self.refresh_tail_compensation();
        let target_frames = output_frame_capacity(
            input_frames,
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        )?;
        let frames = self.fill_converter(out, target_frames)?;
        self.refresh_gapless_after_first_chunk();
        Ok(crate::codec::DecodedPacket::new(frames, metadata))
    }

    fn decoded_pts(&self) -> Option<Duration> {
        self.decoded_pts
    }

    fn media_time_rebase(&self) -> Option<Duration> {
        checked_audio_spec(
            self.spec.channels,
            self.source_sample_rate,
            "apple.codec.source",
        )
        .ok()?
        .duration_for(apple_decoder_algo_delay(self.codec))
        .ok()
    }

    fn decoder_algo_delay(&self, codec: AudioCodec) -> u64 {
        apple_decoder_algo_delay(codec)
    }

    fn flush(&mut self) -> DecodeResult<()> {
        let status = self.converter.reset();
        if status != consts::NO_ERR {
            return Err(DecodeError::BackendStatus {
                code: status,
                op: "AudioConverterReset",
            });
        }
        self.input_state.clear();
        self.eof_drained = false;
        self.source_frames_seen = 0;
        self.output_frames_seen = 0;
        self.output_origin = None;
        self.decoded_pts = None;
        self.tail_compensation_enabled = false;
        self.track_info.gapless_tail = None;
        Ok(())
    }

    fn prepare_output(&self, out: &mut SampleBuffer) -> DecodeResult<()> {
        let input_frames = if let Some(packets) =
            AppleAudioFileDemuxer::CBR_BATCH_TARGET_BYTES.checked_div(self.input_bytes_per_packet)
        {
            packets
                .max(1)
                .checked_mul(self.frames_per_packet.max(1))
                .ok_or(DecodeError::InvalidData {
                    detail: "Apple PCM packet frame count overflows",
                })?
        } else {
            self.frames_per_packet.max(consts::AAC_FRAMES_PER_PACKET)
        };
        let frames = output_frame_capacity(
            input_frames,
            self.source_sample_rate,
            self.spec.sample_rate.get(),
        )?
        .max(self.eof_flush_frame_capacity()?);
        out.ensure_len(output_sample_capacity(
            frames,
            usize::from(self.spec.channels),
        )?)?;
        Ok(())
    }

    fn priming(&self, codec: AudioCodec) -> CodecPriming {
        apple_codec_priming(codec)
    }

    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn track_info(&self) -> DecoderTrackInfo {
        self.track_info.clone()
    }
}

const fn audio_toolbox_status(err: &AudioToolboxError) -> i32 {
    match err {
        AudioToolboxError::Status { status, .. } => *status,
        AudioToolboxError::Config { .. } => -50,
    }
}

/// Per-codec priming requirements for the Apple `AudioConverter` backend.
/// `AudioConverter` does not strip its own MDCT/SBR warm-up — these values
/// represent how far back the demuxer must park before the seek target so
/// the codec can decode-and-discard the right number of warm-up packets.
/// `FrameCodec::priming` delegates here; the free function exists so tests
/// can pin the table without needing a live `AudioConverterRef`.
#[must_use]
pub(crate) fn apple_codec_priming(codec: AudioCodec) -> CodecPriming {
    match codec {
        AudioCodec::AacHeV2 => CodecPriming {
            frames: 4096,
            packets: 3,
            byte_margin: 32768,
        },
        AudioCodec::AacHe => CodecPriming {
            frames: 2048,
            packets: 2,
            byte_margin: 16384,
        },
        AudioCodec::AacLc => CodecPriming {
            frames: 1024,
            packets: 2,
            byte_margin: 8192,
        },
        AudioCodec::Mp3 => CodecPriming {
            frames: 1152,
            packets: 1,
            byte_margin: 4608,
        },
        _ => CodecPriming::default(),
    }
}

/// Apple-backend MP3 decoder algorithmic delay in PCM frames.
///
/// `AudioConverter` for MP3 leaves the LAME-convention 529-frame
/// algorithmic delay un-compensated. Symphonia `mpa` declares the
/// same number; mirror it here so gapless priming matches across
/// backends. Non-MP3 codecs default to 0 — AAC priming is captured
/// via `AudioConverterPrimeInfo` in the gapless capture path.
#[must_use]
pub(crate) const fn apple_decoder_algo_delay(codec: AudioCodec) -> u64 {
    match codec {
        AudioCodec::Mp3 => 529,
        _ => 0,
    }
}

#[cfg(test)]
mod algo_delay_tests {
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::apple_decoder_algo_delay;

    #[kithara::test]
    fn apple_decoder_algo_delay_mp3_is_529() {
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Mp3), 529);
    }

    #[kithara::test]
    fn apple_decoder_algo_delay_non_mp3_codecs_zero() {
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacLc), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacHe), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::AacHeV2), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Flac), 0);
        assert_eq!(apple_decoder_algo_delay(AudioCodec::Opus), 0);
    }
}

#[cfg(test)]
mod priming_table_tests {
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::apple_codec_priming;
    use crate::codec::CodecPriming;

    #[kithara::test]
    fn apple_priming_aac_he_v2() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacHeV2),
            CodecPriming {
                frames: 4096,
                packets: 3,
                byte_margin: 32768
            }
        );
    }

    #[kithara::test]
    fn apple_priming_aac_he() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacHe),
            CodecPriming {
                frames: 2048,
                packets: 2,
                byte_margin: 16384
            }
        );
    }

    #[kithara::test]
    fn apple_priming_aac_lc() {
        assert_eq!(
            apple_codec_priming(AudioCodec::AacLc),
            CodecPriming {
                frames: 1024,
                packets: 2,
                byte_margin: 8192
            }
        );
    }

    #[kithara::test]
    fn apple_priming_mp3() {
        assert_eq!(
            apple_codec_priming(AudioCodec::Mp3),
            CodecPriming {
                frames: 1152,
                packets: 1,
                byte_margin: 4608
            }
        );
    }

    #[kithara::test]
    fn apple_priming_flac_is_default() {
        assert_eq!(
            apple_codec_priming(AudioCodec::Flac),
            CodecPriming::default()
        );
    }
}

#[cfg(test)]
mod output_rate_tests {
    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::unit_fixtures::aac_init;
    use kithara_test_utils::kithara;

    use super::{
        AppleCodec, build_pcm_output_format, output_frame_capacity, resolve_output_sample_rate,
    };
    use crate::{
        codec::FrameCodec, consts, demuxer::TrackInfo, fmp4::parsing::parse_init, test_pools::pools,
    };

    fn aac_lc_track(aac_init: &[u8]) -> TrackInfo {
        let init_bytes = aac_init;
        let init = parse_init(init_bytes, &pools()).expect("BUG: parse AAC init");
        let extra_data = init.config.as_ref().to_vec();
        TrackInfo {
            extra_data,
            codec: AudioCodec::AacLc,
            sample_rate: init.sample_rate,
            channels: init.channels,
            duration: None,
            gapless: init.gapless,
        }
    }

    #[kithara::test]
    fn pcm_output_format_uses_source_rate_without_target() {
        let asbd = build_pcm_output_format(consts::SOURCE_RATE, consts::TEST_CHANNELS, None);

        assert_eq!(asbd.sample_rate, f64::from(consts::SOURCE_RATE));
    }

    #[kithara::test]
    fn pcm_output_format_uses_source_rate_when_target_matches() {
        let asbd = build_pcm_output_format(
            consts::SOURCE_RATE,
            consts::TEST_CHANNELS,
            Some(consts::SOURCE_RATE),
        );

        assert_eq!(asbd.sample_rate, f64::from(consts::SOURCE_RATE));
    }

    #[kithara::test]
    fn pcm_output_format_uses_target_rate_when_different() {
        let asbd = build_pcm_output_format(
            consts::SOURCE_RATE,
            consts::TEST_CHANNELS,
            Some(consts::ALT_RATE),
        );

        assert_eq!(asbd.sample_rate, f64::from(consts::ALT_RATE));
    }

    #[kithara::test]
    fn output_frame_capacity_preserves_equal_rate_passthrough() {
        let capacity = output_frame_capacity(
            consts::INPUT_FRAMES,
            consts::SOURCE_RATE,
            consts::SOURCE_RATE,
        )
        .expect("BUG: compute equal-rate output capacity");

        assert_eq!(capacity, consts::INPUT_FRAMES);
    }

    #[kithara::test]
    fn output_frame_capacity_covers_upsample_ratio() {
        let capacity =
            output_frame_capacity(consts::INPUT_FRAMES, consts::SOURCE_RATE, consts::ALT_RATE)
                .expect("BUG: compute upsample output capacity");

        assert_eq!(capacity, consts::UPSAMPLE_CAPACITY);
    }

    #[kithara::test]
    fn output_frame_capacity_covers_downsample_ratio() {
        let capacity =
            output_frame_capacity(consts::INPUT_FRAMES, consts::ALT_RATE, consts::SOURCE_RATE)
                .expect("BUG: compute downsample output capacity");

        assert_eq!(capacity, consts::DOWNSAMPLE_CAPACITY);
    }

    #[kithara::test]
    fn apple_codec_spec_uses_resolved_output_rate(aac_init: Vec<u8>) {
        let track = aac_lc_track(&aac_init);
        let target_rate = if track.sample_rate == consts::ALT_RATE {
            consts::SOURCE_RATE
        } else {
            consts::ALT_RATE
        };
        for target_output_rate in [None, Some(track.sample_rate), Some(target_rate)] {
            let codec = AppleCodec::open_with_config(&track, false, target_output_rate)
                .expect("BUG: open Apple codec with target rate");
            let expected_rate = resolve_output_sample_rate(track.sample_rate, target_output_rate);

            assert_eq!(codec.spec().sample_rate.get(), expected_rate);
        }
    }
}

#[cfg(test)]
mod aac_lc_decode_tests {
    use kithara_bufpool::PoolRegion;
    use kithara_platform::time::Duration;
    use kithara_stream::AudioCodec;
    use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment};
    use kithara_test_utils::kithara;

    use super::{AppleCodec, ceil_resampled_frames, output_frame_capacity};
    use crate::{
        codec::FrameCodec,
        consts,
        demuxer::TrackInfo,
        fmp4::parsing::{Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
        test_pools::{TestPools, pools},
    };

    fn track_from_init(init: &Fmp4InitInfo) -> TrackInfo {
        let extra_data = init.config.as_ref().to_vec();
        TrackInfo {
            extra_data,
            codec: init.codec,
            sample_rate: init.sample_rate,
            channels: init.channels,
            duration: None,
            gapless: init.gapless,
        }
    }

    fn target_rate_for_source(source_rate: u32) -> u32 {
        if source_rate < consts::COMMON_TARGET_RATE {
            consts::COMMON_TARGET_RATE
        } else {
            consts::HIGH_TARGET_RATE
        }
    }

    fn decode_frames(
        codec: &mut AppleCodec,
        pools: &PoolRegion<TestPools>,
        seg: &[u8],
        frames: &[Fmp4Frame],
    ) -> u64 {
        let mut total = 0_u64;
        for frame in frames {
            let mut buf = pools.get::<f32>();
            let decoded = codec
                .decode_frame(
                    crate::demuxer::Frame {
                        data: &seg[frame.offset..frame.offset + frame.size],
                        pts: Duration::ZERO,
                        duration: Duration::ZERO,
                        packet_desc: &[],
                    },
                    crate::demuxer::PacketMetadata::default(),
                    &mut buf,
                )
                .expect("BUG: decode Apple AAC-LC frame")
                .frames;
            total += u64::from(decoded);
        }
        total
    }

    fn drain_eof(codec: &mut AppleCodec, pools: &PoolRegion<TestPools>) -> u64 {
        let mut total = 0_u64;
        for _ in 0..consts::MAX_EOF_DRAIN_CALLS {
            let mut buf = pools.get::<f32>();
            let frames = codec
                .decode_frame(
                    crate::demuxer::Frame {
                        data: &[],
                        pts: Duration::ZERO,
                        duration: Duration::ZERO,
                        packet_desc: &[],
                    },
                    crate::demuxer::PacketMetadata::default(),
                    &mut buf,
                )
                .expect("BUG: drain Apple AAC-LC EOF")
                .frames;
            if frames == 0 {
                return total;
            }
            total += u64::from(frames);
        }
        panic!("Apple AAC-LC EOF drain did not finish");
    }

    /// RED (device repro): the Apple AAC-LC decoder must turn real fMP4
    /// access units into finite PCM with no symphonia fallback compiled in
    /// — the exact decode path the size-reduced iOS framework exercises.
    /// The module already lives under the `apple` + macOS/iOS gate, so no
    /// per-item `cfg` is needed.
    #[kithara::test]
    fn apple_aac_lc_decode_produces_finite_pcm(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);

        let seg = aac_segment;
        let ranges: Vec<(usize, usize)> = parse_segment_frames(&init, &seg)
            .expect("BUG: parse segment frames")
            .iter()
            .map(|f| (f.offset, f.size))
            .collect();
        assert!(!ranges.is_empty(), "segment yielded no AAC frames");

        let pools = pools();
        let mut codec = AppleCodec::open_with_config(&track, false, None)
            .expect("BUG: open Apple AAC-LC codec");
        let mut pcm = Vec::new();
        for &(offset, size) in &ranges {
            let mut buf = pools.get::<f32>();
            codec
                .decode_frame(
                    crate::demuxer::Frame {
                        data: &seg[offset..offset + size],
                        pts: Duration::ZERO,
                        duration: Duration::ZERO,
                        packet_desc: &[],
                    },
                    crate::demuxer::PacketMetadata::default(),
                    &mut buf,
                )
                .expect("BUG: decode Apple AAC-LC frame");
            pcm.extend_from_slice(&buf[..]);
        }

        assert!(!pcm.is_empty(), "Apple AAC-LC decode produced no PCM");
        assert!(
            pcm.iter().all(|sample| sample.is_finite()),
            "Apple AAC-LC decode produced non-finite PCM",
        );
    }

    #[kithara::test]
    fn apple_aac_lc_resampled_decode_produces_ratio_sized_frames(
        aac_init: Vec<u8>,
        aac_segment: Vec<u8>,
    ) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);
        let target_rate = target_rate_for_source(init.sample_rate);
        assert_ne!(
            target_rate, init.sample_rate,
            "fixture sample rate must differ from target rate"
        );

        let seg = aac_segment;
        let ranges: Vec<(usize, usize)> = parse_segment_frames(&init, &seg)
            .expect("BUG: parse segment frames")
            .iter()
            .take(consts::RESAMPLED_TEST_PACKETS)
            .map(|f| (f.offset, f.size))
            .collect();
        assert!(
            ranges.len() >= 2,
            "segment yielded too few AAC frames for resampled decode"
        );

        let pools = pools();
        let mut codec = AppleCodec::open_with_config(&track, false, Some(target_rate))
            .expect("BUG: open Apple AAC-LC codec with target rate");
        let mut total_output_frames = 0_u64;
        let mut total_capacity_frames = 0_u64;
        let mut produced_more_than_source_packet = false;
        for &(offset, size) in &ranges {
            let mut buf = pools.get::<f32>();
            let frames = codec
                .decode_frame(
                    crate::demuxer::Frame {
                        data: &seg[offset..offset + size],
                        pts: Duration::ZERO,
                        duration: Duration::ZERO,
                        packet_desc: &[],
                    },
                    crate::demuxer::PacketMetadata::default(),
                    &mut buf,
                )
                .expect("BUG: decode resampled Apple AAC-LC frame")
                .frames;
            let capacity = output_frame_capacity(
                super::consts::AAC_FRAMES_PER_PACKET,
                init.sample_rate,
                target_rate,
            )
            .expect("BUG: compute per-packet output capacity");

            assert!(frames <= capacity, "converter wrote past computed capacity");
            produced_more_than_source_packet |= frames > super::consts::AAC_FRAMES_PER_PACKET;
            total_output_frames += u64::from(frames);
            total_capacity_frames += u64::from(capacity);
        }

        let packet_count = u32::try_from(ranges.len()).expect("BUG: test packet count fits in u32");
        let input_frames = packet_count
            .checked_mul(super::consts::AAC_FRAMES_PER_PACKET)
            .expect("BUG: test input frame count fits in u32");
        let ideal_total = ceil_resampled_frames(input_frames, init.sample_rate, target_rate)
            .expect("BUG: compute total resampled frame count");
        let minimum_expected = ideal_total.saturating_sub(consts::MAX_SRC_DELAY_FRAMES);

        assert!(
            produced_more_than_source_packet,
            "upsampled decode never exceeded the old source-rate frame cap"
        );
        assert!(
            total_output_frames >= u64::from(minimum_expected),
            "resampled decode produced too few frames: {total_output_frames} < {minimum_expected}"
        );
        assert!(
            total_output_frames <= total_capacity_frames,
            "resampled decode exceeded requested output capacity"
        );
    }

    #[kithara::test]
    fn apple_aac_lc_src_eof_flush_total_output_within_one_frame(
        aac_init: Vec<u8>,
        aac_segment: Vec<u8>,
    ) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);
        let target_rate = target_rate_for_source(init.sample_rate);
        assert_ne!(
            target_rate, init.sample_rate,
            "fixture sample rate must differ from target rate"
        );

        let seg = aac_segment;
        let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
        assert!(!frames.is_empty(), "segment yielded no AAC frames");

        let pools = pools();
        let mut source_codec =
            AppleCodec::open_with_config(&track, false, None).expect("BUG: open source-rate codec");
        let source_frames = decode_frames(&mut source_codec, &pools, &seg, &frames);
        let source_drain = drain_eof(&mut source_codec, &pools);
        assert_eq!(
            source_drain, 0,
            "equal-rate EOF drain must not change passthrough length"
        );

        let mut src_codec = AppleCodec::open_with_config(&track, false, Some(target_rate))
            .expect("BUG: open SRC codec");
        let before_drain = decode_frames(&mut src_codec, &pools, &seg, &frames);
        let drained = drain_eof(&mut src_codec, &pools);
        let total_output = before_drain + drained;
        let source_frames_u32 =
            u32::try_from(source_frames).expect("BUG: test source frame count fits in u32");
        let ideal = u64::from(
            ceil_resampled_frames(source_frames_u32, init.sample_rate, target_rate)
                .expect("BUG: compute ideal SRC frame count"),
        );

        assert!(
            drained > 0,
            "SRC EOF drain emitted no tail frames; before={before_drain}, ideal={ideal}"
        );
        assert!(
            total_output.abs_diff(ideal) <= consts::OUTPUT_LENGTH_TOLERANCE_FRAMES,
            "SRC total output length off: total={total_output}, ideal={ideal}, \
             before_drain={before_drain}, drained={drained}, source={source_frames}"
        );
    }

    #[kithara::test]
    fn apple_aac_lc_passthrough_eof_drain_preserves_length(
        aac_init: Vec<u8>,
        aac_segment: Vec<u8>,
    ) {
        let init_bytes = aac_init;
        let init = parse_init(&init_bytes, &pools()).expect("BUG: parse AAC init");
        assert_eq!(init.codec, AudioCodec::AacLc, "slq fixture must be AAC-LC");
        let track = track_from_init(&init);

        let seg = aac_segment;
        let frames = parse_segment_frames(&init, &seg).expect("BUG: parse segment frames");
        assert!(!frames.is_empty(), "segment yielded no AAC frames");

        let pools = pools();
        let mut codec = AppleCodec::open_with_config(&track, false, None)
            .expect("BUG: open Apple AAC-LC codec");
        let before_drain = decode_frames(&mut codec, &pools, &seg, &frames);
        let drained = drain_eof(&mut codec, &pools);

        assert!(before_drain > 0, "passthrough decode produced no frames");
        assert_eq!(drained, 0, "passthrough EOF drain produced extra frames");
    }
}

#[cfg(test)]
mod flac_decode_tests {
    use std::io::Cursor;

    use kithara_apple::audio_toolbox::{
        AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE, AudioConverter,
    };
    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_fixtures::unit_fixtures::flac_saw;
    use kithara_test_utils::kithara;

    use super::{AppleCodec, build_input_format, build_pcm_output_format, consts, flac};
    use crate::{
        apple::demuxer::{AppleAudioFileDemuxer, SourceOpenMode},
        codec::FrameCodec,
        demuxer::{DemuxOutcome, Demuxer},
        test_pools::pools,
    };

    #[kithara::test]
    #[case::native_cookie(false, false)]
    #[case::raw_streaminfo(true, false)]
    #[case::id3_prefix(false, true)]
    fn apple_flac_cookie_is_accepted_and_decodes_pcm(
        flac_saw: &'static [u8],
        #[case] raw_streaminfo: bool,
        #[case] id3_prefix: bool,
    ) {
        let mut bytes = Vec::new();
        if id3_prefix {
            bytes.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x10");
            bytes.extend_from_slice(&[0; 16]);
        }
        bytes.extend_from_slice(flac_saw);
        let mut demuxer = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes)),
            AudioCodec::Flac,
            Some(ContainerFormat::Flac),
            SourceOpenMode::Complete,
        )
        .expect("open standalone FLAC");
        let mut track = demuxer.track_info().clone();
        if raw_streaminfo {
            track.extra_data = flac::streaminfo_body(&track.extra_data)
                .expect("STREAMINFO body")
                .to_vec();
        }
        let input = build_input_format(&track).expect("FLAC input format");
        let output = build_pcm_output_format(track.sample_rate, track.channels, None);
        let mut converter = AudioConverter::new(&input.asbd, &output).expect("FLAC converter");
        assert_eq!(
            converter.set_property_bytes(
                AUDIO_CONVERTER_DECOMPRESSION_MAGIC_COOKIE,
                input.cookie.as_deref().expect("FLAC cookie"),
            ),
            consts::NO_ERR,
            "Apple must accept the cookie before any frame is decoded",
        );
        let mut codec = AppleCodec::open_with_config(&track, false, None).expect("FLAC codec");
        let pools = pools();
        let mut pcm = pools.get::<f32>();
        let mut frames = 0_u64;
        let mut peak = 0.0_f32;
        loop {
            match demuxer.next_frame().expect("FLAC packet") {
                DemuxOutcome::Frame(frame) => {
                    frames += u64::from(
                        codec
                            .decode_frame(
                                frame,
                                crate::demuxer::PacketMetadata::default(),
                                &mut pcm,
                            )
                            .expect("decode FLAC packet")
                            .frames,
                    );
                    assert!(pcm.iter().all(|sample| sample.is_finite()));
                    peak = pcm.iter().fold(peak, |peak, sample| peak.max(sample.abs()));
                }
                DemuxOutcome::Eof => break,
                DemuxOutcome::Pending(reason) => panic!("complete FLAC is pending: {reason:?}"),
            }
        }
        assert_eq!(frames, u64::from(track.sample_rate) * 6);
        assert!(peak > 0.1, "FLAC must contain the fixture signal");
    }
}
