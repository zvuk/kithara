use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{AudioCodec, ContainerFormat};

#[cfg(feature = "ape")]
use super::super::build::create_ape;
use super::super::{
    build::finish,
    mpeg::build_mpeg_decoder,
    probe::wav_container_end,
    segment::{build_fmp4_segment_decoder, should_use_segment_aware},
};
use crate::{
    DecodeError, DecodeResult, Decoder, DecoderConfig, GaplessInfo,
    apple::{AppleAudioFileDemuxer, AppleCodec, SourceOpenMode},
    demuxer::Demuxer,
    gapless::scoped_probe,
    traits::BoxedSource,
};

pub(in crate::factory) fn create<B, S>(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    #[cfg(feature = "ape")]
    if codec == AudioCodec::Ape {
        return create_ape(source, config);
    }

    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        if AppleCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Apple HW codec path"
            );
            let gapless = config.gapless;
            let target_output_rate = decoder_embedded_target_output_rate(&config);
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                let output_track = track_with_output_domain_gapless(track, target_output_rate)?;
                AppleCodec::open_with_config(&output_track, gapless, target_output_rate)
            });
        }
        return super::software::create_segment(source, codec, layout, config);
    }

    if matches!(
        (codec, container),
        (
            AudioCodec::Mp3,
            Some(ContainerFormat::MpegAudio | ContainerFormat::Wav)
        )
    ) {
        tracing::debug!("apple-mpeg: routing via the MPEG audio demuxer");
        let target_output_rate = decoder_embedded_target_output_rate(&config);
        let gapless = config.gapless;
        return build_mpeg_decoder(source, container, config, |demuxer| {
            let output_track =
                track_with_output_domain_gapless(demuxer.track_info(), target_output_rate)?;
            if output_track.gapless.is_some() {
                demuxer.set_gapless(output_track.gapless);
            }
            AppleCodec::open_with_config(&output_track, gapless, target_output_rate)
        });
    }

    if AppleAudioFileDemuxer::supports(codec, container) {
        tracing::debug!(
            ?codec,
            ?container,
            "apple-standalone: routing via AudioFileServices"
        );
        return build_apple_standalone_decoder(source, codec, container, config);
    }

    super::software::create(source, codec, container, config)
}

fn build_apple_standalone_decoder<B, S>(
    mut source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig<B, S>,
) -> DecodeResult<Box<dyn Decoder>>
where
    B: ResamplerBackend,
    S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
{
    let probed_gapless = if config.gapless {
        scoped_probe(&mut *source, codec, &config.pools)?
    } else {
        None
    };
    let open_mode = if config.byte_map.is_some() && container == Some(ContainerFormat::Wav) {
        SourceOpenMode::Segmented(wav_container_end(&mut source)?)
    } else if config.byte_len_handle.is_some()
        && (codec != AudioCodec::Pcm || config.byte_map.is_some())
    {
        SourceOpenMode::Streaming
    } else {
        SourceOpenMode::Complete
    };
    let mut demuxer = AppleAudioFileDemuxer::open_for_with_mode_and_pool(
        source,
        codec,
        container,
        open_mode,
        &config.pools,
    )?;
    demuxer.set_byte_len_handle(config.byte_len_handle.clone());
    demuxer.set_byte_map(config.byte_map.clone());
    demuxer.set_gapless(probed_gapless);
    let target_output_rate = decoder_embedded_target_output_rate(&config);
    let output_track = track_with_output_domain_gapless(demuxer.track_info(), target_output_rate)?;
    if output_track.gapless.is_some() {
        demuxer.set_gapless(output_track.gapless);
    }
    let codec_impl =
        AppleCodec::open_with_config(&output_track, config.gapless, target_output_rate)?;
    finish(demuxer, codec_impl, config)
}

fn decoder_embedded_target_output_rate<B, S>(config: &DecoderConfig<B, S>) -> Option<u32>
where
    B: ResamplerBackend,
{
    crate::apple::embedded_target_output_rate(
        config
            .resampler
            .as_ref()
            .map(|resampler| resampler.target_sample_rate),
    )
}

