use std::{io::Cursor, num::NonZeroU32};

use kithara::{
    decode::{DecodeResult, DecoderConfig, DecoderFactory, GaplessTrimmer, InnerDecoder},
    platform::time::Duration,
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_test_utils::{
    HlsFixtureBuilder, SignalFormat, SignalSpec, SignalSpecLength, TestServerHelper,
    fixture_protocol::{PackagedAudioRequest, PackagedAudioSource, PackagedSignal},
};
use reqwest::Client;

use crate::gapless_common::{
    AAC_FRAME_SAMPLES, AAC_GAPLESS_ENCODER_DELAY, AAC_GAPLESS_TRAILING_DELAY, GAPLESS_CHANNELS,
    GAPLESS_SAMPLE_RATE,
};

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn generated_aac_elst_visible_frames_match_generated_timing_across_factory_paths() {
    let server = TestServerHelper::new().await;
    let fixture =
        generated_aac_elst_fixture(&server, PackagedSignal::Sine { freq_hz: 1_000.0 }, 0).await;

    let direct = create_decoder_from_media_info(&fixture, DecoderConfig::default())
        .expect("create direct AAC fMP4 decoder");
    let direct_gapless = direct
        .track_info()
        .gapless
        .expect("direct gapless metadata");
    let expected_leading = u64::from(AAC_GAPLESS_ENCODER_DELAY)
        + u64::try_from(AAC_FRAME_SAMPLES).expect("AAC frame samples fit u64");
    assert_eq!(direct_gapless.leading_frames, expected_leading);
    assert_eq!(
        direct_gapless.trailing_frames,
        u64::from(AAC_GAPLESS_TRAILING_DELAY)
    );
    let direct_pcm = decode_visible_frames(direct).expect("decode direct AAC fMP4");

    let probe = create_decoder_with_probe(
        fixture.bytes.clone(),
        "m4a",
        DecoderConfig {
            hint: Some("m4a".to_string()),
            ..DecoderConfig::default()
        },
    )
    .expect("create probe AAC fMP4 decoder");
    let probe_gapless = probe.track_info().gapless.expect("probe gapless metadata");
    assert_eq!(probe_gapless, direct_gapless);
    let probe_pcm = decode_visible_frames(probe).expect("decode probe AAC fMP4");

    let preferred = create_decoder_from_media_info(
        &fixture,
        DecoderConfig {
            prefer_hardware: true,
            ..DecoderConfig::default()
        },
    )
    .expect("create preferred-backend AAC fMP4 decoder");
    let preferred_pcm = decode_visible_frames(preferred).expect("decode preferred AAC fMP4");

    assert_frames_within(
        direct_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "direct AAC fMP4",
    );
    assert_frames_within(
        probe_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "probe AAC fMP4",
    );
    assert_frames_within(
        preferred_pcm.frames,
        fixture.expected_visible_frames,
        2,
        "preferred AAC fMP4",
    );
    assert_frames_within(
        probe_pcm.frames,
        direct_pcm.frames,
        2,
        "probe vs direct AAC",
    );
    assert_frames_within(
        preferred_pcm.frames,
        direct_pcm.frames,
        2,
        "preferred vs direct AAC",
    );
}

struct GaplessFixture {
    bytes: Vec<u8>,
    media_info: MediaInfo,
    expected_visible_frames: usize,
}

struct DecodedFrames {
    frames: usize,
}

async fn generated_aac_elst_fixture(
    server: &TestServerHelper,
    signal: PackagedSignal,
    start_frame: u64,
) -> GaplessFixture {
    let source = PackagedAudioSource::Signal(signal);

    let created = server
        .create_hls(
            HlsFixtureBuilder::new()
                .variant_count(1)
                .segments_per_variant(crate::gapless_common::AAC_GAPLESS_SEGMENTS)
                .segment_duration_secs(crate::gapless_common::AAC_GAPLESS_SEGMENT_SECS)
                .packaged_audio(PackagedAudioRequest {
                    codec: AudioCodec::AacLc,
                    sample_rate: GAPLESS_SAMPLE_RATE,
                    channels: GAPLESS_CHANNELS,
                    start_frame: NonZeroU32::new(
                        u32::try_from(start_frame).expect("start_frame fits u32"),
                    ),
                    timescale: Some(GAPLESS_SAMPLE_RATE),
                    bit_rate: Some(128_000),
                    encoder_delay: NonZeroU32::new(AAC_GAPLESS_ENCODER_DELAY),
                    trailing_delay: NonZeroU32::new(AAC_GAPLESS_TRAILING_DELAY),
                    source,
                    variant_overrides: Vec::new(),
                }),
        )
        .await
        .expect("create generated AAC gapless fixture");

    let client = Client::new();
    let init = fetch_bytes(&client, created.init_url(0).to_string()).await;
    let segments = futures::future::join_all(
        (0..crate::gapless_common::AAC_GAPLESS_SEGMENTS)
            .map(|segment| fetch_bytes(&client, created.segment_url(0, segment).to_string())),
    )
    .await;

    let mut bytes = Vec::with_capacity(init.len() + segments.iter().map(Vec::len).sum::<usize>());
    bytes.extend_from_slice(&init);
    for segment in segments {
        bytes.extend_from_slice(&segment);
    }

    GaplessFixture {
        bytes,
        media_info: MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4))
            .with_sample_rate(GAPLESS_SAMPLE_RATE)
            .with_channels(GAPLESS_CHANNELS),
        expected_visible_frames: crate::gapless_common::generated_aac_elst_visible_frames(),
    }
}

