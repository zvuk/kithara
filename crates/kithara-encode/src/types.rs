use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo};

/// PCM source for encoder requests.
pub trait PcmSource: Send + Sync {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u16;
    fn total_byte_len(&self) -> Option<usize>;
    fn read_pcm_at(&self, offset: usize, buf: &mut [u8]) -> usize;
}

/// Target container/codec for byte-oriented encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytesEncodeTarget {
    Mp3,
    Flac,
    Aac,
    M4a,
}

impl BytesEncodeTarget {
    #[must_use]
    pub const fn codec(self) -> AudioCodec {
        match self {
            Self::Mp3 => AudioCodec::Mp3,
            Self::Flac => AudioCodec::Flac,
            Self::Aac | Self::M4a => AudioCodec::AacLc,
        }
    }

    #[must_use]
    pub const fn container(self) -> ContainerFormat {
        match self {
            Self::Mp3 => ContainerFormat::MpegAudio,
            Self::Flac => ContainerFormat::Flac,
            Self::Aac => ContainerFormat::Adts,
            Self::M4a => ContainerFormat::Fmp4,
        }
    }

    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Flac => "audio/flac",
            Self::Aac => "audio/aac",
            Self::M4a => "audio/mp4",
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Aac => "aac",
            Self::M4a => "m4a",
        }
    }

    #[must_use]
    pub const fn default_bit_rate(self) -> Option<u64> {
        match self {
            Self::Mp3 | Self::Aac | Self::M4a => Some(128_000),
            Self::Flac => None,
        }
    }

    #[must_use]
    pub fn media_info(self, sample_rate: u32, channels: u16) -> MediaInfo {
        MediaInfo::default()
            .with_codec(self.codec())
            .with_container(self.container())
            .with_sample_rate(sample_rate)
            .with_channels(channels)
    }
}

/// Byte-oriented encode request producing complete encoded bytes.
pub struct BytesEncodeRequest<'a> {
    pub pcm: &'a dyn PcmSource,
    pub target: BytesEncodeTarget,
    pub bit_rate: Option<u64>,
}

impl BytesEncodeRequest<'_> {
    #[must_use]
    pub fn media_info(&self) -> MediaInfo {
        self.target
            .media_info(self.pcm.sample_rate(), self.pcm.channels())
    }
}

/// Packaged encode request producing compressed access units for muxing.
pub struct PackagedEncodeRequest<'a> {
    pub pcm: &'a dyn PcmSource,
    pub media_info: MediaInfo,
    pub timescale: u32,
    pub bit_rate: u64,
    pub packets_per_segment: usize,
}

#[derive(Debug, Clone)]
pub struct EncodedBytes {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
    pub media_info: MediaInfo,
}

#[derive(Debug, Clone)]
pub struct EncodedAccessUnit {
    pub bytes: Vec<u8>,
    pub pts: u64,
    pub dts: u64,
    pub duration: u32,
    pub is_sync: bool,
}

#[derive(Debug, Clone)]
pub struct EncodedTrack {
    pub media_info: MediaInfo,
    pub timescale: u32,
    pub bit_rate: u64,
    pub codec_config: Vec<u8>,
    pub packets_per_segment: usize,
    pub access_units: Vec<EncodedAccessUnit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_target_maps_to_expected_media_info() {
        let info = BytesEncodeTarget::M4a.media_info(44_100, 2);
        assert_eq!(info.codec, Some(AudioCodec::AacLc));
        assert_eq!(info.container, Some(ContainerFormat::Fmp4));
        assert_eq!(info.sample_rate, Some(44_100));
        assert_eq!(info.channels, Some(2));
    }

    #[test]
    fn bytes_target_defaults_match_route_contract() {
        let cases = [
            (BytesEncodeTarget::Mp3, "mp3", "audio/mpeg", Some(128_000)),
            (BytesEncodeTarget::Flac, "flac", "audio/flac", None),
            (BytesEncodeTarget::Aac, "aac", "audio/aac", Some(128_000)),
            (BytesEncodeTarget::M4a, "m4a", "audio/mp4", Some(128_000)),
        ];

        for (target, ext, mime, bit_rate) in cases {
            assert_eq!(target.extension(), ext);
            assert_eq!(target.content_type(), mime);
            assert_eq!(target.default_bit_rate(), bit_rate);
        }
    }
}
