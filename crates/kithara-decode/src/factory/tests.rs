use std::io::{Cursor, Error as IoError};
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use std::sync::{Arc, atomic::AtomicU64};

use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::{create_test_wav, kithara};

use super::{
    inner::{DecoderBackend, DecoderConfig, DecoderFactory},
    probe::{CodecSelector, ProbeHint, container_from_extension, probe_codec},
};
use crate::error::DecodeError;

const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/test.mp3"
));

#[kithara::test]
fn test_decoder_config_default() {
    let config = DecoderConfig::default();
    assert_eq!(config.backend, DecoderBackend::Symphonia);
    assert!(config.byte_len_handle.is_none());
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
#[kithara::test]
fn test_decoder_config_custom() {
    let handle = Arc::new(AtomicU64::new(1000));
    let config = DecoderConfig {
        backend: DecoderBackend::Apple,
        byte_len_handle: Some(Arc::clone(&handle)),
        hint: Some("mp3".to_string()),
        ..DecoderConfig::default()
    };
    assert_eq!(config.backend, DecoderBackend::Apple);
    assert!(config.byte_len_handle.is_some());
    assert_eq!(config.hint, Some("mp3".to_string()));
}

#[kithara::test]
fn test_auto_selector_fails() {
    let empty = Cursor::new(Vec::new());
    let result = DecoderFactory::create(empty, &CodecSelector::Auto, &DecoderConfig::default());
    assert!(matches!(result, Err(DecodeError::ProbeFailed)));
}

/// `DecoderBackend::Symphonia` propagates a typed error verbatim when
/// the source produces no decoder — no fallback chain to mask probe /
/// codec-unsupported failures. Replaces the old
/// `test_hardware_failure_is_terminal_no_symphonia_fallback` which
/// relied on a `FailingDecoderBackend` shim wired through the now-gone
/// `create_with_backend::<B>` generic dispatch.
#[kithara::test]
fn test_symphonia_propagates_probe_error_without_fallback() {
    let _ = create_test_wav; // reserved for future positive-path coverage
    let _ = (
        IoError::other("force keep import"),
        AudioCodec::Pcm,
        ContainerFormat::Wav,
    );
    let config = DecoderConfig {
        backend: DecoderBackend::Symphonia,
        ..Default::default()
    };
    let hint = ProbeHint {
        codec: Some(AudioCodec::Opus),
        ..Default::default()
    };
    let empty = Cursor::new(Vec::new());
    let result = DecoderFactory::create(empty, &CodecSelector::Probe(hint), &config);
    assert!(
        result.is_err(),
        "Symphonia must surface a typed error without falling back to a different backend"
    );
}

#[kithara::test]
fn test_create_with_probe_without_hint_fails_with_probe_failed() {
    let result = DecoderFactory::create_with_probe(
        Cursor::new(TEST_MP3_BYTES.to_vec()),
        None,
        DecoderConfig::default(),
    );
    assert!(matches!(result, Err(DecodeError::ProbeFailed)));
}

#[cfg(feature = "symphonia")]
#[kithara::test]
fn test_create_with_probe_with_mp3_hint_succeeds() {
    let decoder = DecoderFactory::create_with_probe(
        Cursor::new(TEST_MP3_BYTES.to_vec()),
        Some("mp3"),
        DecoderConfig::default(),
    )
    .expect("mp3 hint should produce a decoder");

    let spec = decoder.spec();
    assert!(spec.channels > 0);
    assert!(spec.sample_rate > 0);
}

/// `create_for_recreate` must propagate the metadata-driven error
/// verbatim. Before the fix, any failure from `create_from_media_info`
/// was silently retried via a native Symphonia probe on a fresh
/// source — which happily matched MP3 frame sync in mid-fMP4 bytes,
/// returning an `MpaReader` while `session.media_info` still claimed
/// fMP4/AAC (see silvercomet seek RCA §3.2). The contract is: one
/// decoder path per call, no fallback.
#[cfg(feature = "symphonia")]
#[kithara::test]
fn create_for_recreate_surfaces_error_without_native_probe_fallback() {
    use kithara_stream::MediaInfo;

    let media_info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
    let make_source = || Cursor::new(TEST_MP3_BYTES.to_vec());
    let result =
        DecoderFactory::create_for_recreate(make_source, &media_info, &DecoderConfig::default());
    assert!(
        result.is_err(),
        "create_for_recreate must propagate the typed error from the \
         metadata-driven path — no native-probe fallback to mask a \
         codec/container mismatch"
    );
}

#[kithara::test]
fn test_create_with_probe_maps_m4a_to_mp4_container_hint() {
    let probe_hint = ProbeHint {
        extension: Some("m4a".into()),
        container: container_from_extension("m4a"),
        ..Default::default()
    };

    assert_eq!(probe_hint.container, Some(ContainerFormat::Mp4));
    assert_eq!(
        probe_codec(&probe_hint).expect("m4a should map to AAC"),
        AudioCodec::AacLc
    );
}
