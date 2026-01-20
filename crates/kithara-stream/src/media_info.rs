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
}

impl MediaInfo {
    /// Create empty `MediaInfo`.
    pub fn new() -> Self {
        Self::default()
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
}