fn track_with_output_domain_gapless(
    track: &crate::demuxer::TrackInfo,
    target_output_rate: Option<u32>,
) -> DecodeResult<crate::demuxer::TrackInfo> {
    let mut output_track = track.clone();
    output_track.gapless = scale_gapless_for_output_domain(
        output_track.gapless,
        output_track.sample_rate,
        target_output_rate,
    )?;
    Ok(output_track)
}

fn scale_gapless_for_output_domain(
    gapless: Option<GaplessInfo>,
    source_rate: u32,
    target_output_rate: Option<u32>,
) -> DecodeResult<Option<GaplessInfo>> {
    let Some(info) = gapless else {
        return Ok(None);
    };
    let Some(output_rate) = target_output_rate.filter(|rate| *rate != source_rate) else {
        return Ok(Some(info));
    };

    if source_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.gapless.source",
        });
    }
    if output_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.gapless.output",
        });
    }

    Ok(Some(GaplessInfo {
        leading_frames: round_scaled_frames(info.leading_frames, source_rate, output_rate)?,
        trailing_frames: round_scaled_frames(info.trailing_frames, source_rate, output_rate)?,
    }))
}

fn round_scaled_frames(count: u64, source_rate: u32, output_rate: u32) -> DecodeResult<u64> {
    let numerator = u128::from(count)
        .saturating_mul(u128::from(output_rate))
        .saturating_add(u128::from(source_rate / 2));
    let scaled = numerator / u128::from(source_rate);
    u64::try_from(scaled).map_err(|_| DecodeError::InvalidData {
        detail: "apple gapless output-domain frame count overflow",
    })
}

/// RED (device repro): on the size-reduced apple-only build (no symphonia
/// fallback), `MediaInfo { codec: AacLc, container: None }` over real fMP4
/// bytes must resolve the shared container hint before backend dispatch.
#[cfg(all(test, apple_backend))]
mod apple_factory_tests {
    use std::{io::Cursor, num::NonZeroU32};

    use kithara_bufpool::SampleBuffer;
    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_signal::{AudioChunk, AudioSpec};
    use kithara_stream::{AudioCodec, ByteMap, MediaInfo};
    use kithara_test_fixtures::{mock_fixtures::one_packet, unit_fixtures::trim_silence};
    use kithara_test_utils::kithara;

    use super::{round_scaled_frames, track_with_output_domain_gapless};
    use crate::{
        DecodeError, DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
        DecoderTrackInfo, GaplessInfo, GaplessTrimmer,
        codec::FrameCodec,
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
        error::DecodeResult,
        fmp4::test_layout::{FakeSegmented, aac_one},
        test_pools::{TestPools, pools},
        traits::Decoder,
    };

