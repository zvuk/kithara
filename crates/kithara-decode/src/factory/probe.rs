use std::io::{Read, Seek, SeekFrom};

use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, id3v2_tag_len};

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

/// Position the encoded input at its first audio/container byte.
pub(crate) fn skip_id3_tags(source: &mut BoxedSource) -> DecodeResult<u64> {
    let mut offset = 0u64;
    loop {
        source.seek(SeekFrom::Start(offset))?;
        let mut prefix = [0; 10];
        source.read_exact(&mut prefix)?;
        if !prefix.starts_with(b"ID3") {
            source.seek(SeekFrom::Start(offset))?;
            return Ok(offset);
        }
        let len = id3v2_tag_len(&prefix).ok_or(DecodeError::InvalidData {
            detail: "invalid ID3v2 header",
        })?;
        offset = offset.checked_add(len).ok_or(DecodeError::InvalidData {
            detail: "ID3v2 extent overflow",
        })?;
    }
}

pub(super) fn sniff_wav_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    let position = source.stream_position()?;
    let result = read_wav_codec(source);
    source.seek(SeekFrom::Start(position))?;
    result
}

fn read_wav_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    let range = wav_chunk_range(source, *b"fmt ")?;
    let size = range.end - range.start;
    if size < 16 {
        return Err(DecodeError::ProbeFailed);
    }
    let mut fmt = [0; 40];
    let len = usize::try_from(size.min(fmt.len() as u64)).map_err(|_| DecodeError::ProbeFailed)?;
    source.read_exact(&mut fmt[..len])?;
    let tag = u16::from_le_bytes([fmt[0], fmt[1]]);
    let tag = if tag == 0xfffe {
        if size < 40
            || u16::from_le_bytes([fmt[16], fmt[17]]) < 22
            || u64::from(u16::from_le_bytes([fmt[16], fmt[17]])) + 18 > size
            || fmt[26..40] != [0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]
        {
            return Err(DecodeError::ProbeFailed);
        }
        u16::from_le_bytes([fmt[24], fmt[25]])
    } else {
        tag
    };
    match tag {
        1 | 3 => Ok(AudioCodec::Pcm),
        0x55 => Ok(AudioCodec::Mp3),
        2 | 0x11 => Ok(AudioCodec::Adpcm),
        _ => Err(DecodeError::ProbeFailed),
    }
}

pub(super) fn wav_data_range(source: &mut BoxedSource) -> DecodeResult<std::ops::Range<u64>> {
    wav_chunk_range(source, *b"data")
}

/// Locate a RIFF chunk without crossing the declared container extent.
fn wav_chunk_range(source: &mut BoxedSource, id: [u8; 4]) -> DecodeResult<std::ops::Range<u64>> {
    let origin = skip_id3_tags(source)?;
    let mut header = [0; 12];
    source.read_exact(&mut header)?;
    if &header[..4] != b"RIFF" || &header[8..] != b"WAVE" {
        return Err(DecodeError::ProbeFailed);
    }
    let size = u32::from_le_bytes(
        header[4..8]
            .try_into()
            .map_err(|_| DecodeError::ProbeFailed)?,
    );
    let end = origin
        .checked_add(u64::from(size))
        .and_then(|end| end.checked_add(8))
        .ok_or(DecodeError::ProbeFailed)?;
    while source
        .stream_position()?
        .checked_add(8)
        .is_some_and(|next| next <= end)
    {
        let mut chunk = [0; 8];
        source.read_exact(&mut chunk)?;
        let size = u32::from_le_bytes(
            chunk[4..]
                .try_into()
                .map_err(|_| DecodeError::ProbeFailed)?,
        );
        let start = source.stream_position()?;
        let chunk_end = start
            .checked_add(u64::from(size))
            .filter(|chunk_end| *chunk_end <= end)
            .ok_or(DecodeError::ProbeFailed)?;
        if chunk[..4] == id {
            return Ok(start..chunk_end);
        }
        let next = chunk_end
            .checked_add(u64::from(size % 2))
            .filter(|next| *next <= end)
            .ok_or(DecodeError::ProbeFailed)?;
        source.seek(SeekFrom::Start(next))?;
    }
    Err(DecodeError::ProbeFailed)
}

/// Read the identification packet at the beginning of an Ogg stream.
pub(super) fn sniff_ogg_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    let position = source.stream_position()?;
    let result = read_ogg_codec(source);
    source.seek(SeekFrom::Start(position))?;
    result
}

