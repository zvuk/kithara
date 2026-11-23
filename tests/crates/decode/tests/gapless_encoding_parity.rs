use std::{io::Cursor, num::NonZeroU32};

use kithara::{platform::time::Duration, stream::AudioCodec};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper,
    bufpool_ext::pools,
    fixture_protocol::{
        GaplessEncoding, PackagedAudioRequest, PackagedAudioSource, PackagedSignal,
    },
};
use reqwest::Client;

use crate::gapless_common::{
    AAC_FRAME_SAMPLES, AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_SEGMENT_SECS, AAC_GAPLESS_SEGMENTS,
    AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS, GAPLESS_SAMPLE_RATE,
};

#[cfg(not(target_os = "android"))]
#[kithara::test(native)]
#[case::lc(AudioCodec::AacLc, 128_000)]
#[case::he(AudioCodec::AacHe, 64_000)]
fn itunsmpb_mux_probe_roundtrip_preserves_pcm_counts(
    #[case] codec: AudioCodec,
    #[case] bit_rate: u64,
) {
    use kithara::decode::{
        DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, GaplessInfo,
    };
    use kithara_test_fixtures::fmp4::{GaplessEncoding, mux_audio_track};

    let region = pools();
    let track = encoded_clock_domain_track(codec, bit_rate, &region);
    let info = track.media_info.clone();
    assert_eq!(track.timescale, 24_000);
    assert_eq!(track.media_info.sample_rate, Some(48_000));
    assert!(track.codec_delay > 0);
    let expected = GaplessInfo::new(u64::from(track.codec_delay), 64);
    let output_rate = NonZeroU32::new(48_000).expect("output rate");
    let declaration_rate = NonZeroU32::new(track.timescale).expect("declaration rate");
    let tag_counts = GaplessInfo::new(
        kithara::signal::rescale_frame_round(
            expected.leading_frames,
            output_rate,
            declaration_rate,
        )
        .expect("leading mdhd ticks"),
        kithara::signal::rescale_frame_round(
            expected.trailing_frames,
            output_rate,
            declaration_rate,
        )
        .expect("trailing mdhd ticks"),
    );
    let package = mux_audio_track(&track, GaplessEncoding::ItunSmpb).expect("actual fixture mux");
    let tag = format!(
        " 00000000 {:08X} {:08X} ",
        tag_counts.leading_frames, tag_counts.trailing_frames,
    );
    assert!(
        package
            .init_segment
            .windows(tag.len())
            .any(|bytes| bytes == tag.as_bytes()),
        "raw tag must declare its counts in mdhd ticks",
    );
    let mut decoder = DecoderFactory::create_from_media_info(
        Cursor::new(Vec::<u8>::from(package.clone())),
        &info,
        DecoderConfig::<kithara::resampler::NoResamplerBackend, _>::builder()
            .backend(DecoderBackend::Symphonia)
            .pools(region.clone())
            .gapless(false)
            .build(),
    )
    .expect("open the muxed real packets without metadata trim");
    let chunk = match decoder.next_chunk().expect("decode muxed real packets") {
        DecoderChunkOutcome::Chunk(chunk) => chunk,
        other => panic!("real packets must produce PCM, got {other:?}"),
    };
    assert!(chunk.meta.frames > 0);
    assert_eq!(chunk.meta.spec.sample_rate.get(), 48_000);
    let mut declared = decoder_with_declared_trim(Vec::<u8>::from(package), &info);
    let prepared = match declared.next_chunk().expect("prepare declared real PCM") {
        DecoderChunkOutcome::Chunk(chunk) => chunk,
        other => panic!("declared real packets must produce PCM, got {other:?}"),
    };
    assert!(prepared.meta.frames > 0);
    assert_eq!(prepared.meta.spec.sample_rate.get(), 48_000);
    assert_eq!(
        declared.track_info().gapless,
        Some(expected),
        "iTunSMPB counts declared in mdhd ticks must be rescaled to output PCM frames",
    );
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native)]
#[case::lc(AudioCodec::AacLc, 128_000)]
#[case::he(AudioCodec::AacHe, 64_000)]
fn edts_mux_roundtrip_preserves_pcm_counts(#[case] codec: AudioCodec, #[case] bit_rate: u64) {
    use kithara::decode::{DecoderChunkOutcome, GaplessInfo};
    use kithara_test_fixtures::fmp4::{GaplessEncoding, mux_audio_track};

    let track = encoded_clock_domain_track(codec, bit_rate, &pools());
    assert_eq!(track.media_info.sample_rate, Some(48_000));
    assert_eq!(track.timescale, 24_000);
    assert!(track.codec_delay > 0);
    let expected = GaplessInfo::new(
        u64::from(track.codec_delay),
        u64::from(track.trailing_delay),
    );
    let package = mux_audio_track(&track, GaplessEncoding::Edts).expect("actual edit-list mux");
    let mut decoder = decoder_with_declared_trim(Vec::<u8>::from(package), &track.media_info);
    let pcm = match decoder
        .next_chunk()
        .expect("decode the real edit-list package")
    {
        DecoderChunkOutcome::Chunk(chunk) => chunk,
        other => panic!("real edit-list package must produce PCM: {other:?}"),
    };
    assert!(pcm.meta.frames > 0);
    assert_eq!(pcm.meta.spec.sample_rate.get(), 48_000);
    assert_eq!(decoder.track_info().gapless, Some(expected));
}

