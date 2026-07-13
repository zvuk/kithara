#![cfg(all(target_arch = "wasm32", feature = "webcodecs"))]

use std::{io::Cursor, sync::Once};

use js_sys::Uint8Array;
use kithara_decode::{
    Decoder, DecoderBackend, DecoderChunkOutcome, DecoderConfig, DecoderFactory,
    DecoderSeekOutcome, PcmSpec, duration_for_frames, spawn_webcodecs_probe,
};
use kithara_platform::time::{self, Duration};
use kithara_resampler::NoResamplerBackend;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::kithara;
use num_traits::ToPrimitive;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AudioDecoder, AudioDecoderConfig, AudioDecoderSupport};

const MP3: &[u8] = include_bytes!("../../../assets/test.mp3");
const FLAC: &[u8] = include_bytes!("../../../assets/sawtooth.flac");
const AAC_INIT: &[u8] = include_bytes!("../../../assets/hls/init-slq-a1.mp4");
const AAC_SEGMENT: &[u8] = include_bytes!("../../../assets/hls/segment-1-slq-a1.m4s");
const EXPECTED_CHANNELS: u16 = 2;
const EXPECTED_SAMPLE_RATE: u32 = 44_100;
const MP3_FRAME_TOLERANCE: usize = 2 * 1_152;
const AAC_FRAME_TOLERANCE: usize = 2 * 1_024;
const MAX_DECODE_OUTCOMES: usize = 100_000;
const SAW_PERIOD: usize = 65_536;
// Keep in sync with webcodecs/probe.rs.
const FLAC_PROBE_STREAMINFO: [u8; 34] = [
    0x10, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0xC4, 0x42, 0xF0, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00,
];

static PROBE_STARTED: Once = Once::new();

struct HeAacV1Fixture;

impl HeAacV1Fixture {
    const INIT: &'static [u8] = include_bytes!("../../../assets/he_aac_v1_init.mp4");
    const SEGMENT: &'static [u8] = include_bytes!("../../../assets/he_aac_v1_segment.m4s");
}

struct HeAacV2Fixture;

impl HeAacV2Fixture {
    const INIT: &'static [u8] = include_bytes!("../../../assets/he_aac_v2_init.mp4");
    const SEGMENT: &'static [u8] = include_bytes!("../../../assets/he_aac_v2_segment.m4s");
}

#[derive(Debug)]
struct DecodeSummary {
    eof: bool,
    frames: usize,
    non_empty_chunks: usize,
    saw_ascending: bool,
    spec: PcmSpec,
}

#[kithara::test(wasm, timeout(Duration::from_secs(300)))]
async fn mp3_parity() {
    prepare_webcodecs("mp3").await;

    let webcodecs = decode_file(MP3, "mp3", DecoderBackend::WebCodecs).await;
    let symphonia = decode_file(MP3, "mp3", DecoderBackend::Symphonia).await;

    assert_common_parity("mp3", &webcodecs, &symphonia, MP3_FRAME_TOLERANCE);
    tracing::info!(
        webcodecs_frames = webcodecs.frames,
        symphonia_frames = symphonia.frames,
        frame_delta = webcodecs.frames.abs_diff(symphonia.frames),
        "WebCodecs MP3 priming/tail frame-count probe"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(120)))]
async fn flac_parity_direction() {
    prepare_webcodecs("flac").await;

    let webcodecs = decode_file(FLAC, "flac", DecoderBackend::WebCodecs).await;
    let symphonia = decode_file(FLAC, "flac", DecoderBackend::Symphonia).await;

    assert_common_parity("flac", &webcodecs, &symphonia, MP3_FRAME_TOLERANCE);
    if webcodecs.frames != symphonia.frames {
        tracing::warn!(
            webcodecs_frames = webcodecs.frames,
            symphonia_frames = symphonia.frames,
            frame_delta = webcodecs.frames.abs_diff(symphonia.frames),
            "WebCodecs FLAC frame count differs from lossless reference"
        );
    }
    assert!(
        webcodecs.saw_ascending,
        "WebCodecs FLAC PCM must preserve the ascending sawtooth direction"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(120)))]