fn read_ogg_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    source.seek(SeekFrom::Start(0))?;
    let mut header = [0; 27];
    source.read_exact(&mut header)?;
    if &header[..4] != b"OggS" || header[4] != 0 || header[5] & 3 != 2 {
        return Err(DecodeError::ProbeFailed);
    }
    let segments = usize::from(header[26]);
    let mut lacing = [0; 255];
    source.read_exact(&mut lacing[..segments])?;
    if segments == 0 || lacing[0] < 8 {
        return Err(DecodeError::ProbeFailed);
    }
    let mut packet = [0; 8];
    source.read_exact(&mut packet)?;
    match &packet {
        b"OpusHead" => Ok(AudioCodec::Opus),
        [1, b'v', b'o', b'r', b'b', b'i', b's', ..] => Ok(AudioCodec::Vorbis),
        _ => Err(DecodeError::ProbeFailed),
    }
}

/// CAF's description chunk identifies the codec independently of the extension.
pub(super) fn sniff_caf_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    let position = source.stream_position()?;
    let result = read_caf_codec(source);
    source.seek(SeekFrom::Start(position))?;
    result
}

fn read_caf_codec(source: &mut BoxedSource) -> DecodeResult<AudioCodec> {
    source.seek(SeekFrom::Start(0))?;
    let mut header = [0; 8];
    source.read_exact(&mut header)?;
    if &header[..4] != b"caff" || header[4..6] != [0, 1] {
        return Err(DecodeError::ProbeFailed);
    }
    loop {
        let mut chunk = [0; 12];
        source.read_exact(&mut chunk)?;
        let size = i64::from_be_bytes(
            chunk[4..12]
                .try_into()
                .map_err(|_| DecodeError::ProbeFailed)?,
        );
        if size < 0 {
            return Err(DecodeError::ProbeFailed);
        }
        if &chunk[..4] == b"desc" {
            if size != 32 {
                return Err(DecodeError::ProbeFailed);
            }
            let mut description = [0; 32];
            source.read_exact(&mut description)?;
            return match &description[8..12] {
                b"alac" => Ok(AudioCodec::Alac),
                b"lpcm" => Ok(AudioCodec::Pcm),
                _ => Err(DecodeError::ProbeFailed),
            };
        }
        source.seek(SeekFrom::Current(size))?;
    }
}

/// Non-fatal byte sniff for inputs that genuinely arrive without container
/// metadata; HLS should normally supply this through `MediaInfo`.
pub(super) fn sniff_container_from_source(source: &mut BoxedSource) -> Option<ContainerFormat> {
    let position = source.stream_position().ok()?;
    let container = read_container_from_source(source);
    source.seek(SeekFrom::Start(position)).ok()?;
    container
}

fn read_container_from_source(source: &mut BoxedSource) -> Option<ContainerFormat> {
    const PREFIX_LEN: usize = 12;

    if source.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }

    let mut prefix = [0; PREFIX_LEN];
    let mut read = source.read(&mut prefix).ok()?;
    while prefix[..read].starts_with(b"ID3") {
        let skip = id3v2_tag_len(&prefix[..read])?;
        let offset = source
            .stream_position()
            .ok()?
            .checked_sub(u64::try_from(read).ok()?)?;
        source
            .seek(SeekFrom::Start(offset.checked_add(skip)?))
            .ok()?;
        read = source.read(&mut prefix).ok()?;
    }
    if source.seek(SeekFrom::Start(0)).is_err() {
        return None;
    }

    sniff_container_from_prefix(&prefix[..read], source)
}

fn sniff_container_from_prefix(prefix: &[u8], source: &mut BoxedSource) -> Option<ContainerFormat> {
    if is_mp4_prefix(prefix) {
        return sniff_mp4_fragmented(&mut **source).map(|fragmented| {
            if fragmented {
                ContainerFormat::Fmp4
            } else {
                ContainerFormat::Mp4
            }
        });
    }
    MediaInfo::try_from(prefix)
        .ok()
        .and_then(|info| info.container)
}

