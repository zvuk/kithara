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
}

/// Media format information.
///
/// This information can be derived from:
/// - HLS playlist `CODECS` attribute (e.g., `mp4a.40.2`)
/// - File extension
/// - HTTP Content-Type header
/// - Container metadata
#[derive(Debug, Clone, Default, PartialEq, Eq, Builder)]
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

impl MediaInfo {
    /// Create `MediaInfo` with optional codec and container.
    #[must_use]
    pub fn new(codec: Option<AudioCodec>, container: Option<ContainerFormat>) -> Self {
        Self {
            codec,
            container,
            channels: None,
            sample_rate: None,
            variant_index: None,
        }
    }

    /// Parse codec **and** container from an HTTP `Content-Type` value.
    ///
    /// Distinct from [`AudioCodec::parse_mime`], which returns the codec
    /// only. Standalone HTTP file sources can lose container information
    /// if the caller drops it on the floor; downstream Apple/Android
    /// dispatch needs both codec and container to pick a backend.
    #[must_use]
    pub fn parse_mime(mime: &str) -> Option<Self> {
        let codec = AudioCodec::parse_mime(mime)?;
        let container = match mime.to_lowercase().as_str() {
            "audio/mp4" | "audio/x-m4a" => Some(ContainerFormat::Mp4),
            "audio/aac" | "audio/aacp" => Some(ContainerFormat::Adts),
            _ => ContainerFormat::try_from(codec).ok(),
        };
        Some(Self::new(Some(codec), container))
    }
}

/// Build `MediaInfo` from a codec alone, filling the container when it is
/// implied by the codec for standalone (non-HLS) sources. AAC and Adpcm
/// have ambiguous containers and leave `container = None`.
impl From<AudioCodec> for MediaInfo {
    fn from(codec: AudioCodec) -> Self {
        Self::new(Some(codec), ContainerFormat::try_from(codec).ok())
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
    pub fn encoder_priming_frames(codec: Self) -> u64 {
        match codec {
            Self::AacLc | Self::AacHe | Self::AacHeV2 => 1024,
            Self::Mp3 => 576,
            Self::Opus => 312,
            Self::Flac | Self::Vorbis | Self::Alac | Self::Pcm | Self::Adpcm => 0,
        }
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
        [
            (m.contains("mp3") || m == "audio/mpeg", Self::Mp3),
            (m.contains("aac"), Self::AacLc),
            (m.contains("flac"), Self::Flac),
            (m.contains("vorbis"), Self::Vorbis),
            (m.contains("opus"), Self::Opus),
            (m == "audio/ogg", Self::Vorbis),
            (
                matches!(m.as_str(), "audio/wav" | "audio/wave" | "audio/x-wav"),
                Self::Pcm,
            ),
            (
                matches!(m.as_str(), "audio/mp4" | "audio/x-m4a"),
                Self::AacLc,
            ),
        ]
        .into_iter()
        .find_map(|(matches, codec)| matches.then_some(codec))
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
            [b'I', b'D', b'3', ..] => Ok(Self::Mp3),
            [b'f', b'L', b'a', b'C', ..] => Ok(Self::Flac),
            [b'O', b'g', b'g', b'S', ..] => Ok(Self::Vorbis),
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
            ] => Ok(Self::Pcm),
            [_, _, _, _, b'f', b't', b'y', b'p', ..] => Ok(Self::AacLc),
            [0xFF, b1, ..] if (b1 & 0xE0) == 0xE0 => match (b1 >> 1) & 0b11 {
                0b00 => Ok(Self::AacLc),
                _ => Ok(Self::Mp3),
            },
            _ => Err(CodecMagicError::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(wasm)]
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

    #[kithara::test(wasm)]
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

    #[kithara::test(wasm)]
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

    #[kithara::test(wasm)]
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
    #[case::id3v2(
        b"ID3\x04\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00",
        AudioCodec::Mp3
    )]
    #[case::mpeg_sync_layer3(&[0xFF, 0xFB, 0x90, 0x44], AudioCodec::Mp3)]
    #[case::aac_adts_sync(&[0xFF, 0xF1, 0x50, 0x80, 0x00, 0x1F, 0xFC], AudioCodec::AacLc)]
    #[case::flac(b"fLaC\x00\x00\x00\x22", AudioCodec::Flac)]
    #[case::ogg(b"OggS\x00\x02\x00\x00", AudioCodec::Vorbis)]
    #[case::wav(b"RIFF\x24\x08\x00\x00WAVEfmt ", AudioCodec::Pcm)]
    #[case::mp4(b"\x00\x00\x00\x20ftypisom", AudioCodec::AacLc)]
    fn try_from_recognises_known_magic(#[case] bytes: &[u8], #[case] expected: AudioCodec) {
        assert_eq!(AudioCodec::try_from(bytes), Ok(expected));
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
    fn try_from_unknown_magic_errors(#[case] bytes: &[u8]) {
        assert_eq!(AudioCodec::try_from(bytes), Err(CodecMagicError::Unknown));
    }
}
