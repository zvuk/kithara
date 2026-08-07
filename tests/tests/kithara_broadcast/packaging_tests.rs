use std::io::Cursor;

use kithara::{
    self,
    decode::{DecoderChunkOutcome, DecoderConfig, DecoderFactory},
    platform::time::Duration,
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_broadcast::{LiveWindow, PlaylistSnapshot, Segmenter};
use kithara_encode::StreamEncoder;
use kithara_integration_tests::{
    goertzel::goertzel_magnitude,
    signal_pcm::signal::{SignalFn, SineWave},
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const TONE_HZ: f64 = 440.0;
const BIT_RATE: u64 = 128_000;
const FRAMES: usize = 2 * SAMPLE_RATE as usize;
const PUSH_FRAMES: usize = 1_500;
const TARGET: Duration = Duration::from_millis(500);
const PRIMING_SKIP_FRAMES: usize = 4_800;
const SEGMENT_PRIMING_SKIP_FRAMES: usize = 2_048;
const TONE_MARGIN: f64 = 50.0;

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

/// Run interleaved f32 through the whole packaging chain and stop the stream.
fn broadcast(samples: &[f32]) -> PlaylistSnapshot {
    let mut encoder = StreamEncoder::new(SAMPLE_RATE, CHANNELS, BIT_RATE, SAMPLE_RATE)
        .expect("open the streaming AAC-LC encoder");
    let mut segmenter =
        Segmenter::new(SAMPLE_RATE, CHANNELS, SAMPLE_RATE, TARGET).expect("open the segmenter");
    let mut window = LiveWindow::new(LiveWindow::WINDOW, LiveWindow::GRACE, SAMPLE_RATE)
        .expect("open the window");

    let mut units = Vec::new();
    for chunk in samples.chunks(PUSH_FRAMES * usize::from(CHANNELS)) {
        units.extend(encoder.push(chunk).expect("push interleaved f32"));
    }
    units.extend(encoder.finish().expect("finish the stream"));

    for unit in &units {
        if let Some(segment) = segmenter.push(unit).expect("package the access unit") {
            window.push(segment);
        }
    }
    if let Some(segment) = segmenter.flush() {
        window.push(segment);
    }
    window.finish();

    window.snapshot()
}

fn decode_left_channel(bytes: Vec<u8>) -> Vec<f32> {
    let mut decoder = DecoderFactory::create_from_media_info(
        Cursor::new(bytes),
        &MediaInfo::builder()
            .codec(AudioCodec::AacLc)
            .container(ContainerFormat::Adts)
            .build(),
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .build(),
    )
    .expect("create the ADTS AAC-LC decoder");

    let mut left = Vec::new();
    while let DecoderChunkOutcome::Chunk(chunk) = decoder.next_chunk().expect("decode chunk") {
        let channels = usize::from(chunk.spec().channels);
        left.extend(chunk.samples.iter().step_by(channels));
    }
    left
}

fn assert_carries_the_tone(pcm: &[f32], label: &str) {
    let tone = goertzel_magnitude(pcm, TONE_HZ, SAMPLE_RATE);
    let off_tone = goertzel_magnitude(pcm, TONE_HZ * 3.0, SAMPLE_RATE);

    assert!(
        tone > off_tone * TONE_MARGIN,
        "{label}: expected a {TONE_HZ} Hz tone over {} frames: |tone| = {tone:.1}, \
         |off tone| = {off_tone:.1}",
        pcm.len()
    );
}

#[kithara::test]
fn the_packaged_segments_decode_back_to_the_source_tone() {
    let snapshot = broadcast(&sine(FRAMES));

    assert!(
        snapshot.segments.len() >= 3,
        "expected several segments, packaged {}",
        snapshot.segments.len()
    );
    assert!(snapshot.finished);
    assert!(snapshot.playlist.contains("#EXT-X-ENDLIST"));

    let mut stream = Vec::new();
    for segment in snapshot.segments.iter() {
        stream.extend_from_slice(&segment.bytes);
    }

    let decoded = decode_left_channel(stream);
    let priming_slack = 2 * StreamEncoder::FRAME_SAMPLES;
    assert!(
        decoded.len() >= FRAMES - priming_slack && decoded.len() <= FRAMES + priming_slack,
        "decoded {} frames, expected {FRAMES} within {priming_slack} frames of priming",
        decoded.len()
    );

    assert_carries_the_tone(&decoded[PRIMING_SKIP_FRAMES..], "concatenated stream");
}

#[kithara::test]
fn a_late_joiner_decodes_one_segment_on_its_own() {
    let snapshot = broadcast(&sine(FRAMES));

    let joined = snapshot
        .segments
        .iter()
        .rev()
        .nth(1)
        .expect("a full segment before the tail");
    let decoded = decode_left_channel(joined.bytes.to_vec());

    assert!(
        decoded.len() > SEGMENT_PRIMING_SKIP_FRAMES,
        "segment {} decoded to {} frames",
        joined.seq,
        decoded.len()
    );
    assert_carries_the_tone(
        &decoded[SEGMENT_PRIMING_SKIP_FRAMES..],
        &format!("segment {}", joined.seq),
    );
}
