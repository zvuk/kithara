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
    let package = mux_audio_track(&track, GaplessEncoding::ItunSmpb).expect("actual fixture mux");
    let tag = format!(
        " 00000000 {:08X} {:08X} ",
        expected.leading_frames, expected.trailing_frames,
    );
    assert!(
        package
            .init_segment
            .windows(tag.len())
            .any(|bytes| bytes == tag.as_bytes()),
        "raw tag must retain the encoder's PCM-frame counts",
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
        "mdhd ticks must not rescale a raw PCM-count tag",
    );
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
        signal::{AudioSpec, rescale_frame_round},
        stream::mock::NoopWorkerWake,
    };
    use kithara_integration_tests::bufpool_ext::TestPools;
    use kithara_test_fixtures::fmp4::{GaplessEncoding, mux_audio_track};

    let region = pools();
    let encode_region = region.clone();
    let (stored, first_raw, leading) = spawn_blocking(move || {
        let mut track = encoded_clock_domain_track(AudioCodec::AacHe, 64_000, &encode_region);
        let output_rate = NonZeroU32::new(48_000).expect("output rate");
        let core_rate = NonZeroU32::new(24_000).expect("core rate");
        let leading = u64::from(track.codec_delay);
        track.codec_delay = u32::try_from(
            rescale_frame_round(leading, output_rate, core_rate).expect("core delay"),
        )
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
        let mut raw = DecoderFactory::create_from_media_info(
            Cursor::new(bytes.clone()),
            &track.media_info,
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

    DecoderFactory::create_from_media_info(
        Cursor::new(bytes),
        info,
        DecoderConfig::<NoResamplerBackend, _>::builder()
            .pools(pools())
            .backend(DecoderBackend::Symphonia)
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