async fn seek_generation() {
    prepare_webcodecs("mp3").await;

    let mut decoder = create_file_decoder(MP3, "mp3", DecoderBackend::WebCodecs);
    let spec = decoder.spec();
    let mut warmup_chunks = 0usize;
    while warmup_chunks < 4 {
        match decoder.next_chunk().expect("decode MP3 warmup chunk") {
            DecoderChunkOutcome::Chunk(chunk) => warmup_chunks += usize::from(chunk.frames() > 0),
            DecoderChunkOutcome::Pending(_) => {}
            DecoderChunkOutcome::Eof => panic!("MP3 reached EOF during seek warmup"),
        }
        time::sleep(Duration::ZERO).await;
    }

    let duration = decoder.duration().expect("MP3 duration must be known");
    let target = Duration::from_secs_f64(duration.as_secs_f64() * 0.6);
    assert!(
        matches!(
            decoder.seek(target).expect("seek WebCodecs MP3"),
            DecoderSeekOutcome::Landed { .. }
        ),
        "60% MP3 seek must land before EOF"
    );

    let earliest = target.saturating_sub(duration_for_frames(spec.sample_rate.get(), 1));
    let mut post_seek_chunks = 0usize;
    for _ in 0..MAX_DECODE_OUTCOMES {
        match decoder.next_chunk().expect("decode post-seek MP3 chunk") {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(
                    chunk.meta.timestamp >= earliest,
                    "stale WebCodecs generation leaked timestamp {:?} before seek threshold {:?}",
                    chunk.meta.timestamp,
                    earliest
                );
                post_seek_chunks += 1;
                if post_seek_chunks == 16 {
                    break;
                }
            }
            DecoderChunkOutcome::Pending(_) => {}
            DecoderChunkOutcome::Eof => break,
        }
        time::sleep(Duration::ZERO).await;
    }
    assert!(
        post_seek_chunks > 0,
        "WebCodecs MP3 must produce PCM after seek"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(300)))]
async fn seek_trim_no_preroll_leak() {
    prepare_webcodecs("mp3").await;

    let mut decoder = create_file_decoder(MP3, "mp3", DecoderBackend::WebCodecs);
    let duration = decoder.duration().expect("MP3 duration must be known");
    let sample_rate = decoder.spec().sample_rate.get();

    for ratio in [0.17, 0.37, 0.73] {
        let target = mid_packet_target(duration, sample_rate, ratio);
        assert!(
            matches!(
                decoder.seek(target).expect("seek WebCodecs MP3"),
                DecoderSeekOutcome::Landed { .. }
            ),
            "MP3 seek at ratio {ratio} must land before EOF"
        );

        let mut chunks = 0usize;
        let mut decoded_frames = 0usize;
        for _ in 0..MAX_DECODE_OUTCOMES {
            match decoder.next_chunk().expect("decode post-seek MP3 chunk") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    if chunks == 0 {
                        assert!(
                            chunk.meta.timestamp >= target,
                            "post-seek PCM timestamp {:?} precedes target {:?} at ratio {ratio}",
                            chunk.meta.timestamp,
                            target
                        );
                    }
                    assert!(!chunk.samples.is_empty(), "post-seek PCM must be non-empty");
                    assert!(
                        chunk.samples.iter().all(|sample| sample.is_finite()),
                        "post-seek PCM must be finite"
                    );
                    decoded_frames = decoded_frames.saturating_add(chunk.frames());
                    chunks += 1;
                    if chunks == 4 {
                        break;
                    }
                }
                DecoderChunkOutcome::Pending(_) => {}
                DecoderChunkOutcome::Eof => break,
            }
            time::sleep(Duration::ZERO).await;
        }
        assert_eq!(chunks, 4, "WebCodecs MP3 must continue after seek");
        assert!(decoded_frames > 0, "post-seek PCM must contain frames");
    }
}