fn create_decoder_from_media_info(
    fixture: &GaplessFixture,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    DecoderFactory::create_from_media_info(
        Cursor::new(fixture.bytes.clone()),
        &fixture.media_info,
        config,
    )
}

fn create_decoder_with_probe(
    bytes: Vec<u8>,
    hint: &'static str,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    DecoderFactory::create_with_probe(Cursor::new(bytes), Some(hint), config)
}

fn decode_visible_frames(mut decoder: Box<dyn InnerDecoder>) -> DecodeResult<DecodedFrames> {
    let mut trimmer = decoder
        .track_info()
        .gapless
        .map_or_else(GaplessTrimmer::disabled, GaplessTrimmer::from_info);
    let mut frames = 0usize;

    while let Some(chunk) = decoder.next_chunk()? {
        for chunk in trimmer.push(chunk) {
            frames = frames.saturating_add(chunk.frames());
        }
    }

    for chunk in trimmer.flush() {
        frames = frames.saturating_add(chunk.frames());
    }

    Ok(DecodedFrames { frames })
}

fn assert_frames_within(actual: usize, expected: usize, tolerance: usize, label: &str) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= tolerance,
        "{label}: expected {expected} frames +/-{tolerance}, got {actual} (delta {delta})"
    );
}

async fn fetch_bytes(client: &Client, url: String) -> Vec<u8> {
    client
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|error| panic!("fetch {url}: {error}"))
        .error_for_status()
        .unwrap_or_else(|error| panic!("fetch {url}: unexpected status: {error}"))
        .bytes()
        .await
        .unwrap_or_else(|error| panic!("read {url}: {error}"))
        .to_vec()
}

#[kithara::test(
    native,
    tokio,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
#[case::mp3(SignalFormat::Mp3, "mp3", 48_000)]
#[case::flac(SignalFormat::Flac, "flac", 48_000)]
async fn generated_encoded_signal_visible_frames_match_requested_signal_frames(
    #[case] format: SignalFormat,
    #[case] hint: &'static str,
    #[case] expected_frames: usize,
) {
    let server = TestServerHelper::new().await;
    let spec = SignalSpec {
        sample_rate: GAPLESS_SAMPLE_RATE,
        channels: GAPLESS_CHANNELS,
        length: SignalSpecLength::Frames(expected_frames),
        format,
    };

    let bytes = Client::new()
        .get(server.sine(&spec, 1_000.0).await)
        .send()
        .await
        .expect("fetch encoded signal")
        .error_for_status()
        .expect("encoded signal response status")
        .bytes()
        .await
        .expect("read encoded signal bytes")
        .to_vec();

    let default_pcm = decode_visible_frames(
        create_decoder_with_probe(bytes.clone(), hint, DecoderConfig::default())
            .expect("create default decoder"),
    )
    .expect("decode default path");
    let preferred_pcm = decode_visible_frames(
        create_decoder_with_probe(
            bytes,
            hint,
            DecoderConfig {
                prefer_hardware: true,
                ..DecoderConfig::default()
            },
        )
        .expect("create preferred decoder"),
    )
    .expect("decode preferred path");

    assert_frames_within(default_pcm.frames, expected_frames, 2, hint);
    assert_frames_within(preferred_pcm.frames, expected_frames, 2, hint);
    assert_frames_within(
        preferred_pcm.frames,
        default_pcm.frames,
        2,
        "preferred parity",
    );
}
