use std::io::Cursor;
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
use std::num::NonZeroU32;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use std::sync::atomic::AtomicU64;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use kithara::platform::sync::Arc;
use kithara::{
    self,
    decode::{DecodeError, DecoderBackend, DecoderConfig, DecoderFactory},
    resampler::NoResamplerBackend,
    stream::{AudioCodec, ContainerFormat, MediaInfo},
};
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
use kithara::{
    decode::{DecodeResult, Decoder, DecoderResamplerConfig},
    resampler::{ResamplerOptions, ResamplerQuality},
};
use kithara_integration_tests::bufpool_ext::{TestPools, pools};
use kithara_test_fixtures::assets::signal_mp3_track_sine440_187s;

type TestDecoderConfig = DecoderConfig<NoResamplerBackend, TestPools>;

#[kithara::test]
fn decoder_config_default_uses_symphonia_backend() {
    let config: TestDecoderConfig = TestDecoderConfig::builder().pools(pools()).build();
    assert_eq!(config.backend, DecoderBackend::Symphonia);
    assert!(config.byte_len_handle.is_none());
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[kithara::test]
fn decoder_config_custom_apple_backend_preserves_fields() {
    let handle = Arc::new(AtomicU64::new(1000));
    let mut config: TestDecoderConfig = TestDecoderConfig::builder().pools(pools()).build();
    config.backend = DecoderBackend::Apple;
    config.byte_len_handle = Some(Arc::clone(&handle));
    config.hint = Some("mp3".to_string());
    assert_eq!(config.backend, DecoderBackend::Apple);
    assert!(config.byte_len_handle.is_some());
    assert_eq!(config.hint, Some("mp3".to_string()));
}

#[kithara::test]
#[case::without_hint(None, false)]
#[case::mp3_hint(Some("mp3"), true)]
fn create_with_probe_uses_hint(#[case] hint: Option<&str>, #[case] should_succeed: bool) {
    let result = DecoderFactory::create_with_probe(
        Cursor::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
        hint,
        TestDecoderConfig::builder().pools(pools()).build(),
    );
    if should_succeed {
        let spec = result.expect("mp3 hint should produce a decoder").spec();
        assert!(spec.channels > 0);
        assert!(spec.sample_rate.get() > 0);
    } else {
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }
}

#[kithara::test]
fn create_from_media_info_surfaces_error_without_native_probe_fallback() {
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::AacLc))
        .maybe_container(Some(ContainerFormat::Fmp4))
        .build();
    let source = Cursor::new(signal_mp3_track_sine440_187s().bytes().to_vec());
    let result = DecoderFactory::create_from_media_info(
        source,
        &media_info,
        TestDecoderConfig::builder().pools(pools()).build(),
    );
    assert!(
        result.is_err(),
        "create_from_media_info must propagate the typed error from the metadata-driven path \
         — no native-probe fallback to mask a codec/container mismatch"
    );
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
fn apple_mp3_decoder(
    media_info: &MediaInfo,
    resampler: Option<DecoderResamplerConfig<NoResamplerBackend>>,
) -> DecodeResult<Box<dyn Decoder>> {
    let config = TestDecoderConfig::builder()
        .pools(pools())
        .backend(DecoderBackend::Apple)
        .maybe_resampler(resampler)
        .build();
    DecoderFactory::create_from_media_info(
        Cursor::new(signal_mp3_track_sine440_187s().bytes().to_vec()),
        media_info,
        config,
    )
}

/// With the SRC fused into the Apple codec there is no external resampler
/// backend to fall back on — `NoResamplerBackend` is the whole story on device.
/// A host rate that differs from the source rate must therefore be served by
/// the codec itself, which is what a route change asks for at runtime.
#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
#[kithara::test]
fn apple_fused_src_decodes_at_the_requested_host_rate() {
    let media_info = MediaInfo::builder()
        .maybe_codec(Some(AudioCodec::Mp3))
        .maybe_container(Some(ContainerFormat::MpegAudio))
        .build();
    let source_rate = apple_mp3_decoder(&media_info, None)
        .expect("BUG: the Apple backend must open the MP3 fixture")
        .spec()
        .sample_rate;
    let target_rate = source_rate
        .checked_mul(NonZeroU32::new(2).expect("BUG: two is non-zero"))
        .expect("BUG: a doubled sample rate must fit in u32");

    let resampler = Some(
        DecoderResamplerConfig::builder()
            .target_sample_rate(target_rate)
            .backend(NoResamplerBackend)
            .quality(ResamplerQuality::default())
            .options(ResamplerOptions::default())
            .build(),
    );

    match apple_mp3_decoder(&media_info, resampler) {
        Ok(decoder) => assert_eq!(
            decoder.spec().sample_rate,
            target_rate,
            "the fused Apple codec accepted the plan but kept emitting at {source_rate}"
        ),
        Err(error) => panic!(
            "the fused Apple codec must convert {source_rate} to the requested {target_rate}, \
             but decoder creation failed: {error}"
        ),
    }
}