#[kithara::test(wasm, timeout(Duration::from_secs(300)))]
async fn eof_tail_drain() {
    prepare_webcodecs("mp3").await;

    let webcodecs = decode_file(MP3, "mp3", DecoderBackend::WebCodecs).await;
    let symphonia = decode_file(MP3, "mp3", DecoderBackend::Symphonia).await;

    assert!(webcodecs.eof, "WebCodecs MP3 must reach explicit EOF");
    assert!(
        webcodecs.frames >= symphonia.frames.saturating_sub(MP3_FRAME_TOLERANCE),
        "WebCodecs EOF drain lost tail frames: webcodecs={}, symphonia={}, tolerance={MP3_FRAME_TOLERANCE}",
        webcodecs.frames,
        symphonia.frames
    );
    tracing::info!(
        webcodecs_frames = webcodecs.frames,
        symphonia_frames = symphonia.frames,
        frame_delta = webcodecs.frames.abs_diff(symphonia.frames),
        "WebCodecs MP3 EOF tail-drain frame-count probe"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(120)))]
async fn aac_parity() {
    prepare_webcodecs("mp4a.40.2").await;

    let mut bytes = Vec::with_capacity(AAC_INIT.len() + AAC_SEGMENT.len());
    bytes.extend_from_slice(AAC_INIT);
    bytes.extend_from_slice(AAC_SEGMENT);
    let webcodecs = decode_aac(&bytes, DecoderBackend::WebCodecs).await;
    let symphonia = decode_aac(&bytes, DecoderBackend::Symphonia).await;

    assert_common_parity("aac-lc", &webcodecs, &symphonia, AAC_FRAME_TOLERANCE);
    tracing::info!(
        webcodecs_frames = webcodecs.frames,
        symphonia_frames = symphonia.frames,
        frame_delta = webcodecs.frames.abs_diff(symphonia.frames),
        "WebCodecs AAC-LC priming/tail frame-count probe"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(120)))]
async fn he_aac_v1_decode() {
    prepare_webcodecs("mp4a.40.5").await;

    let mut bytes = Vec::with_capacity(HeAacV1Fixture::INIT.len() + HeAacV1Fixture::SEGMENT.len());
    bytes.extend_from_slice(HeAacV1Fixture::INIT);
    bytes.extend_from_slice(HeAacV1Fixture::SEGMENT);
    let decoded = decode_he_aac_v1(&bytes).await;

    assert!(decoded.eof, "WebCodecs HE-AAC v1 must reach explicit EOF");
    assert!(
        decoded.non_empty_chunks > 0,
        "WebCodecs HE-AAC v1 PCM must be non-empty"
    );
    assert!(
        decoded.frames > 1_024,
        "WebCodecs HE-AAC v1 decoded too few frames: {}",
        decoded.frames
    );
    assert_eq!(decoded.spec.channels, EXPECTED_CHANNELS);
    assert_eq!(decoded.spec.sample_rate.get(), EXPECTED_SAMPLE_RATE);
    tracing::info!(
        frames = decoded.frames,
        channels = decoded.spec.channels,
        sample_rate = decoded.spec.sample_rate.get(),
        "WebCodecs HE-AAC v1 resolved output spec"
    );
}

#[kithara::test(wasm, timeout(Duration::from_secs(120)))]
async fn he_aac_v2_decode() {
    prepare_webcodecs("mp4a.40.29").await;

    let mut bytes = Vec::with_capacity(HeAacV2Fixture::INIT.len() + HeAacV2Fixture::SEGMENT.len());
    bytes.extend_from_slice(HeAacV2Fixture::INIT);
    bytes.extend_from_slice(HeAacV2Fixture::SEGMENT);
    let decoded = decode_he_aac_v2(&bytes).await;

    assert!(decoded.eof, "WebCodecs HE-AAC v2 must reach explicit EOF");
    assert!(
        decoded.non_empty_chunks > 0,
        "WebCodecs HE-AAC v2 PCM must be non-empty"
    );
    assert!(
        decoded.frames > 1_024,
        "WebCodecs HE-AAC v2 decoded too few frames: {}",
        decoded.frames
    );
    assert_eq!(decoded.spec.channels, EXPECTED_CHANNELS);
    assert_eq!(decoded.spec.sample_rate.get(), EXPECTED_SAMPLE_RATE);
    tracing::info!(
        frames = decoded.frames,
        channels = decoded.spec.channels,
        sample_rate = decoded.spec.sample_rate.get(),
        "WebCodecs HE-AAC v2 resolved output spec"
    );
}