    struct PacketDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        packet_frames: u32,
        source_rate: u32,
        next_index: u64,
        packet_count: u64,
    }

    impl PacketDemuxer {
        fn source_spec(&self) -> AudioSpec {
            AudioSpec::new(
                self.track.channels,
                NonZeroU32::new(self.source_rate).expect("test source rate is non-zero"),
            )
        }
    }

    impl Demuxer for PacketDemuxer {
        fn duration(&self) -> Option<crate::DecoderDuration> {
            Some(crate::DecoderDuration::RawLength(
                self.source_spec()
                    .duration_for(
                        self.packet_count
                            .saturating_mul(u64::from(self.packet_frames)),
                    )
                    .expect("test duration is representable"),
            ))
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            if self.next_index >= self.packet_count {
                return Ok(DemuxOutcome::Eof);
            }
            let packet_idx = self.next_index;
            self.next_index = self.next_index.saturating_add(1);
            let spec = self.source_spec();
            Ok(DemuxOutcome::Frame(Frame {
                data: &self.held,
                packet_desc: &[],
                pts: spec
                    .duration_for(packet_idx.saturating_mul(u64::from(self.packet_frames)))
                    .expect("test timestamp is representable"),
                duration: spec
                    .duration_for(u64::from(self.packet_frames))
                    .expect("test duration is representable"),
            }))
        }

        fn seek(
            &mut self,
            _target: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            self.next_index = 0;
            Ok(DemuxSeekOutcome::Landed {
                landed_at: Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    struct OutputDomainCodec {
        spec: AudioSpec,
        track_info: DecoderTrackInfo,
        pcm: Vec<f32>,
        frames_per_call: u32,
    }

    impl FrameCodec for OutputDomainCodec {
        fn decode_frame(
            &mut self,
            frame: Frame<'_>,
            metadata: crate::demuxer::PacketMetadata,
            out: &mut SampleBuffer,
        ) -> DecodeResult<crate::codec::DecodedPacket> {
            let Frame {
                data: bytes,
                pts: _pts,
                packet_desc: _packet_desc,
                ..
            } = frame;
            if bytes.is_empty() {
                out.clear();
                return Ok(crate::codec::DecodedPacket::new(0, metadata));
            }
            write_silent_frame(&self.pcm, self.spec, self.frames_per_call, out)
                .map(|frames| crate::codec::DecodedPacket::new(frames, metadata))
        }

        fn flush(&mut self) -> DecodeResult<()> {
            Ok(())
        }

        fn spec(&self) -> AudioSpec {
            self.spec
        }

        fn track_info(&self) -> DecoderTrackInfo {
            self.track_info.clone()
        }
    }

    fn write_silent_frame(
        pcm: &[f32],
        spec: AudioSpec,
        frames: u32,
        out: &mut SampleBuffer,
    ) -> DecodeResult<u32> {
        let samples = usize::try_from(frames)?
            .checked_mul(usize::from(spec.channels))
            .ok_or(DecodeError::InvalidData {
                detail: "factory test sample count overflow",
            })?;
        out.ensure_len(samples)?;
        out[..samples].copy_from_slice(&pcm[..samples]);
        out.truncate(samples);
        Ok(frames)
    }

    const fn aac_track(sample_rate: u32, gapless: Option<GaplessInfo>) -> TrackInfo {
        TrackInfo {
            sample_rate,
            gapless,
            codec: AudioCodec::AacLc,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
        }
    }

    fn output_frames(chunks: impl IntoIterator<Item = AudioChunk>) -> u64 {
        chunks
            .into_iter()
            .map(|chunk| u64::from(chunk.meta.frames))
            .sum()
    }

    #[kithara::test]
    fn apple_aac_lc_metadata_probe_is_not_rejected_as_unsupported_codec(
        aac_one: (Vec<u8>, FakeSegmented),
    ) {
        let (blob, segmented) = aac_one;
        let source = Cursor::new(blob);
        let byte_map: Arc<dyn ByteMap> = Arc::new(segmented);
        let media_info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::AacLc))
            .maybe_container(None)
            .build();
        let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
            DecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .byte_map(byte_map)
                .pools(pools())
                .build();

        let result = DecoderFactory::create_from_media_info(source, &media_info, config);

        assert!(
            !matches!(
                result,
                Err(DecodeError::UnsupportedCodec {
                    codec: AudioCodec::AacLc
                })
            ),
            "apple-only AAC-LC fMP4 metadata probe was rejected as UnsupportedCodec"
        );
    }

    #[kithara::test]
    fn apple_gapless_scaling_is_identity_without_fused_src() {
        let source_gapless = Some(GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        });
        let track = aac_track(44_100, source_gapless);

        let no_target = track_with_output_domain_gapless(&track, None)
            .expect("BUG: output-domain track without target");
        let equal_target = track_with_output_domain_gapless(&track, Some(44_100))
            .expect("BUG: output-domain track with equal target");

        assert_eq!(no_target.gapless, source_gapless);
        assert_eq!(equal_target.gapless, source_gapless);
    }

    #[kithara::test]
    fn apple_gapless_scaling_rounds_source_counts_to_output_rate() {
        let source_gapless = Some(GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        });
        let track = aac_track(44_100, source_gapless);

        let output_track = track_with_output_domain_gapless(&track, Some(48_000))
            .expect("BUG: output-domain track with fused SRC");

        assert_eq!(
            output_track.gapless,
            Some(GaplessInfo {
                leading_frames: 1115,
                trailing_frames: 480,
            })
        );
    }

    #[kithara::test]
    fn apple_scaled_gapless_trims_composed_resampled_output_domain(
        trim_silence: Vec<f32>,
        one_packet: &'static [u8],
    ) {
        const SOURCE_RATE: u32 = 44_100;
        const OUTPUT_RATE: u32 = 48_000;
        const PACKET_COUNT: u64 = 10;
        const PACKET_FRAMES: u32 = 1024;

        let source_gapless = GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        };
        let source_track = aac_track(SOURCE_RATE, Some(source_gapless));
        let output_track = track_with_output_domain_gapless(&source_track, Some(OUTPUT_RATE))
            .expect("BUG: output-domain track with fused SRC");
        let output_gapless = output_track.gapless.expect("BUG: scaled gapless");
        let frames_per_call = u32::try_from(
            round_scaled_frames(u64::from(PACKET_FRAMES), SOURCE_RATE, OUTPUT_RATE)
                .expect("BUG: scaled packet frame count"),
        )
        .expect("BUG: scaled packet frames fit u32");
        let decoded_frames = u64::from(frames_per_call).saturating_mul(PACKET_COUNT);
        let demuxer = PacketDemuxer {
            track: source_track,
            held: one_packet.to_vec(),
            next_index: 0,
            packet_count: PACKET_COUNT,
            packet_frames: PACKET_FRAMES,
            source_rate: SOURCE_RATE,
        };
        let codec = OutputDomainCodec {
            frames_per_call,
            pcm: trim_silence,
            spec: AudioSpec::new(2, NonZeroU32::new(OUTPUT_RATE).expect("test rate")),
            track_info: DecoderTrackInfo {
                gapless: Some(output_gapless),
                ..DecoderTrackInfo::default()
            },
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
        let mut trimmer =
            GaplessTrimmer::from(decoder.track_info().gapless.expect("BUG: decoder gapless"));
        let mut trimmed_frames = 0_u64;
        loop {
            match decoder.next_chunk().expect("BUG: next chunk") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    trimmed_frames = trimmed_frames.saturating_add(output_frames(
                        trimmer.push(*chunk).expect("valid gapless PCM"),
                    ));
                }
                DecoderChunkOutcome::Pending(reason) => panic!("unexpected Pending: {reason:?}"),
                DecoderChunkOutcome::Eof => {
                    trimmed_frames = trimmed_frames
                        .saturating_add(output_frames(trimmer.flush().expect("valid gapless PCM")));
                    break;
                }
            }
        }
        let expected_frames = decoded_frames
            .saturating_sub(output_gapless.leading_frames)
            .saturating_sub(output_gapless.trailing_frames);

        assert_eq!(output_gapless.leading_frames, 1115);
        assert_eq!(output_gapless.trailing_frames, 480);
        assert_eq!(decoded_frames, 11_150);
        assert_eq!(trimmed_frames, expected_frames);
    }
}