#[cfg(not(target_os = "android"))]
fn encoded_clock_domain_track(
    codec: AudioCodec,
    bit_rate: u64,
    region: &kithara::bufpool::PoolRegion<kithara_integration_tests::bufpool_ext::TestPools>,
) -> kithara::encode::EncodedTrack {
    use kithara::{
        encode::{EncoderFactory, PackagedEncodeRequest},
        stream::{ContainerFormat, MediaInfo},
    };
    use kithara_test_fixtures::signal::{Pcm, Wave};

    let pcm = Pcm::new(48_000, 2, 16_384, Wave::Sawtooth);
    let info = MediaInfo::builder()
        .codec(codec)
        .container(ContainerFormat::Fmp4)
        .sample_rate(48_000)
        .channels(2)
        .build();
    EncoderFactory::encode_packaged(
        region,
        &PackagedEncodeRequest::builder()
            .pcm(&pcm)
            .media_info(info.clone())
            .timescale(24_000)
            .bit_rate(bit_rate)
            .packets_per_segment(32)
            .encoder_delay(0)
            .trailing_delay(64)
            .build(),
    )
    .expect("encode a real track with an independent media timescale")
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn implicit_sbr_geometry_reaches_the_audio_trimmer_before_first_pcm() {
    use std::io::Write;

    use kithara::{
        assets::{AssetStore, StorageBackend},
        audio::{Audio, AudioConfig, AudioDecoderConfig, ChunkOutcome},
        decode::{DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory, GaplessMode},
        file::{File, FileConfig, FileSrc},
        platform::{
            sync::Arc,
            tokio::task::{spawn_blocking, yield_now},
        },
        resampler::NoResamplerBackend,
        signal::AudioSpec,
        stream::mock::NoopWorkerWake,
    };
    use kithara_integration_tests::bufpool_ext::TestPools;

    let region = pools();
    let encode_region = region.clone();
    let (stored, first_raw, leading) = spawn_blocking(move || {
        let (bytes, info, leading) = implicit_sbr_media(&encode_region);
        let output_rate = NonZeroU32::new(48_000).expect("output rate");
        let core_rate = NonZeroU32::new(24_000).expect("core rate");
        let mut raw = DecoderFactory::create_from_media_info(
            Cursor::new(bytes.clone()),
            &info,
            DecoderConfig::<NoResamplerBackend, TestPools>::builder()
                .pools(encode_region)
                .backend(DecoderBackend::Symphonia)
                .gapless(false)
                .build(),
        )
        .expect("real implicit-SBR source");
        assert_eq!(raw.spec().sample_rate.get(), core_rate.get());
        let first = match raw.next_chunk().expect("real implicit-SBR PCM") {
            DecoderChunkOutcome::Chunk(chunk) => chunk,
            other => panic!("real implicit-SBR packets must produce PCM: {other:?}"),
        };
        assert!(first.meta.frames > 0);
        assert_eq!(first.spec().sample_rate, output_rate);
        let mut stored = tempfile::NamedTempFile::new().expect("local encoded source");
        stored
            .write_all(&bytes)
            .expect("write local encoded source");
        (stored, first.meta.timestamp, leading)
    })
    .await
    .expect("real packet preparation");
    let file = FileConfig::for_src(FileSrc::Local(stored.path().to_path_buf()))
        .store(
            AssetStore::builder(region.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint("mp4".to_owned())
        .decoder(
            AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .gapless_mode(GaplessMode::MediaOnly)
                .build(),
        )
        .build();
    let mut audio = Audio::prepare(config, Arc::new(NoopWorkerWake), region)
        .await
        .expect("actual Audio preparation");
    let first = loop {
        match audio.next_chunk().expect("actual Audio PCM") {
            ChunkOutcome::Chunk(chunk) => break chunk,
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => panic!("the declared source has an audible suffix"),
        }
    };
    let spec = AudioSpec::new(2, NonZeroU32::new(48_000).expect("output rate"));
    assert_eq!(first.spec(), spec);
    let expected = first_raw + spec.duration_for(leading).expect("declared output delay");
    let tolerance = Duration::from_secs_f64(
        kithara_integration_tests::phase_continuity::TOLERANCE_SAMPLES / 48_000.0,
    );
    assert!(
        first.meta.timestamp.abs_diff(expected) <= tolerance,
        "first PCM must follow the full declared output prefix: got {:?}, expected {expected:?}",
        first.meta.timestamp,
    );
}

#[cfg(not(target_os = "android"))]
fn implicit_sbr_media(
    region: &kithara::bufpool::PoolRegion<kithara_integration_tests::bufpool_ext::TestPools>,
) -> (Vec<u8>, kithara::stream::MediaInfo, u64) {
    use kithara::signal::rescale_frame_round;
    use kithara_test_fixtures::fmp4::{GaplessEncoding, mux_audio_track};

    let mut track = encoded_clock_domain_track(AudioCodec::AacHe, 64_000, region);
    let output_rate = NonZeroU32::new(48_000).expect("output rate");
    let core_rate = NonZeroU32::new(24_000).expect("core rate");
    let leading = u64::from(track.codec_delay);
    track.codec_delay =
        u32::try_from(rescale_frame_round(leading, output_rate, core_rate).expect("core delay"))
            .expect("core delay fits u32");
    track.trailing_delay = u32::try_from(
        rescale_frame_round(u64::from(track.trailing_delay), output_rate, core_rate)
            .expect("core trailing frames"),
    )
    .expect("core trailing frames fit u32");
    track.media_info.sample_rate = Some(core_rate.get());
    track.codec_config = vec![0x13, 0x10];
    let bytes = Vec::<u8>::from(
        mux_audio_track(&track, GaplessEncoding::Edts).expect("core-clock edit package"),
    );
    (bytes, track.media_info, leading)
}

#[cfg(not(target_os = "android"))]
async fn open_implicit_audio(
    path: &std::path::Path,
) -> kithara::audio::Audio<
    kithara::stream::Stream<kithara::file::File<kithara_integration_tests::bufpool_ext::TestPools>>,
> {
    use kithara::{
        assets::{AssetStore, StorageBackend},
        audio::{Audio, AudioConfig, AudioDecoderConfig},
        decode::{DecoderBackend, GaplessMode},
        file::{File, FileConfig, FileSrc},
        platform::sync::Arc,
        resampler::NoResamplerBackend,
        stream::mock::NoopWorkerWake,
    };
    use kithara_integration_tests::bufpool_ext::TestPools;

    let region = pools();
    let file = FileConfig::for_src(FileSrc::Local(path.to_path_buf()))
        .store(
            AssetStore::builder(region.clone())
                .backend(StorageBackend::Memory)
                .build(),
        )
        .build();
    let config = AudioConfig::<File<TestPools>, NoResamplerBackend>::for_stream(file)
        .hint("mp4".to_owned())
        .decoder(
            AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .gapless_mode(GaplessMode::MediaOnly)
                .build(),
        )
        .build();
    Audio::prepare(config, Arc::new(NoopWorkerWake), region)
        .await
        .expect("actual Audio preparation")
}

#[cfg(not(target_os = "android"))]
async fn remaining_implicit_pcm(
    audio: &mut kithara::audio::Audio<
        kithara::stream::Stream<
            kithara::file::File<kithara_integration_tests::bufpool_ext::TestPools>,
        >,
    >,
) -> Result<(Option<kithara::signal::AudioChunkInfo>, Vec<f32>), kithara::audio::AudioReadError> {
    use kithara::{audio::ChunkOutcome, platform::tokio::task::yield_now};
    let mut first = None;
    let mut pcm = Vec::new();
    loop {
        match audio.next_chunk()? {
            ChunkOutcome::Chunk(chunk) => {
                assert_eq!(chunk.meta.spec.sample_rate.get(), 48_000);
                if first.is_none() {
                    first = Some(chunk.meta);
                }
                pcm.extend_from_slice(&chunk.samples);
            }
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => return Ok((first, pcm)),
        }
    }
}

#[cfg(not(target_os = "android"))]
#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
#[case::zero(0)]
#[case::content(150)]
#[case::past_end(1_000)]
async fn an_implicit_sbr_seek_before_first_output_matches_the_prepared_seek(
    #[case] target_ms: u64,
) {
    use std::io::Write;

    use kithara::{
        audio::ChunkOutcome,
        platform::tokio::task::{spawn_blocking, yield_now},
    };

    let stored = spawn_blocking(|| {
        let (bytes, _, _) = implicit_sbr_media(&pools());
        let mut file = tempfile::NamedTempFile::new().expect("local encoded source");
        file.write_all(&bytes).expect("write encoded source");
        file
    })
    .await
    .expect("real packet preparation");
    let mut reference = open_implicit_audio(stored.path()).await;
    loop {
        match reference.next_chunk().expect("prepare reference PCM") {
            ChunkOutcome::Chunk(chunk) => {
                assert_eq!(chunk.meta.spec.sample_rate.get(), 48_000);
                break;
            }
            ChunkOutcome::Pending { .. } => yield_now().await,
            ChunkOutcome::Eof { .. } => panic!("reference must have audible PCM"),
        }
    }
    let mut subject = open_implicit_audio(stored.path()).await;
    assert_eq!(
        subject.spec().sample_rate.get(),
        24_000,
        "seek must precede actual PCM geometry"
    );
    let target = Duration::from_millis(target_ms);
    reference.seek(target).expect("prepared reference seek");
    subject.seek(target).expect("pre-output subject seek");
    let expected = remaining_implicit_pcm(&mut reference).await;
    let actual = remaining_implicit_pcm(&mut subject).await;
    if target_ms == 1_000 {
        use kithara::audio::{AudioReadError, DecodeErrorKind, FailureSource, TrackFailureKind};
        for (path, result) in [("prepared", expected), ("pre-output", actual)] {
            match result {
                Ok((first, pcm)) => {
                    assert!(
                        first.is_none() && pcm.is_empty(),
                        "{path} out-of-range seek produced PCM"
                    );
                    tracing::info!(path, terminal = "EOF", "out-of-range seek disposition");
                }
                Err(AudioReadError::Stream {
                    source: FailureSource::Producer { failure },
                    ..
                }) => {
                    assert_eq!(
                        failure,
                        TrackFailureKind::Decode {
                            kind: DecodeErrorKind::SeekOutOfRange
                        }
                    );
                    tracing::info!(
                        path,
                        terminal = "SeekOutOfRange",
                        "out-of-range seek disposition"
                    );
                }
                Err(other) => panic!("unexpected {path} out-of-range failure: {other:?}"),
            }
        }
        return;
    }
    let (expected_first, expected_pcm) = expected.expect("prepared seek PCM");
    let (actual_first, actual_pcm) = actual.expect("pre-output seek PCM");
    assert_eq!(
        actual_pcm.len(),
        expected_pcm.len(),
        "remaining seek PCM differs: actual={actual_first:?}, reference={expected_first:?}"
    );
    assert!(!expected_pcm.is_empty());
    let expected = expected_first.expect("prepared seek output");
    let actual = actual_first.expect("pre-output seek output");
    let tolerance = Duration::from_secs_f64(
        kithara_integration_tests::phase_continuity::TOLERANCE_SAMPLES / 48_000.0,
    );
    assert!(
        actual.timestamp.abs_diff(expected.timestamp) <= tolerance,
        "seek media position differs: actual={actual:?}, reference={expected:?}"
    );
    assert_eq!(actual.media_origin, expected.media_origin);
    let first_difference = actual_pcm
        .iter()
        .zip(&expected_pcm)
        .position(|(actual, expected)| actual.to_bits() != expected.to_bits());
    assert!(
        first_difference.is_none(),
        "cropped seek PCM first differs at sample {first_difference:?}"
    );
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(1))]
async fn gapless_encoding_variants_yield_matching_decoder_metadata(
    #[future(awt)] metadata_sources: (TestServerHelper, [Vec<u8>; 4]),
) {
    let (_server, [edts, itunsmpb, both, none]) = metadata_sources;

    let edts = probe_encoded_source(edts);
    let itunsmpb = probe_encoded_source(itunsmpb);
    let both = probe_encoded_source(both);
    let none = probe_encoded_source(none);

    let edts_info = edts.expect("Edts fixture must yield gapless info");
    let itunsmpb_info = itunsmpb.expect("ItunSmpb fixture must yield gapless info");
    let both_info = both.expect("Both fixture must yield gapless info");

    assert_eq!(
        edts_info, itunsmpb_info,
        "edts and iTunSMPB must report identical GaplessInfo for the same fixture"
    );
    assert_eq!(
        edts_info, both_info,
        "Both must report identical GaplessInfo to a single-source fixture (elst wins)"
    );
    let expected_leading = u64::from(AAC_GAPLESS_ENCODER_DELAY)
        + u64::try_from(AAC_FRAME_SAMPLES).expect("AAC frame samples fit u64");
    assert_eq!(
        edts_info.leading_frames, expected_leading,
        "leading_frames must equal encoder_delay + native AAC priming"
    );
    assert_eq!(
        u32::try_from(edts_info.trailing_frames).expect("trailing fits u32"),
        AAC_GAPLESS_TRAILING_DELAY,
        "trailing_frames must equal the configured trailing_delay"
    );

    assert!(
        none.is_none(),
        "None encoding must produce a fixture without gapless metadata, got {none:?}"
    );
}

fn probe_encoded_source(init_bytes: Vec<u8>) -> Option<kithara::decode::GaplessInfo> {
    let info = kithara::stream::MediaInfo::builder()
        .codec(AudioCodec::AacLc)
        .container(kithara::stream::ContainerFormat::Fmp4)
        .sample_rate(GAPLESS_SAMPLE_RATE)
        .channels(GAPLESS_CHANNELS)
        .build();
    let decoder = decoder_with_declared_trim(init_bytes, &info);
    assert_eq!(Some(decoder.spec().sample_rate.get()), info.sample_rate);
    decoder.track_info().gapless
}

fn decoder_with_declared_trim(
    bytes: Vec<u8>,
    info: &kithara::stream::MediaInfo,
) -> Box<dyn kithara::decode::Decoder> {
    use kithara::{
        decode::{DecoderBackend, DecoderConfig, DecoderFactory},
        resampler::NoResamplerBackend,
    };

    #[cfg(target_os = "android")]
    let backend = DecoderBackend::Android;
    #[cfg(not(target_os = "android"))]
    let backend = DecoderBackend::Symphonia;

    DecoderFactory::create_from_media_info(
        Cursor::new(bytes),
        info,
        DecoderConfig::<NoResamplerBackend, _>::builder()
            .pools(pools())
            .backend(backend)
            .gapless(true)
            .build(),
    )
    .expect("prepare real decoder metadata")
}

async fn build_metadata_source(server: &TestServerHelper, encoding: GaplessEncoding) -> Vec<u8> {
    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: None,
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source: PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz: 1_000.0 }),
                    gapless_encoding: encoding,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create gapless encoding parity fixture");

    let client = Client::new();
    let url = created.init_url(0).to_string();
    let response = client.get(&url).send().await.expect("fetch init segment");
    let mut bytes = response
        .bytes()
        .await
        .expect("read init segment bytes")
        .to_vec();
    let media = client
        .get(created.segment_url(0, 0))
        .send()
        .await
        .expect("fetch the first media segment")
        .bytes()
        .await
        .expect("read the first media segment");
    bytes.extend_from_slice(&media);
    bytes
}

#[kithara::fixture]
async fn metadata_sources() -> (TestServerHelper, [Vec<u8>; 4]) {
    let server = TestServerHelper::new().await;
    let segments = [
        build_metadata_source(&server, GaplessEncoding::Edts).await,
        build_metadata_source(&server, GaplessEncoding::ItunSmpb).await,
        build_metadata_source(&server, GaplessEncoding::Both).await,
        build_metadata_source(&server, GaplessEncoding::None).await,
    ];
    (server, segments)
}
