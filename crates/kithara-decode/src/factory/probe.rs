use std::io::SeekFrom;

use kithara_stream::{AudioCodec, ContainerFormat};

use crate::{
    error::{DecodeError, DecodeResult},
    mp4::{sniff_mp4_codec, sniff_mp4_fragmented},
    traits::DecoderInput,
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

/// Completes `hint` against the bytes, then resolves `(codec, container)`.
///
/// One function because there is one question: a hint built from a file
/// extension and one built from `MediaInfo` are indistinguishable by the time
/// a decoder has to be chosen, and both entry points reach here.
///
/// Order matters. The container is sniffed *before* the codec is resolved,
/// because the codec's last resort is the container it sits in — resolving the
/// codec first means a source whose extension says nothing fails while its
/// first twelve bytes name the format outright.
pub(super) fn resolve_against_source(
    hint: &ProbeHint,
    source: &mut dyn DecoderInput,
) -> DecodeResult<(AudioCodec, Option<ContainerFormat>)> {
    let mut hint = hint.clone();
    if hint.container.is_none() {
        hint.container = sniff_container_from_source(source);
    }
    // MP4/M4A is container-only — AAC, ALAC and FLAC all live there — so the
    // sample-entry tag is what picks the backend.
    if hint.codec.is_none()
        && matches!(
            hint.container,
            Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
        )
    {
        hint.codec = sniff_mp4_codec(source).and_then(codec_from_mp4_fourcc);
        if source.seek(SeekFrom::Start(0)).is_err() {
            return Err(DecodeError::ProbeFailed);
        }
    }
    Ok((probe_codec(&hint)?, hint.container))
}

/// Non-fatal byte sniff for inputs that genuinely arrive without container
/// metadata; HLS should normally supply this through `MediaInfo`.
pub(super) fn sniff_container_from_source(
    source: &mut dyn DecoderInput,
) -> Option<ContainerFormat> {
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

fn sniff_container_from_prefix(
    prefix: &[u8],
    source: &mut dyn DecoderInput,
) -> Option<ContainerFormat> {
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
        return sniff_mp4_fragmented(source).map(|fragmented| {
            if fragmented {
                ContainerFormat::Fmp4
            } else {
                ContainerFormat::Mp4
            }
        });
    }
    if prefix.starts_with(b"ID3") {
        return Some(ContainerFormat::MpegAudio);
    }
    mpeg_sync_container(prefix)
}

fn is_mp4_prefix(prefix: &[u8]) -> bool {
    prefix
        .get(4..8)
        .is_some_and(|kind| kind == b"ftyp" || kind == b"styp")
}

/// What an MPEG sync word introduces, if anything.
///
/// One function because there is one header. ADTS and MPEG audio share their
/// leading sync bits, so they cannot be recognised independently: asking two
/// questions in sequence makes the answer depend on which is asked first, and
/// the ordinary MP3 frame satisfies both. The layer field is the discriminator
/// — ADTS syncs on twelve bits and its layer is `00` by specification, MPEG
/// audio syncs on eleven and always names Layer I, II or III. Testing the sync
/// alone claims every common MP3 frame (`0xFB` is MPEG-1 Layer III) and hands
/// it to an AAC decoder.
fn mpeg_sync_container(prefix: &[u8]) -> Option<ContainerFormat> {
    /// Low sync bits every member of the family asserts.
    const SYNC: u8 = 0b1110_0000;
    /// The twelfth sync bit, which only ADTS asserts.
    const ADTS_SYNC: u8 = 0b1111_0000;
    /// Layer field: zero for ADTS, a named layer for MPEG audio.
    const LAYER: u8 = 0b0000_0110;

    let [0xff, second, ..] = prefix else {
        return None;
    };
    if second & SYNC != SYNC {
        return None;
    }
    if second & LAYER == 0 {
        (second & ADTS_SYNC == ADTS_SYNC).then_some(ContainerFormat::Adts)
    } else {
        Some(ContainerFormat::MpegAudio)
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

    /// A source that arrives with no usable extension still names its own
    /// format in its first bytes, and that has to be enough.
    ///
    /// This is why the container is sniffed *before* the codec is resolved:
    /// the codec's last resort is the container it sits in, so resolving the
    /// codec against hints alone refuses the source before anyone looks at it.
    #[kithara::test]
    fn a_source_with_no_extension_is_identified_by_its_bytes() {
        // An MPEG audio frame header: sync word plus MPEG-1 Layer III.
        let mut source: BoxedSource = Box::new(Cursor::new(vec![0xff, 0xfb, 0x90, 0x00]));

        let (codec, container) = resolve_against_source(&ProbeHint::default(), &mut *source)
            .expect("invariant: the frame header identifies the format");

        assert_eq!(codec, AudioCodec::Mp3);
        assert_eq!(container, Some(ContainerFormat::MpegAudio));
    }

    /// ADTS and MPEG audio share a sync word; the layer field is the only
    /// thing that separates them, and getting it wrong hands every common MP3
    /// frame to an AAC decoder.
    /// 0xFB is MPEG-1 Layer III — the ordinary MP3 header — and it satisfies
    /// a twelve-bit sync, so a sync-only test hands it to an AAC decoder.
    #[kithara::test]
    fn an_mpeg_layer_three_frame_reads_as_mpeg_audio() {
        assert_eq!(
            mpeg_sync_container(&[0xff, 0xfb]),
            Some(ContainerFormat::MpegAudio)
        );
    }

    /// 0xF1 is MPEG-4 AAC with the layer field zeroed, as ADTS requires.
    #[kithara::test]
    fn an_adts_frame_reads_as_adts() {
        assert_eq!(
            mpeg_sync_container(&[0xff, 0xf1]),
            Some(ContainerFormat::Adts)
        );
    }

    /// An eleven-bit sync with a zero layer is neither: ADTS needs the twelfth
    /// bit and MPEG audio needs a layer.
    #[kithara::test]
    fn a_sync_word_naming_no_layer_and_no_adts_is_neither() {
        assert_eq!(mpeg_sync_container(&[0xff, 0xe1]), None);
    }

    /// An ID3 tag in front of the audio names the same thing.
    #[kithara::test]
    fn an_id3_tagged_source_is_identified_by_its_bytes() {
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x00".to_vec();
        tagged.extend_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
        let mut source: BoxedSource = Box::new(Cursor::new(tagged));

        let (codec, _) = resolve_against_source(&ProbeHint::default(), &mut *source)
            .expect("invariant: the tag identifies the format");

        assert_eq!(codec, AudioCodec::Mp3);
    }

    /// Bytes that name nothing are still refused: the sniff is a resort, not a
    /// guess.
    #[kithara::test]
    fn a_source_that_names_nothing_is_still_refused() {
        let mut source: BoxedSource = Box::new(Cursor::new(vec![0x00; 64]));

        assert!(matches!(
            resolve_against_source(&ProbeHint::default(), &mut *source),
            Err(DecodeError::ProbeFailed)
        ));
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
