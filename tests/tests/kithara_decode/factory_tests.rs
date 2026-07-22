use std::io::Cursor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use std::sync::atomic::AtomicU64;

use kithara::{
    self,
    decode::{DecodeError, DecoderBackend, DecoderConfig, DecoderFactory},
    platform::{sync::Arc, time::Duration},
};

const TEST_MP3_BYTES: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../assets/test.mp3"));

#[kithara::test]
fn decoder_config_default_uses_symphonia_backend() {
    let config: DecoderConfig = DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .build();
    assert_eq!(config.backend, DecoderBackend::Symphonia);
    assert!(config.byte_len_handle.is_none());
    assert_eq!(config.blend_duration, Duration::from_millis(10));
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[kithara::test]
fn decoder_config_custom_apple_backend_preserves_fields() {
    let handle = Arc::new(AtomicU64::new(1000));
    let mut config: DecoderConfig =
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .build();
    config.backend = DecoderBackend::Apple;
    config.byte_len_handle = Some(Arc::clone(&handle));
    config.hint = Some("mp3".to_string());
    assert_eq!(config.backend, DecoderBackend::Apple);
    assert!(config.byte_len_handle.is_some());
    assert_eq!(config.hint, Some("mp3".to_string()));
}

#[kithara::test]
fn create_with_probe_without_hint_fails_with_probe_failed() {
    let result = DecoderFactory::create_with_probe(
        Cursor::new(TEST_MP3_BYTES.to_vec()),
        None,
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .build(),
    );
    assert!(matches!(result, Err(DecodeError::ProbeFailed)));
}

#[kithara::test]
fn create_with_probe_with_mp3_hint_succeeds() {
    let decoder = DecoderFactory::create_with_probe(
        Cursor::new(TEST_MP3_BYTES.to_vec()),
        Some("mp3"),
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .blend_duration(Duration::from_millis(17))
            .build(),
    )
    .expect("BUG: mp3 hint should produce a decoder");

    let spec = decoder.spec();
    assert!(spec.channels > 0);
    assert!(spec.sample_rate.get() > 0);
    assert_eq!(decoder.blend_duration(), Duration::from_millis(17));
}

#[kithara::test]
fn create_from_media_info_surfaces_error_without_native_probe_fallback() {
    use kithara::stream::{AudioCodec, ContainerFormat, MediaInfo};

    let media_info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
    let source = Cursor::new(TEST_MP3_BYTES.to_vec());
    let result = DecoderFactory::create_from_media_info(
        source,
        &media_info,
        DecoderConfig::<kithara::resampler::NoResamplerBackend>::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .build(),
    );
    assert!(
        result.is_err(),
        "create_from_media_info must propagate the typed error from the metadata-driven path \
         — no native-probe fallback to mask a codec/container mismatch"
    );
}