/// LABA-417 (device repro): an audiobook encoded as plain CBR MP3 carries no
/// Xing/Info frame, so only the resource length over the bitrate gives its
/// duration. Without one a seek has nothing to scale a byte offset from, and
/// `pipeline/seek/emit.rs` never moves the stream's byte cursor. What only the
/// factory owns is handing the resource length from `config.byte_len_handle`
/// to the MPEG demuxer while it opens, so that is all this test asserts.
#[cfg(all(test, apple_backend))]
mod apple_headerless_cbr_mp3_tests {
    use std::{io::Cursor, sync::atomic::AtomicU64};

    use kithara_platform::{sync::Arc, time::Duration};
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::{assets::signal_mp3_track_sine440_187s, without_xing_frame};
    use kithara_test_utils::kithara;

    use crate::{
        DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
        test_pools::{TestPools, pools},
    };

    #[kithara::test]
    fn factory_gives_the_mpeg_demuxer_the_length_behind_a_headerless_cbr_mp3() {
        let bytes = without_xing_frame(signal_mp3_track_sine440_187s().bytes());
        let total = u64::try_from(bytes.len()).expect("fixture length fits u64");
        let media_info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Mp3))
            .maybe_container(Some(ContainerFormat::MpegAudio))
            .build();
        let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
            DecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .byte_len_handle(Arc::new(AtomicU64::new(total)))
                .pools(pools())
                .build();

        let mut decoder =
            DecoderFactory::create_from_media_info(Cursor::new(bytes), &media_info, config)
                .expect("headerless CBR MP3 must open on the streaming Apple path");

        assert!(
            decoder.duration().is_some(),
            "the factory holds the only resource length on the streaming path; \
             without forwarding it the open reports no duration and every seek \
             lands nowhere"
        );
        assert_eq!(decoder.media_origin(), Some(Duration::ZERO));
        let DecoderChunkOutcome::Chunk(first) = decoder.next_chunk().expect("first headerless PCM")
        else {
            panic!("in-memory headerless source produces PCM");
        };
        assert_eq!(first.meta.timestamp, Duration::ZERO);
        assert_eq!(first.meta.frame_offset, 0);
        assert_eq!(first.meta.media_origin, Some(Duration::ZERO));
        assert!(!first.samples.is_empty());
        assert!(first.samples.iter().all(|sample| sample.is_finite()));
        let first_pcm = first.samples.to_vec();
        let target = Duration::from_millis(1_500);
        decoder.seek(target).expect("headerless content seek");
        let DecoderChunkOutcome::Chunk(sought) =
            decoder.next_chunk().expect("sought headerless PCM")
        else {
            panic!("in-memory headerless seek produces PCM");
        };
        let frame = sought
            .spec()
            .duration_for(1)
            .expect("source frame duration");
        assert!(sought.meta.timestamp.abs_diff(target) <= frame);
        assert_eq!(sought.meta.media_origin, Some(Duration::ZERO));
        decoder.seek(Duration::ZERO).expect("headerless rewind");
        let DecoderChunkOutcome::Chunk(replayed) =
            decoder.next_chunk().expect("replayed headerless PCM")
        else {
            panic!("in-memory headerless rewind produces PCM");
        };
        assert_eq!(replayed.meta.timestamp, first.meta.timestamp);
        assert_eq!(replayed.meta.frame_offset, first.meta.frame_offset);
        assert_eq!(replayed.meta.media_origin, first.meta.media_origin);
        assert_eq!(replayed.samples.as_ref(), first_pcm.as_slice());
    }
}

