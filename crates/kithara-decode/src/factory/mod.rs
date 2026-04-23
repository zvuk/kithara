//! Factory for creating decoders with runtime codec selection.
//!
//! This module provides [`DecoderFactory`] which creates decoders
//! based on runtime codec information, handling probing and backend selection.
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
//!     DecoderConfig::default(),
//! )?;
//! ```

mod hardware;
mod probe;
mod symphonia_entry;

use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use derivative::Derivative;
use kithara_bufpool::PcmPool;
use kithara_stream::{MediaInfo, StreamContext};

pub(crate) use self::probe::{CodecSelector, ProbeHint};
use self::{
    hardware::{HardwareAttempt, try_hardware_decoder},
    probe::{container_from_extension, probe_codec, resolve_codec_container},
    symphonia_entry::{create_symphonia_from_boxed, create_with_symphonia_probe},
};
use crate::{
    InnerDecoder,
    error::{DecodeError, DecodeResult},
    hardware::{BoxedSource, HardwareBackend, PlatformBackend},
};

/// Configuration for `DecoderFactory`.
#[derive(Clone, Derivative)]
#[derivative(Default)]
pub struct DecoderConfig {
    /// Prefer hardware decoder when available.
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
}

/// Factory for creating decoders with runtime backend selection.
///
/// Supports multiple backends with automatic fallback:
/// - Apple `AudioToolbox` (macOS/iOS) when the `apple` feature is enabled
/// - Android `MediaCodec` (Android) when the `android` feature is enabled
/// - Symphonia (software, all platforms) as a default fallback
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with automatic backend selection.
    pub(crate) fn create<R>(
        source: R,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let source: BoxedSource = Box::new(source);
        Self::create_with_backend::<PlatformBackend>(source, selector, config)
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

        let source = match try_hardware_decoder::<B>(source, codec, container, config) {
            HardwareAttempt::Decoded(decoder) => return Ok(decoder),
            HardwareAttempt::Skipped(src) | HardwareAttempt::Failed(src) => src,
        };

        create_symphonia_from_boxed(source, codec, container, config)
    }

    /// Create a decoder for seek-time recreation with a metadata-first strategy.
    ///
    /// First tries `create_from_media_info`. If that fails, retries with
    /// native Symphonia probing from a fresh source.
    ///
    /// # Errors
    ///
    /// Returns error when both strategies fail.
    pub fn create_for_recreate<R, F>(
        make_source: F,
        media_info: &MediaInfo,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        F: Fn() -> R,
    {
        match Self::create_from_media_info(make_source(), media_info, &config) {
            Ok(decoder) => Ok(decoder),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    ?media_info,
                    "create_from_media_info failed during recreate; retrying with native probe"
                );
                Self::create_with_symphonia_probe(make_source(), config)
            }
        }
    }

    /// Create decoder from `MediaInfo` (for kithara-audio compatibility).
    ///
    /// Extracts codec from `MediaInfo` and creates the appropriate decoder.
    ///
    /// # Errors
    ///
    /// Returns error if no codec can be determined or decoder creation fails.
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

    /// Create decoder from a file-extension hint. Falls back to native probe when
    /// the hint is missing or too weak to pick a codec.
    ///
    /// # Errors
    ///
    /// Returns error if codec cannot be determined or decoder creation fails.
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

        if probe_codec(&probe_hint).is_err() {
            return Self::create_with_symphonia_probe(source, config);
        }

        match Self::create(source, &CodecSelector::Probe(probe_hint), &config) {
            Ok(decoder) => Ok(decoder),
            Err(DecodeError::ProbeFailed) => Err(DecodeError::ProbeFailed),
            Err(e) => Err(e),
        }
    }

    /// Create a decoder by delegating to Symphonia's native probe.
    ///
    /// Useful after ABR variant switches when HLS metadata disagrees with the
    /// actual data (e.g., WAV served via HLS).
    ///
    /// # Errors
    ///
    /// Returns error if Symphonia's probe fails or decoder creation fails.
    pub fn create_with_symphonia_probe<R>(
        source: R,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        create_with_symphonia_probe(source, config)
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
    use crate::hardware::{
        BoxedSource, HardwareBackend, RecoverableHardwareError, recoverable_hardware_error,
    };

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
            source: BoxedSource,
            _config: &DecoderConfig,
            _codec: AudioCodec,
            _container: Option<ContainerFormat>,
        ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
            Err(recoverable_hardware_error(
                source,
                DecodeError::Backend(Box::new(IoError::other("forced hardware failure"))),
            ))
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
    fn test_create_for_recreate_falls_back_to_native_probe_on_mismatch() {
        let wav_data = create_test_wav(64, 44_100, 2);
        let wrong_info = MediaInfo::new(Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio));

        let decoder = DecoderFactory::create_for_recreate(
            || Cursor::new(wav_data.clone()),
            &wrong_info,
            DecoderConfig::default(),
        )
        .expect("native probe fallback should recreate decoder");

        let spec = decoder.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 44_100);
    }

    #[kithara::test]
    fn test_create_for_recreate_fails_when_all_strategies_fail() {
        let wrong_info = MediaInfo::new(Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio));
        let result = DecoderFactory::create_for_recreate(
            || Cursor::new(Vec::<u8>::new()),
            &wrong_info,
            DecoderConfig::default(),
        );
        assert!(result.is_err());
    }

    #[kithara::test]
    fn test_create_with_probe_without_hint_falls_back_to_symphonia_probe() {
        let decoder = DecoderFactory::create_with_probe(
            Cursor::new(TEST_MP3_BYTES.to_vec()),
            None,
            DecoderConfig::default(),
        )
        .expect("native probe fallback should create MP3 decoder");

        let spec = decoder.spec();
        assert!(spec.channels > 0);
        assert!(spec.sample_rate > 0);
    }

    #[kithara::test]
    fn test_recoverable_hardware_failure_falls_back_to_symphonia() {
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

        let decoder = DecoderFactory::create_with_backend::<FailingHardwareBackend>(
            Box::new(Cursor::new(wav_data)),
            &CodecSelector::Probe(hint),
            &config,
        )
        .expect("recoverable hardware failure should fall back to Symphonia");

        let spec = decoder.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 44_100);
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
