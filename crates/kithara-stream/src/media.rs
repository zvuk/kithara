use bon::Builder;

/// Container format type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    /// Standard MP4 (ISO/IEC 14496-12)
    Mp4,
    /// Fragmented MP4 (fMP4) - common for HLS
    Fmp4,
    /// MPEG Transport Stream
    MpegTs,
    /// MPEG Audio (MP3 without container)
    MpegAudio,
    /// AAC ADTS (raw AAC with ADTS framing)
    Adts,
    /// FLAC (native FLAC stream)
    Flac,
    /// RIFF WAVE
    Wav,
    /// Audio Interchange File Format, including AIFF-C.
    Aiff,
    /// Monkey's Audio stream.
    Ape,
    /// Ogg container
    Ogg,
    /// CAF (Core Audio Format)
    Caf,
    /// Matroska/WebM
    Mkv,
}

/// Audio codec type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC Low Complexity (mp4a.40.2)
    AacLc,
    /// AAC High Efficiency (mp4a.40.5)
    AacHe,
    /// AAC HE v2 (mp4a.40.29)
    AacHeV2,
    /// MP3 (mp4a.40.34 or audio/mpeg)
    Mp3,
    /// FLAC
    Flac,
    /// Vorbis
    Vorbis,
    /// Opus
    Opus,
    /// ALAC (Apple Lossless)
    Alac,
    /// PCM
    Pcm,
    /// ADPCM
    Adpcm,
    /// Monkey's Audio lossless codec.
    Ape,
}

impl ContainerFormat {
    /// The container named by an HTTP content type.
    #[must_use]
    pub fn parse_mime(mime: &str) -> Option<Self> {
        match mime.to_ascii_lowercase().as_str() {
            "audio/mp4" | "audio/x-m4a" => Some(Self::Mp4),
            "audio/ogg" => Some(Self::Ogg),
            "audio/aac" | "audio/aacp" => Some(Self::Adts),
            "audio/mpeg" | "audio/mp3" => Some(Self::MpegAudio),
            "audio/flac" => Some(Self::Flac),
            "audio/wav" | "audio/wave" | "audio/x-wav" => Some(Self::Wav),
            "audio/x-caf" => Some(Self::Caf),
            "audio/aiff" | "audio/x-aiff" => Some(Self::Aiff),
            "audio/ape" | "audio/x-ape" => Some(Self::Ape),
            _ => None,
        }
    }

    /// The container a file extension names, matched case-insensitively.
    #[must_use]
    pub fn parse_extension(ext: &str) -> Option<Self> {
        const NAMED: &[(&str, ContainerFormat)] = &[
            ("mp3", ContainerFormat::MpegAudio),
            ("aac", ContainerFormat::Adts),
            ("m4a", ContainerFormat::Mp4),
            ("mp4", ContainerFormat::Mp4),
            ("flac", ContainerFormat::Flac),
            ("ogg", ContainerFormat::Ogg),
            ("oga", ContainerFormat::Ogg),
            ("opus", ContainerFormat::Ogg),
            ("wav", ContainerFormat::Wav),
            ("wave", ContainerFormat::Wav),
            ("aiff", ContainerFormat::Aiff),
            ("aif", ContainerFormat::Aiff),
            ("aifc", ContainerFormat::Aiff),
            ("ape", ContainerFormat::Ape),
            ("caf", ContainerFormat::Caf),
        ];
        NAMED
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ext))
            .map(|&(_, format)| format)
    }
}

/// Media format information.
///
/// This information can be derived from:
/// - HLS playlist `CODECS` attribute (e.g., `mp4a.40.2`)
/// - File extension
/// - HTTP Content-Type header
/// - Container metadata
#[derive(Debug, Clone, Default, PartialEq, Eq, Builder)]
#[builder(const)]
#[non_exhaustive]
pub struct MediaInfo {
    /// Number of audio channels
    pub channels: Option<u16>,
    /// Audio codec
    pub codec: Option<AudioCodec>,
    /// Container format (fMP4, MPEG-TS, etc.)
    pub container: Option<ContainerFormat>,
    /// Sample rate in Hz
    pub sample_rate: Option<u32>,
    /// Variant index (for ABR streams).
    /// Different variants have different init segments (ftyp/moov),
    /// so decoder must be recreated when variant changes.
    pub variant_index: Option<u32>,
}