async fn prepare_webcodecs(codec: &str) {
    PROBE_STARTED.call_once(spawn_webcodecs_probe);
    assert_browser_support(codec).await;
    for _ in 0..100 {
        if webcodecs_runtime_ready(codec) {
            return;
        }
        time::sleep(Duration::from_millis(50)).await;
    }
    panic!("WebCodecs capability probe did not publish within 5 seconds");
}

fn webcodecs_runtime_ready(codec: &str) -> bool {
    match codec {
        "mp3" => DecoderFactory::create_with_probe(
            Cursor::new(MP3.to_vec()),
            Some("mp3"),
            decoder_config(DecoderBackend::WebCodecs),
        )
        .is_ok(),
        "flac" => DecoderFactory::create_with_probe(
            Cursor::new(FLAC.to_vec()),
            Some("flac"),
            decoder_config(DecoderBackend::WebCodecs),
        )
        .is_ok(),
        "mp4a.40.2" => {
            let mut bytes = Vec::with_capacity(AAC_INIT.len() + AAC_SEGMENT.len());
            bytes.extend_from_slice(AAC_INIT);
            bytes.extend_from_slice(AAC_SEGMENT);
            DecoderFactory::create_from_media_info(
                Cursor::new(bytes),
                &MediaInfo::builder()
                    .codec(AudioCodec::AacLc)
                    .container(ContainerFormat::Fmp4)
                    .sample_rate(EXPECTED_SAMPLE_RATE)
                    .channels(EXPECTED_CHANNELS)
                    .build(),
                decoder_config(DecoderBackend::WebCodecs),
            )
            .is_ok()
        }
        "mp4a.40.5" => {
            let mut bytes =
                Vec::with_capacity(HeAacV1Fixture::INIT.len() + HeAacV1Fixture::SEGMENT.len());
            bytes.extend_from_slice(HeAacV1Fixture::INIT);
            bytes.extend_from_slice(HeAacV1Fixture::SEGMENT);
            DecoderFactory::create_from_media_info(
                Cursor::new(bytes),
                &aac_media_info(AudioCodec::AacHe),
                decoder_config(DecoderBackend::WebCodecs),
            )
            .is_ok()
        }
        "mp4a.40.29" => {
            let mut bytes =
                Vec::with_capacity(HeAacV2Fixture::INIT.len() + HeAacV2Fixture::SEGMENT.len());
            bytes.extend_from_slice(HeAacV2Fixture::INIT);
            bytes.extend_from_slice(HeAacV2Fixture::SEGMENT);
            DecoderFactory::create_from_media_info(
                Cursor::new(bytes),
                &aac_media_info(AudioCodec::AacHeV2),
                decoder_config(DecoderBackend::WebCodecs),
            )
            .is_ok()
        }
        _ => false,
    }
}

async fn assert_browser_support(codec: &str) {
    let config = AudioDecoderConfig::new(codec, u32::from(EXPECTED_CHANNELS), EXPECTED_SAMPLE_RATE);
    if codec == "flac" {
        let mut description = Vec::with_capacity(4 + 4 + FLAC_PROBE_STREAMINFO.len());
        description.extend_from_slice(b"fLaC");
        description.extend_from_slice(&[0x80, 0, 0, 34]);
        description.extend_from_slice(&FLAC_PROBE_STREAMINFO);
        config.set_description(&Uint8Array::from(description.as_slice()));
    }
    // Resolves to a dictionary (no prototype): unchecked cast, not dyn_into.
    let support = JsFuture::from(AudioDecoder::is_config_supported(&config))
        .await
        .ok()
        .map(JsCast::unchecked_into::<AudioDecoderSupport>)
        .and_then(|support| support.get_supported())
        .unwrap_or(false);
    assert!(support, "WebCodecs unsupported in test browser: {codec}");
}

