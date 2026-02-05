//! Factory for creating decoders with runtime codec selection.
//!
//! This module provides [`DecoderFactory`] which creates [`AudioDecoder`] instances
//! based on runtime codec information, handling probing and backend selection.
//!
//! # Example
//!
//! ```ignore
//! use kithara_decode::factory::{DecoderFactory, CodecSelector, DecoderConfig};
//! use kithara_stream::AudioCodec;
//!
//! let file = std::fs::File::open("audio.mp3")?;
//! let decoder = DecoderFactory::create(
//!     file,
//!     CodecSelector::Exact(AudioCodec::Mp3),
//!     DecoderConfig::default(),
//! )?;
//! ```

// Types are not yet exported but will be used in later tasks
#![allow(dead_code)]

use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{
    error::{DecodeError, DecodeResult},
    symphonia::{
        Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
    },
    traits::{Alac, AudioDecoder},
};

/// Selector for choosing how to detect/specify the codec.
#[derive(Debug, Clone)]
pub enum CodecSelector {
    /// Known codec - no probing needed.
    Exact(AudioCodec),
    /// Probe with hints.
    Probe(ProbeHint),
    /// Full auto-probe.
    Auto,
}

/// Hints for codec probing.
#[derive(Debug, Clone, Default)]
pub struct ProbeHint {
    /// Known codec (highest priority).
    pub codec: Option<AudioCodec>,
    /// Container format hint.
    pub container: Option<ContainerFormat>,
    /// File extension hint (e.g., "mp3", "aac").
    pub extension: Option<String>,
    /// MIME type hint (e.g., "audio/mpeg", "audio/flac").
    pub mime: Option<String>,
}

/// Configuration for DecoderFactory.
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Prefer hardware decoder when available.
    pub prefer_hardware: bool,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Enable gapless playback.
    pub gapless: bool,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self {
            prefer_hardware: false,
            byte_len_handle: None,
            gapless: true,
        }
    }
}

