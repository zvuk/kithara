#![forbid(unsafe_code)]

use std::io::Cursor;

use kithara::{
    decode::{DecoderConfig, DecoderFactory, PcmChunk},
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
use kithara_integration_tests::{
    audio_fixture::EmbeddedAudio, decode_ext::DecoderChunkOutcomeTestExt,
};

fn test_config() -> DecoderConfig {
    DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .build()
}

#[kithara::fixture]
fn audio() -> EmbeddedAudio {
    EmbeddedAudio::get()
}

#[kithara::fixture]
fn wav_media_info() -> MediaInfo {
    MediaInfo::builder().container(ContainerFormat::Wav).build()
}

#[kithara::fixture]
fn mp3_media_info() -> MediaInfo {
    MediaInfo::builder()
        .codec(AudioCodec::Mp3)
        .container(ContainerFormat::MpegAudio)
        .build()
}

#[kithara::test]
#[case::wav(true, "wav")]
#[case::mp3(false, "mp3")]
fn decode_with_probe(audio: EmbeddedAudio, #[case] use_wav: bool, #[case] ext: &str) {
    let data = if use_wav { audio.wav() } else { audio.mp3() };
    let reader = Cursor::new(data);

    let mut decoder = DecoderFactory::create_with_probe(reader, Some(ext), test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate.get() > 0);
    assert!(spec.channels > 0);

    let outcome = decoder.next_chunk().unwrap();
    assert!(outcome.is_chunk());

    let chunk = PcmChunk::try_from(outcome).unwrap();
    assert!(!chunk.samples.is_empty());
}

#[kithara::test]
#[case::wav(true, "wav")]
#[case::mp3(false, "mp3")]
fn decode_complete(audio: EmbeddedAudio, #[case] use_wav: bool, #[case] ext: &str) {
    let data = if use_wav { audio.wav() } else { audio.mp3() };
    let reader = Cursor::new(data);

    let mut decoder = DecoderFactory::create_with_probe(reader, Some(ext), test_config()).unwrap();

    let mut total_samples = 0;
    let mut chunk_count = 0;

    while let Ok(kithara::decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk() {
        total_samples += chunk.samples.len();
        chunk_count += 1;
    }

    assert!(total_samples > 0, "Should decode some samples");
    assert!(chunk_count > 0, "Should have at least one chunk");
}

#[kithara::test]
#[case::wav(true)]
#[case::mp3(false)]
fn from_media_info(
    audio: EmbeddedAudio,
    wav_media_info: MediaInfo,
    mp3_media_info: MediaInfo,
    #[case] use_wav: bool,
) {
    let (data, info) = if use_wav {
        (audio.wav(), &wav_media_info)
    } else {
        (audio.mp3(), &mp3_media_info)
    };
    let reader = Cursor::new(data);

    let mut decoder = DecoderFactory::create_from_media_info(reader, info, test_config()).unwrap();
    let spec = decoder.spec();

    assert!(spec.sample_rate.get() > 0);
    assert!(spec.channels > 0);

    let outcome = decoder.next_chunk().unwrap();
    assert!(outcome.is_chunk());
}

/// Acceptable spec for a probed `test.{wav,mp3}` fixture.
fn assert_spec(spec: &kithara::decode::PcmSpec, ext: &str) {
    match ext {
        "wav" => {
            assert_eq!(spec.sample_rate.get(), 44100);
            assert_eq!(spec.channels, 2);
        }
        "mp3" => {
            assert!(
                [44100, 48000, 22050].contains(&spec.sample_rate.get()),
                "Unexpected MP3 sample rate: {}",
                spec.sample_rate
            );
            assert!(spec.channels == 1 || spec.channels == 2);
        }
        other => panic!("unhandled format: {other}"),
    }
}

#[kithara::test]
#[case::wav("wav")]
#[case::mp3("mp3")]
fn spec_properties(audio: EmbeddedAudio, #[case] ext: &str) {
    let data = if ext == "wav" {
        audio.wav()
    } else {
        audio.mp3()
    };
    let reader = Cursor::new(data);

    let decoder = DecoderFactory::create_with_probe(reader, Some(ext), test_config()).unwrap();
    assert_spec(&decoder.spec(), ext);
}

#[kithara::test]
#[case::invalid(vec![0u8; 100])]
#[case::empty(vec![])]
fn invalid_data_fails(#[case] data: Vec<u8>) {
    let reader = Cursor::new(data);
    let result = DecoderFactory::create_with_probe(reader, Some("wav"), test_config());
    assert!(result.is_err());
}

#[kithara::test]
fn truncated_data_handles_gracefully(audio: EmbeddedAudio) {
    let truncated: Vec<u8> = audio.wav().iter().take(1000).copied().collect();
    let reader = Cursor::new(truncated);

    let decoder_result = DecoderFactory::create_with_probe(reader, Some("wav"), test_config());

    if let Ok(mut decoder) = decoder_result {
        let _ = decoder.next_chunk();
    }
}

#[kithara::test]
fn chunk_has_valid_samples(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();
    let spec = decoder.spec();

    let chunk = PcmChunk::try_from(decoder.next_chunk().unwrap()).unwrap();

    for sample in chunk.samples.iter() {
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

    assert_eq!(
        chunk.samples.len() % spec.channels as usize,
        0,
        "Sample count should be multiple of channels"
    );
}

#[kithara::test]
fn multiple_chunks_consistent(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.mp3());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("mp3"), test_config()).unwrap();
    let spec = decoder.spec();

    let mut prev_len = None;
    let mut chunk_count = 0;

    while let Ok(kithara::decode::DecoderChunkOutcome::Chunk(chunk)) = decoder.next_chunk() {
        chunk_count += 1;

        assert_eq!(chunk.samples.len() % spec.channels as usize, 0);

        if chunk_count > 3
            && let Some(prev) = prev_len
        {
            let ratio = chunk.samples.len() as f64 / prev as f64;
            if !(0.01..=100.0).contains(&ratio) {
                panic!(
                    "Chunk size varies extremely: {} vs {}",
                    chunk.samples.len(),
                    prev
                );
            }
        }
        prev_len = Some(chunk.samples.len());

        if chunk_count > 100 {
            break;
        }
    }
}

#[kithara::test]
fn consecutive_chunks_differ(audio: EmbeddedAudio) {
    let reader = Cursor::new(audio.wav());

    let mut decoder =
        DecoderFactory::create_with_probe(reader, Some("wav"), test_config()).unwrap();

    let first_chunk = PcmChunk::try_from(decoder.next_chunk().unwrap()).unwrap();
    let second_chunk = PcmChunk::try_from(decoder.next_chunk().unwrap()).unwrap();

    if first_chunk.samples.len() > 10 && second_chunk.samples.len() > 10 {
        let differs = first_chunk
            .samples
            .iter()
            .zip(second_chunk.samples.iter())
            .any(|(a, b)| (a - b).abs() > f32::EPSILON);
        assert!(differs, "Consecutive chunks should have different samples");
    }
}
