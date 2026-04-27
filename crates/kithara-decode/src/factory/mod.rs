//! Factory for creating decoders with runtime codec selection.
//!
//! Exactly one decoder path is taken per call — no fallback. The factory
//! picks either the compiled hardware backend (when `prefer_hardware` is
//! set and the backend accepts the codec/container) or Symphonia (when
//! the `symphonia` feature is enabled). If neither path is available
//! the call fails with a classified `DecodeError` — callers must treat
//! such failures as terminal.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::{DecoderFactory, DecoderConfig};
//! use kithara_stream::{AudioCodec, MediaInfo};
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let media_info = MediaInfo { codec: Some(AudioCodec::Mp3), ..Default::default() };
//! let decoder = DecoderFactory::create_from_media_info(
//!     file,
//!     &media_info,
//!     &DecoderConfig::default(),
//! )?;
//! ```

mod hardware;
mod probe;

use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use derivative::Derivative;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_stream::{MediaInfo, SharedHooks, StreamContext};

pub(crate) use self::probe::{CodecSelector, ProbeHint};
use self::{
    hardware::{HardwareAttempt, try_hardware_decoder},
    probe::{container_from_extension, probe_codec, resolve_codec_container},
};
use crate::{
    InnerDecoder,
    backend::{BoxedSource, HardwareBackend, current::Current},
    error::{DecodeError, DecodeResult},
    hooks::HookedDecoder,
};

/// Configuration for `DecoderFactory`.
#[derive(Clone, Derivative)]
#[derivative(Default)]
pub struct DecoderConfig {
    /// Route decoding through the hardware backend (Apple `AudioToolbox`
    /// or Android `MediaCodec`). When `false`, the factory uses Symphonia
    /// (requires the `symphonia` feature). There is no fallback between
    /// the two paths.
    pub prefer_hardware: bool,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Enable gapless playback.
    #[derivative(Default(value = "true"))]
    pub gapless: bool,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used when the container format is not specified, as a hint for auto-detection.
    pub hint: Option<String>,
    /// Stream context for segment/variant metadata.
    pub stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub pcm_pool: Option<PcmPool>,
    /// Optional byte buffer pool override.
    ///
    /// Used by backends that need a reusable raw byte buffer (e.g. the
    /// Apple fMP4 reader loads the container into a pooled slice and
    /// slices packets out without per-packet allocations).  When `None`,
    /// the global `kithara_bufpool::byte_pool()` is used.
    pub byte_pool: Option<BytePool>,
    /// Reader-side hooks injected into the resulting decoder via
    /// [`HookedDecoder`]. The factory wraps the inner decoder in
    /// `HookedDecoder` whenever this is `Some`. `None` keeps the
    /// inner decoder unwrapped (mock sources, tests).
    pub hooks: Option<SharedHooks>,
}

fn wrap_with_hooks(
    inner: Box<dyn InnerDecoder>,
    hooks: Option<SharedHooks>,
) -> Box<dyn InnerDecoder> {
    match hooks {
        Some(hooks) => Box::new(HookedDecoder::new(inner, hooks)),
        None => inner,
    }
}

