use std::io::Cursor;

use kithara::{
    self,
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    encode::{EncodedTrack, StreamBackend, StreamEncoder},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::bufpool_ext::{TestPools, pools};
use kithara_test_fixtures::{
    fmp4::{GaplessEncoding, mux_audio_track},
    integration_fixtures::stream_sine,
    signal::goertzel_magnitude,
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const TONE_HZ: f64 = 440.0;
const FRAMES: usize = 48_000;
const PUSH_FRAMES: usize = 1_500;
const BIT_RATE: u64 = 128_000;
const PRIMING_SKIP_FRAMES: usize = 4_800;

fn encode_stream(samples: &[f32]) -> EncodedTrack {
    let mut encoder = StreamEncoder::builder()
        .backend(StreamBackend::Ffmpeg)
        .sample_rate(SAMPLE_RATE)
        .channels(CHANNELS)
        .bit_rate(BIT_RATE)
        .timescale(SAMPLE_RATE)
        .build()
        .expect("open the streaming AAC-LC encoder");

    let mut access_units = Vec::new();
    for chunk in samples.chunks(PUSH_FRAMES * usize::from(CHANNELS)) {
        access_units.extend(encoder.push(chunk).expect("push interleaved f32"));
    }
    access_units.extend(encoder.finish().expect("finish the stream"));

    EncodedTrack {
        media_info: MediaInfo::builder()
            .codec(AudioCodec::AacLc)
            .container(ContainerFormat::Fmp4)
            .sample_rate(SAMPLE_RATE)
            .channels(CHANNELS)
            .build(),
        access_units,
        codec_config: Vec::new(),
        encoder_delay: 0,
        timescale: SAMPLE_RATE,
        trailing_delay: 0,
        bit_rate: BIT_RATE,
        packets_per_segment: 43,
    }
}

fn decode_left_channel(bytes: Vec<u8>) -> Vec<f32> {
    let mut decoder = DecoderFactory::create_from_media_info(
        Cursor::new(bytes),
        &MediaInfo::builder()
            .codec(AudioCodec::AacLc)
            .container(ContainerFormat::Fmp4)
            .build(),
        DecoderConfig::<kithara::resampler::NoResamplerBackend, TestPools>::builder()
            .pools(pools())
            .build(),
    )
    .expect("create the fMP4 AAC-LC decoder");

    let mut left = Vec::new();
    while let DecoderChunkOutcome::Chunk(chunk) = decoder.next_chunk().expect("decode chunk") {
        let channels = usize::from(chunk.spec().channels);
        left.extend(chunk.samples.iter().step_by(channels));
    }
    left
}

#[kithara::test]
fn pushed_f32_sine_survives_encode_mux_and_decode(stream_sine: Vec<f32>) {
    let track = encode_stream(&stream_sine);
    assert!(
        track.access_units.len() >= FRAMES / StreamEncoder::FRAME_SAMPLES,
        "streamed track is short: {} access units",
        track.access_units.len()
    );

    let decoded = decode_left_channel(Vec::from(
        mux_audio_track(&track, GaplessEncoding::None).expect("mux packaged track into fMP4"),
    ));

    let priming_slack = 2 * StreamEncoder::FRAME_SAMPLES;
    assert!(
        decoded.len() >= FRAMES - priming_slack && decoded.len() <= FRAMES + priming_slack,
        "decoded {} frames, expected {FRAMES} within {priming_slack} frames of priming",
        decoded.len()
    );

    let body = &decoded[PRIMING_SKIP_FRAMES..];
    let tone = goertzel_magnitude(body, TONE_HZ, SAMPLE_RATE);
    let off_tone = goertzel_magnitude(body, TONE_HZ * 3.0, SAMPLE_RATE);

    assert!(
        tone > off_tone * 50.0,
        "expected a {TONE_HZ} Hz tone: |tone| = {tone:.1}, |off tone| = {off_tone:.1}"
    );
}
