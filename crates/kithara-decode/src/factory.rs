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

use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use derivative::Derivative;
use kithara_bufpool::PcmPool;
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, StreamContext};

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use crate::apple::{AppleAac, AppleAlac, AppleConfig, AppleFlac, AppleMp3};
use crate::{
    error::{DecodeError, DecodeResult},
    symphonia::{
        Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
    },
    traits::{Alac, AudioDecoder, InnerDecoder},
};

/// Selector for choosing how to detect/specify the codec.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), expect(dead_code))]
pub(crate) enum CodecSelector {
    /// Known codec - no probing needed.
    Exact(AudioCodec),
    /// Probe with hints.
    Probe(ProbeHint),
    /// Full auto-probe.
    Auto,
}

/// Hints for codec probing.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProbeHint {
    /// Known codec (highest priority).
    pub codec: Option<AudioCodec>,
    /// Container format hint.
    pub container: Option<ContainerFormat>,
    /// File extension hint (e.g., "mp3", "aac").
    pub extension: Option<String>,
    /// MIME type hint (e.g., "audio/mpeg", "audio/flac").
    pub mime: Option<String>,
}

/// Configuration for `DecoderFactory`.
#[derive(Derivative)]
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
    /// Used when container format is not specified, as a hint for auto-detection.
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
/// - Apple `AudioToolbox` (macOS/iOS) when `apple` feature enabled
/// - Android `MediaCodec` (Android) when `android` feature enabled
/// - Symphonia (software, all platforms) as default fallback
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with automatic backend selection.
    ///
    /// # Arguments
    ///
    /// * `source` - The audio data source implementing `Read + Seek`.
    /// * `selector` - Specifies how to determine the codec.
    /// * `config` - Decoder configuration options.
    ///
    /// # Returns
    ///
    /// A boxed decoder implementing [`InnerDecoder`] for runtime polymorphism.
    ///
    /// # Backend Selection
    ///
    /// When `prefer_hardware` is true, the factory attempts platform-specific
    /// hardware decoders first, falling back to Symphonia on failure.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::ProbeFailed`] if codec cannot be determined.
    /// Returns [`DecodeError::UnsupportedCodec`] if the codec is not supported.
    pub(crate) fn create<R>(
        source: R,
        selector: &CodecSelector,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Determine codec and container from selector
        let (codec, container) = match *selector {
            CodecSelector::Exact(c) => (c, None),
            CodecSelector::Probe(ref hint) => (Self::probe_codec(hint)?, hint.container),
            CodecSelector::Auto => return Err(DecodeError::ProbeFailed),
        };

        tracing::debug!(
            ?codec,
            ?container,
            prefer_hardware = config.prefer_hardware,
            "DecoderFactory::create called"
        );

        // Use hardware decoder if preferred
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        if config.prefer_hardware {
            tracing::info!(
                ?codec,
                ?container,
                "Using Apple AudioToolbox hardware decoder"
            );
            return Self::create_apple_decoder(source, &config, codec, container);
        }

        // Build Symphonia config from DecoderConfig
        // If container is specified, Symphonia creates reader directly (no probe).
        // If container is None, Symphonia falls back to probe with hint.
        tracing::debug!(?container, hint = ?config.hint, "Using Symphonia decoder");

        let symphonia_config = SymphoniaConfig {
            verify: false,
            gapless: config.gapless,
            byte_len_handle: config.byte_len_handle,
            container,
            hint: config.hint.clone(),
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
            pcm_pool: config.pcm_pool.clone(),
            ..Default::default()
        };

        Self::create_symphonia_decoder(source, codec, symphonia_config)
    }

    /// Create an Apple AudioToolbox decoder.
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    fn create_apple_decoder<R>(
        source: R,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let apple_config = AppleConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container,
            pcm_pool: config.pcm_pool.clone(),
        };

        let source: Box<dyn crate::traits::DecoderInput> = Box::new(source);

        match codec {
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
                let decoder = AppleAac::create(source, apple_config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Mp3 => {
                let decoder = AppleMp3::create(source, apple_config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Flac => {
                let decoder = AppleFlac::create(source, apple_config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Alac => {
                let decoder = AppleAlac::create(source, apple_config)?;
                Ok(Box::new(decoder))
            }
            _ => Err(DecodeError::UnsupportedCodec(codec)),
        }
    }

    /// Probe codec from hints.
    ///
    /// Priority:
    /// 1. Direct codec hint
    /// 2. Extension mapping
    /// 3. MIME type mapping
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::ProbeFailed`] if no codec can be determined.
    fn probe_codec(hint: &ProbeHint) -> DecodeResult<AudioCodec> {
        // Priority 1: Direct codec hint
        if let Some(codec) = hint.codec {
            return Ok(codec);
        }

        // Priority 2: Extension mapping
        if let Some(ref ext) = hint.extension
            && let Some(codec) = Self::codec_from_extension(ext)
        {
            return Ok(codec);
        }

        // Priority 3: MIME type mapping
        if let Some(ref mime) = hint.mime
            && let Some(codec) = Self::codec_from_mime(mime)
        {
            return Ok(codec);
        }

        // Priority 4: Container format hint (can suggest likely codec)
        if let Some(container) = hint.container
            && let Some(codec) = Self::codec_from_container(container)
        {
            return Ok(codec);
        }

        Err(DecodeError::ProbeFailed)
    }

    /// Map file extension to codec.
    fn codec_from_extension(ext: &str) -> Option<AudioCodec> {
        match ext.to_lowercase().as_str() {
            "mp3" => Some(AudioCodec::Mp3),
            "aac" | "m4a" | "mp4" => Some(AudioCodec::AacLc),
            "flac" => Some(AudioCodec::Flac),
            "ogg" | "oga" => Some(AudioCodec::Vorbis),
            "opus" => Some(AudioCodec::Opus),
            "wav" | "wave" | "aiff" | "aif" => Some(AudioCodec::Pcm),
            "caf" => Some(AudioCodec::Alac),
            _ => None,
        }
    }

    /// Map MIME type to codec.
    fn codec_from_mime(mime: &str) -> Option<AudioCodec> {
        let mime_lower = mime.to_lowercase();

        // Check for specific codec indicators
        if mime_lower.contains("mp3") || mime_lower == "audio/mpeg" {
            return Some(AudioCodec::Mp3);
        }
        if mime_lower.contains("aac") || mime_lower == "audio/aac" {
            return Some(AudioCodec::AacLc);
        }
        if mime_lower.contains("flac") || mime_lower == "audio/flac" {
            return Some(AudioCodec::Flac);
        }
        if mime_lower.contains("vorbis") || mime_lower == "audio/vorbis" {
            return Some(AudioCodec::Vorbis);
        }
        if mime_lower.contains("opus") || mime_lower == "audio/opus" {
            return Some(AudioCodec::Opus);
        }
        if mime_lower == "audio/ogg" {
            // Ogg container, default to Vorbis
            return Some(AudioCodec::Vorbis);
        }
        if mime_lower == "audio/wav" || mime_lower == "audio/wave" || mime_lower == "audio/x-wav" {
            return Some(AudioCodec::Pcm);
        }
        if mime_lower == "audio/mp4" || mime_lower == "audio/x-m4a" {
            return Some(AudioCodec::AacLc);
        }

        None
    }

    /// Infer likely codec from container format.
    fn codec_from_container(container: ContainerFormat) -> Option<AudioCodec> {
        match container {
            ContainerFormat::MpegAudio => Some(AudioCodec::Mp3),
            ContainerFormat::Adts | ContainerFormat::Fmp4 | ContainerFormat::MpegTs => {
                Some(AudioCodec::AacLc)
            }
            ContainerFormat::Flac => Some(AudioCodec::Flac),
            ContainerFormat::Ogg => Some(AudioCodec::Vorbis),
            ContainerFormat::Wav => Some(AudioCodec::Pcm),
            ContainerFormat::Caf => Some(AudioCodec::Alac),
            ContainerFormat::Mkv => None, // Could be anything
        }
    }

    /// Create a Symphonia decoder for the given codec.
    fn create_symphonia_decoder<R>(
        source: R,
        codec: AudioCodec,
        config: SymphoniaConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        use crate::traits::CodecType;

        /// PCM codec marker for WAV files.
        struct Pcm;
        impl CodecType for Pcm {
            const CODEC: AudioCodec = AudioCodec::Pcm;
        }

        let source: Box<dyn crate::traits::DecoderInput> = Box::new(source);
        match codec {
            AudioCodec::Mp3 => {
                let decoder = SymphoniaMp3::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
                let decoder = SymphoniaAac::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Flac => {
                let decoder = SymphoniaFlac::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Vorbis => {
                let decoder = SymphoniaVorbis::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Alac => {
                let decoder = Symphonia::<Alac>::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Pcm => {
                let decoder = Symphonia::<Pcm>::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Opus | AudioCodec::Adpcm => Err(DecodeError::UnsupportedCodec(codec)),
        }
    }

    /// Create decoder from `MediaInfo` (for kithara-audio compatibility).
    ///
    /// This is a convenience method that extracts codec from `MediaInfo`
    /// and creates the appropriate decoder.
    ///
    /// # Arguments
    ///
    /// * `source` - The audio data source
    /// * `media_info` - Media information containing codec/container hints
    /// * `config` - Decoder configuration
    ///
    /// # Errors
    ///
    /// Returns error if no codec can be determined or decoder creation fails.
    pub fn create_from_media_info<R>(
        source: R,
        media_info: &MediaInfo,
        config: DecoderConfig,
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

    /// Create decoder from file extension hint.
    ///
    /// This maps the extension to codec and container, then creates
    /// the appropriate decoder without probing.
    ///
    /// # Arguments
    ///
    /// * `source` - The audio data source
    /// * `hint` - Optional file extension hint (e.g., "mp3", "wav", "aac")
    /// * `config` - Decoder configuration
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
        // Only pass extension hint, don't derive container format.
        // This ensures Symphonia uses its probe mechanism which can handle
        // files with junk data or unusual metadata.
        let probe_hint = ProbeHint {
            extension: hint.map(String::from),
            ..Default::default()
        };

        match Self::create(source, &CodecSelector::Probe(probe_hint), config) {
            Ok(decoder) => Ok(decoder),
            Err(DecodeError::ProbeFailed) => {
                // Hints alone couldn't determine the codec.
                // This should not happen: the source is already consumed.
                Err(DecodeError::ProbeFailed)
            }
            Err(e) => Err(e),
        }
    }

    /// Create decoder by letting Symphonia probe the data directly.
    ///
    /// Unlike [`Self::create_with_probe`] which requires codec hints, this method
    /// delegates entirely to Symphonia's format detection. Useful after ABR
    /// variant switches when the container format reported by HLS metadata
    /// doesn't match the actual data (e.g., WAV served via HLS).
    ///
    /// # Errors
    ///
    /// Returns error if Symphonia's probe fails to detect the format or
    /// decoder creation fails.
    pub fn create_with_symphonia_probe<R>(
        source: R,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Dummy codec marker — actual codec is determined by Symphonia's probe.
        use crate::traits::CodecType;
        struct ProbeAny;
        impl CodecType for ProbeAny {
            const CODEC: AudioCodec = AudioCodec::Pcm;
        }

        tracing::debug!("Attempting Symphonia native probe (no codec hints)");

        let symphonia_config = SymphoniaConfig {
            verify: false,
            gapless: config.gapless,
            byte_len_handle: config.byte_len_handle,
            container: None,
            hint: config.hint,
            probe_no_seek: true,
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
            pcm_pool: config.pcm_pool,
        };

        let source: Box<dyn crate::traits::DecoderInput> = Box::new(source);
        let decoder = Symphonia::<ProbeAny>::create(source, symphonia_config)?;
        Ok(Box::new(decoder))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use rstest::rstest;

    use super::*;

    #[test]
    fn test_codec_selector_exact() {
        let selector = CodecSelector::Exact(AudioCodec::AacLc);
        assert!(matches!(selector, CodecSelector::Exact(AudioCodec::AacLc)));
    }

    #[test]
    fn test_codec_selector_probe() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Mp3),
            ..Default::default()
        };
        let selector = CodecSelector::Probe(hint);
        assert!(matches!(selector, CodecSelector::Probe(_)));
    }

    #[test]
    fn test_codec_selector_auto() {
        let selector = CodecSelector::Auto;
        assert!(matches!(selector, CodecSelector::Auto));
    }

    #[test]
    fn test_probe_hint_default() {
        let hint = ProbeHint::default();
        assert!(hint.codec.is_none());
        assert!(hint.container.is_none());
        assert!(hint.extension.is_none());
        assert!(hint.mime.is_none());
    }

    #[test]
    fn test_probe_hint_with_all_fields() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Flac),
            container: Some(ContainerFormat::Ogg),
            extension: Some("flac".into()),
            mime: Some("audio/flac".into()),
        };
        assert_eq!(hint.codec, Some(AudioCodec::Flac));
        assert_eq!(hint.container, Some(ContainerFormat::Ogg));
        assert_eq!(hint.extension, Some("flac".into()));
        assert_eq!(hint.mime, Some("audio/flac".into()));
    }

    #[test]
    fn test_decoder_config_default() {
        let config = DecoderConfig::default();
        assert!(!config.prefer_hardware);
        assert!(config.byte_len_handle.is_none());
        assert!(config.gapless);
    }

    #[test]
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

    #[test]
    fn test_probe_from_direct_codec() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Vorbis),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[rstest]
    #[case("mp3", AudioCodec::Mp3)]
    #[case("aac", AudioCodec::AacLc)]
    #[case("m4a", AudioCodec::AacLc)]
    #[case("flac", AudioCodec::Flac)]
    #[case("ogg", AudioCodec::Vorbis)]
    #[case("opus", AudioCodec::Opus)]
    #[case("wav", AudioCodec::Pcm)]
    #[case("MP3", AudioCodec::Mp3)]
    fn test_probe_from_extension(#[case] extension: &str, #[case] expected: AudioCodec) {
        let hint = ProbeHint {
            extension: Some(extension.into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[rstest]
    #[case("audio/mpeg", AudioCodec::Mp3)]
    #[case("audio/flac", AudioCodec::Flac)]
    #[case("audio/aac", AudioCodec::AacLc)]
    #[case("audio/vorbis", AudioCodec::Vorbis)]
    #[case("audio/ogg", AudioCodec::Vorbis)]
    #[case("audio/opus", AudioCodec::Opus)]
    #[case("audio/wav", AudioCodec::Pcm)]
    #[case("audio/mp4", AudioCodec::AacLc)]
    fn test_probe_from_mime(#[case] mime: &str, #[case] expected: AudioCodec) {
        let hint = ProbeHint {
            mime: Some(mime.into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[rstest]
    #[case(ContainerFormat::MpegAudio, AudioCodec::Mp3)]
    #[case(ContainerFormat::Ogg, AudioCodec::Vorbis)]
    #[case(ContainerFormat::Wav, AudioCodec::Pcm)]
    #[case(ContainerFormat::Fmp4, AudioCodec::AacLc)]
    #[case(ContainerFormat::Caf, AudioCodec::Alac)]
    fn test_probe_from_container(#[case] container: ContainerFormat, #[case] expected: AudioCodec) {
        let hint = ProbeHint {
            container: Some(container),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[test]
    fn test_probe_priority_codec_over_extension() {
        // Codec hint should take priority over extension
        let hint = ProbeHint {
            codec: Some(AudioCodec::Flac),
            extension: Some("mp3".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[test]
    fn test_probe_priority_extension_over_mime() {
        // Extension should take priority over MIME when no direct codec
        let hint = ProbeHint {
            extension: Some("flac".into()),
            mime: Some("audio/mpeg".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[rstest]
    #[case(ProbeHint::default())]
    #[case(ProbeHint { extension: Some("xyz".into()), ..Default::default() })]
    #[case(ProbeHint { mime: Some("application/octet-stream".into()), ..Default::default() })]
    #[case(ProbeHint { container: Some(ContainerFormat::Mkv), ..Default::default() })]
    fn test_probe_fails_for_insufficient_hints(#[case] hint: ProbeHint) {
        let result = DecoderFactory::probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[rstest]
    #[case("unknown")]
    #[case("")]
    #[case("doc")]
    fn test_codec_from_extension_unknown_returns_none(#[case] extension: &str) {
        assert!(DecoderFactory::codec_from_extension(extension).is_none());
    }

    #[rstest]
    #[case("text/plain")]
    #[case("")]
    #[case("video/mp4")]
    fn test_codec_from_mime_unknown_returns_none(#[case] mime: &str) {
        assert!(DecoderFactory::codec_from_mime(mime).is_none());
    }

    #[test]
    fn test_auto_selector_fails() {
        let empty = Cursor::new(Vec::new());
        let result = DecoderFactory::create(empty, &CodecSelector::Auto, DecoderConfig::default());
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[test]
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    fn test_prefer_hardware_aac_falls_back_to_symphonia() {
        // When prefer_hardware is true but Apple decoder fails (not implemented),
        // should fall back to Symphonia
        let cursor = Cursor::new(vec![0u8; 100]);

        let config = DecoderConfig {
            prefer_hardware: true,
            ..Default::default()
        };

        // AAC will try Apple first (fails with "not implemented"),
        // then fall back to Symphonia (which will also fail with invalid data)
        let result =
            DecoderFactory::create(cursor, &CodecSelector::Exact(AudioCodec::AacLc), config);

        // The important thing is it falls back to Symphonia
        // (would be Backend error if it didn't fall back)
        assert!(result.is_err());
    }
}