/// Factory for creating decoders with a single, strict backend selection.
///
/// Backend matrix:
/// - `prefer_hardware == true` → hardware backend (Apple/Android). Fails if
///   the codec/container is unsupported or decoder construction errors.
/// - `prefer_hardware == false` → Symphonia (requires `feature = "symphonia"`).
///   Without the feature the call fails with `UnsupportedCodec`.
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with the single selected backend.
    pub(crate) fn create<R>(
        source: R,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let source: BoxedSource = Box::new(source);
        Self::create_with_backend::<Current>(source, selector, config)
    }

    fn create_with_backend<B: HardwareBackend>(
        source: BoxedSource,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>> {
        let (codec, container) = resolve_codec_container(selector)?;

        tracing::debug!(
            ?codec,
            ?container,
            prefer_hardware = config.prefer_hardware,
            "DecoderFactory::create called"
        );

        let inner = if config.prefer_hardware {
            match try_hardware_decoder::<B>(source, codec, container, config) {
                HardwareAttempt::Decoded(decoder) => Ok(decoder),
                HardwareAttempt::Skipped => {
                    tracing::warn!(
                        ?codec,
                        ?container,
                        "hardware backend does not support codec/container; \
                         no fallback when prefer_hardware=true"
                    );
                    Err(DecodeError::UnsupportedCodec(codec))
                }
                HardwareAttempt::Failed(err) => Err(err),
            }
        } else {
            #[cfg(feature = "symphonia")]
            {
                crate::symphonia::create_from_boxed(source, codec, container, config)
            }
            #[cfg(not(feature = "symphonia"))]
            {
                let _ = (source, container);
                tracing::warn!(
                    ?codec,
                    "symphonia feature disabled; software decoder unavailable"
                );
                Err(DecodeError::UnsupportedCodec(codec))
            }
        }?;

        Ok(wrap_with_hooks(inner, config.hooks.clone()))
    }

    /// Create decoder from `MediaInfo` (kithara-audio entry point).
    ///
    /// Extracts codec from `MediaInfo` and creates the appropriate decoder.
    ///
    /// # Errors
    ///
    /// Returns error if codec cannot be determined or decoder creation fails.
    /// No fallback — a failure is terminal.
    pub fn create_from_media_info<R>(
        source: R,
        media_info: &MediaInfo,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        tracing::debug!(?media_info, "create_from_media_info called");

        let hint = ProbeHint {
            codec: media_info.codec,
            container: media_info.container,
            extension: None,
            mime: None,
        };

        Self::create(source, &CodecSelector::Probe(hint), config)
    }

    /// Recreate a decoder on HLS variant switch / format change.
    ///
    /// Thin wrapper over `create_from_media_info`: the caller (audio
    /// pipeline) is responsible for handing in a `base_offset` that
    /// aligns with the container's init region, so the metadata-driven
    /// path is the only correct answer. Failures are propagated
    /// verbatim — no native-probe fallback, since probing mid-segment
    /// bytes at a mismatched offset would silently pick an unrelated
    /// codec (e.g. match MP3 frame sync in raw AAC-in-fMP4 bytes) and
    /// drift `session.media_info` out of sync with the decoder it
    /// actually got.
    ///
    /// # Errors
    ///
    /// Returns the decoder-creation error verbatim when the metadata-
    /// driven path cannot build a decoder (unsupported codec, mismatch
    /// between the declared container and the bytes at `base_offset`,
    /// etc.).
    pub fn create_for_recreate<R, F>(
        make_source: F,
        media_info: &MediaInfo,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        F: Fn() -> R,
    {
        Self::create_from_media_info(make_source(), media_info, config).inspect_err(|error| {
            tracing::warn!(
                ?error,
                ?media_info,
                "create_from_media_info failed during recreate"
            );
        })
    }

    /// Create decoder from a file-extension hint.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::ProbeFailed` when the hint is missing or too
    /// weak to pick a codec, and `DecodeError::*` for backend failures.
    /// No fallback — callers must supply a usable hint.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "by-value lets callers pass DecoderConfig::default() without explicit borrow"
    )]
    pub fn create_with_probe<R>(
        source: R,
        hint: Option<&str>,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let probe_hint = ProbeHint {
            container: hint.and_then(container_from_extension),
            extension: hint.map(String::from),
            ..Default::default()
        };

        probe_codec(&probe_hint)?;
        Self::create(source, &CodecSelector::Probe(probe_hint), &config)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Cursor, Error as IoError},
        sync::{Arc, atomic::AtomicU64},
    };

    use kithara_stream::{AudioCodec, ContainerFormat};
    use kithara_test_utils::{create_test_wav, kithara};

    use super::*;
    use crate::backend::{BoxedSource, HardwareBackend};

    const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/test.mp3"
    ));

    struct FailingHardwareBackend;

    impl HardwareBackend for FailingHardwareBackend {
        fn supports_codec(codec: AudioCodec) -> bool {
            codec == AudioCodec::Pcm
        }

        fn can_seek_container(container: ContainerFormat) -> bool {
            container == ContainerFormat::Wav
        }

        fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
            (codec == AudioCodec::Pcm).then_some(ContainerFormat::Wav)
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
        let media_info = MediaInfo::new(Some(AudioCodec::AacLc), Some(ContainerFormat::Fmp4));
        let make_source = || Cursor::new(TEST_MP3_BYTES.to_vec());
        let result = DecoderFactory::create_for_recreate(
            make_source,
            &media_info,
            &DecoderConfig::default(),
        );
        assert!(
            result.is_err(),
            "create_for_recreate must propagate the typed error from the \
             metadata-driven path — no native-probe fallback to mask a \
             codec/container mismatch"
        );
    }

    #[kithara::test]
    fn test_create_with_probe_maps_m4a_to_mp4_container_hint() {
        use self::probe::{container_from_extension, probe_codec};
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
}