fn decoder_config(backend: DecoderBackend) -> DecoderConfig<NoResamplerBackend> {
    DecoderConfig::<NoResamplerBackend>::builder()
        .backend(backend)
        .build()
}

fn mid_packet_target(duration: Duration, sample_rate: u32, ratio: f64) -> Duration {
    const PACKET_FRAMES: u64 = 1_152;

    let scaled_frames = duration.as_secs_f64() * f64::from(sample_rate) * ratio;
    let mut target_frame = scaled_frames.round().to_u64().unwrap_or(1);
    if target_frame.is_multiple_of(PACKET_FRAMES) {
        target_frame = target_frame.saturating_add(1);
    }
    duration_for_frames(sample_rate, target_frame)
}

fn create_file_decoder(bytes: &[u8], hint: &str, backend: DecoderBackend) -> Box<dyn Decoder> {
    DecoderFactory::create_with_probe(
        Cursor::new(bytes.to_vec()),
        Some(hint),
        decoder_config(backend),
    )
    .unwrap_or_else(|error| panic!("create {backend} decoder for {hint}: {error}"))
}

fn aac_media_info(codec: AudioCodec) -> MediaInfo {
    MediaInfo::builder()
        .codec(codec)
        .container(ContainerFormat::Fmp4)
        .sample_rate(EXPECTED_SAMPLE_RATE)
        .channels(EXPECTED_CHANNELS)
        .build()
}

fn create_aac_decoder(
    bytes: &[u8],
    codec: AudioCodec,
    backend: DecoderBackend,
) -> Box<dyn Decoder> {
    DecoderFactory::create_from_media_info(
        Cursor::new(bytes.to_vec()),
        &aac_media_info(codec),
        decoder_config(backend),
    )
    .unwrap_or_else(|error| panic!("create {backend} decoder for {codec:?} fMP4: {error}"))
}

async fn decode_file(bytes: &[u8], hint: &str, backend: DecoderBackend) -> DecodeSummary {
    decode_to_eof(create_file_decoder(bytes, hint, backend), backend, hint).await
}

async fn decode_aac(bytes: &[u8], backend: DecoderBackend) -> DecodeSummary {
    decode_to_eof(
        create_aac_decoder(bytes, AudioCodec::AacLc, backend),
        backend,
        "aac-lc",
    )
    .await
}

async fn decode_he_aac_v1(bytes: &[u8]) -> DecodeSummary {
    decode_he_aac(bytes, AudioCodec::AacHe, "HE-AAC v1").await
}

async fn decode_he_aac_v2(bytes: &[u8]) -> DecodeSummary {
    decode_he_aac(bytes, AudioCodec::AacHeV2, "HE-AAC v2").await
}

async fn decode_he_aac(bytes: &[u8], codec: AudioCodec, fixture: &str) -> DecodeSummary {
    let mut decoder = create_aac_decoder(bytes, codec, DecoderBackend::WebCodecs);
    let mut frames = 0usize;
    let mut non_empty_chunks = 0usize;
    let mut eof = false;

    for _ in 0..MAX_DECODE_OUTCOMES {
        match decoder
            .next_chunk()
            .unwrap_or_else(|error| panic!("decode {fixture} with WebCodecs: {error}"))
        {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(
                    !chunk.samples.is_empty(),
                    "WebCodecs emitted empty {fixture} PCM"
                );
                // Lossy SBR reconstruction can legitimately overshoot [-1, 1],
                // while larger or non-finite values indicate corrupt output.
                assert!(
                    chunk
                        .samples
                        .iter()
                        .all(|sample| sample.is_finite() && sample.abs() < 2.0),
                    "WebCodecs emitted non-finite or wildly out-of-range {fixture} PCM"
                );
                frames += chunk.frames();
                non_empty_chunks += 1;
            }
            DecoderChunkOutcome::Pending(_) => {}
            DecoderChunkOutcome::Eof => {
                eof = true;
                break;
            }
        }
        time::sleep(Duration::ZERO).await;
    }

    DecodeSummary {
        eof,
        frames,
        non_empty_chunks,
        saw_ascending: false,
        spec: decoder.spec(),
    }
}