impl TryFrom<&[u8]> for MediaInfo {
    type Error = CodecMagicError;

    /// Identify only facts carried by an encoded-media prefix.
    /// Container signatures do not identify the codec inside MP4, Ogg or CAF.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let container = match bytes {
            [b'O', b'g', b'g', b'S', ..] => Some(ContainerFormat::Ogg),
            [_, _, _, _, b'f', b't', b'y', b'p', ..] => Some(ContainerFormat::Mp4),
            [_, _, _, _, b's', b't', b'y', b'p', ..] => Some(ContainerFormat::Fmp4),
            [b'c', b'a', b'f', b'f', ..] => Some(ContainerFormat::Caf),
            [
                b'R',
                b'I',
                b'F',
                b'F',
                _,
                _,
                _,
                _,
                b'W',
                b'A',
                b'V',
                b'E',
                ..,
            ] => Some(ContainerFormat::Wav),
            [
                b'F',
                b'O',
                b'R',
                b'M',
                _,
                _,
                _,
                _,
                b'A',
                b'I',
                b'F',
                b'F' | b'C',
                ..,
            ] => {
                return Ok(Self::builder()
                    .codec(AudioCodec::Pcm)
                    .container(ContainerFormat::Aiff)
                    .build());
            }
            _ => None,
        };
        if let Some(container) = container {
            return Ok(Self::builder().container(container).build());
        }
        AudioCodec::try_from(bytes).map(|codec| {
            let mut info = Self::from(codec);
            if codec == AudioCodec::AacLc {
                info.container = Some(ContainerFormat::Adts);
            }
            info
        })
    }
}

impl MediaInfo {
    /// Parse codec **and** container from an HTTP `Content-Type` value.
    ///
    /// Distinct from [`AudioCodec::parse_mime`], which returns the codec
    /// only. Standalone HTTP file sources can lose container information
    /// if the caller drops it on the floor; downstream Apple/Android
    /// dispatch needs both codec and container to pick a backend.
    #[must_use]
    pub fn parse_mime(mime: &str) -> Option<Self> {
        let mime = mime.to_lowercase();
        let codec = AudioCodec::parse_normalized_mime(&mime);
        let container = ContainerFormat::parse_mime(&mime)
            .or_else(|| codec.and_then(|codec| ContainerFormat::try_from(codec).ok()));
        if codec.is_none() && container.is_none() {
            return None;
        }
        Some(
            Self::builder()
                .maybe_codec(codec)
                .maybe_container(container)
                .build(),
        )
    }
}

/// Whether a decoder path needs exact byte sizes before it can safely route
/// reads and seeks through a segmented stream.
///
/// AAC / FLAC in fMP4 is segment-aware: it reads by segment index and learns
/// final byte lengths from body commits, so startup does not need network size
/// probes. Unknown metadata and every file-like container stay conservative.
#[must_use]
pub const fn needs_exact_byte_sizes(
    codec: Option<AudioCodec>,
    container: Option<ContainerFormat>,
) -> bool {
    !matches!(
        (codec, container),
        (
            Some(AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 | AudioCodec::Flac),
            Some(ContainerFormat::Fmp4),
        )
    )
}

/// Build `MediaInfo` from a codec alone, filling the container when it is
/// implied by the codec for standalone (non-HLS) sources. AAC and Adpcm
/// have ambiguous containers and leave `container = None`.
impl From<AudioCodec> for MediaInfo {
    fn from(codec: AudioCodec) -> Self {
        Self::builder()
            .maybe_codec(Some(codec))
            .maybe_container(ContainerFormat::try_from(codec).ok())
            .build()
    }
}

/// The codec uniquely picks a container for standalone sources.
/// Mp3→MpegAudio, Pcm→Wav, Flac→Flac, Vorbis/Opus→Ogg, Alac→Caf.
/// AAC (ADTS vs Mp4) and Adpcm are ambiguous and fail.
impl TryFrom<AudioCodec> for ContainerFormat {
    type Error = AmbiguousContainer;

