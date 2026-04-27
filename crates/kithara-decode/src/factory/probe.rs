//! Probe-hint type and codec-from-hint mappings.

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::error::{DecodeError, DecodeResult};

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
    /// Known codec (the highest priority).
    pub codec: Option<AudioCodec>,
    /// Container format hint.
    pub container: Option<ContainerFormat>,
    /// File extension hint (e.g., "mp3", "aac").
    pub extension: Option<String>,
    /// MIME type hint (e.g., "audio/mpeg", "audio/flac").
    pub mime: Option<String>,
}

/// Resolve `(codec, container)` from a selector, returning an error for `Auto`.
pub(super) fn resolve_codec_container(
    selector: &CodecSelector,
) -> DecodeResult<(AudioCodec, Option<ContainerFormat>)> {
    match *selector {
        CodecSelector::Exact(c) => Ok((c, None)),
        CodecSelector::Probe(ref hint) => Ok((probe_codec(hint)?, hint.container)),
        CodecSelector::Auto => Err(DecodeError::ProbeFailed),
    }
}

/// Probe codec from hints.
///
/// Priority:
/// 1. Direct codec hint
/// 2. Extension mapping
/// 3. MIME type mapping
/// 4. Container format hint (can suggest likely codec)
pub(super) fn probe_codec(hint: &ProbeHint) -> DecodeResult<AudioCodec> {
    if let Some(codec) = hint.codec {
        return Ok(codec);
    }

    if let Some(ref ext) = hint.extension
        && let Some(codec) = codec_from_extension(ext)
    {
        return Ok(codec);
    }

    if let Some(ref mime) = hint.mime {
        if let Some(codec) = AudioCodec::from_mime(mime) {
            return Ok(codec);
        }

        if let Some(container) = container_from_mime(mime)
            && let Some(codec) = codec_from_container(container)
        {
            return Ok(codec);
        }
    }

    if let Some(container) = hint.container
        && let Some(codec) = codec_from_container(container)
    {
        return Ok(codec);
    }

    Err(DecodeError::ProbeFailed)
}