async fn decode_to_eof(
    mut decoder: Box<dyn Decoder>,
    backend: DecoderBackend,
    fixture: &str,
) -> DecodeSummary {
    let spec = decoder.spec();
    assert_eq!(spec.sample_rate.get(), EXPECTED_SAMPLE_RATE);
    assert_eq!(spec.channels, EXPECTED_CHANNELS);

    let mut frames = 0usize;
    let mut non_empty_chunks = 0usize;
    let mut saw_ascending = false;
    let mut eof = false;
    for _ in 0..MAX_DECODE_OUTCOMES {
        match decoder
            .next_chunk()
            .unwrap_or_else(|error| panic!("decode {fixture} with {backend}: {error}"))
        {
            DecoderChunkOutcome::Chunk(chunk) => {
                assert!(!chunk.samples.is_empty(), "{backend} emitted empty PCM");
                assert!(
                    chunk.samples.iter().all(|sample| sample.is_finite()),
                    "{backend} emitted non-finite PCM"
                );
                assert_eq!(chunk.spec(), spec);
                frames += chunk.frames();
                non_empty_chunks += 1;
                saw_ascending |= detect_ascending(&chunk.samples, usize::from(spec.channels));
            }
            DecoderChunkOutcome::Pending(_) => {}
            DecoderChunkOutcome::Eof => {
                eof = true;
                break;
            }
        }
        time::sleep(Duration::ZERO).await;
    }

    DecodeSummary {
        eof,
        frames,
        non_empty_chunks,
        saw_ascending,
        spec,
    }
}

fn assert_common_parity(
    fixture: &str,
    webcodecs: &DecodeSummary,
    symphonia: &DecodeSummary,
    frame_tolerance: usize,
) {
    assert!(webcodecs.eof, "WebCodecs {fixture} must reach EOF");
    assert!(symphonia.eof, "Symphonia {fixture} must reach EOF");
    assert_eq!(webcodecs.spec, symphonia.spec);
    assert!(
        webcodecs.non_empty_chunks > 0,
        "WebCodecs {fixture} PCM must be non-empty"
    );
    assert!(
        symphonia.non_empty_chunks > 0,
        "Symphonia {fixture} PCM must be non-empty"
    );
    assert!(
        webcodecs.frames.abs_diff(symphonia.frames) <= frame_tolerance,
        "{fixture} frame-count delta exceeds tolerance: webcodecs={}, symphonia={}, tolerance={frame_tolerance}",
        webcodecs.frames,
        symphonia.frames
    );
}

fn detect_ascending(samples: &[f32], channels: usize) -> bool {
    if channels == 0 {
        return false;
    }
    let frames = samples.len() / channels;
    if frames < 2 {
        return false;
    }

    let mut ascending_votes = 0u32;
    let mut descending_votes = 0u32;
    for frame in 0..10.min(frames - 1) {
        let current = phase_from_f32(samples[frame * channels]);
        let next = phase_from_f32(samples[(frame + 1) * channels]);
        if next == (current + 1) % SAW_PERIOD {
            ascending_votes += 1;
        } else if next == (current + SAW_PERIOD - 1) % SAW_PERIOD {
            descending_votes += 1;
        }
    }
    ascending_votes > descending_votes && ascending_votes > 0
}

fn phase_from_f32(sample: f32) -> usize {
    let value = (sample * 32_768.0).round().to_i32().unwrap_or_default();
    usize::try_from((value + 32_768) & 0xffff).unwrap_or_default()
}
