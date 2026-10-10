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
use crate::{DecodeError, DecodeResult, Decoder, DecoderConfig, GaplessInfo, traits::BoxedSource};

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
    use crate::apple::AppleCodec;

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
            use crate::demuxer::Demuxer;

            let output_track =
                track_with_output_domain_gapless(demuxer.track_info(), target_output_rate)?;
            if output_track.gapless.is_some() {
                demuxer.set_gapless(output_track.gapless);
            }
            AppleCodec::open_with_config(&output_track, gapless, target_output_rate)
        });
    }

    if crate::apple::AppleAudioFileDemuxer::supports(codec, container) {
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
    use crate::{
        apple::{AppleAudioFileDemuxer, AppleCodec, SourceOpenMode},
        demuxer::Demuxer,
        gapless::scoped_probe,
    };
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
        fn duration(&self) -> Option<Duration> {
            Some(
                self.source_spec()
                    .duration_for(
                        self.packet_count
                            .saturating_mul(u64::from(self.packet_frames)),
                    )
                    .expect("test duration is representable"),
            )
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
            bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut SampleBuffer,
        ) -> DecodeResult<u32> {
            if bytes.is_empty() {
                out.clear();
                return Ok(0);
            }
            write_silent_frame(&self.pcm, self.spec, self.frames_per_call, out)
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
                    trimmed_frames =
                        trimmed_frames.saturating_add(output_frames(trimmer.push(*chunk)));
                }
                DecoderChunkOutcome::Pending(reason) => panic!("unexpected Pending: {reason:?}"),
                DecoderChunkOutcome::Eof => {
                    trimmed_frames = trimmed_frames.saturating_add(output_frames(trimmer.flush()));
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

    use kithara_platform::sync::Arc;
    use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
    use kithara_test_fixtures::{assets::signal_mp3_track_sine440_187s, without_xing_frame};
    use kithara_test_utils::kithara;

    use crate::{
        DecoderBackend, DecoderConfig, DecoderFactory,
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

        let decoder =
            DecoderFactory::create_from_media_info(Cursor::new(bytes), &media_info, config)
                .expect("headerless CBR MP3 must open on the streaming Apple path");

        assert!(
            decoder.duration().is_some(),
            "the factory holds the only resource length on the streaming path; \
             without forwarding it the open reports no duration and every seek \
             lands nowhere"
        );
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