    fn try_from(codec: AudioCodec) -> Result<Self, Self::Error> {
        match codec {
            AudioCodec::Mp3 => Ok(Self::MpegAudio),
            AudioCodec::Pcm => Ok(Self::Wav),
            AudioCodec::Flac => Ok(Self::Flac),
            AudioCodec::Ape => Ok(Self::Ape),
            AudioCodec::Vorbis | AudioCodec::Opus => Ok(Self::Ogg),
            AudioCodec::Alac => Ok(Self::Caf),
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 | AudioCodec::Adpcm => {
                Err(AmbiguousContainer(codec))
            }
        }
    }
}

/// Returned by `TryFrom<AudioCodec> for ContainerFormat` when the codec
/// alone is not enough to determine the container (AAC, Adpcm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("ambiguous container for codec: {0:?}")]
pub struct AmbiguousContainer(pub AudioCodec);

impl AudioCodec {
    /// Encoder-side priming silence in PCM frames added by mainstream
    /// encoders for `codec` when no container or encoder tag declares
    /// an explicit count. Used as a fallback by the gapless pipeline
    /// when probing yields no metadata.
    ///
    /// Does **not** include any decoder-side algorithmic delay — that
    /// is per-backend (LAME-convention `mpa` decoders add 529 for MP3,
    /// Apple's `AudioConverter` internally compensates and adds 0) and
    /// lives on the `FrameCodec` trait in `kithara-decode`.
    ///
    /// Free-standing (`AudioCodec::encoder_priming_frames(codec)`)
    /// rather than `codec.encoder_priming_frames()` so that the
    /// `match codec { ... }` body does not pretend to be a
    /// `From<AudioCodec>` conversion — `u64` here means "priming
    /// frames", not the codec rewritten as an integer.
    #[must_use]
    pub const fn encoder_priming_frames(codec: Self) -> u64 {
        match codec {
            Self::AacLc | Self::AacHe | Self::AacHeV2 => 1024,
            Self::Mp3 => 576,
            Self::Opus => 312,
            Self::Flac | Self::Vorbis | Self::Alac | Self::Pcm | Self::Adpcm | Self::Ape => 0,
        }
    }

    /// The codec a file extension names, matched case-insensitively.
    #[must_use]
    pub fn parse_extension(ext: &str) -> Option<Self> {
        const NAMED: &[(&str, AudioCodec)] = &[
            ("mp3", AudioCodec::Mp3),
            ("aac", AudioCodec::AacLc),
            ("flac", AudioCodec::Flac),
            ("ape", AudioCodec::Ape),
            ("opus", AudioCodec::Opus),
            ("aiff", AudioCodec::Pcm),
            ("aif", AudioCodec::Pcm),
            ("aifc", AudioCodec::Pcm),
        ];
        NAMED
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ext))
            .map(|&(_, codec)| codec)
    }

    /// Parse from HLS CODECS attribute value.
    ///
    /// Examples:
    /// - `mp4a.40.2` -> `AacLc`
    /// - `mp4a.40.5` -> `AacHe`
    /// - `mp4a.40.29` -> `AacHeV2`
    /// - `mp4a.40.34` -> `Mp3`
    /// - `mp4a.69` or `mp4a.6B` -> `Mp3`
    #[must_use]
    pub fn parse_hls_codec(codec: &str) -> Option<Self> {
        const PREFIXES: &[(&str, AudioCodec)] = &[
            ("mp4a.40.29", AudioCodec::AacHeV2),
            ("mp4a.40.34", AudioCodec::Mp3),
            ("mp4a.40.5", AudioCodec::AacHe),
            ("mp4a.40.2", AudioCodec::AacLc),
            ("mp4a.69", AudioCodec::Mp3),
            ("mp4a.6b", AudioCodec::Mp3),
            ("flac", AudioCodec::Flac),
            ("vorbis", AudioCodec::Vorbis),
            ("opus", AudioCodec::Opus),
            ("alac", AudioCodec::Alac),
        ];

        let codec_lower = codec.to_lowercase();
        PREFIXES
            .iter()
            .find_map(|&(prefix, codec)| codec_lower.starts_with(prefix).then_some(codec))
    }

    /// Parse codec from HTTP Content-Type header value.
    ///
    /// Examples:
    /// - `audio/mpeg` -> `Mp3`
    /// - `audio/aac` -> `AacLc`
    /// - `audio/flac` -> `Flac`
    #[must_use]
    pub fn parse_mime(mime: &str) -> Option<Self> {
        let m = mime.to_lowercase();
        Self::parse_normalized_mime(&m)
    }

    fn parse_normalized_mime(m: &str) -> Option<Self> {
        [
            (m.contains("mp3") || m == "audio/mpeg", Self::Mp3),
            (m.contains("aac"), Self::AacLc),
            (m.contains("flac"), Self::Flac),
            (matches!(m, "audio/ape" | "audio/x-ape"), Self::Ape),
            (m.contains("vorbis"), Self::Vorbis),
            (m.contains("opus"), Self::Opus),
        ]
        .into_iter()
        .find_map(|(matches, codec)| matches.then_some(codec))
    }

    /// Whether padding meets audio inside a transform window, leaving the
    /// frame next to a trim tapered rather than exact.
    #[must_use]
    pub const fn transform_padded(codec: Self) -> bool {
        match codec {
            Self::AacLc | Self::AacHe | Self::AacHeV2 | Self::Mp3 | Self::Vorbis | Self::Opus => {
                true
            }
            Self::Flac | Self::Alac | Self::Pcm | Self::Adpcm | Self::Ape => false,
        }
    }
}

