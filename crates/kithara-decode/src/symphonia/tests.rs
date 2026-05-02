//! Tests for the Symphonia bootstrap path. Now drives
//! [`UniversalDecoder<SymphoniaDemuxer, SymphoniaCodec>`] through
//! [`crate::factory::DecoderFactory::create_with_probe`] / `_from_media_info`,
//! since the legacy whole-stream `SymphoniaDecoder` was deleted.

use std::{io::Cursor, time::Duration};

use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};
use kithara_test_utils::{create_test_wav, kithara};

use crate::{
    error::DecodeError,
    factory::{DecoderConfig, DecoderFactory},
};

#[kithara::test]
#[case(Some(ContainerFormat::Wav))]
#[case(None)]
fn test_create_decoder_wav(#[case] container: Option<ContainerFormat>) {
    let wav_data = create_test_wav(100, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), container);
    let decoder = DecoderFactory::create_from_media_info(
        cursor,
        &media_info,
        &DecoderConfig {
            hint: Some("wav".into()),
            ..Default::default()
        },
    );
    assert!(decoder.is_ok(), "decoder creation should succeed");

    let decoder = decoder.unwrap();
    assert_eq!(decoder.spec().sample_rate, 44100);
    assert_eq!(decoder.spec().channels, 2);
}

#[kithara::test]
fn test_next_chunk_returns_data() {
    let wav_data = create_test_wav(100, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let mut decoder =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default())
            .expect("decoder");

    let outcome = decoder.next_chunk().unwrap();
    assert!(outcome.is_chunk());

    let chunk = outcome.into_chunk().unwrap();
    assert_eq!(chunk.spec().sample_rate, 44100);
    assert_eq!(chunk.spec().channels, 2);
    assert!(!chunk.pcm.is_empty());
}

#[kithara::test]
fn test_next_chunk_eof() {
    let wav_data = create_test_wav(10, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let mut decoder =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default())
            .expect("decoder");

    while decoder.next_chunk().unwrap().is_chunk() {}

    let result = decoder.next_chunk().unwrap();
    assert!(result.is_eof());
}

#[kithara::test]
fn test_seek_to_beginning() {
    let wav_data = create_test_wav(10000, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let mut decoder =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default())
            .expect("decoder");

    let _ = decoder.next_chunk().unwrap();
    let _ = decoder.next_chunk().unwrap();

    decoder.seek(Duration::from_secs(0)).unwrap();

    let outcome = decoder.next_chunk().unwrap();
    assert!(outcome.is_chunk());
}

#[kithara::test]
fn test_duration_available() {
    let wav_data = create_test_wav(44100, 44100, 2);
    let cursor = Cursor::new(wav_data);
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let decoder =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default())
            .expect("decoder");

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
    let media_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let result =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default());
    assert!(result.is_err());
}

#[kithara::test]
fn test_unsupported_container_returns_error() {
    let data = vec![0u8; 100];
    let cursor = Cursor::new(data);
    let media_info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::MpegTs));
    let result =
        DecoderFactory::create_from_media_info(cursor, &media_info, &DecoderConfig::default());
    assert!(matches!(result, Err(DecodeError::UnsupportedContainer(_))));
}
