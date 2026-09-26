use std::io::Cursor;

use kithara_decode::{DecodeError, DecoderChunkOutcome, DecoderConfig, DecoderFactory};
use kithara_platform::time::Duration;
use kithara_resampler::NoResamplerBackend;
use kithara_signal::AudioChunk;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::{
    bufpool::{TestPools, pools},
    kithara,
};

type TestDecoderConfig = DecoderConfig<NoResamplerBackend, TestPools>;
use kithara_test_fixtures::fixtures::{decoder_wav, seek_decoder_wav, short_decoder_wav, tone_wav};

#[kithara::test]
#[case(Some(ContainerFormat::Wav))]
#[case(None)]
fn test_create_decoder_wav(#[case] container: Option<ContainerFormat>, decoder_wav: &'static [u8]) {
    let wav_data = decoder_wav;
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(container)
        .build();
    let decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder()
            .pools(pools())
            .hint("wav")
            .build(),
    );
    assert!(decoder.is_ok(), "decoder creation should succeed");

    let decoder = decoder.unwrap();
    assert_eq!(decoder.spec().sample_rate.get(), 44100);
    assert_eq!(decoder.spec().channels, 2);
}

#[kithara::test]
fn test_next_chunk_returns_data(decoder_wav: &'static [u8]) {
    let wav_data = decoder_wav;
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let mut decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .expect("BUG: decoder");

    let outcome = decoder.next_chunk().unwrap();
    assert!(matches!(outcome, DecoderChunkOutcome::Chunk(_)));

    let chunk = AudioChunk::try_from(outcome).unwrap();
    assert_eq!(chunk.spec().sample_rate.get(), 44100);
    assert_eq!(chunk.spec().channels, 2);
    assert!(!chunk.samples.is_empty());
}

#[kithara::test]
fn test_next_chunk_eof(short_decoder_wav: &'static [u8]) {
    let wav_data = short_decoder_wav;
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let mut decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .expect("BUG: decoder");

    while matches!(decoder.next_chunk().unwrap(), DecoderChunkOutcome::Chunk(_)) {}

    let result = decoder.next_chunk().unwrap();
    assert!(matches!(result, DecoderChunkOutcome::Eof));
}

#[kithara::test]
fn test_seek_to_beginning(seek_decoder_wav: &'static [u8]) {
    let wav_data = seek_decoder_wav;
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let mut decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .expect("BUG: decoder");

    let _ = decoder.next_chunk().unwrap();
    let _ = decoder.next_chunk().unwrap();

    decoder.seek(Duration::from_secs(0)).unwrap();

    let outcome = decoder.next_chunk().unwrap();
    assert!(matches!(outcome, DecoderChunkOutcome::Chunk(_)));
}

#[kithara::test]
fn test_duration_available(tone_wav: &'static [u8]) {
    let wav_data = tone_wav;
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    )
    .expect("BUG: decoder");

    let duration = decoder.duration();
    assert!(duration.is_some());

    let dur = duration.unwrap();
    assert!(dur.as_secs_f64() > 0.9 && dur.as_secs_f64() < 1.1);
}

#[kithara::test]
#[case(Vec::new())]
#[case([0xDE, 0xAD, 0xBE, 0xEF].repeat(100))]
fn test_invalid_input_fails(#[case] data: Vec<u8>) {
    let cursor = Cursor::new(data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Pcm))
        .maybe_container(Some(ContainerFormat::Wav))
        .build();
    let result = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    );
    assert!(result.is_err());
}

#[kithara::test]
fn test_unsupported_container_returns_error() {
    let data = vec![0u8; 100];
    let cursor = Cursor::new(data);
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::MpegTs))
        .build();
    let result = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    );
    #[cfg(not(target_os = "android"))]
    assert!(matches!(
        result,
        Err(DecodeError::UnsupportedContainer { .. })
    ));
    #[cfg(target_os = "android")]
    assert!(matches!(
        result,
        Err(DecodeError::UnsupportedCodec {
            codec: AudioCodec::AacLc
        })
    ));
}
