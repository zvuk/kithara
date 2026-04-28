use std::{
    io::{Cursor, Error as IoError},
    sync::{Arc, atomic::AtomicU64},
};

use kithara_stream::{AudioCodec, ContainerFormat};
use kithara_test_utils::{create_test_wav, kithara};

use super::{
    inner::{DecoderConfig, DecoderFactory},
    probe::{CodecSelector, ProbeHint, container_from_extension, probe_codec},
};
use crate::{
    InnerDecoder,
    backend::{BoxedSource, HardwareBackend},
    error::DecodeError,
};

const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/test.mp3"
));

struct FailingHardwareBackend;

impl HardwareBackend for FailingHardwareBackend {
    fn can_seek_container(container: ContainerFormat) -> bool {
        container == ContainerFormat::Wav
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        (codec == AudioCodec::Pcm).then_some(ContainerFormat::Wav)
    }

    fn supports_codec(codec: AudioCodec) -> bool {
        codec == AudioCodec::Pcm
    }

    fn try_create(
        _source: BoxedSource,
        _config: &DecoderConfig,
        _codec: AudioCodec,
        _container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, DecodeError> {
        Err(DecodeError::Backend(Box::new(IoError::other(
            "forced hardware failure",
        ))))
    }
}

#[kithara::test]
fn test_decoder_config_default() {
    let config = DecoderConfig::default();
    assert!(!config.prefer_hardware);
    assert!(config.byte_len_handle.is_none());
    assert!(config.gapless);
}

#[kithara::test]
fn test_decoder_config_custom() {
    let handle = Arc::new(AtomicU64::new(1000));
    let config = DecoderConfig {
        prefer_hardware: true,
        byte_len_handle: Some(Arc::clone(&handle)),
        gapless: false,
        hint: Some("mp3".to_string()),
        stream_ctx: None,
        epoch: 0,
        pcm_pool: None,
        byte_pool: None,
        hooks: None,
    };
    assert!(config.prefer_hardware);
    assert!(config.byte_len_handle.is_some());
    assert!(!config.gapless);
    assert_eq!(config.hint, Some("mp3".to_string()));
}

#[kithara::test]
fn test_auto_selector_fails() {
    let empty = Cursor::new(Vec::new());
    let result = DecoderFactory::create(empty, &CodecSelector::Auto, &DecoderConfig::default());
    assert!(matches!(result, Err(DecodeError::ProbeFailed)));
}

#[kithara::test]
fn test_hardware_failure_is_terminal_no_symphonia_fallback() {
    let wav_data = create_test_wav(64, 44_100, 2);
    let config = DecoderConfig {
        prefer_hardware: true,
        ..Default::default()
    };
    let hint = ProbeHint {
        codec: Some(AudioCodec::Pcm),
        container: Some(ContainerFormat::Wav),
        ..Default::default()
    };

    let result = DecoderFactory::create_with_backend::<FailingHardwareBackend>(
        Box::new(Cursor::new(wav_data)),
        &CodecSelector::Probe(hint),
        &config,
    );
    assert!(
        result.is_err(),
        "hardware failure with prefer_hardware=true must not fall back to Symphonia"
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