/// Error returned by [`TryFrom<&[u8]> for AudioCodec`] when the magic
/// prefix can't be classified.
///
/// Used on cache hits — when the original HTTP `Content-Type` header is
/// no longer available and the URL path carries no extension hint
/// (`streamhq?id=N`) — to recover the codec from the bytes that were
/// already persisted on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodecMagicError {
    /// Buffer is shorter than 4 bytes — not enough to hold any of the
    /// known magic sequences.
    #[error("magic prefix needs at least 4 bytes, got {got}")]
    TooShort {
        /// Length of the supplied buffer in bytes.
        got: usize,
    },
    /// The first bytes did not match any codec we can identify by magic.
    /// Callers should surface this as a probe failure rather than
    /// guessing.
    #[error("magic prefix did not match any known codec")]
    Unknown,
}

impl TryFrom<&[u8]> for AudioCodec {
    type Error = CodecMagicError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        match bytes {
            b if b.len() < 4 => Err(CodecMagicError::TooShort { got: b.len() }),
            [b'f', b'L', b'a', b'C', ..] => Ok(Self::Flac),
            [b'M', b'A', b'C', b' ', ..] => Ok(Self::Ape),
            [0xFF, b1, ..] if (b1 & 0xE0) == 0xE0 => match (b1 >> 1) & 0b11 {
                0b00 => Ok(Self::AacLc),
                _ => Ok(Self::Mp3),
            },
            _ => Err(CodecMagicError::Unknown),
        }
    }
}