#[cfg(all(test, apple_backend))]
mod apple_container_identity_tests {
    use std::io::Cursor;

    use kithara_stream::{ContainerFormat, MediaInfo};
    use kithara_test_fixtures::assets::alac_silence_1s;
    use kithara_test_utils::kithara;

    use crate::{
        DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
        test_pools::{TestPools, pools},
    };

    #[kithara::test]
    #[case::file_metadata(true)]
    #[case::extension(false)]
    fn alac_in_mp4_decodes_through_both_factory_entries(#[case] metadata: bool) {
        let bytes = alac_silence_1s().bytes().to_vec();
        let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
            DecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .gapless(false)
                .pools(pools())
                .build();
        let mut decoder = if metadata {
            DecoderFactory::create_from_media_info(
                Cursor::new(bytes),
                &MediaInfo::builder().container(ContainerFormat::Mp4).build(),
                config,
            )
        } else {
            DecoderFactory::create_with_probe(Cursor::new(bytes), Some("m4a"), config)
        }
        .expect("ALAC in MP4 opens without an AAC guess");
        let mut frames = 0;
        loop {
            match decoder.next_chunk().expect("ALAC decodes") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    frames += u64::from(chunk.meta.frames);
                    assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                }
                DecoderChunkOutcome::Eof => break,
                DecoderChunkOutcome::Pending(reason) => {
                    panic!("complete MP4 is pending: {reason:?}")
                }
            }
        }
        assert_eq!(frames, 44_100);
    }
}

#[cfg(all(test, apple_backend))]
mod apple_output_clock_tests {
    use std::io::Cursor;

    use kithara_platform::time::Duration;
    use kithara_signal::AudioChunkInfo;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::assets::{
        signal_flac_saw_1s, signal_profile_m4a_alac_44100_2ch_16bit,
        signal_profile_mp3_libmp3lame_44100_1ch, signal_wav_saw_1s,
    };
    use kithara_test_utils::kithara;

    use crate::{
        Decoder, DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, PcmOrigin,
        apple::{AppleAudioFileDemuxer, AppleCodec, SourceOpenMode},
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::Demuxer,
        test_pools::{TestPools, pools},
    };

