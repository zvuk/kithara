use std::io::{Read, Seek, SeekFrom};

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{
    error::{DecodeError, DecodeResult},
    mp4::sniff_mp4_fragmented,
    traits::BoxedSource,
};

/// Hints for codec probing.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProbeHint {
    /// Known codec (the highest priority).
    pub(crate) codec: Option<AudioCodec>,
    /// Container format hint.
    pub(crate) container: Option<ContainerFormat>,
    /// File extension hint (e.g., "mp3", "aac").
    pub(crate) extension: Option<String>,
    /// MIME type hint (e.g., "audio/mpeg", "audio/flac").
    pub(crate) mime: Option<String>,
}

/// Resolve `(codec, container)` from a probe hint.
pub(super) fn resolve_codec_container(
    hint: &ProbeHint,
) -> DecodeResult<(AudioCodec, Option<ContainerFormat>)> {
    Ok((probe_codec(hint)?, hint.container))
}

/// Non-fatal byte sniff for inputs that genuinely arrive without container
/// metadata; HLS should normally supply this through `MediaInfo`.
pub(super) fn sniff_container_from_source(source: &mut BoxedSource) -> Option<ContainerFormat> {
    const PREFIX_LEN: usize = 12;

    if source.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }

    let mut prefix = [0; PREFIX_LEN];
    let read = source.read(&mut prefix).ok()?;
    if source.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }

    let container = sniff_container_from_prefix(&prefix[..read], source);
    if source.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }
    container
}

fn sniff_container_from_prefix(prefix: &[u8], source: &mut BoxedSource) -> Option<ContainerFormat> {
    if prefix.starts_with(b"fLaC") {
        return Some(ContainerFormat::Flac);
    }
    if prefix.len() >= 12 && prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WAVE") {
        return Some(ContainerFormat::Wav);
    }
    if prefix.starts_with(b"OggS") {
        return Some(ContainerFormat::Ogg);
    }
    if is_mp4_prefix(prefix) {
        return sniff_mp4_fragmented(&mut **source).map(|fragmented| {
            if fragmented {
                ContainerFormat::Fmp4
            } else {
                ContainerFormat::Mp4
            }
        });
    }
    if is_adts_sync(prefix) {
        return Some(ContainerFormat::Adts);
    }
    if prefix.starts_with(b"ID3") || is_mp3_sync(prefix) {
        return Some(ContainerFormat::MpegAudio);
    }
    None
}

fn is_mp4_prefix(prefix: &[u8]) -> bool {
    prefix
        .get(4..8)
        .is_some_and(|kind| kind == b"ftyp" || kind == b"styp")
}

fn is_adts_sync(prefix: &[u8]) -> bool {
    prefix.len() >= 2 && prefix[0] == 0xff && (prefix[1] & 0xf0) == 0xf0
}

fn is_mp3_sync(prefix: &[u8]) -> bool {
    prefix.len() >= 2 && prefix[0] == 0xff && (prefix[1] & 0xe0) == 0xe0
}

/// Probe codec from hints.
///
/// Priority:
/// 1. Direct codec hint
/// 2. Extension mapping
/// 3. MIME type mapping
/// 4. Container format hint (can suggest likely codec)
pub(super) fn probe_codec(hint: &ProbeHint) -> DecodeResult<AudioCodec> {
    hint.codec
        .or_else(|| {
            hint.extension
                .as_ref()
                .and_then(|ext| codec_from_extension(ext))
        })
        .or_else(|| {
            hint.mime
                .as_ref()
                .and_then(|mime| AudioCodec::parse_mime(mime))
        })
        .or_else(|| {
            hint.mime
                .as_ref()
                .and_then(|mime| container_from_mime(mime))
                .and_then(codec_from_container)
        })
        .or_else(|| hint.container.and_then(codec_from_container))
        .ok_or(DecodeError::ProbeFailed)
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

/// Map an MP4 `stsd` sample-entry tag to a codec. The `.m4a`/`.mp4`
/// extension only narrows the container to MP4; the codec lives in the
/// sample entry, so a sniffed tag disambiguates AAC vs ALAC vs FLAC.
/// `mp4a` covers every AAC profile (AOT lives in the `esds`).
pub(super) fn codec_from_mp4_fourcc(fourcc: [u8; 4]) -> Option<AudioCodec> {
    match &fourcc {
        b"mp4a" => Some(AudioCodec::AacLc),
        b"fLaC" => Some(AudioCodec::Flac),
        b"alac" => Some(AudioCodec::Alac),
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
        ContainerFormat::Mkv => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Seek};

    use kithara_test_utils::kithara;

    use super::*;
    use crate::traits::BoxedSource;

    fn hls_fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/hls")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
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
    fn sniff_container_detects_hls_fmp4_init_and_segment() {
        let mut init_source: BoxedSource = Box::new(Cursor::new(hls_fixture("init-slq-a1.mp4")));
        assert_eq!(
            sniff_container_from_source(&mut init_source),
            Some(ContainerFormat::Fmp4)
        );
        assert_eq!(init_source.stream_position().expect("source position"), 0);

        let mut segment_source: BoxedSource =
            Box::new(Cursor::new(hls_fixture("segment-1-slq-a1.m4s")));
        assert_eq!(
            sniff_container_from_source(&mut segment_source),
            Some(ContainerFormat::Fmp4)
        );
        assert_eq!(
            segment_source.stream_position().expect("source position"),
            0
        );
    }

    #[kithara::test]
    fn test_probe_from_direct_codec() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Vorbis),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
        assert_eq!(codec, AudioCodec::Vorbis);
    }

    #[kithara::test]
    #[case(*b"mp4a", Some(AudioCodec::AacLc))]
    #[case(*b"fLaC", Some(AudioCodec::Flac))]
    #[case(*b"alac", Some(AudioCodec::Alac))]
    #[case(*b"avc1", None)]
    fn test_codec_from_mp4_fourcc(#[case] fourcc: [u8; 4], #[case] expected: Option<AudioCodec>) {
        assert_eq!(codec_from_mp4_fourcc(fourcc), expected);
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
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
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
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
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
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
        assert_eq!(codec, expected);
    }

    #[kithara::test]
    fn test_probe_priority_codec_over_extension() {
        let hint = ProbeHint {
            codec: Some(AudioCodec::Flac),
            extension: Some("mp3".into()),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
        assert_eq!(codec, AudioCodec::Flac);
    }

    #[kithara::test]
    fn test_probe_priority_extension_over_mime() {
        let hint = ProbeHint {
            extension: Some("flac".into()),
            mime: Some("audio/mpeg".into()),
            ..Default::default()
        };
        let codec = probe_codec(&hint).expect("BUG: should probe successfully");
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
        assert!(AudioCodec::parse_mime(mime).is_none());
    }
}
