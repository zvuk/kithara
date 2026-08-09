use std::io::Cursor;

use kithara::{
    self,
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_encode::{EncodedTrack, StreamBackend, StreamEncoder};
use kithara_integration_tests::{
    encode_ext::mux_fmp4_bytes,
    fixture_protocol::GaplessEncoding,
    goertzel::goertzel_magnitude,
    signal_pcm::signal::{SignalFn, SineWave},
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const TONE_HZ: f64 = 440.0;
const FRAMES: usize = 48_000;
const PUSH_FRAMES: usize = 1_500;
const BIT_RATE: u64 = 128_000;
const PRIMING_SKIP_FRAMES: usize = 4_800;

fn sine(frames: usize) -> Vec<f32> {
    let tone = SineWave(TONE_HZ);
    let mut samples = Vec::with_capacity(frames * usize::from(CHANNELS));
    for frame in 0..frames {
        let value = f32::from(tone.sample(frame, SAMPLE_RATE)) / 32_768.0;
        for _ in 0..CHANNELS {
            samples.push(value);
        }
    }
    samples
}

fn encode_stream(samples: &[f32]) -> EncodedTrack {
    let mut encoder = StreamEncoder::new(
        StreamBackend::Ffmpeg,
        SAMPLE_RATE,
        CHANNELS,
        BIT_RATE,
        SAMPLE_RATE,
    )
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
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
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
fn pushed_f32_sine_survives_encode_mux_and_decode() {
    let track = encode_stream(&sine(FRAMES));
    assert!(
        track.access_units.len() >= FRAMES / StreamEncoder::FRAME_SAMPLES,
        "streamed track is short: {} access units",
        track.access_units.len()
    );

    let decoded = decode_left_channel(mux_fmp4_bytes(&track, GaplessEncoding::None));

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