    fn decoder() -> Box<dyn Decoder> {
        let media = MediaInfo::builder()
            .codec(AudioCodec::Mp3)
            .container(ContainerFormat::MpegAudio)
            .build();
        let config: DecoderConfig<kithara_resampler::NoResamplerBackend, TestPools> =
            DecoderConfig::builder()
                .backend(DecoderBackend::Apple)
                .gapless(true)
                .pools(pools())
                .build();
        DecoderFactory::create_from_media_info(
            Cursor::new(signal_profile_mp3_libmp3lame_44100_1ch().bytes().to_vec()),
            &media,
            config,
        )
        .expect("open native Apple MPEG fixture")
    }

    #[kithara::hang_watchdog]
    fn drain(decoder: &mut dyn Decoder) -> (Vec<AudioChunkInfo>, Vec<f32>) {
        let mut metadata = Vec::new();
        let mut pcm = Vec::new();
        loop {
            hang_tick!();
            match decoder.next_chunk().expect("decode native Apple PCM") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    pcm.extend_from_slice(&chunk.samples);
                    assert_eq!(chunk.frames(), chunk.meta.frames as usize);
                    assert_eq!(
                        chunk.samples.len(),
                        chunk.frames() * usize::from(chunk.meta.spec.channels)
                    );
                    assert!(chunk.samples.iter().all(|sample| sample.is_finite()));
                    metadata.push(chunk.meta);
                    hang_reset!();
                }
                DecoderChunkOutcome::Pending(_) => {}
                DecoderChunkOutcome::Eof => break,
            }
        }
        (metadata, pcm)
    }

    #[kithara::test]
    #[case::pcm_upsample(signal_wav_saw_1s().bytes(), AudioCodec::Pcm, ContainerFormat::Wav, 1, 48_000)]
    #[case::pcm_downsample(signal_wav_saw_1s().bytes(), AudioCodec::Pcm, ContainerFormat::Wav, 1, 32_000)]
    #[case::flac_upsample(signal_flac_saw_1s().bytes(), AudioCodec::Flac, ContainerFormat::Flac, 1, 48_000)]
    #[case::flac_downsample(signal_flac_saw_1s().bytes(), AudioCodec::Flac, ContainerFormat::Flac, 1, 32_000)]
    #[case::alac_upsample(signal_profile_m4a_alac_44100_2ch_16bit().bytes(), AudioCodec::Alac, ContainerFormat::Mp4, 6, 48_000)]
    #[case::alac_downsample(signal_profile_m4a_alac_44100_2ch_16bit().bytes(), AudioCodec::Alac, ContainerFormat::Mp4, 6, 32_000)]
    fn apple_lossless_rate_conversion_keeps_content_zero_and_rewinds_every_sample(
        #[case] bytes: &'static [u8],
        #[case] source_codec: AudioCodec,
        #[case] container: ContainerFormat,
        #[case] seconds: u64,
        #[case] output_rate: u32,
    ) {
        let demuxer = AppleAudioFileDemuxer::open_for_with_mode(
            Box::new(Cursor::new(bytes.to_vec())),
            source_codec,
            Some(container),
            SourceOpenMode::Complete,
        )
        .expect("open native PCM ramp");
        let track = demuxer.track_info().clone();
        assert_eq!(track.sample_rate, 44_100);
        let codec = AppleCodec::open_with_config(&track, true, Some(output_rate))
            .expect("open native PCM rate converter");
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
        let (original, original_pcm) = drain(&mut decoder);
        let frames: u64 = original.iter().map(|meta| u64::from(meta.frames)).sum();
        assert_eq!(frames, seconds * u64::from(output_rate));
        let channels = usize::from(track.channels);
        let zero_frame =
            u32::try_from(32_768_u64 * u64::from(output_rate) / 44_100).expect("ramp zero index");
        let zero_index = usize::try_from(zero_frame).expect("ramp index") * channels;
        for channel in 0..channels {
            let crossing = |pcm: &[f32]| {
                let before = f64::from(*pcm.get(zero_index + channel)?);
                let after = f64::from(*pcm.get(zero_index + channels + channel)?);
                if !(before <= 0.0 && after >= 0.0 && after > before) {
                    return None;
                }
                let frame = f64::from(zero_frame) - before / (after - before);
                Some(frame * 44_100.0 / f64::from(output_rate))
            };
            let actual = crossing(&original_pcm).expect("the nominal bracket contains ramp zero");
            assert!(
                (actual - 32_768.0).abs() < 0.5,
                "the resampled ramp must cross zero at source frame 32768, got {actual}",
            );
            for shifted_frames in [1, 18] {
                assert!(
                    crossing(&original_pcm[shifted_frames * channels..]).is_none(),
                    "the phase oracle must reject a shift of {shifted_frames} output frames",
                );
            }
        }
        assert_eq!(original[0].timestamp, Duration::ZERO);
        assert_eq!(assert_clock(&original, Duration::ZERO), frames);
        assert_eq!(
            original.last().expect("PCM end").end_timestamp,
            Duration::from_secs(seconds)
        );
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(Duration::ZERO),
        );
        decoder
            .seek_prefix(Duration::ZERO)
            .expect("native PCM rewind");
        let (replay, replay_pcm) = drain(&mut decoder);
        assert_eq!(assert_clock(&replay, Duration::ZERO), frames);
        assert_eq!(replay_pcm, original_pcm);
    }

    fn assert_clock(chunks: &[AudioChunkInfo], origin: Duration) -> u64 {
        let first = chunks.first().expect("decoded native PCM");
        let mut frames = 0u64;
        for (index, meta) in chunks.iter().enumerate() {
            assert_eq!(meta.spec, first.spec);
            let expected = origin + meta.spec.duration_for(frames).expect("PCM extent");
            assert!(
                meta.timestamp.abs_diff(expected) <= Duration::from_nanos(1),
                "native PCM clock index={index} actual={:?} expected={expected:?} \
                 frame_offset={} total_frames={frames} chunk_frames={} spec={:?} \
                 origin={origin:?} first={first:?}",
                meta.timestamp,
                meta.frame_offset,
                meta.frames,
                meta.spec,
            );
            assert_eq!(
                meta.frame_offset,
                first.frame_offset + frames,
                "the raw clock must follow the PCM actually emitted"
            );
            frames += u64::from(meta.frames);
        }
        frames
    }

    #[kithara::test]
    fn apple_mpeg_buffered_head_and_eof_share_one_raw_clock() {
        let mut decoder = decoder();
        let gapless = decoder
            .gapless_profile(Some(AudioCodec::Mp3))
            .gapless()
            .expect("LAME fixture trim");
        let (chunks, _) = drain(decoder.as_mut());
        let frames = assert_clock(&chunks, Duration::ZERO);
        assert_eq!(chunks[0].timestamp, Duration::ZERO);
        assert_eq!(chunks[0].frame_offset, 0);
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(Duration::ZERO)
        );
        assert_eq!(
            frames - gapless.leading_frames - gapless.trailing_frames,
            264_600
        );
    }

    #[kithara::test]
    fn apple_mpeg_seek_preserves_original_origin_and_exact_sought_suffix() {
        let mut decoder = decoder();
        let (original, _) = drain(decoder.as_mut());
        let original_frames = assert_clock(&original, Duration::ZERO);
        let target = Duration::from_millis(1_500);
        decoder.seek(target).expect("native MPEG content seek");
        let (suffix, _) = drain(decoder.as_mut());
        let suffix_frames = assert_clock(&suffix, target);
        assert_eq!(suffix[0].frame_offset, 66_150);
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(Duration::ZERO)
        );
        assert!(
            suffix_frames.abs_diff(original_frames - 66_150) <= 1,
            "a sought suffix must keep all remaining decoded PCM"
        );
    }

    #[kithara::test]
    fn apple_mpeg_rewind_reanchors_the_codec_from_the_first_input() {
        let mut decoder = decoder();
        let (original, original_pcm) = drain(decoder.as_mut());
        let original_frames = assert_clock(&original, Duration::ZERO);
        decoder.seek(Duration::ZERO).expect("native MPEG rewind");
        let (replay, replay_pcm) = drain(decoder.as_mut());
        assert_eq!(assert_clock(&replay, Duration::ZERO), original_frames);
        assert_eq!(replay[0].frame_offset, 0);
        assert_eq!(
            decoder.initial_pcm_origin().map(PcmOrigin::position),
            Some(Duration::ZERO)
        );
        assert_eq!(replay_pcm, original_pcm);
    }
}