fn is_mp4_prefix(prefix: &[u8]) -> bool {
    prefix
        .get(4..8)
        .is_some_and(|kind| kind == b"ftyp" || kind == b"styp")
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
                .and_then(|ext| AudioCodec::parse_extension(ext))
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

pub(super) fn container_from_mime(mime: &str) -> Option<ContainerFormat> {
    ContainerFormat::parse_mime(mime)
}

/// Map an MP4 `stsd` sample-entry tag to a codec. The `.m4a`/`.mp4`
/// extension only narrows the container to MP4; the codec lives in the
/// sample entry, so a sniffed tag disambiguates AAC vs ALAC vs FLAC.
/// `mp4a` covers every AAC profile (AOT lives in the `esds`).
pub(super) const fn codec_from_mp4_fourcc(fourcc: [u8; 4]) -> Option<AudioCodec> {
    match &fourcc {
        b"mp4a" => Some(AudioCodec::AacLc),
        b"fLaC" => Some(AudioCodec::Flac),
        b"alac" => Some(AudioCodec::Alac),
        _ => None,
    }
}

/// Infer likely codec from container format.
pub(super) const fn codec_from_container(container: ContainerFormat) -> Option<AudioCodec> {
    match container {
        ContainerFormat::MpegAudio => Some(AudioCodec::Mp3),
        ContainerFormat::Adts | ContainerFormat::MpegTs => Some(AudioCodec::AacLc),
        ContainerFormat::Flac => Some(AudioCodec::Flac),
        ContainerFormat::Ape => Some(AudioCodec::Ape),
        ContainerFormat::Aiff => Some(AudioCodec::Pcm),
        ContainerFormat::Wav
        | ContainerFormat::Mkv
        | ContainerFormat::Mp4
        | ContainerFormat::Fmp4
        | ContainerFormat::Ogg
        | ContainerFormat::Caf => None,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::io::{Cursor, Seek};

    use kithara_test_fixtures::unit_fixtures::{aac_init, aac_segment};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::traits::BoxedSource;

    #[kithara::test]
    #[case(b"ID3\x04\0\0\x80\0\0\0".as_slice(), None)]
    #[case(b"ID3\x04\0\0\0\0\0\0fLaC".as_slice(), Some(ContainerFormat::Flac))]
    fn container_probe_restores_position_after_tagged_input(
        #[case] bytes: &[u8],
        #[case] expected: Option<ContainerFormat>,
    ) {
        let mut source: BoxedSource = Box::new(Cursor::new(bytes.to_vec()));
        source
            .seek(SeekFrom::Start(3))
            .expect("set caller position");
        assert_eq!(sniff_container_from_source(&mut source), expected);
        assert_eq!(source.stream_position().expect("caller position"), 3);
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
    fn sniff_container_detects_hls_fmp4_init_and_segment(aac_init: Vec<u8>, aac_segment: Vec<u8>) {
        let mut init_source: BoxedSource = Box::new(Cursor::new(aac_init));
        assert_eq!(
            sniff_container_from_source(&mut init_source),
            Some(ContainerFormat::Fmp4)
        );
        assert_eq!(init_source.stream_position().expect("source position"), 0);

        let mut segment_source: BoxedSource = Box::new(Cursor::new(aac_segment));
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
    #[case::pcm(1, true, Some(AudioCodec::Pcm))]
    #[case::float(3, true, Some(AudioCodec::Pcm))]
    #[case::mpeg(0x55, true, Some(AudioCodec::Mp3))]
    #[case::unsupported(0x1234, true, None)]
    #[case::foreign_guid(1, false, None)]
    fn extensible_wave_codec_comes_from_its_subformat(
        #[case] tag: u32,
        #[case] standard_guid: bool,
        #[case] expected: Option<AudioCodec>,
    ) {
        let mut fmt = vec![0; 40];
        fmt[..2].copy_from_slice(&0xfffe_u16.to_le_bytes());
        fmt[16..18].copy_from_slice(&22_u16.to_le_bytes());
        fmt[24..28].copy_from_slice(&tag.to_le_bytes());
        fmt[28..].copy_from_slice(&[0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        if !standard_guid {
            fmt[39] = 0;
        }
        let mut wav = b"RIFF".to_vec();
        wav.extend_from_slice(&52_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&40_u32.to_le_bytes());
        wav.extend_from_slice(&fmt);
        let mut source: BoxedSource = Box::new(Cursor::new(wav));
        assert_eq!(sniff_wav_codec(&mut source).ok(), expected);
        assert_eq!(source.stream_position().expect("position restored"), 0);
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
    #[case("flac", AudioCodec::Flac)]
    #[case("opus", AudioCodec::Opus)]
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
    #[case("audio/opus", AudioCodec::Opus)]
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
    #[case(ProbeHint { container: Some(ContainerFormat::Mp4), ..Default::default() })]
    #[case(ProbeHint { container: Some(ContainerFormat::Ogg), ..Default::default() })]
    #[case(ProbeHint { container: Some(ContainerFormat::Wav), ..Default::default() })]
    #[case(ProbeHint { mime: Some("audio/wav".into()), ..Default::default() })]
    #[case(ProbeHint::default())]
    #[case(ProbeHint { extension: Some("xyz".into()), ..Default::default() })]
    #[case(ProbeHint { mime: Some("application/octet-stream".into()), ..Default::default() })]
    #[case(ProbeHint { container: Some(ContainerFormat::Mkv), ..Default::default() })]
    fn test_probe_fails_for_insufficient_hints(#[case] hint: ProbeHint) {
        let result = probe_codec(&hint);
        assert!(matches!(result, Err(DecodeError::ProbeFailed)));
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