/// Factory for creating decoders with runtime backend selection.
///
/// Currently only supports Symphonia backend. Future versions may add
/// platform-specific hardware decoders (Apple AudioToolbox, Android MediaCodec).
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
    /// A boxed decoder implementing [`AudioDecoder`].
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::ProbeFailed`] if codec cannot be determined.
    /// Returns [`DecodeError::UnsupportedCodec`] if the codec is not supported.
    pub fn create<R>(
        source: R,
        selector: CodecSelector,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn AudioDecoder<Config = SymphoniaConfig>>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        // Determine codec from selector
        let codec = match selector {
            CodecSelector::Exact(c) => c,
            CodecSelector::Probe(hint) => Self::probe_codec(&hint)?,
            CodecSelector::Auto => return Err(DecodeError::ProbeFailed),
        };

        // Build Symphonia config from DecoderConfig
        let symphonia_config = SymphoniaConfig {
            verify: false,
            gapless: config.gapless,
            byte_len_handle: config.byte_len_handle,
        };

        // Create appropriate decoder based on codec
        Self::create_symphonia_decoder(source, codec, symphonia_config)
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
            "wav" | "wave" => Some(AudioCodec::Pcm),
            "aiff" | "aif" => Some(AudioCodec::Pcm),
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
            ContainerFormat::Ogg => Some(AudioCodec::Vorbis),
            ContainerFormat::Wav => Some(AudioCodec::Pcm),
            ContainerFormat::Fmp4 | ContainerFormat::MpegTs => Some(AudioCodec::AacLc),
            ContainerFormat::Caf => Some(AudioCodec::Alac),
            ContainerFormat::Mkv => None, // Could be anything
        }
    }

    /// Create a Symphonia decoder for the given codec.
    fn create_symphonia_decoder<R>(
        source: R,
        codec: AudioCodec,
        config: SymphoniaConfig,
    ) -> DecodeResult<Box<dyn AudioDecoder<Config = SymphoniaConfig>>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
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
            AudioCodec::Opus => Err(DecodeError::UnsupportedCodec(codec)),
            AudioCodec::Pcm => {
                // Use PCM marker type for WAV files
                use crate::traits::CodecType;
                struct Pcm;
                impl CodecType for Pcm {
                    const CODEC: AudioCodec = AudioCodec::Pcm;
                }
                let decoder = Symphonia::<Pcm>::create(source, config)?;
                Ok(Box::new(decoder))
            }
            AudioCodec::Adpcm => Err(DecodeError::UnsupportedCodec(codec)),
        }
    }
}

#[cfg(test)]
mod tests {
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
        };
        assert!(config.prefer_hardware);
        assert!(config.byte_len_handle.is_some());
        assert!(!config.gapless);
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

    #[test]
    fn test_probe_from_extension_mp3() {
        let hint = ProbeHint {
            extension: Some("mp3".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Mp3);
    }

    #[test]
    fn test_probe_from_extension_aac() {
        let hint = ProbeHint {
            extension: Some("aac".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::AacLc);
    }

    #[test]
    fn test_probe_from_extension_m4a() {
        let hint = ProbeHint {
            extension: Some("m4a".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::AacLc);
    }

    #[test]
    fn test_probe_from_extension_flac() {
        let hint = ProbeHint {
            extension: Some("flac".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[test]
    fn test_probe_from_extension_ogg() {
        let hint = ProbeHint {
            extension: Some("ogg".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[test]
    fn test_probe_from_extension_opus() {
        let hint = ProbeHint {
            extension: Some("opus".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Opus);
    }

    #[test]
    fn test_probe_from_extension_wav() {
        let hint = ProbeHint {
            extension: Some("wav".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Pcm);
    }

    #[test]
    fn test_probe_from_extension_case_insensitive() {
        let hint = ProbeHint {
            extension: Some("MP3".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Mp3);
    }

    #[test]
    fn test_probe_from_mime_mpeg() {
        let hint = ProbeHint {
            mime: Some("audio/mpeg".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Mp3);
    }

    #[test]
    fn test_probe_from_mime_flac() {
        let hint = ProbeHint {
            mime: Some("audio/flac".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[test]
    fn test_probe_from_mime_aac() {
        let hint = ProbeHint {
            mime: Some("audio/aac".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::AacLc);
    }

    #[test]
    fn test_probe_from_mime_vorbis() {
        let hint = ProbeHint {
            mime: Some("audio/vorbis".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[test]
    fn test_probe_from_mime_ogg() {
        let hint = ProbeHint {
            mime: Some("audio/ogg".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[test]
    fn test_probe_from_mime_opus() {
        let hint = ProbeHint {
            mime: Some("audio/opus".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Opus);
    }

    #[test]
    fn test_probe_from_mime_wav() {
        let hint = ProbeHint {
            mime: Some("audio/wav".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Pcm);
    }

    #[test]
    fn test_probe_from_mime_mp4() {
        let hint = ProbeHint {
            mime: Some("audio/mp4".into()),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::AacLc);
    }

    #[test]
    fn test_probe_from_container_mpeg_audio() {
        let hint = ProbeHint {
            container: Some(ContainerFormat::MpegAudio),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Mp3);
    }

    #[test]
    fn test_probe_from_container_ogg() {
        let hint = ProbeHint {
            container: Some(ContainerFormat::Ogg),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[test]
    fn test_probe_from_container_wav() {
        let hint = ProbeHint {
            container: Some(ContainerFormat::Wav),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Pcm);
    }

    #[test]
    fn test_probe_from_container_fmp4() {
        let hint = ProbeHint {
            container: Some(ContainerFormat::Fmp4),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::AacLc);
    }

    #[test]
    fn test_probe_from_container_caf() {
        let hint = ProbeHint {
            container: Some(ContainerFormat::Caf),
            ..Default::default()
        };
        let codec = DecoderFactory::probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Alac);
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

    #[test]
    fn test_probe_fails_no_hints() {
        let hint = ProbeHint::default();
        let result = DecoderFactory::probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[test]
    fn test_probe_fails_unknown_extension() {
        let hint = ProbeHint {
            extension: Some("xyz".into()),
            ..Default::default()
        };
        let result = DecoderFactory::probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[test]
    fn test_probe_fails_unknown_mime() {
        let hint = ProbeHint {
            mime: Some("application/octet-stream".into()),
            ..Default::default()
        };
        let result = DecoderFactory::probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[test]
    fn test_probe_fails_mkv_container_alone() {
        // MKV can contain many codecs, so container alone isn't enough
        let hint = ProbeHint {
            container: Some(ContainerFormat::Mkv),
            ..Default::default()
        };
        let result = DecoderFactory::probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[test]
    fn test_codec_from_extension_unknown_returns_none() {
        assert!(DecoderFactory::codec_from_extension("unknown").is_none());
        assert!(DecoderFactory::codec_from_extension("").is_none());
        assert!(DecoderFactory::codec_from_extension("doc").is_none());
    }

    #[test]
    fn test_codec_from_mime_unknown_returns_none() {
        assert!(DecoderFactory::codec_from_mime("text/plain").is_none());
        assert!(DecoderFactory::codec_from_mime("").is_none());
        assert!(DecoderFactory::codec_from_mime("video/mp4").is_none());
    }

    #[test]
    fn test_auto_selector_fails() {
        use std::io::Cursor;
        let empty = Cursor::new(Vec::new());
        let result = DecoderFactory::create(empty, CodecSelector::Auto, DecoderConfig::default());
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }
}