/// Map file extension to codec.
pub(super) fn codec_from_extension(ext: &str) -> Option<AudioCodec> {
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

pub(super) fn container_from_extension(ext: &str) -> Option<ContainerFormat> {
    match ext.to_lowercase().as_str() {
        "mp3" => Some(ContainerFormat::MpegAudio),
        "aac" => Some(ContainerFormat::Adts),
        "m4a" | "mp4" => Some(ContainerFormat::Mp4),
        "flac" => Some(ContainerFormat::Flac),
        "ogg" | "oga" => Some(ContainerFormat::Ogg),
        "wav" | "wave" => Some(ContainerFormat::Wav),
        "caf" => Some(ContainerFormat::Caf),
        _ => None,
    }
}

pub(super) fn container_from_mime(mime: &str) -> Option<ContainerFormat> {
    let mime = mime.to_lowercase();

    match mime.as_str() {
        "audio/mpeg" => Some(ContainerFormat::MpegAudio),
        "audio/aac" | "audio/aacp" => Some(ContainerFormat::Adts),
        "audio/flac" => Some(ContainerFormat::Flac),
        "audio/ogg" => Some(ContainerFormat::Ogg),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some(ContainerFormat::Wav),
        "audio/mp4" | "audio/x-m4a" => Some(ContainerFormat::Mp4),
        _ => None,
    }
}

/// Infer likely codec from container format.
pub(super) fn codec_from_container(container: ContainerFormat) -> Option<AudioCodec> {
    match container {
        ContainerFormat::MpegAudio => Some(AudioCodec::Mp3),
        ContainerFormat::Adts
        | ContainerFormat::Mp4
        | ContainerFormat::Fmp4
        | ContainerFormat::MpegTs => Some(AudioCodec::AacLc),
        ContainerFormat::Flac => Some(AudioCodec::Flac),
        ContainerFormat::Ogg => Some(AudioCodec::Vorbis),
        ContainerFormat::Wav => Some(AudioCodec::Pcm),
        ContainerFormat::Caf => Some(AudioCodec::Alac),
        ContainerFormat::Mkv => None, // Could be anything
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    fn test_codec_selector_exact() {
        let selector = CodecSelector::Exact(AudioCodec::AacLc);
        assert!(matches!(selector, CodecSelector::Exact(AudioCodec::AacLc)));
    }

    #[kithara::test]
    fn test_codec_selector_probe() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Mp3),
            ..Default::default()
        };
        let selector = CodecSelector::Probe(hint);
        assert!(matches!(selector, CodecSelector::Probe(_)));
    }

    #[kithara::test]
    fn test_codec_selector_auto() {
        let selector = CodecSelector::Auto;
        assert!(matches!(selector, CodecSelector::Auto));
    }

    #[kithara::test]
    fn test_probe_hint_default() {
        let hint = ProbeHint::default();
        assert!(hint.codec.is_none());
        assert!(hint.container.is_none());
        assert!(hint.extension.is_none());
        assert!(hint.mime.is_none());
    }

    #[kithara::test]
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

    #[kithara::test]
    fn test_probe_from_direct_codec() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Vorbis),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[kithara::test]
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
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[kithara::test]
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
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[kithara::test]
    #[case(ContainerFormat::MpegAudio, AudioCodec::Mp3)]
    #[case(ContainerFormat::Ogg, AudioCodec::Vorbis)]
    #[case(ContainerFormat::Wav, AudioCodec::Pcm)]
    #[case(ContainerFormat::Mp4, AudioCodec::AacLc)]
    #[case(ContainerFormat::Fmp4, AudioCodec::AacLc)]
    #[case(ContainerFormat::Caf, AudioCodec::Alac)]
    fn test_probe_from_container(#[case] container: ContainerFormat, #[case] expected: AudioCodec) {
        let hint = ProbeHint {
            container: Some(container),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, expected);
    }

    #[kithara::test]
    fn test_probe_priority_codec_over_extension() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Flac),
            extension: Some("mp3".into()),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[kithara::test]
    fn test_probe_priority_extension_over_mime() {
        let hint = ProbeHint {
            extension: Some("flac".into()),
            mime: Some("audio/mpeg".into()),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[kithara::test]
    #[case(ProbeHint::default())]
    #[case(ProbeHint { extension: Some("xyz".into()), ..Default::default() })]
    #[case(ProbeHint { mime: Some("application/octet-stream".into()), ..Default::default() })]
    #[case(ProbeHint { container: Some(ContainerFormat::Mkv), ..Default::default() })]
    fn test_probe_fails_for_insufficient_hints(#[case] hint: ProbeHint) {
        let result = probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
    }

    #[kithara::test]
    #[case("unknown")]
    #[case("")]
    #[case("doc")]
    fn test_codec_from_extension_unknown_returns_none(#[case] extension: &str) {
        assert!(codec_from_extension(extension).is_none());
    }

    #[kithara::test]
    #[case("mp3", Some(ContainerFormat::MpegAudio))]
    #[case("aac", Some(ContainerFormat::Adts))]
    #[case("m4a", Some(ContainerFormat::Mp4))]
    #[case("mp4", Some(ContainerFormat::Mp4))]
    #[case("flac", Some(ContainerFormat::Flac))]
    #[case("wav", Some(ContainerFormat::Wav))]
    #[case("unknown", None)]
    fn test_container_from_extension(
        #[case] extension: &str,
        #[case] expected: Option<ContainerFormat>,
    ) {
        assert_eq!(container_from_extension(extension), expected);
    }

    #[kithara::test]
    #[case("audio/mpeg", Some(ContainerFormat::MpegAudio))]
    #[case("audio/aac", Some(ContainerFormat::Adts))]
    #[case("audio/mp4", Some(ContainerFormat::Mp4))]
    #[case("audio/x-m4a", Some(ContainerFormat::Mp4))]
    #[case("audio/flac", Some(ContainerFormat::Flac))]
    #[case("audio/ogg", Some(ContainerFormat::Ogg))]
    #[case("text/plain", None)]
    fn test_container_from_mime_case(
        #[case] mime: &str,
        #[case] expected: Option<ContainerFormat>,
    ) {
        assert_eq!(container_from_mime(mime), expected);
    }

    #[kithara::test]
    #[case("text/plain")]
    #[case("")]
    #[case("video/mp4")]
    fn test_codec_from_mime_unknown_returns_none(#[case] mime: &str) {
        assert!(AudioCodec::from_mime(mime).is_none());
    }
}
