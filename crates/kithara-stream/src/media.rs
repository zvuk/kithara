//! Media format information types.
//!
//! These types allow sources to communicate codec/container information
//! to decoders, enabling direct decoder instantiation without probing.

/// Container format type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaInfo {
    /// Container format (fMP4, MPEG-TS, etc.)
    pub container: Option<ContainerFormat>,
    /// Audio codec
    pub codec: Option<AudioCodec>,
    /// Sample rate in Hz
    pub sample_rate: Option<u32>,
    /// Number of audio channels
    pub channels: Option<u16>,
    /// Variant index (for ABR streams).
    /// Different variants have different init segments (ftyp/moov),
    /// so decoder must be recreated when variant changes.
    pub variant_index: Option<u32>,
}

impl MediaInfo {
    /// Create `MediaInfo` with optional codec and container.
    pub fn new(codec: Option<AudioCodec>, container: Option<ContainerFormat>) -> Self {
        Self {
            container,
            codec,
            sample_rate: None,
            channels: None,
            variant_index: None,
        }
    }

    /// Set container format.
    pub fn with_container(mut self, container: ContainerFormat) -> Self {
        self.container = Some(container);
        self
    }

    /// Set audio codec.
    pub fn with_codec(mut self, codec: AudioCodec) -> Self {
        self.codec = Some(codec);
        self
    }

    /// Set sample rate.
    pub fn with_sample_rate(mut self, sample_rate: u32) -> Self {
        self.sample_rate = Some(sample_rate);
        self
    }

    /// Set channel count.
    pub fn with_channels(mut self, channels: u16) -> Self {
        self.channels = Some(channels);
        self
    }

    /// Set variant index (for ABR streams).
    pub fn with_variant_index(mut self, variant_index: u32) -> Self {
        self.variant_index = Some(variant_index);
        self
    }
}

impl AudioCodec {
    /// Parse from HLS CODECS attribute value.
    ///
    /// Examples:
    /// - `mp4a.40.2` -> `AacLc`
    /// - `mp4a.40.5` -> `AacHe`
    /// - `mp4a.40.29` -> `AacHeV2`
    /// - `mp4a.40.34` -> `Mp3`
    /// - `mp4a.69` or `mp4a.6B` -> `Mp3`
    pub fn from_hls_codec(codec: &str) -> Option<Self> {
        let codec_lower = codec.to_lowercase();

        // Check longer prefixes first to avoid false matches
        if codec_lower.starts_with("mp4a.40.29") {
            Some(Self::AacHeV2)
        } else if codec_lower.starts_with("mp4a.40.34") {
            Some(Self::Mp3)
        } else if codec_lower.starts_with("mp4a.40.5") {
            Some(Self::AacHe)
        } else if codec_lower.starts_with("mp4a.40.2") {
            Some(Self::AacLc)
        } else if codec_lower.starts_with("mp4a.69") || codec_lower.starts_with("mp4a.6b") {
            Some(Self::Mp3)
        } else if codec_lower.starts_with("flac") || codec_lower.starts_with("fLaC") {
            Some(Self::Flac)
        } else if codec_lower.starts_with("vorbis") {
            Some(Self::Vorbis)
        } else if codec_lower.starts_with("opus") {
            Some(Self::Opus)
        } else if codec_lower.starts_with("alac") {
            Some(Self::Alac)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
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
        assert_eq!(AudioCodec::from_hls_codec(codec_str), expected);
    }

    #[test]
    fn test_media_info_new() {
        let info = MediaInfo::default();
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[test]
    fn test_media_info_default() {
        let info = MediaInfo::default();
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[rstest]
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
        let info = MediaInfo::default().with_container(container);
        assert_eq!(info.container, Some(container));
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[rstest]
    #[case(AudioCodec::AacLc)]
    #[case(AudioCodec::AacHe)]
    #[case(AudioCodec::AacHeV2)]
    #[case(AudioCodec::Mp3)]
    #[case(AudioCodec::Flac)]
    #[case(AudioCodec::Vorbis)]
    #[case(AudioCodec::Opus)]
    #[case(AudioCodec::Alac)]
    #[case(AudioCodec::Pcm)]
    #[case(AudioCodec::Adpcm)]
    fn test_media_info_with_codec(#[case] codec: AudioCodec) {
        let info = MediaInfo::default().with_codec(codec);
        assert_eq!(info.container, None);
        assert_eq!(info.codec, Some(codec));
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, None);
    }

    #[rstest]
    #[case(44100)]
    #[case(48000)]
    #[case(88200)]
    #[case(96000)]
    #[case(192000)]
    fn test_media_info_with_sample_rate(#[case] sample_rate: u32) {
        let info = MediaInfo::default().with_sample_rate(sample_rate);
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, Some(sample_rate));
        assert_eq!(info.channels, None);
    }

    #[rstest]
    #[case(1)] // Mono
    #[case(2)] // Stereo
    #[case(6)] // 5.1 surround
    #[case(8)] // 7.1 surround
    fn test_media_info_with_channels(#[case] channels: u16) {
        let info = MediaInfo::default().with_channels(channels);
        assert_eq!(info.container, None);
        assert_eq!(info.codec, None);
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.channels, Some(channels));
    }

    #[test]
    fn test_media_info_builder_chain() {
        let info = MediaInfo::default()
            .with_container(ContainerFormat::Fmp4)
            .with_codec(AudioCodec::AacLc)
            .with_sample_rate(44100)
            .with_channels(2);

        assert_eq!(info.container, Some(ContainerFormat::Fmp4));
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.sample_rate, Some(44100));
        assert_eq!(info.channels, Some(2));
    }

    #[test]
    fn test_media_info_partial_builder() {
        let info = MediaInfo::default()
            .with_codec(AudioCodec::Mp3)
            .with_sample_rate(48000);

        assert_eq!(info.container, None);
        assert_eq!(info.codec, Some(AudioCodec::Mp3));
        assert_eq!(info.sample_rate, Some(48000));
        assert_eq!(info.channels, None);
    }

    #[test]
    fn test_container_format_debug() {
        let format = ContainerFormat::Fmp4;
        let debug_str = format!("{:?}", format);
        assert!(debug_str.contains("Fmp4"));
    }

    #[test]
    fn test_audio_codec_debug() {
        let codec = AudioCodec::AacLc;
        let debug_str = format!("{:?}", codec);
        assert!(debug_str.contains("AacLc"));
    }

    #[test]
    fn test_media_info_clone() {
        let info = MediaInfo::default()
            .with_container(ContainerFormat::Fmp4)
            .with_codec(AudioCodec::AacLc);

        let cloned = info.clone();
        assert_eq!(info, cloned);
    }

    #[test]
    fn test_media_info_partial_eq() {
        let info1 = MediaInfo::default().with_codec(AudioCodec::AacLc);
        let info2 = MediaInfo::default().with_codec(AudioCodec::AacLc);
        let info3 = MediaInfo::default().with_codec(AudioCodec::Mp3);

        assert_eq!(info1, info2);
        assert_ne!(info1, info3);
    }
}
