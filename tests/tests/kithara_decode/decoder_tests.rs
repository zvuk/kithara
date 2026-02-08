#![forbid(unsafe_code)]

//! Tests for audio decoding with DecoderFactory.

use std::io::Cursor;

use fixture::EmbeddedAudio;
use kithara_decode::{DecoderConfig, DecoderFactory};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use rstest::{fixture, rstest};

use super::fixture;

fn test_config() -> DecoderConfig {
    DecoderConfig::default()
}

// Fixtures

#[fixture]
fn audio() -> EmbeddedAudio {
    EmbeddedAudio::get()
}

#[fixture]
fn wav_media_info() -> MediaInfo {
    MediaInfo {
        channels: None,
        codec: None,
        container: Some(ContainerFormat::Wav),
        sample_rate: None,
        variant_index: None,
    }
}

#[fixture]
fn mp3_media_info() -> MediaInfo {
    MediaInfo {
        channels: None,
        codec: Some(AudioCodec::Mp3),
        container: Some(ContainerFormat::MpegAudio),
        sample_rate: None,
        variant_index: None,
    }
}

// Basic Decode Tests

#[rstest]
fn decode_wav_with_probe(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);

    let chunk = decoder.next_chunk().unwrap();
    assert!(chunk.is_some());

    let chunk = chunk.unwrap();
    assert!(!chunk.pcm.is_empty());
}

#[rstest]
fn decode_mp3_with_probe(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.mp3());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("mp3"), test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);

    let chunk = decoder.next_chunk().unwrap();
    assert!(chunk.is_some());

    let chunk = chunk.unwrap();
    assert!(!chunk.pcm.is_empty());
}

#[rstest]
#[case::wav(true, "wav")]
#[case::mp3(false, "mp3")]
fn decode_complete(audio: EmbeddedAudio, #[case] use_wav: bool, #[case] ext: &str) {
    let data = if use_wav { audio.wav() } else { audio.mp3() };
    let reader = Cursor::new(data);

    let mut decoder = DecoderFactory::create_with_probe(reader, Some(ext), test_config()).unwrap();

    let mut total_samples = 0;
    let mut chunk_count = 0;

    while let Ok(Some(chunk)) = decoder.next_chunk() {
        total_samples += chunk.pcm.len();
        chunk_count += 1;
    }

    assert!(total_samples > 0, "Should decode some samples");
    assert!(chunk_count > 0, "Should have at least one chunk");
}

// MediaInfo Creation Tests

#[rstest]
fn from_media_info_wav(audio: EmbeddedAudio, wav_media_info: MediaInfo) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_from_media_info(reader, &wav_media_info, test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);

    let chunk = decoder.next_chunk().unwrap();
    assert!(chunk.is_some());
}

#[rstest]
fn from_media_info_mp3(audio: EmbeddedAudio, mp3_media_info: MediaInfo) {
    let reader = Cursor::new(audio.mp3());

    let decoder =
        DecoderFactory::create_from_media_info(reader, &mp3_media_info, test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate > 0);
    assert!(spec.channels > 0);
}

// Spec Tests

#[rstest]
fn spec_wav_properties(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let decoder = DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();
    let spec = decoder.spec();

    // Our test WAV is 44.1kHz stereo
    assert_eq!(spec.sample_rate, 44100);
    assert_eq!(spec.channels, 2);
}

#[rstest]
fn spec_mp3_properties(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.mp3());

    let decoder = DecoderFactory::create_with_probe(reader, Some("mp3"), test_config()).unwrap();
    let spec = decoder.spec();

    // MP3 should have standard sample rate
    assert!(
        spec.sample_rate == 44100 || spec.sample_rate == 48000 || spec.sample_rate == 22050,
        "Unexpected sample rate: {}",
        spec.sample_rate
    );
    assert!(spec.channels == 1 || spec.channels == 2);
}

// Error Handling Tests

#[rstest]
#[case::invalid(vec![0u8; 100])]
#[case::empty(vec![])]
fn invalid_data_fails(#[case] data: Vec<u8>) {
    let reader = Cursor::new(data);
    // Try to create as WAV - should fail with invalid data
    let result = DecoderFactory::create_with_probe(reader, Some("wav"), test_config());
    assert!(result.is_err());
}

#[rstest]
fn truncated_data_handles_gracefully(audio: EmbeddedAudio) {
    // Take only first 1000 bytes of WAV (truncated)
    let truncated: Vec<u8> = audio.wav().iter().take(1000).copied().collect();
    let reader = Cursor::new(truncated);

    // Should be able to create decoder (header is intact)
    let decoder_result = DecoderFactory::create_with_probe(reader, Some("wav"), test_config());

    // Either fails to create or decodes partial data
    if let Ok(mut decoder) = decoder_result {
        let _ = decoder.next_chunk();
    }
}

// Chunk Properties Tests

#[rstest]
fn chunk_has_valid_samples(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();
    let spec = decoder.spec();

    let chunk = decoder.next_chunk().unwrap().unwrap();

    // Samples should be f32 values in reasonable range
    for sample in &chunk.pcm {
        assert!(
            sample.is_finite(),
            "Sample should be finite, got: {}",
            sample
        );
        assert!(
            *sample >= -1.5 && *sample <= 1.5,
            "Sample out of range: {}",
            sample
        );
    }

    // Sample count should be multiple of channels
    assert_eq!(
        chunk.pcm.len() % spec.channels as usize,
        0,
        "Sample count should be multiple of channels"
    );
}

#[rstest]
fn multiple_chunks_consistent(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.mp3());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("mp3"), test_config()).unwrap();
    let spec = decoder.spec();

    let mut prev_len = None;
    let mut chunk_count = 0;

    while let Ok(Some(chunk)) = decoder.next_chunk() {
        chunk_count += 1;

        // Each chunk should have samples multiple of channels
        assert_eq!(chunk.pcm.len() % spec.channels as usize, 0);

        // Skip first few chunks - they can have different sizes during init
        if chunk_count > 3 {
            if let Some(prev) = prev_len {
                let ratio = chunk.pcm.len() as f64 / prev as f64;
                if ratio < 0.01 || ratio > 100.0 {
                    panic!(
                        "Chunk size varies extremely: {} vs {}",
                        chunk.pcm.len(),
                        prev
                    );
                }
            }
        }
        prev_len = Some(chunk.pcm.len());

        if chunk_count > 100 {
            break;
        }
    }
}

// Decode Consistency Tests

#[rstest]
fn consecutive_chunks_differ(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();

    let first_chunk = decoder.next_chunk().unwrap().unwrap();
    let second_chunk = decoder.next_chunk().unwrap().unwrap();

    // Chunks should be different (unless file is very short)
    if first_chunk.pcm.len() > 10 && second_chunk.pcm.len() > 10 {
        let differs = first_chunk
            .pcm
            .iter()
            .zip(second_chunk.pcm.iter())
            .any(|(a, b)| (a - b).abs() > f32::EPSILON);
        assert!(differs, "Consecutive chunks should have different samples");
    }
}