/// Byte length of a validated `ID3v2` tag, including its header and optional footer.
/// The caller reads the media prefix at this offset without retaining the tag body.
#[must_use]
pub fn id3v2_tag_len(header: &[u8]) -> Option<u64> {
    let header = header.get(..10)?;
    if &header[..3] != b"ID3" || !(2..=4).contains(&header[3]) || header[4] == 0xff {
        return None;
    }
    let mut size = 0_u64;
    for byte in &header[6..10] {
        if byte & 0x80 != 0 {
            return None;
        }
        size = (size << 7) | u64::from(*byte);
    }
    let footer = u64::from(header[3] == 4 && header[5] & 0x10 != 0) * 10;
    Some(10 + size + footer)
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test]
    #[case(AudioCodec::AacLc, true, "AAC-LC pads inside its MDCT window")]
    #[case(AudioCodec::AacHe, true, "AAC-HE pads inside its MDCT window")]
    #[case(AudioCodec::AacHeV2, true, "AAC-HE v2 pads inside its MDCT window")]
    #[case(AudioCodec::Mp3, true, "MP3 pads inside its MDCT window")]
    #[case(AudioCodec::Vorbis, true, "Vorbis pads inside its MDCT window")]
    #[case(AudioCodec::Opus, true, "Opus pads inside its MDCT window")]
    #[case(AudioCodec::Flac, false, "FLAC carries every frame exactly")]
    #[case(AudioCodec::Alac, false, "ALAC carries every frame exactly")]
    #[case(AudioCodec::Pcm, false, "PCM carries every frame exactly")]
    #[case(AudioCodec::Adpcm, false, "ADPCM carries every frame exactly")]
    fn transform_padding_follows_the_codec_family(
        #[case] codec: AudioCodec,
        #[case] expected: bool,
        #[case] label: &str,
    ) {
        assert_eq!(AudioCodec::transform_padded(codec), expected, "{label}");
    }

    #[kithara::test]
    #[case("mp4a.40.2", Some(AudioCodec::AacLc), "AAC-LC standard")]
    #[case("MP4A.40.2", Some(AudioCodec::AacLc), "AAC-LC uppercase")]
    #[case("mp4a.40.5", Some(AudioCodec::AacHe), "AAC-HE")]
    #[case("mp4a.40.29", Some(AudioCodec::AacHeV2), "AAC-HE v2")]
    #[case("mp4a.40.34", Some(AudioCodec::Mp3), "MP3 via mp4a.40.34")]
    #[case("mp4a.69", Some(AudioCodec::Mp3), "MP3 via mp4a.69")]
    #[case("mp4a.6B", Some(AudioCodec::Mp3), "MP3 via mp4a.6B uppercase")]
    #[case("mp4a.6b", Some(AudioCodec::Mp3), "MP3 via mp4a.6b")]
    #[case("flac", Some(AudioCodec::Flac), "FLAC lowercase")]
    #[case("FLAC", Some(AudioCodec::Flac), "FLAC uppercase")]
    #[case("fLaC", Some(AudioCodec::Flac), "FLAC mixed case")]
    #[case("vorbis", Some(AudioCodec::Vorbis), "Vorbis")]
    #[case("opus", Some(AudioCodec::Opus), "Opus")]
    #[case("alac", Some(AudioCodec::Alac), "ALAC")]
    #[case("unknown", None, "Unknown codec")]
    #[case("", None, "Empty string")]
    #[case("mp4a", None, "Incomplete codec string")]
    fn test_hls_codec_parsing(
        #[case] codec_str: &str,
        #[case] expected: Option<AudioCodec>,
        #[case] _description: &str,
    ) {
        assert_eq!(AudioCodec::parse_hls_codec(codec_str), expected);
    }

    #[kithara::test]
    fn test_media_info_default() {
        let info = MediaInfo::default();
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[kithara::test]
    fn fmp4_aac_and_flac_do_not_need_exact_byte_sizes() {
        for codec in [
            AudioCodec::AacLc,
            AudioCodec::AacHe,
            AudioCodec::AacHeV2,
            AudioCodec::Flac,
        ] {
            assert!(!needs_exact_byte_sizes(
                Some(codec),
                Some(ContainerFormat::Fmp4)
            ));
        }
    }

    #[kithara::test]
    fn unknown_or_file_like_media_needs_exact_byte_sizes() {
        assert!(needs_exact_byte_sizes(None, Some(ContainerFormat::Fmp4)));
        assert!(needs_exact_byte_sizes(
            Some(AudioCodec::Pcm),
            Some(ContainerFormat::Aiff)
        ));
        assert!(needs_exact_byte_sizes(
            Some(AudioCodec::Pcm),
            Some(ContainerFormat::Wav)
        ));
        assert!(needs_exact_byte_sizes(
            Some(AudioCodec::AacLc),
            Some(ContainerFormat::Adts)
        ));
    }

    #[kithara::test]
    #[case(ContainerFormat::Fmp4)]
    #[case(ContainerFormat::MpegTs)]
    #[case(ContainerFormat::MpegAudio)]
    #[case(ContainerFormat::Adts)]
    #[case(ContainerFormat::Flac)]
    #[case(ContainerFormat::Wav)]
    #[case(ContainerFormat::Ogg)]
    #[case(ContainerFormat::Caf)]
    #[case(ContainerFormat::Mkv)]
    fn test_media_info_with_container(#[case] container: ContainerFormat) {
        let info = MediaInfo::builder().container(container).build();
        assert_eq!(info.container, Some(container));
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[kithara::test]
    #[case(44100)]
    #[case(48000)]
    #[case(88200)]
    #[case(96000)]
    #[case(192000)]
    fn test_media_info_with_sample_rate(#[case] sample_rate: u32) {
        let info = MediaInfo::builder().sample_rate(sample_rate).build();
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, Some(sample_rate));
        assert_eq!(info.channels, None);
    }

    #[kithara::test]
    #[case(1)]
    #[case(2)]
    #[case(6)]
    #[case(8)]
    fn test_media_info_with_channels(#[case] channels: u16) {
        let info = MediaInfo::builder().channels(channels).build();
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, Some(channels));
    }

    #[kithara::test]
    fn test_media_info_builder_chain() {
        let mut info = MediaInfo::builder()
            .container(ContainerFormat::Fmp4)
            .sample_rate(44100)
            .channels(2)
            .build();
        info.codec = Some(AudioCodec::AacLc);

        assert_eq!(info.container, Some(ContainerFormat::Fmp4));
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.sample_rate, Some(44100));
        assert_eq!(info.channels, Some(2));
    }

    #[kithara::test]
    fn test_media_info_partial_builder() {
        let mut info = MediaInfo::builder().sample_rate(48000).build();
        info.codec = Some(AudioCodec::Mp3);

        assert_eq!(info.container, None);
        assert_eq!(info.codec, Some(AudioCodec::Mp3));
        assert_eq!(info.sample_rate, Some(48000));
        assert_eq!(info.channels, None);
    }

    #[kithara::test]
    fn test_container_format_debug() {
        let format = ContainerFormat::Fmp4;
        let debug_str = format!("{:?}", format);
        assert!(debug_str.contains("Fmp4"));
    }

    #[kithara::test]
    fn test_audio_codec_debug() {
        let codec = AudioCodec::AacLc;
        let debug_str = format!("{:?}", codec);
        assert!(debug_str.contains("AacLc"));
    }

    #[kithara::test]
    fn test_media_info_clone() {
        let mut info = MediaInfo::builder()
            .container(ContainerFormat::Fmp4)
            .build();
        info.codec = Some(AudioCodec::AacLc);

        let cloned = info.clone();
        assert_eq!(info, cloned);
    }

    #[kithara::test]
    fn test_media_info_partial_eq() {
        let info1 = MediaInfo {
            codec: Some(AudioCodec::AacLc),
            ..Default::default()
        };
        let info2 = MediaInfo {
            codec: Some(AudioCodec::AacLc),
            ..Default::default()
        };
        let info3 = MediaInfo {
            codec: Some(AudioCodec::Mp3),
            ..Default::default()
        };

        assert_eq!(info1, info2);
        assert_ne!(info1, info3);
    }

    #[kithara::test]
    #[case::mpeg_sync_layer3(&[0xFF, 0xFB, 0x90, 0x44], AudioCodec::Mp3)]
    #[case::aac_adts_sync(&[0xFF, 0xF1, 0x50, 0x80, 0x00, 0x1F, 0xFC], AudioCodec::AacLc)]
    #[case::flac(b"fLaC\x00\x00\x00\x22", AudioCodec::Flac)]
    fn try_from_recognises_known_magic(#[case] bytes: &[u8], #[case] expected: AudioCodec) {
        assert_eq!(AudioCodec::try_from(bytes), Ok(expected));
    }

    #[kithara::test]
    #[case(b"OggS\x00\x02\x00\x00", ContainerFormat::Ogg)]
    #[case(b"\x00\x00\x00\x20ftypisom", ContainerFormat::Mp4)]
    #[case(b"caff\x00\x01\x00\x00", ContainerFormat::Caf)]
    #[case(b"RIFF\x24\x08\x00\x00WAVEfmt ", ContainerFormat::Wav)]
    fn container_magic_does_not_invent_a_codec(
        #[case] bytes: &[u8],
        #[case] container: ContainerFormat,
    ) {
        let info = MediaInfo::try_from(bytes).expect("container signature");
        assert_eq!(info.container, Some(container));
        assert_eq!(info.codec, None);
        assert!(AudioCodec::try_from(bytes).is_err());
    }

    #[kithara::test]
    fn id3_metadata_does_not_identify_audio() {
        assert_eq!(
            MediaInfo::try_from(&b"ID3\x04\x00\x00\x00\x00\x00\x00"[..]).ok(),
            None
        );
    }

    #[kithara::test]
    #[case(AudioCodec::AacLc, 1024, "AAC frames prime a full transform window")]
    #[case(AudioCodec::AacHe, 1024, "AAC-HE primes the same window as AAC-LC")]
    #[case(
        AudioCodec::AacHeV2,
        1024,
        "AAC-HE v2 primes the same window as AAC-LC"
    )]
    #[case(AudioCodec::Mp3, 576, "MP3 primes half of its 1152-frame granule pair")]
    #[case(AudioCodec::Opus, 312, "Opus primes its pre-skip default")]
    #[case(AudioCodec::Flac, 0, "FLAC starts at the first frame it carries")]
    #[case(AudioCodec::Vorbis, 0, "Vorbis carries its priming in the container")]
    #[case(AudioCodec::Alac, 0, "ALAC starts at the first frame it carries")]
    #[case(AudioCodec::Pcm, 0, "PCM has nothing to prime")]
    #[case(AudioCodec::Adpcm, 0, "ADPCM has nothing to prime")]
    fn encoder_priming_is_the_codec_family_default(
        #[case] codec: AudioCodec,
        #[case] expected: u64,
        #[case] label: &str,
    ) {
        assert_eq!(
            AudioCodec::encoder_priming_frames(codec),
            expected,
            "{label}"
        );
    }

    #[kithara::test]
    #[case(
        "audio/mpeg",
        Some(AudioCodec::Mp3),
        "the MP3 mime carries no codec name"
    )]
    #[case("audio/mp3", Some(AudioCodec::Mp3), "the codec name alone is enough")]
    #[case(
        "AUDIO/MPEG",
        Some(AudioCodec::Mp3),
        "the mime is matched case-insensitively"
    )]
    #[case("audio/aac", Some(AudioCodec::AacLc), "an AAC mime is AAC-LC")]
    #[case("audio/flac", Some(AudioCodec::Flac), "a FLAC mime is FLAC")]
    #[case("audio/vorbis", Some(AudioCodec::Vorbis), "a Vorbis mime is Vorbis")]
    #[case("audio/opus", Some(AudioCodec::Opus), "an Opus mime is Opus")]
    #[case("audio/ogg", None, "an Ogg mime names only the container")]
    #[case("audio/wav", None, "a WAV mime names only the container")]
    #[case("audio/wave", None, "the WAVE spelling names only the container")]
    #[case("audio/x-wav", None, "the x- spelling names only the container")]
    #[case("audio/mp4", None, "an MP4 mime names only the container")]
    #[case("audio/x-m4a", None, "the m4a spelling names only the container")]
    #[case("audio/basic", None, "an unknown mime names no codec")]
    #[case("", None, "an empty mime names no codec")]
    fn mime_parsing_names_the_codec(
        #[case] mime: &str,
        #[case] expected: Option<AudioCodec>,
        #[case] label: &str,
    ) {
        assert_eq!(AudioCodec::parse_mime(mime), expected, "{label}");
    }

    #[kithara::test]
    #[case(
        "audio/mp4",
        ContainerFormat::Mp4,
        "an MP4 mime keeps its own container"
    )]
    #[case("audio/x-m4a", ContainerFormat::Mp4, "the m4a spelling is MP4 too")]
    #[case("audio/aac", ContainerFormat::Adts, "a bare AAC mime is ADTS")]
    #[case("audio/aacp", ContainerFormat::Adts, "the aacp spelling is ADTS too")]
    #[case(
        "audio/mpeg",
        ContainerFormat::MpegAudio,
        "the container follows the codec"
    )]
    #[case("audio/flac", ContainerFormat::Flac, "FLAC implies its own container")]
    #[case("audio/wav", ContainerFormat::Wav, "PCM implies WAV")]
    fn a_mime_carries_both_the_codec_and_its_container(
        #[case] mime: &str,
        #[case] expected: ContainerFormat,
        #[case] label: &str,
    ) {
        let info = MediaInfo::parse_mime(mime).expect("a known mime parses");
        assert_eq!(info.container, Some(expected), "{label}");
        assert_eq!(
            info.codec,
            AudioCodec::parse_mime(mime),
            "both entry points agree on the codec"
        );
    }

    #[kithara::test]
    fn an_unknown_mime_carries_no_media_info() {
        assert_eq!(MediaInfo::parse_mime("audio/basic"), None);
    }

    #[kithara::test]
    #[case(
        AudioCodec::Mp3,
        Some(ContainerFormat::MpegAudio),
        "MP3 implies MPEG audio"
    )]
    #[case(AudioCodec::Pcm, Some(ContainerFormat::Wav), "PCM implies WAV")]
    #[case(
        AudioCodec::Flac,
        Some(ContainerFormat::Flac),
        "FLAC implies its own container"
    )]
    #[case(AudioCodec::Vorbis, Some(ContainerFormat::Ogg), "Vorbis implies Ogg")]
    #[case(AudioCodec::Opus, Some(ContainerFormat::Ogg), "Opus implies Ogg")]
    #[case(AudioCodec::Alac, Some(ContainerFormat::Caf), "ALAC implies CAF")]
    #[case(AudioCodec::AacLc, None, "AAC is ADTS or MP4, so the codec cannot say")]
    #[case(AudioCodec::AacHe, None, "AAC-HE is just as ambiguous")]
    #[case(AudioCodec::AacHeV2, None, "AAC-HE v2 is just as ambiguous")]
    #[case(AudioCodec::Adpcm, None, "ADPCM rides several containers")]
    fn a_codec_alone_fills_the_container_only_when_it_implies_one(
        #[case] codec: AudioCodec,
        #[case] expected: Option<ContainerFormat>,
        #[case] label: &str,
    ) {
        let info = MediaInfo::from(codec);
        assert_eq!(info.codec, Some(codec), "the codec is carried through");
        assert_eq!(info.container, expected, "{label}");
    }

    #[kithara::test]
    #[case("mp3", Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio))]
    #[case("aac", Some(AudioCodec::AacLc), Some(ContainerFormat::Adts))]
    #[case("m4a", None, Some(ContainerFormat::Mp4))]
    #[case("mp4", None, Some(ContainerFormat::Mp4))]
    #[case("flac", Some(AudioCodec::Flac), Some(ContainerFormat::Flac))]
    #[case("ogg", None, Some(ContainerFormat::Ogg))]
    #[case("oga", None, Some(ContainerFormat::Ogg))]
    #[case("opus", Some(AudioCodec::Opus), Some(ContainerFormat::Ogg))]
    #[case("wav", None, Some(ContainerFormat::Wav))]
    #[case("wave", None, Some(ContainerFormat::Wav))]
    #[case("aiff", Some(AudioCodec::Pcm), Some(ContainerFormat::Aiff))]
    #[case("aif", Some(AudioCodec::Pcm), Some(ContainerFormat::Aiff))]
    #[case("caf", None, Some(ContainerFormat::Caf))]
    #[case("MP3", Some(AudioCodec::Mp3), Some(ContainerFormat::MpegAudio))]
    #[case("Flac", Some(AudioCodec::Flac), Some(ContainerFormat::Flac))]
    #[case("M4A", None, Some(ContainerFormat::Mp4))]
    #[case("txt", None, None)]
    #[case("doc", None, None)]
    #[case("unknown", None, None)]
    #[case("", None, None)]
    fn an_extension_names_its_codec_and_container(
        #[case] extension: &str,
        #[case] codec: Option<AudioCodec>,
        #[case] container: Option<ContainerFormat>,
    ) {
        assert_eq!(AudioCodec::parse_extension(extension), codec);
        assert_eq!(ContainerFormat::parse_extension(extension), container);
    }

    #[kithara::test]
    fn try_from_rejects_short_buffer() {
        assert_eq!(
            AudioCodec::try_from(&b"ID"[..]),
            Err(CodecMagicError::TooShort { got: 2 })
        );
    }

    #[kithara::test]
    #[case::random(&[0x00, 0x01, 0x02, 0x03])]
    #[case::almost_riff_no_wave(b"RIFF\x00\x00\x00\x00XXXX____")]
    #[case::sync_byte_alone(&[0xFE, 0xFB, 0x00, 0x00])]
    #[case::sync_word_without_framing(&[0xFF, 0x00, 0x00, 0x00])]
    fn try_from_unknown_magic_errors(#[case] bytes: &[u8]) {
        assert_eq!(AudioCodec::try_from(bytes), Err(CodecMagicError::Unknown));
    }
}
