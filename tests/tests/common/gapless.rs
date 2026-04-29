use std::{io::Cursor, num::NonZeroU32};

use kithara::{
    decode::{DecodeResult, DecoderConfig, DecoderFactory, GaplessTrimmer, InnerDecoder},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper,
    fixture_protocol::{PackagedAudioRequest, PackagedAudioSource, PackagedSignal},
};
use reqwest::Client;

pub(crate) const GAPLESS_SAMPLE_RATE: u32 = 48_000;
pub(crate) const GAPLESS_CHANNELS: u16 = 2;
pub(crate) const AAC_GAPLESS_SEGMENTS: usize = 3;
pub(crate) const AAC_GAPLESS_SEGMENT_SECS: f64 = 0.5;
pub(crate) const AAC_GAPLESS_SEGMENT_FRAMES: usize = 24_000;
pub(crate) const AAC_FRAME_SAMPLES: usize = 1_024;
pub(crate) const AAC_GAPLESS_ENCODER_DELAY: u32 = 2_112;
pub(crate) const AAC_GAPLESS_TRAILING_DELAY: u32 = 960;

pub(crate) struct GaplessFixture {
    pub(crate) bytes: Vec<u8>,
    pub(crate) media_info: MediaInfo,
    pub(crate) expected_visible_frames: usize,
}

pub(crate) struct DecodedFrames {
    pub(crate) frames: usize,
}

pub(crate) fn generated_aac_elst_visible_frames() -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let encoder_delay = usize::try_from(AAC_GAPLESS_ENCODER_DELAY).expect("AAC delay fits usize");
    let trailing_delay =
        usize::try_from(AAC_GAPLESS_TRAILING_DELAY).expect("AAC padding fits usize");
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(encoder_delay)
        .saturating_sub(trailing_delay)
}

pub(crate) async fn generated_aac_elst_fixture(
    server: &TestServerHelper,
    signal: PackagedSignal,
    start_frame: u64,
) -> GaplessFixture {
    let source = match (signal, start_frame) {
        (PackagedSignal::Sine { freq_hz }, 0) => {
            PackagedAudioSource::Signal(PackagedSignal::Sine { freq_hz })
        }
        (PackagedSignal::Sine { freq_hz }, start_frame) => {
            PackagedAudioSource::Signal(PackagedSignal::SineOffset {
                freq_hz,
                start_frame,
            })
        }
        (signal, 0) => PackagedAudioSource::Signal(signal),
        (signal, _) => panic!("start_frame is only supported for sine fixtures, got {signal:?}"),
    };

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
        (0..AAC_GAPLESS_SEGMENTS)
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
        expected_visible_frames: generated_aac_elst_visible_frames(),
    }
}

pub(crate) fn create_decoder_from_media_info(
    fixture: &GaplessFixture,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    DecoderFactory::create_from_media_info(
        Cursor::new(fixture.bytes.clone()),
        &fixture.media_info,
        config,
    )
}

pub(crate) fn create_decoder_with_probe(
    bytes: Vec<u8>,
    hint: &'static str,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    DecoderFactory::create_with_probe(Cursor::new(bytes), Some(hint), config)
}

pub(crate) fn decode_visible_frames(
    mut decoder: Box<dyn InnerDecoder>,
) -> DecodeResult<DecodedFrames> {
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

pub(crate) fn assert_frames_within(actual: usize, expected: usize, tolerance: usize, label: &str) {
    let delta = actual.abs_diff(expected);
    assert!(
        delta <= tolerance,
        "{label}: expected {expected} frames ±{tolerance}, got {actual} (delta {delta})"
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
